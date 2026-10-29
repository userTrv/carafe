//! Мост Steamworks в Steam для Mac. Собирает и проверяет его soju (`bin/soju-bridge`), carafe
//! кладёт собранное в `bridge/<sha256 оригинального steam_api64.dll>` и подменяет DLL в игре.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Carafe;
use crate::error::{Error, Result};
use crate::fsutil;
use crate::game::{self, DISABLED_DIR, SteamApiDll};
use crate::process::Log;

/// Для чего собирать мост: игра или любой steam_api64.dll (проверить мост на SDK без игры).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeTarget {
    App(u32),
    Dll(PathBuf),
}

impl BridgeTarget {
    /// Аргумент CLI: существующий файл — DLL, иначе APPID.
    pub fn parse(arg: &str) -> Result<BridgeTarget> {
        if arg.is_empty() {
            return Err(Error::Usage("нужен APPID или путь к steam_api64.dll"));
        }
        if Path::new(arg).is_file() {
            return Ok(BridgeTarget::Dll(arg.into()));
        }
        // Не число и не файл — в bash это была бы «игра» без steam_api64.dll.
        arg.parse().map(BridgeTarget::App).map_err(|_| Error::NoBridgeNeeded)
    }
}

/// Состояние моста у игры.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeState {
    /// В игре нет steam_api64.dll.
    NotNeeded,
    /// Нужен, но ещё не собран: `carafe bridge build`.
    NotBuilt,
    Built {
        dir: PathBuf,
    },
}

impl Carafe {
    fn soju_bridge(&self) -> Result<Command> {
        Ok(Command::new(self.config.soju_dir()?.join("bin/soju-bridge")))
    }

    /// Оригинальный steam_api64.dll для моста.
    pub fn bridge_source_dll(&self, target: &BridgeTarget) -> Result<PathBuf> {
        match target {
            BridgeTarget::Dll(p) => Ok(p.clone()),
            BridgeTarget::App(appid) => {
                game::steam_api_dll(&self.config.game_dir(*appid)).map(|s| s.original).ok_or(Error::NoBridgeNeeded)
            }
        }
    }

    /// Каталог моста под этот DLL (может быть ещё не собран).
    pub fn bridge_dir(&self, original_dll: &Path) -> Result<PathBuf> {
        Ok(self.config.bridges().join(fsutil::sha256_file(original_dll)?))
    }

    pub fn bridge_state(&self, dll: Option<&SteamApiDll>) -> Result<BridgeState> {
        let Some(dll) = dll else { return Ok(BridgeState::NotNeeded) };
        let dir = self.bridge_dir(&dll.original)?;
        Ok(if fsutil::is_executable(&dir.join("soju-steamd")) {
            BridgeState::Built { dir }
        } else {
            BridgeState::NotBuilt
        })
    }

    /// Собрать мост (soju-bridge build). Возвращает его каталог.
    pub fn bridge_build(&self, target: &BridgeTarget) -> Result<PathBuf> {
        let orig = self.bridge_source_dll(target)?;
        let dir = self.bridge_dir(&orig)?;
        let mut cmd = self.soju_bridge()?;
        // stdout soju-bridge build — путь к мосту, он и так известен.
        cmd.arg("build").arg(&orig).arg(&dir).stdout(Stdio::null());
        self.run_logged(&mut cmd)?;
        self.log(Log::Step(format!("мост готов: {}", dir.display())));
        Ok(dir)
    }

    /// Проверка моста на Spacewar (480) против нативного Steam — в soju, на его движке.
    pub fn bridge_test(&self, target: &BridgeTarget) -> Result<()> {
        let orig = self.bridge_source_dll(target)?;
        let mut cmd = self.soju_bridge()?;
        cmd.arg("test").arg(&orig);
        self.run_logged(&mut cmd)
    }

    /// Поставить DLL моста в игру (или вернуть оригинал, если мост выключен или не собран).
    /// Возвращает каталог моста, если он включён.
    pub fn apply_bridge(&self, game_dir: &Path) -> Result<Option<PathBuf>> {
        let Some(dll) = game::steam_api_dll(game_dir) else { return Ok(None) };
        let b = self.bridge_dir(&dll.original)?;
        if !self.config.bridge || !fsutil::is_executable(&b.join("soju-steamd")) {
            if dll.is_swapped() {
                self.log(Log::Step("мост выключен: возвращаю оригинальный steam_api64.dll".into()));
                fsutil::copy(&dll.original, &dll.live)?;
            }
            return Ok(None);
        }
        if !dll.is_swapped() {
            let keep = game_dir.join(DISABLED_DIR);
            fsutil::create_dir_all(&keep)?;
            fsutil::copy(&dll.live, &keep.join("steam_api64.dll"))?;
        }
        let stub = b.join("steam_api64.dll");
        if !fsutil::same_contents(&stub, &dll.live) {
            fsutil::copy(&stub, &dll.live)?;
        }
        Ok(Some(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::fsutil::testdir::TestDir;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn carafe(home: &Path, bridge: bool) -> Carafe {
        let mut c = Config::from_vars(|k| (k == "HOME").then(|| "/h".into())).unwrap();
        c.home = home.into();
        c.bridge = bridge;
        Carafe::new(c).with_logger(|_| {})
    }

    #[test]
    fn target_parse() {
        let d = TestDir::new();
        let f = d.file("steam_api64.dll", b"");
        assert_eq!(BridgeTarget::parse(f.to_str().unwrap()).unwrap(), BridgeTarget::Dll(f));
        assert_eq!(BridgeTarget::parse("3405340").unwrap(), BridgeTarget::App(3405340));
        assert!(matches!(BridgeTarget::parse("/нет/steam_api64.dll"), Err(Error::NoBridgeNeeded)));
        assert!(matches!(BridgeTarget::parse(""), Err(Error::Usage(_))));
    }

    #[test]
    fn swap_and_restore() {
        let home = TestDir::new();
        let c = carafe(home.path(), true);
        let game = c.config.game_dir(1);
        assert_eq!(c.apply_bridge(&game).unwrap(), None);

        let live = home.file("games/1/Game_Data/Plugins/x86_64/steam_api64.dll", b"valve");
        // Мост не собран — ничего не трогаем.
        assert_eq!(c.apply_bridge(&game).unwrap(), None);
        assert_eq!(fs::read(&live).unwrap(), b"valve");

        let b = c.bridge_dir(&live).unwrap();
        let rel = b.strip_prefix(home.path()).unwrap().to_str().unwrap().to_string();
        home.file(&format!("{rel}/steam_api64.dll"), b"stub");
        let steamd = home.file(&format!("{rel}/soju-steamd"), b"");
        fs::set_permissions(&steamd, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(c.bridge_state(game::steam_api_dll(&game).as_ref()).unwrap(), BridgeState::Built { dir: b.clone() });

        assert_eq!(c.apply_bridge(&game).unwrap(), Some(b.clone()));
        assert_eq!(fs::read(&live).unwrap(), b"stub");
        assert_eq!(fs::read(game.join(".carafe-disabled/steam_api64.dll")).unwrap(), b"valve");
        // Повторно: тот же мост — по sha оригинала, а не заглушки.
        assert_eq!(c.apply_bridge(&game).unwrap(), Some(b));

        // CARAFE_BRIDGE=0 — оригинал обратно.
        let off = carafe(home.path(), false);
        assert_eq!(off.apply_bridge(&game).unwrap(), None);
        assert_eq!(fs::read(&live).unwrap(), b"valve");
    }
}
