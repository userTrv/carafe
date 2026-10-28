#!/usr/bin/env python3
"""Какой SDK Steamworks у игры: по строкам версий интерфейсов, зашитым в её steam_api64.dll.

sdk_detect.py <steam_api64.dll> <каталог bridge/sdk>
Печатает имя подкаталога SDK (например, 1.60), если все версии интерфейсов из его
steam_api.json есть в DLL. Valve меняет версию интерфейса при любом изменении методов,
поэтому совпадение версий — совпадение API. Подходят несколько (новый SDK бывает только
убирает интерфейсы, как 1.63 после 1.62) — берётся самый полный, при равенстве — у кого
больше flat-функций совпало с экспортами DLL. Нет подходящего — код 1 и версии из DLL.
"""
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen  # noqa: E402


def main(dll, sdk_root):
    have = gen.dll_versions(dll)
    exports = set(gen.pe_exports(dll))
    best = None
    for api_path in sorted(Path(sdk_root).glob("*/steam_api.json")):
        api = json.loads(api_path.read_text())
        want = {i["version_string"] for i in api["interfaces"] if i.get("version_string")}
        if want and want <= have:
            flat = {m["methodname_flat"] for i in api["interfaces"] for m in i["methods"]}
            score = (len(want), len(flat & exports))
            if best is None or score > best[0]:
                best = (score, api_path.parent.name)
    if best:
        print(best[1])
        return 0
    asked = sorted(v for v in have if not v.startswith("SteamAPI_") and re.search(r"\d{3}$", v))
    print("SDK игры не из каталога bridge/sdk. Версии интерфейсов в её DLL:\n  " + " ".join(asked), file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:3]))
