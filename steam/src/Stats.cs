using System.Globalization;
using System.Text.Json.Nodes;
using SteamKit2;
using SteamKit2.Internal;

namespace Carafe.Steam;

/// Время в играх и достижения своего аккаунта — через сервис Player сессии, без ключа Web API.
sealed partial class Session
{
    static readonly TimeSpan StatsTimeout = TimeSpan.FromSeconds(30);
    /// Иконки достижений: тот же CDN, что и обложки (он разрешён в CSP окна).
    const string AchievementIcons = "https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps";

    async Task<T> Call<T>(AsyncJob<SteamUnifiedMessages.ServiceMethodResponse<T>> job, string what, CancellationToken ct)
        where T : ProtoBuf.IExtensible, new()
    {
        SteamUnifiedMessages.ServiceMethodResponse<T> resp;
        try
        {
            resp = await job.ToTask().WaitAsync(StatsTimeout, ct);
        }
        catch (TimeoutException)
        {
            throw new HelperException("timeout", $"{what}: Steam не ответил за {StatsTimeout.TotalSeconds:0} с");
        }
        if (resp.Result != EResult.OK) throw new HelperException("failed", $"{what}: {resp.Result}");
        return resp.Body;
    }

    ulong OwnSteamId() =>
        client.SteamID is { IsValid: true } id ? id.ConvertToUInt64() : throw new HelperException("not_logged_in", "нет Steam ID");

    /// Сыгранные игры аккаунта: минуты всего и за две недели, последний запуск.
    async Task<JsonObject> Playtime(CancellationToken ct)
    {
        RequireUser();
        var player = unified.CreateService<Player>();
        var body = await Call(player.GetOwnedGames(new CPlayer_GetOwnedGames_Request
        {
            steamid = OwnSteamId(),
            include_played_free_games = true,
            include_free_sub = true,
            skip_unvetted_apps = false,
        }), "время в играх", ct);
        var games = new JsonArray();
        foreach (var g in body.games.Where(g => g.playtime_forever > 0 || g.rtime_last_played > 0).OrderBy(g => g.appid))
            games.Add(new JsonObject
            {
                ["appid"] = g.appid,
                ["minutes"] = g.playtime_forever,
                ["minutes_2weeks"] = g.playtime_2weeks,
                ["last_played"] = g.rtime_last_played > 0 ? g.rtime_last_played : null,
            });
        Log.Write($"время в играх: {games.Count}");
        return new JsonObject { ["games"] = games };
    }

    /// Достижения игры: схема с локализованными названиями и иконками, отметки аккаунта
    /// (когда открыто) и доля игроков, открывших каждое.
    async Task<JsonObject> Achievements(Request req, CancellationToken ct)
    {
        RequireUser();
        var appid = req.UInt("appid");
        var language = req.Body["language"]?.GetValue<string>() is { Length: > 0 } l ? l : "english";
        var player = unified.CreateService<Player>();
        var schema = await Call(player.GetGameAchievements(new CPlayer_GetGameAchievements_Request
        {
            appid = appid,
            language = language,
        }), "схема достижений", ct);
        var mine = await Call(player.GetUserAchievements(new CPlayer_GetUserAchievements_Request
        {
            steamid = OwnSteamId(),
            appid = appid,
        }), "достижения аккаунта", ct);
        var got = mine.achievements.ToDictionary(a => a.internal_key);
        var list = new JsonArray();
        var unlocked = 0;
        foreach (var a in schema.achievements.Where(a => !a.archived))
        {
            got.TryGetValue(a.internal_key, out var u);
            var open = u is { unlocked: true };
            if (open) unlocked++;
            list.Add(new JsonObject
            {
                ["id"] = a.internal_name,
                ["name"] = a.localized_name,
                ["description"] = string.IsNullOrEmpty(a.localized_desc) ? null : a.localized_desc,
                ["icon"] = IconUrl(appid, a.icon),
                ["icon_gray"] = IconUrl(appid, a.icon_gray),
                ["hidden"] = a.hidden,
                ["unlocked"] = open,
                ["unlocked_at"] = open && u!.unlock_time > 0 ? u.unlock_time : null,
                ["global_percent"] = double.TryParse(a.player_percent_unlocked, NumberStyles.Float, CultureInfo.InvariantCulture, out var p)
                    ? Math.Round(p, 1)
                    : null,
            });
        }
        Log.Write($"достижения {appid}: {unlocked}/{list.Count}");
        return new JsonObject
        {
            ["appid"] = appid,
            ["total"] = list.Count,
            ["unlocked"] = unlocked,
            ["achievements"] = list,
        };
    }

    static string? IconUrl(uint appid, string? icon)
    {
        if (string.IsNullOrEmpty(icon)) return null;
        if (icon.StartsWith("https://", StringComparison.Ordinal)) return icon;
        // Имя файла — хеш вида `abc123.jpg`; что-то другое в URL не подставляем.
        if (icon.Any(c => !(char.IsAsciiLetterOrDigit(c) || c == '.' || c == '_'))) return null;
        return $"{AchievementIcons}/{appid}/{icon}";
    }
}
