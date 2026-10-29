//! Бутылки при смене движка: резервные копии рядом (клоны APFS), восстановление, `wineboot -u`
//! новым движком и перенос сейвов, если новый движок зовёт пользователя Windows иначе.
//!
//! Ничего не удаляется: заменённая бутылка уходит в `<имя>.replaced-<дата>`, копии остаются.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::Carafe;
use crate::engine::{BOTTLE_ENGINE_FILE, InstalledEngine};
use crate::error::{Error, IoContext, Result};
use crate::fsutil;
use crate::library::{disk_space, tree_size};
use crate::process::{self, Log};

/// `bottles/<имя>.backup-<дата>` — резервная копия перед сменой движка.
const BACKUP: &str = ".backup-";
/// `bottles/<имя>.replaced-<дата>` — бутылка, которую заменило восстановление из копии.
const REPLACED: &str = ".replaced-";
/// Обычно `wineboot -u` — полминуты; Mono и Gecko на медленном диске — дольше.
const WINEBOOT_TIMEOUT: Duration = Duration::from_secs(600);
/// Каталоги профиля Windows, где игры держат сейвы.
const SAVE_DIRS: [&str; 3] = ["AppData", "Documents", "Saved Games"];
/// Запас места сверх размера бутылки при обычной (не APFS) копии.
const COPY_RESERVE: u64 = 1 << 30;

/// Бутылки: каталоги с `drive_c`, кроме резервных копий и заменённых.
pub fn bottle_names(bottles: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(bottles)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| !is_service_name(n))
        .filter(|n| bottles.join(n).join("drive_c").is_dir())
        .collect();
    names.sort();
    names
}

/// Резервная копия, заменённая бутылка или скрытый каталог — не бутылка для запуска.
pub fn is_service_name(name: &str) -> bool {
    name.starts_with('.') || name.contains(BACKUP) || name.contains(REPLACED)
}

/// Резервные копии бутылки, от старых к новым.
pub fn backups(bottles: &Path, name: &str) -> Vec<PathBuf> {
    let prefix = format!("{name}{BACKUP}");
    let mut list: Vec<PathBuf> = fs::read_dir(bottles)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(&prefix)))
        .map(|e| e.path())
        .filter(|p| p.join("drive_c").is_dir())
        .collect();
    list.sort();
    list
}

/// Жив ли wineserver этого префикса: Wine держит его сокет в
/// `/tmp/.wine-<uid>/server-<dev>-<inode префикса>/`, а сам сервер — открытый файл `lock` там же.
pub fn wineserver_alive(prefix: &Path) -> bool {
    let Ok(m) = fs::metadata(prefix) else { return false };
    let dir = PathBuf::from(format!("/tmp/.wine-{}/server-{:x}-{:x}", m.uid(), m.dev(), m.ino()));
    if !dir.join("socket").exists() {
        return false;
    }
    // Сокет остаётся и после падения сервера; живой — тот, у кого кто-то держит lock или socket.
    match Command::new("/usr/sbin/lsof")
        .arg("-t")
        .arg(dir.join("lock"))
        .arg(dir.join("socket"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => !String::from_utf8_lossy(&o.stdout).trim().is_empty(),
        // Без lsof проверить нечем — считаем занятым: лучше отказать, чем сломать игру.
        Err(_) => true,
    }
}

/// Пользователи Windows в префиксе (`drive_c/users/*`, кроме Public).
pub fn user_dirs(prefix: &Path) -> BTreeSet<String> {
    fs::read_dir(prefix.join("drive_c/users"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n != "Public")
        .collect()
}

/// Метка времени для имён копий: `20261030-153000` по местному времени.
fn stamp() -> String {
    Command::new("/bin/date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
            t.as_secs().to_string()
        })
}

/// Свободное имя `<база><дата>` (с `-2`, `-3`, если в эту секунду уже есть).
fn free_name(dir: &Path, base: &str) -> PathBuf {
    let s = stamp();
    let mut p = dir.join(format!("{base}{s}"));
    let mut n = 2;
    while fs::symlink_metadata(&p).is_ok() {
        p = dir.join(format!("{base}{s}-{n}"));
        n += 1;
    }
    p
}

impl Carafe {
    /// Бутылки этого carafe (без резервных копий).
    pub fn bottles(&self) -> Vec<String> {
        bottle_names(&self.config.bottles())
    }

    /// Каким движком бутылку обновляли последним (`.carafe-engine`); `None` — не записано.
    pub fn bottle_engine(&self, name: &str) -> Option<String> {
        let s = fs::read_to_string(self.config.bottle_dir(name).join(BOTTLE_ENGINE_FILE)).ok()?;
        Some(s.trim().to_string()).filter(|s| !s.is_empty())
    }

    pub(crate) fn mark_bottle(&self, prefix: &Path, engine: &Path) {
        if let Some(n) = engine.file_name() {
            let _ =
                fsutil::write_atomic(&prefix.join(BOTTLE_ENGINE_FILE), format!("{}\n", n.to_string_lossy()).as_bytes());
        }
    }

    /// Резервная копия бутылки рядом: `bottles/<имя>.backup-<дата>`. На APFS — клон (`cp -c`):
    /// мгновенно и без места; иначе обычная копия, если хватает места.
    pub fn bottle_backup(&self, name: &str) -> Result<PathBuf> {
        let src = self.config.bottle_dir(name);
        if !src.join("drive_c").is_dir() {
            return Err(Error::NoBottle(name.into()));
        }
        let dst = free_name(&self.config.bottles(), &format!("{name}{BACKUP}"));
        self.log(Log::Step(format!("{name}: резервная копия → {}", dst.display())));
        clone_tree(&src, &dst)?;
        Ok(dst)
    }

    /// Вернуть бутылку из резервной копии: `backup` — имя копии (`default.backup-…` или только
    /// дата), без него — самая свежая. Нынешняя бутылка не удаляется, а уходит в
    /// `<имя>.replaced-<дата>`; копия остаётся на месте.
    pub fn bottle_restore(&self, name: &str, backup: Option<&str>) -> Result<PathBuf> {
        let list = backups(&self.config.bottles(), name);
        let from = match backup {
            None => list.last().cloned(),
            Some(b) => list
                .iter()
                .find(|p| {
                    let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    n == b || n.strip_prefix(&format!("{name}{BACKUP}")) == Some(b)
                })
                .cloned(),
        }
        .ok_or_else(|| Error::NoBackup(name.into()))?;
        if wineserver_alive(&self.config.bottle_dir(name)) {
            return Err(Error::BottleBusy(name.into()));
        }
        let replaced = self.restore_from(name, &from)?;
        if let Some(r) = replaced {
            self.log(Log::Step(format!("{name}: прежняя бутылка — {}", r.display())));
        }
        let engine = fs::read_to_string(from.join(BOTTLE_ENGINE_FILE)).ok();
        if let Some(e) = engine.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            let label = InstalledEngine::of(&self.config.engines().join(e)).label();
            self.log(Log::Step(format!("копия сделана на движке {label}")));
        }
        Ok(from)
    }

    /// Убрать бутылку в `.replaced-…` и склонировать на её место копию. Возвращает, куда ушла
    /// прежняя бутылка.
    pub(crate) fn restore_from(&self, name: &str, backup: &Path) -> Result<Option<PathBuf>> {
        let dst = self.config.bottle_dir(name);
        let replaced = if fs::symlink_metadata(&dst).is_ok() {
            let r = free_name(&self.config.bottles(), &format!("{name}{REPLACED}"));
            fs::rename(&dst, &r).at(&r)?;
            Some(r)
        } else {
            None
        };
        self.log(Log::Step(format!("{name}: возвращаю из {}", backup.display())));
        if let Err(e) = clone_tree(backup, &dst) {
            // Не склонировалось — возвращаем прежнюю бутылку на место.
            if let Some(r) = &replaced {
                let _ = fs::rename(r, &dst);
            }
            return Err(e);
        }
        Ok(replaced)
    }

    /// `wineboot -u` движком `engine` с пределом по времени, затем `wineserver -w`. Если новый
    /// движок завёл другого пользователя Windows, сейвы копируются к нему (без замены файлов).
    pub(crate) fn bottle_upgrade(&self, name: &str, engine: &Path) -> Result<()> {
        let prefix = self.config.bottle_dir(name);
        let wine = self.wine_with(engine, &prefix)?;
        let before = user_dirs(&prefix);
        let label = InstalledEngine::of(engine).label();
        self.log(Log::Step(format!("{name}: wineboot -u ({label})")));
        let t = Instant::now();
        let mut log = |_, l: &str| self.log(Log::Output(l.into()));
        let r = process::run_lines_timeout(
            wine.wine().args(["wineboot", "-u"]).stdin(Stdio::null()),
            true,
            WINEBOOT_TIMEOUT,
            "wineboot -u",
            &mut log,
        )
        .and_then(|st| process::check(&wine.wine(), st))
        .and_then(|()| {
            let mut w = wine.command(wine.wineserver());
            w.arg("-w").stdin(Stdio::null());
            let st = process::run_lines_timeout(&mut w, true, WINEBOOT_TIMEOUT, "wineserver -w", &mut log)?;
            process::check(&w, st)
        });
        if let Err(e) = r {
            // Этот wineserver запустили мы сами: перед переездом префикс был свободен.
            let _ = wine.command(wine.wineserver()).arg("-k").stderr(Stdio::null()).status();
            return Err(e);
        }
        self.log(Log::Step(format!("{name}: обновлена за {} с", t.elapsed().as_secs())));
        let after = user_dirs(&prefix);
        if let Some(old) = before.iter().find(|u| *u == "crossover").or(before.iter().next()) {
            for new in after.difference(&before) {
                let n =
                    migrate_saves(&prefix.join("drive_c/users").join(old), &prefix.join("drive_c/users").join(new))?;
                self.log(Log::Step(format!("{name}: сейвы users/{old} → users/{new}: файлов {n}")));
            }
        }
        self.mark_bottle(&prefix, engine);
        Ok(())
    }
}

/// Копия каталога: клон APFS (`cp -c`), а если том его не умеет — обычная, после проверки места.
fn clone_tree(src: &Path, dst: &Path) -> Result<()> {
    let cp = |clone: bool| {
        let mut c = Command::new("/bin/cp");
        c.arg(if clone { "-ac" } else { "-a" }).arg(src).arg(dst).stdin(Stdio::null()).stderr(Stdio::null());
        c.status().map_err(process::spawn_err(&c)).and_then(|s| process::check(&c, s))
    };
    if cp(true).is_ok() {
        return Ok(());
    }
    fsutil::remove_all(dst)?;
    let need = tree_size(src);
    let parent = dst.parent().unwrap_or(Path::new("/"));
    let free = disk_space(parent)?.free;
    if need + COPY_RESERVE > free {
        return Err(Error::NoSpace { need, free });
    }
    let r = cp(false);
    if r.is_err() {
        let _ = fsutil::remove_all(dst);
    }
    r
}

/// Сейвы из профиля `old` в `new`: AppData, Documents, Saved Games. Только файлы, которых в `new`
/// нет; ничего не удаляется и не заменяется. Возвращает число скопированных файлов.
pub fn migrate_saves(old: &Path, new: &Path) -> Result<u64> {
    let mut n = 0;
    for d in SAVE_DIRS {
        let from = old.join(d);
        // Ссылку на папку Mac (Documents → ~/Documents) не раскрываем: это не сейвы префикса.
        if fs::symlink_metadata(&from).is_ok_and(|m| m.is_dir()) {
            let to = new.join(d);
            if fs::symlink_metadata(&to).is_ok_and(|m| m.file_type().is_symlink()) {
                continue;
            }
            n += merge_tree(&from, &to)?;
        }
    }
    Ok(n)
}

fn merge_tree(src: &Path, dst: &Path) -> Result<u64> {
    let mut n = 0;
    fsutil::create_dir_all(dst)?;
    let mut entries: Vec<_> = fs::read_dir(src).at(src)?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let Ok(ft) = e.file_type() else { continue };
        let exists = fs::symlink_metadata(&to).is_ok();
        if ft.is_dir() {
            // Ссылки профиля Wine (Documents → ~/Documents) не раскрываем: копируем только своё.
            if !exists || to.is_dir() && !fs::symlink_metadata(&to).is_ok_and(|m| m.file_type().is_symlink()) {
                n += merge_tree(&from, &to)?;
            }
        } else if !exists && ft.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&from).at(&from)?, &to).at(&to)?;
        } else if !exists && ft.is_file() {
            fs::copy(&from, &to).at(&to)?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;
    use crate::library::tests::carafe;

    fn bottle(d: &TestDir, name: &str, user: &str) {
        d.file(&format!("carafe/bottles/{name}/drive_c/users/{user}/AppData/LocalLow/Ved/Megabonk/Saves/a.json"), b"1");
        d.file(&format!("carafe/bottles/{name}/system.reg"), b"WINE REGISTRY Version 2\n");
    }

    #[test]
    fn names_skip_backups() {
        let d = TestDir::new();
        bottle(&d, "default", "crossover");
        bottle(&d, "default.backup-20261030-120000", "crossover");
        bottle(&d, "default.replaced-20261030-120500", "crossover");
        d.file("carafe/bottles/empty/x", b"");
        let b = d.path().join("carafe/bottles");
        assert_eq!(bottle_names(&b), ["default"]);
        assert_eq!(backups(&b, "default").len(), 1);
        assert!(backups(&b, "def").is_empty());
    }

    #[test]
    fn backup_and_restore() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        assert!(matches!(c.bottle_backup("default"), Err(Error::NoBottle(_))));
        assert!(matches!(c.bottle_restore("default", None), Err(Error::NoBackup(_))));
        bottle(&d, "default", "crossover");
        let saves = "drive_c/users/crossover/AppData/LocalLow/Ved/Megabonk/Saves/a.json";
        let prefix = d.path().join("carafe/bottles/default");
        fs::write(prefix.join(BOTTLE_ENGINE_FILE), "gptk-3.0-3\n").unwrap();

        let b1 = c.bottle_backup("default").unwrap();
        assert!(b1.file_name().unwrap().to_str().unwrap().starts_with("default.backup-"));
        assert_eq!(fs::read(b1.join(saves)).unwrap(), b"1");
        let b2 = c.bottle_backup("default").unwrap();
        assert_ne!(b1, b2);
        assert_eq!(c.bottles(), ["default"]);

        // Бутылку «испортили» — возвращаем самую свежую копию; испорченная не удаляется.
        fs::write(prefix.join(saves), "2").unwrap();
        fs::write(prefix.join(BOTTLE_ENGINE_FILE), "soju-26.3.0\n").unwrap();
        assert_eq!(c.bottle_restore("default", None).unwrap(), b2);
        assert_eq!(fs::read(prefix.join(saves)).unwrap(), b"1");
        assert_eq!(c.bottle_engine("default").as_deref(), Some("gptk-3.0-3"));
        let all: Vec<String> = fs::read_dir(d.path().join("carafe/bottles"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        let replaced: Vec<_> = all.iter().filter(|n| n.starts_with("default.replaced-")).collect();
        assert_eq!(replaced.len(), 1);
        let r = d.path().join("carafe/bottles").join(replaced[0]);
        assert_eq!(fs::read(r.join(saves)).unwrap(), b"2");
        // Копии на месте; можно выбрать копию по дате.
        assert!(b1.is_dir() && b2.is_dir());
        let date = b1.file_name().unwrap().to_str().unwrap().trim_start_matches("default.backup-").to_string();
        assert_eq!(c.bottle_restore("default", Some(&date)).unwrap(), b1);
        assert!(matches!(c.bottle_restore("default", Some("nope")), Err(Error::NoBackup(_))));
        assert_eq!(c.bottles(), ["default"]);
    }

    #[test]
    fn saves_move_without_overwrite() {
        let d = TestDir::new();
        let old = d.path().join("users/usertrv");
        let new = d.path().join("users/crossover");
        d.file("users/usertrv/AppData/LocalLow/Ved/Megabonk/Saves/a.json", b"old");
        d.file("users/usertrv/AppData/LocalLow/Ved/Megabonk/Saves/b.json", b"old");
        d.file("users/usertrv/Saved Games/x/s.sav", b"s");
        d.file("users/usertrv/Temp/junk", b"j");
        // Documents — ссылки на папки Mac с обеих сторон: туда ничего не копируется.
        d.file("mac/Documents/private.txt", b"p");
        d.file("mac/NewDocs/.keep", b"");
        std::os::unix::fs::symlink(d.path().join("mac/Documents"), old.join("Documents")).unwrap();
        fs::create_dir_all(&new).unwrap();
        std::os::unix::fs::symlink(d.path().join("mac/NewDocs"), new.join("Documents")).unwrap();
        d.file("users/crossover/AppData/LocalLow/Ved/Megabonk/Saves/a.json", b"new");
        assert_eq!(migrate_saves(&old, &new).unwrap(), 2);
        let s = new.join("AppData/LocalLow/Ved/Megabonk/Saves");
        assert_eq!(fs::read(s.join("a.json")).unwrap(), b"new");
        assert_eq!(fs::read(s.join("b.json")).unwrap(), b"old");
        assert!(new.join("Saved Games/x/s.sav").is_file());
        assert!(!new.join("Temp").exists());
        assert!(!d.path().join("mac/NewDocs/private.txt").exists());
        assert!(old.join("AppData/LocalLow/Ved/Megabonk/Saves/a.json").is_file());
    }

    #[test]
    fn idle_prefix() {
        let d = TestDir::new();
        assert!(!wineserver_alive(d.path()));
        assert!(!wineserver_alive(&d.path().join("нет")));
    }
}
