#pragma once
#include "../wire.h"

#define EXPORT __declspec(dllexport)

/* Отправить кадр хелперу и получить ответ. false — моста нет, вызывающий возвращает ноль. */
bool bridge_call(bw *w, br *r);
void bridge_done(br *r);
/* Строки из ответа живут до конца процесса: игра держит указатели. */
const char *bridge_intern(const char *s);
void bridge_unsupported(const char *name);
