use std::path::PathBuf;
use std::process::ExitStatus;

use crate::vdf::VdfError;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Ошибки ядра. Тексты — те же, что печатал bash-скрипт: CLI выводит их как `carafe: <текст>`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("не задан HOME")]
    NoHome,
    #[error("{0}")]
    Usage(&'static str),
    #[error("APPID — число, а не «{0}»")]
    BadAppId(String),

    #[error("движок не установлен: carafe engine install")]
    EngineMissing,
    #[error("не нашёл bin/wine в {}", .0.display())]
    WineNotFound(PathBuf),
    #[error("sha256 архива не совпал")]
    EngineChecksum,

    #[error("не нашёл exe в {} (задай CARAFE_EXE)", .0.display())]
    ExeNotFound(PathBuf),
    #[error("нет файла {}", .0.display())]
    NoSuchFile(PathBuf),
    #[error("нет папки {}", .0.display())]
    NoSuchDir(PathBuf),
    #[error("не нашёл app_{0} в steamapps/content — скачай depot через консоль Steam или укажи папку")]
    DepotNotFound(u32),
    #[error("нет appinfo для {0} в выводе steamcmd")]
    NoAppInfo(u32),

    #[error("нужен soju: git clone https://github.com/userTrv/soju рядом с carafe (или CARAFE_SOJU)")]
    SojuMissing,
    #[error("в игре нет steam_api64.dll — мост не нужен")]
    NoBridgeNeeded,
    #[error("в игре нет steam_api64.dll — облака у неё нет")]
    NoCloud,
    #[error("сначала собери мост: carafe bridge build {0}")]
    BridgeNotBuilt(u32),
    #[error("в префиксе нет пользователя")]
    NoPrefixUser,

    #[error("нет хелпера carafe-steam: собери steam/build.sh (или задай CARAFE_STEAM_HELPER)")]
    SteamHelperMissing,
    #[error("не вошёл в Steam: carafe login")]
    NotLoggedIn,
    #[error("вход в Steam больше не действует — войди заново: carafe login")]
    TokenExpired,
    /// Ошибка из ответа хелпера: код протокола (`network`, `not_owned`…) и текст.
    #[error("Steam: {message}")]
    Steam { code: String, message: String },
    #[error("carafe-steam: {0}")]
    SteamProtocol(String),
    #[error("Связка ключей: {0}")]
    Keychain(String),
    #[error("отменено")]
    Cancelled,

    #[error(transparent)]
    Vdf(#[from] VdfError),
    #[error("{}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("не запустил {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// Внешняя программа завершилась с ошибкой; что не так, она обычно уже напечатала сама.
    #[error("{program} завершился с ошибкой ({status})")]
    Failed { program: String, status: ExitStatus },
}

impl Error {
    /// Код выхода для CLI: у упавшей внешней программы — её собственный, как с `set -e` в bash.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Failed { status, .. } => crate::process::exit_code(*status).max(1),
            _ => 1,
        }
    }
}

/// `io::Error` с путём: без него «No such file or directory» ничего не говорит.
pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::Io { path: path.into(), source })
    }
}
