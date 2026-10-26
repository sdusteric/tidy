//! Tidy's live status for Shelf (Shelf design section 14): one small JSON file,
//! `Shelf/status/tidy.json`, with the reclaimable total and the time of the last check up.
//!
//! Written after a check up and again after a clean or a Cleanup scan changes the total, so
//! Shelf never shows space that is already gone. The write goes to `.tidy.json.tmp` in the same
//! folder (mode 0600), is fsynced and renamed over the target. The file holds no paths, file
//! names or IDs: only sizes, times and the app version.

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{HistoryEntry, ItemResult, Plan, format_utc};
use crate::paths::AppPaths;

/// Shelf ignores a file bigger than this.
pub const MAX_BYTES: usize = 16 * 1024;
const SCHEMA: u32 = 1;
const FILE: &str = "tidy.json";
const TMP: &str = ".tidy.json.tmp";

/// The app version, from the `VERSION` file at the repo root (the one the bundle uses).
fn app_version() -> &'static str {
    include_str!("../../../VERSION").trim()
}

/// The content of `tidy.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShelfStatus {
    pub schema: u32,
    pub app: String,
    pub app_version: String,
    /// UTC, ISO 8601.
    pub updated_at: String,
    /// What Clean takes right now (the ticked total of the Cleanup plan), in bytes.
    pub reclaimable_bytes: u64,
    /// UTC, ISO 8601: when the last check up finished.
    pub last_check_at: String,
}

impl ShelfStatus {
    pub fn new(now: u64, reclaimable_bytes: u64, last_check_at: u64) -> ShelfStatus {
        ShelfStatus {
            schema: SCHEMA,
            app: "tidy".into(),
            app_version: app_version().into(),
            updated_at: format_utc(now),
            reclaimable_bytes,
            last_check_at: format_utc(last_check_at),
        }
    }
}

/// What the core keeps between writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShelfState {
    pub reclaimable_bytes: u64,
    pub last_check_at: u64,
}

impl ShelfState {
    /// The state after a clean: the bytes of the plan's ticked items that reached the Trash are
    /// no longer reclaimable. Matches by path (a folded row only when all its members moved); an item that was not ticked never counted.
    pub fn after_clean(self, plan: &Plan, entry: &HistoryEntry) -> ShelfState {
        let gone: u64 = plan
            .ticked()
            .filter(|i| {
                i.paths().iter().all(|p| {
                    entry
                        .items
                        .iter()
                        .any(|h| h.result == ItemResult::Trashed && &h.path == p)
                })
            })
            .map(|i| i.bytes_alloc)
            .sum();
        ShelfState {
            reclaimable_bytes: self.reclaimable_bytes.saturating_sub(gone),
            ..self
        }
    }
}

/// Writes `status` to the status folder of `paths`. Creates the folder, mode 0600.
pub fn write(paths: &AppPaths, status: &ShelfStatus) -> io::Result<()> {
    write_to(&paths.shelf_status, status)
}

pub fn write_to(dir: &Path, status: &ShelfStatus) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(status).map_err(io::Error::other)?;
    if json.len() > MAX_BYTES {
        return Err(io::Error::other("status file over 16 KB"));
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(TMP);
    let target = dir.join(FILE);
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Reads the file back (used at start so a clean before any check up keeps `last_check_at`).
pub fn read(paths: &AppPaths) -> Option<ShelfState> {
    let bytes = std::fs::read(paths.shelf_status_file()).ok()?;
    let s: ShelfStatus = serde_json::from_slice(&bytes).ok()?;
    Some(ShelfState {
        reclaimable_bytes: s.reclaimable_bytes,
        last_check_at: parse_utc(&s.last_check_at)?,
    })
}

fn parse_utc(s: &str) -> Option<u64> {
    let ts: jiff::Timestamp = s.parse().ok()?;
    u64::try_from(ts.as_second()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Class, Group, HistoryItem, Item};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn item(name: &str, bytes: u64, ticked: bool) -> Item {
        Item {
            id: format!("cache:{name}"),
            group: Group::Caches,
            class: if ticked { Class::Safe } else { Class::Review },
            ticked,
            blocked: None,
            name: name.into(),
            reason: "r".into(),
            path: PathBuf::from("/h").join(name),
            bytes_alloc: bytes,
            bytes_logical: bytes,
            newest_mtime: Some(1),
            members: Vec::new(),
        }
    }

    fn trashed(name: &str, result: ItemResult) -> HistoryItem {
        HistoryItem {
            path: PathBuf::from("/h").join(name),
            trash_path: None,
            bytes: 0,
            rule: "r".into(),
            result,
            reason: None,
            trash_id: None,
        }
    }

    #[test]
    fn json_shape_matches_the_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(tmp.path());
        write(
            &paths,
            &ShelfStatus::new(1_782_907_200, 6_300_000_000, 1_782_900_000),
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(paths.shelf_status_file()).unwrap()).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<_> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "app",
                "app_version",
                "last_check_at",
                "reclaimable_bytes",
                "schema",
                "updated_at"
            ]
        );
        assert_eq!(v["schema"], 1);
        assert_eq!(v["app"], "tidy");
        assert_eq!(v["app_version"], app_version());
        assert_eq!(v["reclaimable_bytes"], 6_300_000_000u64);
        assert_eq!(v["updated_at"], "2026-07-01T12:00:00Z");
        assert_eq!(v["last_check_at"], "2026-07-01T10:00:00Z");
        assert!(std::fs::metadata(paths.shelf_status_file()).unwrap().len() < MAX_BYTES as u64);
        assert!(!v.to_string().contains('/'), "no paths");
    }

    #[test]
    fn write_is_atomic_mode_0600_and_creates_the_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(tmp.path());
        assert!(!paths.shelf_status.exists());
        write(&paths, &ShelfStatus::new(10, 1, 5)).unwrap();
        write(&paths, &ShelfStatus::new(20, 2, 5)).unwrap();
        let mode = std::fs::metadata(paths.shelf_status_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(&paths.shelf_status)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["tidy.json"], "no temp file is left");
        assert_eq!(read(&paths).unwrap().reclaimable_bytes, 2);
        // A stale temp file from a crash does not block the next write.
        std::fs::write(paths.shelf_status.join(TMP), b"junk").unwrap();
        write(&paths, &ShelfStatus::new(30, 3, 5)).unwrap();
        assert_eq!(read(&paths).unwrap().reclaimable_bytes, 3);
        assert!(!paths.shelf_status.join(TMP).exists());
    }

    #[test]
    fn follows_tidy_home() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(tmp.path());
        assert_eq!(paths.shelf_status, tmp.path().join("shelf-status"));
        assert!(!AppPaths::system().overridden);
        assert!(AppPaths::system().shelf_status.ends_with("Shelf/status"));
    }

    #[test]
    fn a_clean_removes_only_trashed_ticked_bytes() {
        let plan = Plan {
            items: vec![
                item("a", 100, true),
                item("b", 50, true),
                item("r", 900, false),
            ],
            ..Plan::default()
        };
        let entry = HistoryEntry {
            id: "x".into(),
            at: 1,
            action: crate::model::Action::Clean,
            source: crate::model::Source::Cleanup,
            trasher: crate::model::TrasherKind::Dir,
            items: vec![
                trashed("a", ItemResult::Trashed),
                trashed("b", ItemResult::Skipped),
                trashed("r", ItemResult::Trashed),
            ],
            trashed_bytes: 0,
            message: None,
        };
        let s = ShelfState {
            reclaimable_bytes: 150,
            last_check_at: 7,
        }
        .after_clean(&plan, &entry);
        assert_eq!(
            s,
            ShelfState {
                reclaimable_bytes: 50,
                last_check_at: 7
            }
        );
    }
}
