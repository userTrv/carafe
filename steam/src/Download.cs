using System.Collections.Concurrent;
using System.Diagnostics;
using System.Security.Cryptography;
using System.Text.Json.Nodes;
using SteamKit2;
using SteamKit2.CDN;

namespace Carafe.Steam;

/// Загрузка Windows-депо приложения в папку: манифесты, чанки с CDN Steam, прогресс.
sealed partial class Session
{
    const int Parallel = 8;
    const int ChunkRetries = 5;
    static readonly TimeSpan ProgressEvery = TimeSpan.FromMilliseconds(250);

    sealed record Job(uint DepotId, DepotManifest Manifest, byte[] Key, List<DepotManifest.FileData> Files);

    async Task<JsonObject> Download(Request req, CancellationToken ct)
    {
        var appid = req.UInt("appid");
        // Без входа — анонимно: так качаются бесплатные депо Valve (например, Steamworks SDK Redist).
        if (account == null) await LogOnAnonymous(ct);
        var dir = Path.GetFullPath(req.Str("dir"));
        var branch = req.Body["branch"]?.GetValue<string>() is { Length: > 0 } b ? b : "public";
        var only = req.Body["depots"]?.AsArray().Select(n => n!.GetValue<uint>()).ToHashSet();

        var owned = await OwnedAppIds(ct);
        if (account != "" && !owned.Contains(appid)) throw new HelperException("not_owned", $"{appid} нет в лицензиях аккаунта");
        var infos = await AppInfos([appid], ct);
        if (!infos.TryGetValue(appid, out var kv)) throw new HelperException("not_found", $"нет appinfo для {appid}");
        var app = AppDepots.From(appid, kv, owned, branch);
        if (app.Branches.FirstOrDefault(x => x.Name == branch) is { PasswordRequired: true })
            throw new HelperException("bad_request", $"ветка {branch} под паролем — не поддерживается");
        var depots = only == null ? app.Install : app.Depots.Where(d => only.Contains(d.Id)).ToList();
        if (depots.Count == 0) throw new HelperException("no_depots", "у игры нет Windows-депо");

        Directory.CreateDirectory(dir);
        var servers = await CdnServers(appid, ct);
        using var cdn = new Client(client);

        // Сначала все манифесты: общий объём известен до первого чанка.
        var jobs = new List<Job>();
        foreach (var d in depots)
        {
            if (!d.Manifests.TryGetValue(branch, out var gid))
                throw new HelperException("not_found", $"у депо {d.Id} нет манифеста ветки {branch}");
            var key = await apps.GetDepotDecryptionKey(d.Id, appid).ToTask().WaitAsync(TimeSpan.FromSeconds(30), ct);
            if (key.Result != EResult.OK) throw new HelperException("access_denied", $"ключ депо {d.Id}: {key.Result}");
            var code = await content.GetManifestRequestCode(d.Id, appid, gid, branch).WaitAsync(TimeSpan.FromSeconds(30), ct);
            var manifest = await WithServers(servers, s => cdn.DownloadManifestAsync(d.Id, gid, code, s, key.DepotKey), ct);
            var files = (manifest.Files ?? []).Where(f => !f.Flags.HasFlag(EDepotFileFlag.Directory)).ToList();
            jobs.Add(new Job(d.Id, manifest, key.DepotKey, files));
            output.Event(req.Id, "depot", new JsonObject
            {
                ["depotid"] = d.Id,
                ["manifest"] = gid.ToString(),
                ["files"] = files.Count,
                ["total"] = files.Sum(f => (long)f.TotalSize),
            });
        }

        long total = jobs.Sum(j => j.Files.Sum(f => (long)f.TotalSize));
        long done = 0;
        var clock = Stopwatch.StartNew();
        var lastReport = TimeSpan.Zero;
        var reportGate = new Lock();
        void Progress(long add, bool force = false)
        {
            var now = Interlocked.Add(ref done, add);
            lock (reportGate)
            {
                if (!force && clock.Elapsed - lastReport < ProgressEvery) return;
                lastReport = clock.Elapsed;
            }
            output.Event(req.Id, "progress", new JsonObject { ["downloaded"] = now, ["total"] = total });
        }
        Progress(0, force: true);

        var skipped = 0;
        foreach (var job in jobs)
        {
            // Каталоги и пустые файлы — сразу; чанки — параллельно.
            foreach (var f in job.Manifest.Files ?? [])
            {
                var path = SafePath(dir, f.FileName);
                if (f.Flags.HasFlag(EDepotFileFlag.Directory)) Directory.CreateDirectory(path);
            }
            var work = new ConcurrentBag<(string Path, DepotManifest.ChunkData Chunk)>();
            foreach (var f in job.Files)
            {
                var path = SafePath(dir, f.FileName);
                Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                if (f.LinkTarget is { Length: > 0 })
                {
                    Log.Write("симлинк в депо пропущен");
                    continue;
                }
                if (await UpToDate(path, f, ct))
                {
                    skipped++;
                    Progress((long)f.TotalSize);
                    continue;
                }
                using (var h = File.OpenHandle(path, FileMode.OpenOrCreate, FileAccess.Write))
                    RandomAccess.SetLength(h, (long)f.TotalSize);
                foreach (var c in f.Chunks) work.Add((path, c));
            }
            await System.Threading.Tasks.Parallel.ForEachAsync(work,
                new ParallelOptions { MaxDegreeOfParallelism = Parallel, CancellationToken = ct },
                async (item, token) =>
                {
                    var buf = new byte[item.Chunk.UncompressedLength];
                    var n = await WithServers(servers,
                        s => cdn.DownloadDepotChunkAsync(job.DepotId, item.Chunk, s, buf, job.Key), token);
                    using var h = File.OpenHandle(item.Path, FileMode.Open, FileAccess.Write, FileShare.ReadWrite);
                    await RandomAccess.WriteAsync(h, buf.AsMemory(0, n), (long)item.Chunk.Offset, token);
                    Progress(n);
                });
        }
        Progress(0, force: true);
        Log.Write($"загрузка {appid} готова, файлов без изменений: {skipped}");
        return new JsonObject
        {
            ["appid"] = appid,
            ["dir"] = dir,
            ["downloaded"] = done,
            ["files"] = jobs.Sum(j => j.Files.Count),
            ["unchanged"] = skipped,
            ["depots"] = new JsonArray(jobs.Select(j => (JsonNode)new JsonObject
            {
                ["depotid"] = j.DepotId,
                ["manifest"] = j.Manifest.ManifestGID.ToString(),
            }).ToArray()),
        };
    }

    /// Путь файла из манифеста внутри `dir`: без абсолютных путей и `..`.
    static string SafePath(string dir, string name)
    {
        var rel = name.Replace('\\', '/').TrimStart('/');
        if (rel.Split('/').Any(p => p is ".." or "."))
            throw new HelperException("bad_manifest", "путь в манифесте выходит за папку игры");
        var full = Path.GetFullPath(Path.Combine(dir, rel));
        if (!full.StartsWith(dir.TrimEnd('/') + "/", StringComparison.Ordinal))
            throw new HelperException("bad_manifest", "путь в манифесте выходит за папку игры");
        return full;
    }

    /// Файл уже скачан: тот же размер и SHA-1 из манифеста.
    static async Task<bool> UpToDate(string path, DepotManifest.FileData f, CancellationToken ct)
    {
        var info = new FileInfo(path);
        if (!info.Exists || info.Length != (long)f.TotalSize) return false;
        await using var s = File.OpenRead(path);
        var sha = await SHA1.HashDataAsync(s, ct);
        return sha.AsSpan().SequenceEqual(f.FileHash);
    }

    async Task<List<Server>> CdnServers(uint appid, CancellationToken ct)
    {
        var all = await content.GetServersForSteamPipe().WaitAsync(TimeSpan.FromSeconds(30), ct);
        var list = all
            .Where(s => s.Type is "SteamCache" or "CDN" && !s.SteamChinaOnly && !s.UseAsProxy)
            .Where(s => s.AllowedAppIds.Length == 0 || s.AllowedAppIds.Contains(appid))
            .OrderBy(s => s.WeightedLoad)
            .ToList();
        if (list.Count == 0) throw new HelperException("network", "нет CDN-серверов Steam");
        return list;
    }

    /// Запрос к CDN с повторами на других серверах.
    static async Task<T> WithServers<T>(List<Server> servers, Func<Server, Task<T>> f, CancellationToken ct)
    {
        Exception? last = null;
        for (var i = 0; i < ChunkRetries; i++)
        {
            ct.ThrowIfCancellationRequested();
            var s = servers[(Random.Shared.Next(Math.Min(3, servers.Count)) + i) % servers.Count];
            try
            {
                return await f(s).WaitAsync(ct);
            }
            catch (Exception e) when (e is HttpRequestException or SteamKitWebRequestException or TaskCanceledException
                                          or IOException or InvalidDataException && !ct.IsCancellationRequested)
            {
                last = e;
                await Task.Delay(TimeSpan.FromMilliseconds(300 * (i + 1)), ct);
            }
        }
        throw new HelperException("network", $"CDN Steam не отдал данные: {last?.GetType().Name}");
    }
}
