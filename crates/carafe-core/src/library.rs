//! Папки библиотек: где лежат игры. Встроенная — `games/` в каталоге carafe (так было всегда),
//! остальные пользователь добавляет сам, например на внешнем SSD. В каждой игры лежат как
//! `<папка>/<appid>`. Одна из папок — по умолчанию для новых установок.
//!
//! Список хранится в `libraryfolders.vdf` в каталоге carafe — по образцу одноимённого файла
//! Steam. Нет файла — есть только встроенная папка, как до появления библиотек. Для каждой папки
//! файл помнит, какие игры в ней были и сколько они весили: отключили внешний диск — игры с него
//! остаются в списке как недоступные, а не пропадают.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{Error, IoContext, Result};
use crate::vdf::{self, Obj, Value};
use crate::{Carafe, fsutil};

pub const LIBRARY_FILE: &str = "libraryfolders.vdf";
/// Запас свободного места сверх размера игры при переносе.
pub const MOVE_RESERVE: u64 = 256 << 20;

/// Папка библиотеки из `libraryfolders.vdf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryFolder {
    pub path: PathBuf,
    /// Встроенная `games/` в каталоге carafe: её нельзя убрать.
    pub builtin: bool,
    pub default: bool,
    /// Игры, которые были в папке при последнем просмотре, с размером, если его считали.
    pub apps: Vec<KnownApp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownApp {
    pub appid: u32,
    pub size: Option<u64>,
    pub name: Option<String>,
}

impl LibraryFolder {
    pub fn game_dir(&self, appid: u32) -> PathBuf {
        self.path.join(appid.to_string())
    }

    /// Папка на месте. Отключённый внешний диск — нет `/Volumes/<диск>`, папки тоже нет.
    pub fn available(&self) -> bool {
        self.path.is_dir()
    }

    fn app(&self, appid: u32) -> Option<&KnownApp> {
        self.apps.iter().find(|a| a.appid == appid)
    }
}

/// Папка библиотеки с местом на диске и играми — для `library list` и GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryInfo {
    pub folder: LibraryFolder,
    pub available: bool,
    pub space: Option<DiskSpace>,
    /// Сумма известных размеров игр.
    pub used: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub total: u64,
    pub free: u64,
}

/// Где лежит игра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameLocation {
    pub appid: u32,
    pub library: PathBuf,
    pub dir: PathBuf,
    pub available: bool,
    pub size: Option<u64>,
    pub name: Option<String>,
}

/// Прогресс переноса игры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveProgress {
    pub done: u64,
    pub total: u64,
}

/// Как перенесли.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// Тот же том — переименование, мгновенно.
    Rename,
    Copy,
}

/// Ключ без учёта завершающего `/` и `.`: `/Volumes/SSD/Games/` и `/Volumes/SSD/Games` — одна
/// папка. Симлинки не раскрываем — отключённый диск всё равно не раскрыть.
fn norm(p: &Path) -> PathBuf {
    p.components().collect()
}

impl Carafe {
    fn library_file(&self) -> PathBuf {
        self.config.home.join(LIBRARY_FILE)
    }

    /// Папки библиотек: встроенная всегда первая.
    pub fn library_folders(&self) -> Result<Vec<LibraryFolder>> {
        let builtin = norm(&self.config.games());
        let file = self.library_file();
        let text = match fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(Error::Io { path: file, source }),
        };
        let mut folders = parse_folders(&text, &builtin)?;
        if !folders.iter().any(|f| f.builtin) {
            folders.insert(0, LibraryFolder { path: builtin, builtin: true, default: false, apps: vec![] });
        }
        if !folders.iter().any(|f| f.default) {
            folders[0].default = true;
        }
        Ok(folders)
    }

    fn save_folders(&self, folders: &[LibraryFolder]) -> Result<()> {
        let file = self.library_file();
        fsutil::create_dir_all(&self.config.home)?;
        fsutil::write_atomic(&file, format_folders(folders).as_bytes())
    }

    /// Изменить список папок под блокировкой файла: CLI и GUI могут писать одновременно.
    fn edit_folders<T>(&self, f: impl FnOnce(&mut Vec<LibraryFolder>) -> Result<T>) -> Result<T> {
        fsutil::create_dir_all(&self.config.home)?;
        let _lock = fsutil::FileLock::new(&self.config.home.join(format!("{LIBRARY_FILE}.lock")))?;
        let mut folders = self.library_folders()?;
        let before = folders.clone();
        let r = f(&mut folders)?;
        if folders != before {
            self.save_folders(&folders)?;
        }
        Ok(r)
    }

    fn folder_by_path<'a>(folders: &'a [LibraryFolder], path: &Path) -> Result<&'a LibraryFolder> {
        let key = norm(path);
        folders
            .iter()
            .find(|f| f.path == key)
            .or_else(|| {
                // `~/Games` против `/Users/u/Games` и прочие симлинки: сравниваем и раскрытые пути.
                let real = path.canonicalize().ok()?;
                folders.iter().find(|f| f.path.canonicalize().ok().as_ref() == Some(&real))
            })
            .ok_or_else(|| Error::NotALibrary(path.into()))
    }

    /// Добавить папку. Её родитель должен существовать: если `/Volumes/SSD` не смонтирован,
    /// создавать `/Volumes/SSD/Games` на системном диске нельзя.
    pub fn library_add(&self, path: &Path) -> Result<LibraryFolder> {
        if !path.is_absolute() {
            return Err(Error::Usage("путь к библиотеке — абсолютный"));
        }
        let path = norm(path);
        self.edit_folders(|folders| {
            for f in folders.iter() {
                if f.path == path {
                    return Err(Error::LibraryExists(path.clone()));
                }
                if f.path.starts_with(&path) || path.starts_with(&f.path) {
                    return Err(Error::LibraryNested(f.path.clone()));
                }
            }
            if !path.is_dir() {
                if !path.parent().is_some_and(Path::is_dir) {
                    return Err(Error::NoSuchDir(path.clone()));
                }
                fs::create_dir(&path).at(&path)?;
            }
            let folder = LibraryFolder { path: path.clone(), builtin: false, default: false, apps: vec![] };
            folders.push(folder.clone());
            Ok(folder)
        })
    }

    /// Убрать папку из списка. Файлы не трогаются; папку с играми убрать нельзя, пока она
    /// подключена, — сначала перенеси игры. Отключённую можно: игры остаются на диске.
    pub fn library_remove(&self, path: &Path) -> Result<()> {
        let path = path.to_path_buf();
        self.edit_folders(|folders| {
            let f = Self::folder_by_path(folders, &path)?.clone();
            if f.builtin {
                return Err(Error::LibraryBuiltin);
            }
            if f.available() && !scan_dir(&f.path)?.is_empty() {
                return Err(Error::LibraryNotEmpty(f.path));
            }
            folders.retain(|x| x.path != f.path);
            if f.default {
                folders.iter_mut().for_each(|x| x.default = x.builtin);
            }
            Ok(())
        })
    }

    pub fn library_set_default(&self, path: &Path) -> Result<()> {
        let path = path.to_path_buf();
        self.edit_folders(|folders| {
            let key = Self::folder_by_path(folders, &path)?.path.clone();
            for f in folders.iter_mut() {
                f.default = f.path == key;
            }
            Ok(())
        })
    }

    /// Папки с местом на диске. Подключённые просматриваются заново, и файл запоминает их игры.
    pub fn libraries(&self) -> Result<Vec<LibraryInfo>> {
        let folders = self.edit_folders(|folders| {
            for f in folders.iter_mut().filter(|f| f.available()) {
                let ids = scan_dir(&f.path)?;
                let old = std::mem::take(&mut f.apps);
                f.apps = ids
                    .into_iter()
                    .map(|appid| {
                        old.iter().find(|a| a.appid == appid).cloned().unwrap_or(KnownApp {
                            appid,
                            size: None,
                            name: None,
                        })
                    })
                    .collect();
            }
            Ok(folders.clone())
        })?;
        Ok(folders
            .into_iter()
            .map(|folder| {
                let available = folder.available();
                LibraryInfo {
                    space: available.then(|| disk_space(&folder.path).ok()).flatten(),
                    used: folder.apps.iter().filter_map(|a| a.size).sum(),
                    available,
                    folder,
                }
            })
            .collect())
    }

    /// Все игры всех библиотек, включая недоступные (с отключённого диска), по APPID. Одна и та
    /// же игра в двух папках — берётся первая, как при запуске.
    pub fn game_locations(&self) -> Result<Vec<GameLocation>> {
        let mut out: Vec<GameLocation> = vec![];
        for lib in self.libraries()? {
            for a in &lib.folder.apps {
                if out.iter().any(|g| g.appid == a.appid) {
                    continue;
                }
                out.push(GameLocation {
                    appid: a.appid,
                    dir: lib.folder.game_dir(a.appid),
                    library: lib.folder.path.clone(),
                    available: lib.available,
                    size: a.size,
                    name: a.name.clone(),
                });
            }
        }
        out.sort_by_key(|g| g.appid);
        Ok(out)
    }

    /// Где установлена игра: первая подключённая папка, где есть `<appid>/`; иначе — папка,
    /// которая её помнит (диск отключён), с `available: false`.
    pub fn locate(&self, appid: u32) -> Result<Option<GameLocation>> {
        let folders = self.library_folders()?;
        let found = folders.iter().find(|f| f.game_dir(appid).is_dir()).map(|f| (f, true));
        let found =
            found.or_else(|| folders.iter().find(|f| !f.available() && f.app(appid).is_some()).map(|f| (f, false)));
        Ok(found.map(|(f, available)| GameLocation {
            appid,
            library: f.path.clone(),
            dir: f.game_dir(appid),
            available,
            size: f.app(appid).and_then(|a| a.size),
            name: f.app(appid).and_then(|a| a.name.clone()),
        }))
    }

    /// Папка игры: где она установлена, а если нигде — во встроенной библиотеке (так было
    /// до появления библиотек; сообщения об ошибке тогда показывают привычный путь).
    pub fn game_dir(&self, appid: u32) -> PathBuf {
        match self.locate(appid) {
            Ok(Some(l)) => l.dir,
            _ => self.config.game_dir(appid),
        }
    }

    /// Папка игры, только если она на месте; с отключённого диска — понятная ошибка.
    pub fn installed_dir(&self, appid: u32) -> Result<PathBuf> {
        match self.locate(appid)? {
            Some(l) if l.available => Ok(l.dir),
            Some(l) => Err(Error::LibraryUnavailable(l.library)),
            None => Err(Error::NotInstalled(appid)),
        }
    }

    /// Куда ставить игру: туда, где она уже есть (обновление), иначе в `to` или в папку по
    /// умолчанию. Папка должна быть подключена.
    pub fn install_dir(&self, appid: u32, to: Option<&Path>) -> Result<PathBuf> {
        let folders = self.library_folders()?;
        if let Some(l) = self.locate(appid)? {
            if to.is_some_and(|t| Self::folder_by_path(&folders, t).is_ok_and(|f| f.path != l.library)) {
                return Err(Error::InstalledElsewhere { appid, lib: l.library });
            }
            return if l.available { Ok(l.dir) } else { Err(Error::LibraryUnavailable(l.library)) };
        }
        let f = match to {
            Some(t) => Self::folder_by_path(&folders, t)?,
            None => folders.iter().find(|f| f.default).unwrap_or(&folders[0]),
        };
        if f.builtin {
            // Встроенную создаём сами, как раньше `game install`.
            fsutil::create_dir_all(&f.path)?;
        } else if !f.available() {
            return Err(Error::LibraryUnavailable(f.path.clone()));
        }
        Ok(f.game_dir(appid))
    }

    /// Запомнить размер и имя игры в файле библиотек.
    pub(crate) fn remember_app(&self, appid: u32, size: Option<u64>, name: Option<String>) -> Result<()> {
        let Some(loc) = self.locate(appid)? else { return Ok(()) };
        self.edit_folders(|folders| {
            let Some(f) = folders.iter_mut().find(|f| f.path == loc.library) else { return Ok(()) };
            match f.apps.iter_mut().find(|a| a.appid == appid) {
                Some(a) => {
                    a.size = size.or(a.size);
                    a.name = name.or(a.name.take());
                }
                None => f.apps.push(KnownApp { appid, size, name }),
            }
            f.apps.sort_by_key(|a| a.appid);
            Ok(())
        })
    }

    /// Размер игры в байтах. Считается обходом папки; `refresh: false` берёт запомненный.
    pub fn game_size(&self, appid: u32, refresh: bool) -> Result<u64> {
        let loc = self.locate(appid)?.ok_or(Error::NotInstalled(appid))?;
        if !refresh && let Some(s) = loc.size {
            return Ok(s);
        }
        if !loc.available {
            return loc.size.ok_or(Error::LibraryUnavailable(loc.library));
        }
        let size = tree_size(&loc.dir);
        let name = self.cached_app_info(appid).and_then(|i| i.name);
        self.remember_app(appid, Some(size), name)?;
        Ok(size)
    }

    /// Перенести игру в другую библиотеку. На том же томе — переименование. Иначе: проверка
    /// места, копия во временную папку рядом с целью, и только после успешной копии — удаление
    /// исходника. Ошибка или отмена (`cancel`) посередине оставляют исходник целым, а
    /// недокопированное удаляется.
    pub fn move_game(
        &self,
        appid: u32,
        to: &Path,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(MoveProgress),
    ) -> Result<(PathBuf, MoveKind)> {
        self.move_inner(appid, to, MoveTest::default(), cancel, on_progress)
    }

    fn move_inner(
        &self,
        appid: u32,
        to: &Path,
        test: MoveTest,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(MoveProgress),
    ) -> Result<(PathBuf, MoveKind)> {
        let folders = self.library_folders()?;
        let target = Self::folder_by_path(&folders, to)?.clone();
        let loc = self.locate(appid)?.ok_or(Error::NotInstalled(appid))?;
        if !loc.available {
            return Err(Error::LibraryUnavailable(loc.library));
        }
        if loc.library == target.path {
            return Err(Error::AlreadyThere(target.path));
        }
        if !target.available() {
            return Err(Error::LibraryUnavailable(target.path));
        }
        let src = loc.dir;
        let dst = target.game_dir(appid);
        if fs::symlink_metadata(&dst).is_ok() {
            return Err(Error::TargetExists(dst));
        }
        let name = loc.name.or_else(|| self.cached_app_info(appid).and_then(|i| i.name));

        let same_volume = !test.force_copy
            && fs::metadata(&src).at(&src)?.dev() == fs::metadata(&target.path).at(&target.path)?.dev();
        let (size, kind) = if same_volume {
            fs::rename(&src, &dst).at(&dst)?;
            (loc.size, MoveKind::Rename)
        } else {
            let total = tree_size(&src);
            let free = match test.free {
                Some(f) => f,
                None => disk_space(&target.path)?.free,
            };
            if total + MOVE_RESERVE > free {
                return Err(Error::NoSpace { need: total, free });
            }
            let tmp = target.path.join(format!(".{appid}.carafe-moving"));
            fsutil::remove_all(&tmp)?;
            let mut done = 0;
            on_progress(MoveProgress { done, total });
            let copied = copy_tree(&src, &tmp, cancel, &mut |n| {
                done += n;
                on_progress(MoveProgress { done, total });
            });
            if let Err(e) = copied {
                // Исходник не трогали; убираем только свою временную копию.
                let _ = fsutil::remove_all(&tmp);
                return Err(e);
            }
            fs::rename(&tmp, &dst).at(&dst)?;
            // Исходник сначала прячем под служебное имя: если удаление оборвётся, остаток не
            // примут за игру.
            let trash = loc.library.join(format!(".{appid}.carafe-removing"));
            fs::rename(&src, &trash).at(&trash)?;
            fsutil::remove_all(&trash)?;
            (Some(total), MoveKind::Copy)
        };

        self.edit_folders(|folders| {
            for f in folders.iter_mut() {
                f.apps.retain(|a| a.appid != appid);
                if f.path == target.path {
                    f.apps.push(KnownApp { appid, size, name: name.clone() });
                    f.apps.sort_by_key(|a| a.appid);
                }
            }
            Ok(())
        })?;
        Ok((dst, kind))
    }
}

/// Подмены для тестов: в них оба каталога на одном томе, а место на диске — настоящее.
#[derive(Debug, Clone, Copy, Default)]
struct MoveTest {
    force_copy: bool,
    free: Option<u64>,
}

/// `<appid>/` в папке библиотеки.
pub(crate) fn scan_dir(dir: &Path) -> Result<Vec<u32>> {
    let mut ids: Vec<u32> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(source) => return Err(Error::Io { path: dir.into(), source }),
    };
    ids.sort_unstable();
    Ok(ids)
}

/// Размер файлов в дереве (без раскрытия симлинков), как его увидит копирование.
pub fn tree_size(dir: &Path) -> u64 {
    let mut total = 0;
    fsutil::walk(dir, &mut |p, t| {
        if t.is_file() {
            total += fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(0);
        }
        fsutil::Walk::Continue
    });
    total
}

/// Место на томе с папкой: `df -k -P`. statvfs без `unsafe` из std не достать, а ради одного
/// вызова тянуть крейт незачем.
pub fn disk_space(path: &Path) -> Result<DiskSpace> {
    let mut cmd = Command::new("/bin/df");
    cmd.arg("-k").arg("-P").arg(path).stdin(Stdio::null()).stderr(Stdio::null());
    let out = cmd.output().map_err(crate::process::spawn_err(&cmd))?;
    crate::process::check(&cmd, out.status)?;
    parse_df(&String::from_utf8_lossy(&out.stdout)).ok_or(Error::NoSuchDir(path.into()))
}

/// Вторая строка `df -P`: `устройство всего занято свободно ёмкость точка`, в КБ.
fn parse_df(out: &str) -> Option<DiskSpace> {
    let cols: Vec<&str> = out.lines().nth(1)?.split_whitespace().collect();
    let kb = |i: usize| cols.get(i)?.parse::<u64>().ok().map(|v| v * 1024);
    Some(DiskSpace { total: kb(1)?, free: kb(3)? })
}

const CHUNK: usize = 1 << 20;

/// Копия дерева с правами и временем изменения файлов; симлинки — как есть. `on_bytes` —
/// сколько байт скопировано с прошлого вызова. Отмена проверяется на каждом мегабайте.
fn copy_tree(src: &Path, dst: &Path, cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64)) -> Result<()> {
    let meta = fs::symlink_metadata(src).at(src)?;
    fs::create_dir(dst).at(dst)?;
    let mut entries: Vec<_> = fs::read_dir(src).at(src)?.collect::<std::io::Result<_>>().at(src)?;
    entries.sort_by_key(|e| e.file_name());
    let mut buf = vec![0u8; CHUNK];
    for e in entries {
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let ft = e.file_type().at(&from)?;
        if ft.is_symlink() {
            let link = fs::read_link(&from).at(&from)?;
            std::os::unix::fs::symlink(&link, &to).at(&to)?;
        } else if ft.is_dir() {
            copy_tree(&from, &to, cancel, on_bytes)?;
        } else if ft.is_file() {
            copy_file(&from, &to, &mut buf, cancel, on_bytes)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Interrupted);
        }
    }
    fs::set_permissions(dst, meta.permissions()).at(dst)?;
    Ok(())
}

fn copy_file(from: &Path, to: &Path, buf: &mut [u8], cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64)) -> Result<()> {
    let mut r = fs::File::open(from).at(from)?;
    let meta = r.metadata().at(from)?;
    let mut w = fs::File::create(to).at(to)?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Interrupted);
        }
        let n = r.read(buf).at(from)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).at(to)?;
        on_bytes(n as u64);
    }
    w.set_permissions(meta.permissions()).at(to)?;
    if let Ok(t) = meta.modified() {
        w.set_modified(t).at(to)?;
    }
    Ok(())
}

/// ```text
/// "libraryfolders"
/// {
///     "0"
///     {
///         "path"      "/Users/u/Library/Application Support/carafe/games"
///         "default"   "1"
///         "apps"
///         {
///             "3405340"   { "size" "1181116006" "name" "Megabonk" }
///         }
///     }
/// }
/// ```
fn parse_folders(text: &str, builtin: &Path) -> Result<Vec<LibraryFolder>> {
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let doc = vdf::parse(text)?;
    let Some(root) = doc.obj("libraryfolders") else { return Ok(vec![]) };
    let mut out: Vec<LibraryFolder> = vec![];
    for (_, v) in root.iter() {
        let Some(o) = v.as_obj() else { continue };
        let Some(path) = o.str("path").filter(|p| Path::new(p).is_absolute()).map(|p| norm(Path::new(p))) else {
            continue;
        };
        if out.iter().any(|f| f.path == path) {
            continue;
        }
        let apps = o
            .obj("apps")
            .map(|apps| {
                apps.iter()
                    .filter_map(|(k, v)| {
                        let appid = k.parse().ok()?;
                        let a = v.as_obj();
                        Some(KnownApp {
                            appid,
                            // Как у Steam: `"appid" "размер"`; у нас — объект.
                            size: a.and_then(|a| a.str("size")).or(v.as_str()).and_then(|s| s.parse().ok()),
                            name: a.and_then(|a| a.str("name")).filter(|n| !n.is_empty()).map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(LibraryFolder {
            builtin: path == builtin,
            default: o.str("default") == Some("1") && !out.iter().any(|f| f.default),
            path,
            apps,
        });
    }
    // Встроенная — первой.
    out.sort_by_key(|f| !f.builtin);
    Ok(out)
}

fn format_folders(folders: &[LibraryFolder]) -> String {
    let s = |v: &str| Value::Str(v.into());
    let list = folders
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut o = vec![("path".to_string(), s(&f.path.to_string_lossy()))];
            if f.default {
                o.push(("default".into(), s("1")));
            }
            let apps = f
                .apps
                .iter()
                .map(|a| {
                    let mut app = vec![];
                    if let Some(size) = a.size {
                        app.push(("size".to_string(), s(&size.to_string())));
                    }
                    if let Some(n) = &a.name {
                        app.push(("name".into(), s(n)));
                    }
                    (a.appid.to_string(), Value::Obj(Obj(app)))
                })
                .collect();
            o.push(("apps".into(), Value::Obj(Obj(apps))));
            (i.to_string(), Value::Obj(Obj(o)))
        })
        .collect();
    Obj(vec![("libraryfolders".into(), Value::Obj(Obj(list)))]).to_string()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::Config;
    use crate::fsutil::testdir::TestDir;

    pub(crate) fn carafe(home: &Path) -> Carafe {
        let mut cfg = Config::from_vars(|k| (k == "HOME").then(|| "/h".into())).unwrap();
        cfg.home = home.into();
        Carafe::new(cfg).with_logger(|_| {})
    }

    #[test]
    fn no_file_is_old_layout() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        d.file("carafe/games/3405340/Megabonk.exe", b"MZ");
        let f = c.library_folders().unwrap();
        assert_eq!(f.len(), 1);
        assert!(f[0].builtin && f[0].default);
        assert_eq!(f[0].path, d.path().join("carafe/games"));
        assert_eq!(c.game_dir(3405340), d.path().join("carafe/games/3405340"));
        assert_eq!(c.game_dir(1), d.path().join("carafe/games/1"));
        // Просмотр запоминает игры, раскладка остаётся прежней.
        c.libraries().unwrap();
        assert_eq!(c.library_folders().unwrap()[0].apps.len(), 1);
        assert_eq!(c.game_dir(3405340), d.path().join("carafe/games/3405340"));
    }

    #[test]
    fn several_libraries() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        d.file("carafe/games/3405340/Megabonk.exe", b"MZ");
        d.file("ssd/Games/413150/Stardew Valley.exe", b"MZMZ");
        d.file("ssd/Games/notes/x", b"");
        let ssd = d.path().join("ssd/Games");
        c.library_add(&ssd).unwrap();
        assert!(matches!(c.library_add(&ssd), Err(Error::LibraryExists(_))));
        assert!(matches!(c.library_add(&ssd.join("sub")), Err(Error::LibraryNested(_))));
        // Родителя нет — это отключённый диск, а не новая папка.
        assert!(matches!(c.library_add(&d.path().join("nodisk/Games")), Err(Error::NoSuchDir(_))));
        assert!(matches!(c.library_add(Path::new("rel")), Err(Error::Usage(_))));

        let libs = c.libraries().unwrap();
        assert_eq!(libs.len(), 2);
        assert!(libs.iter().all(|l| l.available && l.space.is_some_and(|s| s.total >= s.free)));
        assert_eq!(libs[0].folder.apps.iter().map(|a| a.appid).collect::<Vec<_>>(), [3405340]);
        assert_eq!(libs[1].folder.apps.iter().map(|a| a.appid).collect::<Vec<_>>(), [413150]);
        assert_eq!(c.game_dir(413150), ssd.join("413150"));
        assert_eq!(c.game_size(413150, true).unwrap(), 4);
        assert_eq!(c.libraries().unwrap()[1].used, 4);

        let ids: Vec<u32> = c.games().unwrap().iter().map(|g| g.appid).collect();
        assert_eq!(ids, [413150, 3405340]);

        // Новые установки — в папку по умолчанию; с завершающим / — та же папка.
        assert_eq!(c.install_dir(480, None).unwrap(), d.path().join("carafe/games/480"));
        c.library_set_default(&d.path().join("ssd/Games/")).unwrap();
        assert_eq!(c.install_dir(480, None).unwrap(), ssd.join("480"));
        assert_eq!(
            c.install_dir(480, Some(&d.path().join("carafe/games"))).unwrap(),
            d.path().join("carafe/games/480")
        );
        // Установленная игра обновляется на месте.
        assert_eq!(c.install_dir(3405340, None).unwrap(), d.path().join("carafe/games/3405340"));
        assert!(matches!(c.install_dir(3405340, Some(&ssd)), Err(Error::InstalledElsewhere { .. })));

        // С играми не убрать; встроенную не убрать никогда.
        assert!(matches!(c.library_remove(&ssd), Err(Error::LibraryNotEmpty(_))));
        assert!(matches!(c.library_remove(&d.path().join("carafe/games")), Err(Error::LibraryBuiltin)));
        fs::remove_dir_all(ssd.join("413150")).unwrap();
        c.library_remove(&ssd).unwrap();
        let f = c.library_folders().unwrap();
        assert_eq!(f.len(), 1);
        assert!(f[0].default);
    }

    #[test]
    fn unavailable_library_keeps_games() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        d.file("carafe/games/3405340/Megabonk.exe", b"MZ");
        d.file("Volumes/T7/Games/1145350/Hades2.exe", b"MZMZMZ");
        let t7 = d.path().join("Volumes/T7/Games");
        c.library_add(&t7).unwrap();
        c.library_set_default(&t7).unwrap();
        c.game_size(1145350, true).unwrap();

        // Диск отключили.
        fs::rename(d.path().join("Volumes/T7"), d.path().join("T7-off")).unwrap();
        let libs = c.libraries().unwrap();
        assert!(!libs[1].available && libs[1].space.is_none());
        assert_eq!(libs[1].used, 6);
        let locs = c.game_locations().unwrap();
        assert_eq!(locs.len(), 2);
        let h = locs.iter().find(|g| g.appid == 1145350).unwrap();
        assert!(!h.available);
        assert_eq!(h.size, Some(6));
        assert!(matches!(c.installed_dir(1145350), Err(Error::LibraryUnavailable(_))));
        assert!(matches!(c.game(1145350), Err(Error::LibraryUnavailable(_))));
        // Список доступных игр — без неё, но и без ошибки.
        assert_eq!(c.games().unwrap().len(), 1);
        // Ставить по умолчанию на отключённый диск нельзя — и папку на системном не создаём.
        assert!(matches!(c.install_dir(480, None), Err(Error::LibraryUnavailable(_))));
        assert!(!t7.exists());

        // Подключили обратно — игра на месте.
        fs::rename(d.path().join("T7-off"), d.path().join("Volumes/T7")).unwrap();
        assert!(c.game_locations().unwrap().iter().all(|g| g.available));
        assert_eq!(c.installed_dir(1145350).unwrap(), t7.join("1145350"));
    }

    #[test]
    fn file_round_trip() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        d.file("carafe/games/7/a", b"abc");
        d.file("ext/9/b", b"");
        c.library_add(&d.path().join("ext")).unwrap();
        c.game_size(7, true).unwrap();
        let text = fs::read_to_string(d.path().join("carafe").join(LIBRARY_FILE)).unwrap();
        let back = parse_folders(&text, &d.path().join("carafe/games")).unwrap();
        assert_eq!(back, c.library_folders().unwrap());
        assert_eq!(back[0].apps, [KnownApp { appid: 7, size: Some(3), name: None }]);
        // Формат Steam: размер строкой вместо объекта.
        let steam = "\"libraryfolders\" { \"0\" { \"path\" \"/x\" \"apps\" { \"10\" \"123\" } } }";
        let f = parse_folders(steam, Path::new("/b")).unwrap();
        assert_eq!(f[0].apps[0].size, Some(123));
    }

    #[test]
    fn df_output() {
        let out = "Filesystem 1024-blocks      Used Available Capacity  Mounted on\n\
                   /dev/disk3s5 239362496 150000000  61865320    71%    /System/Volumes/Data\n";
        assert_eq!(parse_df(out), Some(DiskSpace { total: 239362496 * 1024, free: 61865320 * 1024 }));
        assert_eq!(parse_df("Filesystem\n"), None);
    }

    impl Carafe {
        /// Перенос копированием, как между томами.
        fn move_by_copy_for_test(
            &self,
            appid: u32,
            to: &Path,
            cancel: &AtomicBool,
            on: &mut dyn FnMut(MoveProgress),
        ) -> Result<PathBuf> {
            let test = MoveTest { force_copy: true, free: Some(u64::MAX / 2) };
            self.move_inner(appid, to, test, cancel, on).map(|(p, _)| p)
        }

        fn move_copy_with_space_for_test(&self, appid: u32, to: &Path, free: u64) -> Result<PathBuf> {
            let test = MoveTest { force_copy: true, free: Some(free) };
            self.move_inner(appid, to, test, &AtomicBool::new(false), &mut |_| {}).map(|(p, _)| p)
        }
    }

    fn setup_move(d: &TestDir) -> (Carafe, PathBuf) {
        let c = carafe(&d.path().join("carafe"));
        d.file("carafe/games/42/Game.exe", &[1; 3000]);
        d.file("carafe/games/42/Data/a.pak", &[2; 5000]);
        d.file("carafe/games/42/Data/b.pak", &[3; 7000]);
        std::os::unix::fs::symlink("a.pak", d.path().join("carafe/games/42/Data/link")).unwrap();
        let ext = d.path().join("ext");
        fs::create_dir(&ext).unwrap();
        c.library_add(&ext).unwrap();
        (c, ext)
    }

    #[test]
    fn move_same_volume_renames() {
        let d = TestDir::new();
        let (c, ext) = setup_move(&d);
        let (dst, kind) = c.move_game(42, &ext, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!((dst.clone(), kind), (ext.join("42"), MoveKind::Rename));
        assert!(!d.path().join("carafe/games/42").exists());
        assert_eq!(fs::read(dst.join("Data/b.pak")).unwrap().len(), 7000);
        assert_eq!(c.locate(42).unwrap().unwrap().library, ext);
        assert!(matches!(c.move_game(42, &ext, &AtomicBool::new(false), &mut |_| {}), Err(Error::AlreadyThere(_))));
        assert!(matches!(c.move_game(1, &ext, &AtomicBool::new(false), &mut |_| {}), Err(Error::NotInstalled(1))));
    }

    #[test]
    fn copy_keeps_tree() {
        let d = TestDir::new();
        let (_c, _ext) = setup_move(&d);
        let src = d.path().join("carafe/games/42");
        let mtime = fs::metadata(src.join("Data/a.pak")).unwrap().modified().unwrap();
        let dst = d.path().join("copy");
        let mut seen = 0;
        copy_tree(&src, &dst, &AtomicBool::new(false), &mut |n| seen += n).unwrap();
        assert_eq!(seen, 15000);
        assert_eq!(tree_size(&dst), 15000);
        assert_eq!(fs::read_link(dst.join("Data/link")).unwrap(), PathBuf::from("a.pak"));
        assert_eq!(fs::metadata(dst.join("Data/a.pak")).unwrap().modified().unwrap(), mtime);
    }

    #[test]
    fn copy_cancel_and_error_keep_source() {
        let d = TestDir::new();
        let (c, ext) = setup_move(&d);
        let src = d.path().join("carafe/games/42");
        let before = tree_size(&src);

        // Отмена посередине.
        let cancel = AtomicBool::new(false);
        let r = c.move_by_copy_for_test(42, &ext, &cancel, &mut |p| {
            if p.done > 4000 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(matches!(r, Err(Error::Interrupted)), "{r:?}");
        assert_eq!(tree_size(&src), before);
        assert!(!ext.join("42").exists() && !ext.join(".42.carafe-moving").exists());
        assert_eq!(c.locate(42).unwrap().unwrap().dir, src);

        // Ошибка чтения посередине: файл без прав.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(src.join("Data/b.pak"), fs::Permissions::from_mode(0o000)).unwrap();
        let r = c.move_by_copy_for_test(42, &ext, &AtomicBool::new(false), &mut |_| {});
        fs::set_permissions(src.join("Data/b.pak"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(r, Err(Error::Io { .. })), "{r:?}");
        assert_eq!(tree_size(&src), before);
        assert!(!ext.join("42").exists() && !ext.join(".42.carafe-moving").exists());

        // А без помех — переезжает, исходник удалён.
        let mut last = MoveProgress { done: 0, total: 0 };
        let dst = c.move_by_copy_for_test(42, &ext, &AtomicBool::new(false), &mut |p| last = p).unwrap();
        assert_eq!(last, MoveProgress { done: before, total: before });
        assert_eq!(tree_size(&dst), before);
        assert!(!src.exists());
        assert!(!d.path().join("carafe/games/.42.carafe-removing").exists());
        let libs = c.libraries().unwrap();
        assert_eq!(libs[1].folder.apps, [KnownApp { appid: 42, size: Some(before), name: None }]);
        assert!(libs[0].folder.apps.is_empty());
    }

    #[test]
    fn move_checks_space() {
        let d = TestDir::new();
        let (c, ext) = setup_move(&d);
        let r = c.move_copy_with_space_for_test(42, &ext, 1000);
        assert!(matches!(r, Err(Error::NoSpace { need: 15000, free: 1000 })), "{r:?}");
        assert!(d.path().join("carafe/games/42/Game.exe").exists());
    }
}
