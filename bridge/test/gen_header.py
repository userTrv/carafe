#!/usr/bin/env python3
"""C++-заголовок интерфейсов Steamworks из steam_api.json — как их видит игра.

gen_header.py <steam_api.json> <steam_api64.dll игры> <out.hpp>
Рядом пишет steam_ver.h для C: имена flat-аксессоров (ACC_SteamUser…) и HAS_<класс>_<метод> —
чтобы тест собирался под любой SDK из каталога.
Порядок методов восстанавливается по слотам из DLL игры (gen.vtable_slots): json не знает
приватных методов SDK, а место в vtable они занимают — на их месте ставятся заглушки.
Раскладку из этого заголовка считает компилятор: clang под x86_64-pc-windows-msvc — как MSVC
у игр (vt_check.py сверяет её со слотами DLL), clang под Mac — как настоящий libsteam_api
(это проверяет нативный прогон cpptest.cpp).
С -DVT_DUMP методы объявлены не чистыми: так clang строит vftable и печатает раскладку.
"""
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import gen  # noqa: E402

BUILTIN = {"CSteamID", "CGameID"}


def main(api_path, dll_path, out_path):
    api = json.loads(Path(api_path).read_text())
    vslots = gen.vtable_slots(api, dll_path)
    V = ["/* сгенерировано gen_header.py — не править */", "#pragma once"]
    for i in api["interfaces"]:
        for a in i.get("accessors") or []:
            if a.get("kind") == "user":
                V.append(f'#define ACC_{a["name"]} "{a["name_flat"]}"')
        for m in {m["methodname"] for m in i["methods"]}:
            V.append(f"#define HAS_{i['classname']}_{m} 1")
    Path(out_path).with_name("steam_ver.h").write_text("\n".join(V) + "\n")
    L = ["/* сгенерировано gen_header.py — не править */", "#pragma once", "#include <stddef.h>", "#include <stdint.h>",
         '#include "steam_ver.h"', "",
         "#ifdef VT_DUMP", "#define PURE", "#else", "#define PURE = 0", "#endif", ""]
    for s in api["structs"] + api["callback_structs"]:
        if s["struct"] not in BUILTIN:
            L.append(f"struct {s['struct']};")
    for i in api["interfaces"]:
        L.append(f"class {i['classname']};")
    for e in api["enums"]:
        L.append(f"enum {e['enumname']} : int {{}};")
    # Типы, которых нет в json: для объявлений методов хватает неполных.
    known = BUILTIN | {t["typedef"] for t in api["typedefs"]} | {e["enumname"] for e in api["enums"]}
    known |= {s["struct"] for s in api["structs"] + api["callback_structs"]} | {i["classname"] for i in api["interfaces"]}
    known |= {"void", "char", "bool", "int", "float", "double", "short", "long", "unsigned", "signed", "const", "size_t", "intptr_t", "uintptr_t"}
    known |= {f"{u}int{n}_t" for u in ("", "u") for n in (8, 16, 32, 64)}
    used = set()
    for t in [t["type"] for t in api["typedefs"]] + [p["paramtype"] for i in api["interfaces"] for m in i["methods"]
                                                    for p in m["params"]] + [m["returntype"] for i in api["interfaces"] for m in i["methods"]]:
        if "::" not in t:
            used |= set(re.findall(r"[A-Za-z_]\w*", t))
    for n in sorted(used - known):
        L.append(f"enum {n} : int {{}};" if re.match(r"E[A-Z]", n) else f"struct {n};")
    for t in api["typedefs"]:
        name, ty = t["typedef"], t["type"]
        if name in BUILTIN:
            continue
        if "(*)" in ty:
            L.append(f"typedef {ty.replace('(*)', f'(*{name})', 1)};")
        elif ty.endswith("]"):
            L.append(f"typedef {ty[:ty.index('[')].strip()} {name}{ty[ty.index('['):]};")
        else:
            L.append(f"typedef {ty} {name};")
    L += ["struct CSteamID { uint64 m_steamid; };", "struct CGameID { uint64 m_gameID; };"]
    for i in api["interfaces"]:
        if i.get("version_string"):
            L.append(f'#define VER_{i["classname"]} "{i["version_string"]}"')
    client = max((v for v in gen.dll_versions(dll_path) if re.fullmatch(r"SteamClient\d{3}", v)), default=None)
    if client:
        L.append(f'#define VER_ISteamClient "{client}"  /* из DLL игры: в json версии клиента нет */')
    L.append("")
    for i in api["interfaces"]:
        L.append(f"class {i['classname']} {{")
        L.append("public:")
        for e in i.get("enums", []):
            L.append(f"  enum {e['enumname']} : int {{}};")
        order = gen.decl_order(vslots[i["classname"]]) if i["classname"] in vslots else i["methods"]
        for k, m in enumerate(order):
            if m is None:
                L.append(f"  virtual void carafe_private_{k}() PURE;")
                continue
            ps = ", ".join(f"{p['paramtype']} {p['paramname']}" for p in m["params"])
            L.append(f"  virtual {m['returntype']} {m['methodname']}({ps}) PURE;")
        L.append("};")
        L.append("")
    L.append("""/* Как в steam_api_internal.h. */
class CCallbackBase {
public:
  CCallbackBase() { m_nCallbackFlags = 0; m_iCallback = 0; }
  virtual void Run(void *pvParam) PURE;
  virtual void Run(void *pvParam, bool bIOFailure, SteamAPICall_t hSteamAPICall) PURE;
  int GetICallback() { return m_iCallback; }
  virtual int GetCallbackSizeBytes() PURE;
protected:
  enum { k_ECallbackFlagsRegistered = 0x01, k_ECallbackFlagsGameServer = 0x02 };
  uint8 m_nCallbackFlags;
  int m_iCallback;
};""")
    L.append("")
    L.append("#ifdef VT_DUMP")
    L.append("inline void *operator new(decltype(sizeof 0), void *p) noexcept { return p; }")
    for i in api["interfaces"] + [{"classname": "CCallbackBase"}]:
        c = i["classname"]
        L.append(f"void vt_dump_{c}(void *p) {{ new (p) {c}(); }}")
    L.append("#endif")
    Path(out_path).write_text("\n".join(L) + "\n")


if __name__ == "__main__":
    main(*sys.argv[1:4])
