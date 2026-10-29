//! Ядро carafe — запускателя Windows-игр из Steam на macOS (Apple Silicon): движок Wine +
//! D3DMetal, бутылки, игры через steamcmd, мост Steamworks из soju и Steam Cloud.
//!
//! Всё начинается с [`Carafe`]: настройки ([`Config`]) и приёмник сообщений ([`Log`]). CLI
//! печатает сообщения в stderr, GUI может забирать их себе через [`Carafe::with_logger`].
//!
//! Аккаунт Steam ([`account`]) — через хелпер `carafe-steam` на SteamKit2 (`steam/`): вход по QR,
//! купленные игры, загрузка депо. Токен — в Связке ключей ([`Keychain`]).

pub mod account;
pub mod appinfo;
pub mod bridge;
pub mod cloud;
pub mod config;
pub mod engine;
pub mod error;
mod fsutil;
pub mod game;
pub mod launch;
mod process;
pub mod steam;
pub mod steamclient;
pub mod vdf;

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

pub use account::{Keychain, MemoryStore, OwnedGame, TokenStore};
pub use appinfo::{AppInfo, CloudRule};
pub use bridge::{BridgeState, BridgeTarget};
pub use cloud::{CloudAction, CloudEntry, CloudMode, CloudReport, CloudSummary};
pub use config::{Config, parse_appid};
pub use error::{Error, Result};
pub use game::SteamApiDll;
pub use launch::LaunchEvent;
pub use process::{Log, exit_code};
pub use steamclient::{Account, AppDepots, Credentials, DownloadEvent, DownloadResult, LoginEvent, OwnedApp};

pub type Logger = Arc<dyn Fn(Log) + Send + Sync>;

/// carafe с настройками. Дешёво клонируется, можно держать в состоянии Tauri.
#[derive(Clone)]
pub struct Carafe {
    pub config: Config,
    logger: Logger,
    /// Где лежит refresh token Steam: по умолчанию Связка ключей.
    tokens: Arc<dyn TokenStore>,
}

impl std::fmt::Debug for Carafe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Carafe").field("config", &self.config).finish_non_exhaustive()
    }
}

/// Сообщения в stderr, как у bash-версии.
pub fn stderr_logger(l: Log) {
    match l {
        Log::Step(s) => eprintln!("==> {s}"),
        Log::Output(s) => eprintln!("{s}"),
    }
}

/// Установленная игра — для списка в GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInfo {
    pub appid: u32,
    /// Имя из appinfo; `None`, пока appinfo не загружен ([`Carafe::app_info`]).
    pub name: Option<String>,
    pub dir: PathBuf,
    pub exe: Option<PathBuf>,
    pub steam_api: Option<SteamApiDll>,
    pub bridge: BridgeState,
    pub cloud: CloudSummary,
}

impl Carafe {
    pub fn new(config: Config) -> Carafe {
        Carafe { config, logger: Arc::new(stderr_logger), tokens: Arc::new(Keychain) }
    }

    /// Настройки из переменных `CARAFE_*`.
    pub fn from_env() -> Result<Carafe> {
        Config::from_env().map(Carafe::new)
    }

    pub fn with_logger(mut self, f: impl Fn(Log) + Send + Sync + 'static) -> Carafe {
        self.logger = Arc::new(f);
        self
    }

    /// Своё хранилище токена Steam вместо Связки ключей (тесты, демо).
    pub fn with_token_store(mut self, store: Arc<dyn TokenStore>) -> Carafe {
        self.tokens = store;
        self
    }

    pub fn log(&self, l: Log) {
        (self.logger)(l)
    }

    /// Команда с stderr в лог построчно (stdout — как есть).
    pub(crate) fn run_logged(&self, cmd: &mut Command) -> Result<()> {
        let status = process::run_lines(cmd, false, &mut |_, line| self.log(Log::Output(line.into())))?;
        process::check(cmd, status)
    }

    /// Установленные игры (папки `games/<appid>`), по APPID. Без сети и Steam: имя и правила
    /// облака — из кешей, если они уже есть.
    pub fn games(&self) -> Result<Vec<GameInfo>> {
        let games = self.config.games();
        let mut ids: Vec<u32> = match fs::read_dir(&games) {
            Ok(rd) => rd
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| e.file_name().to_str()?.parse().ok())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(source) => return Err(Error::Io { path: games, source }),
        };
        ids.sort_unstable();
        ids.into_iter().map(|id| self.game(id)).collect()
    }

    pub fn game(&self, appid: u32) -> Result<GameInfo> {
        let dir = self.config.game_dir(appid);
        if !dir.is_dir() {
            return Err(Error::NoSuchDir(dir));
        }
        let steam_api = game::steam_api_dll(&dir);
        Ok(GameInfo {
            appid,
            name: self.cached_app_info(appid).and_then(|i| i.name),
            exe: game::find_game_exe(&dir, self.config.exe.as_deref()).filter(|e| e.is_file()),
            bridge: self.bridge_state(steam_api.as_ref())?,
            cloud: cloud::summary(&dir, self.cached_cloud_rules(appid)),
            steam_api,
            dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;

    #[test]
    fn list_games() {
        let home = TestDir::new();
        let mut cfg = Config::from_vars(|k| (k == "HOME").then(|| "/h".into())).unwrap();
        cfg.home = home.path().into();
        let c = Carafe::new(cfg).with_logger(|_| {});
        assert_eq!(c.games().unwrap(), []);

        home.file("games/3405340/Megabonk.exe", &[0; 10]);
        home.file("games/3405340/Megabonk_Data/Plugins/x86_64/steam_api64.dll", b"valve");
        home.file("games/3405340/.carafe-ufs", b"WinAppDataLocalLow:Ved/Megabonk/Saves/CloudDir:*.json:1\n");
        home.file("games/3405340/.carafe-appinfo.vdf", include_bytes!("../tests/data/app_info_print-3405340.txt"));
        home.file("games/480/readme.txt", b"");
        home.file("games/notes/x", b"");
        let g = c.games().unwrap();
        assert_eq!(g.iter().map(|g| g.appid).collect::<Vec<_>>(), [480, 3405340]);
        assert_eq!((g[0].name.as_deref(), &g[0].exe, &g[0].bridge), (None, &None, &BridgeState::NotNeeded));
        let m = &g[1];
        assert_eq!(m.name.as_deref(), Some("Megabonk"));
        assert_eq!(m.exe.as_deref(), Some(home.path().join("games/3405340/Megabonk.exe").as_path()));
        assert_eq!(m.bridge, BridgeState::NotBuilt);
        assert_eq!(m.cloud.rules.as_ref().map(Vec::len), Some(1));
        assert!(matches!(c.game(1), Err(Error::NoSuchDir(_))));
    }
}
