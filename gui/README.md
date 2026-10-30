# carafe GUI

Окно для carafe: библиотека с обложками, запуск игры с этапами и логом, Steam Cloud по файлам, проверка окружения и настройки. Tauri 2: интерфейс — HTML/CSS/JS без сборки (`ui/`), логика — Rust (`src-tauri/`).

## Запуск

Нужен Rust (проверено на 1.99.1). Node и Tauri CLI не нужны.

```bash
cd gui
CARAFE_GUI_BACKEND=mock cargo run                    # демо-данные, ничего не запускает
CARAFE_CHECKOUT=~/Projects/carafe cargo run          # через bin/carafe
cargo clippy --all-targets -- -D warnings && cargo fmt --check && cargo test
```

| Переменная | Что делает |
| --- | --- |
| `CARAFE_GUI_BACKEND` | `mock` — демо-данные; иначе `CliBackend`, если известен checkout |
| `CARAFE_CHECKOUT` | checkout carafe с `bin/carafe` (то же — поле «Checkout carafe» в настройках); без него carafe.app берёт свой `bin/carafe` |
| `CARAFE_MOCK=fresh` | демо первого запуска: нет движка, бутылки и Steam |
| `CARAFE_MOCK_STEAM=in` | демо: уже вошёл в Steam, в библиотеке — все купленные игры, у установленных — время в игре и достижения; у тех, что есть под macOS, метка `MAC`, Stardew Valley стоит нативно |
| `CARAFE_MOCK_QUEUE` | `empty` — очередь загрузок пуста (кнопки загрузок в панели нет); `finishing` — Elden Ring докачивается за секунды |
| `CARAFE_GUI_ROUTE` | открыть экран сразу: `game/<appid>` (`game/<appid>/<px>` — с прокруткой), `details/<appid>` — с открытыми подробностями, `play/<appid>`, `setup`, `settings`, `downloads`; `install/<appid>` и `move/<appid>` — с открытым меню библиотек, `move/<appid>/go` — сразу перенос; `login` — окно входа в Steam |
| `CARAFE_SOJU` | checkout soju для моста (то же — поле «Checkout soju»); по умолчанию рядом с checkout carafe |
| `CARAFE_HOME` | как у `bin/carafe` |

Настройки лежат в `~/Library/Application Support/dev.usertrv.carafe/settings.json`.

### carafe.app

```bash
steam/build.sh
cd gui/src-tauri && cargo tauri build
```

Нужен `tauri-cli` той же ветки, что `tauri` в `Cargo.lock` (`cargo install tauri-cli --version 2.12.4 --locked`). В бандл попадают `bin/carafe` (`Contents/Resources`) и хелпер входа `carafe-steam` с `SteamKit2.dll` (`Contents/MacOS`). Мост к Steam собирается из checkout soju, его путь — в поле «Checkout soju». Подпись ad-hoc: приложение для своего Mac, не для раздачи.

## Как устроено

Экраны знают только трейт `Backend` (`src-tauri/src/backend.rs`): список игр, окружение, статус облака, запуск, сборка моста, установка движка, бутылка, Steam. Методы блокирующие; GUI зовёт их из своих потоков. Долгие операции шлют `TaskEvent` (этап, строка лога, прогресс, файл облака), GUI пересылает их в окно событием `task`, итог — `task-end`.

- `MockBackend` (`mock.rs`) — правдоподобные игры, задержки и события.
- `CliBackend` (`cli.rs`) — зовёт `bin/carafe` и разбирает вывод: маркеры `==> Steam Cloud: pull|push`, `==> мост в Steam`, `==> запускаю`, строки облака `  = / ↓ / ↑ файл`, ошибки `carafe: …`. Состояние (игры, движки, бутылки, собран ли мост) читает с диска так же, как его раскладывает скрипт.

Библиотеки, перенос игр и очередь загрузок (`ui/library.js`, `ui/library.css`) `CliBackend` берёт прямо из `carafe-core`: у bash-версии их нет. Очередь обрабатывает поток GUI загрузчиком steamcmd (нужен сохранённый вход `carafe steam login USER`); при выходе загрузка останавливается и продолжается при следующем запуске. Папку выбирает системный диалог `tauri-plugin-dialog`, он зовётся из Rust — JS-доступа к плагину в capabilities нет.

Облик — по `DESIGN.md`. Шрифты Geist и Geist Mono лежат в `ui/fonts/` (woff2, latin и cyrillic), иконки Phosphor встроены в `ui/icons.js`: окно ничего не грузит из сети, кроме обложек Steam CDN (они разрешены в CSP).

Всё, что уходит в окно, проходит через `redact.rs`: Steam ID вырезается из путей и логов.

## Подключение carafe-core

1. `carafe-core` в `[dependencies]` по пути (`../crates/carafe-core`).
2. `CoreBackend: Backend` поверх API ядра; выбирать его по умолчанию, `CliBackend` оставить запасным.
3. Что нужно от ядра, чтобы убрать разбор текста:
   - список игр с именем из appinfo (сейчас имя — по exe), размером и временем последней синхронизации;
   - состояние моста по игре: нужен / собран / не собран;
   - статус облака структурой, без изменения файла состояния (сейчас `cloud status` его переписывает);
   - запуск с колбэком этапов (облако вниз, мост, игра, облако вверх) и строк лога, плюс возможность остановить игру;
   - прогресс скачивания движка в байтах;
   - проверки окружения (Rosetta, движок, D3DMetal, бутылка, soju, Steam) одной функцией.
