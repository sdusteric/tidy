//! The Large files scan (design section 5.2): every file and package of the record minimum
//! or more, and folder totals from the scan root down.
//!
//! [`scan`] walks the [`Env`] root with the bulk walker, minus the hard denylist (section 7)
//! and minus Tidy's own folders. It records:
//!
//! - every regular file of [`LargeOpts::record_min`] or more (50 MB by default, C13) as a
//!   [`LargeItem`] of kind `file`;
//! - every package (`.app`, `.framework` and so on, see [`crate::packages`]) of that size as
//!   one item of kind `package` with the total of its contents. Nothing inside a package is
//!   listed;
//! - a [`FolderNode`] tree with every folder total. Children below the record minimum are
//!   folded into the parent's `rest_count` and `rest_alloc`, so the saved tree stays small
//!   and any threshold on the menu can be applied without rescanning.
//!
//! Sizes are allocated bytes. A hard-linked file counts once. Which of its paths counts is
//! fixed: the one in the first top-level folder by name, and inside one top-level folder the
//! first one the walk meets (files before subfolders, names in byte order). Sparse files
//! count what they allocate, so a large logical size alone never makes an item.
//!
//! Last-use dates: the [`Env`]'s [`crate::env::LastUsed`] seam (Spotlight in production)
//! when it has a date, else the later of mtime and atime. For a package the fallback is the
//! newest such time of any file inside it, or the package folder's mtime when it is newer:
//! launching an app reads its executable, which moves that file's atime. The folder's own
//! atime is not used, since listing a folder (as this scan does) can move it.
//!
//! Items under `Library` carry `app_data` (the "App data" tag, C10).
//!
//! The Files and Folders modes of the screen are views over a saved scan: [`files`] and
//! [`folder_view`].

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::lastused;
use crate::model::{FolderNode, LARGE_RECORD_MIN, LargeItem, LargeKind, LargeScan};
use crate::packages::is_package_name;
use crate::paths::AppPaths;
use crate::store::{read_json, write_json_atomic};
use crate::walk::{
    Entry, Kind, Meter, ProgressFn, Size, Tally, Visit, WalkOpts, Walker, default_threads,
    device_of, fd_device, open_dir,
};

/// The name of the tree's root node, shown first in the breadcrumb.
pub const ROOT_NAME: &str = "Home";

/// At most this many "Skipped ..." lines are kept in a scan.
pub const MAX_SKIPPED: usize = 100;

/// Options for [`scan`].
#[derive(Clone, Debug)]
pub struct LargeOpts {
    /// The smallest file or package recorded, and the smallest child kept in the tree.
    pub record_min: u64,
    /// Stops the scan before the next folder once set; [`scan`] then returns None.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Called with files and bytes seen so far and the top-level folder being walked.
    pub progress: Option<ProgressFn>,
    /// Walker threads. 0 means [`default_threads`].
    pub threads: usize,
}

impl Default for LargeOpts {
    fn default() -> LargeOpts {
        LargeOpts {
            record_min: LARGE_RECORD_MIN,
            cancel: None,
            progress: None,
            threads: 0,
        }
    }
}

impl LargeOpts {
    /// Default options with this record minimum.
    pub fn with_record_min(record_min: u64) -> LargeOpts {
        LargeOpts {
            record_min,
            ..LargeOpts::default()
        }
    }
}

/// Scans the [`Env`] root. Returns None when the scan was cancelled; the previous result
/// stays valid then. A root that cannot be read gives an empty scan with one skipped line.
pub fn scan(env: &Env, opts: &LargeOpts) -> Option<LargeScan> {
    let root = env.root.as_path();
    let record_min = opts.record_min;
    let mut out = LargeScan::empty(root.to_path_buf(), env.now(), record_min);
    out.tree = FolderNode {
        name: ROOT_NAME.into(),
        kind: Some(LargeKind::Folder),
        ..FolderNode::default()
    };
    let cancelled = || {
        opts.cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
    };
    let device = match root.symlink_metadata() {
        Ok(m) if m.is_dir() => device_of(root),
        _ => None,
    };
    let Some(device) = device else {
        out.skipped
            .push("Skipped the home folder (it could not be read)".into());
        return Some(out);
    };
    let ctx = Ctx {
        root,
        record_min,
        excluded: [&env.paths.data, &env.paths.cache, &env.paths.logs]
            .into_iter()
            .filter(|p| p.starts_with(root))
            .cloned()
            .collect(),
        ask_system: !env.overridden,
        walk: WalkOpts {
            device: Some(device),
            deny_root: Some(root.to_path_buf()),
            inodes: None,
            cancel: opts.cancel.clone(),
            progress: opts.progress.clone(),
        },
    };
    let root_fd = match open_dir(None, root, root.as_os_str()) {
        Ok(fd) => fd,
        Err(e) => {
            out.skipped.push(skipped_line("the home folder", &e));
            return Some(out);
        }
    };
    let tally = Arc::new(Tally::default());
    let units = {
        let meter = Meter::new(tally.clone(), None);
        let mut walker = Walker::new(device, &ctx.walk, &meter);
        match walker.list(root_fd.as_raw_fd(), root) {
            Ok((files, dirs)) => {
                let mut all: Vec<Entry> = files.into_iter().chain(dirs).collect();
                all.sort_by(|a, b| {
                    use std::os::unix::ffi::OsStrExt;
                    a.name.as_bytes().cmp(b.name.as_bytes())
                });
                all
            }
            Err(e) => {
                out.skipped.push(skipped_line("the home folder", &e));
                return Some(out);
            }
        }
    };
    if cancelled() {
        return None;
    }

    let results = run_units(&ctx, &tally, opts, root_fd.as_fd(), units);
    if cancelled() {
        return None;
    }

    // Put the top-level parts together in name order.
    let mut top = Acc::new(ROOT_NAME.into(), String::new());
    let mut raw_items = Vec::new();
    let mut occ = Vec::new();
    for unit in results.into_iter().flatten() {
        top.merge(unit.part);
        raw_items.extend(unit.items);
        occ.extend(unit.occ);
        out.skipped.extend(unit.skipped);
        out.files_seen += unit.files_seen;
    }
    let mut tree = top.into_node();
    let removed = resolve_hard_links(&mut tree, &mut raw_items, occ, record_min);
    out.files_seen -= removed;
    sort_tree(&mut tree);
    out.tree = tree;

    if out.skipped.len() > MAX_SKIPPED {
        let more = out.skipped.len() - MAX_SKIPPED;
        out.skipped.truncate(MAX_SKIPPED);
        out.skipped.push(format!("Skipped {more} more folders"));
    }

    out.items = raw_items
        .into_iter()
        .map(|r| {
            let (last_used, used_source) =
                lastused::resolve(env.last_used.as_ref(), &r.path, r.file_time);
            LargeItem {
                app_data: is_app_data(&r.rel),
                name: r.name,
                path: r.path,
                rel: r.rel,
                kind: r.kind,
                bytes_alloc: r.alloc,
                bytes_logical: r.logical,
                last_used,
                used_source,
            }
        })
        .collect();
    out.items
        .sort_by(|a, b| b.bytes_alloc.cmp(&a.bytes_alloc).then(a.rel.cmp(&b.rel)));
    Some(out)
}

/// Whether a path relative to the scan root is under `Library` (the "App data" tag).
pub fn is_app_data(rel: &str) -> bool {
    rel.split('/')
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case("Library"))
        && rel.contains('/')
}

// ---------- Saved scan ----------

/// Saves the scan to `last-large.json` in the cache folder, atomically.
pub fn save(paths: &AppPaths, scan: &LargeScan) -> io::Result<()> {
    write_json_atomic(&paths.last_large_file(), scan)
}

/// The scan saved by [`save`], or None when there is none or it does not parse (logged).
pub fn load(paths: &AppPaths) -> Option<LargeScan> {
    match read_json(&paths.last_large_file()) {
        Ok(scan) => scan,
        Err(e) => {
            tracing::warn!("cannot read the saved large files scan: {e}");
            None
        }
    }
}

// ---------- Files mode ----------

/// The filter of the Files list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesFilter {
    /// Every item at or above the threshold, largest first.
    #[default]
    Biggest,
    /// The same, keeping only items not used for the "not opened" days (C12).
    NotOpened,
}

/// The Files list for `filter` at threshold `min`, with "not opened" meaning `days` days
/// before `now` (Unix seconds).
pub fn files(
    scan: &LargeScan,
    filter: FilesFilter,
    min: u64,
    now: u64,
    days: u32,
) -> Vec<&LargeItem> {
    match filter {
        FilesFilter::Biggest => scan.biggest(min),
        FilesFilter::NotOpened => scan.not_opened(min, now, days),
    }
}

/// [`files`] with the threshold and the "not opened" days from the settings and the time
/// from the [`Env`] clock.
pub fn files_in<'a>(scan: &'a LargeScan, filter: FilesFilter, env: &Env) -> Vec<&'a LargeItem> {
    files(
        scan,
        filter,
        env.settings.large_min_bytes,
        env.now(),
        env.settings.not_opened_days,
    )
}

// ---------- Folders mode ----------

/// One step of the breadcrumb.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crumb {
    pub name: String,
    /// Relative to the scan root, empty for the root.
    pub rel: String,
}

/// What Folders mode shows for one folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderView<'a> {
    pub node: &'a FolderNode,
    /// From the root to this folder, root first.
    pub crumbs: Vec<Crumb>,
    /// Children at or above the threshold, largest first: folders, packages and files.
    pub rows: Vec<&'a FolderNode>,
    /// The last row, "1,204 smaller items, 3.1 GB": how many children fell below the
    /// threshold and their total.
    pub smaller_count: u64,
    pub smaller_bytes: u64,
}

/// The view of the folder at `rel` with threshold `min`. None when the tree has no folder
/// there (it was folded, or it is a file or a package, which are leaves).
pub fn folder_view<'a>(scan: &'a LargeScan, rel: &str, min: u64) -> Option<FolderView<'a>> {
    let node = scan.tree.find(rel)?;
    if node.kind != Some(LargeKind::Folder) {
        return None;
    }
    let mut crumbs = vec![Crumb {
        name: scan.tree.name.clone(),
        rel: String::new(),
    }];
    let mut at = &scan.tree;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        at = at.children.iter().find(|c| c.name == part)?;
        crumbs.push(Crumb {
            name: at.name.clone(),
            rel: at.rel.clone(),
        });
    }
    let (rows, smaller_count, smaller_bytes) = node.split_at(min);
    Some(FolderView {
        node,
        crumbs,
        rows,
        smaller_count,
        smaller_bytes,
    })
}

/// The parent of a folder in the tree (Cmd-Up). None at the root.
pub fn parent_rel(rel: &str) -> Option<String> {
    let rel = rel.trim_matches('/');
    if rel.is_empty() {
        return None;
    }
    Some(match rel.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    })
}

// ---------- The walk ----------

/// What every walker thread of one scan shares.
struct Ctx<'a> {
    root: &'a Path,
    record_min: u64,
    /// Tidy's own folders when they lie under the root. Never scanned.
    excluded: Vec<PathBuf>,
    /// Whether UTType may decide package extensions outside the fixed list.
    ask_system: bool,
    walk: WalkOpts,
}

impl Ctx<'_> {
    /// `path` relative to the root with `/` separators.
    fn rel(&self, path: &Path) -> String {
        let rel = path.strip_prefix(self.root).unwrap_or(path);
        let mut s = String::new();
        for c in rel.components() {
            if let Component::Normal(part) = c {
                if !s.is_empty() {
                    s.push('/');
                }
                s.push_str(&part.to_string_lossy());
            }
        }
        s
    }
}

/// A file or package found at or above the record minimum, before its last-use date is
/// looked up.
#[derive(Debug)]
struct RawItem {
    path: PathBuf,
    rel: String,
    name: String,
    kind: LargeKind,
    alloc: u64,
    logical: u64,
    file_time: Option<u64>,
}

/// One counted occurrence of a multiply-linked file, so links seen from two top-level
/// folders can be counted once afterwards.
#[derive(Debug)]
struct Occ {
    key: (u64, u64),
    unit: usize,
    rel: String,
    alloc: u64,
    logical: u64,
}

/// A folder being summed.
#[derive(Debug, Default)]
struct Acc {
    name: String,
    rel: String,
    size: Size,
    children: Vec<FolderNode>,
    rest_count: u64,
    rest_alloc: u64,
    /// The folder could not be walked; it is left out of the tree.
    discard: bool,
}

impl Acc {
    fn new(name: String, rel: String) -> Acc {
        Acc {
            name,
            rel,
            ..Acc::default()
        }
    }

    /// Adds a finished child: kept when it reaches `min`, folded otherwise.
    fn add_child(&mut self, node: FolderNode, min: u64) {
        self.size += Size {
            alloc: node.bytes_alloc,
            logical: node.bytes_logical,
            files: node.files,
        };
        if node.bytes_alloc >= min {
            self.children.push(node);
        } else {
            self.rest_count += 1;
            self.rest_alloc += node.bytes_alloc;
        }
    }

    /// Adds another partial sum of the same folder.
    fn merge(&mut self, other: Acc) {
        self.size += other.size;
        self.children.extend(other.children);
        self.rest_count += other.rest_count;
        self.rest_alloc += other.rest_alloc;
    }

    fn into_node(self) -> FolderNode {
        FolderNode {
            name: self.name,
            rel: self.rel,
            kind: Some(LargeKind::Folder),
            bytes_alloc: self.size.alloc,
            bytes_logical: self.size.logical,
            files: self.size.files,
            children: self.children,
            rest_count: self.rest_count,
            rest_alloc: self.rest_alloc,
        }
    }
}

/// What one top-level entry of the root contributed.
struct UnitOut {
    /// A partial sum of the root holding just this entry.
    part: Acc,
    items: Vec<RawItem>,
    occ: Vec<Occ>,
    skipped: Vec<String>,
    files_seen: u64,
}

/// Walks the top-level entries on a few threads, one entry at a time. The result keeps the
/// entries' order; a slot is None only when the scan was cancelled before it.
fn run_units(
    ctx: &Ctx<'_>,
    tally: &Arc<Tally>,
    opts: &LargeOpts,
    root_fd: BorrowedFd<'_>,
    units: Vec<Entry>,
) -> Vec<Option<UnitOut>> {
    let threads = match opts.threads {
        0 => default_threads(),
        n => n,
    }
    .min(units.len())
    .max(1);
    let slots: Vec<Mutex<(Option<Entry>, Option<UnitOut>)>> = units
        .into_iter()
        .map(|e| Mutex::new((Some(e), None)))
        .collect();
    let next = AtomicUsize::new(0);
    let device = ctx.walk.device.unwrap_or_default();
    std::thread::scope(|s| {
        for _ in 0..threads {
            let (slots, next, tally) = (&slots, &next, tally.clone());
            s.spawn(move || {
                let meter = Meter::new(tally, ctx.walk.progress.clone());
                let mut walker = Walker::new(device, &ctx.walk, &meter);
                let mut pkg = Walker::new(device, &ctx.walk, &meter);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = slots.get(i) else { break };
                    if ctx.walk.cancelled() {
                        break;
                    }
                    let Some(entry) = slot.lock().unwrap_or_else(|e| e.into_inner()).0.take()
                    else {
                        continue;
                    };
                    let out = run_unit(ctx, &meter, &mut walker, &mut pkg, root_fd, i, entry);
                    slot.lock().unwrap_or_else(|e| e.into_inner()).1 = Some(out);
                }
                meter.flush();
            });
        }
    });
    slots
        .into_iter()
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()).1)
        .collect()
}

/// Walks one top-level entry of the root.
fn run_unit(
    ctx: &Ctx<'_>,
    meter: &Meter,
    walker: &mut Walker<'_>,
    pkg: &mut Walker<'_>,
    root_fd: BorrowedFd<'_>,
    unit: usize,
    entry: Entry,
) -> UnitOut {
    let mut v = LargeVisitor {
        ctx,
        pkg,
        unit,
        seen: HashSet::new(),
        stack: vec![Acc::new(ROOT_NAME.into(), String::new())],
        items: Vec::new(),
        occ: Vec::new(),
        skipped: Vec::new(),
        files_seen: 0,
    };
    match entry.kind {
        Kind::File => {
            meter.file(entry.alloc);
            v.file(ctx.root, &entry);
        }
        Kind::Dir => {
            meter.top(&ctx.root.join(&entry.name));
            walker.walk_children(Some(root_fd), ctx.root, vec![entry], &mut v);
        }
        Kind::Other => {}
    }
    let LargeVisitor {
        mut stack,
        items,
        occ,
        mut skipped,
        files_seen,
        ..
    } = v;
    skipped.truncate(MAX_SKIPPED + 1);
    UnitOut {
        part: stack.pop().unwrap_or_default(),
        items,
        occ,
        skipped,
        files_seen,
    }
}

/// Builds the tree of one top-level entry.
struct LargeVisitor<'c, 'p, 'w> {
    ctx: &'c Ctx<'c>,
    /// A second walker for measuring packages while the main walk is inside a folder.
    pkg: &'p mut Walker<'w>,
    unit: usize,
    /// Multiply-linked files already counted in this top-level entry.
    seen: HashSet<(u64, u64)>,
    /// The folders being summed, the root's partial sum at the bottom.
    stack: Vec<Acc>,
    items: Vec<RawItem>,
    occ: Vec<Occ>,
    skipped: Vec<String>,
    files_seen: u64,
}

impl LargeVisitor<'_, '_, '_> {
    /// Measures the package at `path` as one leaf and adds it to the current folder.
    fn package(&mut self, path: &Path, e: &Entry) {
        let ctx = self.ctx;
        let rel = ctx.rel(path);
        let fd = match open_dir(None, path, &e.name) {
            Ok(fd) => fd,
            Err(err) => {
                self.skipped.push(skipped_line(&rel, &err));
                return;
            }
        };
        if fd_device(&fd) != Some(self.pkg.device()) {
            return;
        }
        let (files, subdirs) = match self.pkg.list(fd.as_raw_fd(), path) {
            Ok(x) => x,
            Err(err) => {
                self.skipped.push(skipped_line(&rel, &err));
                return;
            }
        };
        let mut count = PackageCount {
            ctx,
            unit: self.unit,
            seen: &mut self.seen,
            occ: &mut self.occ,
            size: Size::default(),
            newest: 0,
        };
        self.pkg.visit_files(path, &files, &mut count);
        self.pkg
            .walk_children(Some(fd.as_fd()), path, subdirs, &mut count);
        let (size, newest) = (count.size, count.newest.max(e.mtime));
        self.files_seen += size.files;
        let name = e.name.to_string_lossy().into_owned();
        if size.alloc >= ctx.record_min {
            self.items.push(RawItem {
                path: path.to_path_buf(),
                rel: rel.clone(),
                name: name.clone(),
                kind: LargeKind::Package,
                alloc: size.alloc,
                logical: size.logical,
                file_time: Some(newest),
            });
        }
        let node = FolderNode {
            name,
            rel,
            kind: Some(LargeKind::Package),
            bytes_alloc: size.alloc,
            bytes_logical: size.logical,
            files: size.files,
            ..FolderNode::default()
        };
        if let Some(top) = self.stack.last_mut() {
            top.add_child(node, ctx.record_min);
        }
    }
}

impl Visit for LargeVisitor<'_, '_, '_> {
    fn file(&mut self, dir: &Path, e: &Entry) {
        let ctx = self.ctx;
        let path = dir.join(&e.name);
        if e.nlink > 1 {
            if !self.seen.insert((e.dev, e.ino)) {
                return;
            }
            self.occ.push(Occ {
                key: (e.dev, e.ino),
                unit: self.unit,
                rel: ctx.rel(&path),
                alloc: e.alloc,
                logical: e.logical,
            });
        }
        self.files_seen += 1;
        let Some(top) = self.stack.last_mut() else {
            return;
        };
        if e.alloc < ctx.record_min {
            top.size += Size {
                alloc: e.alloc,
                logical: e.logical,
                files: 1,
            };
            top.rest_count += 1;
            top.rest_alloc += e.alloc;
            return;
        }
        let rel = ctx.rel(&path);
        let name = e.name.to_string_lossy().into_owned();
        self.items.push(RawItem {
            path,
            rel: rel.clone(),
            name: name.clone(),
            kind: LargeKind::File,
            alloc: e.alloc,
            logical: e.logical,
            file_time: Some(e.file_time()),
        });
        top.add_child(
            FolderNode {
                name,
                rel,
                kind: Some(LargeKind::File),
                bytes_alloc: e.alloc,
                bytes_logical: e.logical,
                files: 1,
                ..FolderNode::default()
            },
            ctx.record_min,
        );
    }

    fn enter(&mut self, path: &Path, e: &Entry) -> bool {
        if self.ctx.excluded.iter().any(|x| x == path) {
            return false;
        }
        if is_package_name(&e.name, self.ctx.ask_system) {
            self.package(path, e);
            return false;
        }
        self.stack.push(Acc::new(
            e.name.to_string_lossy().into_owned(),
            self.ctx.rel(path),
        ));
        true
    }

    fn abandon(&mut self, path: &Path, err: Option<&io::Error>) {
        if let Some(top) = self.stack.last_mut() {
            top.discard = true;
        }
        if let Some(err) = err {
            let rel = self.ctx.rel(path);
            self.skipped.push(skipped_line(&rel, err));
        }
    }

    fn leave(&mut self, _path: &Path) {
        // The bottom of the stack is the root's partial sum, which only run_unit takes.
        if self.stack.len() < 2 {
            return;
        }
        let Some(done) = self.stack.pop() else { return };
        if done.discard {
            return;
        }
        let min = self.ctx.record_min;
        if let Some(parent) = self.stack.last_mut() {
            parent.add_child(done.into_node(), min);
        }
    }
}

/// Sums a package: every file inside, hard links once, and the newest file time.
struct PackageCount<'a> {
    ctx: &'a Ctx<'a>,
    unit: usize,
    seen: &'a mut HashSet<(u64, u64)>,
    occ: &'a mut Vec<Occ>,
    size: Size,
    newest: u64,
}

impl Visit for PackageCount<'_> {
    fn file(&mut self, dir: &Path, e: &Entry) {
        if e.nlink > 1 {
            if !self.seen.insert((e.dev, e.ino)) {
                return;
            }
            self.occ.push(Occ {
                key: (e.dev, e.ino),
                unit: self.unit,
                rel: self.ctx.rel(&dir.join(&e.name)),
                alloc: e.alloc,
                logical: e.logical,
            });
        }
        self.size += Size {
            alloc: e.alloc,
            logical: e.logical,
            files: 1,
        };
        self.newest = self.newest.max(e.file_time());
    }

    fn enter(&mut self, _path: &Path, _e: &Entry) -> bool {
        true
    }

    fn abandon(&mut self, _path: &Path, _err: Option<&io::Error>) {}

    fn leave(&mut self, _path: &Path) {}
}

/// "Skipped Documents (access not allowed)" and the like.
fn skipped_line(what: &str, err: &io::Error) -> String {
    let why = match err.kind() {
        io::ErrorKind::PermissionDenied => "access not allowed",
        io::ErrorKind::NotFound => "it was removed during the scan",
        _ => "it could not be read",
    };
    format!("Skipped {what} ({why})")
}

/// Counts every multiply-linked file once over the whole scan. Inside one top-level entry the
/// walk already did; across entries the first entry by name keeps the file and it is taken
/// out of the others. Returns how many file occurrences were taken out.
fn resolve_hard_links(
    tree: &mut FolderNode,
    items: &mut Vec<RawItem>,
    occ: Vec<Occ>,
    min: u64,
) -> u64 {
    let mut owner: HashMap<(u64, u64), usize> = HashMap::new();
    for o in &occ {
        owner
            .entry(o.key)
            .and_modify(|u| *u = (*u).min(o.unit))
            .or_insert(o.unit);
    }
    let mut removed = 0;
    for o in occ.iter().filter(|o| owner.get(&o.key) != Some(&o.unit)) {
        let parts: Vec<&str> = o.rel.split('/').collect();
        subtract(tree, &parts, o.alloc, o.logical, min);
        removed += 1;
        if let Some(i) = items
            .iter()
            .position(|it| it.kind == LargeKind::File && it.rel == o.rel)
        {
            items.remove(i);
        } else if let Some(i) = items.iter().position(|it| {
            it.kind == LargeKind::Package
                && o.rel.len() > it.rel.len()
                && o.rel.starts_with(&it.rel)
                && o.rel.as_bytes()[it.rel.len()] == b'/'
        }) {
            let it = &mut items[i];
            it.alloc = it.alloc.saturating_sub(o.alloc);
            it.logical = it.logical.saturating_sub(o.logical);
            if it.alloc < min {
                items.remove(i);
            }
        }
    }
    removed
}

/// Takes one file of `alloc` bytes at `parts` (below `node`) out of the tree, folding any
/// kept child that drops below `min`.
fn subtract(node: &mut FolderNode, parts: &[&str], alloc: u64, logical: u64, min: u64) {
    node.bytes_alloc = node.bytes_alloc.saturating_sub(alloc);
    node.bytes_logical = node.bytes_logical.saturating_sub(logical);
    node.files = node.files.saturating_sub(1);
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    let Some(i) = node.children.iter().position(|c| c.name == *first) else {
        // The file, or the folder holding it, was folded into the rest.
        node.rest_alloc = node.rest_alloc.saturating_sub(alloc);
        if rest.is_empty() {
            node.rest_count = node.rest_count.saturating_sub(1);
        }
        return;
    };
    let child = &mut node.children[i];
    match child.kind {
        Some(LargeKind::File) if rest.is_empty() => {
            node.children.remove(i);
            return;
        }
        Some(LargeKind::Package) | Some(LargeKind::File) => {
            child.bytes_alloc = child.bytes_alloc.saturating_sub(alloc);
            child.bytes_logical = child.bytes_logical.saturating_sub(logical);
            child.files = child.files.saturating_sub(1);
        }
        _ => subtract(child, rest, alloc, logical, min),
    }
    if node.children[i].bytes_alloc < min {
        let folded = node.children.remove(i);
        node.rest_count += 1;
        node.rest_alloc += folded.bytes_alloc;
    }
}

/// Sorts every kept child list largest first, then by name.
fn sort_tree(node: &mut FolderNode) {
    node.children
        .sort_by(|a, b| b.bytes_alloc.cmp(&a.bytes_alloc).then(a.name.cmp(&b.name)));
    for c in &mut node.children {
        sort_tree(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(name: &str, rel: &str, kind: LargeKind, b: u64) -> FolderNode {
        FolderNode {
            name: name.into(),
            rel: rel.into(),
            kind: Some(kind),
            bytes_alloc: b,
            bytes_logical: b,
            files: 1,
            ..FolderNode::default()
        }
    }

    #[test]
    fn app_data_is_anything_under_library() {
        assert!(is_app_data("Library/Application Support/x.bin"));
        assert!(is_app_data("library/Caches/y"));
        assert!(!is_app_data("Library"));
        assert!(!is_app_data("Movies/Library/x"));
        assert!(!is_app_data("LibraryX/a"));
    }

    #[test]
    fn parent_of_a_folder() {
        assert_eq!(parent_rel(""), None);
        assert_eq!(parent_rel("Library").as_deref(), Some(""));
        assert_eq!(
            parent_rel("Library/Application Support/").as_deref(),
            Some("Library")
        );
    }

    #[test]
    fn subtract_removes_a_kept_file_and_folds_what_drops_below() {
        // Home (300) > A (300) > [big (200, file), small folder folded (100)]
        let a = FolderNode {
            name: "A".into(),
            rel: "A".into(),
            kind: Some(LargeKind::Folder),
            bytes_alloc: 300,
            bytes_logical: 300,
            files: 3,
            children: vec![leaf("big", "A/big", LargeKind::File, 200)],
            rest_count: 1,
            rest_alloc: 100,
        };
        let mut root = FolderNode {
            name: ROOT_NAME.into(),
            kind: Some(LargeKind::Folder),
            bytes_alloc: 300,
            bytes_logical: 300,
            files: 3,
            children: vec![a],
            ..FolderNode::default()
        };
        // A file inside the folded folder.
        subtract(&mut root, &["A", "f", "x"], 40, 40, 150);
        let a = root.find("A").unwrap();
        assert_eq!((a.bytes_alloc, a.rest_count, a.rest_alloc), (260, 1, 60));
        // The kept file itself: A drops to 60, below 150, so it folds into the root.
        subtract(&mut root, &["A", "big"], 200, 200, 150);
        assert!(root.find("A").is_none());
        assert_eq!(root.bytes_alloc, 60);
        assert_eq!((root.rest_count, root.rest_alloc), (1, 60));
        assert_eq!(root.files, 1);
    }
}
