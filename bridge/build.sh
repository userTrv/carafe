#!/usr/bin/env bash
# Сборка моста: steam_api64.dll (Windows, zig) и carafe-steamd (Mac, clang).
# build.sh <оригинальный steam_api64.dll игры> <libsteam_api.dylib того же SDK> <каталог вывода>
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
orig_dll="${1:?нужен steam_api64.dll игры}"
dylib="${2:?нужен libsteam_api.dylib}"
out="${3:?нужен каталог вывода}"
zig="${ZIG:-zig}"

# Собираем во временный каталог и подменяем файлы переименованием: запущенные игра и хелпер
# держат старые файлы и не падают от перезаписи кода под ними.
stage="$out/.stage"
rm -rf "$stage"
mkdir -p "$stage/gen"
python3 -I "$here/gen.py" "$here/sdk/steam_api.json" "$orig_dll" "$stage/gen"

# Раскладки gen.py против настоящих компиляторов: Windows x64 (pack 8) и Mac arm64 (pack 4).
python3 -I "$here/test/layout_check.py" "$here/sdk/steam_api.json" "$stage/layout_check.c"
"$zig" cc -target x86_64-windows-gnu -c -DPACK=8 -o "$stage/layout_check.obj" "$stage/layout_check.c"
clang -c -DPACK=4 -o "$stage/layout_check.o" "$stage/layout_check.c"

warn=(-Wall -Wno-unused-function -Wno-unused-variable -Wno-unused-but-set-variable)

"$zig" cc -target x86_64-windows-gnu -O2 -shared "${warn[@]}" \
  -I"$here/shim" -I"$stage/gen" \
  "$here/shim/shim.c" "$stage/gen/shim_gen.c" \
  -lws2_32 -o "$stage/steam_api64.dll"

cp "$dylib" "$stage/libsteam_api.dylib"
clang -O2 "${warn[@]}" -I"$here/helper" -I"$stage/gen" \
  "$here/helper/steamd.c" "$here/helper/cloud.c" "$stage/gen/helper_gen.c" \
  -L"$stage" -lsteam_api -lcurl -Wl,-rpath,@executable_path \
  -o "$stage/carafe-steamd"

rm -rf "$out/gen"
for f in gen steam_api64.dll libsteam_api.dylib carafe-steamd; do mv -f "$stage/$f" "$out/$f"; done
rm -rf "$stage"  # .pdb и .lib от zig не нужны
echo "готово: $out/steam_api64.dll, $out/carafe-steamd" >&2
