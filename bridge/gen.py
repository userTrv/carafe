#!/usr/bin/env python3
"""Генератор моста Steamworks: steam_api.json (SDK) + экспорт Windows-DLL игры → C-код.

Выход в OUT/:
  shim_gen.c    — Windows-сторона: экспортируемые функции steam_api64.dll, пакуют аргументы в запрос.
  helper_gen.c  — Mac-сторона: распаковка запроса и вызов настоящего libsteam_api.dylib.
  layouts.h     — раскладки структур для Windows (pack 8) и Mac (pack 4) и таблица колбэков.
"""
import json
import re
import struct
import sys
from pathlib import Path

GEN_BASE = 100  # id 1..99 — ручные функции ядра (proto.h)

# Функции ядра, написанные руками в shim.c / steamd.c.
CORE = {
    "SteamAPI_Init", "SteamAPI_InitSafe", "SteamAPI_InitFlat", "SteamInternal_SteamAPI_Init",
    "SteamAPI_Shutdown", "SteamAPI_RestartAppIfNecessary", "SteamAPI_IsSteamRunning",
    "SteamAPI_GetHSteamPipe", "SteamAPI_GetHSteamUser", "SteamAPI_RunCallbacks",
    "SteamAPI_ReleaseCurrentThreadMemory", "SteamAPI_GetSteamInstallPath",
    "SteamAPI_ManualDispatch_Init", "SteamAPI_ManualDispatch_RunFrame",
    "SteamAPI_ManualDispatch_GetNextCallback", "SteamAPI_ManualDispatch_FreeLastCallback",
    "SteamAPI_ManualDispatch_GetAPICallResult", "SteamAPI_ISteamUtils_GetAPICallResult",
}

# Экспорты вне steam_api.json: через них Steamworks.NET получает ISteamClient после init.
EXTRA_FNS = {
    "SteamInternal_CreateInterface": {
        "params": [{"paramname": "ver", "paramtype": "const char *"}], "returntype": "void *"},
    "SteamInternal_FindOrCreateUserInterface": {
        "params": [{"paramname": "hSteamUser", "paramtype": "HSteamUser"},
                   {"paramname": "ver", "paramtype": "const char *"}], "returntype": "void *"},
    "SteamInternal_FindOrCreateGameServerInterface": {
        "params": [{"paramname": "hSteamUser", "paramtype": "HSteamUser"},
                   {"paramname": "ver", "paramtype": "const char *"}], "returntype": "void *"},
}

# Аннотации, которых нет в steam_api.json: без них массив принимается за одно число.
OVERRIDES = {
    "SteamAPI_ISteamUserStats_UploadLeaderboardScore": {"pScoreDetails": ("array_count", "cScoreDetailsCount")},
    "SteamAPI_ISteamUserStats_GetDownloadedLeaderboardEntry": {"pDetails": ("out_array_count", "cDetailsMax")},
    "SteamAPI_ISteamRemoteStorage_FileRead": {"pvData": ("out_buffer_count", "cubDataToRead")},
    "SteamAPI_ISteamRemoteStorage_FileWrite": {"pvData": ("buffer_count", "cubData")},
}

PRIM = {
    "unsigned char": ("u", 1), "uint8": ("u", 1), "signed char": ("s", 1), "int8": ("s", 1), "char": ("s", 1),
    "short": ("s", 2), "unsigned short": ("u", 2), "int": ("s", 4), "unsigned int": ("u", 4),
    "long long": ("s", 8), "unsigned long long": ("u", 8), "float": ("f", 4), "double": ("d", 8),
    "bool": ("b", 1), "uint64_steamid": ("u", 8), "uint64_gameid": ("u", 8),
    "CSteamID": ("u", 8), "CGameID": ("u", 8), "size_t": ("u", 8), "intptr_t": ("s", 8),
    "uint8_t": ("u", 1), "uint16_t": ("u", 2), "uint32_t": ("u", 4), "uint64_t": ("u", 8),
    "int8_t": ("s", 1), "int16_t": ("s", 2), "int32_t": ("s", 4), "int64_t": ("s", 8),
}
CTYPE = {("u", 1): "uint8_t", ("u", 2): "uint16_t", ("u", 4): "uint32_t", ("u", 8): "uint64_t",
         ("s", 1): "int8_t", ("s", 2): "int16_t", ("s", 4): "int32_t", ("s", 8): "int64_t",
         ("f", 4): "float", ("d", 8): "double", ("b", 1): "bool", ("p", 8): "void *"}


class Unsupported(Exception):
    pass


def pe_exports(path):
    """Имена экспортов PE-файла (x64)."""
    b = Path(path).read_bytes()
    pe = struct.unpack_from("<I", b, 0x3C)[0]
    nsec = struct.unpack_from("<H", b, pe + 6)[0]
    opt = pe + 24
    exp_rva = struct.unpack_from("<I", b, opt + 112)[0]
    sec_tab = opt + struct.unpack_from("<H", b, pe + 20)[0]
    secs = [struct.unpack_from("<8sIIII", b, sec_tab + 40 * i) for i in range(nsec)]

    def off(rva):
        for _, vsize, va, rsize, raw in secs:
            if va <= rva < va + max(vsize, rsize):
                return rva - va + raw
        raise ValueError(rva)

    e = off(exp_rva)
    n_names, names_rva = struct.unpack_from("<I", b, e + 24)[0], struct.unpack_from("<I", b, e + 32)[0]
    out = []
    for i in range(n_names):
        s = off(struct.unpack_from("<I", b, off(names_rva) + 4 * i)[0])
        out.append(b[s:b.index(b"\0", s)].decode())
    return out


class Model:
    def __init__(self, api):
        self.typedefs = {t["typedef"]: t["type"] for t in api["typedefs"]}
        self.enums = {e["enumname"] for e in api["enums"]}
        for i in api["interfaces"]:
            self.enums |= {e["enumname"] for e in i.get("enums", [])}
        self.structs = {}
        for s in api["structs"] + api["callback_structs"]:
            self.structs[s["struct"]] = s
            self.enums |= {e["enumname"] for e in s.get("enums", [])}
        self.layout_cache = {}

    def prim(self, t):
        """Скаляр → (вид, размер) или None."""
        t = t.strip()
        seen = 0
        while t in self.typedefs and seen < 10:
            t = self.typedefs[t].strip()
            seen += 1
        if t in PRIM:
            return PRIM[t]
        if t in self.enums or "::" in t or (t.startswith("E") and t[1:2].isupper() and t not in self.structs):
            return ("s", 4)
        if t.endswith("*") and "(" not in t:
            return ("p", 8)
        return None

    # --- раскладки структур ---
    def field_layout(self, t, pack):
        """(размер, выравнивание, листья[(off, size, kind)]) поля типа t."""
        t = t.strip()
        if t.endswith("]"):
            base, n = t[:t.rindex("[")].strip(), int(t[t.rindex("[") + 1:-1])
            size, align, leaves = self.field_layout(base, pack)
            out = []
            for k in range(n):
                out += [(o + k * size, s, kind) for o, s, kind in leaves]
            return size * n, align, out
        if t in ("CSteamID", "CGameID"):
            return 8, min(8, pack), [(0, 8, "c")]
        if t.endswith("*"):
            return 8, min(8, pack), [(0, 8, "z")]  # указатель из чужого процесса — обнуляем
        p = self.prim(t)
        if p:
            return p[1], min(p[1], pack), [(0, p[1], "c")]
        if t in self.structs:
            lay = self.layout(t, pack)
            return lay["size"], lay["align"], lay["leaves"]
        raise Unsupported(f"field type {t}")

    def layout(self, name, pack):
        key = (name, pack)
        if key not in self.layout_cache:
            off, maxa, leaves = 0, 1, []
            for f in self.structs[name]["fields"]:
                size, align, fl = self.field_layout(f["fieldtype"], pack)
                off = (off + align - 1) // align * align
                leaves += [(off + o, s, k) for o, s, k in fl]
                off += size
                maxa = max(maxa, align)
            self.layout_cache[key] = {"size": (off + maxa - 1) // maxa * maxa or 1, "align": maxa, "leaves": leaves}
        return self.layout_cache[key]

    def conv(self, name):
        """Таблица копирования Windows ↔ Mac: [(woff, moff, size, kind)], склеенная по смежности."""
        w, m = self.layout(name, 8), self.layout(name, 4)
        out = []
        for (wo, ws, wk), (mo, ms, mk) in zip(w["leaves"], m["leaves"]):
            assert ws == ms and wk == mk
            if out and wk == "c" == out[-1][3] and out[-1][0] + out[-1][2] == wo and out[-1][1] + out[-1][2] == mo:
                out[-1] = (out[-1][0], out[-1][1], out[-1][2] + ws, "c")
            else:
                out.append((wo, mo, ws, wk))
        return w["size"], m["size"], out


# Имя параметра-размера у буферов без аннотации: cubBuffer, cchText, nDestBufferSize, cMaxEntries…
SIZE_NAME = re.compile(r"^(cub|cch|cb[A-Z]|unNum)|Size|Sz$|Max|Len|Buf|Count")


def strip_ptr(t):
    t = t.strip()
    const = t.startswith("const ")
    if const:
        t = t[6:]
    return const, t.rstrip("*&").strip(), t.count("*") + t.count("&")


def classify(model, fn):
    """Разобрать параметры метода. Вернёт список (вид, имя, c-тип, доп) или бросит Unsupported."""
    params = []
    if fn.get("self"):
        params.append(("scalar", "self", "void *", ("p", 8)))
    names = {p["paramname"]: p for p in fn["params"]}
    fixes = OVERRIDES.get(fn.get("methodname_flat"), {})
    plist = fn["params"]
    for idx, p in enumerate(plist):
        if p["paramname"] in fixes:
            k, v = fixes[p["paramname"]]
            p = dict(p, **{k: v})
        name, t = "a_" + p["paramname"], p.get("paramtype_flat", p["paramtype"]).strip()
        const, base, depth = strip_ptr(t)
        if t == "const char *":
            params.append(("str", name, "const char *", None))
            continue
        if depth == 0:
            pr = model.prim(t)
            if not pr:
                raise Unsupported(f"by-value {t}")
            params.append(("scalar", name, CTYPE[pr], pr))
            continue
        if depth > 1:
            raise Unsupported(f"pointer-to-pointer {t}")
        elem = 1 if base == "void" else None
        if elem is None:
            bp = model.prim(base)
            if bp and bp[0] != "p":
                elem = bp[1]
            elif base in model.structs:
                elem = model.layout(base, 8)["size"]
        ann = next(((k, p[k]) for k in ("out_string_count", "out_array_count", "out_buffer_count",
                                        "array_count", "buffer_count", "out_struct") if k in p), None)
        # Буфер без аннотации: следом идёт целое с именем размера. Гоним в обе стороны —
        # среди таких есть и входные (StartPlaytimeTracking), и выходные (GetImageRGBA).
        nxt = plist[idx + 1] if idx + 1 < len(plist) else None
        if (not ann and elem and nxt and "*" not in nxt["paramtype"] and SIZE_NAME.search(nxt["paramname"])
                and (model.prim(nxt["paramtype"]) or ("x",))[0] in "us"
                and not (base in model.structs and model.conv(base)[0] != model.conv(base)[1])):
            params.append(("iobuf", name, "const void *" if const else "void *",
                           (elem, ("a_" + nxt["paramname"], False), const)))
            continue
        count_ref = None
        if ann and ann[0] != "out_struct":
            cname = ann[1]
            if cname not in names:
                raise Unsupported(f"count {cname}")
            ct = names[cname].get("paramtype_flat", names[cname]["paramtype"])
            count_ref = ("a_" + cname, "*" in ct)
        if ann and ann[0] in ("out_string_count", "out_array_count", "out_buffer_count") and not const:
            if elem is None or (base in model.structs and model.conv(base)[0] != model.conv(base)[1]):
                raise Unsupported(f"outbuf of {base}")
            params.append(("outbuf", name, "void *", (elem, count_ref)))
        elif ann and ann[0] in ("array_count", "buffer_count"):
            if elem is None or (base in model.structs and model.conv(base)[0] != model.conv(base)[1]):
                raise Unsupported(f"inbuf of {base}")
            params.append(("inbuf", name, "const void *", (elem, count_ref)))
        elif base in model.structs:
            model.conv(base)
            params.append(("struct", name, "const void *" if const else "void *", (base, const)))
        elif not const and model.prim(base) and model.prim(base)[0] != "p" and not ann:
            pr = model.prim(base)
            params.append(("outscalar", name, CTYPE[pr] + " *", pr))
        else:
            raise Unsupported(f"param {t} {ann}")
    rt = fn.get("returntype_flat", fn["returntype"]).strip()
    if rt == "void":
        ret = ("void", "void", None)
    elif rt == "const char *":
        ret = ("str", "const char *", None)
    else:
        pr = model.prim(rt)
        if not pr:
            raise Unsupported(f"return {rt}")
        ret = ("scalar", CTYPE[pr], pr)
    return params, ret


def enc(pr, expr):
    k, _ = pr
    if k == "f":
        return f"bw_f32({expr})"
    if k == "d":
        return f"bw_f64({expr})"
    if k == "b":
        return f"(uint64_t)({expr} ? 1 : 0)"
    if k == "p":
        return f"(uint64_t)(uintptr_t)({expr})"
    if k == "s":
        return f"(uint64_t)(int64_t)({expr})"
    return f"(uint64_t)({expr})"


def dec(pr, expr):
    k, _ = pr
    ct = CTYPE[pr]
    if k == "f":
        return f"br_f32({expr})"
    if k == "d":
        return f"br_f64({expr})"
    if k == "b":
        return f"({expr} != 0)"
    if k == "p":
        return f"(void *)(uintptr_t)({expr})"
    return f"({ct})({expr})"


def count_expr(ref):
    name, is_ptr = ref
    return f"({name} ? (uint32_t)*{name} : 0u)" if is_ptr else f"(uint32_t)({name})"


def gen_shim(fid, name, params, ret):
    sig = ", ".join(f"{ct} {n}" for _, n, ct, _ in params) or "void"
    rtype = ret[1]
    L = [f"EXPORT {rtype} {name}({sig}) {{", f"  bw w; bw_begin(&w, {fid});"]
    for kind, n, ct, x in params:
        if kind == "scalar":
            L.append(f"  bw_u64(&w, {enc(x, n)});")
        elif kind == "str":
            L.append(f"  bw_str(&w, {n});")
        elif kind == "outscalar":
            L.append(f"  bw_u8(&w, {n} != 0); if ({n}) bw_u64(&w, {enc(x, '*' + n)});")
        elif kind == "outbuf":
            L.append(f"  bw_u8(&w, {n} != 0); if ({n}) bw_u32(&w, {count_expr(x[1])} * {x[0]}u);")
        elif kind in ("inbuf", "iobuf"):
            L.append(f"  bw_buf(&w, {n}, {n} ? {count_expr(x[1])} * {x[0]}u : 0);")
        elif kind == "struct":
            L.append(f"  bw_u8(&w, {n} != 0); if ({n}) bw_raw(&w, {n}, {struct_wsize_expr(x[0])});")
    zero = "" if ret[0] == "void" else ("NULL" if ret[0] == "str" else f"({rtype})0")
    L.append(f"  br r; if (!bridge_call(&w, &r)) return {zero};")
    if ret[0] == "scalar":
        L.append(f"  {rtype} ret = {dec(ret[2], 'br_u64(&r)')};")
    elif ret[0] == "str":
        L.append("  const char *ret = bridge_intern(br_str(&r));")
    for kind, n, ct, x in params:
        if kind == "outscalar":
            L.append(f"  if ({n}) *{n} = {dec(x, 'br_u64(&r)')};")
        elif kind == "outbuf" or (kind == "iobuf" and not x[2]):
            L.append(f"  if ({n}) br_copy(&r, {n});")
        elif kind == "struct" and not x[1]:
            L.append(f"  if ({n}) br_copy(&r, {n});")
    L.append("  bridge_done(&r);")
    if ret[0] != "void":
        L.append("  return ret;")
    L.append("}")
    return "\n".join(L)


STRUCT_IDS = {}


def struct_wsize_expr(sname):
    return f"LAYOUTS[{STRUCT_IDS[sname]}].wsize"


def gen_helper(fid, name, params, ret):
    L = [f"  case {fid}: {{ /* {name} */"]
    args = []
    for kind, n, ct, x in params:
        if kind == "scalar":
            L.append(f"    {ct} {n} = {dec(x, 'br_u64(r)')};")
            args.append(n)
        elif kind == "str":
            L.append(f'    const char *{n} = br_str(r); log_arg({fid}, "{n[2:]}", {n});')
            args.append(n)
        elif kind == "outscalar":
            base = CTYPE[x]
            L.append(f"    uint8_t has_{n} = br_u8(r); {base} v_{n} = has_{n} ? {dec(x, 'br_u64(r)')} : 0;")
            args.append(f"has_{n} ? &v_{n} : NULL")
        elif kind == "outbuf":
            L.append(f"    uint8_t has_{n} = br_u8(r); uint32_t len_{n} = has_{n} ? br_u32(r) : 0;"
                     f" if (len_{n} > WIRE_MAX) return -1;"
                     f" void *{n} = has_{n} ? tmp_alloc(len_{n}) : NULL;")
            args.append(n)
        elif kind == "inbuf":
            L.append(f"    const void *{n} = br_buf(r);")
            args.append(n)
        elif kind == "iobuf":
            # Копия во временную память: Steam пишет в буфер, а кадр запроса только для чтения.
            L.append(f"    uint32_t len_{n}; const void *src_{n} = br_buf_len(r, &len_{n}); void *{n} = NULL;"
                     f" if (src_{n}) {{ {n} = tmp_alloc(len_{n}); memcpy({n}, src_{n}, len_{n}); }}")
            args.append(n)
        elif kind == "struct":
            sid = STRUCT_IDS[x[0]]
            L.append(f"    uint8_t has_{n} = br_u8(r); void *{n} = NULL;"
                     f" if (has_{n}) {{ {n} = tmp_alloc(LAYOUTS[{sid}].msize);"
                     f" win_to_mac(&LAYOUTS[{sid}], br_raw(r, LAYOUTS[{sid}].wsize), {n}); }}")
            args.append(n)
    call = f"{name}({', '.join(args)})"
    if ret[0] == "void":
        L.append(f"    {call};")
    elif ret[0] == "str":
        L.append(f"    bw_str(w, {call});")
    else:
        L.append(f"    {ret[1]} ret = {call};")
        L.append(f"    bw_u64(w, {enc(ret[2], 'ret')});")
    for kind, n, ct, x in params:
        if kind == "outscalar":
            L.append(f"    if (has_{n}) bw_u64(w, {enc(x, 'v_' + n)});")
        elif kind == "outbuf":
            L.append(f"    if (has_{n}) bw_buf(w, {n}, len_{n});")
        elif kind == "iobuf" and not x[2]:
            L.append(f"    if ({n}) bw_buf(w, {n}, len_{n});")
        elif kind == "struct" and not x[1]:
            sid = STRUCT_IDS[x[0]]
            L.append(f"    if (has_{n}) {{ void *o = tmp_alloc(LAYOUTS[{sid}].wsize);"
                     f" mac_to_win(&LAYOUTS[{sid}], {n}, o); bw_buf(w, o, LAYOUTS[{sid}].wsize); }}")
    L.append("    return 1; }")
    return "\n".join(L)


def proto(name, params, ret):
    sig = ", ".join(ct for _, _, ct, _ in params) or "void"
    return f"extern {ret[1]} {name}({sig});"


def main(api_path, dll_path, out):
    api = json.loads(Path(api_path).read_text())
    model = Model(api)
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    exports = pe_exports(dll_path)

    fns = dict(EXTRA_FNS)
    for i in api["interfaces"]:
        for a in i.get("accessors") or []:
            fns[a["name_flat"]] = {"params": [], "returntype": i["classname"] + " *"}
        for m in i.get("methods", []):
            fns[m["methodname_flat"]] = dict(m, self=True)

    # Раскладки: все колбэки + структуры, встречающиеся в параметрах.
    lay_names = [c["struct"] for c in api["callback_structs"]] + [s["struct"] for s in api["structs"]]
    layouts = []
    for n in lay_names:
        try:
            wsize, msize, leaves = model.conv(n)
        except Unsupported:
            continue
        STRUCT_IDS[n] = len(layouts)
        cb = next((c.get("callback_id") for c in api["callback_structs"] if c["struct"] == n), -1)
        layouts.append((n, cb, wsize, msize, leaves))

    shim, helper, protos, names, stubs = [], [], [], {}, []
    fid = GEN_BASE
    unsupported = {}
    for name in exports:
        if name in CORE:
            continue
        fn = fns.get(name)
        if fn is None:
            stubs.append(name)
            continue
        try:
            params, ret = classify(model, fn)
        except Unsupported as e:
            unsupported[name] = str(e)
            stubs.append(name)
            continue
        shim.append(gen_shim(fid, name, params, ret))
        helper.append(gen_helper(fid, name, params, ret))
        protos.append(proto(name, params, ret))
        names[fid] = name
        fid += 1

    for name in stubs:
        shim.append(f'EXPORT uint64_t {name}(void) {{ bridge_unsupported("{name}"); return 0; }}')

    L = ["/* сгенерировано gen.py — не править */", "#pragma once", "#include <stdint.h>",
         "typedef struct { uint32_t woff, moff, size; uint8_t zero; } leaf_t;",
         "typedef struct { const char *name; int32_t cb; uint32_t wsize, msize, nleaves; const leaf_t *leaves; } layout_t;"]
    for i, (n, cb, ws, ms, leaves) in enumerate(layouts):
        body = ", ".join(f"{{{wo},{mo},{s},{int(k == 'z')}}}" for wo, mo, s, k in leaves) or "{0,0,0,0}"
        L.append(f"static const leaf_t LEAVES_{i}[] = {{{body}}};")
    L.append("static const layout_t LAYOUTS[] = {")
    for i, (n, cb, ws, ms, leaves) in enumerate(layouts):
        L.append(f'  {{"{n}", {cb}, {ws}, {ms}, {len(leaves)}, LEAVES_{i}}},')
    L.append("};")
    L.append(f"#define N_LAYOUTS {len(layouts)}")
    (out / "layouts.h").write_text("\n".join(L) + "\n")

    (out / "shim_gen.c").write_text(
        '/* сгенерировано gen.py — не править */\n#include "shim.h"\n#include "layouts.h"\n\n' + "\n\n".join(shim) + "\n")
    (out / "helper_gen.c").write_text(
        "/* сгенерировано gen.py — не править */\n#include \"steamd.h\"\n#include \"layouts.h\"\n\n"
        + "\n".join(protos)
        + "\n\nconst char *gen_name(uint32_t id) {\n  switch (id) {\n"
        + "\n".join(f'  case {k}: return "{v}";' for k, v in names.items())
        + "\n  default: return NULL;\n  }\n}\n\n"
        + "int gen_dispatch(uint32_t id, br *r, bw *w) {\n  switch (id) {\n"
        + "\n".join(helper) + "\n  default: return 0;\n  }\n}\n")
    print(f"exports {len(exports)}: bridged {len(names)}, core {len(CORE & set(exports))}, "
          f"stubs {len(stubs)} (unsupported {len(unsupported)}), layouts {len(layouts)}", file=sys.stderr)
    (out / "unsupported.txt").write_text("".join(f"{k}\t{v}\n" for k, v in sorted(unsupported.items())))


if __name__ == "__main__":
    main(*sys.argv[1:4])
