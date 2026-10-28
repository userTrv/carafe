/* Протокол моста: общий для Windows-DLL и Mac-хелпера.
 * Кадр запроса: u32 длина | u32 id функции | аргументы. Ответ: u32 длина | результат.
 * Скаляры — u64 LE, строки — u32 длина с NUL (0xFFFFFFFF = NULL) + байты. */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define WIRE_MAX (64u << 20)
#define WIRE_NULL 0xFFFFFFFFu

enum {
  CORE_HELLO = 1,         /* токен */
  CORE_INIT,              /* str версии интерфейсов → u64 результат, str ошибка */
  CORE_SHUTDOWN,
  CORE_GET_PIPE,
  CORE_GET_USER,
  CORE_MD_INIT,
  CORE_MD_RUN_FRAME,      /* u64 pipe */
  CORE_MD_NEXT,           /* u64 pipe → u8 есть, u64 user, u64 id, buf (Windows-раскладка) */
  CORE_MD_FREE,           /* u64 pipe */
  CORE_MD_CALL_RESULT,    /* u64 pipe, u64 call, u64 ожидаемый id, u64 размер у игры → u8 ok, u8 failed, buf */
  CORE_UTILS_CALL_RESULT, /* u64 self + как выше */
  CORE_RUN_CALLBACKS,
  CORE_INSTALL_PATH,      /* → str */
};

typedef struct { uint8_t *p; uint32_t n, cap; } bw;
typedef struct { const uint8_t *p; uint32_t n, off; bool err; } br;

static inline void bw_raw(bw *w, const void *src, uint32_t len) {
  if (w->n + len > w->cap) {
    uint32_t cap = w->cap ? w->cap : 256;
    while (cap < w->n + len) cap *= 2;
    w->p = (uint8_t *)realloc(w->p, cap);
    w->cap = cap;
  }
  if (len) memcpy(w->p + w->n, src, len);
  w->n += len;
}
static inline void bw_u8(bw *w, uint8_t v) { bw_raw(w, &v, 1); }
static inline void bw_u32(bw *w, uint32_t v) { bw_raw(w, &v, 4); }
static inline void bw_u64(bw *w, uint64_t v) { bw_raw(w, &v, 8); }
static inline void bw_buf(bw *w, const void *p, uint32_t len) {
  if (!p) { bw_u32(w, WIRE_NULL); return; }
  bw_u32(w, len);
  bw_raw(w, p, len);
}
static inline void bw_str(bw *w, const char *s) { bw_buf(w, s, s ? (uint32_t)strlen(s) + 1 : 0); }
static inline uint64_t bw_f32(float f) { uint32_t u; memcpy(&u, &f, 4); return u; }
static inline uint64_t bw_f64(double d) { uint64_t u; memcpy(&u, &d, 8); return u; }
/* Начать кадр: место под длину + id. */
static inline void bw_begin(bw *w, uint32_t id) { w->p = NULL; w->n = w->cap = 0; bw_u32(w, 0); bw_u32(w, id); }
static inline void bw_finish(bw *w) { uint32_t len = w->n - 4; memcpy(w->p, &len, 4); }

static inline const void *br_raw(br *r, uint32_t len) {
  if (r->err || len > r->n - r->off) { r->err = true; return NULL; }
  const void *p = r->p + r->off;
  r->off += len;
  return p;
}
static inline uint8_t br_u8(br *r) { const uint8_t *p = (const uint8_t *)br_raw(r, 1); return p ? *p : 0; }
static inline uint32_t br_u32(br *r) { uint32_t v = 0; const void *p = br_raw(r, 4); if (p) memcpy(&v, p, 4); return v; }
static inline uint64_t br_u64(br *r) { uint64_t v = 0; const void *p = br_raw(r, 8); if (p) memcpy(&v, p, 8); return v; }
static inline float br_f32(uint64_t u) { uint32_t v = (uint32_t)u; float f; memcpy(&f, &v, 4); return f; }
static inline double br_f64(uint64_t u) { double d; memcpy(&d, &u, 8); return d; }
/* Буфер или NULL; длину можно узнать через br_len после вызова. */
static inline const void *br_buf_len(br *r, uint32_t *len) {
  uint32_t n = br_u32(r);
  if (len) *len = n == WIRE_NULL ? 0 : n;
  if (n == WIRE_NULL || r->err) return NULL;
  return br_raw(r, n);
}
static inline const void *br_buf(br *r) { return br_buf_len(r, NULL); }
static inline const char *br_str(br *r) {
  uint32_t n;
  const char *s = (const char *)br_buf_len(r, &n);
  return s && n && s[n - 1] == 0 ? s : NULL;
}
/* Скопировать следующий буфер в dst (длина — из кадра). */
static inline void br_copy(br *r, void *dst) {
  uint32_t n;
  const void *src = br_buf_len(r, &n);
  if (src && dst) memcpy(dst, src, n);
}
