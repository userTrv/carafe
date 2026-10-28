/* steam_api64.dll для Wine: каждый вызов Steamworks уходит в carafe-steamd на Mac. */
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <stdio.h>

#include "shim.h"

static SOCKET g_sock = INVALID_SOCKET;
static bool g_dead;
static CRITICAL_SECTION g_lock;
static INIT_ONCE g_once = INIT_ONCE_STATIC_INIT;

static BOOL CALLBACK init_lock(PINIT_ONCE o, PVOID p, PVOID *c) {
  (void)o; (void)p; (void)c;
  InitializeCriticalSection(&g_lock);
  return TRUE;
}

static void logf_(const char *fmt, const char *arg) {
  fprintf(stderr, "[carafe-bridge] ");
  fprintf(stderr, fmt, arg);
  fprintf(stderr, "\n");
  fflush(stderr);
}

static bool send_all(const void *p, int n) {
  const char *c = (const char *)p;
  while (n > 0) {
    int k = send(g_sock, c, n, 0);
    if (k <= 0) return false;
    c += k; n -= k;
  }
  return true;
}

static bool recv_all(void *p, int n) {
  char *c = (char *)p;
  while (n > 0) {
    int k = recv(g_sock, c, n, 0);
    if (k <= 0) return false;
    c += k; n -= k;
  }
  return true;
}

static bool roundtrip(bw *w, br *r);

static bool connect_bridge(void) {
  char port[16], token[128];
  if (!GetEnvironmentVariableA("CARAFE_BRIDGE_PORT", port, sizeof port) ||
      !GetEnvironmentVariableA("CARAFE_BRIDGE_TOKEN", token, sizeof token)) {
    logf_("%s", "CARAFE_BRIDGE_PORT/TOKEN не заданы — Steam недоступен");
    return false;
  }
  WSADATA wsa;
  if (WSAStartup(MAKEWORD(2, 2), &wsa)) return false;
  g_sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
  if (g_sock == INVALID_SOCKET) return false;
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_port = htons((u_short)atoi(port));
  a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  if (connect(g_sock, (struct sockaddr *)&a, sizeof a)) {
    logf_("не подключиться к хелперу на порту %s", port);
    closesocket(g_sock);
    g_sock = INVALID_SOCKET;
    return false;
  }
  int one = 1;
  setsockopt(g_sock, IPPROTO_TCP, TCP_NODELAY, (const char *)&one, sizeof one);
  bw w; br r;
  bw_begin(&w, CORE_HELLO);
  bw_str(&w, token);
  if (!roundtrip(&w, &r)) return false;
  bool ok = br_u64(&r) == 1;
  bridge_done(&r);
  return ok;
}

static bool roundtrip(bw *w, br *r) {
  bw_finish(w);
  uint32_t len = 0;
  bool ok = send_all(w->p, (int)w->n) && recv_all(&len, 4) && len <= WIRE_MAX;
  free(w->p);
  uint8_t *buf = NULL;
  if (ok) {
    buf = (uint8_t *)malloc(len ? len : 1);
    ok = recv_all(buf, (int)len);
  }
  if (!ok) {
    free(buf);
    logf_("%s", "связь с хелпером потеряна");
    if (g_sock != INVALID_SOCKET) closesocket(g_sock);
    g_sock = INVALID_SOCKET;
    g_dead = true;
    return false;
  }
  r->p = buf; r->n = len; r->off = 0; r->err = false;
  return true;
}

bool bridge_call(bw *w, br *r) {
  InitOnceExecuteOnce(&g_once, init_lock, NULL, NULL);
  EnterCriticalSection(&g_lock);
  bool ok = false;
  if (!g_dead && g_sock == INVALID_SOCKET && !connect_bridge()) g_dead = true;
  if (!g_dead) ok = roundtrip(w, r);
  else free(w->p);
  LeaveCriticalSection(&g_lock);
  return ok;
}

void bridge_done(br *r) { free((void *)r->p); }

/* Интернирование: строк от Steam немного (имена ачивок, ники), держим все. */
typedef struct istr { struct istr *next; char s[]; } istr;
static istr *g_strs[1024];

const char *bridge_intern(const char *s) {
  if (!s) return NULL;
  uint32_t h = 2166136261u;
  for (const char *c = s; *c; c++) h = (h ^ (uint8_t)*c) * 16777619u;
  istr **b = &g_strs[h % 1024];
  EnterCriticalSection(&g_lock);
  for (istr *e = *b; e; e = e->next)
    if (!strcmp(e->s, s)) { LeaveCriticalSection(&g_lock); return e->s; }
  size_t n = strlen(s) + 1;
  istr *e = (istr *)malloc(sizeof *e + n);
  memcpy(e->s, s, n);
  e->next = *b;
  *b = e;
  LeaveCriticalSection(&g_lock);
  return e->s;
}

void bridge_unsupported(const char *name) {
  static const char *seen[256];
  static int n;
  for (int i = 0; i < n; i++) if (seen[i] == name) return;
  if (n < 256) seen[n++] = name;
  logf_("не поддержано: %s", name);
}

/* --- ядро --- */

static int32_t do_init(const char *versions, char *err) {
  bw w; br r;
  bw_begin(&w, CORE_INIT);
  bw_str(&w, versions);
  if (!bridge_call(&w, &r)) {
    if (err) strcpy(err, "carafe: нет связи с carafe-steamd");
    return 2; /* k_ESteamAPIInitResult_NoSteamClient */
  }
  int32_t res = (int32_t)br_u64(&r);
  const char *msg = br_str(&r);
  if (err) { strncpy(err, msg ? msg : "", 1023); err[1023] = 0; }
  bridge_done(&r);
  return res;
}

EXPORT int32_t SteamInternal_SteamAPI_Init(const char *versions, char *err) { return do_init(versions, err); }
EXPORT int32_t SteamAPI_InitFlat(char *err) { return do_init(NULL, err); }
EXPORT bool SteamAPI_Init(void) { return do_init(NULL, NULL) == 0; }
EXPORT bool SteamAPI_InitSafe(void) { return do_init(NULL, NULL) == 0; }
/* Перезапуск через Steam сломал бы запуск под carafe. */
EXPORT bool SteamAPI_RestartAppIfNecessary(uint32_t app) { (void)app; return false; }
EXPORT void SteamAPI_ReleaseCurrentThreadMemory(void) {}

static uint64_t simple(uint32_t id, bool with_arg, uint64_t arg) {
  bw w; br r;
  bw_begin(&w, id);
  if (with_arg) bw_u64(&w, arg);
  if (!bridge_call(&w, &r)) return 0;
  uint64_t v = r.n >= 8 ? br_u64(&r) : 0;
  bridge_done(&r);
  return v;
}

EXPORT void SteamAPI_Shutdown(void) { simple(CORE_SHUTDOWN, false, 0); }
EXPORT bool SteamAPI_IsSteamRunning(void) { return !g_dead; }
EXPORT int32_t SteamAPI_GetHSteamPipe(void) { return (int32_t)simple(CORE_GET_PIPE, false, 0); }
EXPORT int32_t SteamAPI_GetHSteamUser(void) { return (int32_t)simple(CORE_GET_USER, false, 0); }
EXPORT void SteamAPI_RunCallbacks(void) { simple(CORE_RUN_CALLBACKS, false, 0); }
EXPORT void SteamAPI_ManualDispatch_Init(void) { simple(CORE_MD_INIT, false, 0); }
EXPORT void SteamAPI_ManualDispatch_RunFrame(int32_t pipe) { simple(CORE_MD_RUN_FRAME, true, (uint64_t)pipe); }

EXPORT const char *SteamAPI_GetSteamInstallPath(void) {
  bw w; br r;
  bw_begin(&w, CORE_INSTALL_PATH);
  if (!bridge_call(&w, &r)) return "";
  const char *s = bridge_intern(br_str(&r));
  bridge_done(&r);
  return s ? s : "";
}

/* CallbackMsg_t в раскладке Windows. */
typedef struct { int32_t user; int32_t id; uint8_t *param; int32_t size; } CallbackMsg_t;
static uint8_t *g_cb;

EXPORT bool SteamAPI_ManualDispatch_GetNextCallback(int32_t pipe, CallbackMsg_t *msg) {
  bw w; br r;
  bw_begin(&w, CORE_MD_NEXT);
  bw_u64(&w, (uint64_t)pipe);
  if (!bridge_call(&w, &r)) return false;
  bool has = br_u8(&r);
  if (has) {
    int32_t user = (int32_t)br_u64(&r), id = (int32_t)br_u64(&r);
    uint32_t n;
    const void *p = br_buf_len(&r, &n);
    free(g_cb);
    g_cb = (uint8_t *)malloc(n ? n : 1);
    if (p) memcpy(g_cb, p, n);
    msg->user = user; msg->id = id; msg->param = g_cb; msg->size = (int32_t)n;
  }
  bridge_done(&r);
  return has;
}

EXPORT void SteamAPI_ManualDispatch_FreeLastCallback(int32_t pipe) {
  simple(CORE_MD_FREE, true, (uint64_t)pipe);
  free(g_cb);
  g_cb = NULL;
}

static bool call_result(uint32_t core, void *self, int32_t pipe, uint64_t call, void *out, int32_t cub,
                        int32_t expected, bool *failed) {
  bw w; br r;
  bw_begin(&w, core);
  if (core == CORE_UTILS_CALL_RESULT) bw_u64(&w, (uint64_t)(uintptr_t)self);
  else bw_u64(&w, (uint64_t)pipe);
  bw_u64(&w, call);
  bw_u64(&w, (uint64_t)(int64_t)expected);
  bw_u64(&w, (uint64_t)(int64_t)cub);
  if (!bridge_call(&w, &r)) { if (failed) *failed = true; return false; }
  bool ok = br_u8(&r), f = br_u8(&r);
  uint32_t n;
  const void *p = br_buf_len(&r, &n);
  if (ok && p && out) memcpy(out, p, n < (uint32_t)cub ? n : (uint32_t)cub);
  if (failed) *failed = f;
  bridge_done(&r);
  return ok;
}

EXPORT bool SteamAPI_ManualDispatch_GetAPICallResult(int32_t pipe, uint64_t call, void *out, int32_t cub,
                                                     int32_t expected, bool *failed) {
  return call_result(CORE_MD_CALL_RESULT, NULL, pipe, call, out, cub, expected, failed);
}

EXPORT bool SteamAPI_ISteamUtils_GetAPICallResult(void *self, uint64_t call, void *out, int32_t cub,
                                                  int32_t expected, bool *failed) {
  return call_result(CORE_UTILS_CALL_RESULT, self, 0, call, out, cub, expected, failed);
}
