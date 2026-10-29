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

## Ограничения

Steam внутри префикса не запущен, поэтому `SteamAPI_Init()` у игры не проходит: нет облака и ачивок. Сейвы из Steam Cloud можно забрать вручную со страницы `https://store.steampowered.com/account/remotestorageapp/?appid=<appid>` и положить в папку игры в префиксе (у Unity-игр — `drive_c/users/crossover/AppData/LocalLow/...`). Обратно в облако прогресс сам не уходит.

`carafe game run` применяет обходы для старого Wine 7.7 из GPTK: подставляет `api-ms-win-core-winrt-robuffer` для IL2CPP и убирает плагин Rewired для Windows.Gaming.Input в `.carafe-disabled/`.

Переменные окружения:

| Переменная      | Что делает                                  |
| --------------- | ------------------------------------------- |
| `CARAFE_BOTTLE` | имя префикса Wine (по умолчанию `default`)  |
| `CARAFE_EXE`    | exe игры, если автоопределение ошиблось     |
| `CARAFE_HUD=1`  | Metal HUD с FPS                             |
| `WINEDEBUG`     | логи Wine (по умолчанию выключены)          |
