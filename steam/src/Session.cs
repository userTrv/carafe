using System.Text.Json.Nodes;
using SteamKit2;
using SteamKit2.Authentication;
using SteamKit2.Internal;

namespace Carafe.Steam;

/// Подключение к Steam (CM) и вход. Одно на процесс: запросы идут по очереди.
sealed partial class Session : IAsyncDisposable
{
    static readonly TimeSpan ConnectTimeout = TimeSpan.FromSeconds(30);
    static readonly TimeSpan LogonTimeout = TimeSpan.FromSeconds(30);
    static readonly TimeSpan LicensesTimeout = TimeSpan.FromSeconds(30);
    /// Сколько ждать подтверждения QR в приложении: дальше ядро может начать заново.
    static readonly TimeSpan QrTimeout = TimeSpan.FromMinutes(5);
    /// PICS отвечает на большие пачки медленно и обрывает их; по 200 — как у DepotDownloader.
    const int PicsBatch = 200;

    readonly Output output;
    readonly SteamClient client;
    readonly CallbackManager callbacks;
    readonly SteamUser user;
    readonly SteamApps apps;
    readonly SteamContent content;
    readonly SteamUnifiedMessages unified;
    readonly Thread pump;
    volatile bool stopping;

    TaskCompletionSource<bool>? connected;
    TaskCompletionSource<SteamUser.LoggedOnCallback>? loggedOn;
    TaskCompletionSource<IReadOnlyCollection<SteamApps.LicenseListCallback.License>> licenses = NewTcs<IReadOnlyCollection<SteamApps.LicenseListCallback.License>>();

    bool isConnected;
    /// `null` — не вошли; `""` — анонимно; иначе — имя аккаунта.
    string? account;
    /// Токен текущей сессии — только в памяти, нужен для logout.
    string? refreshToken;

    public Session(Output output)
    {
        this.output = output;
        client = new SteamClient(SteamConfiguration.Create(b => b.WithConnectionTimeout(ConnectTimeout)));
        callbacks = new CallbackManager(client);
        user = client.GetHandler<SteamUser>()!;
        apps = client.GetHandler<SteamApps>()!;
        content = client.GetHandler<SteamContent>()!;
        unified = client.GetHandler<SteamUnifiedMessages>()!;

        callbacks.Subscribe<SteamClient.ConnectedCallback>(_ =>
        {
            isConnected = true;
            connected?.TrySetResult(true);
        });
        callbacks.Subscribe<SteamClient.DisconnectedCallback>(cb =>
        {
            var was = isConnected;
            isConnected = false;
            account = null;
            connected?.TrySetException(new HelperException("network", "не удалось подключиться к Steam"));
            loggedOn?.TrySetException(new HelperException("network", "Steam разорвал соединение"));
            licenses.TrySetException(new HelperException("network", "Steam разорвал соединение"));
            if (was && !cb.UserInitiated) Log.Write("соединение со Steam разорвано");
        });
        callbacks.Subscribe<SteamUser.LoggedOnCallback>(cb => loggedOn?.TrySetResult(cb));
        callbacks.Subscribe<SteamUser.LoggedOffCallback>(cb =>
        {
            Log.Write($"Steam завершил сессию: {cb.Result}");
            account = null;
        });
        callbacks.Subscribe<SteamApps.LicenseListCallback>(cb =>
        {
            if (cb.Result == EResult.OK) licenses.TrySetResult(cb.LicenseList);
            else licenses.TrySetException(new HelperException("failed", $"список лицензий: {cb.Result}"));
        });

        pump = new Thread(() =>
        {
            while (!stopping) callbacks.RunWaitCallbacks(TimeSpan.FromMilliseconds(200));
        }) { IsBackground = true, Name = "steam-callbacks" };
        pump.Start();
    }

    static TaskCompletionSource<T> NewTcs<T>() => new(TaskCreationOptions.RunContinuationsAsynchronously);

    public async ValueTask DisposeAsync()
    {
        if (isConnected)
        {
            if (account != null) user.LogOff();
            client.Disconnect();
        }
        stopping = true;
        await Task.Run(() => pump.Join(TimeSpan.FromSeconds(2)));
    }

    public Task<JsonObject> Handle(Request req, CancellationToken ct) => req.Cmd switch
    {
        "login_qr" => LoginQr(req, ct),
        "login_token" => LoginToken(req, ct),
        "status" => Task.FromResult(Status()),
        "licenses" => Licenses(ct),
        "owned_apps" => OwnedApps(ct),
        "app_info" => AppInfo(req.UInt("appid"), ct),
        "download" => Download(req, ct),
        "logout" => Logout(req, ct),
        _ => throw new HelperException("bad_request", $"неизвестная команда {req.Cmd}"),
    };

    JsonObject Status() => new()
    {
        ["connected"] = isConnected,
        ["logged_in"] = !string.IsNullOrEmpty(account),
        ["anonymous"] = account == "",
        ["account_name"] = string.IsNullOrEmpty(account) ? null : account,
        ["steam_id"] = string.IsNullOrEmpty(account) ? null : MaskSteamId(client.SteamID),
    };

    /// Steam ID наружу — только последние 4 цифры.
    static string? MaskSteamId(SteamID? id)
    {
        if (id is null || !id.IsValid) return null;
        var s = id.ConvertToUInt64().ToString();
        return "••••" + s[^4..];
    }

    async Task Connect(CancellationToken ct)
    {
        if (isConnected) return;
        connected = NewTcs<bool>();
        Log.Write("подключаюсь к Steam");
        client.Connect();
        await Wait(connected.Task, ConnectTimeout, "подключение к Steam", ct);
    }

    /// Начать заново: QR и вход по токену нельзя делать поверх чужой (или анонимной) сессии.
    async Task Reconnect(CancellationToken ct)
    {
        if (isConnected)
        {
            if (account != null) user.LogOff();
            client.Disconnect();
            var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(5);
            while (isConnected && DateTime.UtcNow < deadline) await Task.Delay(50, ct);
        }
        account = null;
        refreshToken = null;
        licenses = NewTcs<IReadOnlyCollection<SteamApps.LicenseListCallback.License>>();
        await Connect(ct);
    }

    static async Task<T> Wait<T>(Task<T> task, TimeSpan timeout, string what, CancellationToken ct)
    {
        try
        {
            return await task.WaitAsync(timeout, ct);
        }
        catch (TimeoutException)
        {
            throw new HelperException("timeout", $"{what}: Steam не ответил за {timeout.TotalSeconds:0} с");
        }
    }

    async Task<JsonObject> LoginQr(Request req, CancellationToken ct)
    {
        await Reconnect(ct);
        var details = new AuthSessionDetails
        {
            // Это имя видно в приложении Steam при подтверждении и в списке устройств.
            DeviceFriendlyName = req.Body["device_name"]?.GetValue<string>() is { Length: > 0 } n ? n : "carafe (Mac)",
            PlatformType = EAuthTokenPlatformType.k_EAuthTokenPlatformType_SteamClient,
            IsPersistentSession = true,
        };
        var session = await client.Authentication.BeginAuthSessionViaQRAsync(details);
        Log.Write("жду подтверждения QR в приложении Steam");
        output.Event(req.Id, "qr", new JsonObject { ["url"] = session.ChallengeURL });
        // Steam время от времени меняет challenge — QR надо перерисовать.
        session.ChallengeURLChanged = () =>
            output.Event(req.Id, "qr", new JsonObject { ["url"] = session.ChallengeURL });

        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeout.CancelAfter(QrTimeout);
        AuthPollResult poll;
        try
        {
            poll = await session.PollingWaitForResultAsync(timeout.Token);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            throw new HelperException("timeout", "QR не подтвердили вовремя");
        }
        catch (AuthenticationException e)
        {
            throw new HelperException("auth_failed", $"вход не подтверждён: {e.Result}");
        }
        output.Event(req.Id, "confirmed");
        await LogOn(poll.AccountName, poll.RefreshToken, ct);
        var result = Status();
        result["refresh_token"] = poll.RefreshToken;
        return result;
    }

    async Task<JsonObject> LoginToken(Request req, CancellationToken ct)
    {
        var name = req.Str("account_name");
        var token = req.Str("refresh_token");
        if (account == name && refreshToken == token && isConnected) return Status();
        await Reconnect(ct);
        await LogOn(name, token, ct);
        return Status();
    }

    async Task LogOn(string name, string token, CancellationToken ct)
    {
        loggedOn = NewTcs<SteamUser.LoggedOnCallback>();
        Log.Write("вхожу в Steam");
        user.LogOn(new SteamUser.LogOnDetails
        {
            Username = name,
            AccessToken = token,
            ShouldRememberPassword = true,
        });
        var cb = await Wait(loggedOn.Task, LogonTimeout, "вход", ct);
        if (cb.Result != EResult.OK)
        {
            Log.Write($"вход не удался: {cb.Result} / {cb.ExtendedResult}");
            var code = cb.Result is EResult.InvalidPassword or EResult.AccessDenied or EResult.Expired
                or EResult.Revoked or EResult.InvalidSignature or EResult.AccountLoginDeniedNeedTwoFactor
                ? "token_invalid"
                : "auth_failed";
            throw new HelperException(code, $"Steam не пустил: {cb.Result}");
        }
        account = name;
        refreshToken = token;
        Log.Write("вошёл");
    }

    async Task LogOnAnonymous(CancellationToken ct)
    {
        await Connect(ct);
        if (account != null) return;
        loggedOn = NewTcs<SteamUser.LoggedOnCallback>();
        Log.Write("вхожу анонимно");
        user.LogOnAnonymous();
        var cb = await Wait(loggedOn.Task, LogonTimeout, "анонимный вход", ct);
        if (cb.Result != EResult.OK) throw new HelperException("auth_failed", $"анонимный вход: {cb.Result}");
        account = "";
    }

    void RequireUser()
    {
        if (string.IsNullOrEmpty(account)) throw new HelperException("not_logged_in", "сначала login_qr или login_token");
    }

    async Task<JsonObject> Logout(Request req, CancellationToken ct)
    {
        // Токен можно прислать и без login_token: отзываем его на стороне Steam.
        if (req.Body["refresh_token"]?.GetValue<string>() is { Length: > 0 } token
            && req.Body["account_name"]?.GetValue<string>() is { Length: > 0 } name
            && (account != name || refreshToken != token))
        {
            await Reconnect(ct);
            try
            {
                await LogOn(name, token, ct);
            }
            catch (HelperException e) when (e.Code == "token_invalid")
            {
                // Токен уже не действует — отзывать нечего.
                return new JsonObject { ["revoked"] = false };
            }
        }
        var revoked = false;
        if (!string.IsNullOrEmpty(account) && refreshToken != null)
        {
            try
            {
                var auth = unified.CreateService<Authentication>();
                var resp = await auth.RevokeToken(new CAuthentication_Token_Revoke_Request
                {
                    token = refreshToken,
                    revoke_action = EAuthTokenRevokeAction.k_EAuthTokenRevokePermanent,
                }).ToTask().WaitAsync(LogonTimeout, ct);
                revoked = resp.Result == EResult.OK;
                if (!revoked) Log.Write($"отзыв токена: {resp.Result}");
            }
            catch (Exception e) when (e is not OperationCanceledException)
            {
                Log.Write($"отзыв токена не удался: {e.GetType().Name}");
            }
        }
        if (isConnected)
        {
            if (account != null) user.LogOff();
            client.Disconnect();
        }
        account = null;
        refreshToken = null;
        return new JsonObject { ["revoked"] = revoked };
    }

    async Task<IReadOnlyCollection<SteamApps.LicenseListCallback.License>> UserLicenses(CancellationToken ct)
    {
        RequireUser();
        var all = await Wait(licenses.Task, LicensesTimeout, "список лицензий", ct);
        return all.Where(l => !l.LicenseFlags.HasFlag(ELicenseFlags.Expired)).ToList();
    }

    async Task<JsonObject> Licenses(CancellationToken ct)
    {
        RequireUser();
        var list = await UserLicenses(ct);
        return new JsonObject
        {
            ["packages"] = new JsonArray(list.Select(l => (JsonNode)new JsonObject
            {
                ["packageid"] = l.PackageID,
                ["license_type"] = l.LicenseType.ToString(),
                ["time_created"] = new DateTimeOffset(l.TimeCreated).ToUnixTimeSeconds(),
            }).ToArray()),
        };
    }

    /// appid всех приложений из лицензий аккаунта (по пакетам из PICS).
    async Task<HashSet<uint>> OwnedAppIds(CancellationToken ct)
    {
        // Анонимной сессии Steam список лицензий не присылает.
        if (account == "") return [];
        var list = await UserLicenses(ct);
        var packages = list.Where(l => l.PackageID != 0)
            .Select(l => new SteamApps.PICSRequest(l.PackageID, l.AccessToken))
            .ToList();
        var ids = new HashSet<uint>();
        foreach (var batch in packages.Chunk(PicsBatch))
        {
            var info = await ProductInfo([], batch, ct);
            foreach (var pkg in info.SelectMany(r => r.Packages.Values))
                foreach (var a in pkg.KeyValues["appids"].Children)
                    if (uint.TryParse(a.Value, out var id)) ids.Add(id);
        }
        return ids;
    }

    async Task<List<SteamApps.PICSProductInfoCallback>> ProductInfo(
        IEnumerable<SteamApps.PICSRequest> appReqs, IEnumerable<SteamApps.PICSRequest> pkgReqs, CancellationToken ct)
    {
        var job = apps.PICSGetProductInfo(appReqs, pkgReqs);
        var set = await job.ToTask().WaitAsync(TimeSpan.FromSeconds(60), ct);
        if (!set.Complete) throw new HelperException("timeout", "PICS ответил не полностью");
        return set.Results?.ToList() ?? [];
    }

    /// appinfo пачкой: сначала токены доступа, потом сами узлы.
    async Task<Dictionary<uint, KeyValue>> AppInfos(IReadOnlyCollection<uint> ids, CancellationToken ct)
    {
        var result = new Dictionary<uint, KeyValue>();
        foreach (var batch in ids.Chunk(PicsBatch))
        {
            var tokens = await apps.PICSGetAccessTokens(batch, []).ToTask().WaitAsync(TimeSpan.FromSeconds(60), ct);
            var reqs = batch.Select(id => new SteamApps.PICSRequest(id, tokens.AppTokens.GetValueOrDefault(id)));
            foreach (var r in await ProductInfo(reqs, [], ct))
                foreach (var (id, info) in r.Apps)
                    result[id] = info.KeyValues;
        }
        return result;
    }

    async Task<JsonObject> OwnedApps(CancellationToken ct)
    {
        RequireUser();
        var ids = await OwnedAppIds(ct);
        var infos = await AppInfos(ids, ct);
        var games = new JsonArray();
        foreach (var (id, kv) in infos.OrderBy(p => p.Key))
        {
            var type = kv["common"]["type"].AsString()?.ToLowerInvariant();
            if (type != "game") continue;
            var app = AppDepots.From(id, kv, ids);
            games.Add(new JsonObject
            {
                ["appid"] = id,
                ["name"] = app.Name,
                ["type"] = type,
                ["oslist"] = kv["common"]["oslist"].AsString() ?? "",
                ["windows"] = app.Install.Count > 0,
                ["size_bytes"] = app.Install.Count > 0 ? app.Install.Sum(d => (long)d.Size) : null,
            });
        }
        Log.Write($"приложений в лицензиях: {ids.Count}, игр: {games.Count}");
        return new JsonObject { ["apps"] = games };
    }

    async Task<AppDepots> LoadAppDepots(uint appid, CancellationToken ct)
    {
        // Публичные данные приложения видны и анонимно.
        if (account == null) await LogOnAnonymous(ct);
        var owned = await OwnedAppIds(ct);
        var infos = await AppInfos([appid], ct);
        if (!infos.TryGetValue(appid, out var kv) || kv.Children.Count == 0)
            throw new HelperException("not_found", $"нет appinfo для {appid}");
        return AppDepots.From(appid, kv, owned);
    }

    async Task<JsonObject> AppInfo(uint appid, CancellationToken ct)
    {
        var app = await LoadAppDepots(appid, ct);
        return app.ToJson();
    }
}
