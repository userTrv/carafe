//! Релизы движка soju на GitHub: выбор последнего `engine-v*`, манифест `engine.json`, скачивание
//! с докачкой, сверка и атомарная установка в `engines/soju-<версия>`.
//!
//! Сеть — через curl, как у остального carafe: без токена, API GitHub даёт 60 запросов в час.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::Carafe;
use crate::engine::{self, SOJU_PREFIX};
use crate::error::{Error, IoContext, Result};
use crate::fsutil;
use crate::library::disk_space;
use crate::process::{self, Log};

/// Откуда берётся движок soju.
pub const SOJU_REPO: &str = "userTrv/soju";
const TAG_PREFIX: &str = "engine-v";
const MANIFEST: &str = "engine.json";
/// Формат `engine.json`, который понимает этот carafe.
const MANIFEST_FORMAT: u64 = 1;
/// Подключение D3DMetal в движке, если манифест не назвал своё.
pub const D3DMETAL_TOOL: &str = "bin/soju-d3dmetal";

/// Манифест движка из релиза soju (`engine.json`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub format: u64,
    pub version: String,
    #[serde(default)]
    pub wine: Option<String>,
    #[serde(default)]
    pub crossover: Option<String>,
    /// Имя архива среди файлов релиза.
    pub archive: String,
    /// Каталог верхнего уровня в архиве.
    pub root: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub unpacked_size: Option<u64>,
    pub arch: String,
    #[serde(default)]
    pub rosetta: bool,
    pub macos_min: String,
    #[serde(default)]
    pub d3dmetal: Option<ManifestD3dMetal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ManifestD3dMetal {
    #[serde(default)]
    pub bundled: bool,
    /// Скрипт в движке, который подключает D3DMetal из копии GPTK пользователя.
    #[serde(default)]
    pub install: Option<String>,
}

/// Этот Mac: версия macOS, Apple Silicon, есть ли Rosetta 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub macos: String,
    pub arm64: bool,
    pub rosetta: bool,
}

impl Host {
    pub fn detect() -> Host {
        let out = |cmd: &mut Command| {
            cmd.stderr(Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        };
        Host {
            macos: out(Command::new("/usr/bin/sw_vers").arg("-productVersion")).unwrap_or_default(),
            arm64: out(Command::new("/usr/sbin/sysctl").args(["-n", "hw.optional.arm64"])).as_deref() == Some("1"),
            rosetta: Command::new("/usr/bin/arch")
                .args(["-x86_64", "/usr/bin/true"])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success()),
        }
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| bad(format!("не JSON: {e}")))?;
        // Формат — первым: в новом формате остальные поля могут называться иначе.
        match v.get("format").and_then(|f| f.as_u64()) {
            Some(MANIFEST_FORMAT) => {}
            Some(n) => return Err(bad(format!("формат {n} этот carafe не понимает — обнови carafe"))),
            None => return Err(bad("нет поля format".into())),
        }
        let m: Manifest = serde_json::from_value(v).map_err(|e| bad(e.to_string()))?;
        for (field, value) in [("version", &m.version), ("archive", &m.archive), ("root", &m.root)] {
            if !safe_name(value) {
                return Err(bad(format!("{field}: «{value}» — не имя файла")));
            }
        }
        if m.sha256.len() != 64 || !m.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(bad("sha256: нужно 64 шестнадцатеричных знака".into()));
        }
        if m.size == 0 {
            return Err(bad("size: 0".into()));
        }
        if let Some(t) = m.d3dmetal.as_ref().and_then(|d| d.install.as_deref())
            && !safe_rel(t)
        {
            return Err(bad(format!("d3dmetal.install: «{t}» — не путь внутри движка")));
        }
        Ok(m)
    }

    /// Подходит ли движок этому Mac.
    pub fn check_host(&self, host: &Host) -> Result<()> {
        match self.arch.as_str() {
            "x86_64" if host.arm64 && !host.rosetta => {
                return Err(bad("движку нужна Rosetta 2: softwareupdate --install-rosetta".into()));
            }
            "x86_64" => {}
            "arm64" if host.arm64 => {}
            a => return Err(bad(format!("движок для {a}, а этот Mac — другой"))),
        }
        if !host.macos.is_empty() && version_cmp(&host.macos, &self.macos_min) == Ordering::Less {
            return Err(bad(format!("нужна macOS {} или новее, здесь {}", self.macos_min, host.macos)));
        }
        Ok(())
    }

    pub fn d3dmetal_tool(&self) -> &str {
        self.d3dmetal.as_ref().and_then(|d| d.install.as_deref()).unwrap_or(D3DMETAL_TOOL)
    }
}

fn bad(s: String) -> Error {
    Error::BadManifest(s)
}

/// Имя файла или каталога без путей и служебных имён: буквы, цифры, `.`, `_`, `-`.
fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Относительный путь внутри каталога: без `/` в начале и без `..`.
fn safe_rel(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('/') && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

/// Версии по частям: числа — как числа (`26.10.0` новее `26.3.0`), остальное — как строки.
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let parts = |s: &str| s.split(['.', '-', '_']).map(String::from).collect::<Vec<_>>();
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(&pb) {
        let o = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
            _ => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    pa.len().cmp(&pb.len())
}

#[derive(Debug, Clone, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// Релиз движка: версия из тега `engine-v<версия>` и его файлы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub tag: String,
    pub assets: Vec<GhAsset>,
}

impl Release {
    pub fn asset_url(&self, name: &str) -> Result<&str> {
        self.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.as_str())
            .ok_or_else(|| Error::Github(format!("в релизе {} нет файла {name}", self.tag)))
    }
}

/// Последний релиз движка из ответа `/repos/<repo>/releases`: теги `engine-v*`, без черновиков и
/// предрелизов, самая новая версия (не самая свежая дата — исправление старой ветки не в счёт).
pub fn latest_release(json: &str) -> Result<Release> {
    let list: Vec<GhRelease> =
        serde_json::from_str(json).map_err(|e| Error::Github(format!("не разобрал список релизов: {e}")))?;
    list.into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let v = r.tag_name.strip_prefix(TAG_PREFIX)?.to_string();
            safe_name(&v).then_some(Release { version: v, tag: r.tag_name, assets: r.assets })
        })
        .max_by(|a, b| version_cmp(&a.version, &b.version))
        .ok_or(Error::NoEngineRelease(SOJU_REPO))
}

/// Код ответа и заголовки из вывода `curl -D - -w '\n%{http_code}'`.
fn parse_curl_head(out: &str) -> (u16, Option<u64>) {
    let code = out.lines().rev().find(|l| !l.trim().is_empty()).and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    let reset = out.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("x-ratelimit-reset").then(|| v.trim().parse().ok())?
    });
    (code, reset)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Carafe {
    fn dl_dir(&self) -> Result<PathBuf> {
        let dl = self.config.engines().join("dl");
        fsutil::create_dir_all(&dl)?;
        Ok(dl)
    }

    /// Последний релиз движка soju (API GitHub без токена).
    pub fn soju_latest_release(&self) -> Result<Release> {
        let body = self.dl_dir()?.join(format!(".releases-{}.json", std::process::id()));
        let url = format!("https://api.github.com/repos/{SOJU_REPO}/releases?per_page=50");
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "-L", "--retry", "3", "-H", "Accept: application/vnd.github+json"])
            .args(["-H", "X-GitHub-Api-Version: 2022-11-28", "-D", "-", "-w", "\n%{http_code}", "-o"])
            .arg(&body)
            .arg(&url)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit());
        let out = cmd.output().map_err(process::spawn_err(&cmd));
        let text = fs::read_to_string(&body).unwrap_or_default();
        let _ = fs::remove_file(&body);
        let out = out?;
        if !out.status.success() {
            return Err(Error::Github("нет связи с api.github.com".into()));
        }
        let (code, reset) = parse_curl_head(&String::from_utf8_lossy(&out.stdout));
        match code {
            200 => latest_release(&text),
            403 | 429 if code == 429 || text.contains("rate limit") => {
                Err(Error::GithubRateLimit(reset.map(|r| r.saturating_sub(now_secs()).div_ceil(60).max(1))))
            }
            c => Err(Error::Github(format!("{url}: HTTP {c}"))),
        }
    }

    fn fetch_text(&self, url: &str) -> Result<String> {
        let mut cmd = Command::new("curl");
        cmd.args(["-fsSL", "--retry", "3"]).arg(url).stdin(Stdio::null()).stderr(Stdio::inherit());
        let out = cmd.output().map_err(process::spawn_err(&cmd))?;
        process::check(&cmd, out.status)?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Поставить последний релиз soju. `update` — только сообщения другие: «новых версий нет».
    /// Уже стоящая версия не качается заново; D3DMetal подключается, если его ещё нет.
    pub fn soju_install(&self, update: bool) -> Result<PathBuf> {
        let rel = self.soju_latest_release()?;
        let dir = self.config.engines().join(format!("{SOJU_PREFIX}{}", rel.version));
        if engine::is_soju_engine(&dir) {
            self.log(Log::Step(if update {
                format!("новых версий soju нет: {} уже стоит", rel.version)
            } else {
                format!("soju {} уже установлен", rel.version)
            }));
            self.soju_installed(&dir, D3DMETAL_TOOL)?;
            return Ok(dir);
        }
        self.log(Log::Step(format!("soju {}: {}", rel.version, rel.tag)));
        let m = Manifest::parse(&self.fetch_text(rel.asset_url(MANIFEST)?)?)?;
        if m.version != rel.version {
            return Err(bad(format!("версия {} в релизе {}", m.version, rel.tag)));
        }
        m.check_host(&Host::detect())?;
        let engines = self.config.engines();
        fsutil::create_dir_all(&engines)?;
        if let Ok(space) = disk_space(&engines) {
            let need = m.size + m.unpacked_size.unwrap_or(m.size * 3);
            if need > space.free {
                return Err(Error::NoSpace { need, free: space.free });
            }
        }
        let tarball = self.download_archive(rel.asset_url(&m.archive)?, &m)?;
        let dir = self.install_archive(&m, &tarball)?;
        // Архив своё отработал: движок распакован, а повторная установка скачает его заново.
        let _ = fs::remove_file(&tarball);
        self.soju_installed(&dir, m.d3dmetal_tool())?;
        Ok(dir)
    }

    /// Архив в `engines/dl` с докачкой (`curl -C -`). Готовый файл нужного размера не качается.
    fn download_archive(&self, url: &str, m: &Manifest) -> Result<PathBuf> {
        let dl = self.dl_dir()?;
        let tarball = dl.join(&m.archive);
        if fs::metadata(&tarball).is_ok_and(|md| md.len() == m.size) {
            return Ok(tarball);
        }
        let part = dl.join(format!("{}.part", m.archive));
        // Недокачанный кусок больше архива — мусор от другой версии, с ним докачка не сойдётся.
        if fs::metadata(&part).is_ok_and(|md| md.len() > m.size) {
            fsutil::remove_all(&part)?;
        }
        self.log(Log::Step(format!("качаю движок {} ({})", m.archive, fsutil::human_bytes(m.size))));
        process::run_inherit(
            Command::new("curl")
                .args(["-fL", "--retry", "5", "--speed-limit", "4096", "--speed-time", "60", "-C", "-", "-o"])
                .arg(&part)
                .arg(url),
        )?;
        fs::rename(&part, &tarball).at(&tarball)?;
        Ok(tarball)
    }

    /// Сверить архив с манифестом, распаковать во временный каталог и одним переименованием
    /// поставить в `engines/soju-<версия>`. Не сошлось — архив удаляется, каталога движка нет.
    pub fn install_archive(&self, m: &Manifest, tarball: &Path) -> Result<PathBuf> {
        let got = fs::metadata(tarball).at(tarball)?.len();
        if got != m.size {
            fsutil::remove_all(tarball)?;
            return Err(Error::EngineSize { want: m.size, got });
        }
        if fsutil::sha256_file(tarball)? != m.sha256 {
            fsutil::remove_all(tarball)?;
            return Err(Error::EngineChecksum);
        }
        let engines = self.config.engines();
        let dir = engines.join(format!("{SOJU_PREFIX}{}", m.version));
        // Точка в начале: недораспакованный движок не попадёт в список установленных.
        let tmp = engines.join(format!(".{SOJU_PREFIX}{}.tmp", m.version));
        fsutil::remove_all(&tmp)?;
        fsutil::create_dir_all(&tmp)?;
        self.log(Log::Step(format!("распаковываю в {}", dir.display())));
        let unpacked = (|| {
            process::run_inherit(Command::new("tar").arg("-xJf").arg(tarball).arg("-C").arg(&tmp))?;
            let root = tmp.join(&m.root);
            let soju = root.join("bin/soju");
            if !fsutil::is_executable(&soju) {
                return Err(Error::EngineBroken(PathBuf::from(&m.root).join("bin/soju")));
            }
            // Карантин macOS блокирует запуск скачанных бинарников.
            engine::remove_quarantine(&root);
            fsutil::remove_all(&dir)?;
            fs::rename(&root, &dir).at(&dir)
        })();
        let _ = fsutil::remove_all(&tmp);
        unpacked?;
        self.log(Log::Step(format!("готово: {}", dir.join("bin/soju").display())));
        Ok(dir)
    }

    /// После установки soju: D3DMetal и, для нового пользователя, soju активным движком.
    fn soju_installed(&self, dir: &Path, tool: &str) -> Result<()> {
        if !engine::has_d3dmetal(dir) {
            self.connect_d3dmetal(dir, tool);
        }
        self.adopt_soju(dir)
    }

    /// Подключить D3DMetal к движку soju из копии GPTK пользователя: сначала движок GPTK из этого
    /// carafe, иначе `soju-d3dmetal` ищет сам (/Applications, /Volumes, CrossOver). Не вышло —
    /// движок остаётся, без D3DMetal. Возвращает, подключён ли он.
    pub fn connect_d3dmetal(&self, dir: &Path, tool: &str) -> bool {
        let tool = dir.join(tool);
        let source = engine::gptk_d3dmetal_source(&self.config.engines());
        let mut cmd = Command::new(&tool);
        cmd.arg(source.as_deref().unwrap_or(Path::new(""))).arg(dir).stdin(Stdio::null());
        self.log(Log::Step("подключаю D3DMetal".into()));
        let ok = process::run_lines(&mut cmd, true, &mut |_, l| self.log(Log::Output(l.into())))
            .is_ok_and(|s| s.success())
            && engine::has_d3dmetal(dir);
        if !ok {
            self.log(Log::Step(engine::D3DMETAL_MISSING.into()));
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::testdir::TestDir;
    use crate::library::tests::carafe;

    const GOOD: &str = include_str!("../tests/data/engine.json");

    fn host() -> Host {
        Host { macos: "26.1".into(), arm64: true, rosetta: true }
    }

    #[test]
    fn manifest_good() {
        let m = Manifest::parse(GOOD).unwrap();
        assert_eq!((m.version.as_str(), m.root.as_str()), ("26.3.0", "soju-engine-26.3.0"));
        assert_eq!(m.archive, "soju-engine-26.3.0.tar.xz");
        assert_eq!(m.size, 237592004);
        assert_eq!(m.d3dmetal_tool(), "bin/soju-d3dmetal");
        m.check_host(&host()).unwrap();
    }

    fn with(field: &str, value: serde_json::Value) -> String {
        let mut v: serde_json::Value = serde_json::from_str(GOOD).unwrap();
        v[field] = value;
        v.to_string()
    }

    fn err(text: &str) -> String {
        Manifest::parse(text).unwrap_err().to_string()
    }

    #[test]
    fn manifest_bad() {
        assert!(err("{").contains("не JSON"));
        assert!(err("{}").contains("нет поля format"));
        assert!(err(&with("format", 2.into())).contains("обнови carafe"));
        assert!(err(&with("root", "../x".into())).contains("root"));
        assert!(err(&with("archive", "a/b.tar.xz".into())).contains("archive"));
        assert!(err(&with("version", "".into())).contains("version"));
        assert!(err(&with("sha256", "ABC".into())).contains("sha256"));
        assert!(err(&with("size", 0.into())).contains("size"));
        assert!(err(&with("d3dmetal", serde_json::json!({"install": "../../bin/sh"}))).contains("d3dmetal"));
        let no_size = GOOD.replace("\"size\"", "\"sise\"");
        assert!(err(&no_size).contains("size"));
    }

    #[test]
    fn manifest_host() {
        let m = Manifest::parse(GOOD).unwrap();
        let e = m.check_host(&Host { rosetta: false, ..host() }).unwrap_err().to_string();
        assert!(e.contains("Rosetta"), "{e}");
        let e = m.check_host(&Host { macos: "10.15.7".into(), ..host() }).unwrap_err().to_string();
        assert!(e.contains("11.0"), "{e}");
        let arm = Manifest::parse(&with("arch", "arm64".into())).unwrap();
        arm.check_host(&Host { rosetta: false, ..host() }).unwrap();
        let ppc = Manifest::parse(&with("arch", "ppc".into())).unwrap();
        assert!(ppc.check_host(&host()).is_err());
    }

    #[test]
    fn versions() {
        assert_eq!(version_cmp("26.10.0", "26.3.0"), Ordering::Greater);
        assert_eq!(version_cmp("26.3.0", "26.3.0"), Ordering::Equal);
        assert_eq!(version_cmp("26.3", "26.3.1"), Ordering::Less);
        assert_eq!(version_cmp("26.6.2", "11.0"), Ordering::Greater);
    }

    #[test]
    fn pick_latest() {
        let r = latest_release(include_str!("../tests/data/soju-releases.json")).unwrap();
        // 26.10.0 — черновик, 27.0.0 — предрелиз, bridge-v9 — не движок.
        assert_eq!((r.version.as_str(), r.tag.as_str()), ("26.4.1", "engine-v26.4.1"));
        assert_eq!(
            r.asset_url("engine.json").unwrap(),
            "https://github.com/userTrv/soju/releases/download/engine-v26.4.1/engine.json"
        );
        assert!(r.asset_url("nope").is_err());
        assert!(matches!(latest_release("[]"), Err(Error::NoEngineRelease(_))));
        assert!(matches!(latest_release(r#"{"message":"x"}"#), Err(Error::Github(_))));
    }

    #[test]
    fn curl_head() {
        let out = "HTTP/2 403 \r\nx-ratelimit-limit: 60\r\nX-RateLimit-Reset: 1793296530\r\n\r\n\n403";
        assert_eq!(parse_curl_head(out), (403, Some(1793296530)));
        assert_eq!(parse_curl_head("HTTP/2 200\r\n\r\n\n200"), (200, None));
        let e = Error::GithubRateLimit(Some(12)).to_string();
        assert!(e.contains("через 12 мин"), "{e}");
    }

    /// Маленький «движок» в архиве tar.xz: `<root>/bin/soju`.
    fn tarball(d: &TestDir, root: &str, with_soju: bool) -> (PathBuf, String, u64) {
        let src = d.path().join("src");
        let bin = src.join(root).join("bin");
        fs::create_dir_all(&bin).unwrap();
        let f = if with_soju { "soju" } else { "wine" };
        fs::write(bin.join(f), "#!/bin/sh\n").unwrap();
        let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        fs::set_permissions(bin.join(f), mode).unwrap();
        let tar = d.path().join("e.tar.xz");
        assert!(Command::new("tar").arg("-cJf").arg(&tar).arg("-C").arg(&src).arg(root).status().unwrap().success());
        let sha = fsutil::sha256_file(&tar).unwrap();
        let size = fs::metadata(&tar).unwrap().len();
        (tar, sha, size)
    }

    fn manifest(sha: &str, size: u64) -> Manifest {
        let mut m = Manifest::parse(GOOD).unwrap();
        m.version = "1.2.3".into();
        m.root = "soju-engine-1.2.3".into();
        m.sha256 = sha.into();
        m.size = size;
        m
    }

    #[test]
    fn install_atomic() {
        let d = TestDir::new();
        let c = carafe(&d.path().join("carafe"));
        let engine = d.path().join("carafe/engines/soju-1.2.3");
        let count = || fs::read_dir(d.path().join("carafe/engines")).map(|r| r.count()).unwrap_or(0);

        // Не тот sha256: каталога движка нет, архив удалён, временного каталога не осталось.
        let (tar, sha, size) = tarball(&d, "soju-engine-1.2.3", true);
        let wrong = sha.chars().rev().collect::<String>();
        assert!(matches!(c.install_archive(&manifest(&wrong, size), &tar), Err(Error::EngineChecksum)));
        assert!(!engine.exists() && !tar.exists());
        assert_eq!(count(), 0);

        // Не тот размер.
        let (tar, sha, size) = tarball(&d, "soju-engine-1.2.3", true);
        assert!(matches!(c.install_archive(&manifest(&sha, size + 1), &tar), Err(Error::EngineSize { .. })));
        assert!(!engine.exists());

        // В архиве нет bin/soju.
        let d2 = TestDir::new();
        let (tar, sha, size) = tarball(&d2, "soju-engine-1.2.3", false);
        assert!(matches!(c.install_archive(&manifest(&sha, size), &tar), Err(Error::EngineBroken(_))));
        assert!(!engine.exists());
        assert_eq!(count(), 0);

        // Всё сошлось.
        let (tar, sha, size) = tarball(&d, "soju-engine-1.2.3", true);
        assert_eq!(c.install_archive(&manifest(&sha, size), &tar).unwrap(), engine);
        assert!(engine::is_soju_engine(&engine));
        assert_eq!(count(), 1);
    }
}
