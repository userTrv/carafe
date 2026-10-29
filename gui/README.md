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
| `CARAFE_CHECKOUT` | checkout carafe с `bin/carafe` (то же — поле «Checkout carafe» в настройках) |
| `CARAFE_MOCK=fresh` | демо первого запуска: нет движка, бутылки и Steam |
| `CARAFE_MOCK_STEAM=in` | демо: уже вошёл в Steam, в библиотеке — все купленные игры |
| `CARAFE_GUI_ROUTE` | открыть экран сразу: `game/<appid>`, `play/<appid>`, `setup`, `settings`, `downloads`; `install/<appid>` и `move/<appid>` — с открытым меню библиотек, `move/<appid>/go` — сразу перенос; `login` — окно входа в Steam |
| `CARAFE_HOME`, `CARAFE_SOJU` | как у `bin/carafe` |

Настройки лежат в `~/Library/Application Support/dev.usertrv.carafe/settings.json`. Сборка `.app` — `cargo install tauri-cli` и `cargo tauri build`, в этом шаге не настроена.

## Как устроено

Экраны знают только трейт `Backend` (`src-tauri/src/backend.rs`): список игр, окружение, статус облака, запуск, сборка моста, установка движка, бутылка, Steam. Методы блокирующие; GUI зовёт их из своих потоков. Долгие операции шлют `TaskEvent` (этап, строка лога, прогресс, файл облака), GUI пересылает их в окно событием `task`, итог — `task-end`.

- `MockBackend` (`mock.rs`) — правдоподобные игры, задержки и события.
- `CliBackend` (`cli.rs`) — зовёт `bin/carafe` и разбирает вывод: маркеры `==> Steam Cloud: pull|push`, `==> мост в Steam`, `==> запускаю`, строки облака `  = / ↓ / ↑ файл`, ошибки `carafe: …`. Состояние (игры, движки, бутылки, собран ли мост) читает с диска так же, как его раскладывает скрипт.

Библиотеки, перенос игр и очередь загрузок (`ui/library.js`, `ui/library.css`) `CliBackend` берёт прямо из `carafe-core`: у bash-версии их нет. Очередь обрабатывает поток GUI загрузчиком steamcmd (нужен сохранённый вход `carafe steam login USER`); при выходе загрузка останавливается и продолжается при следующем запуске. Папку выбирает системный диалог `tauri-plugin-dialog`, он зовётся из Rust — JS-доступа к плагину в capabilities нет.

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
