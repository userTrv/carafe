/* carafe-steamd: нативный хелпер, через который Windows-игра под Wine говорит со Steam для Mac. */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <signal.h>
#include <stdio.h>
#include <sys/socket.h>
#include <unistd.h>

#include "steamd.h"
#include "layouts.h"

extern int32_t SteamInternal_SteamAPI_Init(const char *versions, char *err);
extern int32_t SteamAPI_InitFlat(char *err);
extern void SteamAPI_Shutdown(void);
extern int32_t SteamAPI_GetHSteamPipe(void);
extern int32_t SteamAPI_GetHSteamUser(void);
extern void SteamAPI_RunCallbacks(void);
extern const char *SteamAPI_GetSteamInstallPath(void);
extern void SteamAPI_ManualDispatch_Init(void);
extern void SteamAPI_ManualDispatch_RunFrame(int32_t pipe);
typedef struct { int32_t user; int32_t id; uint8_t *param; int32_t size; } CallbackMsg_t;
extern bool SteamAPI_ManualDispatch_GetNextCallback(int32_t pipe, CallbackMsg_t *msg);
extern void SteamAPI_ManualDispatch_FreeLastCallback(int32_t pipe);
extern bool SteamAPI_ManualDispatch_GetAPICallResult(int32_t pipe, uint64_t call, void *out, int32_t cub,
                                                     int32_t expected, bool *failed);
extern bool SteamAPI_ISteamUtils_GetAPICallResult(void *self, uint64_t call, void *out, int32_t cub,
                                                  int32_t expected, bool *failed);

static FILE *g_log;
static bool g_inited;

/* --- память запроса --- */
static void *g_tmp[256];
static int g_ntmp;

void *tmp_alloc(size_t n) {
  if (g_ntmp == 256) return NULL;
  return g_tmp[g_ntmp++] = calloc(1, n ? n : 1);
}

static void tmp_reset(void) {
  while (g_ntmp) free(g_tmp[--g_ntmp]);
}

/* --- раскладки --- */
void win_to_mac(const void *layout, const void *win, void *mac) {
  const layout_t *l = (const layout_t *)layout;
  if (!win) return;
  for (uint32_t i = 0; i < l->nleaves; i++)
    if (!l->leaves[i].zero) memcpy((uint8_t *)mac + l->leaves[i].moff, (const uint8_t *)win + l->leaves[i].woff, l->leaves[i].size);
}

void mac_to_win(const void *layout, const void *mac, void *win) {
  const layout_t *l = (const layout_t *)layout;
  memset(win, 0, l->wsize);
  for (uint32_t i = 0; i < l->nleaves; i++)
    if (!l->leaves[i].zero) memcpy((uint8_t *)win + l->leaves[i].woff, (const uint8_t *)mac + l->leaves[i].moff, l->leaves[i].size);
}

static const layout_t *by_callback(int32_t id) {
  for (int i = 0; i < N_LAYOUTS; i++)
    if (LAYOUTS[i].cb == id) return &LAYOUTS[i];
  return NULL;
}

/* Колбэк из Mac-раскладки в Windows-раскладку. */
static void put_callback(bw *w, int32_t id, const uint8_t *p, uint32_t n) {
  const layout_t *l = by_callback(id);
  if (!l || n < l->msize) { bw_buf(w, p, n); return; }
  uint8_t *win = (uint8_t *)tmp_alloc(l->wsize);
  mac_to_win(l, p, win);
  if (id == 703) {
    /* SteamAPICallCompleted_t: размер результата игре нужен в её раскладке. */
    int32_t inner;
    uint32_t cub;
    memcpy(&inner, win + 8, 4);
    memcpy(&cub, win + 12, 4);
    const layout_t *il = by_callback(inner);
    if (il && cub == il->msize) memcpy(win + 12, &il->wsize, 4);
  }
  bw_buf(w, win, l->wsize);
}

/* --- лог --- */
static uint32_t g_seen[4096];

static bool noisy(uint32_t id) {
  return id == CORE_MD_RUN_FRAME || id == CORE_MD_NEXT || id == CORE_MD_FREE || id == CORE_RUN_CALLBACKS;
}

void log_arg(uint32_t id, const char *param, const char *value) {
  if (g_log && g_seen[id % 4096] <= 50) fprintf(g_log, "    %s = \"%s\"\n", param, value ? value : "(null)");
}

static void log_call(uint32_t id) {
  if (!g_log || noisy(id)) return;
  uint32_t k = ++g_seen[id % 4096];
  const char *name = gen_name(id);
  if (k <= 50) fprintf(g_log, "%s\n", name ? name : "core");
  else if (k == 51) fprintf(g_log, "%s: дальше без лога\n", name ? name : "core");
  fflush(g_log);
}

/* --- ядро --- */
static int core(uint32_t id, br *r, bw *w) {
  switch (id) {
  case CORE_INIT: {
    const char *versions = br_str(r);
    char err[1024] = {0};
    int32_t res = g_inited ? 0 : versions ? SteamInternal_SteamAPI_Init(versions, err) : SteamAPI_InitFlat(err);
    if (res == 0) g_inited = true;
    if (g_log) fprintf(g_log, "SteamAPI init: %d %s\n", res, err);
    bw_u64(w, (uint64_t)(int64_t)res);
    bw_str(w, err);
    return 1;
  }
  case CORE_SHUTDOWN:
    if (g_inited) SteamAPI_Shutdown();
    g_inited = false;
    return 1;
  case CORE_GET_PIPE: bw_u64(w, (uint64_t)(int64_t)SteamAPI_GetHSteamPipe()); return 1;
  case CORE_GET_USER: bw_u64(w, (uint64_t)(int64_t)SteamAPI_GetHSteamUser()); return 1;
  case CORE_RUN_CALLBACKS: SteamAPI_RunCallbacks(); return 1;
  case CORE_INSTALL_PATH: bw_str(w, SteamAPI_GetSteamInstallPath()); return 1;
  case CORE_MD_INIT: SteamAPI_ManualDispatch_Init(); return 1;
  case CORE_MD_RUN_FRAME: SteamAPI_ManualDispatch_RunFrame((int32_t)br_u64(r)); return 1;
  case CORE_MD_FREE: SteamAPI_ManualDispatch_FreeLastCallback((int32_t)br_u64(r)); return 1;
  case CORE_MD_NEXT: {
    CallbackMsg_t m;
    bool has = SteamAPI_ManualDispatch_GetNextCallback((int32_t)br_u64(r), &m);
    bw_u8(w, has);
    if (has) {
      bw_u64(w, (uint64_t)(int64_t)m.user);
      bw_u64(w, (uint64_t)(int64_t)m.id);
      put_callback(w, m.id, m.param, (uint32_t)m.size);
    }
    return 1;
  }
  case CORE_MD_CALL_RESULT:
  case CORE_UTILS_CALL_RESULT: {
    uint64_t first = br_u64(r), call = br_u64(r);
    int32_t expected = (int32_t)br_u64(r), cub = (int32_t)br_u64(r);
    const layout_t *l = by_callback(expected);
    uint32_t msize = l ? l->msize : (uint32_t)cub;
    if (msize > WIRE_MAX) return -1;
    uint8_t *mac = (uint8_t *)tmp_alloc(msize);
    bool failed = true;
    bool ok = id == CORE_MD_CALL_RESULT
                  ? SteamAPI_ManualDispatch_GetAPICallResult((int32_t)first, call, mac, (int32_t)msize, expected, &failed)
                  : SteamAPI_ISteamUtils_GetAPICallResult((void *)(uintptr_t)first, call, mac, (int32_t)msize, expected, &failed);
    bw_u8(w, ok);
    bw_u8(w, failed);
    if (l) {
      uint8_t *win = (uint8_t *)tmp_alloc(l->wsize);
      mac_to_win(l, mac, win);
      bw_buf(w, win, l->wsize);
    } else {
      bw_buf(w, mac, msize);
    }
    return 1;
  }
  }
  return 0;
}

static bool read_all(int fd, void *p, size_t n) {
  uint8_t *c = (uint8_t *)p;
  while (n) {
    ssize_t k = read(fd, c, n);
    if (k <= 0) { if (k < 0 && errno == EINTR) continue; return false; }
    c += k; n -= (size_t)k;
  }
  return true;
}

static bool write_all(int fd, const void *p, size_t n) {
  const uint8_t *c = (const uint8_t *)p;
  while (n) {
    ssize_t k = write(fd, c, n);
    if (k <= 0) { if (k < 0 && errno == EINTR) continue; return false; }
    c += k; n -= (size_t)k;
  }
  return true;
}

static void serve(int fd, const char *token) {
  bool authed = false;
  for (;;) {
    uint32_t len;
    if (!read_all(fd, &len, 4) || len < 4 || len > WIRE_MAX) return;
    uint8_t *buf = (uint8_t *)malloc(len);
    if (!read_all(fd, buf, len)) { free(buf); return; }
    uint32_t id;
    memcpy(&id, buf, 4);
    br r = {buf + 4, len - 4, 0, false};
    bw w = {0};
    bw_u32(&w, 0);
    int rc;
    if (id == CORE_HELLO) {
      const char *t = br_str(&r);
      authed = t && !strcmp(t, token);
      bw_u64(&w, authed);
      rc = 1;
      if (authed) alarm(0);
    } else if (!authed) {
      rc = -1;
    } else {
      log_call(id);
      rc = id < 100 ? core(id, &r, &w) : gen_dispatch(id, &r, &w);
    }
    free(buf);
    tmp_reset();
    if (rc != 1 || r.err) {
      if (g_log) fprintf(g_log, "плохой запрос id=%u rc=%d\n", id, rc);
      free(w.p);
      return;
    }
    uint32_t out = w.n - 4;
    memcpy(w.p, &out, 4);
    bool ok = write_all(fd, w.p, w.n);
    free(w.p);
    if (!ok) return;
  }
}

int cloud_main(int argc, char **argv);

int main(int argc, char **argv) {
  if (argc > 1 && !strcmp(argv[1], "cloud")) return cloud_main(argc, argv);
  const char *token = getenv("CARAFE_BRIDGE_TOKEN");
  if (!token || strlen(token) < 32) {
    fprintf(stderr, "carafe-steamd: нужен CARAFE_BRIDGE_TOKEN\n");
    return 2;
  }
  const char *logpath = getenv("CARAFE_BRIDGE_LOG");
  g_log = logpath ? fopen(logpath, "a") : NULL;

  int s = socket(AF_INET, SOCK_STREAM, 0);
  struct sockaddr_in a = {0};
  a.sin_family = AF_INET;
  a.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
  socklen_t al = sizeof a;
  if (s < 0 || bind(s, (struct sockaddr *)&a, sizeof a) || listen(s, 1) || getsockname(s, (struct sockaddr *)&a, &al)) {
    perror("carafe-steamd");
    return 1;
  }
  printf("%u\n", ntohs(a.sin_port));
  fflush(stdout);
  /* Читатель порта уже ушёл: libsteam_api печатает в stdout, запись в закрытый канал убила бы процесс. */
  signal(SIGPIPE, SIG_IGN);
  dup2(STDERR_FILENO, STDOUT_FILENO);
  /* Игра так и не подключилась — не висим вечно. */
  alarm(120);

  /* Один клиент — игра. Подключения с неверным токеном отбрасываются, ждём следующее. */
  for (;;) {
    int c = accept(s, NULL, NULL);
    if (c < 0) { if (errno == EINTR) continue; break; }
    int one = 1;
    setsockopt(c, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    serve(c, token);
    close(c);
    if (g_inited) break;  /* игра была и ушла */
  }
  if (g_inited) SteamAPI_Shutdown();
  if (g_log) fclose(g_log);
  return 0;
}
