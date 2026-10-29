//! appinfo игры из `steamcmd +app_info_print`: имя и правила Auto-Cloud (`ufs/savefiles`).

use std::fmt;
use std::str::FromStr;

use crate::error::{Error, Result};
use crate::vdf::{self, Obj};

/// Правило Auto-Cloud: файлы `pattern` в `<root>/<path>`. В `soju-steamd cloud --rule` и в кеше
/// `.carafe-ufs` записывается строкой `ROOT:PATH:PATTERN:RECURSIVE` (RECURSIVE — 1 или 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudRule {
    /// Корень Steam: `WinAppDataLocalLow`, `WinMyDocuments`, `gameinstall`…
    pub root: String,
    pub path: String,
    pub pattern: String,
    pub recursive: bool,
}

impl fmt::Display for CloudRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}:{}", self.root, self.path, self.pattern, u8::from(self.recursive))
    }
}

impl FromStr for CloudRule {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let mut it = s.splitn(4, ':');
        match (it.next(), it.next(), it.next(), it.next()) {
            (Some(root), Some(path), Some(pattern), rec) if !root.is_empty() => Ok(CloudRule {
                root: root.into(),
                path: path.into(),
                pattern: pattern.into(),
                recursive: rec == Some("1"),
            }),
            _ => Err(Error::Usage("правило облака: ROOT:PATH:PATTERN:RECURSIVE")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub appid: u32,
    /// `common/name`.
    pub name: Option<String>,
    pub cloud_rules: Vec<CloudRule>,
    /// Весь узел приложения — для того, что ядро пока не разбирает.
    pub data: Obj,
}

impl AppInfo {
    pub fn from_obj(appid: u32, data: Obj) -> AppInfo {
        let name = data.path(&["common"]).and_then(|c| c.str("name")).map(str::to_string);
        let cloud_rules = data.path(&["ufs", "savefiles"]).map(cloud_rules).unwrap_or_default();
        AppInfo { appid, name, cloud_rules, data }
    }
}

/// Правила из `ufs/savefiles`: записи `"0" { root path pattern recursive }`. Запись без root
/// пропускается, как в bash-версии. Поле `platforms` пока не учитывается — тоже как в bash.
pub fn cloud_rules(savefiles: &Obj) -> Vec<CloudRule> {
    savefiles
        .iter()
        .filter_map(|(_, v)| v.as_obj())
        .filter_map(|e| {
            let root = e.str("root").filter(|r| !r.is_empty())?;
            Some(CloudRule {
                root: root.into(),
                path: e.str("path").unwrap_or_default().into(),
                pattern: e.str("pattern").unwrap_or_default().into(),
                recursive: e.str("recursive") == Some("1"),
            })
        })
        .collect()
}

/// Узел приложения в выводе steamcmd: перед ним — шапка steamcmd и строка `AppID : …`, после —
/// `Unloading Steam API...`. Возвращает appinfo и сам VDF-текст узла (для кеша).
pub fn parse_app_info_print(out: &str, appid: u32) -> Result<(AppInfo, &str)> {
    let key = format!("\"{appid}\"");
    let mut offset = 0;
    for line in out.split_inclusive('\n') {
        if line.trim() == key {
            let rest = &out[offset..];
            let (_, value, n) = vdf::parse_pair(rest)?;
            let obj = match value {
                vdf::Value::Obj(o) => o,
                vdf::Value::Str(_) => return Err(Error::NoAppInfo(appid)),
            };
            return Ok((AppInfo::from_obj(appid, obj), &rest[..n]));
        }
        offset += line.len();
    }
    Err(Error::NoAppInfo(appid))
}

/// Кеш `.carafe-ufs`: по правилу в строке, пустые строки пропускаются.
pub fn parse_rules_cache(text: &str) -> Vec<CloudRule> {
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| l.parse().ok()).collect()
}

pub fn format_rules_cache(rules: &[CloudRule]) -> String {
    rules.iter().map(|r| format!("{r}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEGABONK: &str = include_str!("../tests/data/app_info_print-3405340.txt");
    const HADES: &str = include_str!("../tests/data/app_info_print-1145360.txt");
    const STARDEW: &str = include_str!("../tests/data/app_info_print-413150.txt");

    fn rule(root: &str, path: &str, pattern: &str, recursive: bool) -> CloudRule {
        CloudRule { root: root.into(), path: path.into(), pattern: pattern.into(), recursive }
    }

    #[test]
    fn megabonk() {
        let (info, node) = parse_app_info_print(MEGABONK, 3405340).unwrap();
        assert_eq!(info.name.as_deref(), Some("Megabonk"));
        assert_eq!(info.cloud_rules, [rule("WinAppDataLocalLow", "Ved/Megabonk/Saves/CloudDir", "*.json", true)]);
        // Строка правила — та же, что писал awk в .carafe-ufs.
        assert_eq!(format_rules_cache(&info.cloud_rules), "WinAppDataLocalLow:Ved/Megabonk/Saves/CloudDir:*.json:1\n");
        // rootoverrides для Mac к правилам Windows-сборки не относятся.
        assert_eq!(info.data.path(&["ufs", "rootoverrides"]).unwrap().0.len(), 2);
        assert!(node.starts_with("\"3405340\"") && node.ends_with('}'));
        assert_eq!(info.data.path(&["common"]).unwrap().str("oslist"), Some("windows,linux"));
    }

    #[test]
    fn hades_no_recursive_and_escapes() {
        let (info, _) = parse_app_info_print(HADES, 1145360).unwrap();
        assert_eq!(info.name.as_deref(), Some("Hades"));
        assert_eq!(info.cloud_rules, [rule("WinMyDocuments", "Saved Games/Hades", "Profile*.sav", false)]);
        let launch = info.data.path(&["config", "launch", "0"]).unwrap();
        assert_eq!(launch.str("executable"), Some(r"x64\Hades.exe"));
        assert_eq!(launch.str("workingdir"), Some(r"x64\"));
    }

    #[test]
    fn stardew() {
        let (info, _) = parse_app_info_print(STARDEW, 413150).unwrap();
        assert_eq!(info.cloud_rules, [rule("WinAppDataRoaming", "StardewValley/Saves", "*", true)]);
    }

    #[test]
    fn missing_app() {
        assert!(matches!(parse_app_info_print(MEGABONK, 480), Err(Error::NoAppInfo(480))));
        let out = "Connecting anonymously to Steam Public...OK\nNo app info for AppID 1 found\n";
        assert!(matches!(parse_app_info_print(out, 1), Err(Error::NoAppInfo(1))));
    }

    #[test]
    fn cached_node_parses_again() {
        let (info, node) = parse_app_info_print(MEGABONK, 3405340).unwrap();
        let (again, _) = parse_app_info_print(node, 3405340).unwrap();
        assert_eq!(again, info);
    }

    #[test]
    fn savefiles_edge_cases() {
        let o = vdf::parse(
            r#""savefiles" {
                "0" { "root" "gameinstall" "path" "saves" "pattern" "*.sav" "platforms" { "1" "Windows" } }
                "1" { "path" "без корня" }
                "2" { "root" "WinSavedGames" "pattern" "a:b" "recursive" "0" }
                "note" "строка, а не правило"
            }"#,
        )
        .unwrap();
        let rules = cloud_rules(o.obj("savefiles").unwrap());
        assert_eq!(rules, [rule("gameinstall", "saves", "*.sav", false), rule("WinSavedGames", "", "a:b", false)]);
    }

    #[test]
    fn rules_cache_roundtrip() {
        let rules = vec![
            rule("WinAppDataLocalLow", "Ved/Megabonk/Saves/CloudDir", "*.json", true),
            rule("gameinstall", "", "*", false),
        ];
        assert_eq!(parse_rules_cache(&format_rules_cache(&rules)), rules);
        assert_eq!(parse_rules_cache("\n\n"), []);
        assert_eq!(rule("A", "p", "*", true).to_string(), "A:p:*:1");
    }
}
