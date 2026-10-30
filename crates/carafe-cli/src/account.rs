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

/// Время в игре и достижения: сводка и открытые, свежие первыми.
pub fn stats(c: &Carafe, appid: u32, json: bool) -> Result<(), Error> {
    let st = c.steam_game_stats(appid, "russian")?;
    if json {
        let p = st.playtime.as_ref();
        let list: Vec<_> = st.achievements.iter().flat_map(|a| &a.achievements).map(|x| json!({
            "id": x.id, "name": x.name, "description": x.description, "icon": x.icon, "icon_gray": x.icon_gray,
            "hidden": x.hidden, "unlocked": x.unlocked, "unlocked_at": x.unlocked_at, "global_percent": x.global_percent,
        })).collect();
        emit(json!({
            "appid": appid,
            "minutes": p.map(|p| p.minutes),
            "minutes_2weeks": p.map(|p| p.minutes_2weeks),
            "last_played": p.and_then(|p| p.last_played),
            "achievements": st.achievements.as_ref().map(|a| json!({ "total": a.total, "unlocked": a.unlocked, "items": list })),
        }));
        return Ok(());
    }
    match &st.playtime {
        Some(p) => {
            let last = p.last_played.map(|t| format!(", последний запуск {t}")).unwrap_or_default();
            println!(
                "в игре: {:.1} ч (за 2 недели {:.1} ч){last}",
                p.minutes as f64 / 60.0,
                p.minutes_2weeks as f64 / 60.0
            )
        }
        None => println!("в игре: нет данных"),
    }
    let Some(a) = &st.achievements else {
        println!("достижений нет");
        return Ok(());
    };
    println!("достижения: {}/{}", a.unlocked, a.total);
    let mut open: Vec<_> = a.achievements.iter().filter(|x| x.unlocked).collect();
    open.sort_by_key(|x| std::cmp::Reverse(x.unlocked_at));
    for x in open {
        println!("  {}\t{}", x.unlocked_at.unwrap_or_default(), x.name);
    }
    Ok(())
}
