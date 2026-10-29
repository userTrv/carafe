//! Мелочи с файлами, которые bash делал через find/shasum/grep.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{IoContext, Result};

/// Обычный файл с битом исполнения у владельца (`find -type f -perm -u+x`).
pub fn is_executable(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o100 != 0)
}

/// Обход дерева в глубину, как `find`: симлинки не раскрываются. Порядок — по имени, чтобы
/// «первый найденный» не зависел от порядка записей в каталоге. Нечитаемые каталоги пропускаются.
pub fn walk(dir: &Path, f: &mut dyn FnMut(&Path, &fs::FileType) -> Walk) {
    walk_inner(dir, f);
}

/// true — обход остановлен.
fn walk_inner(dir: &Path, f: &mut dyn FnMut(&Path, &fs::FileType) -> Walk) -> bool {
    let Ok(rd) = fs::read_dir(dir) else { return false };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        let path = e.path();
        match f(&path, &ft) {
            Walk::Stop => return true,
            Walk::Continue if ft.is_dir() && walk_inner(&path, f) => return true,
            _ => {}
        }
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    Continue,
    /// Не заходить в этот каталог.
    Skip,
    Stop,
}

/// Первый путь, подходящий под условие.
pub fn find_first(dir: &Path, pred: impl Fn(&Path, &fs::FileType) -> bool) -> Option<PathBuf> {
    let mut found = None;
    walk(dir, &mut |p, t| {
        if pred(p, t) {
            found = Some(p.to_path_buf());
            Walk::Stop
        } else {
            Walk::Continue
        }
    });
    found
}

pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = fs::File::open(p).at(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).at(p)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `grep -aq` по байтам.
pub fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Одинаковое содержимое (`cmp -s`).
pub fn same_contents(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.len() == mb.len() => matches!((fs::read(a), fs::read(b)), (Ok(x), Ok(y)) if x == y),
        _ => false,
    }
}

pub fn copy(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).at(to).map(|_| ())
}

pub fn create_dir_all(p: &Path) -> Result<()> {
    fs::create_dir_all(p).at(p)
}

/// `rm -rf`: отсутствие — не ошибка.
pub fn remove_all(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Err(_) => Ok(()),
        Ok(m) if m.is_dir() => fs::remove_dir_all(p).at(p),
        Ok(_) => fs::remove_file(p).at(p),
    }
}

#[cfg(test)]
pub(crate) mod testdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Временный каталог теста; удаляется при выходе из области видимости.
    pub struct TestDir(pub PathBuf);

    impl TestDir {
        pub fn new() -> TestDir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "carafe-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TestDir(p)
        }

        /// Файл с содержимым; промежуточные каталоги создаются.
        pub fn file(&self, rel: &str, data: &[u8]) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, data).unwrap();
            p
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testdir::TestDir;
    use super::*;

    #[test]
    fn sha256_known() {
        let d = TestDir::new();
        let f = d.file("a", b"abc");
        assert_eq!(sha256_file(&f).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn walk_sorted_and_stops() {
        let d = TestDir::new();
        d.file("b/x", b"");
        d.file("a/y/x", b"");
        d.file("c", b"");
        let first = find_first(d.path(), |p, t| t.is_file() && p.ends_with("x")).unwrap();
        assert_eq!(first, d.path().join("a/y/x"));
        let mut seen = vec![];
        walk(d.path(), &mut |p, _| {
            seen.push(p.strip_prefix(d.path()).unwrap().to_path_buf());
            if p.ends_with("a") { Walk::Skip } else { Walk::Continue }
        });
        assert_eq!(seen, ["a", "b", "b/x", "c"].map(PathBuf::from));
    }

    #[test]
    fn bytes() {
        assert!(contains_bytes(b"..api-ms-win..", b"api-ms"));
        assert!(!contains_bytes(b"api", b"api-ms"));
        assert!(!contains_bytes(b"abc", b""));
    }
}
