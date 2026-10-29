//! carafe — те же команды, что у bash-версии `bin/carafe`, поверх carafe-core.

mod account;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::ExitCode;

use carafe_core::{BridgeState, BridgeTarget, Carafe, CloudMode, Error, parse_appid};

const USAGE: &str = "\
Использование:
  carafe engine install            скачать и распаковать Wine + D3DMetal
  carafe bottle create [NAME]      создать префикс Wine (по умолчанию: default)
  carafe login [--json]            войти в Steam по QR из приложения Steam (без пароля)
  carafe logout                    выйти: отозвать вход и стереть его из Связки ключей
  carafe account [--json]          под каким аккаунтом вошёл (без сети)
  carafe library [--json]          все купленные игры, установленные отмечены
  carafe steam login USER          войти в steamcmd (пароль и Steam Guard вводишь сам)
  carafe game install APPID [--json]  скачать Windows-сборку с аккаунта из carafe login
  carafe game install APPID USER   то же через steamcmd
  carafe game import APPID [DIR]   забрать depot, скачанный консолью Steam (download_depot)
  carafe game run APPID [ARGS...]  запустить игру (exe ищется в папке игры)
  carafe game list                 установленные игры: мост, правила облака
  carafe bridge build APPID|DLL    собрать мост в Steam для Mac (ачивки, лидерборды) — из soju
  carafe bridge test APPID|DLL     проверить мост на Spacewar (480) против нативного Steam
  carafe cloud status|pull|push APPID  сейвы со Steam Cloud (game run делает pull/push сам)
  carafe run EXE [ARGS...]         запустить произвольный .exe в префиксе
  carafe wine [ARGS...]            вызвать wine с окружением префикса (winecfg и т. п.)

Переменные: CARAFE_BOTTLE (префикс, default), CARAFE_EXE (exe игры вручную),
            CARAFE_HUD=1 (Metal HUD с FPS), CARAFE_BRIDGE=0 (запуск без моста).
            CARAFE_ENGINE_DIR (свой движок, например dist/soju-engine из soju),
            CARAFE_SOJU (checkout soju, по умолчанию ../soju рядом с carafe).
            CARAFE_STEAM_HELPER (carafe-steam, по умолчанию steam/dist рядом с carafe).

Вход в Steam идёт через сторонний клиент на SteamKit2: Valve такие терпит, но официально
не поддерживает.";

enum Fail {
    /// Неизвестная команда: справка и код 1, как `usage; exit 1`.
    Usage,
    Core(Error),
}

impl From<Error> for Fail {
    fn from(e: Error) -> Self {
        Fail::Core(e)
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(Fail::Usage) => {
            println!("{USAGE}");
            ExitCode::from(1)
        }
        Err(Fail::Core(e)) => {
            eprintln!("carafe: {e}");
            ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}

/// Позиционный аргумент как строка; нет его — `None` (как `${N:-}`).
fn arg(args: &[OsString], i: usize) -> Option<&str> {
    args.get(i).and_then(|a| a.to_str()).filter(|a| !a.is_empty())
}

fn need<'a>(args: &'a [OsString], i: usize, what: &'static str) -> Result<&'a str, Error> {
    arg(args, i).ok_or(Error::Usage(what))
}

fn appid(args: &[OsString], i: usize) -> Result<u32, Error> {
    parse_appid(need(args, i, "нужен APPID")?)
}

/// `--json` после команды: машинный вывод для GUI. Другие лишние аргументы — ошибка.
fn json_flag(args: &[OsString], from: usize) -> Result<bool, Fail> {
    match args.get(from..).unwrap_or_default() {
        [] => Ok(false),
        [a] if a == "--json" => Ok(true),
        _ => Err(Fail::Usage),
    }
}

fn run(args: &[OsString]) -> Result<i32, Fail> {
    let cmd = arg(args, 0).unwrap_or("");
    let sub = arg(args, 1).unwrap_or("");
    if matches!(cmd, "" | "-h" | "--help" | "help") {
        println!("{USAGE}");
        return Ok(0);
    }
    let c = Carafe::from_env()?;
    match (cmd, sub) {
        ("engine", "install") => {
            c.engine_install()?;
        }
        ("bottle", "create") => {
            c.bottle_create(arg(args, 2))?;
        }
        ("steam", "login") => {
            // Пароль в аргументах оседает в истории шелла и в ps — не принимаем.
            if args.len() > 3 {
                return Err(Error::Usage("только логин; пароль steamcmd спросит сам").into());
            }
            c.steam_login(need(args, 2, "нужен логин Steam")?)?;
        }
        ("login", _) => account::login(&c, json_flag(args, 1)?)?,
        ("logout", _) => account::logout(&c)?,
        ("account", _) => account::status(&c, json_flag(args, 1)?)?,
        ("library", _) => account::library(&c, json_flag(args, 1)?)?,
        ("game", "install") => {
            let id = appid(args, 2)?;
            match arg(args, 3) {
                None | Some("--json") => account::install(&c, id, json_flag(args, 3)?)?,
                Some(user) => c.game_install(id, user)?,
            }
        }
        ("game", "import") => {
            let id = appid(args, 2)?;
            c.game_import(id, args.get(3).filter(|a| !a.is_empty()).map(Path::new))?;
        }
        ("game", "list") => {
            for g in c.games()? {
                let bridge = match g.bridge {
                    BridgeState::NotNeeded => "без моста",
                    BridgeState::NotBuilt => "мост не собран",
                    BridgeState::Built { .. } => "мост собран",
                };
                let cloud = match &g.cloud.rules {
                    None => "облако: ?".to_string(),
                    Some(r) => format!("облако: правил {}, файлов {}", r.len(), g.cloud.synced_files),
                };
                println!("{}\t{}\t{bridge}\t{cloud}", g.appid, g.name.as_deref().unwrap_or("?"));
            }
        }
        ("game", "run") => {
            let id = appid(args, 2)?;
            return Ok(c.run_game(id, &args[3..], &mut |_| {})?);
        }
        ("bridge", "build" | "test") => {
            let target = BridgeTarget::parse(need(args, 2, "нужен APPID или путь к steam_api64.dll")?)?;
            if sub == "build" {
                c.bridge_build(&target)?;
            } else {
                c.bridge_test(&target)?;
            }
        }
        ("cloud", m) => {
            let mode = CloudMode::parse(m).ok_or(Fail::Usage)?;
            c.cloud_sync(appid(args, 2)?, mode, &mut |e| println!("{e}"))?;
        }
        ("run", _) => {
            let exe = args.get(1).filter(|a| !a.is_empty()).ok_or(Error::Usage("нужен путь к exe"))?;
            let err = c.exe_command(Path::new(exe), &args[2..])?.exec();
            return Err(Error::Spawn { program: "wine".into(), source: err }.into());
        }
        ("wine", _) => {
            let wine = c.wine()?;
            let err = wine.wine().args(&args[1..]).exec();
            return Err(Error::Spawn { program: wine.bin.display().to_string(), source: err }.into());
        }
        _ => return Err(Fail::Usage),
    }
    Ok(0)
}
