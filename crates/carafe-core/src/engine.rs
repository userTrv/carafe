//! Движок (Wine + D3DMetal), окружение wine и бутылки.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Error, IoContext, Result};
use crate::fsutil::{self, Walk};
use crate::process::{self, Log};
use crate::{Carafe, Config};

/// Движок по умолчанию: сборка Wine от Gcenx с D3DMetal из Apple Game Porting Toolkit 3.0.
pub const ENGINE_NAME: &str = "gptk-3.0-3";
pub const ENGINE_URL: &str = "https://github.com/Gcenx/game-porting-toolkit/releases/download/Game-Porting-Toolkit-3.0-3/game-porting-toolkit-3.0-3.tar.xz";
pub const ENGINE_SHA256: &str = "d377683937340f914823dbb2e1252b329cbf834ff58907d0293db8cebf0e392e";

/// wine движка в порядке предпочтения: `bin/soju` — wine движка soju с его настройками
/// (msync, D3DMetal).
const WINE_NAMES: [&str; 3] = ["soju", "wine64", "wine"];

/// Исполняемый `*/bin/<имя>` где угодно в движке: у GPTK он лежит глубоко внутри .app.
pub fn find_wine(engine_dir: &Path) -> Result<PathBuf> {
    if !engine_dir.is_dir() {
        return Err(Error::EngineMissing);
    }
    let mut found: [Option<PathBuf>; 3] = Default::default();
    fsutil::walk(engine_dir, &mut |p, t| {
        if t.is_file()
            && p.parent().and_then(Path::file_name) == Some(OsStr::new("bin"))
            && let Some(i) = WINE_NAMES.iter().position(|n| p.file_name() == Some(OsStr::new(n)))
            && found[i].is_none()
            && fsutil::is_executable(p)
        {
            found[i] = Some(p.to_path_buf());
        }
        if found[0].is_some() { Walk::Stop } else { Walk::Continue }
    });
    found.into_iter().flatten().next().ok_or_else(|| Error::WineNotFound(engine_dir.into()))
}

/// wine движка с окружением бутылки.
#[derive(Debug, Clone)]
pub struct Wine {
    pub bin: PathBuf,
    pub prefix: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

impl Wine {
    /// Корень движка: `bin/..`.
    pub fn root(&self) -> &Path {
        self.bin.parent().and_then(Path::parent).unwrap_or(Path::new("/"))
    }

    /// Встроенные DLL Wine под Windows x64 — из них берутся заплатки для игр.
    pub fn windows_dlls(&self) -> PathBuf {
        self.root().join("lib/wine/x86_64-windows")
    }

    /// wineserver рядом с wine; bash звал его из PATH, где первым стоит тот же bin.
    pub fn wineserver(&self) -> PathBuf {
        let s = self.bin.with_file_name("wineserver");
        if s.exists() { s } else { "wineserver".into() }
    }

    /// Команда в окружении бутылки.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        c.envs(self.env.iter().map(|(k, v)| (k, v)));
        c
    }

    /// `wine ARGS…`.
    pub fn wine(&self) -> Command {
        self.command(&self.bin)
    }
}

/// Переменные для wine: префикс бутылки, тихие логи, msync/esync, HUD и bin движка первым в PATH.
/// Уже заданные WINEDEBUG/WINEMSYNC/WINEESYNC не трогаются.
pub fn wine_env(
    config: &Config,
    prefix: &Path,
    wine_bin: &Path,
    get: impl Fn(&str) -> Option<OsString>,
) -> Vec<(OsString, OsString)> {
    let var = |k: &str| get(k).filter(|v| !v.is_empty());
    let mut env: Vec<(OsString, OsString)> = vec![("WINEPREFIX".into(), prefix.into())];
    for (k, default) in [("WINEDEBUG", "-all"), ("WINEMSYNC", "1"), ("WINEESYNC", "1")] {
        env.push((k.into(), var(k).unwrap_or_else(|| default.into())));
    }
    if config.hud {
        env.push(("MTL_HUD_ENABLED".into(), "1".into()));
    }
    let mut path: OsString = wine_bin.parent().unwrap_or(Path::new("/")).into();
    if let Some(p) = get("PATH") {
        path.push(":");
        path.push(p);
    }
    env.push(("PATH".into(), path));
    env
}

impl Carafe {
    /// wine текущей бутылки.
    pub fn wine(&self) -> Result<Wine> {
        self.wine_for(&self.config.prefix())
    }

    fn wine_for(&self, prefix: &Path) -> Result<Wine> {
        let bin = find_wine(&self.config.engine_dir())?;
        let env = wine_env(&self.config, prefix, &bin, |k| std::env::var_os(k));
        Ok(Wine { bin, prefix: prefix.into(), env })
    }

    /// Скачать GPTK-сборку Wine, сверить sha256, распаковать и снять карантин. Возвращает путь к wine.
    pub fn engine_install(&self) -> Result<PathBuf> {
        let dl = self.config.engines().join("dl");
        let file = ENGINE_URL.rsplit('/').next().unwrap_or(ENGINE_NAME);
        let tarball = dl.join(file);
        fsutil::create_dir_all(&dl)?;
        if !tarball.is_file() {
            self.log(Log::Step(format!("качаю движок {file}")));
            let part = dl.join(format!("{file}.part"));
            process::run_inherit(
                Command::new("curl").args(["-fL", "--retry", "5", "-C", "-", "-o"]).arg(&part).arg(ENGINE_URL),
            )?;
            fs::rename(&part, &tarball).at(&tarball)?;
        }
        if fsutil::sha256_file(&tarball)? != ENGINE_SHA256 {
            return Err(Error::EngineChecksum);
        }
        let d = self.config.engine_dir();
        let tmp = with_suffix(&d, ".tmp");
        fsutil::remove_all(&tmp)?;
        fsutil::create_dir_all(&tmp)?;
        self.log(Log::Step(format!("распаковываю в {}", d.display())));
        process::run_inherit(Command::new("tar").arg("-xJf").arg(&tarball).arg("-C").arg(&tmp))?;
        // Карантин macOS блокирует запуск скачанных бинарников.
        remove_quarantine(&tmp);
        fsutil::remove_all(&d)?;
        fs::rename(&tmp, &d).at(&d)?;
        let wine = find_wine(&d)?;
        self.log(Log::Step(format!("готово: {}", wine.display())));
        Ok(wine)
    }

    /// Создать префикс Wine: `name` или текущая бутылка. Windows 10 — то, что ждут современные
    /// Unity-игры.
    pub fn bottle_create(&self, name: Option<&str>) -> Result<PathBuf> {
        let prefix = self.config.bottle_dir(name.unwrap_or(&self.config.bottle));
        let wine = self.wine_for(&prefix)?;
        self.log(Log::Step(format!("создаю префикс {}", prefix.display())));
        fsutil::create_dir_all(&prefix)?;
        self.run_logged(wine.wine().args(["wineboot", "--init"]))?;
        self.run_logged(
            wine.wine()
                .args(["reg", "add", r"HKCU\Software\Wine", "/v", "Version", "/d", "win10", "/f"])
                .stdout(Stdio::null()),
        )?;
        self.run_logged(wine.command(wine.wineserver()).arg("-w"))?;
        self.log(Log::Step("префикс готов".into()));
        Ok(prefix)
    }

    /// Команда `wine EXE ARGS…` из папки exe — так игры находят свои файлы рядом с собой.
    /// Префикс создаётся, если его ещё нет.
    pub fn exe_command(&self, exe: &Path, args: &[OsString]) -> Result<Command> {
        if !exe.is_file() {
            return Err(Error::NoSuchFile(exe.into()));
        }
        let wine = self.wine()?;
        if !wine.prefix.is_dir() {
            self.bottle_create(None)?;
        }
        let mut cmd = wine.wine();
        cmd.current_dir(exe_dir(exe)).arg(exe.file_name().unwrap_or(exe.as_os_str())).args(args);
        Ok(cmd)
    }
}

pub(crate) fn exe_dir(exe: &Path) -> &Path {
    match exe.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

pub(crate) fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

/// `xattr -dr com.apple.quarantine`; атрибута может не быть — это не ошибка.
pub(crate) fn remove_quarantine(p: &Path) {
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(p)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;
    use std::os::unix::fs::PermissionsExt;

    fn exec(d: &TestDir, rel: &str) -> PathBuf {
        let p = d.file(rel, b"#!/bin/sh\n");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn wine_priority() {
        let d = TestDir::new();
        assert!(matches!(find_wine(&d.path().join("нет")), Err(Error::EngineMissing)));
        assert!(matches!(find_wine(d.path()), Err(Error::WineNotFound(_))));
        // GPTK: wine64 глубоко внутри .app; не исполняемый и не в bin/ — мимо.
        d.file("a/bin/wine", b"");
        exec(&d, "a/lib/wine");
        let w64 = exec(&d, "Game Porting Toolkit.app/Contents/Resources/wine/bin/wine64");
        assert_eq!(find_wine(d.path()).unwrap(), w64);
        let w = exec(&d, "z/bin/wine");
        assert_eq!(find_wine(d.path()).unwrap(), w64);
        fs::remove_file(&w64).unwrap();
        assert_eq!(find_wine(d.path()).unwrap(), w);
        // soju: bin/soju важнее wine рядом с ним.
        let soju = exec(&d, "z/bin/soju");
        assert_eq!(find_wine(d.path()).unwrap(), soju);
    }

    #[test]
    fn env_defaults_and_overrides() {
        let mut cfg = Config::from_vars(|k| (k == "HOME").then(|| "/h".into())).unwrap();
        let prefix = cfg.prefix();
        let bin = Path::new("/e/bin/soju");
        let get = |k: &str| match k {
            "PATH" => Some("/usr/bin:/bin".into()),
            "WINEDEBUG" => Some("+loaddll".into()),
            "WINEESYNC" => Some("".into()),
            _ => None,
        };
        let env = wine_env(&cfg, &prefix, bin, get);
        let s = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_str().unwrap().to_string());
        assert_eq!(s("WINEPREFIX").unwrap(), "/h/Library/Application Support/carafe/bottles/default");
        assert_eq!(s("WINEDEBUG").unwrap(), "+loaddll");
        assert_eq!(s("WINEMSYNC").unwrap(), "1");
        assert_eq!(s("WINEESYNC").unwrap(), "1");
        assert_eq!(s("PATH").unwrap(), "/e/bin:/usr/bin:/bin");
        assert_eq!(s("MTL_HUD_ENABLED"), None);
        cfg.hud = true;
        let env = wine_env(&cfg, &prefix, bin, |_| None);
        assert!(env.iter().any(|(k, v)| k == "MTL_HUD_ENABLED" && v == "1"));
        assert!(env.iter().any(|(k, v)| k == "PATH" && v == "/e/bin"));
    }

    #[test]
    fn exe_dir_of_bare_name() {
        assert_eq!(exe_dir(Path::new("Game.exe")), Path::new("."));
        assert_eq!(exe_dir(Path::new("/g/Game.exe")), Path::new("/g"));
    }
}
