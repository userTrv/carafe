using System.Text.Json.Nodes;
using SteamKit2;

namespace Carafe.Steam;

/// Депо приложения из appinfo и выбор того, что ставить под Windows (как в DepotDownloader:
/// oslist с windows или пустой, 64 бита, если есть, английский или без языка, без чужих DLC).
sealed class AppDepots
{
    public required uint AppId { get; init; }
    public required string Name { get; init; }
    public required List<Depot> Depots { get; init; }
    public required List<Branch> Branches { get; init; }

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

    public static AppDepots From(uint appid, KeyValue kv, IReadOnlySet<uint> owned, string branch = "public")
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
            else if (os.Length > 0 && !os.Split(',').Contains("windows"))
                skip = "не Windows";
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

        return new AppDepots
        {
            AppId = appid,
            Name = kv["common"]["name"].AsString() ?? $"App {appid}",
            Depots = depots,
            Branches = branches,
        };
    }

    public JsonObject ToJson() => new()
    {
        ["appid"] = AppId,
        ["name"] = Name,
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
    };
}
