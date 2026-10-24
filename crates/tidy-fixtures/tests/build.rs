//! Builds the standard fixture into temp folders and checks it against its own ledger:
//! allocated sizes read back with `st_blocks * 512` (this proves the exact-size assumption on
//! the machine running the tests), dates are as set, the build is deterministic and the
//! expected results agree with the tree. Every build goes into a `tempfile` folder.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use tidy_core::model::{Class, Group, LargeKind};
use tidy_fixtures::standard::{BLOCKED_CHROME, MIB, rule};
use tidy_fixtures::{
    Expected, FIXTURE_NOW, FakeHome, Ledger, NodeKind, Tree, check_target_dir_in, tree,
};

const DAY: u64 = 86_400;

fn build(now: u64) -> (tempfile::TempDir, Expected, Ledger) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("fx");
    let (expected, ledger) = FakeHome::standard_with_ledger(&dir, now).unwrap();
    (tmp, expected, ledger)
}

#[test]
fn allocated_sizes_equal_declared_sizes() {
    let (_tmp, _expected, ledger) = build(FIXTURE_NOW);
    let mut files = 0;
    for n in ledger.nodes().filter(|n| n.kind == NodeKind::File) {
        let meta = fs::symlink_metadata(ledger.path(&n.rel)).unwrap();
        assert_eq!(meta.len(), n.logical, "{}: logical length", n.rel);
        assert_eq!(
            meta.blocks() * 512,
            n.alloc,
            "{}: allocated bytes (st_blocks * 512); this volume does not give the exact sizes \
             the fixtures assume",
            n.rel
        );
        files += 1;
    }
    assert!(files > 100, "only {files} files built");
}

#[test]
fn sparse_file_has_large_length_and_no_blocks() {
    let (_tmp, _expected, ledger) = build(FIXTURE_NOW);
    let meta = fs::symlink_metadata(ledger.path("Documents/sparse.img")).unwrap();
    assert_eq!(meta.len(), 64 * MIB);
    assert_eq!(meta.blocks(), 0);
}

#[test]
fn hard_links_share_one_inode_and_count_once() {
    let (_tmp, _expected, ledger) = build(FIXTURE_NOW);
    let a = fs::metadata(ledger.path("Pictures/pair/original.bin")).unwrap();
    let b = fs::metadata(ledger.path("Pictures/pair/linked.bin")).unwrap();
    assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
    assert_eq!(a.nlink(), 2);
    assert_eq!(ledger.alloc_under("Pictures/pair"), 3 * MIB);
    // Every other file has its own inode.
    let mut inodes = BTreeMap::new();
    for n in ledger.nodes().filter(|n| n.kind == NodeKind::File) {
        let m = fs::symlink_metadata(ledger.path(&n.rel)).unwrap();
        *inodes.entry((m.dev(), m.ino())).or_insert(0) += 1;
    }
    assert_eq!(inodes.values().filter(|c| **c > 1).count(), 1);
}

#[test]
fn symlink_points_outside_and_nothing_sits_below_it() {
    let (tmp, _expected, ledger) = build(FIXTURE_NOW);
    let link = ledger.path("Documents/linkdir");
    let meta = fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink());
    assert_eq!(fs::read_link(&link).unwrap(), tmp.path().join("fx/outside"));
    assert!(tmp.path().join("fx/outside/huge-outside.bin").is_file());
    assert!(ledger.under("Documents/linkdir").count() == 1);
}

#[test]
fn dates_are_as_set() {
    let now = FIXTURE_NOW;
    let (_tmp, _expected, ledger) = build(now);
    for n in ledger.nodes() {
        let meta = fs::symlink_metadata(ledger.path(&n.rel)).unwrap();
        assert_eq!(meta.mtime() as u64, n.mtime, "{}: mtime", n.rel);
        // No test above or here has read file contents, so access times are still as set.
        assert_eq!(meta.atime() as u64, n.mtime, "{}: atime", n.rel);
    }
    let age = |rel: &str| now - ledger.get(rel).unwrap().mtime;
    assert_eq!(age("Downloads/old-installer.dmg"), 200 * DAY);
    assert_eq!(age("Movies/trip.mov"), 3 * DAY);
    assert_eq!(age("Library/Logs/Idle/old.log"), 20 * DAY);
    assert_eq!(age("Library/Logs/Idle/new.log"), 3 * DAY);
    assert_eq!(
        age("Library/Logs/DiagnosticReports/crash-old.ips"),
        45 * DAY
    );
    assert_eq!(age("Documents/projects/active-rs/src/main.rs"), DAY);
    for f in ["part-0.bin", "part-1.bin"] {
        let rel = format!("Library/Developer/Xcode/DerivedData/OldProj-abc/{f}");
        assert_eq!(age(&rel), 10 * DAY);
    }
    // Everything not given a date is 40 days old: past the 30-day project rule, inside the
    // 90-day "not opened" window.
    assert_eq!(age("Library/Caches/com.example.idle/part-0.bin"), 40 * DAY);
    assert_eq!(age("Documents/projects/idle-rs/Cargo.toml"), 40 * DAY);
    // Only the 200-day file is older than 90 days.
    let old: Vec<&str> = ledger
        .nodes()
        .filter(|n| n.kind == NodeKind::File && now - n.mtime > 90 * DAY)
        .map(|n| n.rel.as_str())
        .collect();
    assert_eq!(old, ["Downloads/old-installer.dmg"]);
}

#[test]
fn build_is_deterministic() {
    let (_t1, e1, l1) = build(FIXTURE_NOW);
    let (_t2, e2, l2) = build(FIXTURE_NOW);
    assert_eq!(e1, e2);
    let n1: Vec<_> = l1.nodes().collect();
    let n2: Vec<_> = l2.nodes().collect();
    assert_eq!(n1, n2);
    // Metadata first, then contents: reading a file can move its access time.
    for n in &n1 {
        let a = fs::symlink_metadata(l1.path(&n.rel)).unwrap();
        let b = fs::symlink_metadata(l2.path(&n.rel)).unwrap();
        assert_eq!(a.len(), b.len(), "{}", n.rel);
        assert_eq!(a.mtime(), b.mtime(), "{}", n.rel);
        assert_eq!(a.blocks(), b.blocks(), "{}", n.rel);
        assert_eq!(a.file_type().is_dir(), b.file_type().is_dir(), "{}", n.rel);
    }
    for n in n1.iter().filter(|n| n.kind == NodeKind::File) {
        assert_eq!(
            fs::read(l1.path(&n.rel)).unwrap(),
            fs::read(l2.path(&n.rel)).unwrap(),
            "{}: content",
            n.rel
        );
    }
}

#[test]
fn pattern_is_non_zero_and_differs_per_file() {
    let a = tree::fill_pattern("a", 8192);
    let b = tree::fill_pattern("b", 8192);
    assert!(a.iter().all(|x| *x != 0));
    assert_ne!(a, b);
    assert_eq!(a, tree::fill_pattern("a", 8192));
    // Not repeating: the two halves differ.
    assert_ne!(a[..4096], a[4096..]);
    // Spread over the byte range, not a short cycle.
    let distinct = a.iter().collect::<std::collections::BTreeSet<_>>().len();
    assert!(distinct > 200, "{distinct} distinct byte values");
}

#[test]
fn fake_apps_have_plists_with_their_bundle_ids() {
    let (_tmp, _expected, ledger) = build(FIXTURE_NOW);
    let idle =
        fs::read_to_string(ledger.path("Applications/Idle.app/Contents/Info.plist")).unwrap();
    assert!(idle.contains("<string>com.example.idle</string>"));
    let big = fs::read_to_string(ledger.path("Applications/Big.app/Contents/Info.plist")).unwrap();
    assert!(big.contains("<string>com.example.big</string>"));
    // The safe cache is named after the bundle id that Idle.app declares.
    assert!(ledger.contains("Library/Caches/com.example.idle"));
}

#[test]
fn expected_results_agree_with_the_tree() {
    let (_tmp, expected, ledger) = build(FIXTURE_NOW);
    let plan = &expected.plan;

    // Items exist, ids are rule:rel, bytes are non-zero and at least 1 MB.
    for i in &plan.items {
        assert_eq!(i.id, format!("{}:{}", i.rule_id(), i.rel()));
        assert!(ledger.contains(i.rel()), "{}", i.id);
        assert!(i.bytes_alloc >= 1_000_000, "{}: {}", i.id, i.bytes_alloc);
        assert_eq!(i.bytes_alloc, ledger.alloc_under(i.rel()));
    }
    let mut ids: Vec<&str> = plan.items.iter().map(|i| i.id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 12);

    // Sorted like Plan::sort.
    let sorted = plan.items.windows(2).all(|w| {
        (w[0].group, std::cmp::Reverse(w[0].bytes_alloc), &w[0].id)
            <= (w[1].group, std::cmp::Reverse(w[1].bytes_alloc), &w[1].id)
    });
    assert!(sorted);

    // Classes, ticks and the one blocked item.
    let by_rule = |r: &str| plan.items.iter().find(|i| i.rule_id() == r).unwrap();
    let chrome = by_rule(rule::CACHE_CHROME);
    assert_eq!(chrome.class, Class::Safe);
    assert!(!chrome.ticked);
    assert_eq!(chrome.blocked.as_deref(), Some(BLOCKED_CHROME));
    assert_eq!(plan.items.iter().filter(|i| i.blocked.is_some()).count(), 1);
    assert_eq!(by_rule(rule::CACHE_UNKNOWN).class, Class::Review);
    assert_eq!(by_rule(rule::BUILD_OUTPUT).class, Class::Review);
    assert_eq!(by_rule(rule::HUGGINGFACE).group, Group::AiModels);
    assert_eq!(by_rule(rule::VM_DISK).class, Class::ReportOnly);
    for i in &plan.items {
        assert_eq!(
            i.ticked,
            i.class == Class::Safe && i.blocked.is_none(),
            "{}",
            i.id
        );
    }
    assert_eq!(plan.clean_set().len(), 7);

    // Nothing in `never` is an item or sits under one, and each exists in the tree (except
    // the bare file name that lives outside the home).
    for n in &plan.never {
        for i in &plan.items {
            assert!(
                !(i.rel() == n
                    || i.rel().starts_with(&format!("{n}/"))
                    || n.starts_with(&format!("{}/", i.rel()))),
                "{n} overlaps item {}",
                i.id
            );
        }
        if n != "huge-outside.bin" {
            assert!(ledger.contains(n), "{n} is not in the tree");
        }
    }

    // Large files: the four items, the exact set, largest first.
    let large = &expected.large;
    let rels: Vec<&str> = large.items.iter().map(|i| i.rel.as_str()).collect();
    assert_eq!(
        rels,
        [
            "Movies/trip.mov",
            "Library/Application Support/FixtureStudio/models/big.gguf",
            "Applications/Big.app",
            "Downloads/old-installer.dmg",
        ]
    );
    for i in &large.items {
        assert!(i.bytes_alloc >= large.min_bytes);
        assert_eq!(i.app_data, i.rel.starts_with("Library/"));
    }
    let big = large.items.iter().find(|i| i.name == "Big.app").unwrap();
    assert_eq!(big.kind, LargeKind::Package);
    assert_eq!(big.bytes_alloc, 64 * 96 * 1024 + 2 * 4096);
    // Every other file in the tree is under the threshold or outside what is scanned.
    for n in ledger.nodes().filter(|n| n.kind == NodeKind::File) {
        let inside_item = large
            .items
            .iter()
            .any(|i| n.rel == i.rel || n.rel.starts_with(&format!("{}/", i.rel)));
        let denied = large
            .never
            .iter()
            .any(|d| n.rel == *d || n.rel.starts_with(&format!("{d}/")));
        if !inside_item && !denied {
            assert!(n.alloc < large.min_bytes, "{} would be a large file", n.rel);
        }
    }
    assert_eq!(large.not_opened, ["Downloads/old-installer.dmg"]);

    // Folder totals.
    let folder = |rel: &str| large.folders.iter().find(|f| f.rel == rel).unwrap();
    assert_eq!(folder("Pictures/pair").bytes_alloc, 3 * MIB);
    assert_eq!(folder("Pictures/pair").bytes_logical, 3 * MIB);
    // The denylisted folders, plus Tidy's own Caches and Logs folders (2 MiB each).
    let denied_bytes = 10 * MIB + 5 * MIB + 4096 + 2 * MIB + 2 * MIB + 2 * MIB;
    let all = ledger.alloc_under("");
    assert_eq!(folder("").bytes_alloc, all - denied_bytes);
    // The sparse file adds no allocation but 64 MiB of length.
    let docs = folder("Documents");
    assert_eq!(docs.bytes_alloc, ledger.alloc_under("Documents"));
    assert!(
        docs.bytes_logical >= docs.bytes_alloc - ledger_padding(&ledger, "Documents") + 63 * MIB
    );
    assert!(ledger.under("Library/Containers").count() > 1);
    assert!(
        !large
            .folders
            .iter()
            .any(|f| f.rel.starts_with("Library/Containers"))
    );
}

/// Bytes of padding in text files under `rel`: allocated minus logical, sparse excluded.
fn ledger_padding(ledger: &Ledger, rel: &str) -> u64 {
    ledger
        .under(rel)
        .filter(|n| n.kind == NodeKind::File && !n.dup && n.alloc >= n.logical)
        .map(|n| n.alloc - n.logical)
        .sum()
}

#[test]
fn tree_builder_rejects_bad_input() {
    let tmp = tempfile::tempdir().unwrap();
    let mut t = Tree::new(tmp.path().join("t"), FIXTURE_NOW).unwrap();
    assert!(t.file("a.bin", 1000).is_err(), "not a multiple of 4096");
    assert!(t.file("a.bin", 0).is_err());
    assert!(t.file("../escape.bin", 4096).is_err());
    assert!(t.file("/abs.bin", 4096).is_err());
    t.file("a.bin", 4096).unwrap();
    assert!(t.file("a.bin", 4096).is_err(), "already exists");
    assert!(t.file("a.bin/inner", 4096).is_err(), "parent is a file");
    assert!(t.hard_link("missing", "b.bin").is_err());
    assert!(t.app("Foo", "x.y", "Foo").is_err());
    assert!(t.age_days("missing", 1).is_err());
    t.marker("empty").unwrap();
    let ledger = t.finish().unwrap();
    let meta = fs::metadata(ledger.path("empty")).unwrap();
    assert_eq!((meta.len(), meta.blocks()), (0, 0));
}

#[test]
fn target_dir_check_refuses_protected_and_non_empty_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    for name in ["Documents", "Desktop", "Downloads", "Other"] {
        fs::create_dir_all(home.join(name)).unwrap();
    }
    for name in ["Documents", "Desktop", "Downloads"] {
        let under = home.join(name).join("fx");
        assert!(check_target_dir_in(&under, Some(&home)).is_err(), "{name}");
        // Also through `..` and through a symlink.
        let dotted = home.join("Other/../").join(name).join("fx");
        assert!(
            check_target_dir_in(&dotted, Some(&home)).is_err(),
            "{name} via .."
        );
        let link = tmp.path().join(format!("link-{name}"));
        std::os::unix::fs::symlink(home.join(name), &link).unwrap();
        assert!(
            check_target_dir_in(&link.join("fx"), Some(&home)).is_err(),
            "{name} via link"
        );
    }
    // A fresh folder elsewhere under the home is fine, and so is a missing one.
    assert!(check_target_dir_in(&home.join("Other/fx"), Some(&home)).is_ok());
    assert!(check_target_dir_in(&home.join("Other"), Some(&home)).is_ok());
    // A non-empty folder is refused, a file is refused.
    fs::write(home.join("Other/x"), b"x").unwrap();
    assert!(check_target_dir_in(&home.join("Other"), Some(&home)).is_err());
    assert!(check_target_dir_in(&home.join("Other/x"), Some(&home)).is_err());
    // The standard builder applies the same check.
    assert!(FakeHome::standard(&home.join("Other"), FIXTURE_NOW).is_err());
}

fn run_bin(args: &[&str], home: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tidy-fixture-home"))
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap()
}

#[test]
fn binary_builds_the_fixture_and_writes_expected_files() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_home = tmp.path().join("h");
    fs::create_dir_all(fake_home.join("Documents")).unwrap();
    let dir = tmp.path().join("d");
    let out = run_bin(&[dir.to_str().unwrap()], &fake_home);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    let line: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(line["test_now"], FIXTURE_NOW);
    assert_eq!(line["env"]["TIDY_TEST_NOW"], FIXTURE_NOW.to_string());
    assert_eq!(line["env"]["TIDY_TEST_RUNNING"], "com.google.Chrome");
    assert_eq!(line["env"]["TIDY_HOME"], dir.join("tidy").to_str().unwrap());
    assert_eq!(
        line["env"]["TIDY_SCAN_ROOT"],
        dir.join("home").to_str().unwrap()
    );
    assert!(dir.join("tidy").is_dir());
    assert!(dir.join("home/Applications/Big.app").is_dir());

    let plan: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("expected-plan.json")).unwrap()).unwrap();
    assert_eq!(plan["items"].as_array().unwrap().len(), 12);
    assert_eq!(plan["now"], FIXTURE_NOW);
    let large: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("expected-large.json")).unwrap())
            .unwrap();
    assert_eq!(large["items"].as_array().unwrap().len(), 4);
    assert_eq!(large["min_bytes"], 4_000_000);

    // The same folder again: not empty, refused, exit 1, nothing changed.
    let again = run_bin(&[dir.to_str().unwrap()], &fake_home);
    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stderr).contains("not empty"));

    // A folder under HOME/Documents is refused.
    let bad = fake_home.join("Documents/fx");
    let refused = run_bin(&[bad.to_str().unwrap()], &fake_home);
    assert_eq!(refused.status.code(), Some(1));
    assert!(!bad.exists());

    // Bad usage.
    assert_eq!(run_bin(&[], &fake_home).status.code(), Some(2));
    assert_eq!(run_bin(&["a", "b"], &fake_home).status.code(), Some(2));
    assert_eq!(run_bin(&["--now"], &fake_home).status.code(), Some(2));
}

#[test]
fn binary_honors_now() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("d");
    let out = run_bin(&[dir.to_str().unwrap(), "--now", "1700000000"], tmp.path());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta = fs::metadata(dir.join("home/Downloads/old-installer.dmg")).unwrap();
    assert_eq!(meta.mtime() as u64, 1_700_000_000 - 200 * DAY);
}
