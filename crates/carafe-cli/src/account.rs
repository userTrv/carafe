//! `carafe login|logout|account` и `carafe game owned`: аккаунт Steam через `carafe-steam`.
//! Установка с аккаунта — через очередь (`carafe game install APPID` без USER).
//!
//! С `--json` вывод — JSON-строки в stdout для GUI (`gui/src-tauri/src/cli.rs`); ошибки —
//! как везде, `carafe: …` в stderr и код 1.

use std::io::Write;

use carafe_core::{Carafe, Error, LoginEvent, human_bytes};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;
use serde_json::json;

fn emit(v: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// QR в терминал полублоками: светлые поля вокруг, чтобы камера нашла код и на тёмной теме.
fn print_qr(url: &str) {
    match QrCode::new(url.as_bytes()) {
        Ok(code) => {
            let img = code.render::<Dense1x2>().dark_color(Dense1x2::Light).light_color(Dense1x2::Dark).build();
            println!("\n{img}\n");
        }
        Err(_) => println!("\n{url}\n"),
    }
}

pub fn login(c: &Carafe, json: bool) -> Result<(), Error> {
    let mut first = true;
    let acc = c.steam_login_qr(
        &mut |e| match (&e, json) {
            (LoginEvent::Qr { url }, true) => emit(json!({ "event": "qr", "url": url })),
            (LoginEvent::Confirmed, true) => emit(json!({ "event": "confirmed" })),
            (LoginEvent::Qr { url }, false) => {
                if first {
                    println!("Открой Steam на телефоне → Steam Guard → значок QR и наведи камеру:");
                } else {
                    println!("Steam обновил код:");
                }
                first = false;
                print_qr(url);
            }
            (LoginEvent::Confirmed, false) => println!("Подтверждено, вхожу…"),
        },
        None,
    )?;
    let id = acc.steam_id_masked.as_deref().unwrap_or("?");
    if json {
        emit(json!({ "event": "done", "account_name": acc.account_name, "steam_id": acc.steam_id_masked }));
    } else {
        println!("Вошёл: {} (Steam ID {id})", acc.account_name);
    }
    Ok(())
}

pub fn logout(c: &Carafe) -> Result<(), Error> {
    match c.steam_account()? {
        None => println!("и так не вошёл"),
        Some(_) => {
            if c.steam_logout()? {
                println!("Вышел, вход отозван в Steam");
            } else {
                println!(
                    "Вышел. Отозвать вход в Steam не удалось — устройство «carafe (Mac)» можно убрать в Steam: Настройки → Безопасность"
                );
            }
        }
    }
    Ok(())
}

pub fn status(c: &Carafe, json: bool) -> Result<(), Error> {
    let name = c.steam_account()?;
    if json {
        emit(json!({ "account_name": name }));
    } else {
        match name {
            Some(n) => println!("{n}"),
            None => println!("не вошёл: carafe login"),
        }
    }
    Ok(())
}

pub fn owned(c: &Carafe, json: bool) -> Result<(), Error> {
    let games = c.owned_games()?;
    if json {
        let list: Vec<_> = games
            .iter()
            .map(|g| {
                json!({
                    "appid": g.app.appid,
                    "name": g.app.name,
                    "installed": g.installed,
                    "windows": g.app.windows,
                    "size_bytes": g.app.size_bytes,
                })
            })
            .collect();
        emit(json!({ "games": list }));
        return Ok(());
    }
    for g in &games {
        let mark = if g.installed { "установлена" } else { "—" };
        let win = if g.app.windows { "" } else { "\tбез Windows-сборки" };
        let size = g.app.size_bytes.map(human_bytes).unwrap_or_default();
        println!("{}\t{}\t{mark}\t{size}{win}", g.app.appid, g.app.name);
    }
    Ok(())
}
