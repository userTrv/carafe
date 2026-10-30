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
    #[error("размер архива не совпал: ждали {}, скачано {}", crate::fsutil::human_bytes(*.want), crate::fsutil::human_bytes(*.got))]
    EngineSize { want: u64, got: u64 },
    #[error("движок «{0}»: бывает soju или gptk")]
    UnknownEngine(String),
    #[error("{name} не установлен: carafe engine install{}", if *.name == "soju" { "" } else { " gptk" })]
    EngineNotInstalled { name: &'static str },
    #[error("в архиве движка нет {}", .0.display())]
    EngineBroken(PathBuf),
    #[error("engine.json: {0}")]
    BadManifest(String),
    #[error("в {0} нет релизов движка (engine-v*)")]
    NoEngineRelease(&'static str),
    #[error("GitHub ограничил запросы без токена{}", .0.map(|m| format!(" — повтори через {m} мин")).unwrap_or_else(|| " — повтори позже".into()))]
    GithubRateLimit(Option<u64>),
    #[error("GitHub: {0}")]
    Github(String),

    #[error("в бутылке {0} что-то запущено (жив её wineserver) — закрой игру и повтори")]
    BottleBusy(String),
    #[error("нет бутылки {0}")]
    NoBottle(String),
    #[error("у бутылки {0} нет резервных копий")]
    NoBackup(String),
    #[error("{what} не закончил за {secs} с")]
    Timeout { what: &'static str, secs: u64 },

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
    /// soju-steamd вышел с кодом 3: сейвы менялись и на диске, и в облаке, ничего не перенесено.
    #[error("конфликт сейвов: менялись и диск, и облако — выбери --prefer local или --prefer cloud")]
    CloudConflict(Vec<crate::cloud::CloudEntry>),

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
    #[error("игра {0} не установлена")]
    NotInstalled(u32),
    #[error("{} — не папка библиотеки (carafe library list)", .0.display())]
    NotALibrary(PathBuf),
    #[error("{} уже в библиотеках", .0.display())]
    LibraryExists(PathBuf),
    #[error("пересекается с библиотекой {}", .0.display())]
    LibraryNested(PathBuf),
    #[error("встроенную библиотеку убрать нельзя")]
    LibraryBuiltin,
    #[error("в {} есть игры — сначала перенеси их", .0.display())]
    LibraryNotEmpty(PathBuf),
    #[error("библиотека недоступна (диск отключён?): {}", .0.display())]
    LibraryUnavailable(PathBuf),
    #[error("игра {appid} уже установлена в {}", .lib.display())]
    InstalledElsewhere { appid: u32, lib: PathBuf },
    #[error("игра уже в {}", .0.display())]
    AlreadyThere(PathBuf),
    #[error("уже есть {}", .0.display())]
    TargetExists(PathBuf),
    #[error("мало места: нужно {}, свободно {}", crate::fsutil::human_bytes(*.need), crate::fsutil::human_bytes(*.free))]
    NoSpace { need: u64, free: u64 },
    #[error("прервано")]
    Interrupted,
    #[error("{0} уже в очереди загрузок")]
    AlreadyQueued(u32),
    #[error("{0} нет в очереди загрузок")]
    NotQueued(u32),
    #[error("очередь загрузок уже обрабатывает другой процесс carafe")]
    QueueBusy,
    #[error("steamcmd: {0}")]
    SteamCmd(String),
    #[error("нужен вход в steamcmd: carafe steam login USER")]
    NoSteamLogin,

    #[error("не нашёл .app в {}", .0.display())]
    NativeAppNotFound(PathBuf),
    #[error("игре нужен запущенный Steam для Mac: открой Steam и запусти игру снова")]
    NativeNeedsSteam,
    #[error("это Intel-сборка, а Rosetta не установлена: softwareupdate --install-rosetta")]
    RosettaMissing,
    #[error("игра {appid} уже стоит как {}-сборка", .installed.label())]
    PlatformMismatch { appid: u32, installed: crate::native::Platform },
    #[error("у игры {0} нет сборки для macOS")]
    NoMacBuild(u32),
    #[error("нативная установка — только через вход carafe login (steamcmd не используется)")]
    NativeNeedsLogin,
    #[error("хелпер carafe-steam не умеет macOS-сборки — пересобери steam/build.sh")]
    HelperTooOld,

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
            Error::CloudConflict(_) => crate::cloud::CONFLICT_EXIT,
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
