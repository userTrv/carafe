//! Мост до carafe-core: зовёт `bin/carafe` из checkout carafe и разбирает его вывод.
//!
//! Состояние (игры, движки, бутылки, мосты) читается прямо с диска — так же, как его
//! раскладывает `bin/carafe`. Всё, что требует Steam или Wine, идёт через сам скрипт.

use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::backend::*;

const DEFAULT_ENGINE: &str = "gptk-3.0-3";
const CLOUD_TIMEOUT: Duration = Duration::from_secs(90);

pub struct CliBackend {
    script: PathBuf,
    home: PathBuf,
    soju: PathBuf,
}

impl CliBackend {
    /// `checkout` — корень checkout carafe (там лежит `bin/carafe`).
    pub fn new(checkout: &Path) -> Result<Self> {
        let script = checkout.join("bin/carafe");
        if !is_exec(&script) {
            return Err(BackendError::new(format!(
                "нет {} — укажи checkout carafe",
                script.display()
            )));
        }
        let home = std::env::var_os("CARAFE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("Library/Application Support/carafe"));
        let soju = std::env::var_os("CARAFE_SOJU")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                checkout
                    .parent()
                    .map(|p| p.join("soju"))
                    .unwrap_or_default()
            });
        Ok(Self { script, home, soju })
    }

    fn engine_dir(&self, profile: &Profile) -> PathBuf {
        match &profile.engine {
            Some(id) => PathBuf::from(id),
            None => self.home.join("engines").join(DEFAULT_ENGINE),
        }
    }

    fn game_dir(&self, appid: AppId) -> PathBuf {
        self.home.join("games").join(appid.to_string())
    }

    fn env(&self, profile: &Profile) -> Vec<(&'static str, String)> {
        let mut env = vec![
            ("CARAFE_BOTTLE", profile.bottle.clone()),
            ("CARAFE_HUD", if profile.hud { "1" } else { "0" }.into()),
            (
                "CARAFE_BRIDGE",
                if profile.bridge { "1" } else { "0" }.into(),
            ),
            ("CARAFE_CLOUD", if profile.cloud { "1" } else { "0" }.into()),
            // Приложение из Finder получает урезанный PATH.
            (
                "PATH",
                "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin".into(),
            ),
        ];
        if let Some(id) = &profile.engine {
            env.push(("CARAFE_ENGINE_DIR", id.clone()));
        }
        env
    }

    /// Запустить `bin/carafe ARGS`, отдавая строки stdout и stderr в `on_line` по мере появления.
    fn run(
        &self,
        args: &[&str],
        profile: &Profile,
        timeout: Option<Duration>,
        mut on_line: impl FnMut(&str),
    ) -> Result<(i32, Vec<String>)> {
        let mut child = Command::new(&self.script)
            .args(args)
            .envs(self.env(profile))
            .env_remove("CARAFE_EXE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| BackendError::new(format!("не запустился bin/carafe: {e}")))?;

        let (tx, rx) = mpsc::channel::<String>();
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(spawn_reader(out, tx.clone()));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(spawn_reader(err, tx.clone()));
        }
        drop(tx);

        let started = Instant::now();
        let mut lines = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    on_line(&line);
                    lines.push(line);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if timeout.is_some_and(|t| started.elapsed() > t) {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(BackendError::new("bin/carafe не ответил вовремя"));
                    }
                }
            }
        }
        for r in readers {
            let _ = r.join();
        }
        let status = child
            .wait()
            .map_err(|e| BackendError::new(format!("bin/carafe: {e}")))?;
        Ok((status.code().unwrap_or(-1), lines))
    }

    /// Оригинальный steam_api64.dll игры — так же, как `steam_api_orig` в bin/carafe.
    fn steam_api_orig(&self, dir: &Path) -> Option<PathBuf> {
        let disabled = dir.join(".carafe-disabled/steam_api64.dll");
        let live = find_files(dir, 4, &|p| {
            p.file_name().is_some_and(|n| n == "steam_api64.dll")
                && p.parent().is_some_and(|d| d.ends_with("Plugins/x86_64"))
        })
        .into_iter()
        .next()
        .or_else(|| Some(dir.join("steam_api64.dll")).filter(|p| p.is_file()))?;
        Some(if disabled.is_file() { disabled } else { live })
    }

    fn bridge_state(&self, dir: &Path) -> BridgeState {
        let Some(orig) = self.steam_api_orig(dir) else {
            return BridgeState::NotNeeded;
        };
        match sha256(&orig) {
            Some(sha) if is_exec(&self.home.join("bridge").join(&sha).join("soju-steamd")) => {
                BridgeState::Ready
            }
            Some(_) => BridgeState::NotBuilt,
            None => BridgeState::Unknown,
        }
    }
}

impl Backend for CliBackend {
    fn label(&self) -> String {
        "bin/carafe".into()
    }

    fn environment(&self, profile: &Profile) -> Result<Environment> {
        let engine = self.engine_dir(profile);
        let engine_ok = wine_in(&engine);
        let d3d = has_d3dmetal(&engine);
        let bottle = self.home.join("bottles").join(&profile.bottle);
        let rosetta = Command::new("/usr/bin/arch")
            .args(["-x86_64", "/usr/bin/true"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let steam = Command::new("/usr/bin/pgrep")
            .args(["-x", "steam_osx"])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let soju = is_exec(&self.soju.join("bin/soju-bridge"));

        let custom = profile.engine.is_some();
        let checks = vec![
            Check {
                id: CheckId::Rosetta,
                ok: rosetta,
                fixable: false,
                hint: Some("softwareupdate --install-rosetta".into()),
            },
            Check {
                id: CheckId::Engine,
                ok: engine_ok,
                fixable: !custom,
                hint: custom.then(|| engine.display().to_string()),
            },
            Check {
                id: CheckId::D3dMetal,
                ok: d3d,
                fixable: !custom,
                hint: custom.then(|| "soju: scripts/install-d3dmetal.sh".into()),
            },
            Check {
                id: CheckId::Bottle,
                ok: bottle.join("drive_c").is_dir(),
                fixable: engine_ok,
                hint: None,
            },
            Check {
                id: CheckId::Bridge,
                ok: soju,
                fixable: false,
                hint: Some(format!(
                    "git clone https://github.com/userTrv/soju {}",
                    self.soju.display()
                )),
            },
            Check {
                id: CheckId::Steam,
                ok: steam,
                fixable: true,
                hint: None,
            },
        ];

        let mut engines = Vec::new();
        for dir in subdirs(&self.home.join("engines")) {
            if dir.file_name().is_some_and(|n| n == "dl") {
                continue;
            }
            engines.push(engine_info(&dir));
        }
        let soju_engine = self.soju.join("dist/soju-engine");
        if wine_in(&soju_engine) {
            engines.push(engine_info(&soju_engine));
        }
        // id движка по умолчанию — None в профиле; в списке он идёт первым.
        let default = self.home.join("engines").join(DEFAULT_ENGINE);
        engines.sort_by_key(|e| Path::new(&e.id) != default);
        if let Some(e) = engines.first_mut().filter(|e| Path::new(&e.id) == default) {
            e.id = String::new();
        }

        let bottles = subdirs(&self.home.join("bottles"))
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(String::from))
            .collect();

        Ok(Environment {
            checks,
            engines,
            bottles,
        })
    }

    fn games(&self, _profile: &Profile) -> Result<Vec<Game>> {
        let mut games = Vec::new();
        for dir in subdirs(&self.home.join("games")) {
            let Some(appid) = dir
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse::<AppId>().ok())
            else {
                continue;
            };
            let exe = find_game_exe(&dir);
            let name = exe
                .as_ref()
                .and_then(|e| e.file_stem())
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(|| format!("App {appid}"));
            let bridge = self.bridge_state(&dir);
            let state_file = dir.join(".carafe-cloud-state");
            let last_sync = mtime(&state_file);
            let rules = fs::read_to_string(dir.join(".carafe-ufs")).ok();
            let state = match (bridge, rules.as_deref(), last_sync) {
                (BridgeState::NotNeeded, ..) => CloudState::Unavailable,
                (_, Some(r), _) if r.trim().is_empty() => CloudState::Unavailable,
                (_, _, Some(_)) => CloudState::Synced,
                _ => CloudState::Never,
            };
            games.push(Game {
                appid,
                name,
                cover: Cover::for_app(appid),
                installed: exe.is_some(),
                size_bytes: None,
                bridge,
                cloud: CloudSummary { state, last_sync },
            });
        }
        games.sort_by_key(|g| g.name.to_lowercase());
        Ok(games)
    }

    fn cloud_status(&self, appid: AppId, profile: &Profile) -> Result<CloudStatus> {
        let id = appid.to_string();
        // `cloud status` переписывает файл состояния — время берём до вызова.
        let last_sync = mtime(&self.game_dir(appid).join(".carafe-cloud-state"));
        let (code, lines) = self.run(
            &["cloud", "status", &id],
            profile,
            Some(CLOUD_TIMEOUT),
            |_| {},
        )?;
        if code != 0 {
            return Err(error_from(&lines));
        }
        let files: Vec<CloudFile> = lines.iter().filter_map(|l| parse_cloud_line(l)).collect();
        let note = if files.is_empty() {
            lines
                .iter()
                .find_map(|l| l.strip_prefix("==> "))
                .filter(|l| !l.starts_with("Steam Cloud:"))
                .map(String::from)
        } else {
            None
        };
        Ok(CloudStatus {
            files,
            last_sync,
            note,
        })
    }

    fn launch(&self, appid: AppId, profile: &Profile, events: Events) -> Result<i32> {
        let id = appid.to_string();
        let mut tracker = LaunchTracker::new(events);
        let (code, lines) = self.run(&["game", "run", &id], profile, None, |line| {
            tracker.line(line);
        })?;
        tracker.finish(code);
        if code != 0 && !tracker.game_started {
            return Err(error_from(&lines));
        }
        Ok(code)
    }

    fn build_bridge(&self, appid: AppId, profile: &Profile, events: Events) -> Result<()> {
        let id = appid.to_string();
        stage_ev(events, Stage::Build, StageStatus::Active);
        let (code, lines) = self.run(&["bridge", "build", &id], profile, None, |l| {
            events(TaskEvent::Log { line: l.into() });
        })?;
        finish_stage(events, Stage::Build, code, &lines)
    }

    fn install_engine(&self, profile: &Profile, events: Events) -> Result<()> {
        stage_ev(events, Stage::Download, StageStatus::Active);
        let mut unpacking = false;
        let (code, lines) = self.run(&["engine", "install"], profile, None, |l| {
            if let Some(p) = curl_percent(l) {
                events(TaskEvent::Progress {
                    stage: Stage::Download,
                    fraction: p,
                });
                return;
            }
            if !unpacking && l.starts_with("==> распаковываю") {
                unpacking = true;
                stage_ev(events, Stage::Download, StageStatus::Done);
                stage_ev(events, Stage::Unpack, StageStatus::Active);
            }
            events(TaskEvent::Log { line: l.into() });
        })?;
        if !unpacking && code == 0 {
            stage_ev(events, Stage::Download, StageStatus::Done);
        }
        finish_stage(
            events,
            if unpacking {
                Stage::Unpack
            } else {
                Stage::Download
            },
            code,
            &lines,
        )
    }

    fn create_bottle(&self, profile: &Profile, events: Events) -> Result<()> {
        stage_ev(events, Stage::Bottle, StageStatus::Active);
        let (code, lines) =
            self.run(&["bottle", "create", &profile.bottle], profile, None, |l| {
                events(TaskEvent::Log { line: l.into() });
            })?;
        finish_stage(events, Stage::Bottle, code, &lines)
    }

    fn open_steam(&self) -> Result<()> {
        let ok = Command::new("/usr/bin/open")
            .args(["-g", "-a", "Steam"])
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            Ok(())
        } else {
            Err(BackendError::new("не нашёл Steam для Mac"))
        }
    }
}

/// Превращает вывод `carafe game run` в стадии запуска.
struct LaunchTracker<'a> {
    events: Events<'a>,
    current: Option<Stage>,
    seen: Vec<Stage>,
    game_started: bool,
}

impl<'a> LaunchTracker<'a> {
    fn new(events: Events<'a>) -> Self {
        Self {
            events,
            current: None,
            seen: Vec::new(),
            game_started: false,
        }
    }

    fn enter(&mut self, stage: Stage) {
        if let Some(prev) = self.current.take() {
            stage_ev(self.events, prev, StageStatus::Done);
        }
        // Пропущенные стадии до этой.
        for s in [
            Stage::CloudPull,
            Stage::Bridge,
            Stage::Game,
            Stage::CloudPush,
        ] {
            if s == stage {
                break;
            }
            if !self.seen.contains(&s) {
                self.seen.push(s);
                stage_ev(self.events, s, StageStatus::Skipped);
            }
        }
        self.seen.push(stage);
        self.current = Some(stage);
        stage_ev(self.events, stage, StageStatus::Active);
    }

    fn fail(&mut self, stage: Stage, message: &str) {
        if self.current == Some(stage) {
            self.current = None;
        }
        stage_ev(
            self.events,
            stage,
            StageStatus::Failed {
                message: message.into(),
            },
        );
    }

    fn line(&mut self, line: &str) {
        (self.events)(TaskEvent::Log { line: line.into() });
        if let Some(file) = parse_cloud_line(line) {
            (self.events)(TaskEvent::CloudFile { file });
            return;
        }
        let Some(msg) = line.strip_prefix("==> ") else {
            return;
        };
        if msg == "Steam Cloud: pull" {
            self.enter(Stage::CloudPull);
        } else if msg == "Steam Cloud: push" {
            self.enter(Stage::CloudPush);
        } else if msg.starts_with("Steam Cloud: не удалось скачать") {
            self.fail(
                Stage::CloudPull,
                "не удалось скачать сейвы — играем с локальными",
            );
        } else if msg.starts_with("Steam Cloud: выгрузка не удалась") {
            self.fail(Stage::CloudPush, "выгрузка не удалась");
        } else if msg.starts_with("мост в Steam") {
            self.enter(Stage::Bridge);
        } else if msg.starts_with("запускаю ") {
            self.enter(Stage::Game);
            self.game_started = true;
        } else if msg.starts_with("у игры нет правил Auto-Cloud")
            && self.current == Some(Stage::CloudPull)
        {
            self.current = None;
            stage_ev(self.events, Stage::CloudPull, StageStatus::Skipped);
        }
    }

    fn finish(&mut self, code: i32) {
        if let Some(stage) = self.current.take() {
            if code == 0 || stage == Stage::Game {
                stage_ev(self.events, stage, StageStatus::Done);
            } else {
                self.fail(stage, &format!("код выхода {code}"));
            }
        }
        if self.game_started && !self.seen.contains(&Stage::CloudPush) {
            // Мост или облако выключены: bin/carafe не выгружает сейвы.
            stage_ev(self.events, Stage::CloudPush, StageStatus::Skipped);
        }
    }
}

fn stage_ev(events: Events, stage: Stage, status: StageStatus) {
    events(TaskEvent::Stage { stage, status });
}

fn finish_stage(events: Events, stage: Stage, code: i32, lines: &[String]) -> Result<()> {
    if code == 0 {
        stage_ev(events, stage, StageStatus::Done);
        Ok(())
    } else {
        let err = error_from(lines);
        stage_ev(
            events,
            stage,
            StageStatus::Failed {
                message: err.0.clone(),
            },
        );
        Err(err)
    }
}

/// Последняя строка-ошибка скрипта или хелпера облака.
fn error_from(lines: &[String]) -> BackendError {
    let msg = lines
        .iter()
        .rev()
        .find_map(|l| {
            l.strip_prefix("carafe: ")
                .or_else(|| l.strip_prefix("soju cloud: "))
        })
        .or_else(|| lines.last().map(String::as_str))
        .unwrap_or("bin/carafe завершился с ошибкой");
    BackendError::new(msg.trim())
}

/// `  = файл`, `  ↓ файл`, `  ↑ файл (конфликт: …)` — формат `soju-steamd cloud`.
fn parse_cloud_line(line: &str) -> Option<CloudFile> {
    let rest = line.strip_prefix("  ")?;
    let mut chars = rest.chars();
    let action = match chars.next()? {
        '=' => CloudAction::Synced,
        '↓' => CloudAction::Download,
        '↑' => CloudAction::Upload,
        _ => return None,
    };
    let rest = chars.as_str().strip_prefix(' ')?;
    let (name, conflict) = match rest.find(" (конфликт") {
        Some(i) => (&rest[..i], true),
        None => (rest, false),
    };
    (!name.is_empty()).then(|| CloudFile {
        name: name.into(),
        action,
        conflict,
    })
}

/// Строка прогресса curl: первая колонка — проценты.
fn curl_percent(line: &str) -> Option<f32> {
    let mut cols = line.split_whitespace();
    let pct: f32 = cols.next()?.parse().ok()?;
    (cols.count() >= 10 && (0.0..=100.0).contains(&pct)).then_some(pct / 100.0)
}

fn spawn_reader(
    src: impl Read + Send + 'static,
    tx: mpsc::Sender<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // curl пишет прогресс через \r — режем и по нему.
        let mut reader = BufReader::new(src);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let mut byte = [0u8; 1];
            let mut got = false;
            while let Ok(1) = reader.read(&mut byte) {
                got = true;
                if byte[0] == b'\n' || byte[0] == b'\r' {
                    break;
                }
                buf.push(byte[0]);
            }
            if !got {
                break;
            }
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            if !line.is_empty() && tx.send(line).is_err() {
                break;
            }
        }
    })
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o100 != 0)
}

fn mtime(p: &Path) -> Option<u64> {
    let t = fs::metadata(p).ok()?.modified().ok()?;
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out
}

/// Файлы до глубины `depth`, без перехода по симлинкам.
fn find_files(dir: &Path, depth: usize, pred: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() && depth > 0 {
            out.extend(find_files(&p, depth - 1, pred));
        } else if ft.is_file() && pred(&p) {
            out.push(p);
        }
    }
    out
}

/// Есть ли каталог с этим именем до глубины `depth` (без симлинков).
fn find_dir(dir: &Path, name: &str, depth: usize) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    rd.flatten().any(|e| {
        e.file_type().is_ok_and(|t| t.is_dir())
            && (e.file_name() == name || (depth > 0 && find_dir(&e.path(), name, depth - 1)))
    })
}

fn wine_in(engine: &Path) -> bool {
    !find_files(engine, 6, &|p| {
        p.parent().is_some_and(|d| d.ends_with("bin"))
            && p.file_name()
                .is_some_and(|n| n == "soju" || n == "wine64" || n == "wine")
            && is_exec(p)
    })
    .is_empty()
}

fn has_d3dmetal(engine: &Path) -> bool {
    find_dir(engine, "D3DMetal.framework", 7)
}

fn engine_info(dir: &Path) -> EngineInfo {
    let base = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let name = match base.as_str() {
        "soju-engine" => "soju".to_string(),
        b if b.starts_with("gptk-") => format!("GPTK {}", &b[5..]),
        b => b.to_string(),
    };
    EngineInfo {
        id: dir.display().to_string(),
        name,
        d3dmetal: has_d3dmetal(dir),
    }
}

/// Самый крупный exe верхнего уровня, кроме служебных — как `find_game_exe` в bin/carafe.
fn find_game_exe(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let Some(name) = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_lowercase)
        else {
            continue;
        };
        if !name.ends_with(".exe")
            || name.starts_with("unitycrashhandler")
            || name.contains("setup")
            || name.contains("redist")
        {
            continue;
        }
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        if best.as_ref().is_none_or(|(s, _)| size > *s) {
            best = Some((size, p));
        }
    }
    best.map(|(_, p)| p)
}

/// sha256 через системный shasum, кешируется по пути, размеру и mtime.
fn sha256(p: &Path) -> Option<String> {
    use std::sync::{Mutex, OnceLock};
    type Key = (PathBuf, u64, u64);
    static CACHE: OnceLock<Mutex<HashMap<Key, String>>> = OnceLock::new();
    let meta = fs::metadata(p).ok()?;
    let key = (p.to_path_buf(), meta.len(), mtime(p).unwrap_or(0));
    let cache = CACHE.get_or_init(Default::default);
    if let Some(s) = cache.lock().ok()?.get(&key) {
        return Some(s.clone());
    }
    let out = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(p)
        .output()
        .ok()?;
    let sha = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()?
        .to_string();
    (sha.len() == 64).then(|| {
        cache.lock().ok().map(|mut c| c.insert(key, sha.clone()));
        sha
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_lines() {
        let f = parse_cloud_line("  ↑ stats.json (конфликт: новее — эта сторона)").unwrap();
        assert_eq!(f.action, CloudAction::Upload);
        assert!(f.conflict);
        assert_eq!(f.name, "stats.json");
        assert_eq!(parse_cloud_line("  = a b.json").unwrap().name, "a b.json");
        assert!(parse_cloud_line("==> Steam Cloud: status").is_none());
        assert!(parse_cloud_line("    \"memorysetup=1\"").is_none());
    }

    #[test]
    fn curl_progress() {
        let l = " 45  120M   45 54.0M    0     0  12.1M      0  0:00:09  0:00:04  0:00:05 12.3M";
        assert_eq!(curl_percent(l), Some(0.45));
        assert_eq!(curl_percent("==> качаю движок"), None);
    }

    #[test]
    fn launch_stages() {
        use std::sync::Mutex;
        let got = Mutex::new(Vec::new());
        let sink = |e: TaskEvent| {
            if let TaskEvent::Stage { stage, status } = e {
                got.lock().unwrap().push(format!("{stage:?}:{status:?}"));
            }
        };
        let mut t = LaunchTracker::new(&sink);
        for l in [
            "==> Steam Cloud: pull",
            "  = stats.json",
            "==> мост в Steam: порт 1, лог x",
            "==> запускаю Game.exe",
            "esync: up and running.",
            "==> Steam Cloud: push",
        ] {
            t.line(l);
        }
        t.finish(0);
        assert_eq!(
            got.into_inner().unwrap(),
            [
                "CloudPull:Active",
                "CloudPull:Done",
                "Bridge:Active",
                "Bridge:Done",
                "Game:Active",
                "Game:Done",
                "CloudPush:Active",
                "CloudPush:Done",
            ]
        );
    }
}
