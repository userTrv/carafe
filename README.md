# carafe

Минимальный запускатель Windows-игр на macOS (Apple Silicon) в духе Whisky, но без GUI и без второго Steam-клиента.

Внутри:

- **Wine + D3DMetal** — готовая сборка [Gcenx/game-porting-toolkit](https://github.com/Gcenx/game-porting-toolkit) (GPTK 3.0), DirectX 11/12 → Metal. Ничего не собирается, просто распаковывается.
- **steamcmd** от Valve — качает Windows-сборку игры с твоего аккаунта.
- Всё хранится в `~/Library/Application Support/carafe`.

Нужна Rosetta 2 (`softwareupdate --install-rosetta`).

## Две версии: bash и Rust

Логика переезжает на Rust: библиотека `crates/carafe-core` (на ней будет и GUI) и CLI `crates/carafe-cli` — бинарник `carafe` с теми же командами, переменными и сообщениями. `bin/carafe` на bash остаётся рабочим, пока Rust-версия не догонит его во всём; данные у них общие, их можно чередовать.

```bash
cargo build --release          # тулчейн — из rust-toolchain.toml
target/release/carafe game run <appid>
```

В примерах ниже — `bin/carafe`; у `target/release/carafe` команды те же, плюс `carafe game list`. soju Rust-версия ищет в `CARAFE_SOJU` или в каталоге `soju` рядом с любым из предков бинарника (`~/Projects/soju` для `~/Projects/carafe/target/release/carafe`).

Чем Rust-версия отличается:

- appinfo разбирается настоящим VDF-парсером и кешируется в `games/<appid>/.carafe-appinfo.vdf` (имя игры для списка); правила облака по-прежнему в `.carafe-ufs`.
- `game run` создаёт префикс до `cloud pull`, а неудачный pull (например, нет пользователя в префиксе) не прерывает запуск.
- wine с игрой запускается напрямую через `soju-bridge exec`, без повторного вызова `carafe run`.

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

## Библиотеки и загрузки (Rust-версия)

Игры могут лежать в нескольких папках, например на внешнем SSD: `<папка>/<appid>`. Встроенная — прежняя `games/`; без настроек всё как раньше. Список — в `libraryfolders.vdf` в каталоге carafe; он помнит игры каждой папки, так что игры с отключённого диска видны как недоступные.

```bash
carafe library add /Volumes/SSD/Games      # папку можно создать, но только на подключённом диске
carafe library default /Volumes/SSD/Games  # сюда ставятся новые игры
carafe library list                        # место на дисках
carafe library du                          # место по играм
carafe game install <appid> [логин] [--to /Volumes/SSD/Games]
carafe game move <appid> /Volumes/SSD/Games   # тот же том — rename, иначе копия, потом удаление
carafe downloads                           # очередь: pause|resume|cancel|top <appid>, run
```

Установка идёт через очередь (`downloads.vdf`): по одной загрузке, порядок меняется, прерванная докачивается после перезапуска. Очередь обрабатывает один процесс — CLI или GUI; управлять ей можно из любого.

## Мост в Steam для Mac

```bash
bin/carafe bridge build <appid>
bin/carafe bridge test <appid>     # сверка с нативным Steam на Spacewar (480)
```

После этого `game run` подменяет `steam_api64.dll` игры заглушкой, которая передаёт вызовы Steamworks нативному хелперу, а тот говорит с запущенным Steam для Mac от имени игры: ачивки, статистика, лидерборды. Steam для Mac должен быть запущен.

Сам мост — часть движка [soju](https://github.com/userTrv/soju) (`bin/soju-bridge`), там же описано, как он устроен и проверяется. carafe ищет checkout soju рядом с собой (`../soju`) или в `CARAFE_SOJU`. Лог вызовов: `~/Library/Application Support/carafe/bridge-<appid>.log`.

`CARAFE_BRIDGE=0` запускает игру с оригинальной DLL.

## Steam Cloud

```bash
bin/carafe cloud status <appid>   # что куда поедет
bin/carafe cloud pull <appid>
bin/carafe cloud push <appid>
```

`game run` сам делает `pull` перед запуском и `push` после выхода (`CARAFE_CLOUD=0` — не трогать облако). Steam для Mac Auto-Cloud с Windows-путями не обслуживает, поэтому carafe делает это сам через хелпер моста: правила `ufs` игры берёт из `app_info_print` steamcmd, а с облаком говорит через сервис `Cloud` Steam-клиента (`ISteamUnifiedMessages`) — тот же путь, что у самого клиента, без логина и пароля.

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
| `CARAFE_CLOUD=0` | `game run` не трогает Steam Cloud          |
| `CARAFE_ENGINE_DIR` | свой движок, например `dist/soju-engine` из soju |
| `CARAFE_SOJU`   | checkout soju (по умолчанию `../soju`)      |
| `CARAFE_HOME`   | где всё хранится                            |
| `WINEDEBUG`     | логи Wine (по умолчанию выключены)          |

## Лицензия

MIT — см. [LICENSE](LICENSE). Сторонние части (движок soju, Steamworks SDK, D3DMetal) и их лицензии — в [NOTICE.md](NOTICE.md).
