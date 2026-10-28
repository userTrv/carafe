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

После этого `game run` подменяет `steam_api64.dll` игры заглушкой, которая передаёт вызовы Steamworks нативному `carafe-steamd`, а тот говорит с запущенным Steam для Mac от имени игры. Steam для Mac должен быть запущен.

Работают оба способа, которыми игры зовут Steamworks:

- **flat API** — Unity со Steamworks.NET и подобные обёртки. Проверено на Megabonk (лидерборды с никами и аватарами) и тестом на Spacewar.
- **C++-интерфейсы** — Unreal и свои движки: `SteamUser()` и прочие аксессоры, методы через vtable, `CCallback`/`CCallResult` с `SteamAPI_RunCallbacks`. Проверено тестом на Spacewar, на реальной C++-игре пока нет.

Как устроено:

- SDK игры carafe узнаёт по версиям интерфейсов, зашитым в её `steam_api64.dll` (`bridge/sdk_detect.py`). Под каждый SDK в `bridge/sdk/<версия>/` лежит его `steam_api.json` из Steamworks.NET, а в `steamapi_mac` в `bin/carafe` — откуда брать `libsteam_api.dylib` того же SDK. Сейчас есть 1.60. Для другого SDK `bridge build` остановится и перечислит версии интерфейсов игры.
- `bridge/gen.py` генерирует обе стороны из `steam_api.json` и самой DLL игры. Колбэки и структуры перекладываются между pack 8 (Windows) и pack 4 (Mac).
- Слоты vtable берутся из кода flat-обёрток той же DLL: `mov rax,[rcx]; jmp [rax+N]`. В json нет приватных методов SDK, а место в vtable они занимают, поэтому json один не годится. Отсюда же видно, где MSVC переставляет перегрузки и где возвращает `CSteamID` через скрытый указатель.
- Интерфейс отдаётся игре объектом с vtable, и flat-функции принимают его наравне с сырым указателем. Версию интерфейса не из SDK игры заглушка не выдаёт: вызов по чужой vtable уронил бы игру.
- Связь — TCP на 127.0.0.1 с одноразовым токеном. Хелпер живёт, пока игра подключена. Лог вызовов: `~/Library/Application Support/carafe/bridge-<appid>.log`.
- Для сборки нужен zig — carafe скачает его сам в `~/Library/Application Support/carafe/toolchain`. Пересборка подменяет файлы переименованием, запущенная игра её переживает.

`CARAFE_BRIDGE=0` запускает игру с оригинальной DLL.

### Проверка моста

```bash
bin/carafe bridge test <appid>
```

`bridge/test/steamtest.c` собирается дважды: под Mac (с настоящим `libsteam_api.dylib`) и под Windows (под Wine грузит заглушку моста). Обе сборки работают от имени Spacewar (appid 480, тестовое приложение Valve), и их вывод должен совпасть строка в строку.

Flat API:

- ник, аватар, страна;
- ачивка: выдать, проверить на сервере, вернуть как было;
- стат: +1 к `NumGames`, проверить на сервере;
- лидерборд: найти, отправить результат 1 с `KeepBest`, скачать свою запись с деталями;
- Steam Cloud: записать, прочитать и удалить файл.

Call result проверяются двумя путями: через ManualDispatch, как в Steamworks.NET, и через `ISteamUtils::GetAPICallResult`, как в C++-играх.

C++ (`bridge/test/cpptest.cpp`) компилируется настоящими компиляторами в двух ABI: clang++ под Mac (Itanium, против настоящего Steam) и clang под `x86_64-pc-windows-msvc` (как MSVC у игр). Проверяются:

- инлайновый аксессор через `SteamInternal_ContextInit`;
- `GetSteamID()` через скрытый указатель;
- интерфейсы через методы `ISteamClient`;
- обе перегрузки `GetStat`;
- `CCallback` на `UserStatsReceived_t` и `CCallResult` на `FindLeaderboard`.

Spacewar после теста остаётся в истории игр, а в его лидерборде — твоя запись с результатом 1.

При каждой сборке `build.sh` ещё сверяет с компиляторами то, что вычислил генератор:

- раскладки всех 239 структур (`bridge/test/layout_check.py`): sizeof и offsetof у zig под Windows x64 и у clang под Mac arm64;
- слоты vtable (`bridge/test/gen_header.py` + `vt_check.py`): по слотам из DLL восстанавливается C++-заголовок, clang под MSVC раскладывает его, и раскладка должна совпасть со слотами DLL.

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
