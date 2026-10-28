/* carafe-steamd cloud: синхронизация Steam Auto-Cloud для Windows-игры через сервис Cloud
 * клиента Steam для Mac (ISteamUnifiedMessages), без браузера и без пароля.
 *
 * carafe-steamd cloud status|pull|push --state FILE --backup DIR
 *     --root NAME=DIR ... --rule ROOT:PATH:PATTERN:RECURSIVE ...
 * Нужен SteamAppId в окружении. */
#define _DARWIN_C_SOURCE
#include <CommonCrypto/CommonDigest.h>
#include <curl/curl.h>
#include <dirent.h>
#include <errno.h>
#include <fnmatch.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#include "steamd.h"

extern void SteamAPI_Shutdown(void);
extern int32_t SteamAPI_GetHSteamPipe(void);
extern int32_t SteamAPI_GetHSteamUser(void);
extern void SteamAPI_RunCallbacks(void);
extern void *SteamInternal_CreateInterface(const char *ver);
extern void *SteamAPI_ISteamClient_GetISteamGenericInterface(void *self, int32_t user, int32_t pipe, const char *ver);

#define MAXF 512
#define SHA_LEN 20

typedef struct { char *name; uint8_t sha[SHA_LEN]; uint64_t ts; uint32_t size; } cfile;   /* в облаке */
typedef struct { char *name; char *path; uint8_t sha[SHA_LEN]; time_t mtime; } lfile;      /* локально */
typedef struct { char *name; char sha[SHA_LEN * 2 + 1]; } sentry;                            /* последняя синхронизация */
typedef struct { char *root; char *path; char *pattern; bool recursive; } rule;

static uint32_t g_app;
static void *g_um;
static char *g_roots[16][2];
static int g_nroots;
static rule g_rules[16];
static int g_nrules;
static cfile g_cloud[MAXF];
static int g_ncloud;
static lfile g_local[MAXF];
static int g_nlocal;
static sentry g_state[MAXF];
static int g_nstate;
static const char *g_backup;
static char g_stamp[32];

static void die(const char *msg) { fprintf(stderr, "carafe cloud: %s\n", msg); exit(1); }

/* --- protobuf --- */
static void pb_varint(bw *w, uint64_t v) { do { uint8_t b = v & 0x7f; v >>= 7; bw_u8(w, b | (v ? 0x80 : 0)); } while (v); }
static void pb_u(bw *w, int f, uint64_t v) { pb_varint(w, (uint64_t)f << 3); pb_varint(w, v); }
static void pb_b(bw *w, int f, const void *p, size_t n) { pb_varint(w, (uint64_t)f << 3 | 2); pb_varint(w, n); bw_raw(w, p, (uint32_t)n); }
static void pb_s(bw *w, int f, const char *s) { pb_b(w, f, s, strlen(s)); }

typedef struct { int field, type; uint64_t v; const uint8_t *data; size_t len; } pbf;

static uint64_t rd_varint(const uint8_t **p, const uint8_t *end) {
  uint64_t v = 0; int s = 0;
  while (*p < end && s < 64) { uint8_t b = *(*p)++; v |= (uint64_t)(b & 0x7f) << s; s += 7; if (!(b & 0x80)) break; }
  return v;
}

static bool pb_next(const uint8_t **p, const uint8_t *end, pbf *f) {
  if (*p >= end) return false;
  uint64_t key = rd_varint(p, end);
  f->field = (int)(key >> 3); f->type = key & 7; f->data = NULL; f->len = 0; f->v = 0;
  switch (f->type) {
  case 0: f->v = rd_varint(p, end); return true;
  case 1: if (end - *p < 8) return false; memcpy(&f->v, *p, 8); *p += 8; return true;
  case 5: if (end - *p < 4) return false; memcpy(&f->v, *p, 4); *p += 4; return true;
  case 2: f->len = rd_varint(p, end); if ((size_t)(end - *p) < f->len) return false; f->data = *p; *p += f->len; return true;
  }
  return false;
}

static char *pb_strdup(const pbf *f) { char *s = malloc(f->len + 1); memcpy(s, f->data, f->len); s[f->len] = 0; return s; }

/* --- вызов сервиса Steam --- */
typedef uint64_t (*send_fn)(void *, const char *, const void *, uint32_t, uint64_t);
typedef bool (*info_fn)(void *, uint64_t, uint32_t *, int32_t *);
typedef bool (*data_fn)(void *, uint64_t, void *, uint32_t, bool);

/* Возвращает EResult; ответ в *out (malloc). */
static int32_t call(const char *method, bw *req, uint8_t **out, uint32_t *outlen) {
  void **vt = *(void ***)g_um;
  uint64_t h = ((send_fn)vt[0])(g_um, method, req->p, req->n, 0);
  free(req->p);
  req->p = NULL; req->n = req->cap = 0;
  uint32_t size = 0; int32_t res = 0;
  for (int i = 0; i < 300; i++) {
    if (((info_fn)vt[1])(g_um, h, &size, &res)) break;
    SteamAPI_RunCallbacks();
    usleep(50000);
  }
  *out = malloc(size ? size : 1);
  *outlen = size;
  if (res == 1 && size && !((data_fn)vt[2])(g_um, h, *out, size, true)) res = 0;
  return res;
}

/* --- HTTP --- */
typedef struct { uint8_t *p; size_t n; } mem;

static size_t on_data(char *p, size_t s, size_t n, void *u) {
  mem *m = u;
  m->p = realloc(m->p, m->n + s * n);
  memcpy(m->p + m->n, p, s * n);
  m->n += s * n;
  return s * n;
}

/* GET или PUT по описанию из ответа Cloud. headers — пары name/value. */
static long http(const char *method, const char *host, const char *path, bool https, char **headers, int nh,
                 const uint8_t *body, size_t blen, mem *resp) {
  char url[4096];
  snprintf(url, sizeof url, "%s://%s%s", https ? "https" : "http", host, path);
  CURL *c = curl_easy_init();
  struct curl_slist *hl = NULL;
  for (int i = 0; i + 1 < nh; i += 2) {
    char line[2048];
    snprintf(line, sizeof line, "%s: %s", headers[i], headers[i + 1]);
    hl = curl_slist_append(hl, line);
  }
  curl_easy_setopt(c, CURLOPT_URL, url);
  curl_easy_setopt(c, CURLOPT_HTTPHEADER, hl);
  curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, on_data);
  curl_easy_setopt(c, CURLOPT_WRITEDATA, resp);
  curl_easy_setopt(c, CURLOPT_TIMEOUT, 120L);
  if (!strcmp(method, "PUT") || !strcmp(method, "POST")) {
    curl_easy_setopt(c, CURLOPT_CUSTOMREQUEST, method);
    curl_easy_setopt(c, CURLOPT_POSTFIELDS, body ? (const char *)body : "");
    curl_easy_setopt(c, CURLOPT_POSTFIELDSIZE_LARGE, (curl_off_t)blen);
  }
  long code = 0;
  if (curl_easy_perform(c) == CURLE_OK) curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &code);
  curl_slist_free_all(hl);
  curl_easy_cleanup(c);
  return code;
}

/* --- файлы --- */
static void sha1(const uint8_t *p, size_t n, uint8_t out[SHA_LEN]) {
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
  CC_SHA1(p, (CC_LONG)n, out);
#pragma clang diagnostic pop
}

static void hex(const uint8_t sha[SHA_LEN], char out[SHA_LEN * 2 + 1]) {
  for (int i = 0; i < SHA_LEN; i++) sprintf(out + 2 * i, "%02x", sha[i]);
}

static uint8_t *slurp(const char *path, size_t *n) {
  FILE *f = fopen(path, "rb");
  if (!f) return NULL;
  fseek(f, 0, SEEK_END);
  long len = ftell(f);
  fseek(f, 0, SEEK_SET);
  uint8_t *p = malloc(len > 0 ? (size_t)len : 1);
  *n = fread(p, 1, (size_t)len, f);
  fclose(f);
  return p;
}

static void mkdirs(const char *path) {
  char tmp[4096];
  snprintf(tmp, sizeof tmp, "%s", path);
  for (char *s = tmp + 1; *s; s++)
    if (*s == '/') { *s = 0; mkdir(tmp, 0755); *s = '/'; }
}

/* Атомарная запись с mtime = время из облака. */
static bool write_file(const char *path, const uint8_t *p, size_t n, time_t mtime) {
  char tmp[4200];
  snprintf(tmp, sizeof tmp, "%s.carafe-tmp", path);
  mkdirs(path);
  FILE *f = fopen(tmp, "wb");
  if (!f) return false;
  bool ok = fwrite(p, 1, n, f) == n;
  ok = fclose(f) == 0 && ok;
  if (ok && mtime) {
    struct timeval tv[2] = {{mtime, 0}, {mtime, 0}};
    utimes(tmp, tv);
  }
  return ok && rename(tmp, path) == 0;
}

static void backup(const char *name, const uint8_t *p, size_t n, const char *side) {
  char safe[1024], path[4096];
  size_t k = 0;
  for (const char *c = name; *c && k + 1 < sizeof safe; c++) safe[k++] = (*c == '/' || *c == '%') ? '_' : *c;
  safe[k] = 0;
  snprintf(path, sizeof path, "%s/%s/%s.%s", g_backup, g_stamp, safe, side);
  if (!write_file(path, p, n, 0)) fprintf(stderr, "carafe cloud: не сохранил копию %s\n", path);
}

/* Имя в облаке → путь на диске: "%WinAppDataLocalLow%Ved/..." → "<root>/Ved/...". */
static char *local_path(const char *name) {
  if (name[0] != '%') return NULL;
  const char *e = strchr(name + 1, '%');
  if (!e) return NULL;
  for (int i = 0; i < g_nroots; i++)
    if (strlen(g_roots[i][0]) == (size_t)(e - name - 1) && !strncmp(g_roots[i][0], name + 1, (size_t)(e - name - 1))) {
      char *p = malloc(strlen(g_roots[i][1]) + strlen(e) + 2);
      sprintf(p, "%s/%s", g_roots[i][1], e + 1);
      return p;
    }
  return NULL;
}

static const char *root_dir(const char *root) {
  for (int i = 0; i < g_nroots; i++) if (!strcmp(g_roots[i][0], root)) return g_roots[i][1];
  return NULL;
}

static void scan(const rule *r, const char *dir, const char *rel) {
  DIR *d = opendir(dir);
  if (!d) return;
  struct dirent *e;
  while ((e = readdir(d)) && g_nlocal < MAXF) {
    if (e->d_name[0] == '.') continue;
    char path[4096], relp[2048];
    snprintf(path, sizeof path, "%s/%s", dir, e->d_name);
    snprintf(relp, sizeof relp, "%s%s%s", rel, *rel ? "/" : "", e->d_name);
    struct stat st;
    if (stat(path, &st)) continue;
    if (S_ISDIR(st.st_mode)) {
      if (r->recursive) scan(r, path, relp);
    } else if (S_ISREG(st.st_mode) && !fnmatch(r->pattern, e->d_name, 0)) {
      lfile *l = &g_local[g_nlocal++];
      l->name = malloc(strlen(r->root) + strlen(r->path) + strlen(relp) + 8);
      sprintf(l->name, "%%%s%%%s/%s", r->root, r->path, relp);
      l->path = strdup(path);
      l->mtime = st.st_mtime;
      size_t n;
      uint8_t *p = slurp(path, &n);
      sha1(p ? p : (const uint8_t *)"", p ? n : 0, l->sha);
      free(p);
    }
  }
  closedir(d);
}

/* --- состояние последней синхронизации --- */
static const char *g_state_path;

static void state_load(void) {
  FILE *f = fopen(g_state_path, "r");
  if (!f) return;
  char line[4200];
  while (fgets(line, sizeof line, f) && g_nstate < MAXF) {
    line[strcspn(line, "\n")] = 0;
    char *tab = strchr(line, '\t');
    if (!tab || tab - line != SHA_LEN * 2) continue;
    memcpy(g_state[g_nstate].sha, line, SHA_LEN * 2);
    g_state[g_nstate].sha[SHA_LEN * 2] = 0;
    g_state[g_nstate++].name = strdup(tab + 1);
  }
  fclose(f);
}

static const char *state_get(const char *name) {
  for (int i = 0; i < g_nstate; i++) if (!strcmp(g_state[i].name, name)) return g_state[i].sha;
  return NULL;
}

static void state_set(const char *name, const uint8_t sha[SHA_LEN]) {
  int i = 0;
  while (i < g_nstate && strcmp(g_state[i].name, name)) i++;
  if (i == g_nstate) { if (g_nstate == MAXF) return; g_state[g_nstate++].name = strdup(name); }
  hex(sha, g_state[i].sha);
}

static void state_save(void) {
  char tmp[4200];
  snprintf(tmp, sizeof tmp, "%s.tmp", g_state_path);
  mkdirs(g_state_path);
  FILE *f = fopen(tmp, "w");
  if (!f) return;
  for (int i = 0; i < g_nstate; i++) fprintf(f, "%s\t%s\n", g_state[i].sha, g_state[i].name);
  fclose(f);
  rename(tmp, g_state_path);
}

/* --- облако --- */
static void load_cloud(void) {
  bw req = {0};
  pb_u(&req, 1, g_app);
  uint8_t *resp; uint32_t n;
  int32_t r = call("Cloud.GetAppFileChangelist#1", &req, &resp, &n);
  if (r != 1) { fprintf(stderr, "carafe cloud: GetAppFileChangelist EResult %d\n", r); exit(1); }
  char *prefixes[MAXF]; int np = 0;
  struct { char *file; uint32_t prefix; uint8_t sha[SHA_LEN]; uint64_t ts; uint32_t size; uint32_t persist; } raw[MAXF];
  int nr = 0;
  const uint8_t *p = resp, *end = resp + n;
  pbf f;
  while (pb_next(&p, end, &f)) {
    if (f.field == 4 && f.type == 2 && np < MAXF) prefixes[np++] = pb_strdup(&f);
    if (f.field == 2 && f.type == 2 && nr < MAXF) {
      memset(&raw[nr], 0, sizeof raw[nr]);
      const uint8_t *q = f.data, *qe = f.data + f.len;
      pbf g;
      while (pb_next(&q, qe, &g)) {
        if (g.field == 1 && g.type == 2) raw[nr].file = pb_strdup(&g);
        else if (g.field == 2 && g.type == 2 && g.len == SHA_LEN) memcpy(raw[nr].sha, g.data, SHA_LEN);
        else if (g.field == 3) raw[nr].ts = g.v;
        else if (g.field == 4) raw[nr].size = (uint32_t)g.v;
        else if (g.field == 5) raw[nr].persist = (uint32_t)g.v;
        else if (g.field == 7) raw[nr].prefix = (uint32_t)g.v;
      }
      nr++;
    }
  }
  free(resp);
  for (int i = 0; i < nr; i++) {
    if (!raw[i].file || raw[i].persist != 0) continue;  /* только живые файлы */
    const char *pre = raw[i].prefix < (uint32_t)np ? prefixes[raw[i].prefix] : "";
    cfile *c = &g_cloud[g_ncloud++];
    c->name = malloc(strlen(pre) + strlen(raw[i].file) + 1);
    sprintf(c->name, "%s%s", pre, raw[i].file);
    memcpy(c->sha, raw[i].sha, SHA_LEN);
    c->ts = raw[i].ts;
    c->size = raw[i].size;
  }
}

static bool download(const cfile *c, uint8_t **data, size_t *n) {
  bw req = {0};
  pb_u(&req, 1, g_app);
  pb_s(&req, 2, c->name);
  uint8_t *resp; uint32_t rn;
  if (call("Cloud.ClientFileDownload#1", &req, &resp, &rn) != 1) { free(resp); return false; }
  char *host = NULL, *path = NULL, *hdr[64]; int nh = 0;
  bool https = true, encrypted = false;
  const uint8_t *p = resp, *end = resp + rn;
  pbf f;
  while (pb_next(&p, end, &f)) {
    if (f.field == 7 && f.type == 2) host = pb_strdup(&f);
    else if (f.field == 8 && f.type == 2) path = pb_strdup(&f);
    else if (f.field == 9) https = f.v;
    else if (f.field == 11) encrypted = f.v;
    else if (f.field == 10 && f.type == 2 && nh + 2 <= 64) {
      const uint8_t *q = f.data, *qe = f.data + f.len;
      pbf g;
      char *name = NULL, *value = NULL;
      while (pb_next(&q, qe, &g)) {
        if (g.field == 1) name = pb_strdup(&g);
        if (g.field == 2) value = pb_strdup(&g);
      }
      if (name && value) { hdr[nh++] = name; hdr[nh++] = value; }
    }
  }
  free(resp);
  if (!host || !path || encrypted) { fprintf(stderr, "carafe cloud: %s — %s\n", c->name, encrypted ? "зашифрован" : "нет ссылки"); return false; }
  mem m = {0};
  long code = http("GET", host, path, https, hdr, nh, NULL, 0, &m);
  uint8_t sha[SHA_LEN];
  sha1(m.p ? m.p : (const uint8_t *)"", m.n, sha);
  if (code != 200 || memcmp(sha, c->sha, SHA_LEN)) {
    fprintf(stderr, "carafe cloud: %s — HTTP %ld%s\n", c->name, code, code == 200 ? ", sha1 не совпал" : "");
    free(m.p);
    return false;
  }
  *data = m.p;
  *n = m.n;
  return true;
}

static uint64_t g_batch;

static bool upload(const lfile *l) {
  size_t n;
  uint8_t *data = slurp(l->path, &n);
  if (!data) return false;
  bw req = {0};
  pb_u(&req, 1, g_app);
  pb_u(&req, 2, n);
  pb_u(&req, 3, n);
  pb_b(&req, 4, l->sha, SHA_LEN);
  pb_u(&req, 5, (uint64_t)l->mtime);
  pb_s(&req, 6, l->name);
  pb_u(&req, 10, 0);  /* can_encrypt = false: шифровать мы не умеем */
  if (g_batch) pb_u(&req, 13, g_batch);
  uint8_t *resp; uint32_t rn;
  int32_t r = call("Cloud.ClientBeginFileUpload#1", &req, &resp, &rn);
  if (r != 1) { fprintf(stderr, "carafe cloud: ClientBeginFileUpload %s EResult %d\n", l->name, r); free(data); free(resp); return false; }
  bool ok = true;
  const uint8_t *p = resp, *end = resp + rn;
  pbf f;
  while (ok && pb_next(&p, end, &f)) {
    if (f.field == 1 && f.v) { fprintf(stderr, "carafe cloud: Steam требует шифрование — не поддержано\n"); ok = false; }
    if (f.field != 2 || f.type != 2) continue;
    char *host = NULL, *path = NULL, *hdr[64]; int nh = 0;
    bool https = true;
    int method = 4;
    uint64_t off = 0, len = n;
    const uint8_t *explicit_body = NULL; size_t explicit_len = 0;
    const uint8_t *q = f.data, *qe = f.data + f.len;
    pbf g;
    while (pb_next(&q, qe, &g)) {
      if (g.field == 1) host = pb_strdup(&g);
      else if (g.field == 2) path = pb_strdup(&g);
      else if (g.field == 3) https = g.v;
      else if (g.field == 4) method = (int)g.v;
      else if (g.field == 6) off = g.v;
      else if (g.field == 7) len = g.v;
      else if (g.field == 8 && g.type == 2) { explicit_body = g.data; explicit_len = g.len; }
      else if (g.field == 5 && g.type == 2 && nh + 2 <= 64) {
        const uint8_t *h = g.data, *he = g.data + g.len;
        pbf hh;
        char *name = NULL, *value = NULL;
        while (pb_next(&h, he, &hh)) {
          if (hh.field == 1) name = pb_strdup(&hh);
          if (hh.field == 2) value = pb_strdup(&hh);
        }
        if (name && value) { hdr[nh++] = name; hdr[nh++] = value; }
      }
    }
    if (!host || !path || off > n || len > n - off) { ok = false; break; }
    /* EHTTPMethod: 3 — POST, 4 — PUT. */
    mem m = {0};
    long code = http(method == 3 ? "POST" : "PUT", host, path, https, hdr, nh,
                     explicit_body ? explicit_body : data + off, explicit_body ? explicit_len : len, &m);
    free(m.p);
    if (code < 200 || code >= 300) { fprintf(stderr, "carafe cloud: загрузка %s — HTTP %ld\n", l->name, code); ok = false; }
  }
  free(resp);
  free(data);

  bw commit = {0};
  pb_u(&commit, 1, ok);
  pb_u(&commit, 2, g_app);
  pb_b(&commit, 3, l->sha, SHA_LEN);
  pb_s(&commit, 4, l->name);
  r = call("Cloud.ClientCommitFileUpload#1", &commit, &resp, &rn);
  bool committed = false;
  p = resp; end = resp + rn;
  while (r == 1 && pb_next(&p, end, &f)) if (f.field == 1) committed = f.v;
  free(resp);
  return ok && committed;
}

/* --- решение по файлу --- */
enum { SYNCED, PULL, PUSH, NONE };

static int decide(const lfile *l, const cfile *c, bool *conflict) {
  *conflict = false;
  if (!l && !c) return NONE;
  if (!c) return PUSH;
  if (!l) return PULL;
  if (!memcmp(l->sha, c->sha, SHA_LEN)) return SYNCED;
  char lh[SHA_LEN * 2 + 1], ch[SHA_LEN * 2 + 1];
  hex(l->sha, lh);
  hex(c->sha, ch);
  const char *s = state_get(l->name);
  if (s && !strcmp(s, lh)) return PULL;  /* локально не менялся — облако новее */
  if (s && !strcmp(s, ch)) return PUSH;  /* облако не менялось — локальный новее */
  *conflict = true;                      /* поменялись оба (или первая синхронизация): новее побеждает */
  return (uint64_t)l->mtime > c->ts ? PUSH : PULL;
}

static const lfile *find_local(const char *name) {
  for (int i = 0; i < g_nlocal; i++) if (!strcmp(g_local[i].name, name)) return &g_local[i];
  return NULL;
}

static const cfile *find_cloud(const char *name) {
  for (int i = 0; i < g_ncloud; i++) if (!strcmp(g_cloud[i].name, name)) return &g_cloud[i];
  return NULL;
}

/* Имя без "%Root%путь/" из правила: видно подкаталог, а одинаковые файлы в разных папках различимы. */
static const char *short_name(const char *name) {
  const char *e = name[0] == '%' ? strchr(name + 1, '%') : NULL;
  if (!e) return name;
  for (int i = 0; i < g_nrules; i++) {
    size_t n = strlen(g_rules[i].path);
    if (!strncmp(e + 1, g_rules[i].path, n) && e[1 + n] == '/') return e + 2 + n;
  }
  return e + 1;
}

int cloud_main(int argc, char **argv) {
  if (argc < 3) die("cloud status|pull|push --state FILE --backup DIR --root N=DIR --rule R:P:PAT:REC");
  const char *mode = argv[2];
  for (int i = 3; i + 1 < argc; i += 2) {
    if (!strcmp(argv[i], "--state")) g_state_path = argv[i + 1];
    else if (!strcmp(argv[i], "--backup")) g_backup = argv[i + 1];
    else if (!strcmp(argv[i], "--root") && g_nroots < 16) {
      char *eq = strchr(argv[i + 1], '=');
      if (!eq) die("--root NAME=DIR");
      *eq = 0;
      g_roots[g_nroots][0] = argv[i + 1];
      g_roots[g_nroots++][1] = eq + 1;
    } else if (!strcmp(argv[i], "--rule") && g_nrules < 16) {
      char *s = strdup(argv[i + 1]);
      rule *r = &g_rules[g_nrules++];
      r->root = strsep(&s, ":");
      r->path = strsep(&s, ":");
      r->pattern = strsep(&s, ":");
      r->recursive = s && !strcmp(s, "1");
      if (!r->root || !r->path || !r->pattern) die("--rule ROOT:PATH:PATTERN:RECURSIVE");
    }
  }
  const char *app = getenv("SteamAppId");
  if (!app || !g_state_path || !g_backup) die("нужны SteamAppId, --state, --backup");
  g_app = (uint32_t)strtoul(app, NULL, 10);
  time_t now = time(NULL);
  strftime(g_stamp, sizeof g_stamp, "%Y%m%d-%H%M%S", localtime(&now));

  char err[1024] = {0};
  if (steam_init(NULL, err) != 0) { fprintf(stderr, "carafe cloud: Steam недоступен: %s\n", err); return 1; }
  void *client = SteamInternal_CreateInterface("SteamClient021");
  g_um = client ? SteamAPI_ISteamClient_GetISteamGenericInterface(client, SteamAPI_GetHSteamUser(), SteamAPI_GetHSteamPipe(),
                                                                  "STEAMUNIFIEDMESSAGES_INTERFACE_VERSION001") : NULL;
  if (!g_um) { SteamAPI_Shutdown(); die("нет ISteamUnifiedMessages в клиенте Steam"); }
  curl_global_init(CURL_GLOBAL_DEFAULT);

  state_load();
  load_cloud();
  for (int i = 0; i < g_nrules; i++) {
    const char *rd = root_dir(g_rules[i].root);
    if (!rd) { fprintf(stderr, "carafe cloud: нет каталога для корня %s — пропускаю\n", g_rules[i].root); continue; }
    char dir[4096];
    snprintf(dir, sizeof dir, "%s/%s", rd, g_rules[i].path);
    scan(&g_rules[i], dir, "");
  }

  /* Все имена: из облака и с диска. */
  const char *names[MAXF * 2]; int nn = 0;
  for (int i = 0; i < g_ncloud; i++) names[nn++] = g_cloud[i].name;
  for (int i = 0; i < g_nlocal; i++) if (!find_cloud(g_local[i].name)) names[nn++] = g_local[i].name;

  int failed = 0, uploads = 0;
  for (int i = 0; i < nn; i++) {
    const lfile *l = find_local(names[i]);
    const cfile *c = find_cloud(names[i]);
    bool conflict;
    int act = decide(l, c, &conflict);
    if (act == PUSH) uploads++;
  }

  bool pushing = !strcmp(mode, "push");
  if (pushing && uploads) {
    /* Пачка — как у клиента Steam: так облако видит одну запись о синхронизации. */
    bw req = {0};
    pb_u(&req, 1, g_app);
    pb_s(&req, 2, "carafe");
    for (int i = 0; i < nn; i++) {
      bool conflict;
      if (decide(find_local(names[i]), find_cloud(names[i]), &conflict) == PUSH) pb_s(&req, 3, names[i]);
    }
    uint8_t *resp; uint32_t rn;
    if (call("Cloud.BeginAppUploadBatch#1", &req, &resp, &rn) == 1) {
      const uint8_t *p = resp, *end = resp + rn;
      pbf f;
      while (pb_next(&p, end, &f)) if (f.field == 1) g_batch = f.v;
    }
    free(resp);
  }

  for (int i = 0; i < nn; i++) {
    const lfile *l = find_local(names[i]);
    const cfile *c = find_cloud(names[i]);
    bool conflict;
    int act = decide(l, c, &conflict);
    const char *sn = short_name(names[i]);
    if (act == SYNCED) {
      state_set(names[i], l->sha);
      if (!strcmp(mode, "status")) printf("  = %s\n", sn);
      continue;
    }
    if (!strcmp(mode, "status")) {
      printf("  %s %s%s\n", act == PULL ? "↓" : "↑", sn, conflict ? " (конфликт: новее — эта сторона)" : "");
      continue;
    }
    if (act == PULL && !strcmp(mode, "pull")) {
      uint8_t *data; size_t n;
      if (!download(c, &data, &n)) { failed++; continue; }
      char *path = l ? strdup(l->path) : local_path(names[i]);
      if (!path) { fprintf(stderr, "carafe cloud: нет каталога для %s\n", names[i]); free(data); failed++; continue; }
      if (l) {
        size_t on;
        uint8_t *old = slurp(l->path, &on);
        if (old) backup(names[i], old, on, "local");
        free(old);
      }
      if (write_file(path, data, n, (time_t)c->ts)) {
        state_set(names[i], c->sha);
        printf("  ↓ %s%s\n", sn, conflict ? " (конфликт: облако новее, локальный — в backups)" : "");
      } else {
        fprintf(stderr, "carafe cloud: не записал %s: %s\n", path, strerror(errno));
        failed++;
      }
      free(data);
      free(path);
    } else if (act == PUSH && pushing) {
      if (c) {
        uint8_t *old; size_t on;
        if (download(c, &old, &on)) { backup(names[i], old, on, "cloud"); free(old); }
      }
      if (upload(l)) {
        state_set(names[i], l->sha);
        printf("  ↑ %s%s\n", sn, conflict ? " (конфликт: локальный новее, облачный — в backups)" : "");
      } else {
        failed++;
      }
    }
  }

  if (g_batch) {
    bw req = {0};
    pb_u(&req, 1, g_app);
    pb_u(&req, 2, g_batch);
    pb_u(&req, 3, failed ? 2 : 1);
    uint8_t *resp; uint32_t rn;
    call("Cloud.CompleteAppUploadBatchBlocking#1", &req, &resp, &rn);
    free(resp);
  }
  state_save();
  curl_global_cleanup();
  SteamAPI_Shutdown();
  return failed ? 1 : 0;
}
