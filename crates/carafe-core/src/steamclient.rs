//! Клиент хелпера `carafe-steam` (`steam/`, SteamKit2): вход в Steam по QR, купленные игры,
//! депо и загрузка. Протокол — JSON-строки по stdin/stdout, описан в `steam/PROTOCOL.md`.
//!
//! Refresh token ходит только через stdin/stdout хелпера: не в аргументах, не в логе. Пароль
//! Steam не нужен вовсе — вход подтверждается в мобильном приложении Steam.
//!
//! Это сторонний клиент Steam: Valve такие терпит, но официально не поддерживает.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Logger;
use crate::error::{Error, Result};
use crate::process::Log;

pub const PROTOCOL: u64 = 1;
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Короткие запросы (вход по токену, appinfo, список игр). QR и загрузка ждут сколько нужно.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Refresh token Steam. В `Debug` не печатается; наружу — только [`RefreshToken::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct RefreshToken(String);

impl RefreshToken {
    pub fn new(token: String) -> RefreshToken {
        RefreshToken(token)
    }

    /// Сам токен — для хранилища и для stdin хелпера. Не логировать.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RefreshToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RefreshToken(•••)")
    }
}

/// Что лежит в Связке ключей: имя аккаунта и refresh token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub account_name: String,
    pub refresh_token: RefreshToken,
}

/// Аккаунт после входа. Steam ID — только маской (`••••1234`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Account {
    pub account_name: String,
    #[serde(rename = "steam_id")]
    pub steam_id_masked: Option<String>,
}

/// События QR-входа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEvent {
    /// Нарисовать QR из `url`. Steam время от времени меняет challenge — тогда приходит новый.
    Qr { url: String },
    /// Вход подтверждён в приложении, хелпер входит в Steam.
    Confirmed,
}

impl std::fmt::Display for LoginEvent {
    /// Без URL: он одноразовый, но в лог ему всё равно незачем.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginEvent::Qr { .. } => f.write_str("QR готов"),
            LoginEvent::Confirmed => f.write_str("вход подтверждён"),
        }
    }
}

/// Игра из лицензий аккаунта.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OwnedApp {
    pub appid: u32,
    pub name: String,
    /// `oslist` из appinfo: `windows,macos,linux`.
    #[serde(default)]
    pub oslist: String,
    /// Есть ли Windows-депо для установки.
    pub windows: bool,
    /// Сумма размеров этих депо из appinfo: примерно столько займёт игра.
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppDepots {
    pub appid: u32,
    pub name: String,
    pub size_bytes: u64,
    pub depots: Vec<Depot>,
    pub branches: Vec<Branch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Depot {
    pub depotid: u32,
    pub name: Option<String>,
    pub oslist: String,
    pub arch: Option<String>,
    pub language: Option<String>,
    pub size_bytes: u64,
    /// Ветка → id манифеста (строкой: u64 не влезает в JSON-число без потерь).
    pub manifests: std::collections::BTreeMap<String, String>,
    pub dlc_appid: Option<u32>,
    /// Ставится ли при установке под Windows; почему нет — в `skip`.
    pub install: bool,
    pub skip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Branch {
    pub name: String,
    pub buildid: u32,
    pub time_updated: i64,
    pub password_required: bool,
    pub description: Option<String>,
}

/// События загрузки от хелпера (в очередь их переводит [`crate::SteamKitDownloader`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepotEvent {
    /// Манифест депо получен: сколько в нём файлов и байт.
    Depot { depotid: u32, manifest: String, files: u64, total: u64 },
    /// Байты всего задания (все депо).
    Progress { downloaded: u64, total: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DownloadResult {
    pub downloaded: u64,
    pub files: u64,
    /// Файлы, которые уже были на месте (тот же размер и SHA-1).
    pub unchanged: u64,
    pub depots: Vec<DepotManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DepotManifest {
    pub depotid: u32,
    pub manifest: String,
}

enum Line {
    Out(String),
    Closed,
}

/// Запущенный `carafe-steam`. Сессия Steam живёт, пока жив процесс: можно войти по токену
/// и сделать несколько запросов. Drop закрывает stdin, хелпер выходит сам.
pub struct SteamHelper {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<Line>,
    next_id: u64,
}

impl std::fmt::Debug for SteamHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteamHelper").field("pid", &self.child.id()).finish_non_exhaustive()
    }
}

impl SteamHelper {
    /// Запустить хелпер и дождаться `ready`. stderr хелпера идёт в `log` построчно.
    pub fn spawn(program: &Path, log: Logger) -> Result<SteamHelper> {
        let mut cmd = Command::new(program);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(crate::process::spawn_err(&cmd))?;
        let (tx, rx) = mpsc::channel();
        if let Some(out) = child.stdout.take() {
            thread::spawn(move || {
                for line in BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(Line::Out(line)).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Line::Closed);
            });
        }
        if let Some(err) = child.stderr.take() {
            thread::spawn(move || pump_log(err, log));
        }
        let mut h = SteamHelper { stdin: child.stdin.take(), child, rx, next_id: 1 };
        let ready = h.next_message(Instant::now() + READY_TIMEOUT)?;
        if ready["event"] != "ready" {
            return Err(Error::SteamProtocol("хелпер не прислал ready".into()));
        }
        if ready["protocol"].as_u64() != Some(PROTOCOL) {
            return Err(Error::SteamProtocol(format!(
                "протокол {} вместо {PROTOCOL} — пересобери steam/build.sh",
                ready["protocol"]
            )));
        }
        Ok(h)
    }

    fn next_message(&mut self, deadline: Instant) -> Result<Value> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left.min(Duration::from_millis(100))) {
                Ok(Line::Out(line)) => match serde_json::from_str::<Value>(&line) {
                    Ok(v @ Value::Object(_)) => return Ok(v),
                    // Не JSON в stdout — это не наш хелпер; строку не печатаем: мало ли что в ней.
                    _ => return Err(Error::SteamProtocol("в stdout не JSON".into())),
                },
                Ok(Line::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait().ok().map(|s| s.to_string()).unwrap_or_default();
                    return Err(Error::SteamProtocol(format!("хелпер завершился {status}").trim_end().into()));
                }
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                    return Err(Error::Steam {
                        code: "timeout".into(), message: "хелпер не ответил вовремя".into()
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| Error::SteamProtocol("stdin закрыт".into()))?;
        let mut line = msg.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| Error::SteamProtocol(format!("не смог написать хелперу: {e}")))
    }

    /// Запрос: события — в `on_event(имя, сообщение)`, итог — `result` ответа.
    /// `cancel()` вернул true — хелперу уходит `cancel`, ответ придёт ошибкой `cancelled`
    /// ([`Error::Interrupted`]).
    pub fn request(
        &mut self,
        cmd: &str,
        mut args: Value,
        on_event: &mut dyn FnMut(&str, &Value),
        cancel: Option<&dyn Fn() -> bool>,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        if !args.is_object() {
            args = json!({});
        }
        args["id"] = id.into();
        args["cmd"] = cmd.into();
        self.send(&args)?;
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut cancel_sent = false;
        loop {
            if !cancel_sent && cancel.is_some_and(|c| c()) {
                cancel_sent = true;
                let cid = self.next_id;
                self.next_id += 1;
                self.send(&json!({ "id": cid, "cmd": "cancel", "target": id }))?;
            }
            // Ждём кусками, чтобы видеть отмену.
            let step = Instant::now() + Duration::from_millis(200);
            let msg = match self.next_message(deadline.map_or(step, |d| d.min(step))) {
                Err(Error::Steam { code, .. }) if code == "timeout" => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        return Err(Error::Steam {
                            code, message: format!("{cmd}: хелпер не ответил вовремя")
                        });
                    }
                    continue;
                }
                r => r?,
            };
            if msg["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(ev) = msg["event"].as_str() {
                on_event(ev, &msg);
                continue;
            }
            return match msg["ok"].as_bool() {
                Some(true) => Ok(msg["result"].clone()),
                Some(false) => {
                    let code = msg["error"]["code"].as_str().unwrap_or("failed").to_string();
                    let message = msg["error"]["message"].as_str().unwrap_or("ошибка").to_string();
                    Err(if code == "cancelled" { Error::Interrupted } else { Error::Steam { code, message } })
                }
                None => Err(Error::SteamProtocol("ответ без ok".into())),
            };
        }
    }

    fn simple<T: for<'de> Deserialize<'de>>(&mut self, cmd: &str, args: Value) -> Result<T> {
        let v = self.request(cmd, args, &mut |_, _| {}, None, Some(REQUEST_TIMEOUT))?;
        parse(cmd, v)
    }

    /// QR-вход. Возвращает аккаунт и данные для Связки ключей.
    pub fn login_qr(
        &mut self,
        device_name: &str,
        on_event: &mut dyn FnMut(LoginEvent),
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<(Account, Credentials)> {
        let mut v = self.request(
            "login_qr",
            json!({ "device_name": device_name }),
            &mut |ev, msg| match ev {
                "qr" => {
                    if let Some(url) = msg["url"].as_str() {
                        on_event(LoginEvent::Qr { url: url.into() })
                    }
                }
                "confirmed" => on_event(LoginEvent::Confirmed),
                _ => {}
            },
            cancel,
            None,
        )?;
        let token = match v.get_mut("refresh_token").map(Value::take) {
            Some(Value::String(t)) if !t.is_empty() => RefreshToken(t),
            _ => return Err(Error::SteamProtocol("login_qr без refresh_token".into())),
        };
        let account: Account = parse("login_qr", v)?;
        let creds = Credentials { account_name: account.account_name.clone(), refresh_token: token };
        Ok((account, creds))
    }

    pub fn login_token(&mut self, creds: &Credentials) -> Result<Account> {
        self.simple(
            "login_token",
            json!({ "account_name": creds.account_name, "refresh_token": creds.refresh_token.expose() }),
        )
    }

    pub fn owned_apps(&mut self) -> Result<Vec<OwnedApp>> {
        #[derive(Deserialize)]
        struct R {
            apps: Vec<OwnedApp>,
        }
        self.simple::<R>("owned_apps", json!({})).map(|r| r.apps)
    }

    pub fn app_info(&mut self, appid: u32) -> Result<AppDepots> {
        self.simple("app_info", json!({ "appid": appid }))
    }

    /// Скачать Windows-депо `appid` в `dir`. `depots` пустой — какие выберет хелпер;
    /// `branch: None` — public.
    pub fn download(
        &mut self,
        appid: u32,
        dir: &Path,
        branch: Option<&str>,
        depots: &[u32],
        on_event: &mut dyn FnMut(DepotEvent),
        cancel: Option<&dyn Fn() -> bool>,
    ) -> Result<DownloadResult> {
        let dir = dir.to_str().ok_or(Error::Usage("путь к папке игры — не UTF-8"))?;
        let mut args = json!({ "appid": appid, "dir": dir });
        if !depots.is_empty() {
            args["depots"] = json!(depots);
        }
        if let Some(b) = branch {
            args["branch"] = b.into();
        }
        let v = self.request(
            "download",
            args,
            &mut |ev, m| {
                let n = |k: &str| m[k].as_u64().unwrap_or(0);
                match ev {
                    "progress" => on_event(DepotEvent::Progress { downloaded: n("downloaded"), total: n("total") }),
                    "depot" => on_event(DepotEvent::Depot {
                        depotid: n("depotid") as u32,
                        manifest: m["manifest"].as_str().unwrap_or_default().into(),
                        files: n("files"),
                        total: n("total"),
                    }),
                    _ => {}
                }
            },
            cancel,
            None,
        )?;
        parse("download", v)
    }

    /// Отозвать токен в Steam и закрыть сессию. `false` — токен и так уже не действовал.
    pub fn logout(&mut self, creds: Option<&Credentials>) -> Result<bool> {
        let args = match creds {
            Some(c) => json!({ "account_name": c.account_name, "refresh_token": c.refresh_token.expose() }),
            None => json!({}),
        };
        let v = self.request("logout", args, &mut |_, _| {}, None, Some(REQUEST_TIMEOUT))?;
        Ok(v["revoked"].as_bool().unwrap_or(false))
    }
}

impl Drop for SteamHelper {
    fn drop(&mut self) {
        // Закрытый stdin — сигнал хелперу выйти (и бросить текущую работу).
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse<T: for<'de> Deserialize<'de>>(cmd: &str, v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::SteamProtocol(format!("{cmd}: {e}")))
}

fn pump_log(r: impl Read, log: Logger) {
    for line in BufReader::new(r).lines() {
        let Ok(line) = line else { return };
        if !line.is_empty() {
            log(Log::Output(line));
        }
    }
}

/// Где искать хелпер: `CARAFE_STEAM_HELPER`, затем `steam/dist/<rid>/carafe-steam` рядом с
/// любым из предков бинарника (`target/release/carafe` → checkout carafe), затем рядом с ним.
pub fn find_helper(explicit: Option<&Path>, exe: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return crate::fsutil::is_executable(p).then(|| p.to_path_buf());
    }
    let rid = if cfg!(target_arch = "aarch64") { "osx-arm64" } else { "osx-x64" };
    let exe = exe?;
    exe.ancestors()
        .skip(1)
        .map(|a| a.join("steam/dist").join(rid).join("carafe-steam"))
        .chain(exe.parent().map(|d| d.join("carafe-steam")))
        .find(|p| crate::fsutil::is_executable(p))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    pub(crate) fn fake() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fake-carafe-steam")
    }

    fn spawn() -> (SteamHelper, Arc<Mutex<Vec<String>>>) {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let l = logs.clone();
        let h = SteamHelper::spawn(
            &fake(),
            Arc::new(move |m| {
                if let Log::Output(s) = m {
                    l.lock().unwrap().push(s)
                }
            }),
        )
        .unwrap();
        (h, logs)
    }

    fn creds(token: &str) -> Credentials {
        Credentials { account_name: "tester".into(), refresh_token: RefreshToken::new(token.into()) }
    }

    #[test]
    fn qr_login_then_requests() {
        let (mut h, logs) = spawn();
        let mut events = vec![];
        let (acc, c) = h.login_qr("carafe (test)", &mut |e| events.push(e), None).unwrap();
        assert_eq!(
            events,
            [
                LoginEvent::Qr { url: "https://s.team/q/1/111".into() },
                LoginEvent::Qr { url: "https://s.team/q/1/222".into() },
                LoginEvent::Confirmed
            ]
        );
        assert_eq!(acc, Account { account_name: "tester".into(), steam_id_masked: Some("••••0001".into()) });
        assert_eq!(c, creds("fake-refresh"));
        assert_eq!(format!("{c:?}").matches("fake-refresh").count(), 0, "токен не печатается в Debug");

        let apps = h.owned_apps().unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "Megabonk");
        assert_eq!((apps[0].windows, apps[0].size_bytes), (true, Some(1_100_000_000)));
        assert_eq!((apps[1].windows, apps[1].size_bytes), (false, None));

        let info = h.app_info(3405340).unwrap();
        assert_eq!(info.depots.iter().filter(|d| d.install).map(|d| d.depotid).collect::<Vec<_>>(), [3405341]);
        assert_eq!(info.depots[1].skip.as_deref(), Some("не Windows"));
        assert_eq!(info.depots[0].manifests["public"], "9007199254740993");
        assert!(h.logout(None).unwrap());
        drop(h);
        let logs = logs.lock().unwrap();
        assert!(logs.contains(&"fake: старт".to_string()), "{logs:?}");
    }

    #[test]
    fn token_login_and_errors() {
        let (mut h, _) = spawn();
        assert_eq!(h.login_token(&creds("fake-refresh")).unwrap().account_name, "tester");
        match h.login_token(&creds("old")) {
            Err(Error::Steam { code, .. }) => assert_eq!(code, "token_invalid"),
            r => panic!("{r:?}"),
        }
        match h.request("nope", json!({}), &mut |_, _| {}, None, None) {
            Err(Error::Steam { code, message }) => {
                assert_eq!((code.as_str(), message.as_str()), ("bad_request", "nope"))
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn download_progress_and_cancel() {
        let (mut h, _) = spawn();
        let mut got = vec![];
        let r = h.download(3405340, Path::new("/tmp/carafe-test"), None, &[], &mut |e| got.push(e), None).unwrap();
        assert_eq!(
            got,
            [
                DepotEvent::Depot { depotid: 3405341, manifest: "77".into(), files: 3, total: 300 },
                DepotEvent::Progress { downloaded: 0, total: 300 },
                DepotEvent::Progress { downloaded: 300, total: 300 },
            ]
        );
        assert_eq!((r.downloaded, r.files, r.depots[0].depotid), (300, 3, 3405341));

        // appid 2 — «бесконечная» загрузка: ждёт cancel.
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = AtomicBool::new(false);
        let r = h.download(
            2,
            Path::new("/tmp/x"),
            None,
            &[],
            &mut |_| stop.store(true, Ordering::Relaxed),
            Some(&|| stop.load(Ordering::Relaxed)),
        );
        assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
        // После отмены сессия жива.
        assert_eq!(h.app_info(3405340).unwrap().name, "Megabonk");
    }

    #[test]
    fn helper_dies() {
        let (mut h, _) = spawn();
        let r = h.request("crash", json!({}), &mut |_, _| {}, None, None);
        assert!(matches!(r, Err(Error::SteamProtocol(ref m)) if m.contains("завершился")), "{r:?}");
    }

    #[test]
    fn helper_search() {
        let d = crate::fsutil::testdir::TestDir::new();
        let rid = if cfg!(target_arch = "aarch64") { "osx-arm64" } else { "osx-x64" };
        let exe = d.path().join("target/release/carafe");
        assert_eq!(find_helper(None, Some(&exe)), None);
        let helper = d.path().join("steam/dist").join(rid).join("carafe-steam");
        d.file(&format!("steam/dist/{rid}/carafe-steam"), b"#!/bin/sh\n");
        assert_eq!(find_helper(None, Some(&exe)), None, "без бита исполнения не годится");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_helper(None, Some(&exe)), Some(helper));
        assert_eq!(find_helper(Some(&fake()), Some(&exe)), Some(fake()));
        assert_eq!(find_helper(Some(Path::new("/nonexistent")), Some(&exe)), None);
    }
}
