//! Аккаунт Steam: вход по QR через хелпер `carafe-steam`, refresh token в Связке ключей
//! macOS, библиотека купленных игр и загрузка игры без steamcmd.
//!
//! Пароль Steam carafe не видит и не хранит: вход подтверждается в мобильном приложении Steam.
//! Это сторонний клиент Steam — Valve такие терпит, но официально не поддерживает.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use crate::error::{Error, Result};
use crate::fsutil;
use crate::process::Log;
use crate::steamclient::{
    self, Account, AppDepots, Credentials, DownloadEvent, DownloadResult, LoginEvent, OwnedApp, RefreshToken,
    SteamHelper,
};
use crate::{Carafe, steam};

/// Сервис записи в Связке ключей.
pub const KEYCHAIN_SERVICE: &str = "dev.usertrv.carafe.steam";
const KEYCHAIN_ACCOUNT: &str = "refresh-token";
/// Имя устройства, которое видно в приложении Steam при подтверждении QR.
pub const DEVICE_NAME: &str = "carafe (Mac)";

/// Где лежит refresh token. По умолчанию — [`Keychain`]; тесты и демо GUI берут [`MemoryStore`].
pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<Credentials>>;
    fn save(&self, creds: &Credentials) -> Result<()>;
    fn delete(&self) -> Result<()>;
}

/// Связка ключей macOS: generic password `dev.usertrv.carafe.steam` / `refresh-token`,
/// в значении — `{"account_name":…,"refresh_token":…}`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Keychain;

#[cfg(target_os = "macos")]
impl TokenStore for Keychain {
    fn load(&self) -> Result<Option<Credentials>> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
            Ok(bytes) => decode(&bytes).map(Some),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(Error::Keychain(e.to_string())),
        }
    }

    fn save(&self, creds: &Credentials) -> Result<()> {
        use security_framework::passwords::set_generic_password;
        set_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT, encode(creds).as_bytes())
            .map_err(|e| Error::Keychain(e.to_string()))
    }

    fn delete(&self) -> Result<()> {
        use security_framework::passwords::delete_generic_password;
        match delete_generic_password(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT) {
            Err(e) if e.code() != ERR_SEC_ITEM_NOT_FOUND => Err(Error::Keychain(e.to_string())),
            _ => Ok(()),
        }
    }
}

/// `errSecItemNotFound`.
#[cfg(target_os = "macos")]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

fn encode(c: &Credentials) -> String {
    serde_json::json!({ "account_name": c.account_name, "refresh_token": c.refresh_token.expose() }).to_string()
}

fn decode(bytes: &[u8]) -> Result<Credentials> {
    let v: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| Error::Keychain("запись carafe повреждена".into()))?;
    match (v["account_name"].as_str(), v["refresh_token"].as_str()) {
        (Some(a), Some(t)) if !a.is_empty() && !t.is_empty() => {
            Ok(Credentials { account_name: a.into(), refresh_token: RefreshToken::new(t.into()) })
        }
        _ => Err(Error::Keychain("запись carafe повреждена".into())),
    }
}

/// Токен в памяти процесса.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<Option<Credentials>>);

impl MemoryStore {
    pub fn new(creds: Option<Credentials>) -> MemoryStore {
        MemoryStore(Mutex::new(creds))
    }
}

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<Credentials>> {
        Ok(self.0.lock().expect("token store").clone())
    }

    fn save(&self, creds: &Credentials) -> Result<()> {
        *self.0.lock().expect("token store") = Some(creds.clone());
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        *self.0.lock().expect("token store") = None;
        Ok(())
    }
}

/// Купленная игра и установлена ли она в carafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedGame {
    pub app: OwnedApp,
    pub installed: bool,
}

impl Carafe {
    /// Путь к `carafe-steam`.
    pub fn steam_helper(&self) -> Result<PathBuf> {
        let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok());
        steamclient::find_helper(self.config.steam_helper.as_deref(), exe.as_deref()).ok_or(Error::SteamHelperMissing)
    }

    /// Запустить хелпер; вход — отдельно ([`Carafe::steam_connect`] или [`SteamHelper::login_qr`]).
    pub fn steam_spawn(&self) -> Result<SteamHelper> {
        SteamHelper::spawn(&self.steam_helper()?, self.logger.clone())
    }

    /// Имя аккаунта из Связки ключей, без сети. `None` — не входили.
    pub fn steam_account(&self) -> Result<Option<String>> {
        Ok(self.tokens.load()?.map(|c| c.account_name))
    }

    /// Вход по QR: `on_event` получает URL для QR (и новые, если Steam его сменит). Токен —
    /// в Связку ключей. Прервать — `cancel`.
    pub fn steam_login_qr(&self, on_event: &mut dyn FnMut(LoginEvent), cancel: Option<&AtomicBool>) -> Result<Account> {
        let mut h = self.steam_spawn()?;
        self.log(Log::Step("вход в Steam по QR".into()));
        let (account, creds) = h.login_qr(DEVICE_NAME, on_event, cancel)?;
        self.tokens.save(&creds)?;
        self.log(Log::Step("вошёл в Steam, токен — в Связке ключей".into()));
        Ok(account)
    }

    /// Хелпер, вошедший по токену из Связки ключей.
    pub fn steam_connect(&self) -> Result<(SteamHelper, Account)> {
        let creds = self.tokens.load()?.ok_or(Error::NotLoggedIn)?;
        let mut h = self.steam_spawn()?;
        match h.login_token(&creds) {
            Ok(a) => Ok((h, a)),
            Err(Error::Steam { code, .. }) if code == "token_invalid" => Err(Error::TokenExpired),
            Err(e) => Err(e),
        }
    }

    /// Выйти: отозвать токен в Steam (если получится) и стереть его из Связки ключей.
    /// `Ok(false)` — отозвать не вышло (нет сети, токен уже недействителен), но локально стёрт.
    pub fn steam_logout(&self) -> Result<bool> {
        let Some(creds) = self.tokens.load()? else { return Ok(false) };
        let revoked = match self.steam_spawn().and_then(|mut h| h.logout(Some(&creds))) {
            Ok(r) => r,
            Err(e) => {
                self.log(Log::Output(format!("carafe: токен в Steam не отозван: {e}")));
                false
            }
        };
        self.tokens.delete()?;
        Ok(revoked)
    }

    /// Все купленные игры аккаунта с отметкой установленных, по имени.
    pub fn owned_games(&self) -> Result<Vec<OwnedGame>> {
        let (mut h, _) = self.steam_connect()?;
        let mut games: Vec<OwnedGame> = h
            .owned_apps()?
            .into_iter()
            .map(|app| OwnedGame { installed: self.config.game_dir(app.appid).is_dir(), app })
            .collect();
        games.sort_by_cached_key(|g| (g.app.name.to_lowercase(), g.app.appid));
        Ok(games)
    }

    /// Депо и ветки игры. Без входа хелпер спрашивает Steam анонимно.
    pub fn steam_app_info(&self, appid: u32) -> Result<AppDepots> {
        let mut h = match self.steam_connect() {
            Ok((h, _)) => h,
            Err(Error::NotLoggedIn) => self.steam_spawn()?,
            Err(e) => return Err(e),
        };
        h.app_info(appid)
    }

    /// Скачать Windows-сборку игры с аккаунта в `dir` (по умолчанию — `games/<appid>`), без
    /// steamcmd. На это место встанет очередь установки: прогресс в байтах, отмена флагом.
    pub fn steam_download(
        &self,
        appid: u32,
        dir: Option<&Path>,
        on_event: &mut dyn FnMut(DownloadEvent),
        cancel: Option<&AtomicBool>,
    ) -> Result<DownloadResult> {
        let dir = dir.map(Path::to_path_buf).unwrap_or_else(|| self.config.game_dir(appid));
        let (mut h, _) = self.steam_connect()?;
        fsutil::create_dir_all(&dir)?;
        self.log(Log::Step(format!("качаю {appid} из Steam в {}", dir.display())));
        let r = h.download(appid, &dir, &[], on_event, cancel)?;
        steam::write_appid(&dir, appid)?;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::fsutil::testdir::TestDir;
    use std::sync::Arc;

    fn carafe(home: &Path, store: Arc<MemoryStore>) -> Carafe {
        let mut cfg = Config::from_vars(|k| (k == "HOME").then(|| "/h".into())).unwrap();
        cfg.home = home.into();
        cfg.steam_helper = Some(steamclient::tests::fake());
        Carafe::new(cfg).with_logger(|_| {}).with_token_store(store)
    }

    #[test]
    fn login_library_logout() {
        let home = TestDir::new();
        let store = Arc::new(MemoryStore::default());
        let c = carafe(home.path(), store.clone());
        assert_eq!(c.steam_account().unwrap(), None);
        assert!(matches!(c.owned_games(), Err(Error::NotLoggedIn)));

        let mut urls = vec![];
        let acc = c
            .steam_login_qr(
                &mut |e| {
                    if let LoginEvent::Qr { url } = e {
                        urls.push(url)
                    }
                },
                None,
            )
            .unwrap();
        assert_eq!((acc.account_name.as_str(), urls.len()), ("tester", 2));
        assert_eq!(store.load().unwrap().unwrap().refresh_token.expose(), "fake-refresh");
        assert_eq!(c.steam_account().unwrap().as_deref(), Some("tester"));

        home.file("games/3405340/Megabonk.exe", b"");
        let games = c.owned_games().unwrap();
        assert_eq!(
            games.iter().map(|g| (g.app.appid, g.installed)).collect::<Vec<_>>(),
            [(3405340, true), (1245620, false)]
        );

        let r = c.steam_download(3405340, None, &mut |_| {}, None).unwrap();
        assert_eq!(r.downloaded, 300);
        assert_eq!(std::fs::read_to_string(home.path().join("games/3405340/steam_appid.txt")).unwrap(), "3405340\n");

        assert!(c.steam_logout().unwrap());
        assert_eq!(store.load().unwrap(), None);
        assert!(!c.steam_logout().unwrap(), "второй выход — нечего отзывать");
    }

    #[test]
    fn expired_token() {
        let home = TestDir::new();
        let old = Credentials { account_name: "tester".into(), refresh_token: RefreshToken::new("old".into()) };
        let c = carafe(home.path(), Arc::new(MemoryStore::new(Some(old))));
        assert!(matches!(c.owned_games(), Err(Error::TokenExpired)));
        // app_info при протухшем токене не уходит в анонимный режим молча.
        assert!(matches!(c.steam_app_info(3405340), Err(Error::TokenExpired)));
    }

    #[test]
    fn app_info_without_login() {
        let home = TestDir::new();
        let c = carafe(home.path(), Arc::new(MemoryStore::default()));
        assert_eq!(c.steam_app_info(3405340).unwrap().name, "Megabonk");
    }

    #[test]
    fn keychain_record_format() {
        let c = Credentials { account_name: "tester".into(), refresh_token: RefreshToken::new("t.o.k".into()) };
        assert_eq!(decode(encode(&c).as_bytes()).unwrap(), c);
        assert!(matches!(decode(b"{}"), Err(Error::Keychain(_))));
        assert!(matches!(decode(b"garbage"), Err(Error::Keychain(_))));
    }

    #[test]
    fn missing_helper() {
        let home = TestDir::new();
        let mut c = carafe(home.path(), Arc::new(MemoryStore::default()));
        c.config.steam_helper = Some(home.path().join("nope"));
        assert!(matches!(c.steam_login_qr(&mut |_| {}, None), Err(Error::SteamHelperMissing)));
    }
}
