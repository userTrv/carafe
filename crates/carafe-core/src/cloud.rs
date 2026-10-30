//! Сейвы Steam Auto-Cloud через хелпер моста: `soju-steamd cloud status|pull|push`.
//! Steam для Mac Auto-Cloud с Windows-путями не обслуживает, поэтому правила `ufs` игры carafe
//! передаёт хелперу сам, а тот говорит с облаком через сервис `Cloud` клиента Steam.
//!
//! Хелпер сравнивает диск и облако с последней синхронизацией: поменялась одна сторона — едет
//! на другую, обе — конфликт (`  ! файл (конфликт: …)`, код [`CONFLICT_EXIT`]); его решает
//! [`Prefer`], проигравшая сторона уходит в backups.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::Carafe;
use crate::appinfo::CloudRule;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::game;
use crate::process::{self, Log, Stream};

/// Что синхронизировано в последний раз: `sha1\tимя` в папке игры, ведёт soju-steamd.
pub const CLOUD_STATE: &str = ".carafe-cloud-state";

/// Код выхода soju-steamd: сейвы менялись и на диске, и в облаке, ничего не перенесено.
pub const CONFLICT_EXIT: i32 = 3;

/// Чью сторону взять при конфликте; вторая уходит в backups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefer {
    Local,
    Cloud,
}

impl Prefer {
    pub fn as_str(self) -> &'static str {
        match self {
            Prefer::Local => "local",
            Prefer::Cloud => "cloud",
        }
    }

    pub fn parse(s: &str) -> Option<Prefer> {
        [Prefer::Local, Prefer::Cloud].into_iter().find(|p| p.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudMode {
    /// Показать, что куда поедет.
    Status,
    Pull,
    Push,
}

impl CloudMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CloudMode::Status => "status",
            CloudMode::Pull => "pull",
            CloudMode::Push => "push",
        }
    }

    pub fn parse(s: &str) -> Option<CloudMode> {
        [CloudMode::Status, CloudMode::Pull, CloudMode::Push].into_iter().find(|m| m.as_str() == s)
    }
}

impl fmt::Display for CloudMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudAction {
    /// `=` — одинаково локально и в облаке.
    Synced,
    /// `↓` — из облака на диск (в status — поедет, в pull — скачан).
    Download,
    /// `↑` — с диска в облако.
    Upload,
    /// `!` — менялись обе стороны: нужен выбор ([`Prefer`]).
    Conflict,
}

impl CloudAction {
    const ALL: [CloudAction; 4] =
        [CloudAction::Synced, CloudAction::Download, CloudAction::Upload, CloudAction::Conflict];

    pub fn symbol(self) -> &'static str {
        match self {
            CloudAction::Synced => "=",
            CloudAction::Download => "↓",
            CloudAction::Upload => "↑",
            CloudAction::Conflict => "!",
        }
    }
}

/// Одна сторона конфликта: когда менялась (ISO 8601 с поясом, как печатает soju-steamd) и размер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudSide {
    pub modified: String,
    pub size: u64,
}

impl CloudSide {
    /// `2026-10-30T13:42:59+03:00, 1234 Б`
    fn parse(s: &str) -> Option<CloudSide> {
        let (t, n) = s.trim().split_once(", ")?;
        let size = n.strip_suffix(" Б")?.parse().ok()?;
        (t.len() >= 19 && t.as_bytes()[10] == b'T').then(|| CloudSide { modified: t.into(), size })
    }
}

/// Строка отчёта soju-steamd: `  ↓ 7656…/stats.json (конфликт: …)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudEntry {
    /// Имя без корня и пути правила: подкаталог виден, одинаковые файлы в разных папках различимы.
    pub name: String,
    pub action: CloudAction,
    /// Пояснение в скобках — сейчас только про конфликт (менялись обе стороны).
    pub note: Option<String>,
}

impl CloudEntry {
    pub fn is_conflict(&self) -> bool {
        self.action == CloudAction::Conflict || self.note.as_deref().is_some_and(|n| n.starts_with("конфликт"))
    }

    /// Нерешённый конфликт: (на диске, в облаке) — из «конфликт: локально …; в облаке …».
    pub fn sides(&self) -> Option<(CloudSide, CloudSide)> {
        let n = self.note.as_deref()?.strip_prefix("конфликт: локально ")?;
        let (local, cloud) = n.split_once("; в облаке ")?;
        Some((CloudSide::parse(local)?, CloudSide::parse(cloud)?))
    }

    pub fn parse(line: &str) -> Option<CloudEntry> {
        let rest = line.strip_prefix("  ")?;
        let (sym, rest) = rest.split_once(' ')?;
        let action = CloudAction::ALL.into_iter().find(|a| a.symbol() == sym)?;
        let (name, note) = match rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")) {
            Some((n, note)) if note.starts_with("конфликт") => (n, Some(note.to_string())),
            _ => (rest, None),
        };
        (!name.is_empty()).then(|| CloudEntry { name: name.into(), action, note })
    }
}

impl fmt::Display for CloudEntry {
    /// Ровно как печатает soju-steamd.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "  {} {}", self.action.symbol(), self.name)?;
        if let Some(n) = &self.note {
            write!(f, " ({n})")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudReport {
    pub appid: u32,
    pub mode: CloudMode,
    /// Правила Auto-Cloud; пусто — синхронизировать нечего, хелпер не запускался.
    pub rules: Vec<CloudRule>,
    /// В status — все файлы, в pull/push — перенесённые.
    pub entries: Vec<CloudEntry>,
}

impl CloudReport {
    pub fn in_sync(&self) -> bool {
        self.entries.iter().all(|e| e.action == CloudAction::Synced)
    }
}

/// Дешёвая сводка для списка игр, без Steam: что известно из файлов carafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudSummary {
    /// `None` — правила ещё не узнавали (нужен steamcmd).
    pub rules: Option<Vec<CloudRule>>,
    /// Сколько файлов было синхронизировано в последний раз.
    pub synced_files: usize,
    pub last_sync: Option<SystemTime>,
}

pub(crate) fn summary(game_dir: &Path, rules: Option<Vec<CloudRule>>) -> CloudSummary {
    let state = game_dir.join(CLOUD_STATE);
    let text = fs::read_to_string(&state).unwrap_or_default();
    CloudSummary {
        rules,
        synced_files: text.lines().filter(|l| l.contains('\t')).count(),
        last_sync: fs::metadata(&state).and_then(|m| m.modified()).ok(),
    }
}

/// Корни Steam для Windows-игры в префиксе: куда указывают `%WinAppDataLocalLow%` и прочие.
pub fn cloud_roots(user_dir: &Path, game_dir: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("WinAppDataLocalLow", user_dir.join("AppData/LocalLow")),
        ("WinAppDataLocal", user_dir.join("AppData/Local")),
        ("WinAppDataRoaming", user_dir.join("AppData/Roaming")),
        ("WinMyDocuments", user_dir.join("Documents")),
        ("WinSavedGames", user_dir.join("Saved Games")),
        ("gameinstall", game_dir.into()),
    ]
}

/// Где хелпер держит состояние, копии и журнал.
pub struct SteamdPaths<'a> {
    pub state: &'a Path,
    pub backup: &'a Path,
    pub log: Option<&'a Path>,
}

/// Аргументы `soju-steamd cloud …`.
pub fn steamd_args(
    mode: CloudMode,
    prefer: Option<Prefer>,
    paths: &SteamdPaths<'_>,
    roots: &[(&str, PathBuf)],
    rules: &[CloudRule],
) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec!["cloud".into(), mode.as_str().into()];
    a.extend(["--state".into(), paths.state.into(), "--backup".into(), paths.backup.into()]);
    if let Some(log) = paths.log {
        a.extend(["--log".into(), log.into()]);
    }
    if let Some(p) = prefer {
        a.extend(["--prefer".into(), p.as_str().into()]);
    }
    for (name, dir) in roots {
        let mut r = OsString::from(format!("{name}="));
        r.push(dir);
        a.extend(["--root".into(), r]);
    }
    for r in rules {
        a.extend(["--rule".into(), r.to_string().into()]);
    }
    a
}

/// Пользователь Windows в префиксе (у CrossOver-Wine — `crossover`), кроме Public.
pub fn prefix_user(prefix: &Path) -> Option<PathBuf> {
    let mut users: Vec<PathBuf> = fs::read_dir(prefix.join("drive_c/users"))
        .ok()?
        .flatten()
        .filter(|e| e.file_name() != "Public" && e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    users.sort();
    users.into_iter().next()
}

impl Carafe {
    /// Синхронизация сейвов со Steam Cloud; конфликт решается по `CARAFE_CLOUD_PREFER`, если задан.
    /// Строки отчёта приходят в `on_entry` по мере работы хелпера. Нужен собранный мост и
    /// запущенный Steam для Mac. Нерешённый конфликт — [`Error::CloudConflict`].
    pub fn cloud_sync(
        &self,
        appid: u32,
        mode: CloudMode,
        on_entry: &mut dyn FnMut(&CloudEntry),
    ) -> Result<CloudReport> {
        self.cloud_sync_prefer(appid, mode, self.config.cloud_prefer, on_entry)
    }

    pub fn cloud_sync_prefer(
        &self,
        appid: u32,
        mode: CloudMode,
        prefer: Option<Prefer>,
        on_entry: &mut dyn FnMut(&CloudEntry),
    ) -> Result<CloudReport> {
        let game_dir = self.game_dir(appid);
        let dll = game::steam_api_dll(&game_dir).ok_or(Error::NoCloud)?;
        let steamd = self.bridge_dir(&dll.original)?.join("soju-steamd");
        if !fsutil::is_executable(&steamd) {
            return Err(Error::BridgeNotBuilt(appid));
        }
        let user = prefix_user(&self.config.prefix()).ok_or(Error::NoPrefixUser)?;
        let rules = self.cloud_rules(appid)?;
        let mut report = CloudReport { appid, mode, rules, entries: vec![] };
        if report.rules.is_empty() {
            self.log(Log::Step("у игры нет правил Auto-Cloud — синхронизировать нечего".into()));
            return Ok(report);
        }
        let (state, backup, log) =
            (game_dir.join(CLOUD_STATE), self.config.backups(appid), self.config.cloud_log(appid));
        let paths = SteamdPaths { state: &state, backup: &backup, log: Some(&log) };
        let args = steamd_args(mode, prefer, &paths, &cloud_roots(&user, &game_dir), &report.rules);
        self.log(Log::Step(format!("Steam Cloud: {mode}")));
        let mut cmd = std::process::Command::new(&steamd);
        cmd.args(args).env("SteamAppId", appid.to_string());
        let status = process::run_lines(&mut cmd, true, &mut |s, line| match (s, CloudEntry::parse(line)) {
            (Stream::Out, Some(e)) => {
                on_entry(&e);
                report.entries.push(e);
            }
            _ => self.log(Log::Output(line.into())),
        })?;
        if status.code() == Some(CONFLICT_EXIT) {
            return Err(Error::CloudConflict(report.entries.into_iter().filter(CloudEntry::is_conflict).collect()));
        }
        process::check(&cmd, status)?;
        Ok(report)
    }

    pub fn cloud_status(&self, appid: u32) -> Result<CloudReport> {
        self.cloud_sync(appid, CloudMode::Status, &mut |_| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;

    #[test]
    fn parse_lines() {
        let e = CloudEntry::parse("  = user-id/stats.json").unwrap();
        assert_eq!(e, CloudEntry { name: "user-id/stats.json".into(), action: CloudAction::Synced, note: None });
        let e = CloudEntry::parse("  ↓ saves/a b.sav (конфликт: новее — эта сторона)").unwrap();
        assert_eq!(e.action, CloudAction::Download);
        assert_eq!(e.name, "saves/a b.sav");
        assert!(e.is_conflict());
        assert_eq!(e.to_string(), "  ↓ saves/a b.sav (конфликт: новее — эта сторона)");
        let e = CloudEntry::parse("  ↑ x (1).json (конфликт: локальный новее, облачный — в backups)").unwrap();
        assert_eq!((e.name.as_str(), e.action), ("x (1).json", CloudAction::Upload));
        // Скобки в имени — не пояснение.
        let e = CloudEntry::parse("  ↑ slot (2).sav").unwrap();
        assert_eq!((e.name.as_str(), e.note), ("slot (2).sav", None));
        let e = CloudEntry::parse(
            "  ! 7656…/stats.json (конфликт: локально 2026-10-30T13:42:59+03:00, 1234 Б; в облаке 2026-10-29T19:56:34+03:00, 1200 Б)",
        )
        .unwrap();
        assert_eq!((e.name.as_str(), e.action), ("7656…/stats.json", CloudAction::Conflict));
        assert!(e.is_conflict());
        let (l, c) = e.sides().unwrap();
        assert_eq!(l, CloudSide { modified: "2026-10-30T13:42:59+03:00".into(), size: 1234 });
        assert_eq!(c, CloudSide { modified: "2026-10-29T19:56:34+03:00".into(), size: 1200 });
        // Решённый конфликт — без сторон.
        let e = CloudEntry::parse("  ↑ a.json (конфликт: оставлены локальные, облачные — в backups)").unwrap();
        assert!(e.is_conflict() && e.sides().is_none());
        for noise in ["Setting breakpad minidump AppID = 3405340", "  ? x", "  = ", "= x", ""] {
            assert_eq!(CloudEntry::parse(noise), None, "{noise:?}");
        }
    }

    #[test]
    fn args_like_bash() {
        let user = Path::new("/b/drive_c/users/crossover");
        let game = Path::new("/c/games/3405340");
        let rules = [CloudRule {
            root: "WinAppDataLocalLow".into(),
            path: "Ved/Megabonk/Saves/CloudDir".into(),
            pattern: "*.json".into(),
            recursive: true,
        }];
        let state = game.join(CLOUD_STATE);
        let paths = SteamdPaths { state: &state, backup: Path::new("/c/backups/3405340"), log: None };
        let a = steamd_args(CloudMode::Pull, None, &paths, &cloud_roots(user, game), &rules);
        let a: Vec<_> = a.iter().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(
            a,
            [
                "cloud",
                "pull",
                "--state",
                "/c/games/3405340/.carafe-cloud-state",
                "--backup",
                "/c/backups/3405340",
                "--root",
                "WinAppDataLocalLow=/b/drive_c/users/crossover/AppData/LocalLow",
                "--root",
                "WinAppDataLocal=/b/drive_c/users/crossover/AppData/Local",
                "--root",
                "WinAppDataRoaming=/b/drive_c/users/crossover/AppData/Roaming",
                "--root",
                "WinMyDocuments=/b/drive_c/users/crossover/Documents",
                "--root",
                "WinSavedGames=/b/drive_c/users/crossover/Saved Games",
                "--root",
                "gameinstall=/c/games/3405340",
                "--rule",
                "WinAppDataLocalLow:Ved/Megabonk/Saves/CloudDir:*.json:1",
            ]
        );
        let log = Path::new("/c/cloud-3405340.log");
        let paths = SteamdPaths { log: Some(log), ..paths };
        let a = steamd_args(CloudMode::Push, Some(Prefer::Local), &paths, &[], &[]);
        let a: Vec<_> = a.iter().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(&a[6..], ["--log", "/c/cloud-3405340.log", "--prefer", "local"]);
    }

    #[test]
    fn user_and_summary() {
        let d = TestDir::new();
        assert_eq!(prefix_user(d.path()), None);
        d.file("drive_c/users/Public/x", b"");
        assert_eq!(prefix_user(d.path()), None);
        d.file("drive_c/users/crossover/x", b"");
        d.file("drive_c/users/file", b"");
        assert_eq!(prefix_user(d.path()), Some(d.path().join("drive_c/users/crossover")));

        let s = summary(d.path(), None);
        assert_eq!((s.synced_files, s.last_sync), (0, None));
        d.file(CLOUD_STATE, b"aa\t%WinAppDataLocalLow%a\nbb\t%WinAppDataLocalLow%b\n");
        let s = summary(d.path(), Some(vec![]));
        assert_eq!(s.synced_files, 2);
        assert!(s.last_sync.is_some());
    }

    #[test]
    fn mode_names() {
        assert_eq!(CloudMode::parse("push"), Some(CloudMode::Push));
        assert_eq!(CloudMode::parse("sync"), None);
        assert_eq!(CloudMode::Status.to_string(), "status");
        assert_eq!(Prefer::parse("cloud"), Some(Prefer::Cloud));
        assert_eq!(Prefer::parse("newest"), None);
    }
}
