#!/usr/bin/env python3
"""Сверка слотов vtable из DLL игры (gen.vtable_slots) с раскладкой, которую clang для
x86_64-pc-windows-msvc строит по заголовку gen_header.py. Совпало — значит, заголовок
(порядок объявления, перегрузки, приватные методы) восстановлен верно и тест C++-пути честный.

vt_check.py <steam_api.json> <steam_api64.dll> <дамп clang -fdump-vtable-layouts>
"""
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import gen  # noqa: E402


def norm(types):
    t = re.sub(r"\b(class|struct|enum)\s+", "", types)
    t = re.sub(r"\bISteam\w+::", "", t)
    return re.sub(r"\s+", "", t)


def parse_dump(text):
    tables, cur = {}, None
    for line in text.splitlines():
        m = re.match(r"VFTable indices for '(\w+)'", line)
        if m:
            cur = tables.setdefault(m.group(1), {})
            continue
        m = re.match(r"\s+(\d+) \| .*?\b\w+::(\w+)\((.*)\)", line)
        if m and cur is not None:
            cur[int(m.group(1))] = (m.group(2), norm(m.group(3)))
        elif not line.strip():
            cur = None
    return tables


def main(api_path, dll_path, dump_path):
    api = json.loads(Path(api_path).read_text())
    vslots = gen.vtable_slots(api, dll_path)
    tables = parse_dump(Path(dump_path).read_text())
    bad = checked = 0
    for cls, slots in vslots.items():
        got = tables.get(cls)
        if got is None or len(got) != max(slots) + 1:
            print(f"{cls}: у clang {len(got or {})} слотов, в DLL {max(slots) + 1}", file=sys.stderr)
            bad += 1
            continue
        for k in range(max(slots) + 1):
            m = slots.get(k)
            name, types = got[k]
            want = (m["methodname"], norm(", ".join(p["paramtype"] for p in m["params"]))) if m else (f"carafe_private_{k}", "")
            if (name, types) != want:
                print(f"{cls}[{k}]: clang {name}({types}), DLL {want[0]}({want[1]})", file=sys.stderr)
                bad += 1
        checked += 1
    cb = tables.get("CCallbackBase", {})
    expect_cb = {0: ("Run", "void*,bool,SteamAPICall_t"), 1: ("Run", "void*"), 2: ("GetCallbackSizeBytes", "")}
    if cb != expect_cb:
        print(f"CCallbackBase: {cb}, shim.c ждёт {expect_cb}", file=sys.stderr)
        bad += 1
    print(f"vtable: интерфейсов {checked}, расхождений {bad}", file=sys.stderr)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main(*sys.argv[1:4])
