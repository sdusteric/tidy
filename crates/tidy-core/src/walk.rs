//! Measuring folders: allocated bytes, logical bytes and file count.
//!
//! [`measure`] is the API every scan uses. It reads folders with `getattrlistbulk`, which
//! returns the names and sizes of many entries per system call, and descends with `openat`.
//! [`measure_reference`] is a plain `readdir` and `fstatat` walker that defines the
//! semantics; tests assert that both give identical totals on every tree they build.
//!
//! Neither walker passes a full path to a system call below the measured folder: both open
//! each folder relative to its parent. So both count trees whose full paths are longer than
//! `PATH_MAX` (1024 bytes), and agree on them. Code that later acts on such a path by its full
//! name (a move to the Trash, Reveal in Finder) gets `ENAMETOOLONG` and reports the failure.
//!
//! The semantics, which both walkers share:
//!
//! - Symlinks are never followed and count nothing, whether at the top or inside.
//! - The walk never crosses a volume boundary: a folder on another device than
//!   [`WalkOpts::device`] (by default the device of the measured path) is skipped whole.
//! - Only regular files count. Folders, symlinks, sockets and FIFOs add nothing.
//! - A file whose link count is above 1 counts once per (device, inode), across every measure
//!   that shares the same [`Inodes`] set.
//! - `alloc` is the space on disk (`st_blocks * 512` in the reference, `ATTR_FILE_ALLOCSIZE`
//!   in the bulk walker), `logical` is the file length (`st_size`, `ATTR_FILE_TOTALSIZE`).
//!   The two agree for plain and for compressed files. A file with a classic resource fork
//!   counts the fork too in the bulk walker; the reference sees only the data fork.
//! - With [`WalkOpts::deny_root`] set, denylisted paths relative to it are skipped whole.
//! - Folders that cannot be read are skipped quietly; their siblings still count.
//! - Dataless folders (cloud placeholders that macOS would download to list) are not entered.
//! - With [`WalkOpts::cancel`] set, the walk stops before the next folder and returns what it
//!   has so far.
//!
//! The bulk walker splits the work across a few threads, one top-level folder at a time, and
//! reports progress through [`WalkOpts::progress`].

use std::cell::Cell;
use std::collections::HashSet;
use std::ffi::{CString, OsStr, OsString};
use std::fmt;
use std::io;
use std::ops::{Add, AddAssign};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::paths::{DENIED_NAMES, DENYLIST, OWN_FOLDERS, is_denied};

/// The size of what a walk found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    /// Allocated bytes: what moving it to the Trash and emptying the Trash would free.
    pub alloc: u64,
    /// The sum of file lengths.
    pub logical: u64,
    /// Regular files counted (hard links once).
    pub files: u64,
}

impl Add for Size {
    type Output = Size;
    fn add(self, o: Size) -> Size {
        Size {
            alloc: self.alloc + o.alloc,
            logical: self.logical + o.logical,
            files: self.files + o.files,
        }
    }
}

impl AddAssign for Size {
    fn add_assign(&mut self, o: Size) {
        *self = *self + o;
    }
}

/// The (device, inode) pairs of multiply-linked files already counted. Share one through an
/// `Arc` across measures so a hard-linked pair in two places counts once overall.
#[derive(Debug, Default)]
pub struct Inodes(Mutex<HashSet<(u64, u64)>>);

impl Inodes {
    pub fn new() -> Inodes {
        Inodes::default()
    }

    /// Records a file. Returns true the first time a pair is seen.
    pub fn insert(&self, dev: u64, ino: u64) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((dev, ino))
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// How far a walk has got. `files` and `bytes` count every regular file seen so far (before
/// hard links are deduplicated), `current` is the top-level folder being walked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkProgress {
    pub files: u64,
    pub bytes: u64,
    pub current: Option<PathBuf>,
}

/// A progress callback. It is called from the walker threads, at most once per
/// [`PROGRESS_EVERY`] files per thread and once per top-level folder.
#[derive(Clone)]
pub struct ProgressFn(Arc<dyn Fn(&WalkProgress) + Send + Sync>);

impl ProgressFn {
    pub fn new(f: impl Fn(&WalkProgress) + Send + Sync + 'static) -> ProgressFn {
        ProgressFn(Arc::new(f))
    }

    pub fn call(&self, p: &WalkProgress) {
        (self.0)(p)
    }
}

impl fmt::Debug for ProgressFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProgressFn")
    }
}

/// How many files a walker thread sees between two progress calls.
pub const PROGRESS_EVERY: u64 = 4096;

/// Options for a walk. The default stays on the measured path's device, skips nothing by
/// name, dedupes hard links within this one call, cannot be cancelled and reports nothing.
#[derive(Clone, Debug, Default)]
pub struct WalkOpts {
    /// Stay on this device. None: the device of the path being measured.
    pub device: Option<u64>,
    /// Skip paths on the denylist relative to this root (the scan root).
    pub deny_root: Option<PathBuf>,
    /// A hard-link set shared across measures. None: a fresh set for this call.
    pub inodes: Option<Arc<Inodes>>,
    /// Stops the walk before the next folder once set.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Called as the bulk walker makes progress. The reference walker never calls it.
    pub progress: Option<ProgressFn>,
}

impl WalkOpts {
    /// Options that stay on `root`'s device and skip the denylist under `root`. Hard links
    /// are deduplicated within each call.
    pub fn for_root(root: &Path) -> WalkOpts {
        WalkOpts {
            device: device_of(root),
            deny_root: Some(root.to_path_buf()),
            ..WalkOpts::default()
        }
    }

    pub(crate) fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    }

    pub(crate) fn denied(&self, path: &Path) -> bool {
        self.deny_root
            .as_deref()
            .is_some_and(|r| is_denied(r, path))
    }

    /// The number of components of `path` below the deny root, or None when there is no deny
    /// root or `path` is not under it (then nothing below it can be denied).
    fn deny_depth(&self, path: &Path) -> Option<usize> {
        let rel = path.strip_prefix(self.deny_root.as_deref()?).ok()?;
        Some(
            rel.components()
                .filter(|c| matches!(c, Component::Normal(_)))
                .count(),
        )
    }
}

/// The device id of `path` itself (not following a symlink), or None when it cannot be read.
pub fn device_of(path: &Path) -> Option<u64> {
    path.symlink_metadata().ok().map(|m| m.dev())
}

/// The number of threads [`measure`] uses: up to 4.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(4))
        .unwrap_or(2)
}

/// Measures `path` (a file or a folder) with the bulk walker on [`default_threads`] threads.
/// See the module docs for the rules.
pub fn measure(path: &Path, opts: &WalkOpts) -> Size {
    measure_bulk(path, opts, default_threads())
}

/// [`measure`] with an explicit thread count. 1 walks on the calling thread only.
pub fn measure_bulk(path: &Path, opts: &WalkOpts, threads: usize) -> Size {
    let local;
    let inodes: &Inodes = match &opts.inodes {
        Some(shared) => shared,
        None => {
            local = Inodes::new();
            &local
        }
    };
    let mut total = Size::default();
    if opts.denied(path) {
        return total;
    }
    let Ok(meta) = path.symlink_metadata() else {
        return total;
    };
    let device = opts.device.unwrap_or(meta.dev());
    if meta.dev() != device {
        return total;
    }
    let ft = meta.file_type();
    if ft.is_file() {
        count(&meta, inodes, &mut total);
        return total;
    }
    if !ft.is_dir() || meta.st_flags() & SF_DATALESS != 0 || opts.cancelled() {
        return total;
    }
    let Ok(root_fd) = open_dir(None, path, path.as_os_str()) else {
        return total;
    };
    let tally = Arc::new(Tally::default());
    let meter = Meter::new(tally.clone(), opts.progress.clone());
    let mut walker = Walker::new(device, opts, &meter);
    let Ok((files, subdirs)) = walker.list(root_fd.as_raw_fd(), path) else {
        return total;
    };
    let mut counter = Count {
        inodes,
        size: Size::default(),
    };
    walker.visit_files(path, &files, &mut counter);
    let threads = threads.max(1).min(subdirs.len());
    if threads <= 1 {
        for unit in subdirs {
            meter.top(&path.join(&unit.name));
            walker.walk_children(Some(root_fd.as_fd()), path, vec![unit], &mut counter);
        }
        meter.flush();
        return counter.size;
    }
    drop(walker);
    meter.flush();
    total = counter.size;
    let units: Vec<Mutex<Option<Entry>>> =
        subdirs.into_iter().map(|e| Mutex::new(Some(e))).collect();
    let next = AtomicUsize::new(0);
    let root_borrow = root_fd.as_fd();
    let parts: Vec<Size> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (units, next, tally) = (&units, &next, tally.clone());
                s.spawn(move || {
                    let meter = Meter::new(tally, opts.progress.clone());
                    let mut walker = Walker::new(device, opts, &meter);
                    let mut counter = Count {
                        inodes,
                        size: Size::default(),
                    };
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(slot) = units.get(i) else { break };
                        let Some(unit) = slot.lock().unwrap_or_else(|e| e.into_inner()).take()
                        else {
                            continue;
                        };
                        meter.top(&path.join(&unit.name));
                        walker.walk_children(Some(root_borrow), path, vec![unit], &mut counter);
                    }
                    meter.flush();
                    counter.size
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_default())
            .collect()
    });
    for p in parts {
        total += p;
    }
    total
}

/// The reference walker. It defines what [`measure`] must return.
///
/// It lists folders with `readdir` and reads each entry with `fstatat`, both relative to an
/// open folder, so it shares no listing or parsing code with the bulk walker. Like the bulk
/// walker it never builds a full path for a system call: each folder is opened from the
/// measured folder one name at a time with `openat`, so trees whose full paths are longer
/// than `PATH_MAX` (1024 bytes) are counted in full by both.
pub fn measure_reference(path: &Path, opts: &WalkOpts) -> Size {
    let local;
    let inodes: &Inodes = match &opts.inodes {
        Some(shared) => shared,
        None => {
            local = Inodes::new();
            &local
        }
    };
    let mut total = Size::default();
    if opts.denied(path) {
        return total;
    }
    let Ok(meta) = path.symlink_metadata() else {
        return total;
    };
    let device = opts.device.unwrap_or(meta.dev());
    if meta.dev() != device {
        return total;
    }
    let ft = meta.file_type();
    if ft.is_file() {
        count(&meta, inodes, &mut total);
        return total;
    }
    if !ft.is_dir() || meta.st_flags() & SF_DATALESS != 0 {
        return total;
    }
    let Ok(top) = open_dir(None, path, path.as_os_str()) else {
        return total;
    };
    // Folders still to list, as names below `path`.
    let mut stack: Vec<Vec<OsString>> = vec![Vec::new()];
    while let Some(rel) = stack.pop() {
        if opts.cancelled() {
            break;
        }
        let Ok(fd) = open_below(&top, &rel) else {
            continue;
        };
        let mut dir = path.to_path_buf();
        dir.extend(&rel);
        let Ok(entries) = list_with_stat(fd) else {
            continue;
        };
        for (name, st) in entries {
            let kind = st.st_mode & libc::S_IFMT;
            if kind == libc::S_IFLNK {
                continue;
            }
            if opts.denied(&dir.join(&name)) {
                continue;
            }
            let dev = st.st_dev as u64;
            if kind == libc::S_IFDIR {
                if dev == device && st.st_flags & SF_DATALESS == 0 {
                    let mut child = rel.clone();
                    child.push(name);
                    stack.push(child);
                }
            } else if kind == libc::S_IFREG && dev == device {
                if st.st_nlink > 1 && !inodes.insert(dev, st.st_ino) {
                    continue;
                }
                total.alloc += st.st_blocks as u64 * 512;
                total.logical += st.st_size as u64;
                total.files += 1;
            }
        }
    }
    total
}

/// Opens the folder reached from `top` through the names in `rel`, one `openat` per name and
/// never through a symlink.
fn open_below(top: &OwnedFd, rel: &[OsString]) -> io::Result<OwnedFd> {
    let mut fd = open_dir(Some(top.as_raw_fd()), Path::new("."), OsStr::new("."))?;
    for name in rel {
        fd = open_dir(Some(fd.as_raw_fd()), Path::new(name), name)?;
    }
    Ok(fd)
}

/// Every entry of the open folder `fd` (except `.` and `..`) with its `fstatat` result, not
/// following symlinks. Entries that cannot be read are left out. Takes ownership of `fd`.
fn list_with_stat(fd: OwnedFd) -> io::Result<Vec<(OsString, libc::stat)>> {
    use std::os::fd::IntoRawFd;
    let raw = fd.into_raw_fd();
    // SAFETY: `raw` is an open folder descriptor that nothing else owns; on success the DIR
    // owns it and closedir closes it.
    let dirp = unsafe { libc::fdopendir(raw) };
    if dirp.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so `raw` is still ours to close.
        unsafe { libc::close(raw) };
        return Err(e);
    }
    let mut out = Vec::new();
    loop {
        // SAFETY: `dirp` is a valid DIR stream until closedir below.
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            break;
        }
        // SAFETY: readdir returned a valid entry that stays valid until the next call.
        let ent = unsafe { &*ent };
        let len = usize::from(ent.d_namlen).min(ent.d_name.len());
        // SAFETY: d_name holds d_namlen bytes of the name.
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ent.d_name.as_ptr().cast(), len) };
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = OsStr::from_bytes(bytes).to_os_string();
        let Ok(c) = CString::new(bytes) else { continue };
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `c` is NUL-terminated, `st` a valid out pointer, and the DIR's descriptor
        // is open.
        let rc = unsafe {
            libc::fstatat(
                libc::dirfd(dirp),
                c.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            continue;
        }
        // SAFETY: fstatat succeeded, so it filled `st`.
        out.push((name, unsafe { st.assume_init() }));
    }
    // SAFETY: `dirp` came from fdopendir and is closed once.
    unsafe { libc::closedir(dirp) };
    Ok(out)
}

/// Adds one regular file, once per (device, inode) when it has more than one link.
fn count(meta: &std::fs::Metadata, inodes: &Inodes, total: &mut Size) {
    if meta.nlink() > 1 && !inodes.insert(meta.dev(), meta.ino()) {
        return;
    }
    total.alloc += meta.blocks() * 512;
    total.logical += meta.size();
    total.files += 1;
}

// ---------- The bulk walker ----------

/// `st_flags` bit for a dataless (cloud placeholder) item, from `<sys/stat.h>`.
const SF_DATALESS: u32 = 0x4000_0000;
/// `ATTR_CMN_ERROR`, from `<sys/attr.h>` (not in the libc crate).
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
/// `fsobj_type_t` values (`enum vtype` in `<sys/vnode.h>`).
const VREG: u32 = 1;
const VDIR: u32 = 2;

/// The common attributes requested for every entry. With `FSOPT_PACK_INVAL_ATTRS` each is
/// always present, in this order: returned set, error, name, devid, objtype, modtime,
/// acctime, flags, fileid (the error comes right after the returned set, the rest in bit
/// order).
const COMMON_ATTRS: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_ERROR
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_ACCTIME
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID;
/// The file attributes: link count (u32), total size and allocated size (off_t each).
const FILE_ATTRS: u32 =
    libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_TOTALSIZE | libc::ATTR_FILE_ALLOCSIZE;

/// The bytes of the common part of one packed entry, after its 4-byte length: returned set,
/// error, name reference, devid, objtype, modtime, acctime, flags, fileid.
const COMMON_LEN: usize = 20 + 4 + 8 + 4 + 4 + 16 + 16 + 4 + 8;
/// The file part that follows for everything but folders: link count, total size, allocated
/// size. The kernel leaves it out for folders even with `FSOPT_PACK_INVAL_ATTRS`.
const FILE_LEN: usize = 4 + 8 + 8;

/// The buffer each walker hands to `getattrlistbulk`.
const BULK_BUF: usize = 64 * 1024;

/// Folders deeper than this in one walk are opened by path instead of keeping their parent's
/// file descriptor open, so a deep tree never runs into the 256 open files limit.
const KEEP_FD_DEPTH: usize = 16;

/// What kind of object an entry is. Symlinks never get this far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Dir,
    #[default]
    Other,
}

/// One folder entry as `getattrlistbulk` reported it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Entry {
    pub name: OsString,
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    pub alloc: u64,
    pub logical: u64,
    /// Unix seconds, 0 before the epoch.
    pub mtime: u64,
    pub atime: u64,
    pub dataless: bool,
}

impl Entry {
    /// The later of mtime and atime (Unix seconds), the fallback last-use date.
    pub fn file_time(&self) -> u64 {
        self.mtime.max(self.atime)
    }
}

/// What a walk calls back into. The walker guarantees that for every [`Visit::enter`] that
/// returns true there is exactly one later [`Visit::leave`] for the same path, with that
/// folder's files and subfolders visited in between.
pub(crate) trait Visit {
    /// A regular file on the walk's device in `dir`, not denylisted.
    fn file(&mut self, dir: &Path, e: &Entry);
    /// A subfolder at `path`. Return true to walk into it.
    fn enter(&mut self, path: &Path, e: &Entry) -> bool;
    /// The folder entered at `path` cannot be walked: `err` is why, or None when it is on
    /// another volume. [`Visit::leave`] follows at once.
    fn abandon(&mut self, path: &Path, err: Option<&io::Error>);
    /// Everything below `path` has been visited.
    fn leave(&mut self, path: &Path);
}

/// The visitor behind [`measure`]: sums files, hard links once.
struct Count<'a> {
    inodes: &'a Inodes,
    size: Size,
}

impl Visit for Count<'_> {
    fn file(&mut self, _dir: &Path, e: &Entry) {
        if e.nlink > 1 && !self.inodes.insert(e.dev, e.ino) {
            return;
        }
        self.size.alloc += e.alloc;
        self.size.logical += e.logical;
        self.size.files += 1;
    }

    fn enter(&mut self, _path: &Path, _e: &Entry) -> bool {
        true
    }

    fn abandon(&mut self, _path: &Path, _err: Option<&io::Error>) {}

    fn leave(&mut self, _path: &Path) {}
}

/// Progress shared by the threads of one walk.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    files: AtomicU64,
    bytes: AtomicU64,
}

/// One thread's view of the progress: it batches counts and calls the callback.
pub(crate) struct Meter {
    tally: Arc<Tally>,
    cb: Option<ProgressFn>,
    files: Cell<u64>,
    bytes: Cell<u64>,
    current: std::cell::RefCell<Option<PathBuf>>,
}

impl Meter {
    pub(crate) fn new(tally: Arc<Tally>, cb: Option<ProgressFn>) -> Meter {
        Meter {
            tally,
            cb,
            files: Cell::new(0),
            bytes: Cell::new(0),
            current: std::cell::RefCell::new(None),
        }
    }

    /// Counts one file seen.
    pub(crate) fn file(&self, bytes: u64) {
        if self.cb.is_none() {
            return;
        }
        self.files.set(self.files.get() + 1);
        self.bytes.set(self.bytes.get() + bytes);
        if self.files.get() >= PROGRESS_EVERY {
            self.flush();
        }
    }

    /// The walk moved on to the top-level folder `path`.
    pub(crate) fn top(&self, path: &Path) {
        if self.cb.is_none() {
            return;
        }
        *self.current.borrow_mut() = Some(path.to_path_buf());
        self.flush();
    }

    /// Adds the batched counts to the shared tally and reports.
    pub(crate) fn flush(&self) {
        let Some(cb) = &self.cb else { return };
        let added_files = self.files.take();
        let files = self.tally.files.fetch_add(added_files, Ordering::Relaxed) + added_files;
        let added = self.bytes.take();
        let bytes = self.tally.bytes.fetch_add(added, Ordering::Relaxed) + added;
        cb.call(&WalkProgress {
            files,
            bytes,
            current: self.current.borrow().clone(),
        });
    }
}

/// One folder on the walk's stack.
struct Frame {
    /// Keeps the folder open while its subfolders are opened relative to it.
    _owned: Option<OwnedFd>,
    raw: Option<RawFd>,
    path: PathBuf,
    subdirs: Vec<Entry>,
    next: usize,
}

/// The bulk walker for one thread: a read buffer and the walk's rules.
pub(crate) struct Walker<'a> {
    device: u64,
    opts: &'a WalkOpts,
    meter: &'a Meter,
    buf: Vec<u64>,
    /// Folders at most this deep below the deny root get the full denylist check; deeper
    /// ones only the name check, since no list entry is that long.
    full_check_depth: usize,
}

impl<'a> Walker<'a> {
    pub(crate) fn new(device: u64, opts: &'a WalkOpts, meter: &'a Meter) -> Walker<'a> {
        let full_check_depth = DENYLIST
            .iter()
            .chain(OWN_FOLDERS)
            .map(|e| e.split('/').count())
            .max()
            .unwrap_or(0);
        Walker {
            device,
            opts,
            meter,
            buf: vec![0u64; BULK_BUF / 8],
            full_check_depth,
        }
    }

    pub(crate) fn device(&self) -> u64 {
        self.device
    }

    /// Reads the folder open at `fd` (found at `dir`) and splits it into the regular files and
    /// the subfolders the walk may enter, each sorted by name. Symlinks, other object types,
    /// denylisted names, other devices and dataless folders are dropped here.
    pub(crate) fn list(&mut self, fd: RawFd, dir: &Path) -> io::Result<(Vec<Entry>, Vec<Entry>)> {
        let mut entries = read_entries(fd, &mut self.buf)?;
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        let depth = self.opts.deny_depth(dir);
        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for e in entries {
            if e.kind == Kind::Other || e.dev != self.device {
                continue;
            }
            if let Some(d) = depth {
                let denied = if d < self.full_check_depth {
                    self.opts.denied(&dir.join(&e.name))
                } else {
                    denied_name(&e.name)
                };
                if denied {
                    continue;
                }
            }
            match e.kind {
                Kind::File => files.push(e),
                Kind::Dir if !e.dataless => dirs.push(e),
                _ => {}
            }
        }
        Ok((files, dirs))
    }

    /// Hands `files` (from [`Walker::list`] of `dir`) to the visitor and the progress meter.
    pub(crate) fn visit_files(&self, dir: &Path, files: &[Entry], v: &mut dyn Visit) {
        for f in files {
            self.meter.file(f.alloc);
            v.file(dir, f);
        }
    }

    /// Walks `subdirs` of the folder `parent_path` (open at `parent`, or None to open the
    /// subfolders by path), depth first and in name order.
    pub(crate) fn walk_children(
        &mut self,
        parent: Option<BorrowedFd<'_>>,
        parent_path: &Path,
        subdirs: Vec<Entry>,
        v: &mut dyn Visit,
    ) {
        let mut stack = vec![Frame {
            _owned: None,
            raw: parent.map(|f| f.as_raw_fd()),
            path: parent_path.to_path_buf(),
            subdirs,
            next: 0,
        }];
        while let Some(top) = stack.last_mut() {
            if top.next == top.subdirs.len() {
                let done = stack.pop().expect("the stack has a top");
                if !stack.is_empty() {
                    v.leave(&done.path);
                }
                continue;
            }
            if self.opts.cancelled() {
                return;
            }
            let e = std::mem::take(&mut top.subdirs[top.next]);
            top.next += 1;
            let child = top.path.join(&e.name);
            let parent_raw = top.raw;
            if !v.enter(&child, &e) {
                continue;
            }
            let fd = match open_dir(parent_raw, &child, &e.name) {
                Ok(fd) => fd,
                Err(err) => {
                    v.abandon(&child, Some(&err));
                    v.leave(&child);
                    continue;
                }
            };
            if fd_device(&fd) != Some(self.device) {
                v.abandon(&child, None);
                v.leave(&child);
                continue;
            }
            let (files, subdirs) = match self.list(fd.as_raw_fd(), &child) {
                Ok(x) => x,
                Err(err) => {
                    v.abandon(&child, Some(&err));
                    v.leave(&child);
                    continue;
                }
            };
            self.visit_files(&child, &files, v);
            let (owned, raw) = if stack.len() < KEEP_FD_DEPTH {
                let raw = fd.as_raw_fd();
                (Some(fd), Some(raw))
            } else {
                (None, None)
            };
            stack.push(Frame {
                _owned: owned,
                raw,
                path: child,
                subdirs,
                next: 0,
            });
        }
    }
}

/// Whether one file name is denied wherever it appears (see `paths::DENIED_NAMES`).
fn denied_name(name: &OsStr) -> bool {
    let b = name.as_bytes();
    let sqlite = b".sqlite";
    (b.len() >= sqlite.len() && b[b.len() - sqlite.len()..].eq_ignore_ascii_case(sqlite))
        || DENIED_NAMES
            .iter()
            .any(|n| n.as_bytes().eq_ignore_ascii_case(b))
}

/// Opens a folder for listing without following a symlink: relative to `parent` when given,
/// otherwise by its full `path`.
pub(crate) fn open_dir(parent: Option<RawFd>, path: &Path, name: &OsStr) -> io::Result<OwnedFd> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = match parent {
        Some(dirfd) => {
            let c = CString::new(name.as_bytes()).map_err(io::Error::other)?;
            // SAFETY: `c` is a valid NUL-terminated string and `dirfd` an open descriptor
            // owned by a frame or caller that outlives this call.
            unsafe { libc::openat(dirfd, c.as_ptr(), flags) }
        }
        None => {
            let c = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
            // SAFETY: `c` is a valid NUL-terminated string.
            unsafe { libc::open(c.as_ptr(), flags) }
        }
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by open or openat and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The device id of an open descriptor, as `MetadataExt::dev` reports it.
pub(crate) fn fd_device(fd: &OwnedFd) -> Option<u64> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `st` is a valid out pointer for one `stat` and `fd` is open.
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: fstat succeeded, so it filled `st`.
    let st = unsafe { st.assume_init() };
    Some(st.st_dev as u64)
}

/// Reads every entry of the folder open at `fd` with `getattrlistbulk`. An error on the first
/// call is returned; an error after some entries were read ends the listing with what it has.
fn read_entries(fd: RawFd, buf: &mut [u64]) -> io::Result<Vec<Entry>> {
    let mut list = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: COMMON_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: FILE_ATTRS,
        forkattr: 0,
    };
    let byte_len = std::mem::size_of_val(buf);
    let mut out = Vec::new();
    let mut first = true;
    loop {
        // SAFETY: `list` is a valid attrlist, `buf` is writable for `byte_len` bytes and `fd`
        // is an open folder descriptor.
        let n = unsafe {
            libc::getattrlistbulk(
                fd,
                (&mut list as *mut libc::attrlist).cast(),
                buf.as_mut_ptr().cast(),
                byte_len,
                u64::from(libc::FSOPT_PACK_INVAL_ATTRS),
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return if first { Err(err) } else { Ok(out) };
        }
        first = false;
        if n == 0 {
            return Ok(out);
        }
        // SAFETY: the buffer is `byte_len` initialized bytes (a Vec<u64>), viewed as u8.
        let bytes: &[u8] =
            unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), byte_len) };
        let mut off = 0usize;
        for _ in 0..n {
            let Some(len) = read_u32(bytes, off) else {
                return Ok(out);
            };
            let len = len as usize;
            if len < 4 || off + len > bytes.len() {
                return Ok(out);
            }
            if let Some(e) = parse_entry(&bytes[off..off + len]) {
                out.push(e);
            }
            off += len;
        }
    }
}

fn read_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn read_i64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_ne_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn read_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Parses one packed entry (starting at its length field). Returns None for an entry with an
/// error, a symlink, or a malformed record.
fn parse_entry(e: &[u8]) -> Option<Entry> {
    if e.len() < 4 + COMMON_LEN {
        return None;
    }
    let mut p = 4;
    let returned_common = read_u32(e, p)?;
    let returned_file = read_u32(e, p + 12)?;
    p += 20;
    let error = read_u32(e, p)?;
    p += 4;
    if error != 0 {
        return None;
    }
    let name_ref = p;
    let name_off = read_i32(e, p)?;
    let name_len = read_u32(e, p + 4)? as usize;
    p += 8;
    let dev = read_i32(e, p)?;
    p += 4;
    let objtype = read_u32(e, p)?;
    p += 4;
    let mtime = read_i64(e, p)?;
    p += 16;
    let atime = read_i64(e, p)?;
    p += 16;
    let flags = read_u32(e, p)?;
    p += 4;
    let fileid = read_u64(e, p)?;
    p += 8;
    if returned_common & libc::ATTR_CMN_OBJTYPE == 0 || returned_common & libc::ATTR_CMN_NAME == 0 {
        return None;
    }
    let kind = match objtype {
        VREG => Kind::File,
        VDIR => Kind::Dir,
        // Symlinks (VLNK) are never followed and count nothing, like sockets and FIFOs.
        _ => Kind::Other,
    };
    let (mut nlink, mut logical, mut alloc) = (1, 0, 0);
    if kind == Kind::File {
        if e.len() < 4 + COMMON_LEN + FILE_LEN
            || returned_file & libc::ATTR_FILE_ALLOCSIZE == 0
            || returned_file & libc::ATTR_FILE_TOTALSIZE == 0
        {
            return None;
        }
        nlink = read_u32(e, p)?;
        logical = read_i64(e, p + 4)?.max(0) as u64;
        alloc = read_i64(e, p + 12)?.max(0) as u64;
    }
    let start = usize::try_from(name_ref as i64 + i64::from(name_off)).ok()?;
    let raw = e.get(start..start.checked_add(name_len)?)?;
    let raw = match raw.iter().position(|&c| c == 0) {
        Some(i) => &raw[..i],
        None => raw,
    };
    if raw.is_empty() {
        return None;
    }
    Some(Entry {
        name: OsStr::from_bytes(raw).to_os_string(),
        kind,
        // dev_t is an i32; widen it the way MetadataExt::dev does.
        dev: dev as u64,
        ino: fileid,
        nlink,
        alloc,
        logical,
        mtime: mtime.max(0) as u64,
        atime: atime.max(0) as u64,
        dataless: flags & SF_DATALESS != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    /// Writes `len` bytes of a deterministic non-zero pattern, so APFS allocates every block.
    fn write_pattern(path: &Path, len: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8 + 1).collect();
        std::fs::write(path, data).unwrap();
    }

    fn root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("home");
        std::fs::create_dir_all(&root).unwrap();
        (tmp, root)
    }

    fn opts(root: &Path) -> WalkOpts {
        WalkOpts::for_root(root)
    }

    /// Both walkers, on one thread and on several, must agree.
    fn both(path: &Path, o: &WalkOpts) -> Size {
        let r = measure_reference(path, o);
        assert_eq!(measure_bulk(path, o, 1), r, "single thread at {path:?}");
        assert_eq!(measure_bulk(path, o, 4), r, "four threads at {path:?}");
        r
    }

    #[test]
    fn counts_files_in_nested_folders_exactly() {
        let (_t, r) = root();
        write_pattern(&r.join("a/one.bin"), 4096);
        write_pattern(&r.join("a/b/two.bin"), 8192);
        write_pattern(&r.join("a/b/c/three.bin"), 3 * 4096);
        std::fs::create_dir_all(r.join("a/empty")).unwrap();
        let s = measure(&r.join("a"), &opts(&r));
        assert_eq!(s.files, 3);
        assert_eq!(s.logical, 6 * 4096);
        assert_eq!(
            s.alloc,
            6 * 4096,
            "plain writes of whole blocks allocate exactly"
        );
        // A single file.
        let f = measure(&r.join("a/b/two.bin"), &opts(&r));
        assert_eq!(
            f,
            Size {
                alloc: 8192,
                logical: 8192,
                files: 1
            }
        );
        // Missing and empty.
        assert_eq!(measure(&r.join("nope"), &opts(&r)), Size::default());
        assert_eq!(measure(&r.join("a/empty"), &opts(&r)), Size::default());
        assert_eq!(both(&r.join("a"), &opts(&r)), s);
    }

    #[test]
    fn symlinks_are_never_followed() {
        let (t, r) = root();
        let outside = t.path().canonicalize().unwrap().join("outside");
        write_pattern(&outside.join("big.bin"), 64 * 4096);
        write_pattern(&r.join("d/own.bin"), 4096);
        std::os::unix::fs::symlink(&outside, r.join("d/link-dir")).unwrap();
        std::os::unix::fs::symlink(outside.join("big.bin"), r.join("d/link-file")).unwrap();
        let s = both(&r.join("d"), &opts(&r));
        assert_eq!(
            s,
            Size {
                alloc: 4096,
                logical: 4096,
                files: 1
            }
        );
        // A symlink measured directly counts nothing either.
        assert_eq!(measure(&r.join("d/link-dir"), &opts(&r)), Size::default());
        assert_eq!(measure(&r.join("d/link-file"), &opts(&r)), Size::default());
    }

    #[test]
    fn hard_links_count_once_and_across_shared_measures() {
        let (_t, r) = root();
        write_pattern(&r.join("x/a.bin"), 4 * 4096);
        std::fs::create_dir_all(r.join("y")).unwrap();
        std::fs::hard_link(r.join("x/a.bin"), r.join("x/a-again.bin")).unwrap();
        std::fs::hard_link(r.join("x/a.bin"), r.join("y/a-elsewhere.bin")).unwrap();
        let whole = both(&r, &opts(&r));
        assert_eq!(
            whole,
            Size {
                alloc: 4 * 4096,
                logical: 4 * 4096,
                files: 1
            }
        );
        // Each call has its own set by default.
        assert_eq!(measure(&r.join("y"), &opts(&r)).files, 1);
        // A shared set counts the pair once over both calls.
        let shared = WalkOpts {
            inodes: Some(Arc::new(Inodes::new())),
            ..opts(&r)
        };
        let x = measure(&r.join("x"), &shared);
        let y = measure(&r.join("y"), &shared);
        assert_eq!(x.files + y.files, 1);
        assert_eq!(x.alloc + y.alloc, 4 * 4096);
        assert_eq!(shared.inodes.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn sparse_files_report_small_allocation() {
        let (_t, r) = root();
        let f = r.join("s/sparse.img");
        std::fs::create_dir_all(r.join("s")).unwrap();
        let mut file = std::fs::File::create(&f).unwrap();
        file.seek(SeekFrom::Start(64 * 1024 * 1024)).unwrap();
        file.write_all(&[7u8; 4096]).unwrap();
        drop(file);
        let s = measure(&f, &opts(&r));
        assert_eq!(s.logical, 64 * 1024 * 1024 + 4096);
        assert!(s.alloc < 1024 * 1024, "{s:?}");
        let d = both(&r.join("s"), &opts(&r));
        assert_eq!(d, s);
    }

    #[test]
    fn denylisted_paths_are_skipped_under_the_root() {
        let (_t, r) = root();
        write_pattern(&r.join("Library/Containers/com.x/Data/big.bin"), 16 * 4096);
        write_pattern(&r.join("Library/Caches/app/c.bin"), 4096);
        write_pattern(&r.join("Library/Caches/app/deep/x/y/z/Cookies"), 4096);
        write_pattern(&r.join("Library/Caches/app/deep/x/y/z/db.SQLite"), 4096);
        write_pattern(&r.join("Library/Application Support/App/db.sqlite"), 4096);
        let s = both(&r.join("Library"), &opts(&r));
        assert_eq!(
            s,
            Size {
                alloc: 4096,
                logical: 4096,
                files: 1
            }
        );
        assert_eq!(
            measure(&r.join("Library/Containers"), &opts(&r)),
            Size::default()
        );
        // Without a deny root everything counts.
        let all = both(&r.join("Library"), &WalkOpts::default());
        assert_eq!(all.files, 5);
    }

    #[test]
    fn another_device_is_not_entered() {
        let (_t, r) = root();
        write_pattern(&r.join("v/f.bin"), 4096);
        let dev = device_of(&r).unwrap();
        let other = WalkOpts {
            device: Some(dev.wrapping_add(1)),
            ..opts(&r)
        };
        assert_eq!(measure(&r.join("v"), &other), Size::default());
        assert_eq!(measure(&r.join("v/f.bin"), &other), Size::default());
        assert_eq!(measure(&r.join("v"), &opts(&r)).files, 1);
    }

    #[test]
    fn cancel_stops_the_walk() {
        let (_t, r) = root();
        write_pattern(&r.join("c/f.bin"), 4096);
        write_pattern(&r.join("c/sub/g.bin"), 4096);
        let cancel = Arc::new(AtomicBool::new(true));
        let o = WalkOpts {
            cancel: Some(cancel.clone()),
            ..opts(&r)
        };
        assert_eq!(measure(&r.join("c"), &o), Size::default());
        cancel.store(false, Ordering::Relaxed);
        assert_eq!(measure(&r.join("c"), &o).files, 2);
    }

    #[test]
    fn progress_reports_files_and_the_top_folder() {
        let (_t, r) = root();
        for i in 0..5 {
            write_pattern(&r.join(format!("top{i}/f.bin")), 4096);
        }
        let seen: Arc<Mutex<Vec<WalkProgress>>> = Arc::default();
        let sink = seen.clone();
        let o = WalkOpts {
            progress: Some(ProgressFn::new(move |p| {
                sink.lock().unwrap().push(p.clone())
            })),
            ..opts(&r)
        };
        assert_eq!(measure_bulk(&r, &o, 2).files, 5);
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty());
        assert_eq!(seen.iter().map(|p| p.files).max(), Some(5));
        assert_eq!(seen.iter().map(|p| p.bytes).max(), Some(5 * 4096));
        assert!(
            seen.iter()
                .any(|p| p.current.as_deref() == Some(&r.join("top3")))
        );
    }

    #[test]
    fn deep_trees_do_not_run_out_of_descriptors() {
        let (_t, r) = root();
        let mut deep = r.join("deep");
        for i in 0..(KEEP_FD_DEPTH * 3) {
            deep = deep.join(format!("d{i}"));
        }
        write_pattern(&deep.join("leaf.bin"), 4096);
        write_pattern(&r.join("deep/d0/near.bin"), 4096);
        assert_eq!(both(&r.join("deep"), &opts(&r)).files, 2);
    }

    /// A chain of folders whose full paths pass `PATH_MAX` (1024 bytes). It is built with
    /// `mkdirat` and `openat`, since a full path that long cannot be used at all.
    #[test]
    fn paths_longer_than_path_max_count_the_same_in_both() {
        let (_t, r) = root();
        let top = r.join("long");
        std::fs::create_dir_all(&top).unwrap();
        let data: Vec<u8> = (0..4096).map(|i| (i % 251) as u8 + 1).collect();
        let mut fd = open_dir(None, &top, top.as_os_str()).unwrap();
        let mut depth_bytes = top.as_os_str().len();
        let levels = 12;
        for i in 0..levels {
            let name = CString::new(format!("{i:02}{}", "n".repeat(198))).unwrap();
            // SAFETY: `fd` is an open folder and `name` NUL-terminated.
            assert_eq!(
                unsafe { libc::mkdirat(fd.as_raw_fd(), name.as_ptr(), 0o755) },
                0
            );
            fd = open_dir(
                Some(fd.as_raw_fd()),
                Path::new("."),
                OsStr::from_bytes(name.as_bytes()),
            )
            .unwrap();
            depth_bytes += 201;
            let file = CString::new("f.bin").unwrap();
            // SAFETY: as above; the new descriptor is owned by `out` right away.
            let raw = unsafe {
                libc::openat(
                    fd.as_raw_fd(),
                    file.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_CLOEXEC,
                    0o644,
                )
            };
            assert!(raw >= 0);
            // SAFETY: `raw` was just opened and nothing else owns it.
            let mut out = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
            out.write_all(&data).unwrap();
        }
        assert!(depth_bytes > 2 * libc::PATH_MAX as usize);
        let s = both(&top, &opts(&r));
        assert_eq!(
            s,
            Size {
                alloc: levels * 4096,
                logical: levels * 4096,
                files: levels,
            }
        );
        // Clean up from the bottom, one name at a time.
        let names: Vec<CString> = (0..levels)
            .map(|i| CString::new(format!("{i:02}{}", "n".repeat(198))).unwrap())
            .collect();
        let mut fds = vec![open_dir(None, &top, top.as_os_str()).unwrap()];
        for n in &names[..names.len() - 1] {
            let next = open_dir(
                Some(fds.last().unwrap().as_raw_fd()),
                Path::new("."),
                OsStr::from_bytes(n.as_bytes()),
            )
            .unwrap();
            fds.push(next);
        }
        let file = CString::new("f.bin").unwrap();
        for (i, n) in names.iter().enumerate().rev() {
            let parent = fds[i].as_raw_fd();
            let inner = open_dir(
                Some(parent),
                Path::new("."),
                OsStr::from_bytes(n.as_bytes()),
            )
            .unwrap();
            // SAFETY: open descriptors and NUL-terminated names.
            unsafe {
                assert_eq!(libc::unlinkat(inner.as_raw_fd(), file.as_ptr(), 0), 0);
                assert_eq!(libc::unlinkat(parent, n.as_ptr(), libc::AT_REMOVEDIR), 0);
            }
        }
    }

    #[test]
    fn many_entries_span_several_bulk_reads() {
        let (_t, r) = root();
        let dir = r.join("many");
        std::fs::create_dir_all(&dir).unwrap();
        // Long names so the entries need more than one 64 KiB buffer.
        for i in 0..1500 {
            let name = format!("{i:05}-{}.bin", "n".repeat(120));
            std::fs::write(dir.join(name), [1u8; 10]).unwrap();
        }
        let s = both(&dir, &opts(&r));
        assert_eq!(s.files, 1500);
        assert_eq!(s.logical, 15_000);
    }

    #[test]
    fn bulk_entries_match_lstat() {
        let (_t, r) = root();
        write_pattern(&r.join("m/a.bin"), 3 * 4096);
        std::fs::create_dir_all(r.join("m/sub")).unwrap();
        std::os::unix::fs::symlink("a.bin", r.join("m/link")).unwrap();
        let meter = Meter::new(Arc::default(), None);
        let o = WalkOpts::default();
        let mut w = Walker::new(device_of(&r).unwrap(), &o, &meter);
        let fd = open_dir(None, &r.join("m"), OsStr::new("m")).unwrap();
        let (files, dirs) = w.list(fd.as_raw_fd(), &r.join("m")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(dirs.len(), 1);
        let meta = r.join("m/a.bin").symlink_metadata().unwrap();
        let f = &files[0];
        assert_eq!(f.name, "a.bin");
        assert_eq!(f.dev, meta.dev());
        assert_eq!(f.ino, meta.ino());
        assert_eq!(u64::from(f.nlink), meta.nlink());
        assert_eq!(f.alloc, meta.blocks() * 512);
        assert_eq!(f.logical, meta.size());
        assert_eq!(f.mtime, meta.mtime() as u64);
        assert_eq!(f.atime, meta.atime() as u64);
        assert_eq!(dirs[0].name, "sub");
        assert_eq!(dirs[0].kind, Kind::Dir);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folders_are_skipped_quietly() {
        use std::os::unix::fs::PermissionsExt;
        let (_t, r) = root();
        write_pattern(&r.join("p/ok/a.bin"), 4096);
        write_pattern(&r.join("p/locked/b.bin"), 4096);
        let locked = r.join("p/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let s = measure_bulk(&r.join("p"), &opts(&r), 1);
        let s4 = measure_bulk(&r.join("p"), &opts(&r), 4);
        let reference = measure_reference(&r.join("p"), &opts(&r));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(s.files, 1);
        assert_eq!(s4, s);
        assert_eq!(reference, s);
    }

    #[test]
    fn denied_names_ignore_case() {
        assert!(denied_name(OsStr::new("x.SQLITE")));
        assert!(denied_name(OsStr::new("cookies")));
        assert!(denied_name(OsStr::new("Local Storage")));
        assert!(!denied_name(OsStr::new("sqlite")));
        assert!(!denied_name(OsStr::new("x.sqlite-wal")));
    }

    #[test]
    fn sizes_add_up() {
        let a = Size {
            alloc: 1,
            logical: 2,
            files: 3,
        };
        let mut b = Size {
            alloc: 10,
            logical: 20,
            files: 30,
        };
        b += a;
        assert_eq!(
            b,
            Size {
                alloc: 11,
                logical: 22,
                files: 33
            }
        );
        assert_eq!(
            a + a,
            Size {
                alloc: 2,
                logical: 4,
                files: 6
            }
        );
    }
}
