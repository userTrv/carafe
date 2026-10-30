//! steamcmd: логин, установка игр, appinfo; импорт depot из Steam для Mac.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::Carafe;
use crate::appinfo::{self, AppInfo, CloudRule};
use crate::error::{Error, IoContext, Result};
use crate::fsutil;
use crate::native::Platform;
use crate::process::{self, Log};
use crate::queue::{Job, JobKind};

pub const STEAMCMD_URL: &str = "https://steamcdn-a.akamaihd.net/client/installer/steamcmd_osx.tar.gz";

/// Кеш appinfo игры: узел приложения из `app_info_print` (имя, правила облака).
pub const APPINFO_CACHE: &str = ".carafe-appinfo.vdf";
/// Кеш правил Auto-Cloud: формат общий с bash-версией.
pub const UFS_CACHE: &str = ".carafe-ufs";

impl Carafe {
    fn steamcmd(&self) -> PathBuf {
        self.config.steamcmd_dir().join("steamcmd.sh")
    }

    /// steamcmd от Valve: скачать при первом использовании.
    pub fn ensure_steamcmd(&self) -> Result<PathBuf> {
        let sh = self.steamcmd();
        if !fsutil::is_executable(&sh) {
            self.log(Log::Step("качаю steamcmd от Valve".into()));
            let dir = self.config.steamcmd_dir();
            fsutil::create_dir_all(&dir)?;
            let tgz = dir.join("steamcmd_osx.tar.gz");
            process::run_inherit(Command::new("curl").arg("-fsSL").arg("-o").arg(&tgz).arg(STEAMCMD_URL))?;
            process::run_inherit(Command::new("tar").arg("-xzf").arg(&tgz).arg("-C").arg(&dir))?;
            fs::remove_file(&tgz).at(&tgz)?;
        }
        Ok(sh)
    }

    /// Войти в steamcmd. Пароль и Steam Guard steamcmd спрашивает сам в терминале: в аргументах
    /// пароль оседал бы в истории шелла и в `ps`.
    pub fn steam_login(&self, user: &str) -> Result<()> {
        let sh = self.ensure_steamcmd()?;
        process::run_inherit(Command::new(sh).args(["+login", user, "+quit"]))
    }

    /// Поставить игру в очередь загрузок: установку в `to` (или в библиотеку по умолчанию), а если
    /// игра уже стоит — обновление на месте. Качает обработчик очереди ([`crate::Queue`]).
    pub fn enqueue_game(&self, appid: u32, to: Option<&Path>, verify: bool) -> Result<Job> {
        self.enqueue_game_as(appid, to, verify, None)
    }

    /// Как [`Carafe::enqueue_game`], но с выбором сборки: `Some(Macos)` — нативная. Уже
    /// установленная игра обновляется той сборкой, что стоит; другую — [`Error::PlatformMismatch`].
    /// `None` — какая стоит, а для новой — Windows.
    pub fn enqueue_game_as(
        &self,
        appid: u32,
        to: Option<&Path>,
        verify: bool,
        platform: Option<Platform>,
    ) -> Result<Job> {
        let installed = self.locate(appid)?.is_some();
        let dir = self.install_dir(appid, to)?;
        let have = installed.then(|| crate::native::platform_of(&dir));
        let platform = match (have, platform) {
            (Some(h), Some(p)) if h != p => return Err(Error::PlatformMismatch { appid, installed: h }),
            (Some(h), _) => h,
            (None, p) => p.unwrap_or_default(),
        };
        let kind = match (installed, verify) {
            (false, _) => JobKind::Install,
            (true, true) => JobKind::Verify,
            (true, false) => JobKind::Update,
        };
        let mut job = Job::new(appid, dir, kind);
        job.platform = platform;
        job.name =
            self.cached_app_info(appid).and_then(|i| i.name).or_else(|| self.cached_owned_app(appid).map(|a| a.name));
        self.queue().add(job.clone())?;
        Ok(job)
    }

    /// Забрать игру, скачанную основным Steam-клиентом через консоль (`download_depot`).
    /// Без `src` ищет `steamapps/content/app_<appid>/depot_*` в Steam для Mac.
    pub fn game_import(&self, appid: u32, src: Option<&Path>) -> Result<PathBuf> {
        let src = match src {
            Some(s) => s.to_path_buf(),
            None => find_depot(&self.config.steam_dir, appid).ok_or(Error::DepotNotFound(appid))?,
        };
        if !src.is_dir() {
            return Err(Error::NoSuchDir(src));
        }
        let dir = self.install_dir(appid, None)?;
        self.log(Log::Step(format!("переношу {} → {}", src.display(), dir.display())));
        fsutil::create_dir_all(&dir)?;
        let mut from = src.as_os_str().to_owned();
        from.push("/");
        let mut to = dir.as_os_str().to_owned();
        to.push("/");
        process::run_inherit(Command::new("rsync").arg("-a").arg(from).arg(to))?;
        write_appid(&dir, appid)?;
        self.log(Log::Step(format!("готово, удалить исходник можно руками: {}", src.display())));
        Ok(dir)
    }

    /// Вывод `steamcmd +login anonymous +app_info_print APPID` — анонимно, без аккаунта.
    fn fetch_app_info(&self, appid: u32) -> Result<AppInfo> {
        let sh = self.ensure_steamcmd()?;
        let mut cmd = Command::new(sh);
        cmd.args(["+login", "anonymous", "+app_info_print", &appid.to_string(), "+quit"])
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let out = cmd.output().map_err(process::spawn_err(&cmd))?;
        let text = String::from_utf8_lossy(&out.stdout);
        let (info, node) = appinfo::parse_app_info_print(&text, appid)?;
        let dir = self.game_dir(appid);
        if dir.is_dir() {
            let cache = dir.join(APPINFO_CACHE);
            fs::write(&cache, format!("{node}\n")).at(&cache)?;
        }
        Ok(info)
    }

    /// appinfo игры: из кеша в папке игры или (если его нет или `refresh`) от steamcmd — это
    /// несколько секунд и сеть.
    pub fn app_info(&self, appid: u32, refresh: bool) -> Result<AppInfo> {
        if !refresh && let Some(info) = self.cached_app_info(appid) {
            return Ok(info);
        }
        self.fetch_app_info(appid)
    }

    pub fn cached_app_info(&self, appid: u32) -> Option<AppInfo> {
        let text = fs::read_to_string(self.game_dir(appid).join(APPINFO_CACHE)).ok()?;
        appinfo::parse_app_info_print(&text, appid).ok().map(|(i, _)| i)
    }

    /// Правила Auto-Cloud игры. Кеш `.carafe-ufs` — тот же, что у bash-версии; пустой кеш
    /// перечитывается из appinfo. Не получилось узнать — правил нет, как в bash.
    pub fn cloud_rules(&self, appid: u32) -> Result<Vec<CloudRule>> {
        let dir = self.game_dir(appid);
        let cache = dir.join(UFS_CACHE);
        if let Ok(text) = fs::read_to_string(&cache)
            && !text.is_empty()
        {
            return Ok(appinfo::parse_rules_cache(&text));
        }
        let rules = match self.cached_app_info(appid).map(Ok).unwrap_or_else(|| self.fetch_app_info(appid)) {
            Ok(info) => info.cloud_rules,
            Err(e) => {
                self.log(Log::Output(format!("carafe: appinfo {appid}: {e}")));
                vec![]
            }
        };
        let tmp = dir.join(format!("{UFS_CACHE}.tmp"));
        fs::write(&tmp, appinfo::format_rules_cache(&rules)).at(&tmp)?;
        fs::rename(&tmp, &cache).at(&cache)?;
        Ok(rules)
    }

    /// Правила из кеша, без steamcmd: `None` — ещё не узнавали.
    pub fn cached_cloud_rules(&self, appid: u32) -> Option<Vec<CloudRule>> {
        let dir = self.game_dir(appid);
        match fs::read_to_string(dir.join(UFS_CACHE)) {
            Ok(text) if !text.is_empty() => Some(appinfo::parse_rules_cache(&text)),
            _ => self.cached_app_info(appid).map(|i| i.cloud_rules),
        }
    }
}

/// Steamworks внутри игры ищет appid рядом с exe, когда Steam-клиент не запущен.
pub(crate) fn write_appid(dir: &Path, appid: u32) -> Result<()> {
    let p = dir.join("steam_appid.txt");
    fs::write(&p, format!("{appid}\n")).at(&p)
}

/// `find <Steam> -maxdepth 8 -type d -path '*/steamapps/content/app_<appid>/depot_*'`.
fn find_depot(steam: &Path, appid: u32) -> Option<PathBuf> {
    let app = format!("app_{appid}");
    let base = steam.components().count();
    let mut found = None;
    fsutil::walk(steam, &mut |p, t| {
        if !t.is_dir() {
            return fsutil::Walk::Skip;
        }
        if p.components().count() - base > 8 {
            return fsutil::Walk::Skip;
        }
        let name = p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
        let parent = p.parent();
        if name.starts_with("depot_")
            && parent.and_then(Path::file_name).is_some_and(|n| n == app.as_str())
            && parent.and_then(Path::parent).is_some_and(|c| c.ends_with("steamapps/content"))
        {
            found = Some(p.to_path_buf());
            return fsutil::Walk::Stop;
        }
        fsutil::Walk::Continue
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;

    #[test]
    fn depot_search() {
        let d = TestDir::new();
        assert_eq!(find_depot(d.path(), 3405340), None);
        d.file("steamapps/content/app_480/depot_481/x", b"");
        d.file("steamapps/content/app_3405340/notdepot/x", b"");
        assert_eq!(find_depot(d.path(), 3405340), None);
        d.file("steamapps/content/app_3405340/depot_3405341/Megabonk.exe", b"");
        assert_eq!(find_depot(d.path(), 3405340), Some(d.path().join("steamapps/content/app_3405340/depot_3405341")));
        // Глубже 8 уровней find не смотрел.
        let d = TestDir::new();
        d.file("1/2/3/4/5/steamapps/content/app_7/depot_8/x", b"");
        assert_eq!(find_depot(d.path(), 7), None);
        d.file("1/2/3/4/steamapps/content/app_7/depot_8/x", b"");
        assert!(find_depot(d.path(), 7).is_some());
    }
}
