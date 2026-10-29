//! Загрузчики игр: общий трейт [`Downloader`] и его реализации — steamcmd и мок для тестов и GUI.
//!
//! Загрузчик качает одну игру в одну папку и шлёт события по ходу дела. Очередь
//! ([`crate::queue`]) сама решает, что качать и в каком порядке, считает скорость и ETA и
//! останавливает загрузку флагом [`StopFlag`]: пауза и отмена для загрузчика одно и то же —
//! остановиться как можно скорее и вернуть [`Error::Interrupted`]. Продолжение — это новый вызов
//! [`Downloader::download`] с той же папкой: steamcmd и Steam докачивают недостающее сами.

use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::Carafe;
use crate::error::{Error, IoContext, Result};
use crate::fsutil;
use crate::vdf;

/// Что скачать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub appid: u32,
    /// Папка игры: `<библиотека>/<appid>`. Существует к началу загрузки.
    pub dir: PathBuf,
    /// Ветка (`-beta` у steamcmd); `None` — public.
    pub branch: Option<String>,
    /// Депо Windows-сборки; пусто — все, какие выберет загрузчик. steamcmd выбирает сам и это поле
    /// не смотрит, загрузчику через SteamKit2 оно нужно для `download_depot`.
    pub depots: Vec<u32>,
    /// Проверить уже скачанные файлы (steamcmd `validate`).
    pub verify: bool,
}

/// Этап загрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadStage {
    /// Запуск загрузчика, вход в Steam, манифесты.
    Preparing,
    /// Место под файлы.
    Allocating,
    Downloading,
    /// Проверка файлов.
    Verifying,
    /// Раскладка скачанного по местам.
    Committing,
}

impl DownloadStage {
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadStage::Preparing => "preparing",
            DownloadStage::Allocating => "allocating",
            DownloadStage::Downloading => "downloading",
            DownloadStage::Verifying => "verifying",
            DownloadStage::Committing => "committing",
        }
    }

    pub fn parse(s: &str) -> Option<DownloadStage> {
        [Self::Preparing, Self::Allocating, Self::Downloading, Self::Verifying, Self::Committing]
            .into_iter()
            .find(|v| v.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            DownloadStage::Preparing => "подготовка",
            DownloadStage::Allocating => "место на диске",
            DownloadStage::Downloading => "загрузка",
            DownloadStage::Verifying => "проверка файлов",
            DownloadStage::Committing => "установка",
        }
    }
}

/// Событие загрузчика.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadEvent {
    Stage(DownloadStage),
    /// Байты этапа: сколько готово из скольких. Скорость и ETA считает очередь.
    Progress {
        done: u64,
        total: u64,
    },
    /// Строка вывода загрузчика — для лога.
    Log(String),
}

/// Флаг остановки загрузки: его ставит очередь, загрузчик проверяет.
#[derive(Debug, Default)]
pub struct StopFlag(AtomicBool);

impl StopFlag {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed)
    }

    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Загрузчик одной игры.
pub trait Downloader: Send + Sync {
    /// Для лога и GUI: «steamcmd», «SteamKit2».
    fn name(&self) -> &str;

    /// Скачать (или докачать, или проверить) игру в `req.dir`. Блокирует до конца. Остановлен
    /// флагом — [`Error::Interrupted`], уже скачанное остаётся на месте для продолжения.
    fn download(&self, req: &DownloadRequest, stop: &StopFlag, on_event: &mut dyn FnMut(DownloadEvent)) -> Result<()>;
}

/// steamcmd от Valve. Нужен сохранённый вход (`carafe steam login USER`): пароль загрузчик не
/// спрашивает — он работает без терминала.
pub struct SteamCmdDownloader {
    carafe: Carafe,
    user: Option<String>,
}

impl SteamCmdDownloader {
    /// `user: None` — первый аккаунт, с которым входили в steamcmd.
    pub fn new(carafe: Carafe, user: Option<String>) -> SteamCmdDownloader {
        SteamCmdDownloader { carafe, user }
    }
}

/// Аккаунты, с которыми входили в steamcmd: `config/config.vdf` → `Accounts`. Только имена;
/// остальное в файле (токены входа) не читаем.
pub fn steamcmd_accounts(steamcmd_dir: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(steamcmd_dir.join("config/config.vdf")) else { return vec![] };
    let Ok(doc) = vdf::parse(&text) else { return vec![] };
    doc.path(&["InstallConfigStore", "Software", "Valve", "Steam", "Accounts"])
        .map(|a| a.iter().map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default()
}

impl Downloader for SteamCmdDownloader {
    fn name(&self) -> &str {
        "steamcmd"
    }

    fn download(&self, req: &DownloadRequest, stop: &StopFlag, on_event: &mut dyn FnMut(DownloadEvent)) -> Result<()> {
        on_event(DownloadEvent::Stage(DownloadStage::Preparing));
        let user = match &self.user {
            Some(u) => u.clone(),
            None => {
                steamcmd_accounts(&self.carafe.config.steamcmd_dir()).into_iter().next().ok_or(Error::NoSteamLogin)?
            }
        };
        let sh = self.carafe.ensure_steamcmd()?;
        let mut cmd = Command::new(sh);
        cmd.args(["+@ShutdownOnFailedCommand", "1", "+@NoPromptForPassword", "1"])
            .args(["+@sSteamCmdForcePlatformType", "windows", "+force_install_dir"])
            .arg(&req.dir)
            .args(["+login", &user, "+app_update", &req.appid.to_string()]);
        if let Some(b) = &req.branch {
            cmd.args(["-beta", b]);
        }
        if req.verify {
            cmd.arg("validate");
        }
        cmd.arg("+quit").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(crate::process::spawn_err(&cmd))?;

        let (tx, rx) = mpsc::channel::<String>();
        for r in [
            child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>),
            child.stderr.take().map(|e| Box::new(e) as _),
        ]
        .into_iter()
        .flatten()
        {
            let tx = tx.clone();
            thread::spawn(move || read_lines(r, &tx));
        }
        drop(tx);

        let mut out = SteamCmdOutput::default();
        let status = loop {
            if stop.is_stopped() {
                kill_tree(&mut child);
                return Err(Error::Interrupted);
            }
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    for ev in out.line(&line) {
                        on_event(ev);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break child.wait(),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if let Some(st) = child.try_wait().transpose() {
                // Хвост вывода.
                while let Ok(line) = rx.recv_timeout(Duration::from_millis(300)) {
                    for ev in out.line(&line) {
                        on_event(ev);
                    }
                }
                break st;
            }
        };
        let status = status.map_err(crate::process::spawn_err(&cmd))?;
        out.result(status.success(), &cmd, status)
    }
}

/// Остановить steamcmd.sh вместе с его дочерним steamcmd: SIGTERM, через 5 секунд — SIGKILL.
/// Группу процессов не заводим, чтобы Ctrl-C в терминале доходил до steamcmd сам; `kill(2)` без
/// `unsafe` недоступен — зовём `pkill -P` и `kill`.
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    for sig in ["-TERM", "-KILL"] {
        let _ = Command::new("/usr/bin/pkill").args([sig, "-P", &pid]).stderr(Stdio::null()).status();
        let _ = Command::new("/bin/kill").args([sig, &pid]).stderr(Stdio::null()).status();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = child.wait();
}

/// Строки по `\n` и `\r`: steamcmd перерисовывает прогресс через `\r`.
fn read_lines(r: impl Read, tx: &mpsc::Sender<String>) {
    let mut r = BufReader::new(r);
    let mut line = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                if !line.is_empty() {
                    let s = String::from_utf8_lossy(&line).trim_end().to_string();
                    line.clear();
                    if !s.is_empty() && tx.send(s).is_err() {
                        return;
                    }
                }
            } else {
                line.push(b);
            }
        }
    }
    if !line.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&line).trim_end().to_string());
    }
}

/// Разбор вывода `steamcmd +app_update`.
#[derive(Debug, Default)]
struct SteamCmdOutput {
    stage: Option<DownloadStage>,
    success: bool,
    error: Option<String>,
    login_failed: bool,
}

impl SteamCmdOutput {
    fn line(&mut self, line: &str) -> Vec<DownloadEvent> {
        let mut ev = vec![];
        let l = line.trim();
        // ` Update state (0x61) downloading, progress: 45.23 (1234567 / 2729384727)`
        if let Some(rest) = l.strip_prefix("Update state (")
            && let Some((_, rest)) = rest.split_once(") ")
        {
            let (state, progress) = rest.split_once(", progress:").unwrap_or((rest, ""));
            let stage = match state.trim() {
                s if s.contains("download") => DownloadStage::Downloading,
                s if s.contains("verif") || s.contains("validat") => DownloadStage::Verifying,
                s if s.contains("prealloc") => DownloadStage::Allocating,
                s if s.contains("commit") => DownloadStage::Committing,
                _ => DownloadStage::Preparing,
            };
            if self.stage != Some(stage) {
                self.stage = Some(stage);
                ev.push(DownloadEvent::Stage(stage));
            }
            if let Some((_, nums)) = progress.split_once('(')
                && let Some((done, total)) = nums.trim_end_matches(')').split_once('/')
                && let (Ok(done), Ok(total)) = (done.trim().parse(), total.trim().parse())
            {
                ev.push(DownloadEvent::Progress { done, total });
            }
            return ev;
        }
        if l.starts_with("Success! App") {
            self.success = true;
        } else if let Some(rest) = l.strip_prefix("ERROR! ").or_else(|| l.strip_prefix("Error! ")) {
            // `ERROR! Failed to install app '480' (No subscription)` — причина в скобках.
            let why = rest.rsplit_once('(').map(|(_, w)| w.trim_end_matches(')')).unwrap_or(rest);
            self.error = Some(why.to_string());
        } else if l.contains("Cached credentials not found")
            || l.contains("Invalid Password")
            || l.contains("password:")
            || l.contains("Two-factor code")
        {
            self.login_failed = true;
        } else if l.starts_with("Logging in user") && self.stage.is_none() {
            ev.push(DownloadEvent::Stage(DownloadStage::Preparing));
        }
        ev.push(DownloadEvent::Log(line.to_string()));
        ev
    }

    fn result(self, ok: bool, cmd: &Command, status: std::process::ExitStatus) -> Result<()> {
        if self.login_failed {
            return Err(Error::NoSteamLogin);
        }
        if let Some(e) = self.error {
            return Err(Error::SteamCmd(e));
        }
        // steamcmd бывает выходит с 0 и после ошибки — верим только «Success!».
        if self.success {
            return Ok(());
        }
        if ok {
            return Err(Error::SteamCmd("загрузка не завершилась".into()));
        }
        crate::process::check(cmd, status)
    }
}

/// Мок для тестов и GUI: «качает» `total` байт кусками по `step` с паузой `delay`, пишет
/// `mock-game.exe` в папку игры. Сколько скачано, помнит в `.mock-download` — после паузы
/// продолжает с того же места, как настоящий загрузчик.
#[derive(Debug, Clone)]
pub struct MockDownloader {
    pub total: u64,
    pub step: u64,
    pub delay: Duration,
    /// Упасть, дойдя до этого числа байт.
    pub fail_at: Option<u64>,
}

impl Default for MockDownloader {
    fn default() -> Self {
        MockDownloader { total: 1 << 20, step: 64 << 10, delay: Duration::ZERO, fail_at: None }
    }
}

pub const MOCK_EXE: &str = "mock-game.exe";
const MOCK_STATE: &str = ".mock-download";

impl Downloader for MockDownloader {
    fn name(&self) -> &str {
        "mock"
    }

    fn download(&self, req: &DownloadRequest, stop: &StopFlag, on_event: &mut dyn FnMut(DownloadEvent)) -> Result<()> {
        on_event(DownloadEvent::Stage(DownloadStage::Preparing));
        let state = req.dir.join(MOCK_STATE);
        let mut done: u64 = fs::read_to_string(&state).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        if req.verify && done >= self.total {
            on_event(DownloadEvent::Stage(DownloadStage::Verifying));
            on_event(DownloadEvent::Progress { done: self.total, total: self.total });
            return Ok(());
        }
        on_event(DownloadEvent::Stage(DownloadStage::Downloading));
        while done < self.total {
            if stop.is_stopped() {
                return Err(Error::Interrupted);
            }
            if self.fail_at.is_some_and(|f| done >= f) {
                return Err(Error::SteamCmd("No connection".into()));
            }
            thread::sleep(self.delay);
            done = (done + self.step).min(self.total);
            fs::write(&state, done.to_string()).at(&state)?;
            on_event(DownloadEvent::Progress { done, total: self.total });
        }
        on_event(DownloadEvent::Stage(DownloadStage::Committing));
        let exe = req.dir.join(MOCK_EXE);
        fs::write(&exe, b"MZ").at(&exe)?;
        fsutil::remove_all(&state)?;
        on_event(DownloadEvent::Log(format!("Success! App '{}' fully installed.", req.appid)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;

    fn feed(lines: &[&str]) -> (Vec<DownloadEvent>, SteamCmdOutput) {
        let mut o = SteamCmdOutput::default();
        let ev = lines.iter().flat_map(|l| o.line(l)).filter(|e| !matches!(e, DownloadEvent::Log(_))).collect();
        (ev, o)
    }

    #[test]
    fn steamcmd_progress() {
        let (ev, o) = feed(&[
            "Logging in user 'someone' to Steam Public...OK",
            " Update state (0x3) reconfiguring, progress: 0.00 (0 / 0)",
            " Update state (0x11) preallocating, progress: 12.50 (100 / 800)",
            " Update state (0x61) downloading, progress: 45.23 (1234567 / 2729384727)",
            " Update state (0x61) downloading, progress: 50.00 (1364692363 / 2729384727)",
            " Update state (0x81) verifying update, progress: 10.00 (10 / 100)",
            " Update state (0x101) committing, progress: 99.00 (99 / 100)",
            "Success! App '3405340' fully installed.",
        ]);
        use DownloadEvent::*;
        use DownloadStage::*;
        assert_eq!(
            ev,
            [
                Stage(Preparing),
                Stage(Preparing),
                Progress { done: 0, total: 0 },
                Stage(Allocating),
                Progress { done: 100, total: 800 },
                Stage(Downloading),
                Progress { done: 1234567, total: 2729384727 },
                Progress { done: 1364692363, total: 2729384727 },
                Stage(Verifying),
                Progress { done: 10, total: 100 },
                Stage(Committing),
                Progress { done: 99, total: 100 },
            ]
        );
        assert!(o.success && o.error.is_none());
    }

    #[test]
    fn steamcmd_errors() {
        let ok = std::process::ExitStatus::default();
        let cmd = Command::new("steamcmd.sh");
        let (_, o) = feed(&["ERROR! Failed to install app '480' (No subscription)"]);
        assert!(matches!(o.result(true, &cmd, ok), Err(Error::SteamCmd(e)) if e == "No subscription"));
        let (_, o) = feed(&["Error! App '480' state is 0x202 after update job."]);
        assert!(matches!(o.result(true, &cmd, ok), Err(Error::SteamCmd(_))));
        let (_, o) = feed(&["Cached credentials not found."]);
        assert!(matches!(o.result(true, &cmd, ok), Err(Error::NoSteamLogin)));
        let (_, o) = feed(&["Loading Steam API...OK"]);
        assert!(matches!(o.result(true, &cmd, ok), Err(Error::SteamCmd(_))));
    }

    #[test]
    fn accounts_from_config() {
        let d = TestDir::new();
        assert!(steamcmd_accounts(d.path()).is_empty());
        d.file(
            "config/config.vdf",
            b"\"InstallConfigStore\" { \"Software\" { \"Valve\" { \"steam\" { \"Accounts\" { \"player\" { \"SteamID\" \"1\" } } } } } }",
        );
        assert_eq!(steamcmd_accounts(d.path()), ["player"]);
    }

    #[test]
    fn mock_resumes() {
        let d = TestDir::new();
        let req = DownloadRequest { appid: 1, dir: d.path().into(), branch: None, depots: vec![], verify: false };
        let dl = MockDownloader { total: 10, step: 2, ..Default::default() };
        let stop = StopFlag::default();
        let mut seen = vec![];
        let r = dl.download(&req, &stop, &mut |e| {
            if let DownloadEvent::Progress { done, .. } = e {
                seen.push(done);
                if done == 4 {
                    stop.stop();
                }
            }
        });
        assert!(matches!(r, Err(Error::Interrupted)));
        assert!(!d.path().join(MOCK_EXE).exists());
        let mut rest = vec![];
        dl.download(&req, &StopFlag::default(), &mut |e| {
            if let DownloadEvent::Progress { done, .. } = e {
                rest.push(done)
            }
        })
        .unwrap();
        assert_eq!((seen, rest), (vec![2, 4], vec![6, 8, 10]));
        assert!(d.path().join(MOCK_EXE).exists());
    }

    #[test]
    fn stage_names() {
        for s in [DownloadStage::Preparing, DownloadStage::Downloading, DownloadStage::Committing] {
            assert_eq!(DownloadStage::parse(s.as_str()), Some(s));
        }
    }
}
