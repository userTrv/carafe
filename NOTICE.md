# Сторонние части carafe

Код carafe — MIT, см. `LICENSE`. carafe ничего не распространяет из Steam, Apple или CodeWeavers
и не связан с ними. Steam — товарный знак Valve Corporation.

| Что | Лицензия | Как попадает к пользователю |
|---|---|---|
| Движок по умолчанию [soju](https://github.com/userTrv/soju) (Wine из исходников CrossOver) | LGPL 2.1+ | `carafe engine install` скачивает релиз `engine-v*` с GitHub soju, без D3DMetal; лицензии и ссылки на исходники — в `share/doc/soju/` движка |
| Запасной движок и источник D3DMetal: сборка Wine от Gcenx с D3DMetal из GPTK 3.0 | LGPL 2.1+ (Wine); D3DMetal — лицензия Apple | `carafe engine install gptk` скачивает её с GitHub Gcenx; carafe её не распространяет |
| Мост Steamworks из soju | LGPL 2.1+ | собирается на машине пользователя |
| Steamworks SDK (`libsteam_api.dylib`) | собственность Valve | скачивается при сборке моста, carafe его не распространяет |
| D3DMetal из Game Porting Toolkit | лицензия Apple | `bin/soju-d3dmetal` копирует его в движок soju из копии GPTK у пользователя (движок GPTK от carafe, Game Porting Toolkit.app, образ GPTK, CrossOver); carafe его не распространяет |
| steamcmd | собственность Valve | скачивается с серверов Valve |
| Rust-зависимости (Tauri и др.) | MIT / Apache 2.0 | полный список — `Cargo.lock`, `gui/Cargo.lock` |
| [SteamKit2](https://github.com/SteamRE/SteamKit) 3.5.0 (SteamRE) | LGPL 2.1 | входит в хелпер `carafe-steam` (`steam/`) отдельным файлом `SteamKit2.dll` рядом с бинарником — его можно заменить своей сборкой SteamKit2 той же или более новой версии 3.x; хелпер пересобирается из исходников `steam/build.sh` |
| Шрифты [Geist и Geist Mono](https://github.com/vercel/geist-font) 1.x (The Geist Project Authors), npm `@fontsource/geist` и `@fontsource/geist-mono` 5.3.0 | SIL OFL 1.1 | woff2 (latin, cyrillic) лежат в GUI, `gui/ui/fonts/`; текст лицензии рядом: `OFL-Geist.txt`, `OFL-GeistMono.txt` |
| Иконки [Phosphor](https://phosphoricons.com) 2.1.1, npm `@phosphor-icons/core` | MIT | нужные SVG встроены в `gui/ui/icons.js`; лицензия — `gui/ui/LICENSE-phosphor.txt` |
| .NET runtime 10 (Microsoft) | MIT | входит в самодостаточный single-file `carafe-steam` |
| protobuf-net, ZstdSharp.Port, System.IO.Hashing (зависимости SteamKit2) | Apache 2.0 / MIT / MIT | внутри `carafe-steam`; версии и хеши — `steam/packages.lock.json` |

`carafe-steam` — сторонний клиент Steam: Valve такие клиенты терпит, но официально не поддерживает.
