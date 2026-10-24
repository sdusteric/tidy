//! `history.jsonl` (design section 8.3): one JSON object per user action, appended with
//! `O_APPEND` and fsync. A line that does not parse is skipped on read and logged, never
//! fatal.
//!
//! Besides reading and writing, this module answers "which items that Tidy moved to the
//! Trash are still there" ([`in_trash`]). The status bar total and Empty Trash both use it,
//! because listing `~/.Trash` itself needs Full Disk Access (C8).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::model::{FileId, HistoryEntry, ItemResult, TrashSummary, TrasherKind, format_utc};
use crate::paths::AppPaths;
use crate::store::{append_json_line, read_json_lines};

/// Every entry in `file`, oldest first. A missing file is empty; corrupt lines are skipped.
pub fn read_file(file: &Path) -> io::Result<Vec<HistoryEntry>> {
    read_json_lines(file)
}

/// Every entry in Tidy's history, oldest first. A file that cannot be read gives an empty
/// list (logged), so a broken history never stops a clean or the app.
pub fn read(paths: &AppPaths) -> Vec<HistoryEntry> {
    let file = paths.history_file();
    read_file(&file).unwrap_or_else(|e| {
        tracing::warn!("cannot read {}: {e}", file.display());
        Vec::new()
    })
}

/// The last `n` entries, oldest first.
pub fn last(paths: &AppPaths, n: usize) -> Vec<HistoryEntry> {
    let mut all = read(paths);
    let skip = all.len().saturating_sub(n);
    all.drain(..skip);
    all
}

/// The id for a new entry written at `at`: `<UTC time>-<n>`, where `n` is one more than the
/// number of entries already written in that same second.
pub fn next_id(existing: &[HistoryEntry], at: u64) -> String {
    let prefix = format!("{}-", format_utc(at));
    let taken = existing
        .iter()
        .filter(|e| e.id.starts_with(&prefix))
        .count();
    HistoryEntry::make_id(at, u32::try_from(taken + 1).unwrap_or(u32::MAX))
}

/// Gives `entry` a fresh id (see [`next_id`]) and appends it to `history.jsonl`. Returns the
/// entry as written.
pub fn append(paths: &AppPaths, mut entry: HistoryEntry) -> io::Result<HistoryEntry> {
    let file = paths.history_file();
    let existing = read_file(&file).unwrap_or_default();
    entry.id = next_id(&existing, entry.at);
    append_json_line(&file, &entry)?;
    Ok(entry)
}

/// One item Tidy moved to the Trash that is still there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InTrash {
    /// Where it was before the move.
    pub path: PathBuf,
    /// Where it is in the Trash.
    pub trash_path: PathBuf,
    /// The size recorded when it moved.
    pub bytes: u64,
    /// The rule id (cleanup) or `large`.
    pub rule: String,
    /// The history entry that moved it.
    pub entry_id: String,
    /// What Tidy saw at `trash_path` right after the move.
    pub trash_id: Option<FileId>,
}

/// What a recorded Trash path holds now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrashState {
    /// The same object Tidy moved there (same device, inode and birth time).
    Same,
    /// Nothing is there.
    Gone,
    /// Something else took the name (the Trash reuses a name once it is free), or Tidy has no
    /// record of what it moved there. It is not Tidy's to delete.
    Other,
    /// The path cannot be read (macOS may refuse to look inside `~/.Trash` without Full
    /// Disk Access), so Tidy cannot tell.
    Unknown,
}

/// What `trash_path` holds now compared with `recorded`, the identity read after the move.
pub fn trash_state(trash_path: &Path, recorded: Option<FileId>) -> TrashState {
    match FileId::of(trash_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => TrashState::Gone,
        Err(_) => TrashState::Unknown,
        Ok(now) if Some(now) == recorded => TrashState::Same,
        Ok(_) => TrashState::Other,
    }
}

/// The items of `entries` that went to a Trash of `kind` and whose recorded Trash path still
/// holds the object Tidy moved there, in the order they were moved. A Trash path that two
/// entries name (the Trash reused a name after the first item left) counts once, with the
/// later entry's details. A path that now holds something else, or whose identity Tidy did
/// not record, does not count ([`TrashState::Other`]). A path whose state cannot be read
/// counts as there: macOS may refuse to look inside `~/.Trash` without Full Disk Access.
pub fn in_trash(entries: &[HistoryEntry], kind: TrasherKind) -> Vec<InTrash> {
    let mut order: Vec<PathBuf> = Vec::new();
    let mut latest: HashMap<PathBuf, InTrash> = HashMap::new();
    for entry in entries.iter().filter(|e| e.trasher == kind) {
        for item in &entry.items {
            if item.result != ItemResult::Trashed {
                continue;
            }
            let Some(tp) = &item.trash_path else { continue };
            let found = InTrash {
                path: item.path.clone(),
                trash_path: tp.clone(),
                bytes: item.bytes,
                rule: item.rule.clone(),
                entry_id: entry.id.clone(),
                trash_id: item.trash_id,
            };
            if latest.insert(tp.clone(), found).is_none() {
                order.push(tp.clone());
            }
        }
    }
    order
        .into_iter()
        .filter_map(|tp| latest.remove(&tp))
        .filter(|f| {
            matches!(
                trash_state(&f.trash_path, f.trash_id),
                TrashState::Same | TrashState::Unknown
            )
        })
        .collect()
}

/// Count and total of [`in_trash`]: "In the Trash from Tidy: 2.1 GB".
pub fn summarize(items: &[InTrash]) -> TrashSummary {
    TrashSummary {
        items: items.len() as u64,
        bytes: items.iter().map(|i| i.bytes).sum(),
    }
}

/// [`in_trash`] over Tidy's history for a Trash of `kind`, summed.
pub fn trash_summary(paths: &AppPaths, kind: TrasherKind) -> TrashSummary {
    summarize(&in_trash(&read(paths), kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::TEST_NOW;
    use crate::model::{Action, HistoryItem, Source};
    use crate::store::append_line;

    fn item(path: &str, trash: Option<&Path>, bytes: u64, result: ItemResult) -> HistoryItem {
        HistoryItem {
            path: PathBuf::from(path),
            trash_path: trash.map(Path::to_path_buf),
            bytes,
            rule: "cache.app".into(),
            result,
            reason: None,
            // What is at the Trash path now, the way a move records it.
            trash_id: trash.and_then(|p| FileId::of(p).ok()),
        }
    }

    fn entry(at: u64, trasher: TrasherKind, items: Vec<HistoryItem>) -> HistoryEntry {
        HistoryEntry {
            id: String::new(),
            at,
            action: Action::Clean,
            source: Source::Cleanup,
            trasher,
            trashed_bytes: items
                .iter()
                .filter(|i| i.result == ItemResult::Trashed)
                .map(|i| i.bytes)
                .sum(),
            items,
            message: None,
        }
    }

    #[test]
    fn append_numbers_entries_within_a_second_and_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(tmp.path());
        assert!(read(&paths).is_empty());
        let a = append(&paths, entry(TEST_NOW, TrasherKind::Dir, vec![])).unwrap();
        let b = append(&paths, entry(TEST_NOW, TrasherKind::Dir, vec![])).unwrap();
        let c = append(&paths, entry(TEST_NOW + 1, TrasherKind::Dir, vec![])).unwrap();
        assert_eq!(a.id, "2026-10-23T14:02:11Z-1");
        assert_eq!(b.id, "2026-10-23T14:02:11Z-2");
        assert_eq!(c.id, "2026-10-23T14:02:12Z-1");
        assert_eq!(read(&paths), [a.clone(), b.clone(), c.clone()]);
        assert_eq!(last(&paths, 2), [b, c.clone()]);
        assert_eq!(last(&paths, 0), []);
        assert_eq!(last(&paths, 9).len(), 3);
        // One compact line per entry.
        let text = std::fs::read_to_string(paths.history_file()).unwrap();
        assert_eq!(text.lines().count(), 3);
        assert!(text.ends_with('\n'));
        assert_eq!(next_id(&[c], TEST_NOW + 1), "2026-10-23T14:02:12Z-2");
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(tmp.path());
        let a = append(&paths, entry(TEST_NOW, TrasherKind::Dir, vec![])).unwrap();
        append_line(&paths.history_file(), "{\"id\":\"half").unwrap();
        append_line(&paths.history_file(), "").unwrap();
        append_line(&paths.history_file(), "[1,2,3]").unwrap();
        let b = append(&paths, entry(TEST_NOW, TrasherKind::Dir, vec![])).unwrap();
        // The broken lines do not count toward the id either.
        assert_eq!(b.id, "2026-10-23T14:02:11Z-2");
        assert_eq!(read(&paths), [a, b]);
    }

    #[test]
    fn an_unreadable_history_reads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(tmp.path());
        // A folder where the file should be.
        std::fs::create_dir_all(paths.history_file()).unwrap();
        assert!(read(&paths).is_empty());
        assert!(read_file(&paths.history_file()).is_err());
    }

    #[test]
    fn in_trash_lists_what_is_still_there_once() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        let there = t.join("there");
        let gone = t.join("gone");
        let reused = t.join("reused");
        std::fs::write(&there, b"x").unwrap();
        std::fs::write(&reused, b"x").unwrap();
        let mut entries = vec![
            entry(
                1,
                TrasherKind::Dir,
                vec![
                    item("/h/a", Some(&there), 10, ItemResult::Trashed),
                    item("/h/b", Some(&gone), 20, ItemResult::Trashed),
                    item("/h/c", Some(&reused), 30, ItemResult::Trashed),
                    item("/h/d", None, 40, ItemResult::Trashed),
                    item("/h/e", None, 50, ItemResult::Skipped),
                ],
            ),
            entry(
                2,
                TrasherKind::Dir,
                vec![item("/h/c2", Some(&reused), 35, ItemResult::Trashed)],
            ),
            // Another Trash: never counted here.
            entry(
                3,
                TrasherKind::System,
                vec![item("/h/s", Some(&there), 99, ItemResult::Trashed)],
            ),
        ];
        entries[0].id = "one".into();
        entries[1].id = "two".into();
        let found = in_trash(&entries, TrasherKind::Dir);
        let paths: Vec<&Path> = found.iter().map(|i| i.path.as_path()).collect();
        assert_eq!(paths, [Path::new("/h/a"), Path::new("/h/c2")]);
        assert_eq!(found[1].bytes, 35);
        assert_eq!(found[1].entry_id, "two");
        assert_eq!(found[1].trash_path, reused);
        assert_eq!(
            summarize(&found),
            TrashSummary {
                items: 2,
                bytes: 45
            }
        );
        assert_eq!(in_trash(&entries, TrasherKind::System).len(), 1);
    }

    #[test]
    fn in_trash_skips_a_path_that_now_holds_something_else() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        let tp = t.join("trip.mov");
        let unrecorded = t.join("unrecorded.mov");
        std::fs::write(&tp, b"tidy moved this").unwrap();
        std::fs::write(&unrecorded, b"x").unwrap();
        let mut no_id = item("/h/u", Some(&unrecorded), 5, ItemResult::Trashed);
        no_id.trash_id = None;
        let entries = vec![entry(
            1,
            TrasherKind::Dir,
            vec![
                item("/h/trip.mov", Some(&tp), 10, ItemResult::Trashed),
                no_id,
            ],
        )];
        let recorded = entries[0].items[0].trash_id;
        assert_eq!(trash_state(&tp, recorded), TrashState::Same);
        // No record of what was moved: not Tidy's to count or delete.
        assert_eq!(trash_state(&unrecorded, None), TrashState::Other);
        assert_eq!(in_trash(&entries, TrasherKind::Dir).len(), 1);
        // The item leaves the Trash (Put Back, or emptied in Finder) and a different item
        // the user trashed later lands under the same name.
        std::fs::remove_file(&tp).unwrap();
        assert_eq!(trash_state(&tp, recorded), TrashState::Gone);
        std::fs::write(&tp, b"the user's own file").unwrap();
        assert_eq!(trash_state(&tp, recorded), TrashState::Other);
        assert!(in_trash(&entries, TrasherKind::Dir).is_empty());
    }

    #[test]
    fn trash_summary_reads_the_history_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(&tmp.path().join("tidy"));
        let kept = tmp.path().join("kept");
        std::fs::write(&kept, b"x").unwrap();
        append(
            &paths,
            entry(
                TEST_NOW,
                TrasherKind::Dir,
                vec![item("/h/a", Some(&kept), 4096, ItemResult::Trashed)],
            ),
        )
        .unwrap();
        assert_eq!(
            trash_summary(&paths, TrasherKind::Dir),
            TrashSummary {
                items: 1,
                bytes: 4096
            }
        );
        assert_eq!(
            trash_summary(&paths, TrasherKind::System),
            TrashSummary::default()
        );
    }
}
