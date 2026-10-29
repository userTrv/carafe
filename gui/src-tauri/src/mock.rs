//! Правдоподобные данные и задержки для разработки UI. Ничего не запускает.
//!
//! `CARAFE_MOCK=fresh` — первый запуск: нет движка, бутылки и запущенного Steam.

use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::backend::*;

struct MockGame {
    appid: AppId,
    name: &'static str,
    installed: bool,
    size_gb: f64,
    bridge: BridgeState,
    cloud: CloudState,
    last_sync_ago: Option<u64>,
    files: &'static [(&'static str, CloudAction, bool)],
}

struct State {
    engine: bool,
    bottle: bool,
    steam: bool,
    games: Vec<MockGame>,
}

pub struct MockBackend {
    state: Mutex<State>,
}

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;

impl MockBackend {
    pub fn new() -> Self {
        let fresh = std::env::var("CARAFE_MOCK").is_ok_and(|v| v == "fresh");
        use CloudAction::*;
        let games = vec![
            MockGame {
                appid: 3405340,
                name: "Megabonk",
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
                installed: false,
                size_gb: 122.0,
                bridge: BridgeState::Unknown,
                cloud: CloudState::Unknown,
                last_sync_ago: None,
                files: &[],
            },
            MockGame {
                appid: 367520,
                name: "Hollow Knight",
                installed: true,
                size_gb: 6.9,
                bridge: BridgeState::Ready,
                cloud: CloudState::Synced,
                last_sync_ago: Some(5 * DAY),
                files: &[("user1.dat", Synced, false), ("user2.dat", Synced, false)],
            },
        ];
        Self {
            state: Mutex::new(State {
                engine: !fresh,
                bottle: !fresh,
                steam: !fresh,
                games,
            }),
        }
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
        let check = |id, ok, fixable| Check {
            id,
            ok,
            fixable,
            hint: None,
        };
        Ok(Environment {
            checks: vec![
                check(CheckId::Rosetta, true, false),
                check(CheckId::Engine, st.engine, true),
                check(CheckId::D3dMetal, st.engine, true),
                check(CheckId::Bottle, st.bottle, true),
                check(CheckId::Bridge, true, false),
                check(CheckId::Steam, st.steam, true),
            ],
            engines: vec![
                EngineInfo {
                    id: "gptk-3.0-3".into(),
                    name: "GPTK 3.0-3".into(),
                    d3dmetal: true,
                },
                EngineInfo {
                    id: "soju-engine".into(),
                    name: "soju · Wine 11.0".into(),
                    d3dmetal: true,
                },
            ],
            bottles: vec!["default".into(), "soju-test".into()],
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
        self.state.lock().expect("mock state").engine = true;
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
}
