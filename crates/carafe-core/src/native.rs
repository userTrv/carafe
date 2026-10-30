//! Нативные macOS-игры: у многих купленных игр есть сборка под Mac, и Wine им не нужен.
//!
//! Такая игра лежит в библиотеке так же, как Windows-игра (`<библиотека>/<appid>/`), но в папке
//! есть метка [`NATIVE_FILE`] с платформой и вариантами запуска из appinfo. Метка едет вместе с
//! папкой при переносе и пропадает вместе с ней при удалении. Запуск — сам `.app` (его бинарник
//! из `Contents/MacOS`) без Wine, бутылки и моста. Steamworks в нативной игре говорит с Steam для
//! Mac напрямую, поэтому ему нужен запущенный Steam; облако тоже синхронизирует сам Steam.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::Carafe;
use crate::error::{Error, IoContext, Result};
use crate::fsutil::{self, Walk};
use crate::launch::LaunchEvent;
use crate::process::{self, Log};

/// Метка нативной игры в её папке: JSON с платформой и вариантами запуска.
pub const NATIVE_FILE: &str = ".carafe-native";

/// Для какой платформы стоит (или будет стоять) игра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Windows-сборка через Wine (soju) — как было всегда.
    #[default]
    Windows,
    /// Нативная сборка для macOS.
    Macos,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Windows => "windows",
            Platform::Macos => "macos",
        }
    }

    pub fn parse(s: &str) -> Option<Platform> {
        [Platform::Windows, Platform::Macos].into_iter().find(|p| p.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Windows => "Windows",
            Platform::Macos => "macOS",
        }
    }
}

/// Вариант запуска из appinfo (`config/launch`), уже отобранный хелпером под macOS.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LaunchEntry {
    /// Относительно папки игры: `Game.app` или `Game.app/Contents/MacOS/Game`, или скрипт.
    pub executable: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workingdir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
struct Marker {
    platform: Platform,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    launch: Vec<LaunchEntry>,
}

/// Платформа игры в `dir`: есть метка — macOS, нет — Windows.
pub fn platform_of(dir: &Path) -> Platform {
    read_marker(dir).map(|m| m.platform).unwrap_or_default()
}

fn read_marker(dir: &Path) -> Option<Marker> {
    let text = fs::read_to_string(dir.join(NATIVE_FILE)).ok()?;
    // Повреждённая метка — всё равно нативная: Windows-игре метку никто не пишет.
    Some(serde_json::from_str(&text).unwrap_or(Marker { platform: Platform::Macos, launch: vec![] }))
}

/// Пометить папку как нативную. `launch: None` — оставить уже записанные варианты запуска.
pub fn mark_native(dir: &Path, launch: Option<Vec<LaunchEntry>>) -> Result<()> {
    let old = read_marker(dir).unwrap_or_default();
    let m = Marker { platform: Platform::Macos, launch: launch.unwrap_or(old.launch) };
    let path = dir.join(NATIVE_FILE);
    let text = serde_json::to_string_pretty(&m).expect("marker json");
    fs::write(&path, format!("{text}\n")).at(&path)
}

/// Что запускать: бинарник (в `.app` — `Contents/MacOS/<CFBundleExecutable>`), аргументы из
/// appinfo и рабочая папка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTarget {
    pub program: PathBuf,
    /// `.app`, если запускаем бандл.
    pub bundle: Option<PathBuf>,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// Архитектура Mach-O: Apple Silicon, Intel (через Rosetta) или обе.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MachArch {
    Arm64,
    X86_64,
    Universal,
}

impl MachArch {
    pub fn as_str(self) -> &'static str {
        match self {
            MachArch::Arm64 => "arm64",
            MachArch::X86_64 => "x86_64",
            MachArch::Universal => "universal",
        }
    }

    /// Пойдёт ли на этом Mac без Rosetta.
    pub fn runs_natively(self) -> bool {
        match self {
            MachArch::X86_64 => !cfg!(target_arch = "aarch64"),
            MachArch::Arm64 => cfg!(target_arch = "aarch64"),
            MachArch::Universal => true,
        }
    }
}

/// Что запускать в нативной игре: вариант из appinfo, а без него — `.app` в корне папки
/// (или на уровень глубже), кроме служебных.
pub fn find_target(dir: &Path) -> Option<NativeTarget> {
    let marker = read_marker(dir).unwrap_or_default();
    for l in &marker.launch {
        let rel = l.executable.replace('\\', "/");
        let rel = rel.trim_start_matches("./");
        if rel.split('/').any(|p| p == "..") || rel.starts_with('/') {
            continue;
        }
        let p = dir.join(rel);
        let cwd = match l.workingdir.as_deref().filter(|w| !w.is_empty() && !w.contains("..")) {
            Some(w) => dir.join(w.replace('\\', "/")),
            None => dir.to_path_buf(),
        };
        let args = l.arguments.as_deref().map(split_args).unwrap_or_default();
        if let Some(t) = target_at(&p, args, cwd) {
            return Some(t);
        }
    }
    find_app(dir).and_then(|app| target_at(&app, vec![], dir.to_path_buf()))
}

fn target_at(p: &Path, args: Vec<String>, cwd: PathBuf) -> Option<NativeTarget> {
    if is_bundle(p) {
        let program = bundle_executable(p)?;
        return Some(NativeTarget { program, bundle: Some(p.to_path_buf()), args, cwd });
    }
    // Бинарник внутри .app — запускаем как бандл, чтобы macOS видела его иконку и имя.
    let bundle = p.ancestors().skip(1).find(|a| is_bundle(a)).map(Path::to_path_buf);
    fsutil::is_executable(p).then(|| NativeTarget { program: p.to_path_buf(), bundle, args, cwd })
}

fn is_bundle(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) && p.join("Contents").is_dir()
}

/// `.app` в папке игры: сначала в корне, потом на уровень глубже. Краш-репортеры и лаунчеры
/// пропускаем; из оставшихся — самый крупный (по `Contents/MacOS`).
pub fn find_app(dir: &Path) -> Option<PathBuf> {
    let level = |d: &Path| -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(d) else { return vec![] };
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| is_bundle(p)).collect();
        v.sort();
        v
    };
    let skip = |p: &Path| {
        let n = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        n.contains("crash") || n.contains("uninstall") || n.contains("redist") || n.contains("setup")
    };
    let pick = |v: Vec<PathBuf>| {
        v.into_iter().filter(|p| !skip(p)).max_by_key(|p| crate::library::tree_size(&p.join("Contents/MacOS")))
    };
    if let Some(a) = pick(level(dir)) {
        return Some(a);
    }
    let subdirs: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_bundle(p) && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    pick(subdirs.iter().flat_map(|d| level(d)).collect())
}

/// `Contents/MacOS/<CFBundleExecutable>`; без Info.plist или ключа — единственный файл в MacOS.
fn bundle_executable(app: &Path) -> Option<PathBuf> {
    let macos = app.join("Contents/MacOS");
    let plist = app.join("Contents/Info.plist");
    if let Some(name) = plist_string(&plist, "CFBundleExecutable") {
        let p = macos.join(&name);
        if fsutil::is_executable(&p) {
            return Some(p);
        }
    }
    let mut files: Vec<PathBuf> =
        fs::read_dir(&macos).ok()?.flatten().map(|e| e.path()).filter(|p| fsutil::is_executable(p)).collect();
    files.sort();
    (files.len() == 1).then(|| files.remove(0))
}

/// Строка из plist (XML или бинарного) через `plutil`: он есть в каждой macOS.
fn plist_string(plist: &Path, key: &str) -> Option<String> {
    if !plist.is_file() {
        return None;
    }
    let out = Command::new("/usr/bin/plutil").args(["-extract", key, "raw", "-o", "-"]).arg(plist).output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !s.is_empty() && !s.contains('/')).then_some(s)
}

/// Аргументы из appinfo: через пробел, кавычки группируют.
fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Архитектура Mach-O по заголовку: тонкий 64-битный или fat с несколькими срезами.
pub fn macho_arch(path: &Path) -> Option<MachArch> {
    const CPU_X86_64: u32 = 0x0100_0007;
    const CPU_ARM64: u32 = 0x0100_000c;
    let mut f = fs::File::open(path).ok()?;
    let mut head = [0u8; 8 + 20 * 8];
    let n = f.read(&mut head).ok()?;
    let head = &head[..n];
    let be = |i: usize| head.get(i..i + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()));
    let le = |i: usize| head.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let arches: Vec<u32> = match be(0)? {
        // FAT_MAGIC / FAT_MAGIC_64: число срезов и cputype у каждого (20 или 32 байта на срез).
        0xcafe_babe | 0xcafe_babf => {
            let step = if be(0)? == 0xcafe_babf { 32 } else { 20 };
            let count = be(4)?.min(8) as usize;
            (0..count).filter_map(|i| be(8 + i * step)).collect()
        }
        // MH_MAGIC_64 в little-endian.
        0xcffa_edfe => vec![le(4)?],
        _ => return None,
    };
    let arm = arches.contains(&CPU_ARM64);
    let x86 = arches.contains(&CPU_X86_64);
    match (arm, x86) {
        (true, true) => Some(MachArch::Universal),
        (true, false) => Some(MachArch::Arm64),
        (false, true) => Some(MachArch::X86_64),
        _ => None,
    }
}

/// Есть ли в игре Steamworks (`libsteam_api.dylib`): тогда ей нужен запущенный Steam для Mac.
pub fn uses_steam_api(dir: &Path) -> bool {
    let mut found = false;
    fsutil::walk(dir, &mut |p, _| {
        if p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("libsteam_api.dylib")) {
            found = true;
            return Walk::Stop;
        }
        Walk::Continue
    });
    found
}

/// Запущен ли Steam для Mac.
pub fn steam_running() -> bool {
    Command::new("/usr/bin/pgrep").args(["-x", "steam_osx"]).output().is_ok_and(|o| o.status.success())
}

/// Стоит ли Rosetta: без неё Intel-сборка не запустится.
pub fn rosetta_installed() -> bool {
    Path::new("/Library/Apple/usr/share/rosetta/rosetta").exists()
}

/// Что известно о нативной игре в папке — для списков и проверок перед запуском.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeInfo {
    pub target: Option<NativeTarget>,
    pub arch: Option<MachArch>,
    pub steam_api: bool,
}

pub fn native_info(dir: &Path) -> NativeInfo {
    let target = find_target(dir);
    let arch = target.as_ref().and_then(|t| macho_arch(&t.program));
    NativeInfo { target, arch, steam_api: uses_steam_api(dir) }
}

impl Carafe {
    /// Платформа установленной игры; `None` — не установлена или диск отключён.
    pub fn game_platform(&self, appid: u32) -> Result<Option<Platform>> {
        Ok(self.locate(appid)?.filter(|l| l.available).map(|l| platform_of(&l.dir)))
    }

    /// Запустить нативную игру и дождаться выхода. Без Wine, бутылки и моста: Steamworks игры
    /// говорит с Steam для Mac сам, поэтому нужен запущенный Steam ([`Error::NativeNeedsSteam`]).
    pub(crate) fn run_native(
        &self,
        appid: u32,
        dir: &Path,
        args: &[OsString],
        on_event: &mut dyn FnMut(LaunchEvent),
    ) -> Result<i32> {
        let info = native_info(dir);
        let t = info.target.ok_or_else(|| Error::NativeAppNotFound(dir.to_path_buf()))?;
        if info.arch == Some(MachArch::X86_64) && cfg!(target_arch = "aarch64") && !rosetta_installed() {
            return Err(Error::RosettaMissing);
        }
        if info.steam_api && !steam_running() {
            return Err(Error::NativeNeedsSteam);
        }
        // Steamworks берёт appid из steam_appid.txt в рабочей папке и из SteamAppId: без них игра
        // перезапустилась бы через Steam (SteamAPI_RestartAppIfNecessary).
        crate::steam::write_appid(&t.cwd, appid)?;
        on_event(LaunchEvent::Prepared { exe: t.program.clone(), bridge: None });
        let mut cmd = Command::new(&t.program);
        cmd.args(&t.args).args(args).current_dir(&t.cwd);
        cmd.env("SteamAppId", appid.to_string()).env("SteamGameId", appid.to_string());
        self.log(Log::Step(format!("запускаю нативно {}", t.bundle.as_deref().unwrap_or(&t.program).display())));
        let mut child = cmd.spawn().map_err(process::spawn_err(&cmd))?;
        on_event(LaunchEvent::GameStarted);
        let status = child.wait().map_err(process::spawn_err(&cmd))?;
        let code = process::exit_code(status);
        on_event(LaunchEvent::GameExited { code });
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;
    use std::os::unix::fs::PermissionsExt;

    fn exec(d: &TestDir, rel: &str, body: &[u8]) -> PathBuf {
        let p = d.file(rel, body);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn marker_round_trip() {
        let d = TestDir::new();
        assert_eq!(platform_of(d.path()), Platform::Windows);
        mark_native(d.path(), None).unwrap();
        assert_eq!(platform_of(d.path()), Platform::Macos);
        let l = vec![LaunchEntry { executable: "Game.app".into(), arguments: Some("-x".into()), workingdir: None }];
        mark_native(d.path(), Some(l.clone())).unwrap();
        mark_native(d.path(), None).unwrap();
        assert_eq!(read_marker(d.path()).unwrap().launch, l, "None не стирает варианты запуска");
        d.file(NATIVE_FILE, b"garbage");
        assert_eq!(platform_of(d.path()), Platform::Macos);
        assert_eq!(Platform::parse("macos"), Some(Platform::Macos));
        assert_eq!(Platform::parse("linux"), None);
    }

    #[test]
    fn target_from_bundle() {
        let d = TestDir::new();
        assert_eq!(find_target(d.path()), None);
        // Без Info.plist — единственный исполняемый файл в Contents/MacOS.
        let bin = exec(&d, "Game.app/Contents/MacOS/Game", b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01");
        exec(&d, "CrashReporter.app/Contents/MacOS/CrashReporter", &[0; 4096]);
        let t = find_target(d.path()).unwrap();
        assert_eq!(t.program, bin);
        assert_eq!(t.bundle.as_deref(), Some(d.path().join("Game.app").as_path()));
        assert_eq!(t.cwd, d.path());
        assert_eq!(macho_arch(&t.program), Some(MachArch::Arm64));
    }

    #[test]
    fn target_one_level_down_and_from_launch() {
        let d = TestDir::new();
        let bin = exec(&d, "Hades/Hades.app/Contents/MacOS/Hades", b"x");
        assert_eq!(find_target(d.path()).unwrap().program, bin);
        // Вариант запуска из appinfo главнее поиска: скрипт с аргументами.
        let sh = exec(&d, "run.sh", b"#!/bin/sh\n");
        mark_native(
            d.path(),
            Some(vec![
                LaunchEntry { executable: "../escape".into(), ..Default::default() },
                LaunchEntry { executable: "missing.app".into(), ..Default::default() },
                LaunchEntry {
                    executable: "run.sh".into(),
                    arguments: Some(r#"-a "b c""#.into()),
                    workingdir: Some("Hades".into()),
                },
            ]),
        )
        .unwrap();
        let t = find_target(d.path()).unwrap();
        assert_eq!((t.program, t.args, t.cwd), (sh, vec!["-a".to_string(), "b c".into()], d.path().join("Hades")));
        assert_eq!(t.bundle, None);
    }

    #[test]
    fn plist_executable() {
        let d = TestDir::new();
        d.file(
            "G.app/Contents/Info.plist",
            br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleExecutable</key><string>Real</string></dict></plist>"#,
        );
        exec(&d, "G.app/Contents/MacOS/Other", b"x");
        let real = exec(&d, "G.app/Contents/MacOS/Real", b"x");
        assert_eq!(bundle_executable(&d.path().join("G.app")), Some(real));
    }

    #[test]
    fn mach_headers() {
        let d = TestDir::new();
        let fat = |arches: &[u32]| {
            let mut v = vec![];
            v.extend(0xcafe_babe_u32.to_be_bytes());
            v.extend((arches.len() as u32).to_be_bytes());
            for a in arches {
                v.extend(a.to_be_bytes());
                v.extend([0; 16]);
            }
            v
        };
        let u = d.file("u", &fat(&[0x0100_0007, 0x0100_000c]));
        let x = d.file("x", &fat(&[0x0100_0007]));
        let thin = d.file("t", &[0xcf, 0xfa, 0xed, 0xfe, 0x07, 0, 0, 0x01]);
        let sh = d.file("s", b"#!/bin/sh\n");
        assert_eq!(macho_arch(&u), Some(MachArch::Universal));
        assert_eq!(macho_arch(&x), Some(MachArch::X86_64));
        assert_eq!(macho_arch(&thin), Some(MachArch::X86_64));
        assert_eq!(macho_arch(&sh), None);
        assert!(MachArch::Universal.runs_natively());
    }

    #[test]
    fn run_without_wine() {
        let home = TestDir::new();
        let c = crate::library::tests::carafe(home.path());
        let dir = home.path().join("games/413150");
        exec(
            &home,
            "games/413150/G.app/Contents/MacOS/G",
            b"#!/bin/sh\n[ \"$SteamAppId\" = 413150 ] || exit 9\nexit 3\n",
        );
        mark_native(&dir, None).unwrap();
        let mut ev = vec![];
        // Движка и бутылки нет — нативной игре они и не нужны.
        assert_eq!(c.run_game(413150, &[], &mut |e| ev.push(e)).unwrap(), 3);
        assert!(matches!(ev[0], LaunchEvent::Prepared { bridge: None, .. }));
        assert_eq!(ev[1..], [LaunchEvent::GameStarted, LaunchEvent::GameExited { code: 3 }]);
        assert_eq!(fs::read_to_string(dir.join("steam_appid.txt")).unwrap(), "413150\n");
        let g = c.game(413150).unwrap();
        assert_eq!((g.platform, g.exe), (Platform::Macos, Some(dir.join("G.app/Contents/MacOS/G"))));

        // Со Steamworks и без запущенного Steam — понятная ошибка, а не тихий вылет игры.
        home.file("games/413150/G.app/Contents/Frameworks/libsteam_api.dylib", b"");
        if !steam_running() {
            assert!(matches!(c.run_game(413150, &[], &mut |_| {}), Err(Error::NativeNeedsSteam)));
        }
    }

    #[test]
    fn steam_api_and_args() {
        let d = TestDir::new();
        assert!(!uses_steam_api(d.path()));
        d.file("G.app/Contents/Frameworks/libsteam_api.dylib", b"");
        assert!(uses_steam_api(d.path()));
        assert_eq!(split_args(r#"  -w 1  "" -q"#), ["-w", "1", "", "-q"]);
        assert!(split_args("").is_empty());
    }
}
