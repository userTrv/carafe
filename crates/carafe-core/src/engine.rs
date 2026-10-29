//! Движок (Wine + D3DMetal), окружение wine и бутылки.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Error, IoContext, Result};
use crate::fsutil::{self, Walk};
use crate::process::{self, Log};
use crate::{Carafe, Config};

/// Запасной движок и источник D3DMetal: сборка Wine 7.7 от Gcenx с D3DMetal из Apple Game
/// Porting Toolkit 3.0. Движок по умолчанию — soju из релизов `userTrv/soju` ([`crate::release`]).
pub const GPTK_NAME: &str = "gptk-3.0-3";
pub const GPTK_URL: &str = "https://github.com/Gcenx/game-porting-toolkit/releases/download/Game-Porting-Toolkit-3.0-3/game-porting-toolkit-3.0-3.tar.xz";
pub const GPTK_SHA256: &str = "d377683937340f914823dbb2e1252b329cbf834ff58907d0293db8cebf0e392e";

/// Движки soju живут в `engines/soju-<версия>`.
pub const SOJU_PREFIX: &str = "soju-";
/// `engines/current` — имя каталога активного движка. Его пишут `engine use` и первая установка
/// soju у нового пользователя; `CARAFE_ENGINE_DIR` главнее.
pub const CURRENT_FILE: &str = "current";
/// `bottles/<имя>/.carafe-engine` — каким движком бутылку создали или обновили последним.
pub const BOTTLE_ENGINE_FILE: &str = ".carafe-engine";

pub const D3DMETAL_MISSING: &str = "D3DMetal не подключён: нужна своя копия Game Porting Toolkit — поставь движок GPTK как источник: carafe engine install gptk";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineKind {
    Soju,
    Gptk,
}

impl EngineKind {
    pub fn parse(s: &str) -> Result<EngineKind> {
        match s {
            "soju" => Ok(EngineKind::Soju),
            "gptk" => Ok(EngineKind::Gptk),
            _ => Err(Error::UnknownEngine(s.into())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Soju => "soju",
            EngineKind::Gptk => "gptk",
        }
    }

    /// Вид движка по имени каталога в `engines/`.
    pub fn of_dir(dir: &Path) -> Option<EngineKind> {
        let name = dir.file_name()?.to_str()?;
        if name.starts_with(SOJU_PREFIX) || name == "soju-engine" || dir.join("SOJU").is_file() {
            Some(EngineKind::Soju)
        } else if name.starts_with("gptk-") {
            Some(EngineKind::Gptk)
        } else {
            None
        }
    }
}

/// Установленный движок.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledEngine {
    /// Имя каталога в `engines/`: `soju-26.3.0`, `gptk-3.0-3`.
    pub name: String,
    pub dir: PathBuf,
    pub kind: Option<EngineKind>,
    pub version: Option<String>,
    pub d3dmetal: bool,
}

impl InstalledEngine {
    pub fn of(dir: &Path) -> InstalledEngine {
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let kind = EngineKind::of_dir(dir);
        let version = match kind {
            Some(EngineKind::Soju) => soju_field(dir, "version")
                .or_else(|| name.strip_prefix(SOJU_PREFIX).filter(|v| *v != "engine").map(String::from)),
            // gptk-3.0-3: 3.0 — версия GPTK, 3 — сборка Gcenx.
            Some(EngineKind::Gptk) => {
                let v = name.trim_start_matches("gptk-");
                Some(v.rsplit_once('-').map_or(v, |(v, _)| v).to_string())
            }
            None => None,
        };
        InstalledEngine { name, dir: dir.into(), kind, version, d3dmetal: has_d3dmetal(dir) }
    }

    /// Для людей: «soju 26.3.0», «GPTK 3.0».
    pub fn label(&self) -> String {
        match (self.kind, &self.version) {
            (Some(EngineKind::Soju), Some(v)) => format!("soju {v}"),
            (Some(EngineKind::Soju), None) => "soju".into(),
            (Some(EngineKind::Gptk), Some(v)) => format!("GPTK {v}"),
            _ => self.name.clone(),
        }
    }
}

/// Строка `ключ значение` из файла `SOJU` движка.
fn soju_field(dir: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(dir.join("SOJU")).ok()?;
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' ').map(|v| v.trim().to_string()))
}

/// Движок soju целиком на месте: `bin/soju` исполняемый.
pub fn is_soju_engine(dir: &Path) -> bool {
    fsutil::is_executable(&dir.join("bin/soju"))
}

/// D3DMetal подключён: у soju — `lib/external/libd3dshared.dylib`, у GPTK — внутри .app.
pub fn has_d3dmetal(dir: &Path) -> bool {
    if dir.join("lib/external/libd3dshared.dylib").is_file() {
        return true;
    }
    let mut found = false;
    let depth = |p: &Path| p.strip_prefix(dir).map(|r| r.components().count()).unwrap_or(0);
    fsutil::walk(dir, &mut |p, t| {
        if !t.is_dir() || depth(p) > 7 {
            return Walk::Skip;
        }
        if p.file_name() == Some(OsStr::new("D3DMetal.framework")) {
            found = true;
            return Walk::Stop;
        }
        Walk::Continue
    });
    found
}

/// Копия GPTK из этого carafe — источник D3DMetal для `soju-d3dmetal`.
pub fn gptk_d3dmetal_source(engines: &Path) -> Option<PathBuf> {
    installed_engines(engines)
        .into_iter()
        .filter(|e| e.kind == Some(EngineKind::Gptk) && e.d3dmetal)
        .map(|e| e.dir.join("Game Porting Toolkit.app"))
        .find(|p| p.is_dir())
}

/// Движки в `engines/`: soju от новых к старым, потом GPTK. Служебные (`dl`, `.…tmp`) — мимо.
pub fn installed_engines(engines: &Path) -> Vec<InstalledEngine> {
    let mut list: Vec<InstalledEngine> = fs::read_dir(engines)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| e.file_name().to_str().is_some_and(|n| !n.starts_with('.') && n != "dl"))
        .map(|e| InstalledEngine::of(&e.path()))
        .filter(|e| match e.kind {
            Some(EngineKind::Soju) => is_soju_engine(&e.dir),
            _ => find_wine(&e.dir).is_ok(),
        })
        .collect();
    list.sort_by(|a, b| {
        let rank = |e: &InstalledEngine| match e.kind {
            Some(EngineKind::Soju) => 0,
            Some(EngineKind::Gptk) => 1,
            None => 2,
        };
        rank(a).cmp(&rank(b)).then_with(|| {
            crate::release::version_cmp(b.version.as_deref().unwrap_or(""), a.version.as_deref().unwrap_or(""))
        })
    });
    list
}

/// Самый новый установленный soju.
pub fn newest_soju(engines: &Path) -> Option<InstalledEngine> {
    installed_engines(engines).into_iter().find(|e| e.kind == Some(EngineKind::Soju))
}

/// Откуда взялся активный движок.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveSource {
    /// `CARAFE_ENGINE_DIR`.
    Env,
    /// `engines/current`: `engine use` или первая установка soju.
    Chosen,
    /// Выбора не было, а GPTK и бутылки уже есть: пользователь до soju, молча не переключаем.
    Legacy,
    /// Выбора не было: soju (или GPTK, если стоит только он).
    Default,
}

/// Каталог активного движка. Если ничего не установлено — `engines/soju`: такого нет, и wine
/// попросит `carafe engine install`.
pub fn active_engine(config: &Config) -> (PathBuf, ActiveSource) {
    if let Some(d) = &config.engine_dir {
        return (d.clone(), ActiveSource::Env);
    }
    let engines = config.engines();
    if let Some(name) = chosen_engine(&engines) {
        return (engines.join(name), ActiveSource::Chosen);
    }
    let gptk = engines.join(GPTK_NAME);
    let has_gptk = gptk.is_dir();
    if has_gptk && !crate::bottle::bottle_names(&config.bottles()).is_empty() {
        return (gptk, ActiveSource::Legacy);
    }
    match newest_soju(&engines) {
        Some(s) => (s.dir, ActiveSource::Default),
        None if has_gptk => (gptk, ActiveSource::Default),
        None => (engines.join("soju"), ActiveSource::Default),
    }
}

/// Имя из `engines/current`, если оно похоже на каталог движка.
fn chosen_engine(engines: &Path) -> Option<String> {
    let s = fs::read_to_string(engines.join(CURRENT_FILE)).ok()?;
    let s = s.trim();
    (!s.is_empty() && !s.starts_with('.') && !s.contains('/')).then(|| s.to_string())
}

/// Что стоит и что активно — для `engine status` и GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStatus {
    pub active: InstalledEngine,
    pub source: ActiveSource,
    /// Активный движок на месте (есть wine).
    pub ready: bool,
    pub installed: Vec<InstalledEngine>,
}

impl EngineStatus {
    pub fn soju(&self) -> Option<&InstalledEngine> {
        self.installed.iter().find(|e| e.kind == Some(EngineKind::Soju))
    }

    pub fn gptk(&self) -> Option<&InstalledEngine> {
        self.installed.iter().find(|e| e.kind == Some(EngineKind::Gptk))
    }

    pub fn active_kind(&self) -> Option<EngineKind> {
        self.active.kind
    }

    /// Пользователь до soju: предложить перейти (`engine use soju`).
    pub fn suggest_soju(&self) -> bool {
        self.source != ActiveSource::Env && self.active_kind() == Some(EngineKind::Gptk)
    }

    /// Подсказки к статусу: что сделать дальше.
    pub fn hints(&self) -> Vec<String> {
        let mut h = vec![];
        if self.suggest_soju() {
            h.push(match self.soju() {
                Some(_) => "движок по умолчанию теперь soju — перейти: carafe engine use soju (бутылки сначала копируются рядом)".into(),
                None => "движок по умолчанию теперь soju — поставить: carafe engine install, потом carafe engine use soju".into(),
            });
        } else if !self.ready {
            h.push("движок не установлен: carafe engine install".into());
        }
        if self.ready && !self.active.d3dmetal {
            h.push(D3DMETAL_MISSING.into());
        }
        h
    }
}

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
        self.wine_with(&self.config.engine_dir(), prefix)
    }

    /// wine движка `engine` для префикса `prefix`.
    pub fn wine_with(&self, engine: &Path, prefix: &Path) -> Result<Wine> {
        let bin = find_wine(engine)?;
        let env = wine_env(&self.config, prefix, &bin, |k| std::env::var_os(k));
        Ok(Wine { bin, prefix: prefix.into(), env })
    }

    /// Какие движки стоят и какой активен.
    pub fn engine_status(&self) -> EngineStatus {
        let (dir, source) = active_engine(&self.config);
        EngineStatus {
            ready: find_wine(&dir).is_ok(),
            active: InstalledEngine::of(&dir),
            source,
            installed: installed_engines(&self.config.engines()),
        }
    }

    /// `engine install [soju|gptk]`: по умолчанию — последний релиз soju.
    pub fn engine_install(&self, kind: EngineKind) -> Result<PathBuf> {
        match kind {
            EngineKind::Soju => self.soju_install(false),
            EngineKind::Gptk => self.gptk_install(),
        }
    }

    /// Новый пользователь получает soju активным; пользователь до soju (GPTK и бутылки, выбора не
    /// было) остаётся на GPTK, пока не скажет `engine use soju`. Выбранный раньше soju
    /// переезжает на свежую версию — префиксы Wine обновит сам при следующем запуске.
    pub(crate) fn adopt_soju(&self, dir: &Path) -> Result<()> {
        let engines = self.config.engines();
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let adopt = match chosen_engine(&engines) {
            Some(cur) => cur != name && cur.starts_with(SOJU_PREFIX),
            None => active_engine(&Config { engine_dir: None, ..self.config.clone() }).1 != ActiveSource::Legacy,
        };
        if adopt {
            self.set_current(&name)?;
            self.log(Log::Step(format!("активный движок: {}", InstalledEngine::of(dir).label())));
        } else if chosen_engine(&engines).as_deref() != Some(&name) {
            self.log(Log::Step("активным остаётся GPTK — перейти: carafe engine use soju".into()));
        }
        Ok(())
    }

    pub(crate) fn set_current(&self, name: &str) -> Result<()> {
        let engines = self.config.engines();
        fsutil::create_dir_all(&engines)?;
        fsutil::write_atomic(&engines.join(CURRENT_FILE), format!("{name}\n").as_bytes())
    }

    /// Скачать GPTK-сборку Wine, сверить sha256, распаковать и снять карантин. Нужна как запасной
    /// движок и как источник D3DMetal для soju: ставится — подключается к soju без D3DMetal.
    pub fn gptk_install(&self) -> Result<PathBuf> {
        let dl = self.config.engines().join("dl");
        let file = GPTK_URL.rsplit('/').next().unwrap_or(GPTK_NAME);
        let tarball = dl.join(file);
        fsutil::create_dir_all(&dl)?;
        if !tarball.is_file() {
            self.log(Log::Step(format!("качаю движок {file}")));
            let part = dl.join(format!("{file}.part"));
            process::run_inherit(
                Command::new("curl")
                    .args(["-fL", "--retry", "5", "--speed-limit", "4096", "--speed-time", "60", "-C", "-", "-o"])
                    .arg(&part)
                    .arg(GPTK_URL),
            )?;
            fs::rename(&part, &tarball).at(&tarball)?;
        }
        if fsutil::sha256_file(&tarball)? != GPTK_SHA256 {
            return Err(Error::EngineChecksum);
        }
        let d = self.config.engines().join(GPTK_NAME);
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
        for e in installed_engines(&self.config.engines()) {
            if e.kind == Some(EngineKind::Soju) && !e.d3dmetal {
                self.connect_d3dmetal(&e.dir, crate::release::D3DMETAL_TOOL);
            }
        }
        Ok(wine)
    }

    /// `engine use soju|gptk`: каждая бутылка, которую последним обновлял другой движок, сначала
    /// копируется рядом (`bottles/<имя>.backup-<дата>`), потом обновляется новым движком
    /// (`wineboot -u`). Не вышло — бутылки возвращаются из этих копий, движок остаётся прежним.
    pub fn engine_use(&self, kind: EngineKind) -> Result<PathBuf> {
        let engines = self.config.engines();
        let target = match kind {
            EngineKind::Soju => newest_soju(&engines).ok_or(Error::EngineNotInstalled { name: "soju" })?,
            EngineKind::Gptk => {
                let g = InstalledEngine::of(&engines.join(GPTK_NAME));
                find_wine(&g.dir).map_err(|_| Error::EngineNotInstalled { name: "GPTK" })?;
                g
            }
        };
        if self.config.engine_dir.is_some() {
            self.log(Log::Step("задан CARAFE_ENGINE_DIR — пока он есть, запуск идёт на нём".into()));
        }
        let (prev, _) = active_engine(&Config { engine_dir: None, ..self.config.clone() });
        let prev_name = prev.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let todo: Vec<String> = crate::bottle::bottle_names(&self.config.bottles())
            .into_iter()
            .filter(|b| self.bottle_engine(b).unwrap_or_else(|| prev_name.clone()) != target.name)
            .collect();
        for b in &todo {
            if crate::bottle::wineserver_alive(&self.config.bottle_dir(b)) {
                return Err(Error::BottleBusy(b.clone()));
            }
        }
        let mut done: Vec<(String, PathBuf)> = vec![];
        for b in &todo {
            let backup = self.bottle_backup(b)?;
            if let Err(e) = self.bottle_upgrade(b, &target.dir) {
                self.log(Log::Step(format!("не вышло: {e} — возвращаю бутылки из резервных копий")));
                for (b, backup) in done.iter().chain(std::iter::once(&(b.clone(), backup))) {
                    if let Err(e) = self.restore_from(b, backup) {
                        self.log(Log::Step(format!("{b}: не вернул ({e}), копия цела: {}", backup.display())));
                    }
                }
                return Err(e);
            }
            done.push((b.clone(), backup));
        }
        self.set_current(&target.name)?;
        self.log(Log::Step(format!("активный движок: {}", target.label())));
        if !done.is_empty() {
            let other = match kind {
                EngineKind::Soju => "gptk",
                EngineKind::Gptk => "soju",
            };
            for (b, backup) in &done {
                self.log(Log::Step(format!("{b}: резервная копия {}", backup.display())));
            }
            self.log(Log::Step(format!(
                "откат: carafe engine use {other}; бутылку целиком: carafe bottle restore ИМЯ"
            )));
        }
        if !target.d3dmetal {
            self.log(Log::Step(D3DMETAL_MISSING.into()));
        }
        Ok(target.dir)
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
        self.mark_bottle(&prefix, &self.config.engine_dir());
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

    fn script(d: &TestDir, rel: &str, body: &str) -> PathBuf {
        let p = d.file(rel, format!("#!/bin/sh\n{body}\n").as_bytes());
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// Поддельный soju: `wineboot -u` заводит пользователя `wineboot_user` или падает.
    fn fake_soju(d: &TestDir, version: &str, wineboot: &str) {
        let root = format!("carafe/engines/soju-{version}");
        script(d, &format!("{root}/bin/soju"), &format!("[ \"$1\" = wineboot ] && {{ {wineboot}; }}; exit 0"));
        script(d, &format!("{root}/bin/wineserver"), "exit 0");
        d.file(&format!("{root}/SOJU"), format!("crossover 26.3.0\nversion {version}\n").as_bytes());
    }

    fn gptk(d: &TestDir) {
        exec(d, "carafe/engines/gptk-3.0-3/Game Porting Toolkit.app/Contents/Resources/wine/bin/wine64");
        exec(d, "carafe/engines/gptk-3.0-3/Game Porting Toolkit.app/Contents/Resources/wine/bin/wineserver");
        fs::create_dir_all(
            d.path().join("carafe/engines/gptk-3.0-3/Game Porting Toolkit.app/Contents/Resources/wine/lib/external/D3DMetal.framework"),
        )
        .unwrap();
    }

    fn bottle(d: &TestDir, name: &str) -> PathBuf {
        d.file(
            &format!("carafe/bottles/{name}/drive_c/users/crossover/AppData/LocalLow/Ved/Megabonk/Saves/a.json"),
            b"1",
        );
        d.path().join("carafe/bottles").join(name)
    }

    #[test]
    fn active_for_new_user() {
        let d = TestDir::new();
        let c = crate::library::tests::carafe(&d.path().join("carafe"));
        let e = |n: &str| d.path().join("carafe/engines").join(n);
        assert_eq!(active_engine(&c.config), (e("soju"), ActiveSource::Default));
        assert!(!c.engine_status().ready);
        fake_soju(&d, "26.3.0", "true");
        fake_soju(&d, "26.10.0", "true");
        assert_eq!(active_engine(&c.config), (e("soju-26.10.0"), ActiveSource::Default));
        // Первая установка soju записывает выбор: GPTK, поставленный потом ради D3DMetal, его не перебьёт.
        c.adopt_soju(&e("soju-26.10.0")).unwrap();
        gptk(&d);
        bottle(&d, "default");
        assert_eq!(active_engine(&c.config), (e("soju-26.10.0"), ActiveSource::Chosen));
        let st = c.engine_status();
        assert!(st.ready && !st.suggest_soju());
        assert_eq!(st.active.label(), "soju 26.10.0");
        assert_eq!(
            st.installed.iter().map(|e| e.label()).collect::<Vec<_>>(),
            ["soju 26.10.0", "soju 26.3.0", "GPTK 3.0"]
        );
        assert_eq!(st.hints(), [D3DMETAL_MISSING]);
        // CARAFE_ENGINE_DIR главнее.
        let mut cfg = c.config.clone();
        cfg.engine_dir = Some("/e".into());
        assert_eq!(active_engine(&cfg), ("/e".into(), ActiveSource::Env));
    }

    #[test]
    fn existing_user_stays_on_gptk() {
        let d = TestDir::new();
        let c = crate::library::tests::carafe(&d.path().join("carafe"));
        let e = |n: &str| d.path().join("carafe/engines").join(n);
        gptk(&d);
        bottle(&d, "default");
        assert_eq!(active_engine(&c.config), (e("gptk-3.0-3"), ActiveSource::Legacy));
        fake_soju(&d, "26.3.0", "true");
        c.adopt_soju(&e("soju-26.3.0")).unwrap();
        assert!(!e("current").exists());
        let st = c.engine_status();
        assert_eq!(st.source, ActiveSource::Legacy);
        assert!(st.suggest_soju() && st.active.d3dmetal);
        assert!(st.hints()[0].contains("carafe engine use soju"), "{:?}", st.hints());
        // Без бутылок GPTK — просто источник D3DMetal, по умолчанию soju.
        fs::remove_dir_all(d.path().join("carafe/bottles")).unwrap();
        assert_eq!(active_engine(&c.config).0, e("soju-26.3.0"));
    }

    #[test]
    fn use_soju_and_back() {
        let d = TestDir::new();
        let c = crate::library::tests::carafe(&d.path().join("carafe"));
        let e = |n: &str| d.path().join("carafe/engines").join(n);
        gptk(&d);
        let prefix = bottle(&d, "default");
        assert!(matches!(c.engine_use(EngineKind::Soju), Err(Error::EngineNotInstalled { name: "soju" })));
        // soju заводит пользователя usertrv: сейвы переезжают к нему, у crossover всё остаётся.
        fake_soju(&d, "26.3.0", "mkdir -p \"$WINEPREFIX/drive_c/users/usertrv\"");
        assert_eq!(c.engine_use(EngineKind::Soju).unwrap(), e("soju-26.3.0"));
        assert_eq!(active_engine(&c.config), (e("soju-26.3.0"), ActiveSource::Chosen));
        assert_eq!(c.bottle_engine("default").as_deref(), Some("soju-26.3.0"));
        let saves = "AppData/LocalLow/Ved/Megabonk/Saves/a.json";
        assert!(prefix.join("drive_c/users/usertrv").join(saves).is_file());
        assert!(prefix.join("drive_c/users/crossover").join(saves).is_file());
        let backups = crate::bottle::backups(&c.config.bottles(), "default");
        assert_eq!(backups.len(), 1);
        assert!(!backups[0].join("drive_c/users/usertrv").exists());
        assert_eq!(c.bottles(), ["default"]);
        // Повторно — бутылка уже на soju, новой копии нет.
        c.engine_use(EngineKind::Soju).unwrap();
        assert_eq!(crate::bottle::backups(&c.config.bottles(), "default").len(), 1);
        // Обратно на GPTK: тоже через копию.
        c.engine_use(EngineKind::Gptk).unwrap();
        assert_eq!(active_engine(&c.config).0, e("gptk-3.0-3"));
        assert_eq!(crate::bottle::backups(&c.config.bottles(), "default").len(), 2);
    }

    #[test]
    fn failed_upgrade_restores_bottle() {
        let d = TestDir::new();
        let c = crate::library::tests::carafe(&d.path().join("carafe"));
        gptk(&d);
        let prefix = bottle(&d, "default");
        fake_soju(&d, "26.3.0", "echo сломано > \"$WINEPREFIX/broken\"; exit 3");
        assert!(matches!(c.engine_use(EngineKind::Soju), Err(Error::Failed { .. })));
        // Движок прежний, бутылка — из копии, сломанная лежит рядом.
        assert_eq!(active_engine(&c.config).1, ActiveSource::Legacy);
        assert!(!prefix.join("broken").exists());
        assert!(prefix.join("drive_c/users/crossover/AppData/LocalLow/Ved/Megabonk/Saves/a.json").is_file());
        let names: Vec<String> =
            fs::read_dir(c.config.bottles()).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        assert!(names.iter().any(|n| n.starts_with("default.replaced-")), "{names:?}");
        assert_eq!(c.bottles(), ["default"]);
    }

    #[test]
    fn exe_dir_of_bare_name() {
        assert_eq!(exe_dir(Path::new("Game.exe")), Path::new("."));
        assert_eq!(exe_dir(Path::new("/g/Game.exe")), Path::new("/g"));
    }
}
