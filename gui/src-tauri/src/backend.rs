//! Интерфейс между GUI и ядром carafe.
//!
//! Экраны знают только этот трейт. Сейчас его реализуют `MockBackend` (для разработки UI)
//! и `CliBackend` (обёртка над `bin/carafe`); позже сюда встанет `carafe-core`.
//!
//! Все методы блокирующие: GUI зовёт их из отдельных потоков. Долгие операции
//! (запуск, сборка моста, установка движка) шлют события в `Events` по ходу дела.

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
    pub bridge: BridgeState,
    pub cloud: CloudSummary,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CloudAction {
    Synced,
    Download,
    Upload,
}

/// Готовность окружения: список проверок для экрана первого запуска.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub checks: Vec<Check>,
    pub engines: Vec<EngineInfo>,
    pub bottles: Vec<String>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    CloudPull,
    Bridge,
    Game,
    CloudPush,
    Download,
    Unpack,
    Bottle,
    Build,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum StageStatus {
    Active,
    Done,
    Skipped,
    Failed { message: String },
}

pub trait Backend: Send + Sync {
    /// Человеческое имя реализации для настроек: «демо-данные», «bin/carafe».
    fn label(&self) -> String;

    fn environment(&self, profile: &Profile) -> Result<Environment>;

    fn games(&self, profile: &Profile) -> Result<Vec<Game>>;

    fn cloud_status(&self, appid: AppId, profile: &Profile) -> Result<CloudStatus>;

    /// Облако вниз → мост → игра → облако вверх. Возвращает код выхода игры.
    fn launch(&self, appid: AppId, profile: &Profile, events: Events) -> Result<i32>;

    fn build_bridge(&self, appid: AppId, profile: &Profile, events: Events) -> Result<()>;

    fn install_engine(&self, profile: &Profile, events: Events) -> Result<()>;

    fn create_bottle(&self, profile: &Profile, events: Events) -> Result<()>;

    /// Открыть Steam для Mac (мост и облако говорят с ним).
    fn open_steam(&self) -> Result<()>;

    /// Починить одну проверку окружения.
    fn fix(&self, check: CheckId, profile: &Profile, events: Events) -> Result<()> {
        match check {
            CheckId::Engine | CheckId::D3dMetal => self.install_engine(profile, events),
            CheckId::Bottle => self.create_bottle(profile, events),
            CheckId::Steam => self.open_steam(),
            CheckId::Rosetta | CheckId::Bridge => Err(BackendError::new("это не чинится кнопкой")),
        }
    }
}
