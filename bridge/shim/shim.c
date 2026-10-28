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

/* --- объекты интерфейсов --- */

typedef struct { void *const *vtbl; void *mac; int cls; } iface_obj;
static iface_obj g_objs[256];
static int g_nobjs;

void *bridge_unwrap(void *p) {
  iface_obj *o = (iface_obj *)p;
  return o >= g_objs && o < g_objs + 256 ? o->mac : p;
}

void *bridge_wrap(void *mac, int cls, const char *ver) {
  if (!mac) return NULL;
  if (ver) {
    int v = -1;
    for (int i = 0; i < N_IFACES; i++)
      if (!strcmp(IFACES[i].ver, ver)) v = i;
    if (v < 0 || (cls >= 0 && v != cls)) {
      /* Версия не из SDK игры: раскладки её методов мы не знаем. NULL игра переживёт
         («интерфейс недоступен»), а вызов по чужой vtable — нет. */
      bridge_unsupported(bridge_intern(ver));
      return NULL;
    }
    cls = v;
  }
  if (cls < 0 || !IFACES[cls].vtbl) return mac;
  InitOnceExecuteOnce(&g_once, init_lock, NULL, NULL);
  EnterCriticalSection(&g_lock);
  iface_obj *o = NULL;
  for (int i = 0; i < g_nobjs && !o; i++)
    if (g_objs[i].mac == mac && g_objs[i].cls == cls) o = &g_objs[i];
  if (!o && g_nobjs < 256) {
    o = &g_objs[g_nobjs++];
    o->vtbl = IFACES[cls].vtbl;
    o->mac = mac;
    o->cls = cls;
  }
  LeaveCriticalSection(&g_lock);
  return o ? (void *)o : mac;
}

/* --- ядро --- */

/* Счётчик для SteamInternal_ContextInit: меняется на каждом Init, кэш аксессоров игры сбрасывается. */
static uintptr_t g_ctx_counter = 1;

static int32_t do_init(const char *versions, char *err) {
  bw w; br r;
  bw_begin(&w, CORE_INIT);
  bw_str(&w, versions);
  if (!bridge_call(&w, &r)) {
    if (err) strcpy(err, "carafe: нет связи с carafe-steamd");
    return 2; /* k_ESteamAPIInitResult_NoSteamClient */
  }
  int32_t res = (int32_t)br_u64(&r);
  if (res == 0) g_ctx_counter++;
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

static int32_t g_pipe;

EXPORT void SteamAPI_Shutdown(void) { simple(CORE_SHUTDOWN, false, 0); g_ctx_counter++; g_pipe = 0; }

/* Инлайновые SteamUser() и т. п. из SDK: { функция-инициализатор, счётчик, контекст }.
   Инициализатор игры сам зовёт SteamInternal_FindOrCreateUserInterface. */
EXPORT void *SteamInternal_ContextInit(void *data) {
  struct { void (*init)(void *ctx); uintptr_t counter; void *ctx; } *d = data;
  if (d->counter != g_ctx_counter) {
    d->init(&d->ctx);
    d->counter = g_ctx_counter;
  }
  return &d->ctx;
}
EXPORT bool SteamAPI_IsSteamRunning(void) { return !g_dead; }
EXPORT int32_t SteamAPI_GetHSteamPipe(void) { return (int32_t)simple(CORE_GET_PIPE, false, 0); }
EXPORT int32_t SteamAPI_GetHSteamUser(void) { return (int32_t)simple(CORE_GET_USER, false, 0); }
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
  return call_result(CORE_UTILS_CALL_RESULT, bridge_unwrap(self), 0, call, out, cub, expected, failed);
}

/* --- CCallback / CCallResult для C++-игр ---
   Объект игры в раскладке MSVC: vtable, uint8 flags (+8), int iCallback (+12).
   Слоты vtable (перегрузки Run у MSVC в обратном порядке): 0 — Run(param, failed, call),
   1 — Run(param), 2 — GetCallbackSizeBytes(). Сами колбэки берём через ManualDispatch. */
typedef struct { void **vtbl; uint8_t flags; int32_t icb; } ccallback;
typedef void (*run3_fn)(ccallback *, void *, bool, uint64_t);
typedef void (*run1_fn)(ccallback *, void *);
typedef int (*size_fn)(ccallback *);
enum { CB_REGISTERED = 1, CB_GAMESERVER = 2 };

static CRITICAL_SECTION g_cblock;
static INIT_ONCE g_cbonce = INIT_ONCE_STATIC_INIT;
static BOOL CALLBACK init_cblock(PINIT_ONCE o, PVOID p, PVOID *c) {
  (void)o; (void)p; (void)c;
  InitializeCriticalSection(&g_cblock);
  return TRUE;
}
static ccallback **g_cbs;            /* подписки на колбэки */
static int g_ncbs, g_capcbs;
typedef struct { ccallback *cb; uint64_t call; } pending_t;
static pending_t *g_calls;           /* ожидающие call result */
static int g_ncalls, g_capcalls;
static bool g_dispatch_wanted, g_dispatch_on;

static void cb_lock(void) { InitOnceExecuteOnce(&g_cbonce, init_cblock, NULL, NULL); EnterCriticalSection(&g_cblock); }
static void cb_unlock(void) { LeaveCriticalSection(&g_cblock); }

EXPORT void SteamAPI_RegisterCallback(ccallback *cb, int32_t icb) {
  cb_lock();
  cb->icb = icb;
  cb->flags |= CB_REGISTERED;
  if (g_ncbs == g_capcbs) {
    g_capcbs = g_capcbs ? g_capcbs * 2 : 64;
    g_cbs = (ccallback **)realloc(g_cbs, sizeof *g_cbs * (size_t)g_capcbs);
  }
  g_cbs[g_ncbs++] = cb;
  g_dispatch_wanted = true;
  cb_unlock();
}

EXPORT void SteamAPI_UnregisterCallback(ccallback *cb) {
  cb_lock();
  cb->flags &= (uint8_t)~CB_REGISTERED;
  for (int i = 0; i < g_ncbs; i++)
    if (g_cbs[i] == cb) { g_cbs[i] = g_cbs[--g_ncbs]; break; }
  cb_unlock();
}

EXPORT void SteamAPI_RegisterCallResult(ccallback *cb, uint64_t call) {
  if (!call) return;
  cb_lock();
  cb->flags |= CB_REGISTERED;
  if (g_ncalls == g_capcalls) {
    g_capcalls = g_capcalls ? g_capcalls * 2 : 64;
    g_calls = (pending_t *)realloc(g_calls, sizeof *g_calls * (size_t)g_capcalls);
  }
  g_calls[g_ncalls++] = (pending_t){cb, call};
  g_dispatch_wanted = true;
  cb_unlock();
}

EXPORT void SteamAPI_UnregisterCallResult(ccallback *cb, uint64_t call) {
  cb_lock();
  for (int i = 0; i < g_ncalls; i++)
    if (g_calls[i].cb == cb && g_calls[i].call == call) { g_calls[i] = g_calls[--g_ncalls]; break; }
  cb->flags &= (uint8_t)~CB_REGISTERED;
  cb_unlock();
}

static bool take_call(uint64_t call, ccallback **out) {
  bool found = false;
  cb_lock();
  for (int i = 0; i < g_ncalls; i++)
    if (g_calls[i].call == call) {
      *out = g_calls[i].cb;
      g_calls[i] = g_calls[--g_ncalls];
      (*out)->flags &= (uint8_t)~CB_REGISTERED;
      found = true;
      break;
    }
  cb_unlock();
  return found;
}

static void dispatch_callback(int32_t id, void *param) {
  /* Снимок: обработчик может подписаться или отписаться прямо во время вызова. */
  cb_lock();
  int n = 0;
  ccallback **snap = (ccallback **)malloc(sizeof *snap * (size_t)(g_ncbs ? g_ncbs : 1));
  for (int i = 0; i < g_ncbs; i++)
    if (g_cbs[i]->icb == id && !(g_cbs[i]->flags & CB_GAMESERVER)) snap[n++] = g_cbs[i];
  cb_unlock();
  for (int i = 0; i < n; i++) {
    bool still = false;
    cb_lock();
    for (int k = 0; k < g_ncbs && !still; k++) still = g_cbs[k] == snap[i];
    cb_unlock();
    if (still) ((run1_fn)snap[i]->vtbl[1])(snap[i], param);
  }
  free(snap);
}

EXPORT void SteamAPI_RunCallbacks(void) {
  static bool running;
  if (!g_dispatch_wanted) { simple(CORE_RUN_CALLBACKS, false, 0); return; }
  if (running) return;
  running = true;
  if (!g_dispatch_on) { SteamAPI_ManualDispatch_Init(); g_dispatch_on = true; }
  if (!g_pipe) g_pipe = SteamAPI_GetHSteamPipe();
  SteamAPI_ManualDispatch_RunFrame(g_pipe);
  CallbackMsg_t m;
  while (SteamAPI_ManualDispatch_GetNextCallback(g_pipe, &m)) {
    if (m.id == 703 && m.size >= 16) {
      /* SteamAPICallCompleted_t: uint64 call, int iCallback, uint32 cubParam. */
      uint64_t call;
      memcpy(&call, m.param, 8);
      ccallback *cb;
      if (take_call(call, &cb)) {
        int size = ((size_fn)cb->vtbl[2])(cb);
        void *buf = calloc(1, size > 0 ? (size_t)size : 1);
        bool failed = true;
        bool ok = SteamAPI_ManualDispatch_GetAPICallResult(g_pipe, call, buf, size, cb->icb, &failed);
        ((run3_fn)cb->vtbl[0])(cb, buf, failed || !ok, call);
        free(buf);
      }
    }
    dispatch_callback(m.id, m.param);
    SteamAPI_ManualDispatch_FreeLastCallback(g_pipe);
  }
  running = false;
}
