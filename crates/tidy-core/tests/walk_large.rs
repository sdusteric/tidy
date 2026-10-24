//! The bulk walker and the Large files scan on small trees built in temp folders.
//!
//! These tests build their own trees (they do not use tidy-fixtures), always under a fresh
//! temp folder, and run with `Env::for_tests`: a fixed clock, file times only (no Spotlight)
//! and a folder Trash. Nothing here reads the real home folder.

use std::fs::{File, FileTimes};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use tidy_core::env::{FixedClock, TEST_NOW};
use tidy_core::large::{self, FilesFilter, LargeOpts};
use tidy_core::model::{LargeKind, LargeScan, UseSource};
use tidy_core::walk::{
    ProgressFn, Size, WalkOpts, WalkProgress, measure, measure_bulk, measure_reference,
};
use tidy_core::{Env, Settings};

const KB4: usize = 4096;
/// The record minimum the tests scan with: 1 MB (10^6 bytes).
const MIN: u64 = 1_000_000;
const DAY: u64 = 86_400;

/// A temp folder with a fake home and a Tidy home beside it.
struct Tree {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    home: PathBuf,
}

impl Tree {
    fn new() -> Tree {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        Tree {
            _tmp: tmp,
            base,
            home,
        }
    }

    fn at(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    /// `len` bytes (a multiple of 4096) of a non-zero pattern, so APFS allocates every block.
    fn write(&self, rel: &str, len: usize) -> PathBuf {
        let p = self.at(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8 + 1).collect();
        std::fs::write(&p, data).unwrap();
        p
    }

    /// Sets mtime and atime to `days_ago` days before the test clock.
    fn date(&self, rel: &str, days_ago: u64) {
        let t = UNIX_EPOCH + Duration::from_secs(TEST_NOW - days_ago * DAY);
        let f = File::options().write(true).open(self.at(rel)).unwrap();
        f.set_times(FileTimes::new().set_modified(t).set_accessed(t))
            .unwrap();
    }

    fn env(&self) -> Env {
        Env::for_tests(&self.home, &self.base.join("tidy"))
    }
}

fn scan(env: &Env, threads: usize) -> LargeScan {
    large::scan(
        env,
        &LargeOpts {
            threads,
            ..LargeOpts::with_record_min(MIN)
        },
    )
    .expect("not cancelled")
}

fn rels(items: &[&tidy_core::LargeItem]) -> Vec<String> {
    items.iter().map(|i| i.rel.clone()).collect()
}

/// A tree with a bit of everything the walker must handle.
fn varied_tree() -> Tree {
    let t = Tree::new();
    t.write("Movies/trip.mov", 600 * KB4);
    t.write("Movies/2025/a.bin", 3 * KB4);
    t.write("Movies/2025/deeper/b.bin", 5 * KB4);
    t.write("Documents/notes.txt", KB4);
    t.write("Library/Caches/app/blob", 40 * KB4);
    t.write("Library/Application Support/Models/m.gguf", 700 * KB4);
    t.write("Library/Containers/com.x/Data/secret.bin", 900 * KB4);
    t.write("Library/Application Support/Browser/Cookies", 10 * KB4);
    t.write("Library/Application Support/Browser/db.sqlite", 10 * KB4);
    t.write(".Trash/old.bin", 300 * KB4);
    for i in 0..40 {
        t.write(
            &format!("Applications/Big.app/Contents/Resources/r{i:02}.dat"),
            8 * KB4,
        );
    }
    t.write("Applications/Big.app/Contents/MacOS/Big", 4 * KB4);
    std::fs::create_dir_all(t.at("empty/inner")).unwrap();
    // Hard links: a pair in one folder, and a pair across two top-level folders.
    t.write("Archive/pair-a.bin", 300 * KB4);
    std::fs::hard_link(t.at("Archive/pair-a.bin"), t.at("Archive/pair-b.bin")).unwrap();
    std::fs::create_dir_all(t.at("Backup")).unwrap();
    std::fs::hard_link(t.at("Archive/pair-a.bin"), t.at("Backup/pair-c.bin")).unwrap();
    // A sparse file: large logical size, tiny allocation.
    std::fs::create_dir_all(t.at("Downloads")).unwrap();
    let mut f = File::create(t.at("Downloads/sparse.img")).unwrap();
    f.seek(SeekFrom::Start(512 * 1024 * 1024)).unwrap();
    f.write_all(&[9u8; KB4]).unwrap();
    drop(f);
    // Symlinks out of the root, to a folder and to a file.
    let outside = t.base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("huge.bin"), vec![3u8; 800 * KB4]).unwrap();
    std::os::unix::fs::symlink(&outside, t.at("Links-dir")).unwrap();
    std::fs::create_dir_all(t.at("Links")).unwrap();
    std::os::unix::fs::symlink(outside.join("huge.bin"), t.at("Links/huge-link.bin")).unwrap();
    t
}

// ---------- Walker equivalence ----------

#[test]
fn bulk_walker_totals_equal_the_reference_exactly() {
    let t = varied_tree();
    let o = WalkOpts::for_root(&t.home);
    let mut checked = 0;
    let mut stack = vec![t.home.clone()];
    while let Some(dir) = stack.pop() {
        let reference = measure_reference(&dir, &o);
        for threads in [1, 2, 4] {
            assert_eq!(
                measure_bulk(&dir, &o, threads),
                reference,
                "{dir:?} on {threads} threads"
            );
        }
        // Without the denylist too.
        let plain = WalkOpts::default();
        assert_eq!(
            measure(&dir, &plain),
            measure_reference(&dir, &plain),
            "{dir:?}"
        );
        checked += 1;
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
        }
    }
    assert!(checked > 15, "{checked}");
    // The whole root: everything but the denylist, the sparse file by allocation, the
    // hard-linked file once, nothing through the symlinks.
    let whole = measure(&t.home, &o);
    let expected_alloc = (600 + 3 + 5 + 1 + 40 + 700 + 40 * 8 + 4 + 300) * KB4 as u64;
    let sparse_alloc = t.at("Downloads/sparse.img").metadata().unwrap().blocks() * 512;
    assert_eq!(whole.alloc, expected_alloc + sparse_alloc);
    assert_eq!(whole.files, 6 + 41 + 1 + 1);
}

#[test]
fn system_bundles_with_compressed_files_agree() {
    // Read-only system folders (not user data), whose files are often compressed.
    for p in [
        "/System/Applications/Calculator.app",
        "/System/Library/Frameworks/AppKit.framework",
    ] {
        let p = Path::new(p);
        if !p.exists() {
            continue;
        }
        let o = WalkOpts::default();
        let r = measure_reference(p, &o);
        assert!(r.files > 0, "{p:?}");
        assert_eq!(measure_bulk(p, &o, 1), r, "{p:?}");
        assert_eq!(measure_bulk(p, &o, 4), r, "{p:?}");
    }
}

#[test]
fn sparse_file_counts_by_allocation() {
    let t = Tree::new();
    let path = t.at("sparse.img");
    let mut f = File::create(&path).unwrap();
    f.seek(SeekFrom::Start(256 * 1024 * 1024)).unwrap();
    f.write_all(&[1u8; KB4]).unwrap();
    drop(f);
    let s = measure(&path, &WalkOpts::for_root(&t.home));
    assert_eq!(s.logical, 256 * 1024 * 1024 + KB4 as u64);
    assert!(s.alloc < MIN, "{s:?}");
    let scan = scan(&t.env(), 2);
    assert!(scan.items.is_empty(), "{:?}", scan.items);
    assert_eq!(scan.tree.bytes_alloc, s.alloc);
    assert_eq!(scan.tree.bytes_logical, s.logical);
}

#[test]
fn hard_links_count_once() {
    let t = Tree::new();
    t.write("A/x.bin", 400 * KB4);
    std::fs::hard_link(t.at("A/x.bin"), t.at("A/y.bin")).unwrap();
    std::fs::create_dir_all(t.at("B/deep")).unwrap();
    std::fs::hard_link(t.at("A/x.bin"), t.at("B/deep/z.bin")).unwrap();
    t.write("B/own.bin", 300 * KB4);
    let o = WalkOpts::for_root(&t.home);
    assert_eq!(measure(&t.home, &o).files, 2);
    for threads in [1, 4] {
        let s = scan(&t.env(), threads);
        // The first top-level folder by name keeps the file, the first path in it wins.
        assert_eq!(rels(&s.biggest(0)), ["A/x.bin", "B/own.bin"]);
        assert_eq!(s.files_seen, 2);
        assert_eq!(s.tree.bytes_alloc, measure_reference(&t.home, &o).alloc);
        let b = s.tree.find("B").unwrap();
        assert_eq!(b.bytes_alloc, 300 * KB4 as u64);
        assert_eq!(b.files, 1);
        // B/deep held only the link, so it is empty now and folded.
        assert!(s.tree.find("B/deep").is_none());
    }
}

#[test]
fn symlinks_are_not_followed() {
    let t = Tree::new();
    let outside = t.base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("huge.bin"), vec![5u8; 600 * KB4]).unwrap();
    std::os::unix::fs::symlink(&outside, t.at("to-outside")).unwrap();
    std::fs::create_dir_all(t.at("Movies")).unwrap();
    std::os::unix::fs::symlink(outside.join("huge.bin"), t.at("Movies/huge.bin")).unwrap();
    t.write("Movies/real.mov", 300 * KB4);
    let s = scan(&t.env(), 2);
    assert_eq!(rels(&s.biggest(0)), ["Movies/real.mov"]);
    assert_eq!(s.tree.bytes_alloc, 300 * KB4 as u64);
    assert!(s.tree.find("to-outside").is_none());
    assert_eq!(
        measure(&t.at("to-outside"), &WalkOpts::for_root(&t.home)),
        Size::default()
    );
}

#[test]
fn an_app_is_one_item_with_the_total_of_its_contents() {
    let t = Tree::new();
    // 300 files of 4 KB: each is far below the record minimum, the total is above it.
    for i in 0..300 {
        t.write(
            &format!("Applications/Big.app/Contents/Resources/r{i:03}"),
            KB4,
        );
    }
    t.write("Applications/Big.app/Contents/MacOS/Big", 2 * KB4);
    t.write("Applications/Small.app/Contents/MacOS/Small", KB4);
    for i in 0..10 {
        t.write(&format!("Tools/Kit.framework/Versions/A/f{i}"), 30 * KB4);
    }
    let s = scan(&t.env(), 2);
    let all = s.biggest(0);
    assert_eq!(rels(&all), ["Applications/Big.app", "Tools/Kit.framework"]);
    let big = all[0];
    assert_eq!(big.kind, LargeKind::Package);
    assert_eq!(big.name, "Big.app");
    assert_eq!(big.bytes_alloc, 302 * KB4 as u64);
    assert!(!big.app_data);
    // Nothing inside a package is listed, and the package is a leaf in the tree.
    assert!(s.items.iter().all(|i| !i.rel.contains(".app/")));
    let node = s.tree.find("Applications/Big.app").unwrap();
    assert_eq!(node.kind, Some(LargeKind::Package));
    assert!(node.children.is_empty());
    assert_eq!(node.files, 301);
    assert!(large::folder_view(&s, "Applications/Big.app", 0).is_none());
    // The small app is folded into the rest of Applications.
    let apps = s.tree.find("Applications").unwrap();
    assert_eq!((apps.rest_count, apps.rest_alloc), (1, KB4 as u64));
    assert_eq!(s.files_seen, 312);
}

#[test]
fn denylisted_folders_are_never_visited() {
    let t = Tree::new();
    t.write("Library/Containers/com.x/Data/big.bin", 900 * KB4);
    t.write("Library/Group Containers/g/big.bin", 900 * KB4);
    t.write("Library/Mail/V10/big.mbox", 900 * KB4);
    t.write(
        "Library/Application Support/Chrome/Default/Cookies",
        900 * KB4,
    );
    t.write("Library/Application Support/App/store.sqlite", 900 * KB4);
    t.write(".ssh/id_big", 900 * KB4);
    t.write("Library/Application Support/Models/m.gguf", 700 * KB4);
    // Make Containers unreadable: had the scan tried to enter it, it would say it skipped it.
    use std::os::unix::fs::PermissionsExt;
    let containers = t.at("Library/Containers");
    std::fs::set_permissions(&containers, std::fs::Permissions::from_mode(0o000)).unwrap();
    let s = scan(&t.env(), 2);
    std::fs::set_permissions(&containers, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(s.skipped.is_empty(), "{:?}", s.skipped);
    let items = s.biggest(0);
    assert_eq!(rels(&items), ["Library/Application Support/Models/m.gguf"]);
    assert!(items[0].app_data, "items under Library are app data");
    assert_eq!(s.tree.bytes_alloc, 700 * KB4 as u64);
    assert!(s.tree.find("Library/Containers").is_none());
    assert!(s.tree.find(".ssh").is_none());
    let lib = s.tree.find("Library").unwrap();
    assert_eq!(lib.files, 1);
}

#[test]
fn tidy_folders_under_the_root_are_left_out() {
    let t = Tree::new();
    t.write("Movies/m.mov", 300 * KB4);
    // A Tidy home inside the scan root.
    let env = Env::for_tests(&t.home, &t.home.join("tidy-home"));
    std::fs::create_dir_all(&env.paths.data).unwrap();
    std::fs::write(env.paths.data.join("big.bin"), vec![1u8; 600 * KB4]).unwrap();
    std::fs::create_dir_all(&env.paths.cache).unwrap();
    std::fs::write(env.paths.cache.join("big.bin"), vec![1u8; 600 * KB4]).unwrap();
    let s = scan(&env, 1);
    assert_eq!(rels(&s.biggest(0)), ["Movies/m.mov"]);
    assert_eq!(s.tree.bytes_alloc, 300 * KB4 as u64);
}

#[test]
fn threshold_and_not_opened_filters_use_the_clock_and_settings() {
    let t = Tree::new();
    t.write("Movies/old.mov", 800 * KB4);
    t.date("Movies/old.mov", 200);
    t.write("Movies/new.mov", 1200 * KB4);
    t.date("Movies/new.mov", 3);
    t.write("Downloads/edge.zip", 300 * KB4);
    t.date("Downloads/edge.zip", 89);
    t.write("Downloads/past.zip", 260 * KB4);
    t.date("Downloads/past.zip", 91);
    t.write("small.txt", 10 * KB4);
    let settings = Settings {
        large_min_bytes: 3 * MIN,
        ..Settings::default()
    };
    let clock = Arc::new(FixedClock::new(TEST_NOW));
    let env = t.env().with_settings(settings).with_clock(clock.clone());
    let s = scan(&env, 2);
    assert_eq!(s.scanned_at, TEST_NOW);
    assert_eq!(
        rels(&s.biggest(MIN)),
        [
            "Movies/new.mov",
            "Movies/old.mov",
            "Downloads/edge.zip",
            "Downloads/past.zip"
        ]
    );
    // Files mode with the threshold from settings (3 MB) drops the two zips.
    assert_eq!(
        rels(&large::files_in(&s, FilesFilter::Biggest, &env)),
        ["Movies/new.mov", "Movies/old.mov"]
    );
    // Not opened lately: 90 days from settings, now from the clock.
    assert_eq!(
        rels(&large::files_in(&s, FilesFilter::NotOpened, &env)),
        ["Movies/old.mov"]
    );
    assert_eq!(
        rels(&large::files(&s, FilesFilter::NotOpened, MIN, TEST_NOW, 90)),
        ["Movies/old.mov", "Downloads/past.zip"]
    );
    // Dates come from the file times and say so.
    let old = s.items.iter().find(|i| i.rel == "Movies/old.mov").unwrap();
    assert_eq!(old.last_used, Some(TEST_NOW - 200 * DAY));
    assert_eq!(old.used_source, UseSource::FileTimes);
    // Moving the clock 100 days on makes the 3-day-old file old too, without rescanning.
    clock.advance(100 * DAY);
    assert_eq!(
        rels(&large::files_in(&s, FilesFilter::NotOpened, &env)),
        ["Movies/new.mov", "Movies/old.mov"]
    );
    // A shorter "not opened" setting.
    let env = env.with_settings(Settings {
        not_opened_days: 300,
        large_min_bytes: MIN,
        ..Settings::default()
    });
    assert_eq!(
        rels(&large::files_in(&s, FilesFilter::NotOpened, &env)),
        Vec::<String>::new()
    );
}

#[test]
fn folders_mode_drills_down_and_folds_smaller_items() {
    let t = Tree::new();
    t.write("Library/Application Support/Models/a.gguf", 700 * KB4);
    t.write("Library/Application Support/Models/b.gguf", 400 * KB4);
    for i in 0..5 {
        t.write(&format!("Library/Application Support/Models/tiny{i}"), KB4);
    }
    t.write("Library/Application Support/Other/o.bin", 260 * KB4);
    t.write("Library/Caches/c.bin", 10 * KB4);
    t.write("Movies/m.mov", 300 * KB4);
    let s = scan(&t.env(), 3);
    assert_eq!(s.tree.name, "Home");
    assert_eq!(
        s.tree.bytes_alloc,
        (700 + 400 + 5 + 260 + 10 + 300) * KB4 as u64
    );
    let home = large::folder_view(&s, "", MIN).unwrap();
    assert_eq!(
        home.rows
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        ["Library", "Movies"]
    );
    let models = large::folder_view(&s, "Library/Application Support/Models", 2 * MIN).unwrap();
    assert_eq!(
        models
            .crumbs
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["Home", "Library", "Application Support", "Models"]
    );
    assert_eq!(models.crumbs[2].rel, "Library/Application Support");
    // a.gguf (2.8 MB) is shown; b.gguf (1.6 MB) and the five tiny files fold into one row.
    assert_eq!(
        models
            .rows
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        ["a.gguf"]
    );
    assert_eq!(models.smaller_count, 6);
    assert_eq!(models.smaller_bytes, (400 + 5) * KB4 as u64);
    assert_eq!(models.node.bytes_alloc, (700 + 400 + 5) * KB4 as u64);
    let support = large::folder_view(&s, "Library/Application Support", MIN).unwrap();
    assert_eq!(
        support
            .rows
            .iter()
            .map(|n| (n.name.as_str(), n.bytes_alloc))
            .collect::<Vec<_>>(),
        [
            ("Models", (700 + 400 + 5) * KB4 as u64),
            ("Other", 260 * KB4 as u64)
        ]
    );
    // Library/Caches (40 KB) is below the record minimum, so the tree folded it.
    assert!(large::folder_view(&s, "Library/Caches", 0).is_none());
    let lib = s.tree.find("Library").unwrap();
    assert_eq!((lib.rest_count, lib.rest_alloc), (1, 10 * KB4 as u64));
    assert_eq!(
        large::parent_rel("Library/Application Support").as_deref(),
        Some("Library")
    );
    // Every kept folder total matches the reference walker (no hard links here).
    let o = WalkOpts::for_root(&t.home);
    let mut stack = vec![&s.tree];
    while let Some(n) = stack.pop() {
        if n.kind == Some(LargeKind::Folder) {
            assert_eq!(
                n.bytes_alloc,
                measure_reference(&t.home.join(&n.rel), &o).alloc,
                "{}",
                n.rel
            );
        }
        stack.extend(n.children.iter());
    }
}

#[test]
fn the_varied_tree_scans_the_same_on_any_thread_count() {
    let t = varied_tree();
    let one = scan(&t.env(), 1);
    for threads in [2, 4, 8] {
        assert_eq!(scan(&t.env(), threads), one, "{threads} threads");
    }
    assert_eq!(
        rels(&one.biggest(0)),
        [
            "Library/Application Support/Models/m.gguf",
            "Movies/trip.mov",
            "Applications/Big.app",
            "Archive/pair-a.bin",
        ]
    );
    let o = WalkOpts::for_root(&t.home);
    assert_eq!(one.tree.bytes_alloc, measure_reference(&t.home, &o).alloc);
    assert_eq!(one.tree.files, measure_reference(&t.home, &o).files);
    assert_eq!(one.files_seen, one.tree.files);
    assert!(one.skipped.is_empty(), "{:?}", one.skipped);
}

#[test]
fn the_scan_saves_and_loads() {
    let t = varied_tree();
    let env = t.env();
    assert!(large::load(&env.paths).is_none());
    let s = scan(&env, 2);
    large::save(&env.paths, &s).unwrap();
    assert!(env.paths.last_large_file().starts_with(&env.paths.cache));
    assert_eq!(large::load(&env.paths), Some(s));
    std::fs::write(env.paths.last_large_file(), b"{not json").unwrap();
    assert!(large::load(&env.paths).is_none());
}

#[test]
fn cancel_returns_none_and_progress_names_the_top_folder() {
    let t = varied_tree();
    let env = t.env();
    let cancel = Arc::new(AtomicBool::new(true));
    let opts = LargeOpts {
        cancel: Some(cancel.clone()),
        ..LargeOpts::with_record_min(MIN)
    };
    assert!(large::scan(&env, &opts).is_none());
    cancel.store(false, Ordering::Relaxed);
    let seen: Arc<Mutex<Vec<WalkProgress>>> = Arc::default();
    let sink = seen.clone();
    let opts = LargeOpts {
        progress: Some(ProgressFn::new(move |p| {
            sink.lock().unwrap().push(p.clone())
        })),
        threads: 2,
        ..opts
    };
    let s = large::scan(&env, &opts).unwrap();
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|p| p.current.as_deref() == Some(t.at("Movies").as_path()))
    );
    assert!(seen.iter().map(|p| p.files).max().unwrap() >= s.files_seen);
}

#[test]
fn an_unreadable_folder_is_reported_and_its_siblings_still_count() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tree::new();
    t.write("Documents/locked/a.bin", 300 * KB4);
    t.write("Movies/m.mov", 300 * KB4);
    let locked = t.at("Documents/locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let s = scan(&t.env(), 2);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(rels(&s.biggest(0)), ["Movies/m.mov"]);
    assert_eq!(s.skipped, ["Skipped Documents/locked (access not allowed)"]);
    assert!(s.tree.find("Documents/locked").is_none());
}

#[test]
fn a_missing_root_gives_an_empty_scan() {
    let t = Tree::new();
    let env = Env::for_tests(&t.base.join("nope"), &t.base.join("tidy"));
    let s = scan(&env, 2);
    assert!(s.items.is_empty());
    assert_eq!(s.tree.bytes_alloc, 0);
    assert_eq!(s.skipped.len(), 1);
}
