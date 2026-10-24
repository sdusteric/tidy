//! User settings, stored as settings.json (design section 8.2).
//!
//! The pattern is Haul's: a `#[serde(default)]` struct, so a file with missing fields still
//! loads; a `version` field with migrations; values clamped on load and on save; and an
//! atomic write (temp file, fsync, rename, fsync the folder).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{LARGE_DEFAULT_MIN, LARGE_THRESHOLDS, NOT_OPENED_DAYS};
use crate::paths::AppPaths;
use crate::store::write_atomic;

/// The settings.json schema this build writes.
///
/// * 1: the first release (M1).
pub const SCHEMA_VERSION: u32 = 1;

/// The range "Not opened lately" may be set to, in days.
pub const NOT_OPENED_DAYS_MIN: u32 = 1;
pub const NOT_OPENED_DAYS_MAX: u32 = 3650;

/// Default project roots (C21), relative to the home folder. Only the ones that exist are
/// walked.
pub const DEFAULT_PROJECT_ROOTS: [&str; 5] =
    ["Documents/projects", "Developer", "Projects", "src", "code"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Schema version of the file, see [`SCHEMA_VERSION`].
    pub version: u32,
    /// The Large files threshold, one of [`LARGE_THRESHOLDS`] (500 MB by default).
    pub large_min_bytes: u64,
    /// "Not opened lately" means not used for this many days (90 by default, C12).
    pub not_opened_days: u32,
    /// Folders walked for project build output (table 6.3). A relative entry is relative to
    /// the scan root; see [`Settings::project_root_paths`].
    pub project_roots: Vec<PathBuf>,
    /// Paths the user chose "Never show this again" for. A path inside one is ignored too.
    pub ignored_paths: Vec<PathBuf>,
    /// The user has clicked Scan on the Cleanup screen once, so later visits may rescan on
    /// their own (C3).
    pub cleanup_scanned_once: bool,
    /// The user has run a Large files scan once.
    pub large_scanned_once: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            version: SCHEMA_VERSION,
            large_min_bytes: LARGE_DEFAULT_MIN,
            not_opened_days: NOT_OPENED_DAYS,
            project_roots: DEFAULT_PROJECT_ROOTS.iter().map(PathBuf::from).collect(),
            ignored_paths: Vec::new(),
            cleanup_scanned_once: false,
            large_scanned_once: false,
        }
    }
}

impl Settings {
    /// Forces values into their allowed range: a threshold that is not on the menu becomes
    /// the default, the day count is clamped, and empty or repeated paths are dropped.
    pub fn clamped(mut self) -> Settings {
        if !LARGE_THRESHOLDS.contains(&self.large_min_bytes) {
            self.large_min_bytes = LARGE_DEFAULT_MIN;
        }
        self.not_opened_days = self
            .not_opened_days
            .clamp(NOT_OPENED_DAYS_MIN, NOT_OPENED_DAYS_MAX);
        dedupe_paths(&mut self.project_roots);
        dedupe_paths(&mut self.ignored_paths);
        self
    }

    /// The project roots as absolute paths under `root` (the scan root). A relative entry is
    /// joined to `root`. An absolute entry is kept only when it lies inside `root`, so a test
    /// home never reaches the real Documents folder.
    pub fn project_root_paths(&self, root: &Path) -> Vec<PathBuf> {
        self.project_roots
            .iter()
            .filter_map(|p| {
                if p.is_absolute() {
                    (p.starts_with(root) && p != root).then(|| p.clone())
                } else if p
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    None
                } else {
                    Some(root.join(p))
                }
            })
            .collect()
    }

    /// Whether `path` is an ignored path or lies inside one.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.ignored_paths.iter().any(|p| path.starts_with(p))
    }

    /// Adds `path` to the ignore list. Returns false when it was already covered.
    pub fn ignore(&mut self, path: PathBuf) -> bool {
        if self.is_ignored(&path) {
            return false;
        }
        self.ignored_paths.push(path);
        true
    }

    /// Loads settings.json from `paths.data`.
    pub fn load(paths: &AppPaths) -> Settings {
        Self::load_from(&paths.settings_file())
    }

    /// Loads settings.json. A missing or unreadable file gives the defaults. A file from an
    /// older schema (or without a version) is migrated and written back once.
    pub fn load_from(file: &Path) -> Settings {
        let bytes = match std::fs::read(file) {
            Ok(b) => b,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    tracing::warn!("settings: cannot read {}: {e}", file.display());
                }
                return Settings::default();
            }
        };
        let Ok(serde_json::Value::Object(user)) =
            serde_json::from_slice::<serde_json::Value>(&bytes)
        else {
            tracing::warn!("settings: {} is not valid, using defaults", file.display());
            return Settings::default();
        };
        // Each field is read on its own, so one bad value falls back to its default and the
        // rest of the file still counts.
        let mut s = Settings::default();
        let mut base = match serde_json::to_value(&s) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => return s,
        };
        for (k, v) in &user {
            if base.contains_key(k) {
                let mut probe = base.clone();
                probe.insert(k.clone(), v.clone());
                if serde_json::from_value::<Settings>(serde_json::Value::Object(probe)).is_ok() {
                    base.insert(k.clone(), v.clone());
                } else {
                    tracing::warn!("settings: ignoring a bad value for {k}");
                }
            }
        }
        if let Ok(parsed) = serde_json::from_value::<Settings>(serde_json::Value::Object(base)) {
            s = parsed;
        }
        let saved_version = user
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let migrate = saved_version < u64::from(SCHEMA_VERSION);
        if migrate {
            s.version = SCHEMA_VERSION;
        }
        let s = s.clamped();
        if migrate && let Err(e) = s.save_to(file) {
            tracing::warn!("settings: cannot write back {}: {e}", file.display());
        }
        s
    }

    pub fn save(&self, paths: &AppPaths) -> io::Result<()> {
        self.save_to(&paths.settings_file())
    }

    /// Clamps and writes atomically. The version written is never below [`SCHEMA_VERSION`].
    pub fn save_to(&self, file: &Path) -> io::Result<()> {
        let mut s = self.clone().clamped();
        s.version = s.version.max(SCHEMA_VERSION);
        let json = serde_json::to_vec_pretty(&s).map_err(io::Error::other)?;
        write_atomic(file, &json)
    }
}

/// Drops empty paths and later repeats, keeping the first order.
fn dedupe_paths(v: &mut Vec<PathBuf>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|p| !p.as_os_str().is_empty() && seen.insert(p.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MB;

    fn paths() -> (tempfile::TempDir, AppPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let p = AppPaths::under(tmp.path());
        p.create_all().unwrap();
        (tmp, p)
    }

    fn disk(p: &AppPaths) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(p.settings_file()).unwrap()).unwrap()
    }

    #[test]
    fn defaults() {
        let s = Settings::default();
        assert_eq!(s.version, SCHEMA_VERSION);
        assert_eq!(s.large_min_bytes, 500 * MB);
        assert_eq!(s.not_opened_days, 90);
        assert_eq!(s.project_roots.len(), 5);
        assert!(s.ignored_paths.is_empty());
        assert!(!s.cleanup_scanned_once);
        assert!(!s.large_scanned_once);
    }

    #[test]
    fn missing_file_gives_defaults_and_writes_nothing() {
        let (_t, p) = paths();
        assert_eq!(Settings::load(&p), Settings::default());
        assert!(!p.settings_file().exists());
    }

    #[test]
    fn round_trip() {
        let (_t, p) = paths();
        let s = Settings {
            large_min_bytes: 1000 * MB,
            not_opened_days: 30,
            project_roots: vec!["code".into()],
            ignored_paths: vec!["/h/Library/Caches/x".into()],
            cleanup_scanned_once: true,
            large_scanned_once: true,
            ..Settings::default()
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
    }

    #[test]
    fn clamps_on_save_and_load() {
        let (_t, p) = paths();
        let s = Settings {
            large_min_bytes: 123,
            not_opened_days: 0,
            project_roots: vec!["a".into(), "".into(), "a".into(), "b".into()],
            ..Settings::default()
        };
        s.save(&p).unwrap();
        let back = Settings::load(&p);
        assert_eq!(back.large_min_bytes, 500 * MB);
        assert_eq!(back.not_opened_days, 1);
        assert_eq!(back.project_roots, [PathBuf::from("a"), PathBuf::from("b")]);
        std::fs::write(
            p.settings_file(),
            br#"{"version":1,"not_opened_days":99999}"#,
        )
        .unwrap();
        assert_eq!(Settings::load(&p).not_opened_days, NOT_OPENED_DAYS_MAX);
    }

    #[test]
    fn partial_and_bad_values_keep_the_rest() {
        let (_t, p) = paths();
        std::fs::write(
            p.settings_file(),
            br#"{"version":1,"cleanup_scanned_once":true,"not_opened_days":"many","future_field":3}"#,
        )
        .unwrap();
        let s = Settings::load(&p);
        assert!(s.cleanup_scanned_once);
        assert_eq!(s.not_opened_days, 90);
        assert_eq!(s.large_min_bytes, 500 * MB);
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        let (_t, p) = paths();
        for raw in [&b"{ not json"[..], b"[1,2]", b"null"] {
            std::fs::write(p.settings_file(), raw).unwrap();
            assert_eq!(Settings::load(&p), Settings::default());
            // Left alone for the user to inspect.
            assert_eq!(std::fs::read(p.settings_file()).unwrap(), raw);
        }
    }

    #[test]
    fn a_file_without_a_version_is_migrated_and_written_back_once() {
        let (_t, p) = paths();
        std::fs::write(p.settings_file(), br#"{"large_scanned_once":true}"#).unwrap();
        let s = Settings::load(&p);
        assert!(s.large_scanned_once);
        assert_eq!(s.version, SCHEMA_VERSION);
        assert_eq!(disk(&p)["version"], SCHEMA_VERSION);
        assert_eq!(disk(&p)["large_scanned_once"], true);
    }

    #[test]
    fn a_current_file_is_not_rewritten() {
        let (_t, p) = paths();
        let raw = br#"{"version":1,"large_min_bytes":100000000}"#;
        std::fs::write(p.settings_file(), raw).unwrap();
        assert_eq!(Settings::load(&p).large_min_bytes, 100 * MB);
        assert_eq!(std::fs::read(p.settings_file()).unwrap(), raw);
    }

    #[test]
    fn a_newer_version_is_kept() {
        let (_t, p) = paths();
        std::fs::write(p.settings_file(), br#"{"version":7}"#).unwrap();
        let s = Settings::load(&p);
        assert_eq!(s.version, 7);
        s.save(&p).unwrap();
        assert_eq!(disk(&p)["version"], 7);
    }

    #[test]
    fn project_roots_stay_inside_the_scan_root() {
        let root = Path::new("/tmp/fake/home");
        let s = Settings {
            project_roots: vec![
                "Documents/projects".into(),
                "/tmp/fake/home/work".into(),
                "/Users/someone/Documents/projects".into(),
                "/tmp/fake/home".into(),
                "../escape".into(),
            ],
            ..Settings::default()
        };
        assert_eq!(
            s.project_root_paths(root),
            [root.join("Documents/projects"), root.join("work")]
        );
    }

    #[test]
    fn ignore_list() {
        let mut s = Settings::default();
        assert!(s.ignore("/h/Library/Caches/x".into()));
        assert!(!s.ignore("/h/Library/Caches/x".into()));
        assert!(!s.ignore("/h/Library/Caches/x/inner".into()));
        assert!(s.is_ignored(Path::new("/h/Library/Caches/x/a/b")));
        assert!(!s.is_ignored(Path::new("/h/Library/Caches/xy")));
        assert_eq!(s.ignored_paths.len(), 1);
    }
}
