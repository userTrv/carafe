using System.Text.Json.Nodes;
using SteamKit2;

namespace Carafe.Steam;

/// Платформа сборки: `windows` (через Wine) или `macos` (нативно).
static class Os
{
    public const string Windows = "windows";
    public const string MacOS = "macos";

    /// Поле `platform` запроса; нет — Windows, как до появления нативных игр.
    public static string From(Request req) => req.Body["platform"]?.GetValue<string>() switch
    {
        null or "" or Windows => Windows,
        MacOS => MacOS,
        var p => throw new HelperException("bad_request", $"неизвестная платформа {p}"),
    };

    public static string Label(string platform) => platform == MacOS ? "macOS" : "Windows";

    public static bool In(string oslist, string platform) => oslist.Split(',').Contains(platform);
}

/// Депо приложения из appinfo и выбор того, что ставить под платформу (как в DepotDownloader:
/// oslist с платформой или пустой, 64 бита, если есть, английский или без языка, без чужих DLC).
sealed class AppDepots
{
    public required uint AppId { get; init; }
    public required string Name { get; init; }
    /// Для какой платформы выбраны депо (`Install`).
    public required string Platform { get; init; }
    public required List<Depot> Depots { get; init; }
    public required List<Branch> Branches { get; init; }
    public required List<Launch> LaunchEntries { get; init; }

    public List<Depot> Install => Depots.Where(d => d.Skip == null).ToList();

    public sealed record Depot(
        uint Id,
        string? Name,
        string OsList,
        string? Arch,
        string? Language,
        ulong Size,
        Dictionary<string, ulong> Manifests,
        uint? DlcAppId,
        string? Skip);

    public sealed record Branch(string Name, uint BuildId, long TimeUpdated, bool PasswordRequired, string? Description);

    /// Вариант запуска из `config/launch`: что запускать, с какими аргументами, на какой ОС.
    public sealed record Launch(
        string Executable,
        string? Arguments,
        string? WorkingDir,
        string? Type,
        string? Description,
        string OsList,
        string? OsArch,
        string? BetaKey);

    /// Есть ли нативная сборка для macOS: `common/oslist` с macos и депо именно под macOS, которое
    /// ставится (есть манифест ветки, своё или купленное DLC). Либо у игры вообще нет депо под
    /// macOS, но есть вариант запуска под macOS: тогда `.app` лежит в общем депо (DDLC, игры на
    /// Ren'Py). `ownedDepots` — депо из лицензий аккаунта: пустой набор не проверяется (анонимно
    /// или пакеты без списка депо).
    public static bool HasMac(KeyValue kv, AppDepots mac, IReadOnlySet<uint> ownedDepots)
    {
        if (!Os.In(kv["common"]["oslist"].AsString() ?? "", Os.MacOS)) return false;
        bool Owned(Depot d) => ownedDepots.Count == 0 || ownedDepots.Contains(d.Id);
        if (mac.Install.Any(d => Os.In(d.OsList, Os.MacOS) && Owned(d))) return true;
        return !mac.Depots.Any(d => Os.In(d.OsList, Os.MacOS))
               && mac.LaunchEntries.Any(l => Os.In(l.OsList, Os.MacOS))
               && mac.Install.Any(Owned);
    }

    /// Варианты запуска под платформу и ветку: oslist пустой или с платформой, без чужих бет.
    public List<Launch> LaunchFor(string branch) => LaunchEntries
        .Where(l => l.OsList.Length == 0 || Os.In(l.OsList, Platform))
        .Where(l => string.IsNullOrEmpty(l.BetaKey) || l.BetaKey == branch)
        .Where(l => l.Type is null or "" or "default" or "none" or "option1")
        .ToList();

    public static AppDepots From(uint appid, KeyValue kv, IReadOnlySet<uint> owned, string branch = "public",
        string platform = Os.Windows)
    {
        var depots = new List<Depot>();
        var branches = new List<Branch>();
        foreach (var d in kv["depots"].Children)
        {
            if (d.Name == "branches")
            {
                foreach (var b in d.Children)
                    branches.Add(new Branch(
                        b.Name ?? "",
                        b["buildid"].AsUnsignedInteger(),
                        b["timeupdated"].AsLong(),
                        b["pwdrequired"].AsBoolean(),
                        b["description"].AsString()));
                continue;
            }
            if (!uint.TryParse(d.Name, out var id)) continue;
            var cfg = d["config"];
            var manifests = new Dictionary<string, ulong>();
            ulong size = 0;
            foreach (var m in d["manifests"].Children)
            {
                // Новый формат: manifests/<ветка>/{gid,size,download}; старый — сразу gid.
                var gid = m.Children.Count > 0 ? m["gid"].AsUnsignedLong() : m.AsUnsignedLong();
                if (gid != 0 && m.Name != null) manifests[m.Name] = gid;
                if (m.Name == branch && m.Children.Count > 0) size = m["size"].AsUnsignedLong();
            }
            if (size == 0) size = d["maxsize"].AsUnsignedLong();
            var dlc = d["dlcappid"].AsUnsignedInteger();
            var os = cfg["oslist"].AsString() ?? "";
            var lang = cfg["language"].AsString();
            string? skip = null;
            if (d["depotfromapp"].Value != null || d["sharedinstall"].AsBoolean())
                skip = "общий редистрибутив";
            else if (os.Length > 0 && !Os.In(os, platform))
                skip = $"не {Os.Label(platform)}";
            else if (dlc != 0 && !owned.Contains(dlc))
                skip = "DLC не куплено";
            else if (!string.IsNullOrEmpty(lang) && lang != "english")
                skip = "другой язык";
            else if (cfg["lowviolence"].AsBoolean())
                skip = "low violence";
            else if (!manifests.ContainsKey(branch))
                skip = "нет манифеста";
            depots.Add(new Depot(id, d["name"].AsString(), os, cfg["osarch"].AsString(), lang, size, manifests,
                dlc == 0 ? null : dlc, skip));
        }
        // Есть 64-битные депо — 32-битные не нужны.
        if (depots.Any(d => d.Skip == null && d.Arch == "64"))
            for (var i = 0; i < depots.Count; i++)
                if (depots[i].Skip == null && depots[i].Arch == "32")
                    depots[i] = depots[i] with { Skip = "32 бита" };

        var launch = new List<Launch>();
        foreach (var l in kv["config"]["launch"].Children)
        {
            var exe = l["executable"].AsString();
            if (string.IsNullOrEmpty(exe)) continue;
            var c = l["config"];
            launch.Add(new Launch(exe, l["arguments"].AsString(), l["workingdir"].AsString(), l["type"].AsString(),
                l["description"].AsString(), c["oslist"].AsString() ?? "", c["osarch"].AsString(),
                c["betakey"].AsString()));
        }

        return new AppDepots
        {
            AppId = appid,
            Name = kv["common"]["name"].AsString() ?? $"App {appid}",
            Platform = platform,
            Depots = depots,
            Branches = branches,
            LaunchEntries = launch,
        };
    }

    public JsonObject ToJson() => new()
    {
        ["appid"] = AppId,
        ["name"] = Name,
        ["platform"] = Platform,
        ["size_bytes"] = Install.Sum(d => (long)d.Size),
        ["depots"] = new JsonArray(Depots.Select(d => (JsonNode)new JsonObject
        {
            ["depotid"] = d.Id,
            ["name"] = d.Name,
            ["oslist"] = d.OsList,
            ["arch"] = d.Arch,
            ["language"] = d.Language,
            ["size_bytes"] = d.Size,
            ["manifests"] = new JsonObject(d.Manifests.Select(m =>
                KeyValuePair.Create(m.Key, (JsonNode?)JsonValue.Create(m.Value.ToString())))),
            ["dlc_appid"] = d.DlcAppId,
            ["install"] = d.Skip == null,
            ["skip"] = d.Skip,
        }).ToArray()),
        ["branches"] = new JsonArray(Branches.Select(b => (JsonNode)new JsonObject
        {
            ["name"] = b.Name,
            ["buildid"] = b.BuildId,
            ["time_updated"] = b.TimeUpdated,
            ["password_required"] = b.PasswordRequired,
            ["description"] = b.Description,
        }).ToArray()),
        ["launch"] = LaunchJson(LaunchEntries),
    };

    public static JsonArray LaunchJson(IEnumerable<Launch> list) => new(list.Select(l => (JsonNode)new JsonObject
    {
        ["executable"] = l.Executable,
        ["arguments"] = l.Arguments,
        ["workingdir"] = l.WorkingDir,
        ["type"] = l.Type,
        ["description"] = l.Description,
        ["oslist"] = l.OsList,
        ["osarch"] = l.OsArch,
        ["betakey"] = l.BetaKey,
    }).ToArray());
}
