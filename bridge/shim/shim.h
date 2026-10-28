#pragma once
#include "../wire.h"

#define EXPORT __declspec(dllexport)

/* Отправить кадр хелперу и получить ответ. false — моста нет, вызывающий возвращает ноль. */
bool bridge_call(bw *w, br *r);
void bridge_done(br *r);
/* Строки из ответа живут до конца процесса: игра держит указатели. */
const char *bridge_intern(const char *s);
void bridge_unsupported(const char *name);

/* Интерфейсы для C++-игр: объект { vtable, указатель на Mac }. Flat-функции принимают оба вида. */
typedef struct { const char *cls, *ver; void *const *vtbl; } iface_desc_t;
extern const iface_desc_t IFACES[];
extern const int N_IFACES;
/* cls — индекс в IFACES или -1; ver — запрошенная версия (NULL — не проверять). */
void *bridge_wrap(void *mac, int cls, const char *ver);
void *bridge_unwrap(void *p);
EXPORT bool SteamAPI_ISteamUtils_GetAPICallResult(void *self, uint64_t call, void *out, int32_t cub,
                                                  int32_t expected, bool *failed);
