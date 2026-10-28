#!/usr/bin/env bash
# Сборка моста: steam_api64.dll (Windows, zig) и carafe-steamd (Mac, clang).
# build.sh <оригинальный steam_api64.dll игры> <libsteam_api.dylib того же SDK> <каталог вывода>
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
orig_dll="${1:?нужен steam_api64.dll игры}"
dylib="${2:?нужен libsteam_api.dylib}"
out="${3:?нужен каталог вывода}"
zig="${ZIG:-zig}"

mkdir -p "$out/gen"
python3 -I "$here/gen.py" "$here/sdk/steam_api.json" "$orig_dll" "$out/gen"

warn=(-Wall -Wno-unused-function -Wno-unused-variable -Wno-unused-but-set-variable)

"$zig" cc -target x86_64-windows-gnu -O2 -shared "${warn[@]}" \
  -I"$here/shim" -I"$out/gen" \
  "$here/shim/shim.c" "$out/gen/shim_gen.c" \
  -lws2_32 -o "$out/steam_api64.dll"

cp "$dylib" "$out/libsteam_api.dylib"
clang -O2 "${warn[@]}" -I"$here/helper" -I"$out/gen" \
  "$here/helper/steamd.c" "$here/helper/cloud.c" "$out/gen/helper_gen.c" \
  -L"$out" -lsteam_api -lcurl -Wl,-rpath,@executable_path \
  -o "$out/carafe-steamd"

echo "готово: $out/steam_api64.dll, $out/carafe-steamd" >&2
