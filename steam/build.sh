#!/usr/bin/env bash
# Сборка carafe-steam: один исполняемый файл с рантаймом .NET внутри.
#   steam/build.sh                 → dist/osx-arm64/carafe-steam
#   steam/build.sh osx-x64         → dist/osx-x64/carafe-steam
#   steam/build.sh osx-arm64 osx-x64
# Зависимости — строго по packages.lock.json (--locked-mode): пакет с другим contentHash
# или новая версия сборку роняют. Нужен .NET SDK из global.json.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1

DOTNET="${DOTNET:-$(command -v dotnet || echo "$HOME/.dotnet/dotnet")}"
[[ -x "$DOTNET" ]] || { echo "build.sh: нет dotnet (нужен SDK из global.json)" >&2; exit 1; }

rids=("$@")
[[ ${#rids[@]} -gt 0 ]] || rids=(osx-arm64)

"$DOTNET" restore --locked-mode
for rid in "${rids[@]}"; do
  echo "==> carafe-steam $rid" >&2
  "$DOTNET" publish carafe-steam.csproj -c Release -r "$rid" --no-restore -o "dist/$rid"
  # Рядом с бинарником .NET кладёт pdb/xml — они не нужны (отладка встроена в сборку).
  # SteamKit2.dll остаётся отдельным файлом: LGPL, пользователь может заменить её своей.
  find "dist/$rid" -type f ! -name carafe-steam ! -name SteamKit2.dll -delete
  [[ -f "dist/$rid/SteamKit2.dll" ]] || { echo "build.sh: SteamKit2.dll попала внутрь бандла" >&2; exit 1; }
  echo "dist/$rid/carafe-steam (+ SteamKit2.dll рядом)"
done
