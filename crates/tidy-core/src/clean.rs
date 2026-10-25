//! The executor: everything that moves or deletes. Clean (Cleanup items), Move to Trash
//! (Large files paths) and Empty Trash (design section 5.1).
//!
//! - [`clean`] re-checks each item right before its move ([`Rechecker`], design 6.2 rule 5),
//!   moves it with the [`Env`]'s trasher and records where it went. One failure never stops
//!   the rest.
//! - [`trash_paths`] moves Large files paths, each through the [`check`] guard (strictly
//!   inside the scan root, off the denylist, never one of Tidy's own folders).
//! - [`empty_tidy_trash`] deletes for good only the items Tidy moved to the Trash, by the
//!   Trash paths recorded in history (C8). Nothing else in the Trash is touched or counted.
//!
//! Each action writes one entry to `history.jsonl` and returns it. Progress goes out as the
//! same [`Event`]s the core sends to the UI.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::Event;
use crate::cleanup::Rechecker;
use crate::env::Env;
use crate::history;
use crate::history::TrashState;
use crate::model::{
    Action, FileId, HistoryEntry, HistoryItem, Item, ItemResult, Source, TrasherKind,
};
use crate::paths::HOME_ENV;
use crate::trash::{Checked, Refusal, check, error_reason};
use crate::walk::{WalkOpts, measure};

/// The message of an Empty Trash that macOS refused (EPERM), shown in the notice band and
/// written to the history entry.
pub const EMPTY_TRASH_REFUSED: &str =
    "macOS didn't let Tidy empty the Trash. Empty it from Finder.";
/// The skip reason for an id that the last scan did not produce.
pub const NOT_IN_PLAN: &str = "Not in the last scan. Scan again first.";
/// The skip reason for items left when Tidy was asked to stop (it is quitting).
pub const STOPPED: &str = "Tidy stopped before this item";
/// The skip reason for a path that has gone since the scan.
pub const GONE: &str = "No longer there";
/// The skip reason for a recorded Trash path that is not inside a Trash folder.
pub const NOT_IN_TRASH: &str = "Not in the Trash, left alone";
/// The skip reason for a recorded Trash path that now holds a different item (the Trash
/// reused the name after Tidy's item left).
pub const NOT_TIDYS: &str = "Not the item Tidy moved, left alone";
/// The skip reason for a path on the user's ignore list, inside one, or holding one.
pub const ON_IGNORE_LIST: &str = "On your ignore list";
/// The skip reason when the ignore list cannot be read. The run stops there: every item left
/// is skipped with it.
pub const IGNORE_LIST_UNREADABLE: &str = "Tidy couldn't read your ignore list, skipped";
/// The history message of a run that stopped because the ignore list could not be read.
pub const IGNORE_LIST_STOPPED: &str =
    "Tidy couldn't read your ignore list, so it stopped before moving anything else.";
/// The skip reason when the open files cannot be read.
pub const OPEN_FILES_UNKNOWN: &str = "Tidy couldn't check which files are open, skipped";
/// The rule recorded for Large files items.
pub const LARGE_RULE: &str = "large";
/// The rule recorded for Empty Trash items.
pub const EMPTY_TRASH_RULE: &str = "trash";

/// Moves `items` (rows of a Cleanup plan) to the Trash. See [`clean_until`].
pub fn clean(env: &Env, items: &[Item], events: &mut dyn FnMut(Event)) -> HistoryEntry {
    clean_until(env, items, events, None)
}

/// Moves `items` to the Trash, one by one, re-checking each right before its move. Sends
/// [`Event::ItemTrashed`] or [`Event::ItemSkipped`] per item and [`Event::CleanProgress`]
/// after each. Once `stop` is set, the remaining items are skipped with [`STOPPED`]. Writes
/// one `clean` entry to history (none when `items` is empty) and returns it.
///
/// A folded row ("38 small items") moves its members that still pass, each recorded with its
/// own size. A plain row is recorded with the size the scan measured.
pub fn clean_until(
    env: &Env,
    items: &[Item],
    events: &mut dyn FnMut(Event),
    stop: Option<&AtomicBool>,
) -> HistoryEntry {
    let rechecker = Rechecker::new(env);
    let total = items.len();
    let mut out: Vec<HistoryItem> = Vec::new();
    let mut halted = false;
    for (n, item) in items.iter().enumerate() {
        let rule = item.rule_id().to_string();
        // Read right before this item's move: the ignore list and the open files.
        let gate = if halted {
            Err(IGNORE_LIST_UNREADABLE.to_string())
        } else {
            MoveGate::read(env).inspect_err(|_| halted = true)
        };
        let checked = if stopped(stop) {
            Err(STOPPED.to_string())
        } else {
            gate.and_then(|gate| rechecker.check(item).map(|paths| (gate, paths)))
        };
        match checked {
            Err(reason) => {
                out.push(record(
                    &item.path,
                    None,
                    None,
                    item.bytes_alloc,
                    &rule,
                    ItemResult::Skipped,
                    Some(reason.clone()),
                ));
                events(Event::ItemSkipped {
                    id: item.id.clone(),
                    reason,
                });
            }
            Ok((gate, paths)) => {
                let folded = !item.members.is_empty();
                let mut moved: Vec<Option<PathBuf>> = Vec::new();
                let mut first_err: Option<String> = None;
                for path in paths {
                    let bytes = if folded {
                        measure(&path, &WalkOpts::for_root(&env.root)).alloc
                    } else {
                        item.bytes_alloc
                    };
                    match guarded(env, &path)
                        .and_then(|at| gate.allows(&path, at))
                        .and_then(|at| move_checked(env, &at))
                    {
                        Ok((dest, moved_id)) => {
                            out.push(record(
                                &path,
                                dest.clone(),
                                moved_id,
                                bytes,
                                &rule,
                                ItemResult::Trashed,
                                None,
                            ));
                            moved.push(dest);
                        }
                        Err((result, reason)) => {
                            out.push(record(
                                &path,
                                None,
                                None,
                                bytes,
                                &rule,
                                result,
                                Some(reason.clone()),
                            ));
                            first_err.get_or_insert(reason);
                        }
                    }
                }
                if moved.is_empty() {
                    events(Event::ItemSkipped {
                        id: item.id.clone(),
                        reason: first_err.unwrap_or_else(|| GONE.to_string()),
                    });
                } else {
                    // A folded row went to several places; the row has no single Trash path.
                    let trash_path = if folded { None } else { moved.remove(0) };
                    events(Event::ItemTrashed {
                        id: item.id.clone(),
                        trash_path,
                    });
                }
            }
        }
        events(Event::CleanProgress { done: n + 1, total });
    }
    let message = halted.then(|| IGNORE_LIST_STOPPED.to_string());
    finish(env, Action::Clean, Source::Cleanup, out, message)
}

/// Moves Large files paths to the Trash. See [`trash_paths_until`].
pub fn trash_paths(env: &Env, paths: &[PathBuf], events: &mut dyn FnMut(Event)) -> HistoryEntry {
    trash_paths_until(env, paths, events, None)
}

/// Moves each of `paths` to the Trash when the guard puts it strictly inside the scan root,
/// off the denylist and outside Tidy's own folders. The id in each event is the path as given.
/// Each item is measured right before its move. Writes one `trash` entry (none when `paths`
/// is empty) and returns it.
pub fn trash_paths_until(
    env: &Env,
    paths: &[PathBuf],
    events: &mut dyn FnMut(Event),
    stop: Option<&AtomicBool>,
) -> HistoryEntry {
    let total = paths.len();
    let mut out = Vec::new();
    let mut halted = false;
    for (n, path) in paths.iter().enumerate() {
        let id = path.to_string_lossy().into_owned();
        let at = if stopped(stop) {
            Err((ItemResult::Skipped, STOPPED.to_string()))
        } else if halted {
            Err((ItemResult::Skipped, IGNORE_LIST_UNREADABLE.to_string()))
        } else {
            match MoveGate::read(env) {
                Ok(gate) => guarded(env, path).and_then(|at| gate.allows(path, at)),
                Err(reason) => {
                    halted = true;
                    Err((ItemResult::Skipped, reason))
                }
            }
        };
        let bytes = match &at {
            Ok(at) => measure(at, &WalkOpts::for_root(&env.root)).alloc,
            Err(_) => 0,
        };
        match at.and_then(|at| move_checked(env, &at)) {
            Ok((trash_path, moved_id)) => {
                out.push(record(
                    path,
                    trash_path.clone(),
                    moved_id,
                    bytes,
                    LARGE_RULE,
                    ItemResult::Trashed,
                    None,
                ));
                events(Event::ItemTrashed { id, trash_path });
            }
            Err((result, reason)) => {
                out.push(record(
                    path,
                    None,
                    None,
                    bytes,
                    LARGE_RULE,
                    result,
                    Some(reason.clone()),
                ));
                events(Event::ItemSkipped { id, reason });
            }
        }
        events(Event::CleanProgress { done: n + 1, total });
    }
    let message = halted.then(|| IGNORE_LIST_STOPPED.to_string());
    finish(env, Action::Trash, Source::Large, out, message)
}

/// Deletes for good the items Tidy moved to the Trash. See [`empty_tidy_trash_until`].
pub fn empty_tidy_trash(env: &Env) -> HistoryEntry {
    empty_tidy_trash_until(env, None)
}

/// Deletes for good the items Tidy moved to the Trash that are still there, by the Trash
/// paths recorded in history for this [`Env`]'s kind of Trash. A recorded path is deleted
/// only when it sits directly in a Trash folder: `data/trash` for the folder Trash, and
/// `.Trash` (or a volume's `.Trashes/<uid>`) for the macOS Trash, which is never touched
/// while `TIDY_HOME` is set. It must also still hold the same object Tidy moved there (same
/// device, inode and birth time, recorded at the move); a different item that took the name
/// later is skipped with [`NOT_TIDYS`], and a path Tidy cannot read is not deleted.
///
/// When macOS refuses (EPERM), the item fails with "macOS didn't allow access" and the
/// entry's message is [`EMPTY_TRASH_REFUSED`]. Writes one `empty_trash` entry (none when
/// there was nothing to delete) and returns it.
pub fn empty_tidy_trash_until(env: &Env, stop: Option<&AtomicBool>) -> HistoryEntry {
    let kind = env.trasher.kind();
    let found = history::in_trash(&history::read(&env.paths), kind);
    let mut out = Vec::new();
    let mut refused = false;
    for f in found {
        let (result, reason) = if stopped(stop) {
            (ItemResult::Skipped, Some(STOPPED.to_string()))
        } else if !in_a_trash_folder(env, kind, &f.trash_path) {
            tracing::warn!(path = %f.trash_path.display(), "recorded Trash path is not in a Trash folder");
            (ItemResult::Skipped, Some(NOT_IN_TRASH.to_string()))
        } else {
            // Right before the delete: the path must still hold the very item Tidy moved.
            match history::trash_state(&f.trash_path, f.trash_id) {
                TrashState::Gone => (ItemResult::Missing, Some(GONE.to_string())),
                TrashState::Other => {
                    tracing::warn!(path = %f.trash_path.display(), "Trash path now holds another item");
                    (ItemResult::Skipped, Some(NOT_TIDYS.to_string()))
                }
                TrashState::Unknown => {
                    // Without reading it Tidy cannot tell what is there, so it deletes nothing.
                    refused = true;
                    (
                        ItemResult::Failed,
                        Some(error_reason(&io::Error::from_raw_os_error(libc::EPERM))),
                    )
                }
                TrashState::Same => match delete(&f.trash_path) {
                    Ok(()) => (ItemResult::Deleted, None),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        (ItemResult::Missing, Some(GONE.to_string()))
                    }
                    Err(e) => {
                        tracing::warn!(path = %f.trash_path.display(), "could not delete: {e}");
                        if is_permission(&e) {
                            refused = true;
                        }
                        (ItemResult::Failed, Some(error_reason(&e)))
                    }
                },
            }
        };
        out.push(HistoryItem {
            path: f.path,
            trash_path: Some(f.trash_path),
            bytes: f.bytes,
            rule: EMPTY_TRASH_RULE.to_string(),
            result,
            reason,
            trash_id: f.trash_id,
        });
    }
    let message = refused.then(|| EMPTY_TRASH_REFUSED.to_string());
    finish(env, Action::EmptyTrash, Source::Cleanup, out, message)
}

// ---------- Helpers ----------

fn stopped(stop: Option<&AtomicBool>) -> bool {
    stop.is_some_and(|s| s.load(Ordering::Relaxed))
}

/// One history item. For a move that reached the Trash, it also reads the identity of what
/// now sits at the Trash path, so Empty Trash can later tell it from a different item that
/// took the same name. When the Trash path cannot be read (macOS refuses to look inside
/// `~/.Trash` without Full Disk Access), it keeps `moved_id`, the identity read from the
/// original path right before the move: a move to the Trash is a rename on the same volume,
/// which keeps the device, inode and birth time.
fn record(
    path: &Path,
    trash_path: Option<PathBuf>,
    moved_id: Option<FileId>,
    bytes: u64,
    rule: &str,
    result: ItemResult,
    reason: Option<String>,
) -> HistoryItem {
    let trash_id = match (&trash_path, result) {
        (Some(tp), ItemResult::Trashed) => FileId::of(tp).ok().or(moved_id),
        _ => None,
    };
    HistoryItem {
        path: path.to_path_buf(),
        trash_path,
        bytes,
        rule: rule.to_string(),
        result,
        reason,
        trash_id,
    }
}

/// The last checks before a move, read fresh for each item (design 6.2, rule 5), for every
/// path to the Trash: a Cleanup clean, a Large files move and project build output alike.
///
/// - The ignore list is read from settings.json, together with the one in memory. When the
///   file cannot be read the run stops: Tidy never moves a path it may have been told to
///   leave alone.
/// - The open files of the user's processes are read with libproc. A path that a process
///   holds open, or holds something open inside, is skipped; when they cannot be read, every
///   path is skipped.
struct MoveGate {
    ignored: Vec<PathBuf>,
    open: Result<crate::env::OpenFiles, String>,
}

impl MoveGate {
    /// Reads the ignore list and the open files. The error is the ignore list's.
    fn read(env: &Env) -> Result<MoveGate, String> {
        let mut ignored = crate::settings::Settings::read_ignore_list(&env.paths.settings_file())
            .map_err(|e| {
            tracing::warn!("cannot read the ignore list: {e}");
            IGNORE_LIST_UNREADABLE.to_string()
        })?;
        ignored.extend(env.settings.ignored_paths.iter().cloned());
        // Both spellings of each entry: as saved, and resolved (`/var` is `/private/var`).
        let resolved: Vec<PathBuf> = ignored
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .collect();
        ignored.extend(resolved);
        let open = env.open_files.open_files().map_err(|e| {
            tracing::warn!("cannot read the open files: {e}");
            OPEN_FILES_UNKNOWN.to_string()
        });
        Ok(MoveGate { ignored, open })
    }

    /// Passes `at` (the resolved form of `path`) on when neither check refuses it.
    fn allows(&self, path: &Path, at: PathBuf) -> Result<PathBuf, (ItemResult, String)> {
        let touches = |p: &Path| {
            self.ignored
                .iter()
                .any(|i| p.starts_with(i) || i.starts_with(p))
        };
        if touches(path) || touches(&at) {
            return Err((ItemResult::Skipped, ON_IGNORE_LIST.to_string()));
        }
        match &self.open {
            Err(reason) => Err((ItemResult::Skipped, reason.clone())),
            Ok(open) => match open.inside(&at) {
                Some(f) => Err((
                    ItemResult::Skipped,
                    format!("Open in {}, skipped", f.holder),
                )),
                None => Ok(at),
            },
        }
    }
}

/// The guard every move goes through: [`check`] against the scan root and the denylist, plus
/// Tidy's own folders wherever they are. Returns the resolved path to move.
fn guarded(env: &Env, path: &Path) -> Result<PathBuf, (ItemResult, String)> {
    match check(&env.root, path) {
        Checked::Inside(at) if touches_tidy(env, &at) => {
            Err((ItemResult::Skipped, Refusal::Denied.reason().to_string()))
        }
        Checked::Inside(at) => Ok(at),
        Checked::Missing => Err((ItemResult::Missing, GONE.to_string())),
        Checked::Refused(why) => Err((ItemResult::Skipped, why.reason().to_string())),
    }
}

/// Moves a path the guard accepted. EPERM and EACCES read "macOS didn't allow access" and
/// are never retried (design 6.2, rule 6).
fn move_checked(
    env: &Env,
    at: &Path,
) -> Result<(Option<PathBuf>, Option<FileId>), (ItemResult, String)> {
    // Read before the move: the Trash path may be unreadable once the item is there.
    let moved_id = FileId::of(at).ok();
    env.trasher.trash(at).map(|tp| (tp, moved_id)).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            (ItemResult::Missing, GONE.to_string())
        } else {
            tracing::warn!(path = %at.display(), "could not move to the Trash: {e}");
            (ItemResult::Failed, error_reason(&e))
        }
    })
}

/// Whether `at` is one of Tidy's folders, inside one, or holds one.
fn touches_tidy(env: &Env, at: &Path) -> bool {
    let p = &env.paths;
    [&p.data, &p.cache, &p.logs].into_iter().any(|d| {
        let d = d.canonicalize().unwrap_or_else(|_| d.clone());
        at.starts_with(&d) || d.starts_with(at)
    })
}

/// Whether a recorded Trash path sits directly in a Trash folder of this kind.
fn in_a_trash_folder(env: &Env, kind: TrasherKind, tp: &Path) -> bool {
    let (Some(parent), Some(name)) = (tp.parent(), tp.file_name()) else {
        return false;
    };
    if Path::new(name).components().count() != 1
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
    {
        return false;
    }
    match kind {
        TrasherKind::Dir => {
            let (Ok(dir), Ok(parent)) =
                (env.paths.dir_trash().canonicalize(), parent.canonicalize())
            else {
                return false;
            };
            parent == dir
        }
        TrasherKind::System => {
            // Never the macOS Trash in a test or agent run.
            if env.overridden || std::env::var_os(HOME_ENV).is_some_and(|v| !v.is_empty()) {
                return false;
            }
            let named =
                |p: Option<&Path>, n: &str| p.and_then(Path::file_name).is_some_and(|f| f == n);
            named(Some(parent), ".Trash") || named(parent.parent(), ".Trashes")
        }
    }
}

/// Deletes a file, a link (not what it points to) or a folder with everything in it.
fn delete(path: &Path) -> io::Result<()> {
    let meta = path.symlink_metadata()?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

fn is_permission(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
        || matches!(e.raw_os_error(), Some(libc::EPERM | libc::EACCES))
}

/// Builds the entry, appends it to history (unless it has no items) and returns it. A failed
/// write is logged and noted in the entry's message; the action itself already happened.
fn finish(
    env: &Env,
    action: Action,
    source: Source,
    items: Vec<HistoryItem>,
    message: Option<String>,
) -> HistoryEntry {
    let counted = match action {
        Action::EmptyTrash => ItemResult::Deleted,
        Action::Clean | Action::Trash => ItemResult::Trashed,
    };
    let at = env.now();
    let entry = HistoryEntry {
        id: history::next_id(&[], at),
        at,
        action,
        source,
        trasher: env.trasher.kind(),
        trashed_bytes: items
            .iter()
            .filter(|i| i.result == counted)
            .map(|i| i.bytes)
            .sum(),
        items,
        message,
    };
    if entry.items.is_empty() {
        return entry;
    }
    match history::append(&env.paths, entry.clone()) {
        Ok(written) => written,
        Err(e) => {
            tracing::warn!("cannot write history: {e}");
            HistoryEntry {
                message: entry
                    .message
                    .clone()
                    .or_else(|| Some(format!("Tidy couldn't write this to its history: {e}"))),
                ..entry
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{FixedProbe, TEST_NOW};
    use crate::model::{Class, Group, TrashSummary, from_unix_secs};
    use crate::trash::{DirTrash, Trasher};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    const MIB: usize = 1 << 20;

    struct Fx {
        _tmp: tempfile::TempDir,
        base: PathBuf,
        env: Env,
    }

    fn fx() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env = Env::for_tests(&home, &base.join("tidy"));
        Fx {
            _tmp: tmp,
            base,
            env,
        }
    }

    /// Writes `len` bytes of a non-zero pattern, dated 60 days before the test clock.
    fn put(path: &Path, len: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8 + 1).collect();
        std::fs::write(path, data).unwrap();
        let t = from_unix_secs(TEST_NOW - 60 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
            .unwrap();
    }

    /// The `.npm/_cacache` row, as a scan of this home would produce it.
    fn npm_item(env: &Env) -> Item {
        let path = env.at(".npm/_cacache");
        put(&path.join("a.bin"), 2 * MIB);
        Item {
            id: "dev.npm-cacache:.npm/_cacache".into(),
            group: Group::Developer,
            class: Class::Safe,
            ticked: true,
            blocked: None,
            name: "npm".into(),
            reason: "Package cache".into(),
            path,
            bytes_alloc: 2 * MIB as u64,
            bytes_logical: 2 * MIB as u64,
            newest_mtime: Some(TEST_NOW - 60 * 86_400),
            members: Vec::new(),
        }
    }

    struct Failing;
    impl Trasher for Failing {
        fn trash(&self, _: &Path) -> io::Result<Option<PathBuf>> {
            Err(io::Error::from_raw_os_error(libc::EPERM))
        }
    }

    #[test]
    fn clean_moves_a_passing_item_and_records_it() {
        let f = fx();
        let item = npm_item(&f.env);
        let mut events = Vec::new();
        let entry = clean(&f.env, std::slice::from_ref(&item), &mut |e| events.push(e));
        let dest = f.env.paths.dir_trash().join("_cacache");
        assert!(dest.join("a.bin").is_file());
        assert!(!item.path.exists());
        assert_eq!(
            events,
            [
                Event::ItemTrashed {
                    id: item.id.clone(),
                    trash_path: Some(dest.clone())
                },
                Event::CleanProgress { done: 1, total: 1 }
            ]
        );
        assert_eq!(entry.action, Action::Clean);
        assert_eq!(entry.trasher, TrasherKind::Dir);
        assert_eq!(entry.trashed_bytes, 2 * MIB as u64);
        assert_eq!(entry.items[0].rule, "dev.npm-cacache");
        assert_eq!(entry.items[0].trash_path, Some(dest));
        assert_eq!(history::read(&f.env.paths), [entry]);
    }

    #[test]
    fn clean_never_stops_on_one_failure_and_maps_eperm() {
        let f = fx();
        let item = npm_item(&f.env);
        let mut report = item.clone();
        report.id = "ai.vm:.colima".into();
        report.class = Class::ReportOnly;
        report.path = f.env.at(".colima");
        put(&report.path.join("disk.img"), 4096);
        let env = f.env.clone().with_trasher(Arc::new(Failing));
        let mut events = Vec::new();
        let entry = clean(&env, &[report.clone(), item.clone()], &mut |e| {
            events.push(e)
        });
        assert_eq!(entry.items.len(), 2);
        assert_eq!(entry.items[0].result, ItemResult::Skipped);
        assert_eq!(
            entry.items[0].reason.as_deref(),
            Some("Tidy won't remove this")
        );
        assert_eq!(entry.items[1].result, ItemResult::Failed);
        assert_eq!(
            entry.items[1].reason.as_deref(),
            Some("macOS didn't allow access")
        );
        assert_eq!(entry.trashed_bytes, 0);
        assert!(item.path.is_dir() && report.path.is_dir());
        assert_eq!(
            events.last(),
            Some(&Event::CleanProgress { done: 2, total: 2 })
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Event::ItemSkipped { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn clean_rechecks_the_owner_and_stops_when_asked() {
        let f = fx();
        let item = npm_item(&f.env);
        let probe = Arc::new(FixedProbe::parse("npm"));
        let env = f.env.clone().with_procs(probe.clone());
        let mut sink = |_: Event| {};
        let entry = clean(&env, std::slice::from_ref(&item), &mut sink);
        assert_eq!(entry.items[0].result, ItemResult::Skipped);
        assert!(entry.items[0].reason.as_deref().unwrap().contains("opened"));
        assert!(item.path.is_dir());
        probe.set(Default::default());
        let stop = AtomicBool::new(true);
        let entry = clean_until(&env, std::slice::from_ref(&item), &mut sink, Some(&stop));
        assert_eq!(entry.items[0].reason.as_deref(), Some(STOPPED));
        assert!(item.path.is_dir());
        // Nothing to clean writes nothing.
        let empty = clean(&env, &[], &mut sink);
        assert!(empty.items.is_empty());
        assert_eq!(history::read(&env.paths).len(), 2);
    }

    #[test]
    fn trash_paths_goes_through_the_guard() {
        let f = fx();
        let env = &f.env;
        put(&env.at("Movies/trip.mov"), 3 * MIB);
        put(&env.at("Library/Containers/x/big.bin"), 4096);
        put(&f.base.join("outside/secret.bin"), 4096);
        put(&env.paths.data.join("settings.json"), 10);
        // A settings file that reads, so the ignore list does not stop the run.
        std::fs::write(env.paths.data.join("settings.json"), "{}").unwrap();
        let paths = vec![
            env.at("Movies/trip.mov"),
            env.at("Library/Containers/x/big.bin"),
            f.base.join("outside/secret.bin"),
            env.root.clone(),
            env.at("gone.bin"),
            PathBuf::from("Movies"),
        ];
        let mut events = Vec::new();
        let entry = trash_paths(env, &paths, &mut |e| events.push(e));
        assert_eq!(entry.action, Action::Trash);
        assert_eq!(entry.source, Source::Large);
        let results: Vec<ItemResult> = entry.items.iter().map(|i| i.result).collect();
        assert_eq!(
            results,
            [
                ItemResult::Trashed,
                ItemResult::Skipped,
                ItemResult::Skipped,
                ItemResult::Skipped,
                ItemResult::Missing,
                ItemResult::Skipped,
            ]
        );
        assert_eq!(entry.items[0].bytes, 3 * MIB as u64);
        assert_eq!(entry.items[0].rule, LARGE_RULE);
        assert_eq!(entry.trashed_bytes, 3 * MIB as u64);
        assert!(env.at("Library/Containers/x/big.bin").is_file());
        assert!(f.base.join("outside/secret.bin").is_file());
        assert!(env.root.is_dir());
        assert_eq!(
            events[0],
            Event::ItemTrashed {
                id: env.at("Movies/trip.mov").to_string_lossy().into_owned(),
                trash_path: Some(env.paths.dir_trash().join("trip.mov")),
            }
        );
    }

    #[test]
    fn trash_paths_never_moves_tidys_own_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        // Tidy's folders inside the scan root.
        let env = Env::for_tests(&base, &base.join("tidy-home"));
        put(&env.paths.data.join("history.jsonl"), 10);
        put(&base.join("other.bin"), 4096);
        let mut sink = |_: Event| {};
        let entry = trash_paths(
            &env,
            &[env.paths.data.clone(), base.join("tidy-home")],
            &mut sink,
        );
        assert!(entry.items.iter().all(|i| i.result == ItemResult::Skipped));
        assert!(env.paths.data.join("history.jsonl").is_file());
    }

    #[test]
    fn empty_trash_deletes_only_what_tidy_recorded_inside_data_trash() {
        let f = fx();
        let env = &f.env;
        let item = npm_item(env);
        put(&env.at("Movies/trip.mov"), 4096);
        let mut sink = |_: Event| {};
        clean(env, std::slice::from_ref(&item), &mut sink);
        trash_paths(env, &[env.at("Movies/trip.mov")], &mut sink);
        // Something else in the folder Trash, and a forged history line that points outside.
        let stranger = env.paths.dir_trash().join("not-from-tidy.txt");
        put(&stranger, 10);
        let victim = env.at("Documents/keep.txt");
        put(&victim, 10);
        let forged = HistoryEntry {
            id: "x".into(),
            at: TEST_NOW,
            action: Action::Clean,
            source: Source::Cleanup,
            trasher: TrasherKind::Dir,
            items: vec![record(
                &victim,
                Some(victim.clone()),
                None,
                10,
                "cache.app",
                ItemResult::Trashed,
                None,
            )],
            trashed_bytes: 10,
            message: None,
        };
        history::append(&env.paths, forged).unwrap();
        assert_eq!(
            history::trash_summary(&env.paths, TrasherKind::Dir).items,
            3
        );

        let entry = empty_tidy_trash(env);
        assert_eq!(entry.action, Action::EmptyTrash);
        assert_eq!(entry.message, None);
        let by_result = |r| entry.items.iter().filter(|i| i.result == r).count();
        assert_eq!(by_result(ItemResult::Deleted), 2);
        assert_eq!(by_result(ItemResult::Skipped), 1);
        assert_eq!(entry.trashed_bytes, 2 * MIB as u64 + 4096);
        assert!(entry.items.iter().all(|i| i.rule == EMPTY_TRASH_RULE));
        assert!(!env.paths.dir_trash().join("_cacache").exists());
        assert!(!env.paths.dir_trash().join("trip.mov").exists());
        assert!(stranger.is_file());
        assert!(victim.is_file());
        // The deleted ones no longer count; the forged one still does, and stays refused.
        assert_eq!(
            history::trash_summary(&env.paths, TrasherKind::Dir).items,
            1
        );
        let again = empty_tidy_trash(env);
        assert_eq!(again.items.len(), 1);
        assert_eq!(again.items[0].reason.as_deref(), Some(NOT_IN_TRASH));
        assert!(victim.is_file());
    }

    #[test]
    fn empty_trash_reports_a_refusal_from_macos() {
        let f = fx();
        let env = &f.env;
        put(&env.at("Movies/locked/inner/a.bin"), 4096);
        let mut sink = |_: Event| {};
        trash_paths(env, &[env.at("Movies/locked")], &mut sink);
        let inner = env.paths.dir_trash().join("locked/inner");
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o500)).unwrap();
        let entry = empty_tidy_trash(env);
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(entry.message.as_deref(), Some(EMPTY_TRASH_REFUSED));
        assert_eq!(entry.items[0].result, ItemResult::Failed);
        assert_eq!(
            entry.items[0].reason.as_deref(),
            Some("macOS didn't allow access")
        );
        assert_eq!(entry.trashed_bytes, 0);
        assert!(inner.join("a.bin").is_file());
        // The failure is in history too.
        assert_eq!(
            history::read(&env.paths).last().unwrap().message.as_deref(),
            Some(EMPTY_TRASH_REFUSED)
        );
    }

    #[test]
    fn trash_paths_never_moves_a_folder_holding_denied_things() {
        let f = fx();
        let env = &f.env;
        put(&env.at("Library/Keychains/login.keychain-db"), 4096);
        put(&env.at("Library/Mail/inbox"), 4096);
        put(
            &env.at("Library/Application Support/Google/Chrome/Default/Login Data"),
            4096,
        );
        put(
            &env.at("Library/Application Support/Google/Chrome/Default/Cookies"),
            4096,
        );
        let mut sink = |_: Event| {};
        let entry = trash_paths(
            env,
            &[
                env.at("Library"),
                env.at("Library/Application Support/Google"),
            ],
            &mut sink,
        );
        assert!(entry.items.iter().all(|i| i.result == ItemResult::Skipped));
        assert_eq!(
            entry.items[0].reason.as_deref(),
            Some(Refusal::HoldsDenied.reason())
        );
        assert_eq!(entry.trashed_bytes, 0);
        assert!(env.at("Library/Keychains/login.keychain-db").is_file());
        assert!(env.at("Library/Mail/inbox").is_file());
        assert!(
            env.at("Library/Application Support/Google/Chrome/Default/Login Data")
                .is_file()
        );
        assert!(!env.paths.dir_trash().exists());
    }

    #[test]
    fn empty_trash_leaves_a_different_item_that_took_the_name() {
        let f = fx();
        let env = &f.env;
        put(&env.at("Movies/trip.mov"), 4096);
        let mut sink = |_: Event| {};
        let moved = trash_paths(env, &[env.at("Movies/trip.mov")], &mut sink);
        let tp = env.paths.dir_trash().join("trip.mov");
        assert_eq!(moved.items[0].trash_path.as_deref(), Some(tp.as_path()));
        assert_eq!(moved.items[0].trash_id, Some(FileId::of(&tp).unwrap()));
        // Put Back, then the user trashes a different file of the same name.
        std::fs::rename(&tp, env.at("Movies/trip.mov")).unwrap();
        put(&env.at("Downloads/trip.mov"), 8192);
        std::fs::rename(env.at("Downloads/trip.mov"), &tp).unwrap();
        assert_eq!(
            history::trash_summary(&env.paths, TrasherKind::Dir).items,
            0
        );
        let entry = empty_tidy_trash(env);
        // Nothing of Tidy's is in the Trash, so nothing is done.
        assert!(entry.items.is_empty());
        assert!(tp.is_file());
        assert_eq!(std::fs::metadata(&tp).unwrap().len(), 8192);
        // Even when the swap happens after the list is read, the delete re-checks.
        let found = history::in_trash(&history::read(&env.paths), TrasherKind::Dir);
        assert!(found.is_empty());
        assert_eq!(
            history::trash_state(&tp, moved.items[0].trash_id),
            TrashState::Other
        );
    }

    /// The folder Trash, locked after each move the way `~/.Trash` is without Full Disk
    /// Access: what Tidy moved is there, but Tidy cannot read its identity.
    struct LockedAfterMove(DirTrash, PathBuf);
    impl Trasher for LockedAfterMove {
        fn trash(&self, path: &Path) -> io::Result<Option<PathBuf>> {
            let out = self.0.trash(path)?;
            set_mode(&self.1, 0o000);
            Ok(out)
        }
    }

    fn set_mode(dir: &Path, mode: u32) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Unlocks the folder Trash even when an assert fails, so the temp dir can be removed.
    struct Unlock(PathBuf);
    impl Drop for Unlock {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn an_item_moved_while_the_trash_is_unreadable_is_emptied_once_it_is_readable() {
        let f = fx();
        let dir = f.env.paths.dir_trash();
        let env = &Env::for_tests(&f.env.root, &f.base.join("tidy")).with_trasher(Arc::new(
            LockedAfterMove(DirTrash::new(dir.clone()), dir.clone()),
        ));
        let _unlock = Unlock(dir.clone());
        put(&env.at("Movies/trip.mov"), 4096);
        let before = FileId::of(&env.at("Movies/trip.mov")).unwrap();
        let mut sink = |_: Event| {};
        let moved = trash_paths(env, &[env.at("Movies/trip.mov")], &mut sink);
        let tp = dir.join("trip.mov");
        assert_eq!(moved.items[0].result, ItemResult::Trashed);
        assert_eq!(moved.items[0].trash_path.as_deref(), Some(tp.as_path()));
        // The Trash path could not be read, so the identity came from before the move.
        assert!(FileId::of(&tp).is_err());
        assert_eq!(moved.items[0].trash_id, Some(before));
        assert_eq!(
            history::trash_state(&tp, moved.items[0].trash_id),
            TrashState::Unknown
        );
        assert_eq!(
            history::trash_summary(&env.paths, TrasherKind::Dir).items,
            1
        );
        // Still unreadable: Empty Trash deletes nothing and says macOS refused.
        let refused = empty_tidy_trash(env);
        assert_eq!(refused.items[0].result, ItemResult::Failed);
        assert_eq!(refused.message.as_deref(), Some(EMPTY_TRASH_REFUSED));

        // The user grants Full Disk Access: the path reads as the very item Tidy moved.
        set_mode(&dir, 0o755);
        assert!(tp.is_file());
        assert_eq!(FileId::of(&tp).unwrap(), before);
        assert_eq!(
            history::trash_state(&tp, moved.items[0].trash_id),
            TrashState::Same
        );
        assert_eq!(
            history::trash_summary(&env.paths, TrasherKind::Dir),
            TrashSummary {
                items: 1,
                bytes: moved.items[0].bytes
            }
        );
        let entry = empty_tidy_trash(env);
        assert_eq!(entry.message, None);
        assert_eq!(entry.items.len(), 1);
        assert_eq!(entry.items[0].result, ItemResult::Deleted);
        assert!(!tp.exists());
        assert_eq!(
            history::trash_summary(&env.paths, TrasherKind::Dir).items,
            0
        );
    }

    #[test]
    fn empty_trash_with_nothing_recorded_writes_nothing() {
        let f = fx();
        let entry = empty_tidy_trash(&f.env);
        assert!(entry.items.is_empty());
        assert!(!f.env.paths.history_file().exists());
    }

    #[test]
    fn the_macos_trash_is_never_emptied_under_tidy_home() {
        let f = fx();
        let tp = f.base.join(".Trash/x");
        put(&tp, 10);
        assert!(!in_a_trash_folder(&f.env, TrasherKind::System, &tp));
        assert!(in_a_trash_folder(&f.env, TrasherKind::Dir, &{
            let p = f.env.paths.dir_trash().join("y");
            put(&p, 10);
            p
        }));
        assert!(!in_a_trash_folder(
            &f.env,
            TrasherKind::Dir,
            &f.env.paths.dir_trash()
        ));
        // A DirTrash elsewhere is not data/trash.
        let other = DirTrash::new(f.base.join("elsewhere"));
        put(&f.base.join("home/z"), 10);
        let moved = other.trash(&f.base.join("home/z")).unwrap().unwrap();
        assert!(!in_a_trash_folder(&f.env, TrasherKind::Dir, &moved));
    }

    #[test]
    fn a_file_a_process_holds_open_stops_its_move() {
        use crate::env::FixedOpenFiles;
        let f = fx();
        let item = npm_item(&f.env);
        put(&f.env.at("Movies/trip.mov"), 4096);
        // macOS reports open files by their resolved path.
        let held = item.path.canonicalize().unwrap().join("a.bin");
        let env = f
            .env
            .clone()
            .with_open_files(Arc::new(FixedOpenFiles::default().with(held, "node")));
        let mut sink = |_: Event| {};
        let entry = clean(&env, std::slice::from_ref(&item), &mut sink);
        assert_eq!(entry.items[0].result, ItemResult::Skipped);
        assert_eq!(
            entry.items[0].reason.as_deref(),
            Some("Open in node, skipped")
        );
        assert!(item.path.join("a.bin").is_file());
        // A Large files move goes through the same check.
        let movie = f.env.at("Movies/trip.mov").canonicalize().unwrap();
        let env = f.env.clone().with_open_files(Arc::new(
            FixedOpenFiles::default().with(&movie, "QuickTime Player"),
        ));
        let entry = trash_paths(&env, &[f.env.at("Movies/trip.mov")], &mut sink);
        assert_eq!(
            entry.items[0].reason.as_deref(),
            Some("Open in QuickTime Player, skipped")
        );
        assert!(movie.is_file());
        // A file open somewhere else does not matter.
        let env = f.env.clone().with_open_files(Arc::new(
            FixedOpenFiles::default().with(f.base.join("elsewhere.txt"), "vim"),
        ));
        let entry = clean(&env, std::slice::from_ref(&item), &mut sink);
        assert_eq!(entry.items[0].result, ItemResult::Trashed);
    }

    #[test]
    fn unreadable_open_files_skip_every_move() {
        use crate::env::FixedOpenFiles;
        let f = fx();
        let item = npm_item(&f.env);
        put(&f.env.at("Movies/trip.mov"), 4096);
        let env = f
            .env
            .clone()
            .with_open_files(Arc::new(FixedOpenFiles::unreadable()));
        let mut sink = |_: Event| {};
        let entry = clean(&env, std::slice::from_ref(&item), &mut sink);
        assert_eq!(entry.items[0].reason.as_deref(), Some(OPEN_FILES_UNKNOWN));
        let entry = trash_paths(&env, &[f.env.at("Movies/trip.mov")], &mut sink);
        assert_eq!(entry.items[0].reason.as_deref(), Some(OPEN_FILES_UNKNOWN));
        assert!(item.path.is_dir() && f.env.at("Movies/trip.mov").is_file());
        assert!(!f.env.paths.dir_trash().exists());
    }

    #[test]
    fn an_unreadable_process_list_skips_the_clean() {
        let f = fx();
        let item = npm_item(&f.env);
        let env = f.env.clone().with_procs(Arc::new(crate::env::UnknownProbe));
        let mut sink = |_: Event| {};
        let entry = clean(&env, std::slice::from_ref(&item), &mut sink);
        assert_eq!(
            entry.items[0].reason.as_deref(),
            Some(crate::cleanup::PROCS_UNKNOWN)
        );
        assert!(item.path.is_dir());
    }

    #[test]
    fn the_ignore_list_on_disk_is_read_for_every_move() {
        let f = fx();
        let item = npm_item(&f.env);
        put(&f.env.at("Movies/keep/trip.mov"), 4096);
        put(&f.env.at("Movies/other.mov"), 4096);
        // Saved by an earlier run (or by the app after this scan); the Env in memory has an
        // empty list.
        let saved = crate::settings::Settings {
            ignored_paths: vec![item.path.clone(), f.env.at("Movies/keep")],
            ..Default::default()
        };
        saved.save(&f.env.paths).unwrap();
        assert!(f.env.settings.ignored_paths.is_empty());
        let mut sink = |_: Event| {};
        let entry = clean(&f.env, std::slice::from_ref(&item), &mut sink);
        assert_eq!(entry.items[0].reason.as_deref(), Some(ON_IGNORE_LIST));
        assert!(item.path.is_dir());
        // Large files: the ignored folder, a path inside it, and a folder holding it.
        let entry = trash_paths(
            &f.env,
            &[
                f.env.at("Movies/keep"),
                f.env.at("Movies/keep/trip.mov"),
                f.env.at("Movies"),
                f.env.at(".npm"),
            ],
            &mut sink,
        );
        assert!(
            entry
                .items
                .iter()
                .all(|i| i.reason.as_deref() == Some(ON_IGNORE_LIST)),
            "{:?}",
            entry.items
        );
        assert!(f.env.at("Movies/keep/trip.mov").is_file());
        // A path that is not on the list still moves.
        let entry = trash_paths(&f.env, &[f.env.at("Movies/other.mov")], &mut sink);
        assert_eq!(entry.items[0].result, ItemResult::Trashed);
    }

    #[test]
    fn an_unreadable_ignore_list_stops_the_run() {
        let f = fx();
        let item = npm_item(&f.env);
        put(&f.env.at("Movies/trip.mov"), 4096);
        std::fs::create_dir_all(&f.env.paths.data).unwrap();
        std::fs::write(f.env.paths.settings_file(), b"{ broken").unwrap();
        let mut events = Vec::new();
        let entry = clean(&f.env, &[item.clone(), item.clone()], &mut |e| {
            events.push(e)
        });
        assert!(
            entry
                .items
                .iter()
                .all(|i| i.reason.as_deref() == Some(IGNORE_LIST_UNREADABLE))
        );
        assert_eq!(entry.message.as_deref(), Some(IGNORE_LIST_STOPPED));
        assert_eq!(entry.trashed_bytes, 0);
        assert!(item.path.is_dir());
        let entry = trash_paths(&f.env, &[f.env.at("Movies/trip.mov")], &mut |_| {});
        assert_eq!(
            entry.items[0].reason.as_deref(),
            Some(IGNORE_LIST_UNREADABLE)
        );
        assert_eq!(entry.message.as_deref(), Some(IGNORE_LIST_STOPPED));
        assert!(f.env.at("Movies/trip.mov").is_file());
        assert!(!f.env.paths.dir_trash().exists());
        // The history says why.
        assert_eq!(
            history::read(&f.env.paths)
                .last()
                .unwrap()
                .message
                .as_deref(),
            Some(IGNORE_LIST_STOPPED)
        );
    }

    #[test]
    fn copy_has_no_em_dashes() {
        for s in [
            EMPTY_TRASH_REFUSED,
            NOT_IN_PLAN,
            STOPPED,
            GONE,
            NOT_IN_TRASH,
            NOT_TIDYS,
            ON_IGNORE_LIST,
            IGNORE_LIST_UNREADABLE,
            IGNORE_LIST_STOPPED,
            OPEN_FILES_UNKNOWN,
        ] {
            assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'));
        }
    }
}
