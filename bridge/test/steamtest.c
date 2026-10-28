/* steamtest: проверка моста на Spacewar (appid 480, тестовое приложение Valve).
 * Один исходник собирается под Mac (dlopen libsteam_api.dylib) и под Windows (LoadLibrary
 * steam_api64.dll — под Wine это заглушка моста). Печатает строки «ключ = значение»: у обеих
 * сборок они должны совпасть. Строки с «#» — справка, в сравнение не идут.
 * Игровое состояние Spacewar возвращается как было, кроме счётчика NumGames и записи в лидерборде. */
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
static HMODULE lib;
static bool open_lib(const char *path) { return (lib = LoadLibraryA(path ? path : "steam_api64.dll")) != NULL; }
static void *sym(const char *n) { return (void *)GetProcAddress(lib, n); }
static void sleep_ms(int ms) { Sleep(ms); }
#pragma pack(push, 8)
#else
#include <dlfcn.h>
#include <unistd.h>
static void *lib;
static bool open_lib(const char *path) { return (lib = dlopen(path ? path : "libsteam_api.dylib", RTLD_NOW)) != NULL; }
static void *sym(const char *n) { return dlsym(lib, n); }
static void sleep_ms(int ms) { usleep(ms * 1000); }
#pragma pack(push, 4)
#endif

/* Структуры колбэков в раскладке своей платформы (как в заголовках SDK) — их и проверяем. */
typedef struct { uint64_t call; int32_t id; uint32_t cub; } SteamAPICallCompleted_t;     /* 703 */
typedef struct { uint64_t game; int32_t result; uint64_t user; } UserStatsReceived_t;   /* 1101 */
typedef struct { uint64_t game; int32_t result; } UserStatsStored_t;                    /* 1102 */
typedef struct { uint64_t board; uint8_t found; } LeaderboardFindResult_t;              /* 1104 */
typedef struct { uint64_t board; uint64_t entries; int32_t count; } LeaderboardScoresDownloaded_t; /* 1105 */
typedef struct { uint8_t ok; uint64_t board; int32_t score; uint8_t changed; int32_t rank_new, rank_prev; } LeaderboardScoreUploaded_t; /* 1106 */
typedef struct { uint64_t user; int32_t rank; int32_t score; int32_t details; uint64_t ugc; } LeaderboardEntry_t;
#pragma pack(pop)
typedef struct { int32_t user; int32_t id; uint8_t *param; int32_t size; } CallbackMsg_t;

#define FNS(X)                                                                                   \
  X(int, SteamAPI_InitFlat, (char *))                                                            \
  X(void, SteamAPI_Shutdown, (void))                                                             \
  X(int32_t, SteamAPI_GetHSteamPipe, (void))                                                     \
  X(void, SteamAPI_ManualDispatch_Init, (void))                                                  \
  X(void, SteamAPI_ManualDispatch_RunFrame, (int32_t))                                           \
  X(bool, SteamAPI_ManualDispatch_GetNextCallback, (int32_t, CallbackMsg_t *))                   \
  X(void, SteamAPI_ManualDispatch_FreeLastCallback, (int32_t))                                   \
  X(bool, SteamAPI_ManualDispatch_GetAPICallResult, (int32_t, uint64_t, void *, int, int, bool *)) \
  X(void *, SteamAPI_SteamUser_v023, (void))                                                     \
  X(void *, SteamAPI_SteamFriends_v017, (void))                                                  \
  X(void *, SteamAPI_SteamUtils_v010, (void))                                                    \
  X(void *, SteamAPI_SteamUserStats_v012, (void))                                                \
  X(void *, SteamAPI_SteamRemoteStorage_v016, (void))                                            \
  X(bool, SteamAPI_ISteamUser_BLoggedOn, (void *))                                               \
  X(uint64_t, SteamAPI_ISteamUser_GetSteamID, (void *))                                          \
  X(uint32_t, SteamAPI_ISteamUtils_GetAppID, (void *))                                           \
  X(const char *, SteamAPI_ISteamUtils_GetIPCountry, (void *))                                   \
  X(bool, SteamAPI_ISteamUtils_GetImageSize, (void *, int, uint32_t *, uint32_t *))              \
  X(bool, SteamAPI_ISteamUtils_GetImageRGBA, (void *, int, uint8_t *, int))                      \
  X(bool, SteamAPI_ISteamUtils_IsAPICallCompleted, (void *, uint64_t, bool *))                   \
  X(bool, SteamAPI_ISteamUtils_GetAPICallResult, (void *, uint64_t, void *, int, int, bool *))   \
  X(const char *, SteamAPI_ISteamFriends_GetPersonaName, (void *))                               \
  X(int, SteamAPI_ISteamFriends_GetMediumFriendAvatar, (void *, uint64_t))                       \
  X(bool, SteamAPI_ISteamUserStats_RequestCurrentStats, (void *))                                \
  X(uint32_t, SteamAPI_ISteamUserStats_GetNumAchievements, (void *))                             \
  X(const char *, SteamAPI_ISteamUserStats_GetAchievementName, (void *, uint32_t))               \
  X(const char *, SteamAPI_ISteamUserStats_GetAchievementDisplayAttribute, (void *, const char *, const char *)) \
  X(bool, SteamAPI_ISteamUserStats_GetAchievement, (void *, const char *, bool *))               \
  X(bool, SteamAPI_ISteamUserStats_SetAchievement, (void *, const char *))                       \
  X(bool, SteamAPI_ISteamUserStats_ClearAchievement, (void *, const char *))                     \
  X(bool, SteamAPI_ISteamUserStats_GetStatInt32, (void *, const char *, int32_t *))              \
  X(bool, SteamAPI_ISteamUserStats_SetStatInt32, (void *, const char *, int32_t))                \
  X(bool, SteamAPI_ISteamUserStats_StoreStats, (void *))                                         \
  X(uint64_t, SteamAPI_ISteamUserStats_RequestUserStats, (void *, uint64_t))                     \
  X(bool, SteamAPI_ISteamUserStats_GetUserAchievement, (void *, uint64_t, const char *, bool *)) \
  X(bool, SteamAPI_ISteamUserStats_GetUserStatInt32, (void *, uint64_t, const char *, int32_t *)) \
  X(uint64_t, SteamAPI_ISteamUserStats_FindLeaderboard, (void *, const char *))                  \
  X(const char *, SteamAPI_ISteamUserStats_GetLeaderboardName, (void *, uint64_t))               \
  X(uint64_t, SteamAPI_ISteamUserStats_UploadLeaderboardScore, (void *, uint64_t, int, int32_t, const int32_t *, int)) \
  X(uint64_t, SteamAPI_ISteamUserStats_DownloadLeaderboardEntries, (void *, uint64_t, int, int, int)) \
  X(bool, SteamAPI_ISteamUserStats_GetDownloadedLeaderboardEntry, (void *, uint64_t, int, LeaderboardEntry_t *, int32_t *, int)) \
  X(bool, SteamAPI_ISteamRemoteStorage_IsCloudEnabledForAccount, (void *))                       \
  X(bool, SteamAPI_ISteamRemoteStorage_FileWrite, (void *, const char *, const void *, int32_t)) \
  X(int32_t, SteamAPI_ISteamRemoteStorage_FileRead, (void *, const char *, void *, int32_t))     \
  X(bool, SteamAPI_ISteamRemoteStorage_FileExists, (void *, const char *))                       \
  X(int32_t, SteamAPI_ISteamRemoteStorage_GetFileSize, (void *, const char *))                   \
  X(bool, SteamAPI_ISteamRemoteStorage_FileDelete, (void *, const char *))

#define DECL(ret, name, args) static ret(*name) args;
FNS(DECL)

static int failures;
static int32_t pipe_;
static void *user, *friends, *utils, *stats, *storage;

#define SAY(...) (printf(__VA_ARGS__), putchar('\n'), fflush(stdout))
#define CHECK(key, cond) do { bool ok_ = (cond); if (!ok_) failures++; SAY("%s = %s", key, ok_ ? "ok" : "FAIL"); } while (0)

/* Отпечаток вместо Steam ID и ника: сравнивать можно, светить нечего. */
static uint32_t fnv(const void *p, size_t n) {
  uint32_t h = 2166136261u;
  for (size_t i = 0; i < n; i++) h = (h ^ ((const uint8_t *)p)[i]) * 16777619u;
  return h;
}

/* Колбэки (не call result) запоминаем по id: последний экземпляр каждого. */
static struct { int32_t id; uint8_t data[512]; int32_t size; bool fresh; } seen[16];

static void pump(uint64_t want_call, void *out, int cub, int expected, bool *done, bool *failed) {
  CallbackMsg_t m;
  SteamAPI_ManualDispatch_RunFrame(pipe_);
  while (SteamAPI_ManualDispatch_GetNextCallback(pipe_, &m)) {
    if (m.id == 703 && want_call) {
      SteamAPICallCompleted_t c;
      memcpy(&c, m.param, sizeof c);
      if (c.call == want_call) {
        if ((int)c.cub != cub) SAY("# 703: размер результата %u, ждали %d", c.cub, cub);
        *done = SteamAPI_ManualDispatch_GetAPICallResult(pipe_, c.call, out, cub, expected, failed);
      }
    } else if (m.id != 703) {
      int k = 0;
      while (k < 15 && seen[k].id && seen[k].id != m.id) k++;
      seen[k].id = m.id;
      seen[k].size = m.size < 512 ? m.size : 512;
      memcpy(seen[k].data, m.param, (size_t)seen[k].size);
      seen[k].fresh = true;
    }
    SteamAPI_ManualDispatch_FreeLastCallback(pipe_);
  }
}

/* Call result через ManualDispatch (так делает Steamworks.NET). */
static bool wait_call(uint64_t call, void *out, int cub, int expected) {
  bool done = false, failed = true;
  for (int i = 0; call && i < 300 && !done; i++) { pump(call, out, cub, expected, &done, &failed); if (!done) sleep_ms(50); }
  return done && !failed;
}

/* Call result через ISteamUtils::GetAPICallResult (так делают C++-игры). */
static bool wait_call_utils(uint64_t call, void *out, int cub, int expected) {
  bool failed = true, d, f;
  for (int i = 0; call && i < 300; i++) {
    pump(0, NULL, 0, 0, &d, &f);
    if (SteamAPI_ISteamUtils_IsAPICallCompleted(utils, call, &failed))
      return SteamAPI_ISteamUtils_GetAPICallResult(utils, call, out, cub, expected, &failed) && !failed;
    sleep_ms(50);
  }
  return false;
}

static void *wait_cb(int32_t id) {
  bool d, f;
  for (int i = 0; i < 300; i++) {
    pump(0, NULL, 0, 0, &d, &f);
    for (int k = 0; k < 16; k++)
      if (seen[k].id == id && seen[k].fresh) { seen[k].fresh = false; return seen[k].data; }
    sleep_ms(50);
  }
  return NULL;
}

static void forget_cb(int32_t id) {
  for (int k = 0; k < 16; k++) if (seen[k].id == id) seen[k].fresh = false;
}

/* Серверное состояние своего пользователя: RequestUserStats обходит локальный кэш клиента. */
static bool server_stats(uint64_t me) {
  UserStatsReceived_t r = {0};
  return wait_call_utils(SteamAPI_ISteamUserStats_RequestUserStats(stats, me), &r, sizeof r, 1101) && r.result == 1;
}

static bool store(void) {
  forget_cb(1102);
  if (!SteamAPI_ISteamUserStats_StoreStats(stats)) return false;
  UserStatsStored_t *s = (UserStatsStored_t *)wait_cb(1102);
  return s && s->result == 1;
}

static void test_identity(uint64_t *me) {
  CHECK("user.logged_on", SteamAPI_ISteamUser_BLoggedOn(user));
  *me = SteamAPI_ISteamUser_GetSteamID(user);
  SAY("user.steamid = %08x", fnv(me, sizeof *me));
  SAY("utils.appid = %u", SteamAPI_ISteamUtils_GetAppID(utils));
  SAY("utils.country = %s", SteamAPI_ISteamUtils_GetIPCountry(utils));
  const char *name = SteamAPI_ISteamFriends_GetPersonaName(friends);
  SAY("friends.persona = %08x", name ? fnv(name, strlen(name)) : 0);

  int img = -1;
  bool d, f;
  for (int i = 0; i < 100 && img == -1; i++) {
    img = SteamAPI_ISteamFriends_GetMediumFriendAvatar(friends, *me);
    if (img == -1) { pump(0, NULL, 0, 0, &d, &f); sleep_ms(50); }
  }
  uint32_t w = 0, h = 0;
  if (img > 0 && SteamAPI_ISteamUtils_GetImageSize(utils, img, &w, &h) && w && h && w * h <= 512 * 512) {
    uint8_t *px = (uint8_t *)malloc(w * h * 4);
    bool ok = SteamAPI_ISteamUtils_GetImageRGBA(utils, img, px, (int)(w * h * 4));
    SAY("friends.avatar = %ux%u %08x", w, h, ok ? fnv(px, w * h * 4) : 0);
    free(px);
  } else {
    SAY("friends.avatar = нет (%d)", img);
  }
}

static void test_stats(uint64_t me) {
  forget_cb(1101);
  bool req = SteamAPI_ISteamUserStats_RequestCurrentStats(stats);
  UserStatsReceived_t *r = (UserStatsReceived_t *)wait_cb(1101);
  CHECK("stats.received", req && r && r->result == 1 && r->user == me);

  uint32_t n = SteamAPI_ISteamUserStats_GetNumAchievements(stats);
  SAY("stats.achievements = %u", n);
  for (uint32_t i = 0; i < n; i++) {
    const char *a = SteamAPI_ISteamUserStats_GetAchievementName(stats, i);
    const char *t = a ? SteamAPI_ISteamUserStats_GetAchievementDisplayAttribute(stats, a, "name") : NULL;
    SAY("stats.ach.%u = %s \"%s\"", i, a ? a : "(null)", t ? t : "(null)");
  }

  /* Ачивка: переключить, проверить на сервере, вернуть как было. */
  const char *ach = n ? SteamAPI_ISteamUserStats_GetAchievementName(stats, 0) : NULL;
  bool was = false;
  if (ach && SteamAPI_ISteamUserStats_GetAchievement(stats, ach, &was)) {
    SAY("# %s сейчас %s", ach, was ? "получена" : "не получена");
    bool flip = was ? SteamAPI_ISteamUserStats_ClearAchievement(stats, ach) : SteamAPI_ISteamUserStats_SetAchievement(stats, ach);
    bool srv = !was;
    CHECK("stats.ach_flip.stored", flip && store());
    CHECK("stats.ach_flip.on_server", server_stats(me) && SteamAPI_ISteamUserStats_GetUserAchievement(stats, me, ach, &srv) && srv == !was);
    bool back = was ? SteamAPI_ISteamUserStats_SetAchievement(stats, ach) : SteamAPI_ISteamUserStats_ClearAchievement(stats, ach);
    srv = !was;
    CHECK("stats.ach_restore.stored", back && store());
    CHECK("stats.ach_restore.on_server", server_stats(me) && SteamAPI_ISteamUserStats_GetUserAchievement(stats, me, ach, &srv) && srv == was);
  } else {
    CHECK("stats.ach_read", false);
  }

  /* Стат: +1 к NumGames, проверить на сервере. */
  int32_t games = -1, srv_games = -1;
  bool got = SteamAPI_ISteamUserStats_GetStatInt32(stats, "NumGames", &games);
  SAY("# NumGames было %d", games);
  CHECK("stats.set.stored", got && SteamAPI_ISteamUserStats_SetStatInt32(stats, "NumGames", games + 1) && store());
  CHECK("stats.set.on_server", server_stats(me) && SteamAPI_ISteamUserStats_GetUserStatInt32(stats, me, "NumGames", &srv_games) && srv_games == games + 1);
}

static void test_leaderboard(uint64_t me) {
  LeaderboardFindResult_t fr = {0};
  bool found = wait_call(SteamAPI_ISteamUserStats_FindLeaderboard(stats, "Feet Traveled"), &fr, sizeof fr, 1104) && fr.found;
  CHECK("lb.find", found);
  if (!found) return;
  const char *name = SteamAPI_ISteamUserStats_GetLeaderboardName(stats, fr.board);
  SAY("lb.name = %s", name ? name : "(null)");

  /* KeepBest с минимальным счётом: результат игрока не портим, но запрос и колбэк проходят. */
  const int32_t details[3] = {4, 8, 15};
  LeaderboardScoreUploaded_t up = {0};
  bool sent = wait_call(SteamAPI_ISteamUserStats_UploadLeaderboardScore(stats, fr.board, 1, 1, details, 3), &up, sizeof up, 1106);
  CHECK("lb.upload", sent && up.ok && up.board == fr.board && up.score == 1);
  SAY("# загрузка: changed=%u rank %d -> %d", up.changed, up.rank_prev, up.rank_new);

  LeaderboardScoresDownloaded_t dl = {0};
  bool got = wait_call(SteamAPI_ISteamUserStats_DownloadLeaderboardEntries(stats, fr.board, 1, 0, 0), &dl, sizeof dl, 1105);
  CHECK("lb.download", got && dl.board == fr.board && dl.count >= 1);
  LeaderboardEntry_t e = {0};
  int32_t det[8] = {0};
  for (int i = 0; got && i < dl.count; i++) {
    if (!SteamAPI_ISteamUserStats_GetDownloadedLeaderboardEntry(stats, dl.entries, i, &e, det, 8) || e.user != me) continue;
    SAY("lb.me = score %d, details %d [%d %d %d]", e.score, e.details, det[0], det[1], det[2]);
    CHECK("lb.me.rank", e.rank >= 1);
    return;
  }
  CHECK("lb.me", false);
}

static void test_cloud(void) {
  SAY("cloud.account_enabled = %d", SteamAPI_ISteamRemoteStorage_IsCloudEnabledForAccount(storage));
  const char *file = "carafe-bridge-test.bin";
  uint8_t data[3000], back[4000];
  for (int i = 0; i < (int)sizeof data; i++) data[i] = (uint8_t)(i * 7 + 3);
  CHECK("cloud.write", SteamAPI_ISteamRemoteStorage_FileWrite(storage, file, data, sizeof data));
  CHECK("cloud.exists", SteamAPI_ISteamRemoteStorage_FileExists(storage, file));
  SAY("cloud.size = %d", SteamAPI_ISteamRemoteStorage_GetFileSize(storage, file));
  int32_t n = SteamAPI_ISteamRemoteStorage_FileRead(storage, file, back, sizeof back);
  CHECK("cloud.read", n == (int32_t)sizeof data && !memcmp(back, data, sizeof data));
  CHECK("cloud.delete", SteamAPI_ISteamRemoteStorage_FileDelete(storage, file) && !SteamAPI_ISteamRemoteStorage_FileExists(storage, file));
}

int main(int argc, char **argv) {
  setvbuf(stdout, NULL, _IONBF, 0);
  if (!open_lib(argc > 1 ? argv[1] : NULL)) { SAY("load = FAIL"); return 2; }
#define LOAD(ret, name, args)                                   \
  if (!(name = (ret(*) args)sym(#name))) { SAY("sym.%s = FAIL", #name); return 2; }
  FNS(LOAD)

  char err[1024] = {0};
  int init = SteamAPI_InitFlat(err);
  SAY("init = %d", init);
  if (init != 0) { SAY("# %s", err); return 1; }
  SteamAPI_ManualDispatch_Init();
  pipe_ = SteamAPI_GetHSteamPipe();
  user = SteamAPI_SteamUser_v023();
  friends = SteamAPI_SteamFriends_v017();
  utils = SteamAPI_SteamUtils_v010();
  stats = SteamAPI_SteamUserStats_v012();
  storage = SteamAPI_SteamRemoteStorage_v016();
  CHECK("interfaces", user && friends && utils && stats && storage);

  uint64_t me = 0;
  test_identity(&me);
  test_stats(me);
  test_leaderboard(me);
  test_cloud();

  SteamAPI_Shutdown();
  SAY("# провалов: %d", failures);
  return failures ? 1 : 0;
}
