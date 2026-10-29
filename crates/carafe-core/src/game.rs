//! Папка игры: exe, steam_api64.dll, обходы для старого Wine.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{IoContext, Result};
use crate::fsutil::{self, Walk};
use crate::process::Log;

/// Сюда carafe убирает то, что мешает игре: оригинальный steam_api64.dll под мостом, плагины.
pub const DISABLED_DIR: &str = ".carafe-disabled";

/// exe игры: `CARAFE_EXE` относительно папки игры или самый крупный exe верхнего уровня,
/// кроме служебных (краш-хендлер Unity, установщики, redist).
pub fn find_game_exe(dir: &Path, exe_override: Option<&str>) -> Option<PathBuf> {
    if let Some(e) = exe_override {
        return Some(dir.join(e));
    }
    let mut best: Option<(u64, String, PathBuf)> = None;
    for e in fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        if !lower.ends_with(".exe")
            || lower.starts_with("unitycrashhandler")
            || lower.contains("setup")
            || lower.contains("redist")
        {
            continue;
        }
        let Ok(m) = e.metadata() else { continue };
        if !m.is_file() {
            continue;
        }
        // При равном размере — как `sort -rn`: по имени в обратном порядке.
        let key = (m.len(), name);
        if best.as_ref().is_none_or(|(s, n, _)| (key.0, &key.1) > (*s, n)) {
            best = Some((key.0, key.1, e.path()));
        }
    }
    best.map(|(_, _, p)| p)
}

/// steam_api64.dll игры: `live` — тот, что лежит в игре (может быть заглушкой моста),
/// `original` — оригинал Valve (тот же файл или копия в `.carafe-disabled`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteamApiDll {
    pub live: PathBuf,
    pub original: PathBuf,
}

impl SteamApiDll {
    /// Оригинал убран в `.carafe-disabled`: на месте сейчас заглушка моста (или её была).
    pub fn is_swapped(&self) -> bool {
        self.live != self.original
    }
}

/// Оригинальный steam_api64.dll: у Unity — в `*/Plugins/x86_64/`, у остальных — рядом с exe.
pub fn steam_api_dll(dir: &Path) -> Option<SteamApiDll> {
    let live = fsutil::find_first(dir, |p, t| {
        !t.is_dir() && p.file_name().is_some_and(|n| n == "steam_api64.dll") && in_plugins_x64(p)
    })
    .or_else(|| {
        let p = dir.join("steam_api64.dll");
        fs::symlink_metadata(&p).is_ok_and(|m| !m.is_dir()).then_some(p)
    })?;
    let disabled = dir.join(DISABLED_DIR).join("steam_api64.dll");
    let original = if disabled.is_file() { disabled } else { live.clone() };
    Some(SteamApiDll { live, original })
}

/// `find -path '*/Plugins/x86_64/*'`: где-то выше файла есть `Plugins/x86_64`.
fn in_plugins_x64(p: &Path) -> bool {
    p.to_string_lossy().contains("/Plugins/x86_64/")
}

/// IL2CPP-сборки Unity импортируют apiset, которого нет в Wine 7.7 из GPTK; функция живёт в wintypes.dll.
const ROBUFFER: &str = "api-ms-win-core-winrt-robuffer-l1-1-0.dll";
/// Плагин Rewired для Windows.Gaming.Input падает без WinRT; без него Rewired берёт XInput/Raw Input.
const REWIRED_WGI: &str = "Rewired_WindowsGamingInput.dll";

/// Обходы того, чего нет в Wine 7.7 из GPTK. Идемпотентно, вызывается перед запуском.
pub fn apply_quirks(dir: &Path, wine_dlls: &Path, log: &dyn Fn(Log)) -> Result<()> {
    let ga = dir.join("GameAssembly.dll");
    let robuf = dir.join(ROBUFFER);
    if ga.is_file() && !robuf.exists() && fsutil::contains_bytes(&fs::read(&ga).at(&ga)?, ROBUFFER.as_bytes()) {
        log(Log::Step(format!("quirk: {ROBUFFER} → wintypes.dll")));
        fsutil::copy(&wine_dlls.join("wintypes.dll"), &robuf)?;
    }
    let mut wgi = vec![];
    fsutil::walk(dir, &mut |p, _| {
        if p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(REWIRED_WGI)) && in_plugins_x64(p) {
            wgi.push(p.to_path_buf());
        }
        Walk::Continue
    });
    for p in wgi {
        let rel = p.strip_prefix(dir).unwrap_or(&p);
        log(Log::Step(format!("quirk: отключаю {}", rel.display())));
        let to_dir = dir.join(DISABLED_DIR);
        fsutil::create_dir_all(&to_dir)?;
        let to = to_dir.join(p.file_name().unwrap_or_default());
        // mv поверх уже отключённой копии: игру могли обновить вместе с плагином.
        fsutil::remove_all(&to)?;
        fs::rename(&p, &to).at(&to)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;
    use std::cell::RefCell;

    #[test]
    fn biggest_exe_without_helpers() {
        let d = TestDir::new();
        assert_eq!(find_game_exe(d.path(), None), None);
        d.file("UnityCrashHandler64.exe", &[0; 900]);
        d.file("Setup.exe", &[0; 800]);
        d.file("vc_Redist.x64.EXE", &[0; 700]);
        d.file("sub/Huge.exe", &[0; 5000]);
        d.file("Megabonk.exe", &[0; 600]);
        d.file("tool.exe", &[0; 10]);
        d.file("readme.txt", &[0; 9000]);
        assert_eq!(find_game_exe(d.path(), None), Some(d.path().join("Megabonk.exe")));
        d.file("BIG.EXE", &[0; 601]);
        assert_eq!(find_game_exe(d.path(), None), Some(d.path().join("BIG.EXE")));
        // CARAFE_EXE — как есть, относительно папки игры, даже если файла нет.
        assert_eq!(find_game_exe(d.path(), Some("x64/Hades.exe")), Some(d.path().join("x64/Hades.exe")));
    }

    #[test]
    fn exe_tie_like_sort_rn() {
        let d = TestDir::new();
        d.file("A.exe", &[0; 5]);
        d.file("B.exe", &[0; 5]);
        assert_eq!(find_game_exe(d.path(), None), Some(d.path().join("B.exe")));
    }

    #[test]
    fn steam_api_paths() {
        let d = TestDir::new();
        assert_eq!(steam_api_dll(d.path()), None);
        // Рядом с exe (Unreal, свои движки).
        let top = d.file("steam_api64.dll", b"orig");
        assert_eq!(steam_api_dll(d.path()), Some(SteamApiDll { live: top.clone(), original: top.clone() }));
        // Unity: Plugins/x86_64 важнее, где бы ни лежал.
        let unity = d.file("Megabonk_Data/Plugins/x86_64/steam_api64.dll", b"orig");
        let s = steam_api_dll(d.path()).unwrap();
        assert_eq!(s, SteamApiDll { live: unity.clone(), original: unity.clone() });
        assert!(!s.is_swapped());
        // Под мостом оригинал — в .carafe-disabled.
        let orig = d.file(".carafe-disabled/steam_api64.dll", b"orig");
        let s = steam_api_dll(d.path()).unwrap();
        assert_eq!(s, SteamApiDll { live: unity, original: orig });
        assert!(s.is_swapped());
        // Не тот регистр и не тот каталог — не steam_api64.dll Unity.
        let d = TestDir::new();
        d.file("Data/Plugins/x86/steam_api64.dll", b"");
        d.file("Data/Plugins/x86_64/STEAM_API64.DLL", b"");
        assert_eq!(steam_api_dll(d.path()), None);
    }

    #[test]
    fn quirks() {
        let d = TestDir::new();
        let wine = TestDir::new();
        wine.file("wintypes.dll", b"wintypes");
        let logs = RefCell::new(vec![]);
        let log = |l: Log| logs.borrow_mut().push(l);

        d.file("GameAssembly.dll", b"MZ...api-ms-win-core-winrt-robuffer-l1-1-0.dll...");
        d.file("Game_Data/Plugins/x86_64/rewired_windowsgaminginput.dll", b"plugin");
        d.file("Game_Data/Plugins/x86/Rewired_WindowsGamingInput.dll", "x86 — не трогаем".as_bytes());
        apply_quirks(d.path(), wine.path(), &log).unwrap();
        assert_eq!(fs::read(d.path().join(ROBUFFER)).unwrap(), b"wintypes");
        assert_eq!(fs::read(d.path().join(".carafe-disabled/rewired_windowsgaminginput.dll")).unwrap(), b"plugin");
        assert!(!d.path().join("Game_Data/Plugins/x86_64/rewired_windowsgaminginput.dll").exists());
        assert!(d.path().join("Game_Data/Plugins/x86/Rewired_WindowsGamingInput.dll").exists());
        assert_eq!(
            *logs.borrow(),
            [
                Log::Step(format!("quirk: {ROBUFFER} → wintypes.dll")),
                Log::Step("quirk: отключаю Game_Data/Plugins/x86_64/rewired_windowsgaminginput.dll".into()),
            ]
        );
        // Второй раз — ничего.
        logs.borrow_mut().clear();
        apply_quirks(d.path(), wine.path(), &log).unwrap();
        assert!(logs.borrow().is_empty());
    }

    #[test]
    fn no_robuffer_without_import() {
        let d = TestDir::new();
        d.file("GameAssembly.dll", b"MZ");
        apply_quirks(d.path(), Path::new("/нет"), &|_| {}).unwrap();
        assert!(!d.path().join(ROBUFFER).exists());
    }
}
