# Сторонние части carafe

Код carafe — MIT, см. `LICENSE`. carafe ничего не распространяет из Steam, Apple или CodeWeavers
и не связан с ними. Steam — товарный знак Valve Corporation.

| Что | Лицензия | Как попадает к пользователю |
|---|---|---|
| Движок [soju](https://github.com/userTrv/soju) (Wine из исходников CrossOver) | LGPL 2.1+ | carafe скачивает релиз soju; лицензии и ссылки на исходники — в `share/doc/soju/` движка |
| Мост Steamworks из soju | LGPL 2.1+ | собирается на машине пользователя |
| Steamworks SDK (`libsteam_api.dylib`) | собственность Valve | скачивается при сборке моста, carafe его не распространяет |
| D3DMetal из Game Porting Toolkit | лицензия Apple | берётся из копии GPTK у пользователя, carafe его не распространяет |
| steamcmd | собственность Valve | скачивается с серверов Valve |
| Rust-зависимости (Tauri и др.) | MIT / Apache 2.0 | полный список — `Cargo.lock`, `gui/Cargo.lock` |
