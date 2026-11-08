//! Moving things to the Trash: the [`Trasher`] seam, the macOS Trash ([`SystemTrash`]), the
//! stand-in folder Trash for tests and `TIDY_HOME` runs ([`DirTrash`]), and the guard
//! ([`check`]) that keeps every move strictly inside the scan root and off the denylist.
//!
//! Safety by construction (design section 8.4): the only code that builds a `SystemTrash` for
//! real use is `Env::from_env` when `TIDY_HOME` is unset, and `SystemTrash::trash` itself
//! refuses whenever `TIDY_HOME` is set in the process environment, as a second guard.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::model::TrasherKind;
use crate::paths::{HOME_ENV, holds_denied_rel, is_denied_name, is_denied_rel};

/// Moves one file or folder to a place the user can get it back from.
pub trait Trasher: Send + Sync {
    /// Moves `path` and returns where it ended up in the Trash, when the trasher knows.
    fn trash(&self, path: &Path) -> io::Result<Option<PathBuf>>;

    /// Which Trash this is, for the history entry's `trasher` field. Test doubles keep the
    /// default, `Dir`; only [`SystemTrash`] says `System`.
    fn kind(&self) -> TrasherKind {
        TrasherKind::Dir
    }
}

/// The macOS Trash, through `NSFileManager trashItemAtURL:resultingItemURL:error:`: the same
/// move Finder makes, so Finder's Put Back works. The resulting URL (which Haul discards) is
/// kept, because Empty Trash later deletes exactly those paths (C8).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTrash;

/// The error [`SystemTrash`] returns when `tidy_home` (the value of `TIDY_HOME`) is set and
/// not empty. None means the real Trash may be used.
pub fn system_trash_refusal(tidy_home: Option<&OsStr>) -> Option<io::Error> {
    tidy_home.filter(|v| !v.is_empty()).map(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to use the macOS Trash while TIDY_HOME is set",
        )
    })
}

impl Trasher for SystemTrash {
    fn trash(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        if let Some(e) = system_trash_refusal(std::env::var_os(HOME_ENV).as_deref()) {
            return Err(e);
        }
        use objc2_foundation::{NSFileManager, NSURL};
        objc2::rc::autoreleasepool(|_| {
            let url = NSURL::from_file_path(path).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("not a file path: {}", path.display()),
                )
            })?;
            let mut out: Option<objc2::rc::Retained<NSURL>> = None;
            NSFileManager::defaultManager()
                .trashItemAtURL_resultingItemURL_error(&url, Some(&mut out))
                .map_err(|e| {
                    // NSCocoaErrorDomain 513 (NSFileWriteNoPermissionError) and POSIX EPERM
                    // both mean macOS did not allow it.
                    let code = e.code();
                    let message = e.localizedDescription().to_string();
                    if code == 513 || code == libc::EPERM as isize || code == libc::EACCES as isize
                    {
                        io::Error::new(io::ErrorKind::PermissionDenied, message)
                    } else if code == 4 {
                        // NSFileNoSuchFileError.
                        io::Error::new(io::ErrorKind::NotFound, message)
                    } else {
                        io::Error::other(message)
                    }
                })?;
            Ok(out.and_then(|u| u.to_file_path()))
        })
    }

    fn kind(&self) -> TrasherKind {
        TrasherKind::System
    }
}

// ---------- Finder ----------

/// What an app row says when the user canceled Finder's password prompt.
pub const FINDER_CANCELED: &str = "You canceled the password prompt, so the app stayed.";
/// What an app row says when macOS does not let Tidy control Finder (Automation is off).
pub const FINDER_NOT_ALLOWED: &str = "Tidy isn't allowed to control Finder. Turn it on in System Settings > Privacy & Security > Automation.";
/// What an app row says for any other Finder failure.
pub const FINDER_FAILED: &str = "Finder couldn't move the app. Drag it to the Trash in Finder.";
/// The error of the stand-in that tests and `TIDY_HOME` runs get in Finder's place.
pub const FINDER_OFF: &str = "Finder isn't used under TIDY_HOME or in tests";

/// The script [`FinderTrash`] runs with `osascript`, one `-e` per line. The path is the first
/// argument, never spliced into the script. Finder's `delete` moves the item to the Trash
/// (it never deletes for good) and asks for an administrator's password itself when the
/// item needs one.
pub const FINDER_SCRIPT: [&str; 10] = [
    "on run argv",
    "tell application \"Finder\"",
    "set moved to delete (POSIX file (item 1 of argv) as alias)",
    "try",
    "return POSIX path of (moved as alias)",
    "on error",
    "return \"\"",
    "end try",
    "end tell",
    "end run",
];

/// Finder, asked through Apple Events to move an app bundle to the Trash (design 19.4, C51).
/// Used only for an app bundle that Tidy's user cannot rename, or whose move macOS refused:
/// Finder shows its own password prompt, Tidy never sees the password, and Finder's Put Back
/// works. The first use asks the user once whether Tidy may control Finder.
///
/// Like [`SystemTrash`], it refuses whenever `TIDY_HOME` is set, and in this crate's tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct FinderTrash;

impl Trasher for FinderTrash {
    fn trash(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        if cfg!(test) {
            return Err(io::Error::other(FINDER_OFF));
        }
        if let Some(e) = system_trash_refusal(std::env::var_os(HOME_ENV).as_deref()) {
            return Err(e);
        }
        let mut cmd = std::process::Command::new("/usr/bin/osascript");
        for line in FINDER_SCRIPT {
            cmd.arg("-e").arg(line);
        }
        let out = cmd.arg(path).output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(path = %path.display(), "Finder did not move it: {}", stderr.trim());
            return Err(io::Error::other(finder_error(&stderr)));
        }
        let moved = String::from_utf8_lossy(&out.stdout);
        let moved = moved.trim().trim_end_matches('/');
        Ok((!moved.is_empty()).then(|| PathBuf::from(moved)))
    }

    fn kind(&self) -> TrasherKind {
        TrasherKind::System
    }
}

/// The sentence for an `osascript` failure, from the AppleScript error number at the end of
/// its message: -128 is a canceled prompt, -1743 is Automation turned off for Tidy.
pub fn finder_error(stderr: &str) -> &'static str {
    if stderr.contains("(-128)") {
        FINDER_CANCELED
    } else if stderr.contains("(-1743)") {
        FINDER_NOT_ALLOWED
    } else {
        FINDER_FAILED
    }
}

/// What tests and `TIDY_HOME` runs get in Finder's place: every move fails with [`FINDER_OFF`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFinder;

impl Trasher for NoFinder {
    fn trash(&self, _path: &Path) -> io::Result<Option<PathBuf>> {
        Err(io::Error::other(FINDER_OFF))
    }
}

/// A stand-in Trash for tests and `TIDY_HOME` runs (`$TIDY_HOME/data/trash`). It renames
/// each item into its folder, adding " 2", " 3" and so on when the name is taken, and records
/// every move. It never touches the macOS Trash, and it never copies: a move across volumes
/// fails instead.
#[derive(Debug)]
pub struct DirTrash {
    dir: PathBuf,
    moves: Mutex<Vec<(PathBuf, PathBuf)>>,
}

impl DirTrash {
    pub fn new(dir: impl Into<PathBuf>) -> DirTrash {
        DirTrash {
            dir: dir.into(),
            moves: Mutex::new(Vec::new()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The original paths moved so far, in order.
    pub fn moved(&self) -> Vec<PathBuf> {
        self.moves().into_iter().map(|(from, _)| from).collect()
    }

    /// Every move so far as (original path, path in the Trash folder), in order.
    pub fn moves(&self) -> Vec<(PathBuf, PathBuf)> {
        self.moves.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Trasher for DirTrash {
    fn trash(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("no file name: {}", path.display()),
            )
        })?;
        // Nothing to move: report it the way the macOS Trash does.
        path.symlink_metadata()?;
        std::fs::create_dir_all(&self.dir)?;
        let mut moves = self.moves.lock().unwrap_or_else(|e| e.into_inner());
        let mut dest = self.dir.join(name);
        let mut n = 2;
        while dest.symlink_metadata().is_ok() {
            dest = self.dir.join(format!("{} {n}", name.to_string_lossy()));
            n += 1;
        }
        std::fs::rename(path, &dest)?;
        moves.push((path.to_path_buf(), dest.clone()));
        Ok(Some(dest))
    }
}

/// Why [`check`] refused a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A relative path would depend on the working folder.
    Relative,
    /// The scan root itself, by any spelling.
    Root,
    /// Outside the scan root after resolving `..` and symlinks.
    Outside,
    /// The path, its parent or its target could not be resolved (a dangling link, a folder
    /// that cannot be read).
    Unresolvable,
    /// On the hard denylist, or one of Tidy's own folders.
    Denied,
    /// A folder between the scan root and the path is a symlink, so the path names something
    /// other than what a scan (which never follows links) found there.
    Linked,
    /// A folder that holds something on the denylist (`Library` holds `Library/Keychains`), or
    /// a file or folder with a denied name (`Cookies`, `*.sqlite`) somewhere inside it.
    HoldsDenied,
    /// A folder inside could not be read, so Tidy cannot tell what moving it would take along.
    Unreadable,
}

impl Refusal {
    /// The plain reason for a skipped item.
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::Relative => "Not a full path",
            Refusal::Root => "That is the whole home folder",
            Refusal::Outside => "Outside the home folder",
            Refusal::Unresolvable => "Couldn't check where this points",
            Refusal::Denied => "Tidy never touches this folder",
            Refusal::Linked => "Now a link, skipped",
            Refusal::HoldsDenied => "Holds files Tidy never touches",
            Refusal::Unreadable => "macOS didn't allow access",
        }
    }
}

/// What [`check`] found for one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checked {
    /// Strictly inside the root and not denied. The path to hand to the trasher, with its
    /// parent folders resolved (a symlink itself is moved as a link, never followed).
    Inside(PathBuf),
    /// Nothing is there.
    Missing,
    Refused(Refusal),
}

/// Checks that `path` lies strictly inside `root` after resolving symlinks and `..`, both for
/// where it sits and for what it points to, and that neither is on the denylist (relative to
/// the resolved root). The root itself is never inside.
///
/// Three more refusals keep a move to exactly what a scan could have offered:
///
/// - Every folder between `root` and `path` must be a real folder. A symlink there (even one
///   that points somewhere inside the root) is refused with [`Refusal::Linked`], because the
///   scan never follows links and the move would take the link's target instead.
/// - A folder that holds a denylisted path or one of Tidy's own folders (`Library` holds
///   `Library/Keychains`) is refused with [`Refusal::HoldsDenied`].
/// - A folder with a denied name anywhere inside it (`Login Data`, `Cookies`, `*.sqlite`) is
///   refused the same way. The search never follows links; a folder inside that cannot be
///   read gives [`Refusal::Unreadable`].
pub fn check(root: &Path, path: &Path) -> Checked {
    if !path.is_absolute() {
        return Checked::Refused(Refusal::Relative);
    }
    match path.symlink_metadata() {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Checked::Missing,
        Err(_) => return Checked::Refused(Refusal::Unresolvable),
    }
    let given_root = root;
    let Ok(root) = root.canonicalize() else {
        return Checked::Refused(Refusal::Unresolvable);
    };
    // Fails for a symlink whose target is gone.
    let Ok(target) = path.canonicalize() else {
        return Checked::Refused(Refusal::Unresolvable);
    };
    if target == root {
        return Checked::Refused(Refusal::Root);
    }
    // file_name() is None for `/` and for a path that ends in "..", which resolved to
    // something other than the root above.
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Checked::Refused(Refusal::Outside);
    };
    let Ok(parent) = parent.canonicalize() else {
        return Checked::Refused(Refusal::Unresolvable);
    };
    let at = parent.join(name);
    if at == root {
        return Checked::Refused(Refusal::Root);
    }
    let (Ok(at_rel), Ok(target_rel)) = (at.strip_prefix(&root), target.strip_prefix(&root)) else {
        return Checked::Refused(Refusal::Outside);
    };
    if is_denied_rel(at_rel) || is_denied_rel(target_rel) {
        return Checked::Refused(Refusal::Denied);
    }
    if !plain_ancestors(given_root, &root, path) {
        return Checked::Refused(Refusal::Linked);
    }
    if holds_denied_rel(at_rel) {
        return Checked::Refused(Refusal::HoldsDenied);
    }
    match at.symlink_metadata() {
        Ok(m) if m.is_dir() => match find_denied_name(&at) {
            Ok(false) => {}
            Ok(true) => return Checked::Refused(Refusal::HoldsDenied),
            Err(_) => return Checked::Refused(Refusal::Unreadable),
        },
        Ok(_) => {}
        Err(_) => return Checked::Refused(Refusal::Unresolvable),
    }
    Checked::Inside(at)
}

/// Whether every folder between the root and `path` (not counting the root and `path`
/// itself) is a real folder and not a symlink. `given_root` is the root as the caller spelled
/// it and `root` its resolved form; a path that reaches neither is not plain.
fn plain_ancestors(given_root: &Path, root: &Path, path: &Path) -> bool {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == given_root || d == root {
            return true;
        }
        match d.symlink_metadata() {
            Ok(m) if m.is_dir() => {}
            _ => return false,
        }
        // Another spelling of the root (`/tmp/x` for `/private/tmp/x`), reached without
        // passing through a link below it.
        if d.canonicalize().is_ok_and(|c| c == root) {
            return true;
        }
        dir = d.parent();
    }
    false
}

/// Whether the folder `dir` holds an entry with a denied name anywhere inside, the check
/// [`check`] makes before a move. A file or a link is judged by its own name. Never follows a
/// symlink; an error means a folder inside cannot be read.
pub fn holds_denied_name(path: &Path) -> io::Result<bool> {
    match path.symlink_metadata() {
        Ok(m) if m.is_dir() => find_denied_name(path),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Searches the folder `dir` for an entry with a denied name, never following a symlink.
/// Returns an error when a folder inside cannot be read.
fn find_denied_name(dir: &Path) -> io::Result<bool> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            if is_denied_name(&entry.file_name()) {
                return Ok(true);
            }
            // DirEntry::file_type does not follow symlinks.
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(false)
}

/// The plain reason for a trasher error, for a skipped row: "macOS didn't allow access" for
/// EPERM and EACCES (rule 6.2.6), the system message otherwise.
pub fn error_reason(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied
        || matches!(e.raw_os_error(), Some(libc::EPERM | libc::EACCES))
    {
        "macOS didn't allow access".to_string()
    } else {
        e.to_string()
    }
}

/// The outcome of [`trash_paths`].
#[derive(Debug, Default)]
pub struct TrashReport {
    /// (original path, where it went in the Trash when known).
    pub trashed: Vec<(PathBuf, Option<PathBuf>)>,
    /// Already gone. Not an error.
    pub missing: Vec<PathBuf>,
    /// Left alone by the guard.
    pub refused: Vec<(PathBuf, Refusal)>,
    /// The trasher could not move them, with the plain reason.
    pub failed: Vec<(PathBuf, String)>,
}

impl TrashReport {
    /// Every path is in the Trash or was already gone.
    pub fn ok(&self) -> bool {
        self.refused.is_empty() && self.failed.is_empty()
    }
}

/// Moves `paths` to the Trash, each one only if [`check`] puts it inside `root`. A folder
/// takes everything in it, so paths inside a folder that is also listed are not moved twice.
pub fn trash_paths(trasher: &dyn Trasher, root: &Path, paths: &[PathBuf]) -> TrashReport {
    let mut report = TrashReport::default();
    let mut inside: Vec<PathBuf> = Vec::new();
    for p in paths {
        match check(root, p) {
            Checked::Inside(at) => inside.push(at),
            Checked::Missing => report.missing.push(p.clone()),
            Checked::Refused(why) => {
                tracing::warn!(path = %p.display(), root = %root.display(), ?why, "not moving to the Trash");
                report.refused.push((p.clone(), why));
            }
        }
    }
    inside.sort();
    inside.dedup();
    let outer: Vec<PathBuf> = inside
        .iter()
        .filter(|p| !inside.iter().any(|q| q != *p && p.starts_with(q)))
        .cloned()
        .collect();
    for p in outer {
        match trasher.trash(&p) {
            Ok(dest) => report.trashed.push((p, dest)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => report.missing.push(p),
            Err(e) => {
                tracing::warn!(path = %p.display(), "could not move to the Trash: {e}");
                report.failed.push((p, error_reason(&e)));
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {

    #[test]
    fn finder_errors_become_sentences() {
        assert_eq!(
            finder_error("execution error: Finder got an error: User canceled. (-128)"),
            FINDER_CANCELED
        );
        assert_eq!(
            finder_error("execution error: Not authorized to send Apple events to Finder. (-1743)"),
            FINDER_NOT_ALLOWED
        );
        assert_eq!(finder_error("something else (-10000)"), FINDER_FAILED);
    }

    #[test]
    fn finder_is_never_asked_from_tests() {
        let dir = tempfile::tempdir().unwrap();
        let err = FinderTrash.trash(dir.path()).unwrap_err();
        assert_eq!(err.to_string(), FINDER_OFF);
        assert!(dir.path().exists());
    }

    #[test]
    fn the_finder_script_takes_the_path_as_an_argument() {
        assert!(FINDER_SCRIPT.iter().all(|l| !l.contains('/')));
        assert!(FINDER_SCRIPT.iter().any(|l| l.contains("item 1 of argv")));
        assert!(FINDER_SCRIPT.iter().any(|l| l.contains("delete")));
    }

    use super::*;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    /// A temp folder with `home` (the scan root), `out` (a sibling outside it) and `trash`
    /// (the stand-in Trash), all canonical.
    struct Dirs {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        out: PathBuf,
        trash: DirTrash,
    }

    fn dirs() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let out = root.join("out");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        Dirs {
            _tmp: tmp,
            home,
            out,
            trash: DirTrash::new(root.join("trash")),
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    struct Failing(io::ErrorKind);
    impl Trasher for Failing {
        fn trash(&self, _: &Path) -> io::Result<Option<PathBuf>> {
            Err(io::Error::new(self.0, "no"))
        }
    }

    #[test]
    fn check_accepts_only_paths_strictly_inside() {
        let d = dirs();
        let h = &d.home;
        touch(&h.join("a.bin"));
        touch(&h.join("sub/b.bin"));
        touch(&d.out.join("secret.txt"));
        assert_eq!(check(h, &h.join("a.bin")), Checked::Inside(h.join("a.bin")));
        assert_eq!(check(h, &h.join("sub")), Checked::Inside(h.join("sub")));
        assert_eq!(
            check(h, &h.join("sub/../a.bin")),
            Checked::Inside(h.join("a.bin"))
        );
        // The root itself, by any spelling.
        assert_eq!(check(h, h), Checked::Refused(Refusal::Root));
        assert_eq!(check(h, &h.join("sub/..")), Checked::Refused(Refusal::Root));
        assert_eq!(check(h, &h.join(".")), Checked::Refused(Refusal::Root));
        // `..` out of the root.
        assert_eq!(
            check(h, &h.join("../out/secret.txt")),
            Checked::Refused(Refusal::Outside)
        );
        assert!(matches!(check(h, &h.join("..")), Checked::Refused(_)));
        assert_eq!(
            check(h, &d.out.join("secret.txt")),
            Checked::Refused(Refusal::Outside)
        );
        assert_eq!(check(h, Path::new("/")), Checked::Refused(Refusal::Outside));
        // Missing is not refused.
        assert_eq!(check(h, &h.join("gone.bin")), Checked::Missing);
        // A relative path cannot be checked.
        assert_eq!(
            check(h, Path::new("a.bin")),
            Checked::Refused(Refusal::Relative)
        );
    }

    #[test]
    fn check_refuses_symlink_escapes() {
        let d = dirs();
        let h = &d.home;
        touch(&d.out.join("secret.txt"));
        touch(&h.join("real.bin"));
        std::os::unix::fs::symlink(d.out.join("secret.txt"), h.join("link.bin")).unwrap();
        std::os::unix::fs::symlink(&d.out, h.join("linkdir")).unwrap();
        std::os::unix::fs::symlink(h.join("nowhere"), h.join("dangling.bin")).unwrap();
        std::os::unix::fs::symlink(h.join("real.bin"), h.join("inner.bin")).unwrap();
        std::os::unix::fs::symlink(h, h.join("self")).unwrap();
        // A link to a file outside.
        assert_eq!(
            check(h, &h.join("link.bin")),
            Checked::Refused(Refusal::Outside)
        );
        // A path through a linked folder that leads outside.
        assert_eq!(
            check(h, &h.join("linkdir/secret.txt")),
            Checked::Refused(Refusal::Outside)
        );
        assert_eq!(
            check(h, &h.join("linkdir")),
            Checked::Refused(Refusal::Outside)
        );
        // A link to the root itself.
        assert_eq!(check(h, &h.join("self")), Checked::Refused(Refusal::Root));
        // A link whose target cannot be resolved.
        assert_eq!(
            check(h, &h.join("dangling.bin")),
            Checked::Refused(Refusal::Unresolvable)
        );
        // A link inside to a file inside: the link itself is what moves.
        assert_eq!(
            check(h, &h.join("inner.bin")),
            Checked::Inside(h.join("inner.bin"))
        );
        // A root reached through a symlink still works.
        let alias = d.out.join("home-alias");
        std::os::unix::fs::symlink(h, &alias).unwrap();
        assert_eq!(
            check(&alias, &alias.join("real.bin")),
            Checked::Inside(h.join("real.bin"))
        );
    }

    #[test]
    fn check_refuses_the_denylist() {
        let d = dirs();
        let h = &d.home;
        touch(&h.join("Library/Containers/com.x/Data/big.bin"));
        touch(&h.join("Library/Caches/Tidy/last.json"));
        touch(&h.join("Library/Application Support/App/db.sqlite"));
        touch(&h.join("Library/Caches/com.example.Idle/a"));
        for rel in [
            "Library/Containers",
            "Library/Containers/com.x/Data/big.bin",
            "library/containers/com.x",
            "Library/Caches/Tidy",
            "Library/Application Support/App/db.sqlite",
        ] {
            assert_eq!(
                check(h, &h.join(rel)),
                Checked::Refused(Refusal::Denied),
                "{rel}"
            );
        }
        // A link from an allowed place into a denied one is refused by its target.
        std::os::unix::fs::symlink(h.join("Library/Containers/com.x"), h.join("Library/x"))
            .unwrap();
        assert_eq!(
            check(h, &h.join("Library/x")),
            Checked::Refused(Refusal::Denied)
        );
        assert_eq!(
            check(h, &h.join("Library/Caches/com.example.Idle")),
            Checked::Inside(h.join("Library/Caches/com.example.Idle"))
        );
    }

    #[test]
    fn check_refuses_a_link_between_the_root_and_the_path() {
        let d = dirs();
        let h = &d.home;
        touch(&h.join("Documents/decoy/com.example.idle/SENTINEL"));
        std::fs::create_dir_all(h.join("Library")).unwrap();
        // Library/Caches is a link to another folder inside the root.
        std::os::unix::fs::symlink(h.join("Documents/decoy"), h.join("Library/Caches")).unwrap();
        assert_eq!(
            check(h, &h.join("Library/Caches/com.example.idle")),
            Checked::Refused(Refusal::Linked)
        );
        assert_eq!(
            check(h, &h.join("Library/Caches/com.example.idle/SENTINEL")),
            Checked::Refused(Refusal::Linked)
        );
        // A link to the root itself in the middle.
        std::os::unix::fs::symlink(h, h.join("self")).unwrap();
        touch(&h.join("a.bin"));
        assert_eq!(
            check(h, &h.join("self/a.bin")),
            Checked::Refused(Refusal::Linked)
        );
        // The real path is still fine, and so is the root reached through another spelling.
        assert_eq!(
            check(h, &h.join("Documents/decoy/com.example.idle")),
            Checked::Inside(h.join("Documents/decoy/com.example.idle"))
        );
        let alias = d.out.join("home-alias");
        std::os::unix::fs::symlink(h, &alias).unwrap();
        assert_eq!(
            check(&alias, &alias.join("Documents/decoy")),
            Checked::Inside(h.join("Documents/decoy"))
        );
        let r = trash_paths(&d.trash, h, &[h.join("Library/Caches/com.example.idle")]);
        assert_eq!(
            r.refused,
            [(h.join("Library/Caches/com.example.idle"), Refusal::Linked)]
        );
        assert!(
            h.join("Documents/decoy/com.example.idle/SENTINEL")
                .is_file()
        );
    }

    #[test]
    fn check_refuses_a_folder_that_holds_denied_things() {
        let d = dirs();
        let h = &d.home;
        touch(&h.join("Library/Keychains/login.keychain-db"));
        touch(&h.join("Library/Mail/inbox"));
        touch(&h.join("Library/Caches/Tidy/last.json"));
        touch(&h.join("Library/Caches/com.example.Idle/a"));
        touch(&h.join("Library/Application Support/Google/Chrome/Default/Login Data"));
        touch(&h.join("Library/Application Support/Google/Chrome/Default/GPUCache/a"));
        touch(&h.join("Movies/project/index.SQLITE"));
        touch(&h.join("Movies/plain/a.mov"));
        // Folders above denylist entries and Tidy's own folders.
        for rel in [
            "Library",
            "library",
            "Library/Caches",
            "Library/Application Support",
        ] {
            assert_eq!(
                check(h, &h.join(rel)),
                Checked::Refused(Refusal::HoldsDenied),
                "{rel}"
            );
        }
        // Folders with a denied name somewhere inside.
        for rel in [
            "Library/Application Support/Google",
            "Library/Application Support/Google/Chrome/Default",
            "Movies/project",
            "Movies",
        ] {
            assert_eq!(
                check(h, &h.join(rel)),
                Checked::Refused(Refusal::HoldsDenied),
                "{rel}"
            );
        }
        // A link to such a folder moves only the link, so it is fine.
        std::os::unix::fs::symlink(h.join("Movies/project"), h.join("Movies/plain/link")).unwrap();
        assert_eq!(
            check(h, &h.join("Movies/plain")),
            Checked::Inside(h.join("Movies/plain"))
        );
        assert_eq!(
            check(
                h,
                &h.join("Library/Application Support/Google/Chrome/Default/GPUCache")
            ),
            Checked::Inside(h.join("Library/Application Support/Google/Chrome/Default/GPUCache"))
        );
        assert_eq!(
            check(h, &h.join("Library/Caches/com.example.Idle")),
            Checked::Inside(h.join("Library/Caches/com.example.Idle"))
        );
        // Nothing moved for the refused ones.
        let r = trash_paths(&d.trash, h, &[h.join("Library")]);
        assert_eq!(r.refused, [(h.join("Library"), Refusal::HoldsDenied)]);
        assert!(h.join("Library/Keychains/login.keychain-db").is_file());
        assert!(h.join("Library/Mail/inbox").is_file());
    }

    #[test]
    fn check_refuses_a_folder_it_cannot_search() {
        use std::os::unix::fs::PermissionsExt;
        let d = dirs();
        let h = &d.home;
        touch(&h.join("Movies/box/locked/a.mov"));
        let locked = h.join("Movies/box/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let got = check(h, &h.join("Movies/box"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(got, Checked::Refused(Refusal::Unreadable));
    }

    #[test]
    fn dir_trash_moves_records_and_keeps_both_names() {
        let d = dirs();
        touch(&d.home.join("a/x.log"));
        touch(&d.home.join("b/x.log"));
        let first = d.trash.trash(&d.home.join("a/x.log")).unwrap();
        let second = d.trash.trash(&d.home.join("b/x.log")).unwrap();
        assert_eq!(first, Some(d.trash.dir().join("x.log")));
        assert_eq!(second, Some(d.trash.dir().join("x.log 2")));
        assert_eq!(names(d.trash.dir()), ["x.log", "x.log 2"]);
        assert_eq!(
            d.trash.moved(),
            [d.home.join("a/x.log"), d.home.join("b/x.log")]
        );
        assert_eq!(d.trash.moves()[1].1, d.trash.dir().join("x.log 2"));
        assert!(!d.home.join("a/x.log").exists());
        assert_eq!(d.trash.kind(), TrasherKind::Dir);
    }

    #[test]
    fn dir_trash_reports_missing_as_not_found() {
        let d = dirs();
        let err = d.trash.trash(&d.home.join("gone")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(d.trash.moved().is_empty());
        assert!(d.trash.trash(Path::new("/")).is_err());
    }

    #[test]
    fn trash_paths_moves_inside_and_refuses_the_rest() {
        let d = dirs();
        let h = &d.home;
        touch(&h.join("f/a.bin"));
        touch(&h.join("g.bin"));
        touch(&d.out.join("secret.txt"));
        touch(&h.join(".ssh/id"));
        let r = trash_paths(
            &d.trash,
            h,
            &[
                h.join("f/a.bin"),
                h.join("f"),
                h.join("f/"),
                h.join("g.bin"),
                h.join("gone"),
                d.out.join("secret.txt"),
                h.join(".ssh/id"),
            ],
        );
        assert!(!r.ok());
        let moved: Vec<&PathBuf> = r.trashed.iter().map(|(p, _)| p).collect();
        assert_eq!(moved, [&h.join("f"), &h.join("g.bin")]);
        assert!(r.trashed.iter().all(|(_, dest)| dest.is_some()));
        assert_eq!(r.missing, [h.join("gone")]);
        assert_eq!(
            r.refused,
            [
                (d.out.join("secret.txt"), Refusal::Outside),
                (h.join(".ssh/id"), Refusal::Denied)
            ]
        );
        assert!(d.out.join("secret.txt").is_file());
        assert!(h.join(".ssh/id").is_file());
        assert_eq!(names(d.trash.dir()), ["f", "g.bin"]);
    }

    #[test]
    fn trasher_failure_is_reported_and_files_stay() {
        let d = dirs();
        touch(&d.home.join("x.bin"));
        let r = trash_paths(
            &Failing(io::ErrorKind::PermissionDenied),
            &d.home,
            &[d.home.join("x.bin")],
        );
        assert!(!r.ok());
        assert_eq!(
            r.failed,
            [(
                d.home.join("x.bin"),
                "macOS didn't allow access".to_string()
            )]
        );
        assert!(d.home.join("x.bin").is_file());
        let r = trash_paths(
            &Failing(io::ErrorKind::NotFound),
            &d.home,
            &[d.home.join("x.bin")],
        );
        assert!(r.ok());
        assert_eq!(r.missing, [d.home.join("x.bin")]);
    }

    #[test]
    fn error_reasons_are_plain() {
        assert_eq!(
            error_reason(&io::Error::from_raw_os_error(libc::EPERM)),
            "macOS didn't allow access"
        );
        assert_eq!(
            error_reason(&io::Error::from_raw_os_error(libc::EACCES)),
            "macOS didn't allow access"
        );
        assert_eq!(error_reason(&io::Error::other("disk full")), "disk full");
        for r in [
            Refusal::Relative,
            Refusal::Root,
            Refusal::Outside,
            Refusal::Unresolvable,
            Refusal::Denied,
            Refusal::Linked,
            Refusal::HoldsDenied,
            Refusal::Unreadable,
        ] {
            assert!(!r.reason().is_empty() && !r.reason().contains('\u{2014}'));
        }
    }

    #[test]
    fn system_trash_refusal_follows_tidy_home() {
        assert!(system_trash_refusal(None).is_none());
        assert!(system_trash_refusal(Some(OsStr::new(""))).is_none());
        let e = system_trash_refusal(Some(OsStr::new("/tmp/th"))).unwrap();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(e.to_string().contains("TIDY_HOME"));
    }

    /// Runs only as the child of `system_trash_refuses_under_tidy_home`, which starts it with
    /// TIDY_HOME set. The path does not exist, so even a broken guard could not move anything.
    #[test]
    fn system_trash_child() {
        if std::env::var_os("TIDY_TRASH_TEST_CHILD").is_none() {
            return;
        }
        let dir = std::env::var_os("TIDY_TRASH_TEST_CHILD").unwrap();
        let path = Path::new(&dir).join("does-not-exist");
        let err = SystemTrash.trash(&path).unwrap_err();
        assert!(err.to_string().contains("TIDY_HOME"), "{err}");
        assert_eq!(SystemTrash.kind(), TrasherKind::System);
        println!("REFUSED");
    }

    /// The second guard, end to end: a process with TIDY_HOME set gets a refusal from
    /// `SystemTrash::trash` before NSFileManager is ever called.
    #[test]
    fn system_trash_refuses_under_tidy_home() {
        let tmp = tempfile::tempdir().unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "trash::tests::system_trash_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TIDY_HOME", tmp.path())
            .env("TIDY_TRASH_TEST_CHILD", tmp.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        assert!(stdout.contains("REFUSED"), "{stdout}");
    }

    /// The real macOS Trash, on a temp file this test creates. Opt-in only: it runs when
    /// TIDY_TEST_REAL_TRASH=1 and TIDY_HOME is unset, which no script or agent sets. It puts
    /// back nothing else and deletes only the exact path the Trash returned.
    #[test]
    fn system_trash_real_opt_in() {
        if std::env::var("TIDY_TEST_REAL_TRASH").as_deref() != Ok("1")
            || std::env::var_os(HOME_ENV).is_some_and(|v| !v.is_empty())
        {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("tidy-real-trash-test.txt");
        std::fs::write(&file, b"tidy").unwrap();
        let dest = SystemTrash.trash(&file).unwrap().expect("a resulting path");
        assert!(!file.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), b"tidy");
        std::fs::remove_file(&dest).unwrap();
    }
}
