/* C++-путь Steamworks, как у игр на Unreal и своих движках: интерфейсы через vtable,
 * колбэки через CCallback/CCallResult и SteamAPI_RunCallbacks.
 * Собирается дважды: clang++ под Mac (Itanium ABI, настоящий libsteam_api) и clang под
 * x86_64-pc-windows-msvc (ABI MSVC, как у игр) — в steamtest.exe, который под Wine идёт через мост.
 * Без CRT и исключений: всё внешнее приходит из steamtest.c через cpp_api. */
#include "steam_iface.hpp"

extern "C" {
struct cpp_api {
  void *(*FindOrCreateUserInterface)(int32, const char *);
  void *(*CreateInterface)(const char *);
  void *(*ContextInit)(void *);
  int32 (*GetHSteamUser)(void);
  int32 (*GetHSteamPipe)(void);
  void (*RunCallbacks)(void);
  void (*RegisterCallback)(CCallbackBase *, int);
  void (*UnregisterCallback)(CCallbackBase *);
  void (*RegisterCallResult)(CCallbackBase *, SteamAPICall_t);
  void (*UnregisterCallResult)(CCallbackBase *, SteamAPICall_t);
  void (*check)(const char *key, bool ok);
  void (*say)(const char *fmt, ...);
  void (*sleep_ms)(int);
  uint32 (*hash)(const void *p, uint32 n);
};
int cpp_tests(const struct cpp_api *api);
}

static const cpp_api *A;

#ifdef _WIN32
#pragma pack(push, 8)
#else
#pragma pack(push, 4)
#endif
struct StatsReceived { uint64 game; int32 result; uint64 user; };  /* UserStatsReceived_t, 1101 */
struct BoardFound { uint64 board; uint8 found; };                   /* LeaderboardFindResult_t, 1104 */
#pragma pack(pop)

static uint32 slen(const char *s) { uint32 n = 0; while (s && s[n]) n++; return n; }

/* Как _STEAM_DEFINE_INTERFACE_ACCESSOR в steam_api_internal.h. */
static void init_user(ISteamUser **p) {
  *p = (ISteamUser *)A->FindOrCreateUserInterface(A->GetHSteamUser(), VER_ISteamUser);
}
static ISteamUser *SteamUser_() {
  static void *ctx[3] = {(void *)&init_user, 0, 0};
  return *(ISteamUser **)A->ContextInit(ctx);
}

struct StatsCallback : CCallbackBase {
  bool got = false;
  StatsReceived data = {};
  void Run(void *p) override { data = *(StatsReceived *)p; got = true; }
  void Run(void *p, bool, SteamAPICall_t) override { Run(p); }
  int GetCallbackSizeBytes() override { return sizeof(StatsReceived); }
};

struct FindResult : CCallbackBase {
  bool got = false, failed = true;
  SteamAPICall_t call = 0;
  BoardFound data = {};
  FindResult() { m_iCallback = 1104; }
  void Run(void *p) override { data = *(BoardFound *)p; got = true; failed = false; }
  void Run(void *p, bool io_failure, SteamAPICall_t h) override {
    data = *(BoardFound *)p;
    failed = io_failure;
    call = h;
    got = true;
  }
  int GetCallbackSizeBytes() override { return sizeof(BoardFound); }
};

static bool pump(const bool *flag) {
  for (int i = 0; i < 300 && !*flag; i++) {
    A->RunCallbacks();
    if (!*flag) A->sleep_ms(50);
  }
  return *flag;
}

int cpp_tests(const struct cpp_api *api) {
  A = api;
  int32 hu = A->GetHSteamUser(), hp = A->GetHSteamPipe();

  /* Инлайновый аксессор SDK: SteamInternal_ContextInit + FindOrCreateUserInterface. */
  ISteamUser *user = SteamUser_();
  A->check("cpp.accessor", user != nullptr && SteamUser_() == user);
  if (!user) return 1;
  A->check("cpp.user.logged_on", user->BLoggedOn());
  CSteamID id = user->GetSteamID();  /* у MSVC — через скрытый указатель на результат */
  A->say("cpp.user.steamid = %08x", A->hash(&id, sizeof id));

  /* ISteamClient той версии, что у SDK игры; интерфейсы — через его методы. */
  ISteamClient *client = (ISteamClient *)A->CreateInterface(VER_ISteamClient);
  A->check("cpp.client", client != nullptr);
  if (!client) return 1;
  ISteamUtils *utils = client->GetISteamUtils(hp, VER_ISteamUtils);
  ISteamFriends *friends = client->GetISteamFriends(hu, hp, VER_ISteamFriends);
  A->check("cpp.client.interfaces", utils && friends);
  if (!utils || !friends) return 1;
  A->say("cpp.utils.appid = %u", utils->GetAppID());
  const char *name = friends->GetPersonaName();
  A->say("cpp.friends.persona = %08x", A->hash(name, slen(name)));

  /* CCallback: UserStatsReceived_t после RequestCurrentStats. */
  ISteamUserStats *stats = (ISteamUserStats *)A->FindOrCreateUserInterface(hu, VER_ISteamUserStats);
  A->check("cpp.stats.interface", stats != nullptr);
  if (!stats) return 1;
  StatsCallback cb;
  A->RegisterCallback(&cb, 1101);
  bool req = stats->RequestCurrentStats();
  bool got = pump(&cb.got);
  A->check("cpp.callback", req && got && cb.data.result == 1 && cb.data.user == id.m_steamid);
  A->UnregisterCallback(&cb);

  /* Перегрузки GetStat: у MSVC они в vtable в обратном порядке. Не та — вернёт false. */
  int32 games = -1;
  float feet = -1;
  bool gi = stats->GetStat("NumGames", &games);
  bool gf = stats->GetStat("FeetTraveled", &feet);
  A->check("cpp.stats.overloads", gi && gf && games >= 0 && feet >= 0);
  A->say("cpp.stats.numgames = %d", games);
  A->say("cpp.stats.feet = %d", (int)feet);

  /* CCallResult: FindLeaderboard. */
  FindResult fr;
  SteamAPICall_t call = stats->FindLeaderboard("Feet Traveled");
  A->RegisterCallResult(&fr, call);
  got = pump(&fr.got);
  A->check("cpp.callresult", got && !fr.failed && fr.call == call && fr.data.found);
  const char *board = got ? stats->GetLeaderboardName(fr.data.board) : nullptr;
  A->say("cpp.lb.name = %s", board ? board : "(null)");
  return 0;
}
