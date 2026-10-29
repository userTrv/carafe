use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub const DEFAULT_BOTTLE: &str = "default";

/// Настройки carafe. CLI берёт их из переменных `CARAFE_*` ([`Config::from_env`]), GUI может
/// собрать сам. Семантика флагов — как в bash-версии: `CARAFE_BRIDGE=0` выключает мост,
/// облако включено только при `CARAFE_CLOUD=1` (или без переменной), HUD — при `CARAFE_HUD=1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `CARAFE_HOME`, по умолчанию `~/Library/Application Support/carafe`.
    pub home: PathBuf,
    /// `CARAFE_BOTTLE` — префикс Wine.
    pub bottle: String,
    /// `CARAFE_ENGINE_DIR` — свой движок вместо скачанного, например `dist/soju-engine` из soju.
    pub engine_dir: Option<PathBuf>,
    /// `CARAFE_SOJU` — checkout soju; без него ищется `soju/` рядом с carafe.
    pub soju: Option<PathBuf>,
    /// `CARAFE_BRIDGE`.
    pub bridge: bool,
    /// `CARAFE_CLOUD`.
    pub cloud: bool,
    /// `CARAFE_EXE` — exe игры относительно её папки, если автоопределение ошиблось.
    pub exe: Option<String>,
    /// `CARAFE_HUD`.
    pub hud: bool,
    /// Steam для Mac: `game import` ищет в нём depot, скачанные через консоль.
    pub steam_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Config> {
        Self::from_vars(|k| std::env::var_os(k))
    }

    /// То же из произвольного источника переменных — для тестов и GUI.
    pub fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> Result<Config> {
        // Пустая переменная — как незаданная: так ведёт себя `${X:-…}` в bash.
        let var = |k: &str| get(k).filter(|v| !v.is_empty());
        let is = |k: &str, v: &str| var(k).is_some_and(|x| x == v);
        let support = var("HOME").map(PathBuf::from).ok_or(Error::NoHome)?.join("Library/Application Support");
        Ok(Config {
            home: var("CARAFE_HOME").map(PathBuf::from).unwrap_or_else(|| support.join("carafe")),
            bottle: var("CARAFE_BOTTLE")
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_else(|| DEFAULT_BOTTLE.into()),
            engine_dir: var("CARAFE_ENGINE_DIR").map(PathBuf::from),
            soju: var("CARAFE_SOJU").map(PathBuf::from),
            bridge: !is("CARAFE_BRIDGE", "0"),
            cloud: var("CARAFE_CLOUD").is_none() || is("CARAFE_CLOUD", "1"),
            exe: var("CARAFE_EXE").map(|v| v.to_string_lossy().into_owned()),
            hud: is("CARAFE_HUD", "1"),
            steam_dir: support.join("Steam"),
        })
    }

    pub fn engines(&self) -> PathBuf {
        self.home.join("engines")
    }

    pub fn bottles(&self) -> PathBuf {
        self.home.join("bottles")
    }

    pub fn games(&self) -> PathBuf {
        self.home.join("games")
    }

    pub fn steamcmd_dir(&self) -> PathBuf {
        self.home.join("steamcmd")
    }

    /// Собранные мосты: `bridge/<sha256 оригинального steam_api64.dll>`.
    pub fn bridges(&self) -> PathBuf {
        self.home.join("bridge")
    }

    pub fn backups(&self, appid: u32) -> PathBuf {
        self.home.join("backups").join(appid.to_string())
    }

    pub fn engine_dir(&self) -> PathBuf {
        self.engine_dir.clone().unwrap_or_else(|| self.engines().join(crate::engine::ENGINE_NAME))
    }

    pub fn bottle_dir(&self, name: &str) -> PathBuf {
        self.bottles().join(name)
    }

    /// WINEPREFIX текущей бутылки.
    pub fn prefix(&self) -> PathBuf {
        self.bottle_dir(&self.bottle)
    }

    pub fn game_dir(&self, appid: u32) -> PathBuf {
        self.games().join(appid.to_string())
    }

    pub fn bridge_log(&self, appid: u32) -> PathBuf {
        self.home.join(format!("bridge-{appid}.log"))
    }

    /// Каталог soju: `CARAFE_SOJU` или `soju/` рядом с carafe — первый предок бинарника,
    /// у которого есть `soju/bin/soju-bridge` (`~/Projects/carafe/target/release/carafe` →
    /// `~/Projects/soju`).
    pub fn soju_dir(&self) -> Result<PathBuf> {
        let found = match &self.soju {
            Some(d) => Some(d.clone()),
            None => std::env::current_exe()
                .ok()
                .and_then(|e| e.canonicalize().ok())
                .and_then(|e| e.ancestors().map(|a| a.join("soju")).find(|s| is_soju(s))),
        };
        found.filter(|d| is_soju(d)).ok_or(Error::SojuMissing)
    }
}

fn is_soju(dir: &Path) -> bool {
    crate::fsutil::is_executable(&dir.join("bin/soju-bridge"))
}

/// APPID из аргумента командной строки или GUI.
pub fn parse_appid(s: &str) -> Result<u32> {
    s.parse().map_err(|_| Error::BadAppId(s.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(vars: &[(&str, &str)]) -> Config {
        Config::from_vars(|k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.into())).unwrap()
    }

    #[test]
    fn defaults() {
        let c = cfg(&[("HOME", "/Users/u")]);
        assert_eq!(c.home, PathBuf::from("/Users/u/Library/Application Support/carafe"));
        assert_eq!(c.bottle, "default");
        assert!(c.bridge && c.cloud && !c.hud);
        assert_eq!(c.engine_dir(), c.home.join("engines/gptk-3.0-3"));
        assert_eq!(c.prefix(), c.home.join("bottles/default"));
        assert_eq!(c.steam_dir, PathBuf::from("/Users/u/Library/Application Support/Steam"));
    }

    #[test]
    fn flags_like_bash() {
        let c = cfg(&[("HOME", "/h"), ("CARAFE_BRIDGE", "1"), ("CARAFE_CLOUD", "0"), ("CARAFE_HUD", "1")]);
        assert!(c.bridge && !c.cloud && c.hud);
        // `[[ $CARAFE_CLOUD == 1 ]]`: всё, кроме 1, выключает облако; `!= 0` — у моста.
        let c = cfg(&[("HOME", "/h"), ("CARAFE_BRIDGE", "no"), ("CARAFE_CLOUD", "yes")]);
        assert!(c.bridge && !c.cloud);
        // Пустые — как незаданные.
        let c = cfg(&[("HOME", "/h"), ("CARAFE_CLOUD", ""), ("CARAFE_BOTTLE", ""), ("CARAFE_BRIDGE", "0")]);
        assert!(c.cloud && !c.bridge);
        assert_eq!(c.bottle, "default");
    }

    #[test]
    fn overrides() {
        let c = cfg(&[
            ("HOME", "/h"),
            ("CARAFE_HOME", "/c"),
            ("CARAFE_BOTTLE", "soju-test"),
            ("CARAFE_ENGINE_DIR", "/e"),
            ("CARAFE_EXE", "Game.exe"),
        ]);
        assert_eq!(c.prefix(), PathBuf::from("/c/bottles/soju-test"));
        assert_eq!(c.engine_dir(), PathBuf::from("/e"));
        assert_eq!(c.exe.as_deref(), Some("Game.exe"));
        assert_eq!(c.game_dir(480), PathBuf::from("/c/games/480"));
        assert_eq!(c.bridge_log(480), PathBuf::from("/c/bridge-480.log"));
    }

    #[test]
    fn appid() {
        assert_eq!(parse_appid("3405340").unwrap(), 3405340);
        assert!(matches!(parse_appid("megabonk"), Err(Error::BadAppId(_))));
    }
}
