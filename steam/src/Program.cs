// carafe-steam: хелпер carafe для входа в Steam по QR, списка своих игр и загрузки депо.
//
// Говорит JSON-строками: запросы — в stdin, события и ответы — в stdout, stderr — только лог
// без секретов. Протокол описан в steam/PROTOCOL.md.
//
// Пароль Steam хелпер не принимает вообще: вход только по QR (подтверждение в мобильном
// приложении Steam) или по refresh token, который ядро держит в Связке ключей и присылает
// через stdin. Токен уходит в stdout ровно один раз — в ответе на login_qr.
//
// Это сторонний клиент Steam на SteamKit2: Valve такие клиенты терпит, но официально
// не поддерживает.

using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Threading.Channels;

namespace Carafe.Steam;

static class Program
{
    public const int ProtocolVersion = 1;

    static async Task<int> Main(string[] args)
    {
        if (args.Length > 0)
        {
            // Никаких аргументов: всё, в том числе токен, приходит через stdin.
            Console.Error.WriteLine("carafe-steam: аргументов нет, протокол — JSON-строки в stdin (steam/PROTOCOL.md)");
            return 2;
        }
        var output = new Output(Console.OpenStandardOutput());
        await using var session = new Session(output);
        var requests = Channel.CreateUnbounded<Request>();
        var running = new Running();

        output.Write(new JsonObject
        {
            ["event"] = "ready",
            ["protocol"] = ProtocolVersion,
            ["version"] = typeof(Program).Assembly.GetName().Version?.ToString(3),
        });

        // stdin читаем отдельно от исполнения запросов: отмена должна доходить, пока запрос идёт.
        var reader = Task.Run(async () =>
        {
            using var stdin = new StreamReader(Console.OpenStandardInput(), new UTF8Encoding(false));
            while (await stdin.ReadLineAsync() is { } line)
            {
                if (string.IsNullOrWhiteSpace(line)) continue;
                Request req;
                try
                {
                    req = Request.Parse(line);
                }
                catch (ProtocolException e)
                {
                    output.Error(e.Id, "bad_request", e.Message);
                    continue;
                }
                if (req.Cmd == "cancel")
                {
                    running.Cancel(req.Body["target"]?.GetValue<long>());
                    continue;
                }
                await requests.Writer.WriteAsync(req);
            }
            // Ядро закрыло stdin — бросаем текущую работу и выходим.
            running.CancelAll();
            requests.Writer.Complete();
        });

        await foreach (var req in requests.Reader.ReadAllAsync())
        {
            using var cts = running.Start(req.Id);
            try
            {
                var result = await session.Handle(req, cts.Token);
                output.Ok(req.Id, result);
            }
            catch (OperationCanceledException) when (cts.IsCancellationRequested)
            {
                output.Error(req.Id, "cancelled", "отменено");
            }
            catch (HelperException e)
            {
                output.Error(req.Id, e.Code, e.Message);
            }
            catch (Exception e)
            {
                // Текст исключения SteamKit2 без секретов: токены в исключения не попадают.
                Log.Write($"{req.Cmd}: {e.GetType().Name}: {e.Message}");
                output.Error(req.Id, "failed", e.Message);
            }
            finally
            {
                running.Finish();
            }
        }
        await reader;
        return 0;
    }
}

/// Запрос ядра: `{"id":1,"cmd":"owned_apps", ...}`.
sealed record Request(long Id, string Cmd, JsonObject Body)
{
    public static Request Parse(string line)
    {
        JsonObject body;
        try
        {
            body = JsonNode.Parse(line) as JsonObject ?? throw new ProtocolException(null, "запрос — JSON-объект");
        }
        catch (System.Text.Json.JsonException)
        {
            throw new ProtocolException(null, "не JSON");
        }
        long? id = null;
        try
        {
            id = body["id"]?.GetValue<long>();
        }
        catch (Exception e) when (e is InvalidOperationException or FormatException)
        {
        }
        if (id is null) throw new ProtocolException(null, "нет id");
        var cmd = body["cmd"]?.GetValue<string>();
        if (string.IsNullOrEmpty(cmd)) throw new ProtocolException(id, "нет cmd");
        return new Request(id.Value, cmd, body);
    }

    public string Str(string key) =>
        Body[key]?.GetValue<string>() is { Length: > 0 } s ? s : throw new HelperException("bad_request", $"нет поля {key}");

    public uint UInt(string key)
    {
        try
        {
            return Body[key]?.GetValue<uint>() ?? throw new HelperException("bad_request", $"нет поля {key}");
        }
        catch (Exception e) when (e is InvalidOperationException or FormatException)
        {
            throw new HelperException("bad_request", $"{key} — число");
        }
    }
}

sealed class ProtocolException(long? id, string message) : Exception(message)
{
    public long? Id { get; } = id;
}

/// Ошибка с кодом для ядра: `not_logged_in`, `token_invalid`, `timeout`, `not_found`...
sealed class HelperException(string code, string message) : Exception(message)
{
    public string Code { get; } = code;
}

/// Какой запрос сейчас выполняется: его можно отменить по id.
sealed class Running
{
    readonly Lock gate = new();
    long? id;
    CancellationTokenSource? cts;
    bool closed;

    public CancellationTokenSource Start(long requestId)
    {
        lock (gate)
        {
            cts = new CancellationTokenSource();
            id = requestId;
            if (closed) cts.Cancel();
            return cts;
        }
    }

    public void Finish()
    {
        lock (gate)
        {
            cts = null;
            id = null;
        }
    }

    /// `target` не задан — отменить текущий запрос, какой бы он ни был.
    public void Cancel(long? target)
    {
        lock (gate)
        {
            if (cts != null && (target is null || target == id)) cts.Cancel();
        }
    }

    public void CancelAll()
    {
        lock (gate)
        {
            closed = true;
            cts?.Cancel();
        }
    }
}

/// stdout: одна JSON-строка на сообщение, запись из любых потоков.
sealed class Output(Stream stream)
{
    /// Кириллица в сообщениях — как есть, без \uXXXX.
    static readonly JsonSerializerOptions Json = new() { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };
    readonly Lock gate = new();
    readonly StreamWriter w = new(stream, new UTF8Encoding(false)) { AutoFlush = true, NewLine = "\n" };

    public void Write(JsonObject msg)
    {
        var line = msg.ToJsonString(Json);
        lock (gate) w.WriteLine(line);
    }

    public void Event(long id, string name, JsonObject? fields = null)
    {
        var msg = new JsonObject { ["id"] = id, ["event"] = name };
        if (fields != null)
            foreach (var (k, v) in fields.ToList())
            {
                fields.Remove(k);
                msg[k] = v;
            }
        Write(msg);
    }

    public void Ok(long id, JsonObject result) => Write(new JsonObject { ["id"] = id, ["ok"] = true, ["result"] = result });

    public void Error(long? id, string code, string message) =>
        Write(new JsonObject
        {
            ["id"] = id,
            ["ok"] = false,
            ["error"] = new JsonObject { ["code"] = code, ["message"] = message },
        });
}

/// Лог в stderr. Сюда не пишем токены, URL QR-кода, Steam ID и имя аккаунта.
static class Log
{
    public static void Write(string msg) => Console.Error.WriteLine($"carafe-steam: {msg}");
}
