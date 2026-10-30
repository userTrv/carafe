//! Интерфейс между GUI и ядром carafe.
//!
//! Экраны знают только этот трейт. Сейчас его реализуют `MockBackend` (для разработки UI)
//! и `CliBackend` (обёртка над `bin/carafe` и `carafe-core`).
//!
//! Все методы блокирующие: GUI зовёт их из отдельных потоков. Долгие операции
//! (запуск, сборка моста, установка движка) шлют события в `Events` по ходу дела.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

pub type AppId = u32;

/// Ошибка, понятная пользователю: короткая фраза по-русски.
#[derive(Debug, Clone, Serialize)]
pub struct BackendError(pub String);

impl BackendError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BackendError {}

pub type Result<T> = std::result::Result<T, BackendError>;

/// Приёмник событий долгой операции.
pub type Events<'a> = &'a (dyn Fn(TaskEvent) + Sync);

/// Игра в библиотеке.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub appid: AppId,
    pub name: String,
    pub cover: Cover,
    pub installed: bool,
    pub size_bytes: Option<u64>,
    /// Папка библиотеки, где лежит игра.
    pub library: Option<String>,
    /// false — библиотека недоступна (внешний диск отключён): игра есть, но запустить нельзя.
    pub available: bool,
    pub bridge: BridgeState,
    pub cloud: CloudSummary,
    /// Windows через carafe или нативная macOS-сборка.
    pub platform: Platform,
    /// Архитектура нативной сборки: `arm64`, `x86_64` (через Rosetta), `universal`.
    pub arch: Option<String>,
}

/// Какая сборка игры стоит или будет стоять.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Platform {
    /// Windows-сборка через Wine (soju) — как всегда.
    #[default]
    Windows,
    /// Нативная сборка для macOS: без Wine, бутылки и моста.
    Macos,
}

/// Обложки из Steam CDN.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cover {
    /// 600×900, сетка библиотеки.
    pub portrait: String,
    /// 460×215, запасная, если вертикальной нет.
    pub header: String,
    /// 1920×620, фон карточки игры.
    pub hero: String,
}

impl Cover {
    pub fn for_app(appid: AppId) -> Self {
        let base = format!("https://cdn.cloudflare.steamstatic.com/steam/apps/{appid}");
        Self {
            portrait: format!("{base}/library_600x900.jpg"),
            header: format!("{base}/header.jpg"),
            hero: format!("{base}/library_hero.jpg"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BridgeState {
    /// Мост собран: ачивки и лидерборды работают.
    Ready,
    /// Игре нужен мост, но он не собран.
    NotBuilt,
    /// В игре нет `steam_api64.dll` — мост не нужен.
    NotNeeded,
    Unknown,
}

/// Быстрая сводка по облаку для сетки — без похода в Steam.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSummary {
    pub state: CloudState,
    /// Unix-время последней синхронизации, секунды.
    pub last_sync: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CloudState {
    Synced,
    /// Есть файлы, которые поедут в одну из сторон.
    Pending,
    Conflict,
    /// Ещё ни разу не синхронизировали.
    Never,
    /// У игры нет Steam Cloud (или нет моста, через который с ним говорить).
    Unavailable,
    Unknown,
}

/// Подробный статус облака: что куда поедет. Требует запущенный Steam для Mac.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStatus {
    pub files: Vec<CloudFile>,
    pub last_sync: Option<u64>,
    /// Короткое пояснение, если файлов нет: «нет правил Auto-Cloud» и т. п.
    pub note: Option<String>,
}

impl CloudStatus {
    pub fn summary(&self) -> CloudState {
        if self.files.iter().any(|f| f.conflict) {
            CloudState::Conflict
        } else if self.files.iter().any(|f| f.action != CloudAction::Synced) {
            CloudState::Pending
        } else if self.files.is_empty() {
            CloudState::Unavailable
        } else {
            CloudState::Synced
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudFile {
    /// Имя относительно каталога сейвов, без Steam ID.
    pub name: String,
    pub action: CloudAction,
    pub conflict: bool,
    /// Нерешённый конфликт: версия на диске и в облаке.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<CloudSide>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloud: Option<CloudSide>,
}

impl CloudFile {
    pub fn new(name: impl Into<String>, action: CloudAction, conflict: bool) -> Self {
        Self {
            name: name.into(),
            action,
            conflict,
            local: None,
            cloud: None,
        }
    }
}

/// Одна сторона конфликта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSide {
    /// ISO 8601 с поясом: `2026-10-30T13:42:59+03:00`.
    pub modified: String,
    pub size: u64,
}

/// Чьи сейвы оставить при конфликте; вторые уходят в резервную копию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
}

/// Как запускать игру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchOptions {
    /// Решение конфликта сейвов, выбранное в окне.
    pub prefer: Option<Prefer>,
    /// Скачать сейвы до игры. Нет — пока идёт выгрузка прошлого запуска: локальные новее.
    pub pull: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            prefer: None,
            pull: true,
        }
    }
}

/// Итог запуска. Выгрузку после игры GUI делает сам, в фоне (`Backend::cloud_push`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Launched {
    /// Код выхода игры.
    pub code: i32,
    /// Конфликт сейвов: игра не запускалась, нужен выбор (`LaunchOptions::prefer`).
    pub conflict: bool,
    /// После игры нужно выгрузить сейвы.
    pub push: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CloudAction {
    Synced,
    Download,
    Upload,
    /// Менялись и диск, и облако.
    Conflict,
}

/// Готовность окружения: список проверок для экрана первого запуска.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub checks: Vec<Check>,
    pub engines: Vec<EngineInfo>,
    pub bottles: Vec<String>,
    /// Активный движок; `None` — бэкенд не знает.
    pub engine: Option<EngineState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineKind {
    /// soju из релизов userTrv/soju — по умолчанию.
    Soju,
    /// GPTK 3.0 от Gcenx: запасной движок и источник D3DMetal.
    Gptk,
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Soju => "soju",
            EngineKind::Gptk => "gptk",
        }
    }
}

/// Активный движок и что с ним можно сделать — для «Окружения».
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineState {
    /// «soju 26.3.0», «GPTK 3.0».
    pub label: String,
    pub kind: Option<EngineKind>,
    /// Активный движок на месте.
    pub ready: bool,
    pub d3dmetal: bool,
    /// Движок выбран в настройках (свой каталог), кнопки смены движка не к месту.
    pub custom: bool,
    /// Установленный soju: «soju 26.3.0».
    pub soju: Option<String>,
    pub gptk: Option<String>,
    /// Пользователь до soju (GPTK и бутылки): предложить перейти.
    pub suggest_soju: bool,
}

impl Environment {
    pub fn ready(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: CheckId,
    pub ok: bool,
    /// Можно ли починить кнопкой (`Backend::fix`).
    pub fixable: bool,
    /// Подсказка по месту, когда кнопкой не починить.
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckId {
    Rosetta,
    Engine,
    D3dMetal,
    Bottle,
    /// Мост Steamworks из soju.
    Bridge,
    Steam,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    /// Идентификатор для настроек; путь к каталогу движка.
    pub id: String,
    pub name: String,
    pub d3dmetal: bool,
}

/// То, что пользователь выбрал в настройках; передаётся в каждую операцию.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub bottle: String,
    /// `EngineInfo::id`; `None` — движок по умолчанию.
    pub engine: Option<String>,
    pub hud: bool,
    pub bridge: bool,
    pub cloud: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            bottle: "default".into(),
            engine: None,
            hud: false,
            bridge: true,
            cloud: true,
        }
    }
}

/// Событие долгой операции.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TaskEvent {
    Stage {
        stage: Stage,
        status: StageStatus,
    },
    /// Строка лога. Steam ID вырезает GUI до показа.
    Log {
        line: String,
    },
    /// Доля 0..1 для шагов с известным объёмом (скачивание движка).
    Progress {
        stage: Stage,
        fraction: f32,
    },
    CloudFile {
        file: CloudFile,
    },
    /// Сейвы разошлись: менялись и диск, и облако. Ничего не перенесено, нужен выбор.
    CloudConflict {
        files: Vec<CloudFile>,
    },
    /// Вход в Steam: нарисовать QR из `url`. В окно уходит уже SVG (`main.rs`), не сам URL.
    Qr {
        url: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    CloudPull,
    Bridge,
    Game,
    CloudPush,
    Download,
    /// Перенос игры в другую библиотеку.
    Move,
    Unpack,
    Bottle,
    Build,
    /// Смена движка: резервные копии бутылок и `wineboot -u`.
    Migrate,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum StageStatus {
    Active,
    Done,
    Skipped,
    Failed { message: String },
}

/// Папка библиотеки с местом на диске.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub path: String,
    /// Короткое имя: «Встроенная», имя тома или папки.
    pub name: String,
    pub builtin: bool,
    pub default: bool,
    pub available: bool,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    /// Сколько занимают игры.
    pub used_bytes: u64,
    pub games: u32,
}

/// Загрузка в очереди.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Download {
    pub appid: AppId,
    pub name: String,
    pub cover: Cover,
    pub library: String,
    pub kind: DownloadKind,
    pub state: DownloadState,
    pub stage: Option<DownloadStage>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// Байт в секунду.
    pub speed: Option<u64>,
    pub eta_secs: Option<u64>,
    pub error: Option<String>,
    pub platform: Platform,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadKind {
    Install,
    Update,
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadState {
    Queued,
    Active,
    Paused,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadStage {
    Preparing,
    Allocating,
    Downloading,
    Verifying,
    Committing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DownloadAction {
    Pause,
    Resume,
    Cancel,
    /// В начало очереди.
    Top,
    Up,
    Down,
}

/// Аккаунт Steam, под которым вошли через `carafe-steam` (QR из приложения Steam).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteamAccount {
    pub account_name: String,
    /// Только маской: `••••1234`.
    pub steam_id: Option<String>,
}

/// Игра из лицензий аккаунта — для полной библиотеки, вместе с неустановленными.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnedGame {
    pub appid: AppId,
    pub name: String,
    pub cover: Cover,
    pub installed: bool,
    /// Есть ли Windows-сборка.
    pub windows: bool,
    pub size_bytes: Option<u64>,
    /// Есть ли нативная сборка для macOS (её можно поставить без Wine).
    pub macos: bool,
    pub macos_size_bytes: Option<u64>,
}

/// Что Steam знает об игре на этом аккаунте. Чего нет, то `None`: блок на экране не рисуется.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStats {
    pub playtime: Option<Playtime>,
    pub achievements: Option<Achievements>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playtime {
    pub minutes: u64,
    pub minutes_2weeks: u64,
    /// Unix-время последнего запуска, секунды.
    pub last_played: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Achievements {
    pub total: u32,
    pub unlocked: u32,
    pub items: Vec<Achievement>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Achievement {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub icon_gray: Option<String>,
    pub hidden: bool,
    pub unlocked: bool,
    pub unlocked_at: Option<i64>,
    /// Доля игроков, открывших достижение, %.
    pub global_percent: Option<f64>,
}

fn unsupported<T>() -> Result<T> {
    Err(BackendError::new("не поддерживается этим ядром"))
}

pub trait Backend: Send + Sync {
    /// Человеческое имя реализации для настроек: «демо-данные», «bin/carafe».
    fn label(&self) -> String;

    fn environment(&self, profile: &Profile) -> Result<Environment>;

    fn games(&self, profile: &Profile) -> Result<Vec<Game>>;

    fn cloud_status(&self, appid: AppId, profile: &Profile) -> Result<CloudStatus>;

    /// Облако вниз → мост → игра. Выгрузку после игры (`Launched::push`) GUI запускает сам,
    /// в фоне, через `cloud_push`: окно и повторный запуск её не ждут.
    fn launch(
        &self,
        appid: AppId,
        profile: &Profile,
        opts: &LaunchOptions,
        events: Events,
    ) -> Result<Launched>;

    /// Выгрузить сейвы в Steam Cloud (этап `CloudPush`). Конфликт — событие `CloudConflict`.
    fn cloud_push(&self, appid: AppId, profile: &Profile, events: Events) -> Result<()>;

    fn build_bridge(&self, appid: AppId, profile: &Profile, events: Events) -> Result<()>;

    /// Поставить движок по умолчанию — последний soju.
    fn install_engine(&self, profile: &Profile, events: Events) -> Result<()>;

    /// Поставить GPTK: запасной движок и источник D3DMetal для soju.
    fn install_gptk(&self, _profile: &Profile, _events: Events) -> Result<()> {
        unsupported()
    }

    /// Сменить движок: бутылки сначала копируются рядом, потом обновляются новым движком.
    fn use_engine(&self, _kind: EngineKind, _profile: &Profile, _events: Events) -> Result<()> {
        unsupported()
    }

    fn create_bottle(&self, profile: &Profile, events: Events) -> Result<()>;

    /// Открыть Steam для Mac (мост и облако говорят с ним).
    fn open_steam(&self) -> Result<()>;

    /// Папки библиотек: встроенная первой.
    fn libraries(&self) -> Result<Vec<Library>> {
        unsupported()
    }

    fn add_library(&self, _path: &Path) -> Result<()> {
        unsupported()
    }

    /// Убрать из списка; файлы не трогаются.
    fn remove_library(&self, _path: &Path) -> Result<()> {
        unsupported()
    }

    fn set_default_library(&self, _path: &Path) -> Result<()> {
        unsupported()
    }

    /// Поставить игру в очередь загрузок: в `library` или в папку по умолчанию; `platform` —
    /// какую сборку (уже установленная обновляется той, что стоит).
    fn install(&self, _appid: AppId, _library: Option<&Path>, _platform: Platform) -> Result<()> {
        unsupported()
    }

    /// Перенести игру в другую библиотеку. Прогресс — `Progress { stage: Move }`; `cancel` —
    /// остановить, исходник остаётся целым.
    fn move_game(
        &self,
        _appid: AppId,
        _library: &Path,
        _cancel: &AtomicBool,
        _events: Events,
    ) -> Result<()> {
        unsupported()
    }

    fn downloads(&self) -> Result<Vec<Download>> {
        Ok(Vec::new())
    }

    fn download_action(&self, _appid: AppId, _action: DownloadAction) -> Result<()> {
        unsupported()
    }

    /// Под каким аккаунтом Steam вошли; без сети. `None` — не входили.
    fn steam_account(&self) -> Result<Option<SteamAccount>> {
        Ok(None)
    }

    /// Вход по QR: события `Qr { url }` (и новые, если Steam сменит код). Ждёт подтверждения
    /// в приложении Steam; `cancel` — бросить вход.
    fn steam_login(&self, _cancel: &AtomicBool, _events: Events) -> Result<SteamAccount> {
        unsupported()
    }

    /// Выйти: отозвать вход в Steam и забыть токен.
    fn steam_logout(&self) -> Result<()> {
        unsupported()
    }

    /// Все купленные игры аккаунта (сеть, несколько секунд).
    fn owned_games(&self) -> Result<Vec<OwnedGame>> {
        unsupported()
    }

    /// Время в игре и достижения (сеть, несколько секунд; реализация кэширует). `None` —
    /// не вошли в Steam: экран просто не показывает эти блоки. `fresh` — мимо кэша.
    fn game_stats(&self, _appid: AppId, _fresh: bool) -> Result<Option<GameStats>> {
        Ok(None)
    }

    /// Выход из GUI: остановить фоновые загрузки (продолжатся при следующем запуске).
    fn shutdown(&self) {}

    /// Починить одну проверку окружения.
    fn fix(&self, check: CheckId, profile: &Profile, events: Events) -> Result<()> {
        match check {
            CheckId::Engine => self.install_engine(profile, events),
            // D3DMetal для soju берётся из GPTK; у GPTK без D3DMetal — переустановка.
            CheckId::D3dMetal => self.install_gptk(profile, events),
            CheckId::Bottle => self.create_bottle(profile, events),
            CheckId::Steam => self.open_steam(),
            CheckId::Rosetta | CheckId::Bridge => Err(BackendError::new("это не чинится кнопкой")),
        }
    }
}
