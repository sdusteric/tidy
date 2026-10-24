//! A small declarative tree builder.
//!
//! Every file is created with a size that is known up front, and every node records its
//! declared allocated and logical bytes in a [`Ledger`]. Plain files are a multiple of the
//! 4096-byte block and filled with a deterministic non-zero pattern, so on APFS the allocated
//! bytes (`st_blocks * 512`) equal the logical bytes. The `tests/build.rs` test checks that
//! on the machine it runs on.
//!
//! Times are set at [`Tree::finish`] with `utimensat`, deepest nodes first, so creating a file
//! never changes a directory time after it was set. Access time is set equal to modification
//! time. Reading a file afterwards can move its access time; scans only stat files.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// The allocation unit the fixtures assume (APFS, 4 KiB blocks).
pub const BLOCK: u64 = 4096;

const DAY: u64 = 86_400;

/// Age given to every node that does not set its own: old enough for the 30-day project
/// rules and the 24-hour rule, young enough for the 90-day "not opened" filter.
pub const DEFAULT_AGE_DAYS: u64 = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File,
    Symlink,
}

/// One entry the builder created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Path relative to the tree root, `/` separated. Empty for the root itself.
    pub rel: String,
    pub kind: NodeKind,
    /// Space on disk the file is declared to take. Zero for directories and symlinks.
    pub alloc: u64,
    /// File length. Larger than `alloc` for sparse files.
    pub logical: u64,
    /// True for the second and later hard links of a file. Totals count a file once.
    pub dup: bool,
    /// Modification and access time, Unix seconds.
    pub mtime: u64,
}

/// What the builder created, with the declared sizes and times.
#[derive(Clone, Debug)]
pub struct Ledger {
    root: PathBuf,
    now: u64,
    nodes: BTreeMap<String, Node>,
}

impl Ledger {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    pub fn get(&self, rel: &str) -> Option<&Node> {
        self.nodes.get(rel)
    }

    pub fn contains(&self, rel: &str) -> bool {
        self.nodes.contains_key(rel)
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }

    /// `rel` itself and everything below it. An empty `rel` is the whole tree.
    pub fn under<'a>(&'a self, rel: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        let prefix = format!("{rel}/");
        self.nodes
            .values()
            .filter(move |n| rel.is_empty() || n.rel == rel || n.rel.starts_with(&prefix))
    }

    /// Declared allocated bytes of the files at or below `rel`, a hard-linked file counted once.
    pub fn alloc_under(&self, rel: &str) -> u64 {
        self.alloc_under_excluding(rel, &[])
    }

    /// Like [`Ledger::alloc_under`], skipping everything under the `excluded` prefixes.
    pub fn alloc_under_excluding(&self, rel: &str, excluded: &[&str]) -> u64 {
        self.counted(rel, excluded).map(|n| n.alloc).sum()
    }

    /// Declared logical bytes of the files at or below `rel`, a hard-linked file counted once.
    pub fn logical_under_excluding(&self, rel: &str, excluded: &[&str]) -> u64 {
        self.counted(rel, excluded).map(|n| n.logical).sum()
    }

    pub fn logical_under(&self, rel: &str) -> u64 {
        self.logical_under_excluding(rel, &[])
    }

    fn counted<'a>(
        &'a self,
        rel: &'a str,
        excluded: &'a [&'a str],
    ) -> impl Iterator<Item = &'a Node> + 'a {
        self.under(rel).filter(move |n| {
            n.kind == NodeKind::File
                && !n.dup
                && !excluded
                    .iter()
                    .any(|e| n.rel == *e || n.rel.starts_with(&format!("{e}/")))
        })
    }
}

/// Builds a tree under a root folder. See the module docs.
pub struct Tree {
    root: PathBuf,
    now: u64,
    nodes: BTreeMap<String, Node>,
}

impl Tree {
    /// Creates `root` (and its parents) if needed. `now` is the fixed clock the times are
    /// relative to, in Unix seconds.
    pub fn new(root: impl Into<PathBuf>, now: u64) -> io::Result<Tree> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let mut nodes = BTreeMap::new();
        nodes.insert(
            String::new(),
            Node {
                rel: String::new(),
                kind: NodeKind::Dir,
                alloc: 0,
                logical: 0,
                dup: false,
                mtime: now.saturating_sub(DEFAULT_AGE_DAYS * DAY),
            },
        );
        Ok(Tree { root, now, nodes })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// A directory, with any missing parents.
    pub fn dir(&mut self, rel: &str) -> io::Result<()> {
        check_rel(rel)?;
        self.ensure_dirs(rel)
    }

    /// A file of `bytes` bytes filled with the deterministic pattern. `bytes` must be a
    /// positive multiple of 4096.
    pub fn file(&mut self, rel: &str, bytes: u64) -> io::Result<()> {
        if bytes == 0 || !bytes.is_multiple_of(BLOCK) {
            return Err(invalid(format!(
                "{rel}: size {bytes} is not a positive multiple of {BLOCK}"
            )));
        }
        self.prepare_file(rel)?;
        let data = fill_pattern(rel, bytes as usize);
        self.write(rel, &data)?;
        self.record(rel, NodeKind::File, bytes, bytes);
        Ok(())
    }

    /// A text file with exact content, for markers and plists. It takes the next whole block on
    /// disk, or nothing when it is empty.
    pub fn text(&mut self, rel: &str, content: &str) -> io::Result<()> {
        self.prepare_file(rel)?;
        self.write(rel, content.as_bytes())?;
        let len = content.len() as u64;
        self.record(rel, NodeKind::File, round_up(len), len);
        Ok(())
    }

    /// An empty file, used where only the name matters (`Package.swift`, a `.py` file).
    pub fn marker(&mut self, rel: &str) -> io::Result<()> {
        self.text(rel, "")
    }

    /// A sparse file: created empty, then its length set to `logical` with `set_len`. It takes
    /// no space on disk and reports `logical` as its length.
    ///
    /// A small write before `set_len` would give a nonzero size, but APFS rounds that up to
    /// its own extent size (16 KiB on the volume this was written on, not one block), so the
    /// exact allocation would depend on the volume. Zero does not.
    pub fn sparse(&mut self, rel: &str, logical: u64) -> io::Result<()> {
        if logical <= BLOCK {
            return Err(invalid(format!("{rel}: sparse length must exceed {BLOCK}")));
        }
        self.prepare_file(rel)?;
        let f = File::create(self.path(rel))?;
        f.set_len(logical)?;
        self.record(rel, NodeKind::File, 0, logical);
        Ok(())
    }

    /// Another name for the same file. The new link is marked `dup`, so folder totals count
    /// the file once. Keep both links in one folder: which link a scan meets first depends on
    /// directory order, so only a folder that holds both has a deterministic total.
    pub fn hard_link(&mut self, existing: &str, new: &str) -> io::Result<()> {
        check_rel(new)?;
        let src = match self.nodes.get(existing) {
            Some(n) if n.kind == NodeKind::File => n.clone(),
            _ => return Err(invalid(format!("{existing}: not a file in this tree"))),
        };
        self.prepare_file_slot(new)?;
        fs::hard_link(self.path(existing), self.path(new))?;
        self.nodes.insert(
            new.to_string(),
            Node {
                rel: new.to_string(),
                kind: NodeKind::File,
                alloc: src.alloc,
                logical: src.logical,
                dup: true,
                mtime: src.mtime,
            },
        );
        Ok(())
    }

    /// A symlink to `target`, which can be any path (also one outside the tree).
    pub fn symlink(&mut self, rel: &str, target: &Path) -> io::Result<()> {
        self.prepare_file(rel)?;
        std::os::unix::fs::symlink(target, self.path(rel))?;
        self.record(rel, NodeKind::Symlink, 0, 0);
        Ok(())
    }

    /// A fake `.app` bundle: `Contents/Info.plist` with the bundle id and name, and a
    /// one-block `Contents/MacOS/<name>` executable (not executable, never run).
    pub fn app(&mut self, rel: &str, bundle_id: &str, name: &str) -> io::Result<()> {
        if !rel.ends_with(".app") {
            return Err(invalid(format!("{rel}: an app folder name ends in .app")));
        }
        self.dir(&format!("{rel}/Contents/MacOS"))?;
        self.text(
            &format!("{rel}/Contents/Info.plist"),
            &info_plist(bundle_id, name),
        )?;
        self.file(&format!("{rel}/Contents/MacOS/{name}"), BLOCK)
    }

    /// Sets the time of one node.
    pub fn set_time(&mut self, rel: &str, secs: u64) -> io::Result<()> {
        match self.nodes.get_mut(rel) {
            Some(n) => {
                n.mtime = secs;
                Ok(())
            }
            None => Err(invalid(format!("{rel}: not in this tree"))),
        }
    }

    /// Sets the time of a node and everything below it to `days` before the clock.
    pub fn age_days(&mut self, rel: &str, days: u64) -> io::Result<()> {
        self.age_secs(rel, days * DAY)
    }

    /// Like [`Tree::age_days`], in seconds.
    pub fn age_secs(&mut self, rel: &str, secs: u64) -> io::Result<()> {
        if !self.nodes.contains_key(rel) {
            return Err(invalid(format!("{rel}: not in this tree")));
        }
        let t = self.now.saturating_sub(secs);
        let prefix = format!("{rel}/");
        for n in self.nodes.values_mut() {
            if rel.is_empty() || n.rel == rel || n.rel.starts_with(&prefix) {
                n.mtime = t;
            }
        }
        Ok(())
    }

    /// Applies every time and returns the ledger.
    pub fn finish(self) -> io::Result<Ledger> {
        let mut order: Vec<&Node> = self.nodes.values().collect();
        // Deepest first, so a directory time is set after everything below it.
        order.sort_by(|a, b| depth(&b.rel).cmp(&depth(&a.rel)).then(a.rel.cmp(&b.rel)));
        for n in order {
            let path = if n.rel.is_empty() {
                self.root.clone()
            } else {
                self.root.join(&n.rel)
            };
            set_times(&path, n.mtime, n.kind == NodeKind::Symlink)?;
        }
        Ok(Ledger {
            root: self.root,
            now: self.now,
            nodes: self.nodes,
        })
    }

    fn prepare_file(&mut self, rel: &str) -> io::Result<()> {
        check_rel(rel)?;
        if rel.is_empty() {
            return Err(invalid("empty path".into()));
        }
        self.prepare_file_slot(rel)
    }

    fn prepare_file_slot(&mut self, rel: &str) -> io::Result<()> {
        if self.nodes.contains_key(rel) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{rel} already exists in the tree"),
            ));
        }
        let parent = rel.rsplit_once('/').map_or("", |(p, _)| p);
        self.ensure_dirs(parent)
    }

    fn ensure_dirs(&mut self, rel: &str) -> io::Result<()> {
        let mut cur = String::new();
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            if !cur.is_empty() {
                cur.push('/');
            }
            cur.push_str(part);
            match self.nodes.get(&cur) {
                Some(n) if n.kind == NodeKind::Dir => {}
                Some(_) => return Err(invalid(format!("{cur}: exists and is not a folder"))),
                None => {
                    fs::create_dir(self.root.join(&cur))?;
                    self.record(&cur.clone(), NodeKind::Dir, 0, 0);
                }
            }
        }
        Ok(())
    }

    fn write(&self, rel: &str, data: &[u8]) -> io::Result<()> {
        let mut f = File::create(self.path(rel))?;
        f.write_all(data)
    }

    fn record(&mut self, rel: &str, kind: NodeKind, alloc: u64, logical: u64) {
        self.nodes.insert(
            rel.to_string(),
            Node {
                rel: rel.to_string(),
                kind,
                alloc,
                logical,
                dup: false,
                mtime: self.now.saturating_sub(DEFAULT_AGE_DAYS * DAY),
            },
        );
    }
}

fn depth(rel: &str) -> usize {
    if rel.is_empty() {
        0
    } else {
        rel.split('/').count()
    }
}

fn round_up(len: u64) -> u64 {
    len.div_ceil(BLOCK) * BLOCK
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// A relative path with plain components only.
fn check_rel(rel: &str) -> io::Result<()> {
    for c in Path::new(rel).components() {
        match c {
            Component::Normal(_) => {}
            _ => return Err(invalid(format!("{rel:?} is not a plain relative path"))),
        }
    }
    Ok(())
}

fn set_times(path: &Path, secs: u64, symlink: bool) -> io::Result<()> {
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| invalid(e.to_string()))?;
    let ts = libc::timespec {
        tv_sec: secs as libc::time_t,
        tv_nsec: 0,
    };
    let times = [ts, ts];
    let flags = if symlink {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: `c` is a valid NUL-terminated path and `times` holds the two timespecs the call
    // reads (access time, modification time).
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), flags) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Deterministic bytes for `seed` and `len`: a splitmix64 stream seeded from an FNV-1a hash of
/// the seed, each byte mapped into 1..=255 (never zero, and not compressible).
pub fn fill_pattern(seed: &str, len: usize) -> Vec<u8> {
    let mut state: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        state ^= u64::from(b);
        state = state.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        out.extend(z.to_le_bytes().iter().map(|b| b % 255 + 1));
    }
    out.truncate(len);
    out
}

/// A minimal `Info.plist` for a fake app.
pub fn info_plist(bundle_id: &str, name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
\t<key>CFBundleIdentifier</key>\n\
\t<string>{bundle_id}</string>\n\
\t<key>CFBundleName</key>\n\
\t<string>{name}</string>\n\
\t<key>CFBundleExecutable</key>\n\
\t<string>{name}</string>\n\
\t<key>CFBundlePackageType</key>\n\
\t<string>APPL</string>\n\
\t<key>CFBundleShortVersionString</key>\n\
\t<string>1.0</string>\n\
</dict>\n\
</plist>\n"
    )
}
