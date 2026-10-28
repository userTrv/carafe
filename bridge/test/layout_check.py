#!/usr/bin/env python3
"""Сверка раскладок gen.py с настоящими компиляторами.

layout_check.py <steam_api.json> <out.c>
Пишет C-файл: структуры из steam_api.json и _Static_assert на sizeof/offsetof, посчитанные gen.py
для pack 8 и pack 4. Компилируется с -DPACK=8 под x86_64-windows-gnu и с -DPACK=4 под Mac;
расхождение арифметики gen.py с компилятором — ошибка сборки.
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import gen  # noqa: E402


def ctype(model, t):
    """Тип поля → (C-тип, суффикс массива)."""
    t = t.strip()
    if t.endswith("]"):
        base, dims = t[:t.index("[")].strip(), t[t.index("["):]
        return ctype(model, base)[0], dims
    if t in ("CSteamID", "CGameID"):
        return "uint64_t", ""
    if t.endswith("*"):
        return "void *", ""
    p = model.prim(t)
    if p:
        return gen.CTYPE[p], ""
    if t in model.structs:
        return f"struct {t}", ""
    raise gen.Unsupported(t)


def main(api_path, out_path):
    api = json.loads(Path(api_path).read_text())
    model = gen.Model(api)
    defs, asserts, done = [], {8: [], 4: []}, set()

    def emit(name):
        if name in done:
            return
        done.add(name)
        fields = []
        for f in model.structs[name]["fields"]:
            ct, dims = ctype(model, f["fieldtype"])
            if ct.startswith("struct "):
                emit(ct[7:])
            fields.append(f"  {ct} {f['fieldname']}{dims};")
        defs.append(f"struct {name} {{\n" + "\n".join(fields or ["  char _empty;"]) + "\n};")
        for pack in (8, 4):
            lay = model.layout(name, pack)
            a = asserts[pack]
            a.append(f'_Static_assert(sizeof(struct {name}) == {lay["size"]}, "{name} size");')
            for fname, off in lay["fields"]:
                a.append(f'_Static_assert(offsetof(struct {name}, {fname}) == {off}, "{name}.{fname}");')

    checked = skipped = 0
    for s in api["structs"] + api["callback_structs"]:
        try:
            snapshot = (list(defs), {k: list(v) for k, v in asserts.items()}, set(done))
            emit(s["struct"])
            checked += 1
        except gen.Unsupported:
            defs[:], done = snapshot[0], snapshot[2]
            asserts.update(snapshot[1])
            skipped += 1

    Path(out_path).write_text(
        "/* сгенерировано layout_check.py — не править */\n#include <stdbool.h>\n#include <stddef.h>\n#include <stdint.h>\n"
        "#pragma pack(push, PACK)\n" + "\n".join(defs) + "\n#pragma pack(pop)\n"
        + "#if PACK == 8\n" + "\n".join(asserts[8]) + "\n#else\n" + "\n".join(asserts[4]) + "\n#endif\n")
    print(f"структур {checked}, пропущено {skipped}", file=sys.stderr)


if __name__ == "__main__":
    main(*sys.argv[1:3])
