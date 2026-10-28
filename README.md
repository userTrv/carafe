# carafe

Минимальный запускатель Windows-игр на macOS (Apple Silicon) в духе Whisky, но без GUI и без второго Steam-клиента.

Внутри:

- **Wine + D3DMetal** — готовая сборка [Gcenx/game-porting-toolkit](https://github.com/Gcenx/game-porting-toolkit) (GPTK 3.0), DirectX 11/12 → Metal. Ничего не собирается, просто распаковывается.
- **steamcmd** от Valve — качает Windows-сборку игры с твоего аккаунта.
- Один bash-скрипт, всё хранится в `~/Library/Application Support/carafe`.

Нужна Rosetta 2 (`softwareupdate --install-rosetta`).

## Быстрый старт

```bash
bin/carafe engine install
bin/carafe bottle create
bin/carafe steam login <steam-логин>          # пароль и Steam Guard вводишь сам
bin/carafe game install <appid> <steam-логин>
bin/carafe game run <appid>
```

Без steamcmd — через уже залогиненный Steam для Mac: открой `steam://open/console`, выполни `download_depot <appid> <windows-depot>`, затем:

```bash
bin/carafe game import <appid>
```

## Мост в Steam для Mac

```bash
bin/carafe bridge build <appid>
```

После этого `game run` подменяет `steam_api64.dll` игры заглушкой, которая передаёт вызовы Steamworks нативному `carafe-steamd`, а тот говорит с запущенным Steam для Mac от имени игры. Проверено на Megabonk: лидерборды с никами и аватарами, чтение статов и ачивок. Выдача новых ачивок и отправка результата в лидерборд пока не проверены. Steam для Mac должен быть запущен.

Как устроено:

- `bridge/gen.py` генерирует обе стороны из `bridge/sdk/steam_api.json` (описание API из Steamworks.NET) и списка экспортов DLL игры. Колбэки и структуры перекладываются между pack 8 (Windows) и pack 4 (Mac).
- Связь — TCP на 127.0.0.1 с одноразовым токеном; хелпер живёт, пока игра подключена. Лог вызовов: `~/Library/Application Support/carafe/bridge-<appid>.log`.
- Mac-версия `libsteam_api.dylib` берётся из того же релиза Steamworks.NET, что у игры, по sha256 её `steam_api64.dll` (`steamapi_mac` в `bin/carafe`). Новая игра — новая строка в этой таблице.
- Для сборки нужен zig — carafe скачает его сам в `~/Library/Application Support/carafe/toolchain`.

`CARAFE_BRIDGE=0` запускает игру с оригинальной DLL.

## Steam Cloud

```bash
bin/carafe cloud status <appid>   # что куда поедет
bin/carafe cloud pull <appid>
bin/carafe cloud push <appid>
```

`game run` сам делает `pull` перед запуском и `push` после выхода (`CARAFE_CLOUD=0` — не трогать облако). Steam для Mac Auto-Cloud с Windows-путями не обслуживает, поэтому carafe делает это сам: правила `ufs` игры берёт из `app_info_print` steamcmd, а с облаком говорит через сервис `Cloud` Steam-клиента (`ISteamUnifiedMessages`) — тот же путь, что у самого клиента, без логина и пароля.

- Что синхронизировано, carafe помнит в `games/<appid>/.carafe-cloud-state`. Изменился один файл — едет в ту сторону. Изменились оба — побеждает более новый.
- Перед перезаписью проигравшая версия, локальная или облачная, кладётся в `backups/<appid>/<время>/`.
- Файлы облака, зашифрованные Steam, carafe не загружает и не перезаписывает.

## Ограничения

Без моста `SteamAPI_Init()` у игры не проходит: нет ни лидербордов, ни ачивок.

`carafe game run` применяет обходы для старого Wine 7.7 из GPTK: подставляет `api-ms-win-core-winrt-robuffer` для IL2CPP и убирает плагин Rewired для Windows.Gaming.Input в `.carafe-disabled/`.

Переменные окружения:

| Переменная      | Что делает                                  |
| --------------- | ------------------------------------------- |
| `CARAFE_BOTTLE` | имя префикса Wine (по умолчанию `default`)  |
| `CARAFE_EXE`    | exe игры, если автоопределение ошиблось     |
| `CARAFE_HUD=1`  | Metal HUD с FPS                             |
| `CARAFE_BRIDGE=0` | запуск без моста в Steam                  |
| `WINEDEBUG`     | логи Wine (по умолчанию выключены)          |
