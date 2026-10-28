#pragma once
#include <stddef.h>
#include "../wire.h"

/* Память на время одного запроса. */
void *tmp_alloc(size_t n);
/* Перекладка структур между pack 8 (Windows) и pack 4 (Mac); layout — const layout_t *. */
void win_to_mac(const void *layout, const void *win, void *mac);
void mac_to_win(const void *layout, const void *mac, void *win);
void log_arg(uint32_t id, const char *param, const char *value);

const char *gen_name(uint32_t id);
/* 1 — обработано, 0 — неизвестный id, -1 — плохой запрос. */
int gen_dispatch(uint32_t id, br *r, bw *w);
