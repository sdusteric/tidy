//! Where Tidy keeps its files, and which parts of the home folder it never looks at.
//!
//! With `TIDY_HOME` set every Tidy folder moves under that one directory (table 8.1 of the
//! design): `data/` (settings, history, lock and the stand-in Trash), `cache/` (the saved
//! scans) and `logs/`. Without it the folders are the usual macOS ones under `~/Library`.

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Set to redirect every Tidy directory under one root (used by tests, tidyctl and agents).
pub const HOME_ENV: &str = "TIDY_HOME";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    /// settings.json, history.jsonl, lock, and under `TIDY_HOME` the stand-in Trash.
    pub data: PathBuf,
    /// last-cleanup.json, last-large.json.
    pub cache: PathBuf,
    /// tidy.log.
    pub logs: PathBuf,
    /// True when the paths come from `TIDY_HOME`.
    pub overridden: bool,
}

impl AppPaths {
    /// Uses `TIDY_HOME` when it is set and not empty, otherwise the macOS locations.
    pub fn from_env() -> AppPaths {
        Self::resolve(std::env::var_os(HOME_ENV))
    }

    /// [`AppPaths::from_env`] with the variable's value passed in, for tests.
    pub fn resolve(home_override: Option<OsString>) -> AppPaths {
        match home_override.filter(|v| !v.is_empty()) {
            Some(home) => Self::under(Path::new(&home)),
            None => Self::system(),
        }
    }

    /// All directories under `home`: `data`, `cache` and `logs`.
    pub fn under(home: &Path) -> AppPaths {
        AppPaths {
            data: home.join("data"),
            cache: home.join("cache"),
            logs: home.join("logs"),
            overridden: true,
        }
    }

    /// `~/Library/Application Support/Tidy`, `~/Library/Caches/Tidy`, `~/Library/Logs/Tidy`.
    pub fn system() -> AppPaths {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let data = dirs::data_dir()
            .unwrap_or_else(|| home.join("Library/Application Support"))
            .join("Tidy");
        let cache = dirs::cache_dir()
            .unwrap_or_else(|| home.join("Library/Caches"))
            .join("Tidy");
        AppPaths {
            data,
            cache,
            logs: home.join("Library/Logs/Tidy"),
            overridden: false,
        }
    }

    /// Creates `data`, `cache` and `logs`. The stand-in Trash is created by `DirTrash` on its
    /// first move.
    pub fn create_all(&self) -> io::Result<()> {
        for dir in [&self.data, &self.cache, &self.logs] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data.join("settings.json")
    }

    pub fn history_file(&self) -> PathBuf {
        self.data.join("history.jsonl")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.data.join("lock")
    }

    pub fn log_file(&self) -> PathBuf {
        self.logs.join("tidy.log")
    }

    /// The last Cleanup plan, so a later visit can show it at once.
    pub fn last_cleanup_file(&self) -> PathBuf {
        self.cache.join("last-cleanup.json")
    }

    /// The last Large files scan, reused until the user asks for a rescan (C11).
    pub fn last_large_file(&self) -> PathBuf {
        self.cache.join("last-large.json")
    }

    /// The folder `DirTrash` moves items into: `data/trash`. Only used under `TIDY_HOME`.
    pub fn dir_trash(&self) -> PathBuf {
        self.data.join("trash")
    }
}

// ---------- The hard denylist (design section 7) ----------

/// Paths relative to the scan root that are never scanned, never shown and never moved, in
/// Cleanup and Large files. Matching ignores ASCII case, like APFS does by default.
pub const DENYLIST: &[&str] = &[
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Calendars",
    "Library/Reminders",
    "Library/Application Support/AddressBook",
    "Library/Application Support/com.apple.TCC",
    "Library/Metadata",
    "Library/Biome",
    "Library/Keychains",
    "Library/Mobile Documents",
    "Pictures/Photos Library.photoslibrary",
    ".Trash",
    ".ssh",
    ".gnupg",
    ".aws",
    ".config",
];

/// Tidy's own folders under the home folder. Excluded from every rule and never moved.
pub const OWN_FOLDERS: &[&str] = &[
    "Library/Application Support/Tidy",
    "Library/Caches/Tidy",
    "Library/Logs/Tidy",
];

/// Names that are never touched wherever they appear (browser profile data). Any name
/// ending in `.sqlite` is denied too.
pub const DENIED_NAMES: &[&str] = &[
    "Login Data",
    "Cookies",
    "IndexedDB",
    "Local Storage",
    "Session Storage",
    "File System",
];

/// Whether `rel` (relative to the scan root) is on the denylist, is one of Tidy's own folders,
/// or lies inside one of them, or has a denied name in it.
///
/// A path with `..` or a root in it is denied, since it cannot be judged without resolving.
pub fn is_denied_rel(rel: &Path) -> bool {
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().to_ascii_lowercase()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    if parts.iter().any(|p| is_denied_lower(p)) {
        return true;
    }
    DENYLIST.iter().chain(OWN_FOLDERS).any(|entry| {
        let entry: Vec<String> = entry.split('/').map(|s| s.to_ascii_lowercase()).collect();
        parts.len() >= entry.len() && parts[..entry.len()] == entry[..]
    })
}

/// Whether one lowercased file name is denied wherever it appears.
fn is_denied_lower(lower: &str) -> bool {
    lower.ends_with(".sqlite") || DENIED_NAMES.iter().any(|n| n.eq_ignore_ascii_case(lower))
}

/// Whether one file name is denied wherever it appears (`Cookies`, `*.sqlite`), ignoring
/// ASCII case.
pub fn is_denied_name(name: &std::ffi::OsStr) -> bool {
    is_denied_lower(&name.to_string_lossy().to_ascii_lowercase())
}

/// Whether a denylist entry or one of Tidy's own folders lies strictly inside `rel`
/// (relative to the scan root), so that moving `rel` would take it along. `Library` holds
/// `Library/Keychains`; `Library/Caches` holds `Library/Caches/Tidy`. A path with `..` or a
/// root in it counts as holding one, since it cannot be judged without resolving.
pub fn holds_denied_rel(rel: &Path) -> bool {
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_string_lossy().to_ascii_lowercase()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return true,
        }
    }
    DENYLIST.iter().chain(OWN_FOLDERS).any(|entry| {
        let entry: Vec<String> = entry.split('/').map(|s| s.to_ascii_lowercase()).collect();
        entry.len() > parts.len() && entry[..parts.len()] == parts[..]
    })
}

/// Whether `path` is denied relative to `root`. A path outside `root` is not judged here
/// (it returns false); the trash guard refuses it separately.
pub fn is_denied(root: &Path, path: &Path) -> bool {
    match path.strip_prefix(root) {
        Ok(rel) => is_denied_rel(rel),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_home_override_layout() {
        let p = AppPaths::resolve(Some("/tmp/th".into()));
        assert!(p.overridden);
        assert_eq!(p.data, PathBuf::from("/tmp/th/data"));
        assert_eq!(p.cache, PathBuf::from("/tmp/th/cache"));
        assert_eq!(p.logs, PathBuf::from("/tmp/th/logs"));
        assert_eq!(
            p.settings_file(),
            PathBuf::from("/tmp/th/data/settings.json")
        );
        assert_eq!(
            p.history_file(),
            PathBuf::from("/tmp/th/data/history.jsonl")
        );
        assert_eq!(p.lock_file(), PathBuf::from("/tmp/th/data/lock"));
        assert_eq!(p.log_file(), PathBuf::from("/tmp/th/logs/tidy.log"));
        assert_eq!(
            p.last_cleanup_file(),
            PathBuf::from("/tmp/th/cache/last-cleanup.json")
        );
        assert_eq!(
            p.last_large_file(),
            PathBuf::from("/tmp/th/cache/last-large.json")
        );
        assert_eq!(p.dir_trash(), PathBuf::from("/tmp/th/data/trash"));
    }

    #[test]
    fn empty_override_is_ignored() {
        let p = AppPaths::resolve(Some("".into()));
        assert!(!p.overridden);
        assert_eq!(p, AppPaths::resolve(None));
    }

    /// Only computes paths; nothing under the real home is created or read.
    #[test]
    fn system_layout_uses_library_dirs() {
        let p = AppPaths::resolve(None);
        assert!(!p.overridden);
        assert!(p.data.ends_with("Library/Application Support/Tidy"));
        assert!(p.cache.ends_with("Library/Caches/Tidy"));
        assert!(p.logs.ends_with("Library/Logs/Tidy"));
        assert_eq!(p.history_file(), p.data.join("history.jsonl"));
    }

    #[test]
    fn create_all_makes_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let p = AppPaths::under(tmp.path());
        p.create_all().unwrap();
        for d in [&p.data, &p.cache, &p.logs] {
            assert!(d.is_dir(), "{d:?}");
        }
        assert!(!p.dir_trash().exists());
        // Idempotent.
        p.create_all().unwrap();
    }

    #[test]
    fn denylist_matches_entries_and_their_insides() {
        for rel in [
            "Library/Containers",
            "Library/Containers/com.x/Data/big.bin",
            "library/containers/x",
            "Library/Group Containers/g",
            "Library/Mobile Documents/com~apple~CloudDocs/a.txt",
            "Pictures/Photos Library.photoslibrary",
            ".Trash/old.zip",
            ".ssh/id_ed25519",
            ".config/gh/hosts.yml",
            "Library/Application Support/com.apple.TCC/TCC.db",
            "Library/Caches/Tidy",
            "Library/Logs/Tidy/tidy.log",
            "Library/Application Support/Tidy/history.jsonl",
            "Library/Application Support/Google/Chrome/Default/Cookies",
            "Library/Application Support/Google/Chrome/Default/IndexedDB/x",
            "Library/Application Support/Google/Chrome/Default/Local Storage",
            "Library/Application Support/Foo/store.sqlite",
            "Library/Application Support/Foo/STORE.SQLITE",
            "../outside",
            "/abs",
        ] {
            assert!(is_denied_rel(Path::new(rel)), "{rel}");
        }
        for rel in [
            "",
            "Library",
            "Library/Caches/com.example.Idle",
            "Library/Container",
            "Library/ContainersX",
            "Library/Caches/Tidyish",
            "Library/Application Support/Google/Chrome/Default/Cache",
            "Library/Application Support/Foo/store.sqlite-wal",
            "Documents/projects/app/target",
            "./Movies/trip.mov",
            ".configs",
        ] {
            assert!(!is_denied_rel(Path::new(rel)), "{rel}");
        }
    }

    #[test]
    fn is_denied_uses_the_root() {
        let root = Path::new("/tmp/fake/home");
        assert!(is_denied(root, &root.join("Library/Containers/x")));
        assert!(!is_denied(root, &root.join("Library/Caches/x")));
        // Outside the root is not this function's call.
        assert!(!is_denied(root, Path::new("/tmp/elsewhere/.ssh")));
    }
}
