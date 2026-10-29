//! `carafe login|logout|account|library` и `game install APPID` через аккаунт Steam.
//!
//! С `--json` вывод — JSON-строки в stdout для GUI (`gui/src-tauri/src/cli.rs`); ошибки —
//! как везде, `carafe: …` в stderr и код 1.

use std::io::{IsTerminal, Write};

use carafe_core::{Carafe, DownloadEvent, Error, LoginEvent};
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

pub fn library(c: &Carafe, json: bool) -> Result<(), Error> {
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
        let size = g.app.size_bytes.map(size).unwrap_or_default();
        println!("{}\t{}\t{mark}\t{size}{win}", g.app.appid, g.app.name);
    }
    Ok(())
}

pub fn install(c: &Carafe, appid: u32, json: bool) -> Result<(), Error> {
    let tty = std::io::stderr().is_terminal();
    let mut last = 0;
    let r = c.steam_download(
        appid,
        None,
        &mut |e| match (e, json) {
            (DownloadEvent::Depot { depotid, files, total, .. }, true) => {
                emit(json!({ "event": "depot", "depotid": depotid, "files": files, "total": total }))
            }
            (DownloadEvent::Progress { downloaded, total }, true) => {
                emit(json!({ "event": "progress", "downloaded": downloaded, "total": total }))
            }
            (DownloadEvent::Depot { depotid, files, total, .. }, false) => {
                eprintln!("==> депо {depotid}: файлов {files}, {}", size(total))
            }
            (DownloadEvent::Progress { downloaded, total }, false) => {
                let pct = (downloaded * 100).checked_div(total).unwrap_or(100);
                if tty {
                    eprint!("\r  {pct:>3}%  {} из {}   ", size(downloaded), size(total));
                } else if pct >= last + 10 || downloaded == total {
                    // Без терминала — не чаще чем раз в 10 %.
                    last = pct;
                    eprintln!("  {pct}%");
                }
            }
        },
        None,
    )?;
    if tty && !json {
        eprintln!();
    }
    if json {
        emit(json!({ "event": "done", "downloaded": r.downloaded, "files": r.files, "unchanged": r.unchanged }));
    } else {
        eprintln!("==> готово: файлов {}, без изменений {}", r.files, r.unchanged);
    }
    Ok(())
}

fn size(b: u64) -> String {
    match b {
        b if b >= 1_000_000_000 => format!("{:.1} ГБ", b as f64 / 1e9),
        b => format!("{} МБ", b.div_ceil(1_000_000)),
    }
}
