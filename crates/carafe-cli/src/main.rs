//! carafe — те же команды, что у bash-версии `bin/carafe`, поверх carafe-core.

mod account;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carafe_core::download::steamcmd_accounts;
use carafe_core::{
    ActiveSource, AutoDownloader, BridgeState, BridgeTarget, Carafe, CloudMode, Downloader, EngineKind, Error,
    JobState, MoveKind, Queue, QueueEvent, SteamCmdDownloader, human_bytes, parse_appid,
};

const USAGE: &str = "\
Использование:
  carafe engine install [gptk]     поставить последний soju (Wine + D3DMetal из своей копии GPTK);
                                   gptk — прежний движок и источник D3DMetal
  carafe engine update             поставить новый релиз soju, если вышел
  carafe engine status             какие движки стоят, какой активен, D3DMetal
  carafe engine use soju|gptk      сменить движок: бутылки сначала копируются рядом
  carafe bottle create [NAME]      создать префикс Wine (по умолчанию: default)
  carafe bottle restore NAME [КОПИЯ]  вернуть бутылку из резервной копии (по умолчанию свежей)
  carafe login [--json]            войти в Steam по QR из приложения Steam (без пароля)
  carafe logout                    выйти: отозвать вход и стереть его из Связки ключей
  carafe account [--json]          под каким аккаунтом вошёл (без сети)
  carafe steam login USER          войти в steamcmd (пароль и Steam Guard вводишь сам)
  carafe game install APPID [USER] [--to DIR]
                                   скачать Windows-сборку (через очередь): без USER —
                                   с аккаунта из carafe login (SteamKit2), иначе steamcmd
  carafe game verify APPID         проверить файлы игры
  carafe game move APPID DIR       перенести игру в другую библиотеку
  carafe game import APPID [DIR]   забрать depot, скачанный консолью Steam (download_depot)
  carafe game run APPID [ARGS...]  запустить игру (exe ищется в папке игры)
  carafe game list                 установленные игры: мост, правила облака
  carafe game owned [--json]       все купленные игры аккаунта (после carafe login)
  carafe game stats APPID [--json] время в игре и достижения (после carafe login)
  carafe library list              папки библиотек: место на диске, игры
  carafe library add|remove DIR    добавить или убрать папку (файлы не трогаются)
  carafe library default DIR       папка для новых установок
  carafe library du                место по играм и библиотекам
  carafe downloads [list]          очередь загрузок
  carafe downloads run             скачать всё из очереди
  carafe downloads pause|resume|cancel|top APPID
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

/// Позиционные аргументы и `--to DIR`.
fn split_to(args: &[OsString]) -> Result<(Vec<String>, Option<PathBuf>), Fail> {
    let mut pos = vec![];
    let mut to = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let a = a.to_str().ok_or(Fail::Usage)?;
        if a == "--to" {
            to = Some(PathBuf::from(it.next().ok_or(Error::Usage("--to: нужна папка библиотеки"))?));
        } else if let Some(v) = a.strip_prefix("--to=") {
            to = Some(PathBuf::from(v));
        } else if !a.is_empty() {
            pos.push(a.to_string());
        }
    }
    Ok((pos, to))
}

/// Обработать очередь загрузок, печатая прогресс. С USER — через steamcmd, без него — через
/// аккаунт из `carafe login` (SteamKit2), если вход есть, иначе через steamcmd. Если её уже обрабатывает
/// другой процесс (GUI), загрузка пойдёт там.
fn run_downloads(c: &Carafe, user: Option<&str>) -> Result<(), Error> {
    let q = Arc::new(c.queue());
    let weak = Arc::downgrade(&q);
    let tty = std::io::stderr().is_terminal();
    let last = Mutex::new((Instant::now(), None));
    q.set_listener(move |e| {
        let Some(q) = weak.upgrade() else { return };
        if let QueueEvent::Finished { appid, error: Some(e) } = e {
            eprintln!("{}carafe: {appid}: {e}", if tty { "\r\x1b[K" } else { "" });
            return;
        }
        let Some(j) = q.jobs().ok().and_then(|j| j.into_iter().find(|j| j.state == JobState::Active)) else { return };
        let mut last = last.lock().expect("progress");
        let key = (j.appid, j.stage);
        let changed = last.1 != Some(key);
        if !changed && last.0.elapsed() < Duration::from_millis(if tty { 250 } else { 5000 }) {
            return;
        }
        if changed && tty && last.1.is_some() {
            eprintln!();
        }
        *last = (Instant::now(), Some(key));
        let stage = j.stage.map(|s| s.label()).unwrap_or("подготовка");
        let mut line = format!("  {}: {stage}", j.name.as_deref().map(String::from).unwrap_or(j.appid.to_string()));
        if let Some(pct) = (j.done * 100).checked_div(j.total) {
            line += &format!(" {pct}% · {} из {}", human_bytes(j.done), human_bytes(j.total));
        }
        if let Some(s) = j.speed {
            line += &format!(" · {}/с", human_bytes(s));
        }
        if let Some(eta) = j.eta {
            line += &format!(" · {}", eta_text(eta));
        }
        if tty { eprint!("\r\x1b[K{line}") } else { eprintln!("{line}") }
    });
    let dl: Box<dyn Downloader> = match user {
        None => Box::new(AutoDownloader::new(c.clone())),
        Some(u) => Box::new(SteamCmdDownloader::new(c.clone(), Some(u.into()))),
    };
    let r = run_queue(&q, dl.as_ref());
    if tty {
        eprintln!();
    }
    match r {
        Err(Error::QueueBusy) => {
            eprintln!("==> очередь уже обрабатывает другой процесс carafe — загрузка пойдёт там");
            Ok(())
        }
        r => r,
    }
}

fn run_queue(q: &Queue, dl: &dyn Downloader) -> Result<(), Error> {
    q.run_until_empty(dl)?;
    let failed = q.jobs()?.into_iter().filter(|j| j.state == JobState::Failed).count();
    if failed > 0 {
        return Err(Error::Steam {
            code: "failed".into(),
            message: format!("не скачалось: {failed} (carafe downloads)"),
        });
    }
    Ok(())
}

fn eta_text(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s} с"),
        s if s < 3600 => format!("{} мин", s.div_ceil(60)),
        s => format!("{} ч {} мин", s / 3600, s % 3600 / 60),
    }
}

/// `engine status`: активный движок, установленные, D3DMetal и что делать дальше.
fn engine_status(c: &Carafe) {
    let st = c.engine_status();
    let from = match st.source {
        ActiveSource::Env => " (CARAFE_ENGINE_DIR)",
        ActiveSource::Chosen => "",
        ActiveSource::Legacy => " (прежний — выбора не было)",
        ActiveSource::Default => " (по умолчанию)",
    };
    let state = if st.ready { "" } else { " — не установлен" };
    println!("движок: {}{from}{state}\t{}", st.active.label(), st.active.dir.display());
    if st.ready {
        println!("D3DMetal: {}", if st.active.d3dmetal { "подключён" } else { "не подключён" });
    }
    for e in &st.installed {
        let mark = if e.dir == st.active.dir { "*" } else { " " };
        let d3d = if e.d3dmetal { "D3DMetal" } else { "без D3DMetal" };
        println!("{mark} {}\t{d3d}\t{}", e.label(), e.dir.display());
    }
    for h in st.hints() {
        eprintln!("==> {h}");
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
            let kind = arg(args, 2).map(EngineKind::parse).transpose()?.unwrap_or(EngineKind::Soju);
            c.engine_install(kind)?;
        }
        ("engine", "update") => {
            c.soju_install(true)?;
        }
        ("engine", "status") => engine_status(&c),
        ("engine", "use") => {
            c.engine_use(EngineKind::parse(need(args, 2, "нужен движок: soju или gptk")?)?)?;
        }
        ("bottle", "restore") => {
            c.bottle_restore(need(args, 2, "нужно имя бутылки")?, arg(args, 3))?;
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
        ("game", "owned") => account::owned(&c, json_flag(args, 2)?)?,
        ("game", "stats") => account::stats(&c, parse_appid(need(args, 2, "нужен APPID")?)?, json_flag(args, 3)?)?,
        ("game", "install" | "verify") => {
            let id = appid(args, 2)?;
            let (pos, to) = split_to(&args[3..])?;
            let user = pos.first().map(String::as_str);
            if sub == "verify" && (user.is_some() || to.is_some()) {
                return Err(Fail::Usage);
            }
            // Пароль и Steam Guard steamcmd спросит в терминале один раз; дальше — сохранённый вход.
            if let Some(u) = user
                && !steamcmd_accounts(&c.config.steamcmd_dir()).iter().any(|a| a == u)
            {
                c.steam_login(u)?;
            }
            match c.enqueue_game(id, to.as_deref(), sub == "verify") {
                Ok(job) => eprintln!("==> в очереди: {id} → {}", job.dir.display()),
                Err(Error::AlreadyQueued(_)) => eprintln!("==> {id} уже в очереди"),
                Err(e) => return Err(e.into()),
            }
            run_downloads(&c, user)?;
        }
        ("game", "move") => {
            let id = appid(args, 2)?;
            let to = PathBuf::from(need(args, 3, "нужна папка библиотеки")?);
            let tty = std::io::stderr().is_terminal();
            let mut last = Instant::now();
            let (dst, kind) = c.move_game(id, &to, &std::sync::atomic::AtomicBool::new(false), &mut |p| {
                if tty && (last.elapsed() > Duration::from_millis(200) || p.done == p.total) {
                    last = Instant::now();
                    let pct = (p.done * 100).checked_div(p.total).unwrap_or(100);
                    eprint!("\r\x1b[K  {pct}% · {} из {}", human_bytes(p.done), human_bytes(p.total));
                }
            })?;
            if tty && kind == MoveKind::Copy {
                eprintln!();
            }
            eprintln!("==> {id} → {}", dst.display());
        }
        ("library", "list" | "") => {
            for l in c.libraries()? {
                let mark = if l.folder.default { "*" } else { " " };
                let space = match (l.available, l.space) {
                    (false, _) => "недоступна".to_string(),
                    (true, Some(s)) => format!("свободно {} из {}", human_bytes(s.free), human_bytes(s.total)),
                    (true, None) => "место: ?".into(),
                };
                let n = l.folder.apps.len();
                println!("{mark} {}\t{space}\tигр: {n} ({})", l.folder.path.display(), human_bytes(l.used));
            }
        }
        ("library", "add" | "remove" | "default") => {
            let dir = PathBuf::from(need(args, 2, "нужна папка")?);
            let dir = if dir.is_absolute() { dir } else { std::env::current_dir().unwrap_or_default().join(dir) };
            match sub {
                "add" => drop(c.library_add(&dir)?),
                "remove" => c.library_remove(&dir)?,
                _ => c.library_set_default(&dir)?,
            }
        }
        ("library", "du") => {
            let mut sum = std::collections::BTreeMap::<PathBuf, u64>::new();
            for g in c.game_locations()? {
                let size = if g.available { c.game_size(g.appid, true).ok() } else { g.size };
                *sum.entry(g.library.clone()).or_default() += size.unwrap_or(0);
                let name = g.name.or_else(|| c.cached_app_info(g.appid).and_then(|i| i.name));
                let size = size.map(human_bytes).unwrap_or_else(|| "?".into());
                let off = if g.available { "" } else { "\tнедоступна" };
                println!("{}\t{}\t{size}\t{}{off}", g.appid, name.as_deref().unwrap_or("?"), g.library.display());
            }
            for (lib, total) in sum {
                println!("итого\t{}\t{}", human_bytes(total), lib.display());
            }
        }
        ("downloads", "list" | "") => {
            for j in c.queue().jobs()? {
                let state = match j.state {
                    JobState::Queued => "ждёт",
                    JobState::Active => "качается",
                    JobState::Paused => "пауза",
                    JobState::Failed => "ошибка",
                };
                let pct = (j.done * 100).checked_div(j.total).map(|p| format!(" {p}%")).unwrap_or_default();
                let err = j.error.map(|e| format!("\t{e}")).unwrap_or_default();
                println!(
                    "{}\t{}\t{}\t{state}{pct}\t{}{err}",
                    j.appid,
                    j.name.as_deref().unwrap_or("?"),
                    j.kind.as_str(),
                    j.dir.display()
                );
            }
        }
        ("downloads", "run") => run_downloads(&c, None)?,
        ("downloads", "pause" | "resume" | "cancel" | "top") => {
            let id = appid(args, 2)?;
            let q = c.queue();
            match sub {
                "pause" => q.pause(id)?,
                "resume" => q.resume(id)?,
                "cancel" => q.cancel(id)?,
                _ => q.move_to(id, 0)?,
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
