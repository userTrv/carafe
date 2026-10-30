# carafe-steam: протокол

`carafe-steam` — хелпер carafe на [SteamKit2](https://github.com/SteamRE/SteamKit) (LGPL-2.1): вход в Steam по QR, список купленных игр, депо и загрузка Windows-сборки или нативной macOS-сборки. Ядро (`crates/carafe-core/src/steamclient.rs`) запускает его как дочерний процесс и говорит с ним JSON-строками.

> **Риск.** Это сторонний клиент Steam. Valve такие клиенты (SteamKit2, DepotDownloader, steamcmd-обёртки) терпит, но официально не поддерживает: протокол может поменяться, а аккаунт формально подпадает под правила подписчика Steam.

## Секреты

- **Пароль Steam хелпер не принимает и не спрашивает.** Вход — только по QR: Steam Guard подтверждается в мобильном приложении Steam.
- Refresh token уходит в stdout ровно один раз — в ответе на `login_qr`. Обратно ядро присылает его только через stdin (`login_token`, `logout`). В аргументах командной строки, в stderr и в файлах токена нет; хелпер ничего не пишет на диск, кроме файлов игры при `download`.
- URL QR-кода, Steam ID и имя аккаунта в лог не пишутся. Steam ID наружу — только маской `••••1234`.
- Ядро хранит токен в Связке ключей macOS (сервис `dev.usertrv.carafe.steam`).

## Транспорт

- Аргументов нет: `carafe-steam` с аргументами завершается с кодом 2.
- stdin: запросы, по одному JSON-объекту на строку (UTF-8).
- stdout: события и ответы, по одному JSON-объекту на строку.
- stderr: человеческий лог `carafe-steam: …` без секретов. Ядро пересылает его в свой лог.
- Закрытый stdin — отмена текущего запроса и выход с кодом 0.
- Запросы выполняются по очереди, сессия Steam живёт между ними: можно `login_token`, потом `owned_apps`, потом `download`.

При старте хелпер пишет:

```json
{"event":"ready","protocol":1,"version":"1.0.0","features":["macos"]}
```

`features` — что хелпер умеет сверх протокола 1; новые поля и команды добавляются так, что старое ядро их просто не замечает. Хелпер без `features` (или без нужного в нём) ядро считает старым и не шлёт ему запросов, которых тот не поймёт: иначе, например, `download` с `platform` молча скачал бы Windows-сборку.

| feature | что даёт |
|---|---|
| `macos` | поле `platform` в `app_info` и `download`, нативность в `owned_apps`, `launch` в ответах |

### Запрос

```json
{"id":1,"cmd":"owned_apps"}
```

`id` — число, выбирает ядро; все события и ответ на запрос несут тот же `id`.

### События

```json
{"id":1,"event":"qr","url":"https://s.team/q/…"}
```

Ноль или больше до ответа.

### Ответ

Ровно один на запрос:

```json
{"id":1,"ok":true,"result":{…}}
{"id":1,"ok":false,"error":{"code":"not_logged_in","message":"сначала login_qr или login_token"}}
```

Коды ошибок: `bad_request`, `not_logged_in`, `token_invalid` (токен отозван или истёк — нужен новый QR-вход), `auth_failed`, `timeout`, `cancelled`, `network`, `not_found`, `not_owned`, `no_depots`, `access_denied`, `bad_manifest`, `failed`. Ошибка разбора строки без `id` приходит с `"id":null`.

### Отмена

```json
{"id":7,"cmd":"cancel","target":3}
```

Отменяет запрос `3`, если он сейчас выполняется (без `target` — любой текущий). Ответа на сам `cancel` нет; отменённый запрос отвечает ошибкой `cancelled`.

## Команды

### `login_qr`

```json
{"id":1,"cmd":"login_qr","device_name":"carafe (Mac)"}
```

Новая сессия (прежняя закрывается). `device_name` необязателен — это имя видно в приложении Steam при подтверждении.

События:

- `{"event":"qr","url":"https://s.team/q/…"}` — нарисовать QR из `url`. Steam время от времени меняет challenge: пришло новое событие — перерисовать.
- `{"event":"confirmed"}` — вход подтверждён в приложении, хелпер входит в Steam.

Ответ:

```json
{"connected":true,"logged_in":true,"anonymous":false,"account_name":"имя","steam_id":"••••1234","refresh_token":"…"}
```

Не подтвердили за 5 минут — `timeout`; отказ в приложении — `auth_failed`.

### `login_token`

```json
{"id":2,"cmd":"login_token","account_name":"имя","refresh_token":"…"}
```

Вход по сохранённому токену. Ответ — как у `status`. Токен больше не действует — `token_invalid`.

### `status`

Без сети: `{"connected":…,"logged_in":…,"anonymous":…,"account_name":…,"steam_id":"••••1234"}`.

### `licenses`

Нужен вход. `{"packages":[{"packageid":…,"license_type":"SinglePurchase","time_created":1700000000}]}` — лицензии без истёкших.

### `owned_apps`

Нужен вход. Все игры (`type` = `game`) из лицензий аккаунта, через PICS:

```json
{"apps":[{"appid":413150,"name":"Stardew Valley","type":"game","oslist":"windows,macos,linux","windows":true,"size_bytes":…,
          "macos":true,"macos_size_bytes":…,"macos_arch":null}]}
```

`windows` — есть ли что ставить под Windows (см. выбор депо ниже), `size_bytes` — сумма размеров этих депо из appinfo (примерно столько займёт игра; `null`, если Windows-депо нет).

`macos` (feature `macos`) — есть ли нативная сборка для macOS: в `common/oslist` есть `macos` **и** среди депо, выбранных под macOS, есть депо с `macos` в своём `oslist`, которое входит в лицензии аккаунта (`depotids` пакетов PICS; если пакеты депо не перечисляют — проверка пропускается). Одних общих депо (`oslist` пустой) мало — нужен ещё вариант запуска под macOS в `config/launch`, и тогда у игры не должно быть ни одного депо с `macos` (такая игра кладёт `.app` в общее депо, как DDLC); если macOS-депо есть, но не ставится или не куплено, нативной сборки нет. `macos_size_bytes` — размер выбранных под macOS депо. `macos_arch` — `arm64`, если `osarch` macOS-депо или варианта запуска это прямо говорит; обычно appinfo архитектуру не называет — тогда `null` (ядро узнаёт её после установки по заголовку Mach-O).

### `app_info`

```json
{"id":4,"cmd":"app_info","appid":1245620,"platform":"macos"}
```

`platform` (feature `macos`) — `windows` (по умолчанию) или `macos`: под какую платформу выбирать депо (`install`, `skip` «не macOS»).

Без входа хелпер входит анонимно (публичные данные приложения видны и так). Ответ:

```json
{
  "appid": 1245620, "name": "ELDEN RING", "size_bytes": 56188103775,
  "depots": [
    {"depotid":1245621,"name":null,"oslist":"","arch":null,"language":null,"size_bytes":…,
     "manifests":{"public":"…"},"dlc_appid":null,"install":true,"skip":null},
    {"depotid":228988,"oslist":"windows","install":false,"skip":"общий редистрибутив", …}
  ],
  "branches": [{"name":"public","buildid":…,"time_updated":…,"password_required":false,"description":null}],
  "platform": "windows", "macos": true,
  "launch": [{"executable":"Game.app","arguments":null,"workingdir":null,"type":"default","description":null,
              "oslist":"macos","osarch":null,"betakey":null}]
}
```

`launch` — все варианты запуска из `config/launch`, `macos` — то же, что в `owned_apps`.

Выбор депо для установки (`install: true`), как у DepotDownloader: `oslist` пустой или содержит платформу (`windows` или `macos`); без `depotfromapp`/`sharedinstall` (общие редистрибутивы); DLC — только купленные; язык пустой или `english`; без low violence; есть манифест ветки; если есть 64-битные депо, 32-битные пропускаются. Почему депо не ставится — в `skip`.

### `download`

```json
{"id":5,"cmd":"download","appid":1245620,"dir":"/абсолютный/путь","branch":"public","depots":[1245621],"platform":"macos"}
```

`platform` (feature `macos`) — какую сборку качать, по умолчанию `windows`. Для `macos` без `depots` нужна настоящая macOS-сборка (как `macos` в `owned_apps`), иначе `no_depots`.

Качает депо приложения в `dir` (создаётся). `branch` — по умолчанию `public`, ветки с паролем не поддерживаются. `depots` необязателен: без него — депо с `install: true`. Без входа хелпер входит анонимно — так качаются бесплатные депо Valve (например, Steamworks SDK Redist, appid 1007); с входом проверяется, что приложение есть в лицензиях (`not_owned`).

События:

- `{"event":"depot","depotid":…,"manifest":"…","files":123,"total":456}` — по одному на депо, когда скачан его манифест (до первого чанка).
- `{"event":"progress","downloaded":…,"total":…}` — байты, не чаще 4 раз в секунду; первое — с `downloaded: 0`, последнее — с `downloaded == total`.

Ответ: `{"appid":…,"dir":"…","downloaded":…,"files":…,"unchanged":…,"depots":[{"depotid":…,"manifest":"…"}],"platform":"macos","launch":[…]}`. `launch` — варианты запуска под платформу и ветку (`oslist` пустой или с платформой, без чужих `betakey`, тип `default`/`none`/`option1`); ядро сохраняет их в папке игры (`.carafe-native`).

Флаг `executable` из манифеста ставится (`chmod +x`), симлинки создаются после файлов — только относительные и только внутрь `dir` (во фреймворках `.app` их много); остальные пропускаются с записью в лог.

Как качает: манифесты и ключи депо — через сессию, чанки — с CDN Steam (`GetServersForSteamPipe`), по 8 параллельно, с повтором на другом сервере. Файл с тем же размером и SHA-1, что в манифесте, не качается заново (`unchanged`) — так докачивается прерванная загрузка. Пути из манифеста не выходят за `dir` (`bad_manifest`). Отмена — `cancel` или закрытый stdin.

Чего пока нет (TODO):

- удаления файлов, которых нет в новом манифесте (обновление игры поверх старой версии оставит лишние файлы);
- докачки внутри файла: недокачанный файл качается целиком заново;
- симлинков наружу из папки игры (пропускаются с записью в лог);
- общих редистрибутивов (`depotfromapp`, например DirectX/VC++ 228980) и веток с паролем;
- проверки свободного места до начала.

### `playtime`

Нужен вход. Сколько аккаунт играл — через `Player.GetOwnedGames` сессии (ключ Web API не нужен). Только сыгранные игры (есть минуты или последний запуск):

```json
{"games":[{"appid":3405340,"minutes":1289,"minutes_2weeks":355,"last_played":1761800000}]}
```

`minutes` — всего, `minutes_2weeks` — за две недели, `last_played` — Unix-время последнего запуска (`null`, если Steam его не знает).

### `achievements`

```json
{"id":8,"cmd":"achievements","appid":3405340,"language":"russian"}
```

Нужен вход. Схема достижений (`Player.GetGameAchievements`, на языке `language`, по умолчанию `english`) и отметки аккаунта (`Player.GetUserAchievements`), склеенные по ключу. Архивные достижения не входят. Ответ:

```json
{
  "appid": 3405340, "total": 139, "unlocked": 70,
  "achievements": [
    {"id":"ACH_ANVIL","name":"Anvil","description":"Complete 3 Challenges",
     "icon":"https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps/3405340/….jpg",
     "icon_gray":"https://…","hidden":false,"unlocked":true,"unlocked_at":1761700000,"global_percent":41.2}
  ]
}
```

`icon` — иконка открытого, `icon_gray` — закрытого (полные URL на CDN Steam, `null`, если иконки нет). `unlocked_at` — Unix-время открытия, `global_percent` — доля игроков, открывших достижение. У скрытого закрытого достижения `description` может быть `null`. Нет достижений — `total: 0`.

Обе команды добавлены без смены `protocol`: ядро, получив `bad_request` от старого хелпера, просто не показывает время и достижения.

### `logout`

```json
{"id":6,"cmd":"logout","account_name":"имя","refresh_token":"…"}
```

Отзывает refresh token на стороне Steam (`IAuthentication.RevokeToken`) и закрывает сессию. Поля необязательны, если сессия уже вошла через `login_token` или `login_qr`. Ответ: `{"revoked":true}`; токен уже недействителен — `{"revoked":false}`. Ядро удаляет токен из Связки ключей в любом случае.

## Сборка

```bash
steam/build.sh                    # dist/osx-arm64/carafe-steam
steam/build.sh osx-arm64 osx-x64  # оба
```

Нужен .NET SDK из `global.json`. Single-file, self-contained (рантайм .NET внутри, ~80 МБ), без тримминга: protobuf-net в SteamKit2 работает через рефлексию. Пакеты — только с nuget.org (`nuget.config`), строго по `packages.lock.json` (`--locked-mode`, contentHash).

SteamKit2 распространяется под LGPL-2.1, поэтому в бандл она не входит: `dist/<rid>/SteamKit2.dll` лежит рядом с `carafe-steam`, и её можно заменить своей сборкой SteamKit2 (3.x той же или более новой версии) без пересборки хелпера. Переносить хелпер — вместе с этим файлом. Лицензии сторонних частей — `NOTICE.md`.

Ядро ищет хелпер в `CARAFE_STEAM_HELPER`, затем в `steam/dist/<rid>/carafe-steam` рядом с любым из предков бинарника carafe, затем рядом с самим бинарником.
