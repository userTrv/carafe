//! Правдоподобные данные и задержки для разработки UI. Ничего не запускает и не пишет на диск.
//!
//! `CARAFE_MOCK=fresh` — первый запуск: нет движка, бутылки и запущенного Steam. Без него —
//! пользователь до soju: стоит и активен GPTK, soju можно поставить и перейти на него.
//! `CARAFE_MOCK_ENGINE=soju` — soju уже стоит и активен.
//!
//! Библиотеки: встроенная, внешний SSD (по умолчанию) и отключённый архив с Hollow Knight.
//! Очередь: Elden Ring качается на SSD, Baldur's Gate 3 ждёт; загрузку ведёт свой поток.
//! `CARAFE_MOCK_QUEUE=empty` — очередь пуста; `=finishing` — Elden Ring докачивается за секунды.
//!
//! После входа в Steam (`CARAFE_MOCK_STEAM=in`) у установленных игр есть время в игре и
//! достижения (`STATS`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::backend::*;

const SOJU: &str = "soju 26.3.0";
const GPTK: &str = "GPTK 3.0";

struct MockGame {
    appid: AppId,
    name: &'static str,
    installed: bool,
    /// Индекс в `State::libraries`.
    library: usize,
    size_gb: f64,
    bridge: BridgeState,
    cloud: CloudState,
    last_sync_ago: Option<u64>,
    files: &'static [(&'static str, CloudAction, bool)],
}

struct MockLibrary {
    path: String,
    name: String,
    builtin: bool,
    available: bool,
    total_gb: f64,
    /// Занято не играми carafe.
    other_gb: f64,
}

struct MockJob {
    appid: AppId,
    library: usize,
    kind: DownloadKind,
    state: DownloadState,
    stage: DownloadStage,
    done: u64,
    total: u64,
    error: Option<String>,
}

struct State {
    /// Установлены soju и GPTK; какой активен.
    soju: bool,
    gptk: bool,
    active: Option<EngineKind>,
    bottle: bool,
    steam: bool,
    games: Vec<MockGame>,
    libraries: Vec<MockLibrary>,
    default_library: usize,
    jobs: Vec<MockJob>,
    /// Вход в Steam: имя аккаунта. `CARAFE_MOCK_STEAM=in` — сразу вошёл.
    account: Option<&'static str>,
}

/// Игры аккаунта, которых нет среди установленных: появляются в библиотеке после входа.
/// (appid, имя, ГБ, есть ли Windows-сборка)
const OWNED_EXTRA: &[(AppId, &str, f64, bool)] = &[
    (1091500, "Cyberpunk 2077", 65.0, true),
    (1030300, "Hollow Knight: Silksong", 7.5, true),
    (620, "Portal 2", 12.8, true),
    (220, "Half-Life 2", 6.5, true),
    (632470, "Disco Elysium", 20.0, true),
    (504230, "Celeste", 1.2, true),
    (105600, "Terraria", 0.5, true),
    (1145360, "Hades", 15.0, true),
    (892970, "Valheim", 1.7, true),
    (646570, "Slay the Spire", 1.1, true),
    (2420110, "Horizon Forbidden West", 150.0, true),
    (1817070, "Marvel's Spider-Man Remastered", 75.0, true),
    // Только Mac/Linux: ставить нечего.
    (1, "Только для Mac (демо)", 0.0, false),
];

/// (appid, минут всего, за 2 недели, сколько назад запускали, достижений всего, открыто)
type MockStats = (AppId, u64, u64, Option<u64>, u32, u32);
const STATS: &[MockStats] = &[
    (3405340, 754, 95, Some(2 * HOUR), 24, 9),
    (1145350, 3120, 0, Some(40 * DAY), 49, 31),
    (2379780, 2410, 310, Some(3 * DAY), 30, 8),
    (1794680, 1880, 0, Some(9 * DAY), 0, 0),
    (413150, 0, 0, None, 40, 0),
    (367520, 4290, 0, Some(5 * DAY), 63, 63),
];

/// Публичная схема достижений Megabonk из Steam (названия, иконки, доля игроков): демо с
/// настоящими картинками. (название, описание, иконка, серая иконка, %)
const MEGABONK_ACH: &[(&str, &str, &str, &str, f64)] = &[
    (
        "Battery",
        "Upgrade Cooldown Tome to Level 5",
        "4c378fd9006af550a537466bc7a9ff876c05ef4c.jpg",
        "0b6b4bf4f9d602343ca56675d27c0ed8b1a18505.jpg",
        86.4,
    ),
    (
        "Boss Buster",
        "Defeat a Stage Boss in under 30 seconds",
        "c1939ca13ee36238e54bf67491d5fe193c32e667.jpg",
        "1eb8a411706dea943e12a2299180c44d49f510a9.jpg",
        80.8,
    ),
    (
        "Cactus",
        "Kill 100 cactus enemies with Thorns damage",
        "babb79a9a15b3bb5921ea39c89525247c99e46ae.jpg",
        "90142ee4cc135e16d787c9c393e4b954d91cf94e.jpg",
        4.4,
    ),
    (
        "Cursed Doll",
        "Die 10 times",
        "e5338e8c47af9398344fbbbb77050c2061460b86.jpg",
        "8b2f3f99e73abb31f6e1e10ceed6534b9a3d0c5c.jpg",
        86.1,
    ),
    (
        "Forbidden Juice",
        "Crit 100 times",
        "2a8ca52617ed0164206863d6517d971985b871a7.jpg",
        "9c6e12390dcdf9609f4630cd227633e71c4e4ae6.jpg",
        95.3,
    ),
    (
        "Brass Knuckles",
        "Kill 5 000 enemies with the Sword",
        "c9cdd7b9a8725111875fcc84407c2aacc254119a.jpg",
        "f9843271304a929c25fd90da2d9d5cbd267797ed.jpg",
        56.5,
    ),
    (
        "Bob (Dead)",
        "Survive the Final Swarm for 2 minues",
        "1c283d91c5119bc8d4ef3e4a27b45c0b4850e581.jpg",
        "f4597556aff71c7fd63100e3f6289af1067b99e4.jpg",
        60.1,
    ),
    (
        "Anvil",
        "Complete 3 Challenges",
        "e10a00d616712e5f7fa362e7fdf8ec16f87f999c.jpg",
        "5077069d8dac9abf522a7470e4e7a1a9a4925c76.jpg",
        37.1,
    ),
    (
        "Ghost",
        "Survive the Final Swarm for 60s",
        "322999a0920ffc6ddf67cd0a505f333cf80ab267.jpg",
        "17919ba5484b96dde3c4013d9b76addff3116688.jpg",
        96.0,
    ),
    (
        "Ice Crystal",
        "Kill 50 enemies with the Frostwalker",
        "f54d74c52cf0f8f195501c56690983f499218ace.jpg",
        "9196cfd7258d16c839f10dffcf840eb3ffc0e764.jpg",
        44.2,
    ),
    (
        "Key",
        "Purchase 25 chests",
        "a19bdb3e6c435b084de0aec0c160922e128d7830.jpg",
        "2e14e22686bdca5d74da3828a41b65fe7dfdd856.jpg",
        91.5,
    ),
    (
        "Skuleg",
        "Obtain 40% Difficulty",
        "1e78c9c9fa565f9ed94a25a5b8a939805c710a23.jpg",
        "68bf96a306e9a4c2499c41f9fb53470b3200e860.jpg",
        44.6,
    ),
    (
        "Tactical Glasses",
        "Defeat a Miniboss in 25s",
        "f9b459e73aba093d2bf22e713e32629fd8a264f5.jpg",
        "e623801917741d2ad8320cd253738ca89e6f3f53.jpg",
        94.3,
    ),
    (
        "Turbo Socks",
        "Upgrade Agility Tome to Level 5",
        "b598122617be7a72928557f4f2cf5c194b80c8a4.jpg",
        "61484582f89fe4300bcca7ee57f1a249b57d14e2.jpg",
        66.2,
    ),
    (
        "Demonic Blade",
        "Heal for 350 HP using Lifesteal",
        "8b915dca7fdf54f1fa9717562d5ab7a59d280e6d.jpg",
        "c4c97240021a771c7cc5d57dd1a88e88ec20872e.jpg",
        82.4,
    ),
    (
        "Demonic Blood",
        "Increase Max HP with Blood Magic 400 times",
        "74c77197d6dad9d4af3313eaa28b05a8d9239132.jpg",
        "4336b3df92917f0d93dd1d1c5a0db451bdfdcb05.jpg",
        19.4,
    ),
    (
        "Echo Shard",
        "Upgrade XP Tome to Level 8",
        "f8c35747a27ac21aea18e4f186787818e9f59547.jpg",
        "358d06f55700b5b98cdd19aea34f6be23fff1510.jpg",
        54.9,
    ),
    (
        "Golden Sneakers",
        "Break 150 pots",
        "35d3decef0a81532136f54cee922de37ce941ab9.jpg",
        "d3ca3e2e027cf0a5197ab6130dc8ea191a3a1802.jpg",
        82.8,
    ),
    (
        "Idle Juice",
        "Kill 500 enemies while standing still",
        "398bcb80252ebfd868b3c0027df1d876f49d0363.jpg",
        "71f5657cc162610037fbb9e71c381bfc95145030.jpg",
        85.2,
    ),
    (
        "Kevin",
        "Obtain 3 Leeching Crystal in a run",
        "edd5813740a51842aea14a94b6752d1ff254a1f6.jpg",
        "fe488c5c7cf49f5111c9aba9a0878e41fad49d14.jpg",
        14.6,
    ),
    (
        "Leeching Crystal",
        "Upgrade Regen Tome to Level 8",
        "be15f49209540f650abccb1710087e6a1b30df73.jpg",
        "1f3da837a4b503a817d83998fcc95a8a2b624f61.jpg",
        31.3,
    ),
    (
        "Poison Gloves",
        "Use a Microwave to duplicate Moldy Cheese 3 times in a row",
        "b6d6f82824402b3baa6d765cf0ce8abc50b4f2c7.jpg",
        "4aebe34ea5a5439bacf2f1409562bf19dfbb85cd.jpg",
        33.6,
    ),
    (
        "Cursed Gloves",
        "Get the Cursed Tome to level 10",
        "4b980b6f8f4cae604f8e04a0db7e7e95149ac38a.jpg",
        "8aebf3c75a34bbb66311446dbe153c715369a8b2.jpg",
        23.0,
    ),
    (
        "Demonic Soul",
        "Reach Level 30 as Calcium",
        "44f3b615e0ca772d9dd28cc47ddb086e16099bfe.jpg",
        "f9aab199558186924766e0661cfd868e512c2690.jpg",
        65.4,
    ),
];

/// Названия для демо-достижений: по кругу, с номером, если не хватило.
const ACH_NAMES: &[(&str, &str)] = &[
    ("Первая кровь", "Победить первого врага"),
    ("Без единой царапины", "Пройти уровень, не получив урона"),
    ("Коллекционер", "Собрать 100 предметов"),
    ("Марафонец", "Продержаться 30 минут"),
    ("Всё и сразу", "Открыть все улучшения одного оружия"),
    ("Скупой рыцарь", "Накопить 10 000 золота"),
    ("Пацифист", "Пройти уровень, никого не убив"),
    ("Ночная смена", "Сыграть после полуночи"),
    ("Мастер на все руки", "Открыть всех персонажей"),
    ("На волоске", "Победить с 1 очком здоровья"),
    ("Охотник", "Победить 1000 врагов"),
    ("Исследователь", "Открыть все области карты"),
];

const MOCK_QR: [&str; 2] = [
    "https://s.team/q/1/0000000000000000001",
    "https://s.team/q/1/0000000000000000002",
];

impl State {
    /// После входа: купленные, но не установленные игры — в библиотеку.
    fn add_owned(&mut self) {
        for &(appid, name, size_gb, windows) in OWNED_EXTRA {
            if !windows || self.game(appid).is_some() {
                continue;
            }
            self.games.push(MockGame {
                appid,
                name,
                installed: false,
                library: self.default_library,
                size_gb,
                bridge: BridgeState::Unknown,
                cloud: CloudState::Unknown,
                last_sync_ago: None,
                files: &[],
            });
        }
    }

    fn game(&self, appid: AppId) -> Option<&MockGame> {
        self.games.iter().find(|g| g.appid == appid)
    }

    fn used_gb(&self, lib: usize) -> f64 {
        self.games
            .iter()
            .filter(|g| g.installed && g.library == lib)
            .map(|g| g.size_gb)
            .sum()
    }

    fn free_bytes(&self, lib: usize) -> u64 {
        let l = &self.libraries[lib];
        let reserved: f64 = self
            .jobs
            .iter()
            .filter(|j| j.library == lib)
            .map(|j| j.done as f64 / 1e9)
            .sum();
        ((l.total_gb - l.other_gb - self.used_gb(lib) - reserved).max(0.0) * 1e9) as u64
    }

    fn library_index(&self, path: &Path) -> Result<usize> {
        let p = path.to_string_lossy();
        let p = p.trim_end_matches('/');
        self.libraries
            .iter()
            .position(|l| l.path == p)
            .ok_or_else(|| BackendError::new(format!("{p} — не папка библиотеки")))
    }
}

pub struct MockBackend {
    state: Arc<Mutex<State>>,
}

/// Скорость «загрузки»: 38 МБ/с с колебаниями.
const MOCK_SPEED: f64 = 38e6;

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;

impl MockBackend {
    pub fn new() -> Self {
        let fresh = std::env::var("CARAFE_MOCK").is_ok_and(|v| v == "fresh");
        let on_soju = !fresh && std::env::var("CARAFE_MOCK_ENGINE").is_ok_and(|v| v == "soju");
        use CloudAction::*;
        let games = vec![
            MockGame {
                appid: 3405340,
                name: "Megabonk",
                library: 0,
                installed: true,
                size_gb: 1.1,
                bridge: BridgeState::Ready,
                cloud: CloudState::Synced,
                last_sync_ago: Some(2 * HOUR),
                files: &[
                    ("progression.json", Synced, false),
                    ("stats.json", Synced, false),
                    ("controller_config.json", Synced, false),
                ],
            },
            MockGame {
                appid: 1145350,
                name: "Hades II",
                library: 1,
                installed: true,
                size_gb: 9.4,
                bridge: BridgeState::NotBuilt,
                cloud: CloudState::Never,
                last_sync_ago: None,
                files: &[
                    ("Profile1.sav", Download, false),
                    ("Profile1_Temp.sav", Download, false),
                ],
            },
            MockGame {
                appid: 2379780,
                name: "Balatro",
                library: 0,
                installed: true,
                size_gb: 0.06,
                bridge: BridgeState::Ready,
                cloud: CloudState::Pending,
                last_sync_ago: Some(3 * DAY),
                files: &[
                    ("1/profile.jkr", Upload, false),
                    ("1/save.jkr", Upload, false),
                    ("settings.jkr", Synced, false),
                ],
            },
            MockGame {
                appid: 1794680,
                name: "Vampire Survivors",
                library: 0,
                installed: true,
                size_gb: 0.8,
                bridge: BridgeState::Ready,
                cloud: CloudState::Conflict,
                last_sync_ago: Some(9 * DAY),
                files: &[("SaveData", Upload, true), ("Settings", Synced, false)],
            },
            MockGame {
                appid: 413150,
                name: "Stardew Valley",
                library: 1,
                installed: true,
                size_gb: 0.7,
                bridge: BridgeState::NotNeeded,
                cloud: CloudState::Unavailable,
                last_sync_ago: None,
                files: &[],
            },
            MockGame {
                appid: 1245620,
                name: "Elden Ring",
                library: 1,
                installed: false,
                size_gb: 49.0,
                bridge: BridgeState::Unknown,
                cloud: CloudState::Unknown,
                last_sync_ago: None,
                files: &[],
            },
            MockGame {
                appid: 1086940,
                name: "Baldur's Gate 3",
                library: 1,
                installed: false,
                size_gb: 122.0,
                bridge: BridgeState::Unknown,
                cloud: CloudState::Unknown,
                last_sync_ago: None,
                files: &[],
            },
            MockGame {
                appid: 1091500,
                name: "Cyberpunk 2077",
                library: 0,
                installed: false,
                size_gb: 65.0,
                bridge: BridgeState::Unknown,
                cloud: CloudState::Unknown,
                last_sync_ago: None,
                files: &[],
            },
            MockGame {
                appid: 367520,
                name: "Hollow Knight",
                library: 2,
                installed: true,
                size_gb: 6.9,
                bridge: BridgeState::Ready,
                cloud: CloudState::Synced,
                last_sync_ago: Some(5 * DAY),
                files: &[("user1.dat", Synced, false), ("user2.dat", Synced, false)],
            },
        ];
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/me".into());
        let lib = |path: String, name: &str, builtin, available, total_gb, other_gb| MockLibrary {
            path,
            name: name.into(),
            builtin,
            available,
            total_gb,
            other_gb,
        };
        let libraries = vec![
            lib(
                format!("{home}/Library/Application Support/carafe/games"),
                "Встроенная",
                true,
                true,
                494.0,
                402.0,
            ),
            lib(
                "/Volumes/Samsung T7/Games".into(),
                "Samsung T7",
                false,
                true,
                1000.0,
                212.0,
            ),
            lib(
                "/Volumes/Archive/carafe".into(),
                "Archive",
                false,
                false,
                2000.0,
                0.0,
            ),
        ];
        let queue = std::env::var("CARAFE_MOCK_QUEUE").unwrap_or_default();
        let jobs = if fresh || queue == "empty" {
            Vec::new()
        } else if queue == "finishing" {
            vec![MockJob {
                appid: 1245620,
                library: 1,
                kind: DownloadKind::Install,
                state: DownloadState::Active,
                stage: DownloadStage::Downloading,
                done: 48_800_000_000,
                total: 49_000_000_000,
                error: None,
            }]
        } else {
            vec![
                MockJob {
                    appid: 1245620,
                    library: 1,
                    kind: DownloadKind::Install,
                    state: DownloadState::Active,
                    stage: DownloadStage::Downloading,
                    done: 9_100_000_000,
                    total: 49_000_000_000,
                    error: None,
                },
                MockJob {
                    appid: 1086940,
                    library: 1,
                    kind: DownloadKind::Install,
                    state: DownloadState::Queued,
                    stage: DownloadStage::Preparing,
                    done: 0,
                    total: 122_000_000_000,
                    error: None,
                },
            ]
        };
        let state = Arc::new(Mutex::new(State {
            soju: on_soju,
            gptk: !fresh,
            active: if fresh {
                None
            } else if on_soju {
                Some(EngineKind::Soju)
            } else {
                Some(EngineKind::Gptk)
            },
            bottle: !fresh,
            steam: !fresh,
            games,
            libraries,
            default_library: 1,
            jobs,
            account: None,
        }));
        if std::env::var("CARAFE_MOCK_STEAM").is_ok_and(|v| v == "in") {
            let mut st = state.lock().expect("mock state");
            st.account = Some("tester_mac");
            st.add_owned();
        }
        let st = Arc::downgrade(&state);
        std::thread::spawn(move || download_loop(st));
        Self { state }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("mock state")
    }

    fn with_game<T>(&self, appid: AppId, f: impl FnOnce(&mut MockGame) -> T) -> Result<T> {
        let mut st = self.state.lock().expect("mock state");
        st.games
            .iter_mut()
            .find(|g| g.appid == appid)
            .map(f)
            .ok_or_else(|| BackendError::new(format!("нет игры {appid}")))
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn wait(ms: u64) {
    sleep(Duration::from_millis(ms));
}

fn stage(events: Events, stage: Stage, status: StageStatus) {
    events(TaskEvent::Stage { stage, status });
}

fn log(events: Events, line: impl Into<String>) {
    events(TaskEvent::Log { line: line.into() });
}

impl Backend for MockBackend {
    fn label(&self) -> String {
        "демо-данные".into()
    }

    fn environment(&self, _profile: &Profile) -> Result<Environment> {
        wait(250);
        let st = self.state.lock().expect("mock state");
        // D3DMetal у soju — из копии GPTK; у GPTK он свой.
        let d3dmetal = match st.active {
            Some(EngineKind::Soju) => st.gptk,
            Some(EngineKind::Gptk) => true,
            None => false,
        };
        let mut engines: Vec<EngineInfo> = [
            (st.soju, EngineKind::Soju, SOJU, st.gptk),
            (st.gptk, EngineKind::Gptk, GPTK, true),
        ]
        .into_iter()
        .filter(|(installed, ..)| *installed)
        .map(|(_, kind, name, d3dmetal)| EngineInfo {
            // Активный — первым и с пустым id: «как в carafe».
            id: if st.active == Some(kind) {
                String::new()
            } else {
                kind.as_str().into()
            },
            name: name.into(),
            d3dmetal,
        })
        .collect();
        engines.sort_by_key(|e| !e.id.is_empty());
        let check = |id, ok, fixable| Check {
            id,
            ok,
            fixable,
            hint: None,
        };
        Ok(Environment {
            checks: vec![
                check(CheckId::Rosetta, true, false),
                check(CheckId::Engine, st.active.is_some(), true),
                check(CheckId::D3dMetal, d3dmetal, st.active.is_some()),
                check(CheckId::Bottle, st.bottle, true),
                check(CheckId::Bridge, true, false),
                check(CheckId::Steam, st.steam, true),
            ],
            engines,
            bottles: vec!["default".into(), "soju-test".into()],
            engine: Some(EngineState {
                label: match st.active {
                    Some(EngineKind::Soju) => SOJU.into(),
                    Some(EngineKind::Gptk) => GPTK.into(),
                    None => "soju".into(),
                },
                kind: st.active,
                ready: st.active.is_some(),
                d3dmetal,
                custom: false,
                soju: st.soju.then(|| SOJU.into()),
                gptk: st.gptk.then(|| GPTK.into()),
                suggest_soju: st.active == Some(EngineKind::Gptk),
            }),
        })
    }

    fn games(&self, _profile: &Profile) -> Result<Vec<Game>> {
        wait(400);
        let st = self.state.lock().expect("mock state");
        let t = now();
        Ok(st
            .games
            .iter()
            .map(|g| Game {
                appid: g.appid,
                name: g.name.into(),
                cover: Cover::for_app(g.appid),
                installed: g.installed,
                size_bytes: Some((g.size_gb * 1e9) as u64),
                library: g.installed.then(|| st.libraries[g.library].path.clone()),
                available: !g.installed || st.libraries[g.library].available,
                bridge: g.bridge,
                cloud: CloudSummary {
                    state: g.cloud,
                    last_sync: g.last_sync_ago.map(|a| t - a),
                },
            })
            .collect())
    }

    fn cloud_status(&self, appid: AppId, _profile: &Profile) -> Result<CloudStatus> {
        wait(700);
        if !self.state.lock().expect("mock state").steam {
            return Err(BackendError::new("Steam для Mac не запущен"));
        }
        let t = now();
        self.with_game(appid, |g| CloudStatus {
            files: g
                .files
                .iter()
                .map(|&(name, action, conflict)| CloudFile {
                    name: name.into(),
                    action,
                    conflict,
                })
                .collect(),
            last_sync: g.last_sync_ago.map(|a| t - a),
            note: g
                .files
                .is_empty()
                .then(|| "у игры нет Steam Cloud".to_string()),
        })
    }

    fn launch(&self, appid: AppId, profile: &Profile, events: Events) -> Result<i32> {
        {
            let st = self.lock();
            let g = st
                .game(appid)
                .ok_or_else(|| BackendError::new("нет игры"))?;
            if !st.libraries[g.library].available {
                return Err(BackendError::new(format!(
                    "библиотека недоступна: {}",
                    st.libraries[g.library].path
                )));
            }
        }
        let (name, files, bridge) = self.with_game(appid, |g| (g.name, g.files, g.bridge))?;
        if !self.state.lock().expect("mock state").steam {
            return Err(BackendError::new("Steam для Mac не запущен"));
        }

        let cloud = profile.cloud && !files.is_empty();
        if cloud {
            stage(events, Stage::CloudPull, StageStatus::Active);
            log(events, "==> Steam Cloud: pull");
            wait(900);
            for &(f, action, conflict) in files {
                if action == CloudAction::Download {
                    events(TaskEvent::CloudFile {
                        file: CloudFile {
                            name: f.into(),
                            action,
                            conflict,
                        },
                    });
                    log(events, format!("  ↓ {f}"));
                    wait(250);
                }
            }
            stage(events, Stage::CloudPull, StageStatus::Done);
        } else {
            stage(events, Stage::CloudPull, StageStatus::Skipped);
        }

        if profile.bridge && bridge == BridgeState::Ready {
            stage(events, Stage::Bridge, StageStatus::Active);
            wait(500);
            log(events, "==> мост в Steam: порт 54310");
            stage(events, Stage::Bridge, StageStatus::Done);
        } else {
            stage(events, Stage::Bridge, StageStatus::Skipped);
        }

        stage(events, Stage::Game, StageStatus::Active);
        log(events, format!("==> запускаю {name}.exe"));
        wait(600);
        log(events, "esync: up and running.");
        log(events, "Direct3D: D3DMetal, feature level 11_1");
        if profile.hud {
            log(events, "Metal HUD включён");
        }
        wait(5000);
        stage(events, Stage::Game, StageStatus::Done);

        if cloud {
            stage(events, Stage::CloudPush, StageStatus::Active);
            log(events, "==> Steam Cloud: push");
            wait(800);
            for &(f, action, conflict) in files {
                if action == CloudAction::Upload {
                    events(TaskEvent::CloudFile {
                        file: CloudFile {
                            name: f.into(),
                            action,
                            conflict,
                        },
                    });
                    log(events, format!("  ↑ {f}"));
                    wait(250);
                }
            }
            stage(events, Stage::CloudPush, StageStatus::Done);
            self.with_game(appid, |g| {
                g.cloud = CloudState::Synced;
                g.last_sync_ago = Some(0);
            })?;
        } else {
            stage(events, Stage::CloudPush, StageStatus::Skipped);
        }
        Ok(0)
    }

    fn build_bridge(&self, appid: AppId, _profile: &Profile, events: Events) -> Result<()> {
        stage(events, Stage::Build, StageStatus::Active);
        for line in [
            "sdk: 1.62 (по версиям интерфейсов в steam_api64.dll)",
            "gen: 1031 функция, 47 интерфейсов",
            "layout: 318 структур совпали",
            "llvm-mingw: steam_api64.dll",
            "clang: soju-steamd",
        ] {
            wait(500);
            log(events, line);
        }
        self.with_game(appid, |g| g.bridge = BridgeState::Ready)?;
        stage(events, Stage::Build, StageStatus::Done);
        Ok(())
    }

    fn install_engine(&self, _profile: &Profile, events: Events) -> Result<()> {
        stage(events, Stage::Download, StageStatus::Active);
        for i in 1..=20 {
            wait(120);
            events(TaskEvent::Progress {
                stage: Stage::Download,
                fraction: i as f32 / 20.0,
            });
        }
        stage(events, Stage::Download, StageStatus::Done);
        stage(events, Stage::Unpack, StageStatus::Active);
        wait(1200);
        stage(events, Stage::Unpack, StageStatus::Done);
        let mut st = self.state.lock().expect("mock state");
        st.soju = true;
        // Новый пользователь получает soju; тот, кто на GPTK, переходит сам.
        st.active.get_or_insert(EngineKind::Soju);
        Ok(())
    }

    fn install_gptk(&self, _profile: &Profile, events: Events) -> Result<()> {
        stage(events, Stage::Download, StageStatus::Active);
        for i in 1..=10 {
            wait(120);
            events(TaskEvent::Progress {
                stage: Stage::Download,
                fraction: i as f32 / 10.0,
            });
        }
        stage(events, Stage::Download, StageStatus::Done);
        let mut st = self.state.lock().expect("mock state");
        st.gptk = true;
        st.active.get_or_insert(EngineKind::Gptk);
        Ok(())
    }

    fn use_engine(&self, kind: EngineKind, _profile: &Profile, events: Events) -> Result<()> {
        {
            let st = self.state.lock().expect("mock state");
            let installed = match kind {
                EngineKind::Soju => st.soju,
                EngineKind::Gptk => st.gptk,
            };
            if !installed {
                return Err(BackendError::new(format!(
                    "{} не установлен",
                    kind.as_str()
                )));
            }
        }
        stage(events, Stage::Migrate, StageStatus::Active);
        for line in [
            "==> default: резервная копия → bottles/default.backup-20261030-120000",
            "==> default: wineboot -u",
            "==> default: обновлена за 24 с",
        ] {
            wait(700);
            log(events, line);
        }
        stage(events, Stage::Migrate, StageStatus::Done);
        self.state.lock().expect("mock state").active = Some(kind);
        Ok(())
    }

    fn create_bottle(&self, _profile: &Profile, events: Events) -> Result<()> {
        stage(events, Stage::Bottle, StageStatus::Active);
        wait(1500);
        stage(events, Stage::Bottle, StageStatus::Done);
        self.state.lock().expect("mock state").bottle = true;
        Ok(())
    }

    fn open_steam(&self) -> Result<()> {
        wait(1200);
        self.state.lock().expect("mock state").steam = true;
        Ok(())
    }

    fn libraries(&self) -> Result<Vec<Library>> {
        wait(150);
        let st = self.lock();
        Ok(st
            .libraries
            .iter()
            .enumerate()
            .map(|(i, l)| Library {
                path: l.path.clone(),
                name: l.name.clone(),
                builtin: l.builtin,
                default: i == st.default_library,
                available: l.available,
                total_bytes: l.available.then_some((l.total_gb * 1e9) as u64),
                free_bytes: l.available.then(|| st.free_bytes(i)),
                used_bytes: (st.used_gb(i) * 1e9) as u64,
                games: st
                    .games
                    .iter()
                    .filter(|g| g.installed && g.library == i)
                    .count() as u32,
            })
            .collect())
    }

    fn add_library(&self, path: &Path) -> Result<()> {
        let mut st = self.lock();
        let p = path.to_string_lossy().trim_end_matches('/').to_string();
        if st.libraries.iter().any(|l| l.path == p) {
            return Err(BackendError::new(format!("{p} уже в библиотеках")));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.clone());
        st.libraries.push(MockLibrary {
            path: p,
            name,
            builtin: false,
            available: true,
            total_gb: 500.0,
            other_gb: 180.0,
        });
        Ok(())
    }

    fn remove_library(&self, path: &Path) -> Result<()> {
        let mut st = self.lock();
        let i = st.library_index(path)?;
        if st.libraries[i].builtin {
            return Err(BackendError::new("встроенную библиотеку убрать нельзя"));
        }
        let busy = st.games.iter().any(|g| g.installed && g.library == i)
            || st.jobs.iter().any(|j| j.library == i);
        if busy && st.libraries[i].available {
            return Err(BackendError::new(
                "в библиотеке есть игры — сначала перенеси их",
            ));
        }
        st.libraries.remove(i);
        for g in st.games.iter_mut() {
            if g.library == i {
                // Игры с отключённого диска забываются вместе с ним.
                g.installed = false;
                g.library = 0;
            } else if g.library > i {
                g.library -= 1;
            }
        }
        st.jobs.retain(|j| j.library != i);
        for j in st.jobs.iter_mut().filter(|j| j.library > i) {
            j.library -= 1;
        }
        if st.default_library == i {
            st.default_library = 0;
        } else if st.default_library > i {
            st.default_library -= 1;
        }
        Ok(())
    }

    fn set_default_library(&self, path: &Path) -> Result<()> {
        let mut st = self.lock();
        st.default_library = st.library_index(path)?;
        Ok(())
    }

    fn steam_account(&self) -> Result<Option<SteamAccount>> {
        Ok(self.lock().account.map(|a| SteamAccount {
            account_name: a.into(),
            steam_id: Some("••••4242".into()),
        }))
    }

    fn steam_login(&self, cancel: &AtomicBool, events: Events) -> Result<SteamAccount> {
        // Два кода, как будто Steam сменил challenge, потом «подтверждение в приложении».
        for url in MOCK_QR {
            events(TaskEvent::Qr { url: url.into() });
            for _ in 0..40 {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(BackendError::new("отменено"));
                }
                wait(100);
            }
        }
        let mut st = self.lock();
        st.account = Some("tester_mac");
        st.add_owned();
        Ok(SteamAccount {
            account_name: "tester_mac".into(),
            steam_id: Some("••••4242".into()),
        })
    }

    fn steam_logout(&self) -> Result<()> {
        wait(500);
        let mut st = self.lock();
        st.account = None;
        let queued: Vec<AppId> = st.jobs.iter().map(|j| j.appid).collect();
        st.games.retain(|g| {
            g.installed || queued.contains(&g.appid) || !OWNED_EXTRA.iter().any(|o| o.0 == g.appid)
        });
        Ok(())
    }

    fn owned_games(&self) -> Result<Vec<OwnedGame>> {
        wait(900);
        let st = self.lock();
        if st.account.is_none() {
            return Err(BackendError::new("не вошёл в Steam"));
        }
        let mut out: Vec<OwnedGame> = st
            .games
            .iter()
            .map(|g| OwnedGame {
                appid: g.appid,
                name: g.name.into(),
                cover: Cover::for_app(g.appid),
                installed: g.installed,
                windows: true,
                size_bytes: Some((g.size_gb * 1e9) as u64),
            })
            .collect();
        for &(appid, name, size_gb, windows) in OWNED_EXTRA {
            if !out.iter().any(|g| g.appid == appid) {
                out.push(OwnedGame {
                    appid,
                    name: name.into(),
                    cover: Cover::for_app(appid),
                    installed: false,
                    windows,
                    size_bytes: windows.then_some((size_gb * 1e9) as u64),
                });
            }
        }
        Ok(out)
    }

    fn game_stats(&self, appid: AppId, _fresh: bool) -> Result<Option<GameStats>> {
        if self.lock().account.is_none() {
            return Ok(None);
        }
        wait(600);
        let Some(&(_, minutes, two_weeks, ago, total, unlocked)) =
            STATS.iter().find(|s| s.0 == appid)
        else {
            return Ok(Some(GameStats::default()));
        };
        let t = now() as i64;
        let ago = ago.map(|a| t - a as i64);
        let items = (0..total)
            .map(|i| {
                let (name, desc) = ACH_NAMES[i as usize % ACH_NAMES.len()];
                let lap = i as usize / ACH_NAMES.len();
                let open = i < unlocked;
                let real = (appid == 3405340).then(|| MEGABONK_ACH.get(i as usize)).flatten();
                let icon = |h: &str| {
                    format!("https://cdn.cloudflare.steamstatic.com/steamcommunity/public/images/apps/{appid}/{h}")
                };
                // Последнее из открытых — в последний запуск, остальные — раньше, через день-два.
                let at = ago.filter(|_| open).map(|last| last - (unlocked - 1 - i) as i64 * 37 * HOUR as i64);
                let rare = real.map_or(100.0 * 0.82_f64.powi(i as i32 + 1), |r| r.4);
                Achievement {
                    id: format!("ACH_{i:02}"),
                    name: match real {
                        Some(r) => r.0.into(),
                        None if lap > 0 => format!("{name} {}", lap + 1),
                        None => name.into(),
                    },
                    description: match real {
                        Some(r) => Some(r.1.into()),
                        None => (i % 7 != 6).then(|| desc.into()),
                    },
                    icon: real.map(|r| icon(r.2)),
                    icon_gray: real.map(|r| icon(r.3)),
                    hidden: real.is_none() && i % 7 == 6,
                    unlocked: open,
                    unlocked_at: at,
                    global_percent: Some((rare * 10.0).round() / 10.0),
                }
            })
            .collect();
        Ok(Some(GameStats {
            playtime: (minutes > 0 || ago.is_some()).then_some(Playtime {
                minutes,
                minutes_2weeks: two_weeks,
                last_played: ago,
            }),
            achievements: (total > 0).then_some(Achievements {
                total,
                unlocked,
                items,
            }),
        }))
    }

    fn install(&self, appid: AppId, library: Option<&Path>) -> Result<()> {
        let mut st = self.lock();
        let lib = match library {
            Some(p) => st.library_index(p)?,
            None => st.default_library,
        };
        if !st.libraries[lib].available {
            return Err(BackendError::new("библиотека недоступна (диск отключён?)"));
        }
        if st.jobs.iter().any(|j| j.appid == appid) {
            return Err(BackendError::new("уже в очереди загрузок"));
        }
        let g = st
            .game(appid)
            .ok_or_else(|| BackendError::new(format!("нет игры {appid}")))?;
        let (installed, size, glib) = (g.installed, (g.size_gb * 1e9) as u64, g.library);
        if installed && glib != lib {
            return Err(BackendError::new(
                "игра уже установлена в другой библиотеке",
            ));
        }
        if !installed && size > st.free_bytes(lib) {
            return Err(BackendError::new(format!(
                "мало места в «{}»",
                st.libraries[lib].name
            )));
        }
        st.jobs.push(MockJob {
            appid,
            library: lib,
            kind: if installed {
                DownloadKind::Update
            } else {
                DownloadKind::Install
            },
            state: DownloadState::Queued,
            stage: DownloadStage::Preparing,
            done: 0,
            total: if installed { size / 20 } else { size },
            error: None,
        });
        Ok(())
    }

    fn move_game(
        &self,
        appid: AppId,
        library: &Path,
        cancel: &AtomicBool,
        events: Events,
    ) -> Result<()> {
        let (to, size, name) = {
            let st = self.lock();
            let to = st.library_index(library)?;
            let g = st
                .game(appid)
                .filter(|g| g.installed)
                .ok_or_else(|| BackendError::new("игра не установлена"))?;
            if !st.libraries[g.library].available || !st.libraries[to].available {
                return Err(BackendError::new("библиотека недоступна (диск отключён?)"));
            }
            if g.library == to {
                return Err(BackendError::new("игра уже там"));
            }
            if st.jobs.iter().any(|j| j.appid == appid) {
                return Err(BackendError::new("игра в очереди загрузок"));
            }
            let size = (g.size_gb * 1e9) as u64;
            if size > st.free_bytes(to) {
                return Err(BackendError::new(format!(
                    "мало места в «{}»",
                    st.libraries[to].name
                )));
            }
            (to, size, st.libraries[to].name.clone())
        };
        stage(events, Stage::Move, StageStatus::Active);
        log(events, format!("==> копирую в «{name}»"));
        // Крупные игры «копируются» дольше, но не больше 8 секунд.
        let steps = 40;
        let per = ((size as f64 / 1e9 * 60.0) as u64).clamp(20, 200);
        for i in 1..=steps {
            if cancel.load(Ordering::Relaxed) {
                stage(
                    events,
                    Stage::Move,
                    StageStatus::Failed {
                        message: "отменено".into(),
                    },
                );
                return Err(BackendError::new("перенос отменён — игра на прежнем месте"));
            }
            wait(per);
            events(TaskEvent::Progress {
                stage: Stage::Move,
                fraction: i as f32 / steps as f32,
            });
        }
        self.with_game(appid, |g| g.library = to)?;
        stage(events, Stage::Move, StageStatus::Done);
        Ok(())
    }

    fn downloads(&self) -> Result<Vec<Download>> {
        let st = self.lock();
        Ok(st
            .jobs
            .iter()
            .map(|j| {
                let active = j.state == DownloadState::Active;
                let speed = (active && j.stage == DownloadStage::Downloading).then(|| {
                    // Колебания скорости, чтобы цифры жили.
                    let t = now() as f64;
                    (MOCK_SPEED * (1.0 + 0.15 * (t * 0.7).sin())) as u64
                });
                Download {
                    appid: j.appid,
                    name: st
                        .game(j.appid)
                        .map(|g| g.name.to_string())
                        .unwrap_or_else(|| format!("App {}", j.appid)),
                    cover: Cover::for_app(j.appid),
                    library: st.libraries[j.library].path.clone(),
                    kind: j.kind,
                    state: j.state,
                    stage: (j.state != DownloadState::Queued).then_some(j.stage),
                    done_bytes: j.done,
                    total_bytes: j.total,
                    speed,
                    eta_secs: speed.map(|s| j.total.saturating_sub(j.done) / s.max(1)),
                    error: j.error.clone(),
                }
            })
            .collect())
    }

    fn download_action(&self, appid: AppId, action: DownloadAction) -> Result<()> {
        let mut st = self.lock();
        let i = st
            .jobs
            .iter()
            .position(|j| j.appid == appid)
            .ok_or_else(|| BackendError::new("нет в очереди"))?;
        match action {
            DownloadAction::Pause => st.jobs[i].state = DownloadState::Paused,
            DownloadAction::Resume => {
                st.jobs[i].state = DownloadState::Queued;
                st.jobs[i].error = None;
            }
            DownloadAction::Cancel => {
                st.jobs.remove(i);
            }
            DownloadAction::Top => {
                let j = st.jobs.remove(i);
                st.jobs.insert(0, j);
            }
            DownloadAction::Up if i > 0 => st.jobs.swap(i, i - 1),
            DownloadAction::Down if i + 1 < st.jobs.len() => st.jobs.swap(i, i + 1),
            DownloadAction::Up | DownloadAction::Down => {}
        }
        Ok(())
    }
}

/// Поток «загрузки»: по одной за раз, первая ждущая по порядку. Активная, которую обогнали
/// перестановкой, доигрывает до конца — как в ядре.
fn download_loop(state: std::sync::Weak<Mutex<State>>) {
    let mut last = Instant::now();
    loop {
        sleep(Duration::from_millis(200));
        let Some(state) = state.upgrade() else { return };
        let mut st = state.lock().expect("mock state");
        let dt = last.elapsed().as_secs_f64();
        last = Instant::now();
        let active = match st
            .jobs
            .iter()
            .position(|j| j.state == DownloadState::Active)
        {
            Some(i) => i,
            None => match st
                .jobs
                .iter()
                .position(|j| j.state == DownloadState::Queued)
            {
                Some(i) => {
                    let j = &mut st.jobs[i];
                    j.state = DownloadState::Active;
                    j.stage = DownloadStage::Preparing;
                    i
                }
                None => continue,
            },
        };
        let lib_ok = st.libraries[st.jobs[active].library].available;
        let j = &mut st.jobs[active];
        if !lib_ok {
            j.state = DownloadState::Failed;
            j.error = Some("библиотека недоступна (диск отключён?)".into());
            continue;
        }
        match j.stage {
            DownloadStage::Preparing => j.stage = DownloadStage::Downloading,
            DownloadStage::Downloading => {
                j.done = (j.done + (MOCK_SPEED * dt) as u64).min(j.total);
                if j.done >= j.total {
                    j.stage = DownloadStage::Verifying;
                    j.done = 0;
                }
            }
            DownloadStage::Verifying | DownloadStage::Allocating => {
                // Проверка — секунды, а не минуты: на демо её не ждут.
                let speed = (MOCK_SPEED * 8.0).max(j.total as f64 / 6.0);
                j.done = (j.done + (speed * dt) as u64).min(j.total);
                if j.done >= j.total {
                    j.stage = DownloadStage::Committing;
                }
            }
            DownloadStage::Committing => {
                let (appid, lib) = (j.appid, j.library);
                st.jobs.remove(active);
                if let Some(g) = st.games.iter_mut().find(|g| g.appid == appid) {
                    g.installed = true;
                    g.library = lib;
                }
            }
        }
    }
}
