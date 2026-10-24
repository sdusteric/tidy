//! The standard fixture end to end: scan it, compare the plan with the fixture's `Expected`
//! exactly, clean the safe items into the folder Trash, and check that nothing else in the
//! fake home changed. Everything runs in a fresh temp folder with `Env::for_tests` (fixed
//! clock, fixed process list, `DirTrash`); nothing touches the real home or the real Trash.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tidy_core::api::Event;
use tidy_core::clean::{clean, empty_tidy_trash};
use tidy_core::cleanup::scan_cleanup;
use tidy_core::env::{Env, FixedClock, FixedProbe};
use tidy_core::history;
use tidy_core::model::{Action, Class, ItemResult, Plan, RunningSet, Source, TrasherKind};
use tidy_fixtures::{Expected, ExpectedItem, FIXTURE_NOW, FakeHome};

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    expected: Expected,
    env: Env,
    probe: Arc<FixedProbe>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap().join("fx");
    let expected = FakeHome::standard(&dir, FIXTURE_NOW).unwrap();
    let home = dir.join("home");
    let probe = Arc::new(FixedProbe::new(RunningSet::new(&expected.plan.running)));
    let env = Env::for_tests(&home, &dir.join("tidy"))
        .with_clock(Arc::new(FixedClock::new(expected.plan.now)))
        .with_procs(probe.clone());
    Fixture {
        _tmp: tmp,
        home,
        expected,
        env,
        probe,
    }
}

fn scan(env: &Env) -> Plan {
    scan_cleanup(env, &|_| {}, &Arc::new(AtomicBool::new(false)))
}

/// The plan in the shape of `expected-plan.json`.
fn as_expected(plan: &Plan) -> Vec<ExpectedItem> {
    plan.items
        .iter()
        .map(|i| ExpectedItem {
            id: i.id.clone(),
            group: i.group,
            class: i.class,
            ticked: i.ticked,
            blocked: i.blocked.clone(),
            bytes_alloc: i.bytes_alloc,
        })
        .collect()
}

/// Every path under `root` (relative, not following symlinks) with its kind and length.
fn listing(root: &Path) -> BTreeMap<String, (char, u64)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = path.symlink_metadata().unwrap();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let kind = if meta.file_type().is_symlink() {
                'l'
            } else if meta.is_dir() {
                stack.push(path.clone());
                'd'
            } else {
                'f'
            };
            out.insert(rel, (kind, if kind == 'f' { meta.len() } else { 0 }));
        }
    }
    out
}

fn under(path: &str, item: &str) -> bool {
    path == item || path.starts_with(&format!("{item}/"))
}

#[test]
fn the_plan_equals_the_expected_plan_exactly() {
    let f = fixture();
    let plan = scan(&f.env);
    assert_eq!(as_expected(&plan), f.expected.plan.items);
    // Nothing excluded or declined appears anywhere in the plan.
    let json = serde_json::to_string(&plan).unwrap();
    for never in &f.expected.plan.never {
        assert!(!json.contains(never.as_str()), "{never} in the plan");
    }
}

#[test]
fn clean_moves_exactly_the_safe_set_and_nothing_else() {
    let f = fixture();
    let plan = scan(&f.env);
    let before = listing(&f.home);
    let safe: Vec<&str> = f
        .expected
        .plan
        .clean_set()
        .iter()
        .map(|i| i.rel())
        .collect();
    assert!(!safe.is_empty());
    let ticked: Vec<_> = plan.ticked().cloned().collect();
    assert_eq!(
        ticked.iter().map(|i| i.rel_path()).collect::<Vec<_>>(),
        safe
    );

    let mut events = Vec::new();
    let entry = clean(&f.env, &ticked, &mut |e| events.push(e));

    // Every item reached the folder Trash.
    let trashed: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::ItemTrashed { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(trashed.len(), safe.len(), "{events:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::ItemSkipped { .. }))
    );

    // The home differs from before only by the safe items and what was inside them.
    let after = listing(&f.home);
    let removed: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let added: Vec<&String> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    assert!(added.is_empty(), "{added:?}");
    for path in &removed {
        assert!(
            safe.iter().any(|s| under(path, s)),
            "{path} moved but is not a safe item"
        );
    }
    for s in &safe {
        assert!(
            removed.iter().any(|p| p.as_str() == *s),
            "{s} is still there"
        );
    }
    for (path, meta) in &after {
        assert_eq!(before.get(path), Some(meta), "{path} changed");
    }

    // What is in the folder Trash is exactly those items, by name.
    let trash = f.env.paths.dir_trash();
    let mut in_trash: Vec<String> = std::fs::read_dir(&trash)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    in_trash.sort();
    let mut names: Vec<String> = safe
        .iter()
        .map(|s| s.rsplit('/').next().unwrap().to_string())
        .collect();
    names.sort();
    assert_eq!(in_trash, names);

    // One clean entry, folder Trash, per-item bytes equal to the plan.
    assert_eq!(history::read(&f.env.paths), std::slice::from_ref(&entry));
    assert_eq!(entry.action, Action::Clean);
    assert_eq!(entry.source, Source::Cleanup);
    assert_eq!(entry.trasher, TrasherKind::Dir);
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"trasher\":\"dir\""));
    assert_eq!(entry.items.len(), safe.len());
    for (item, exp) in entry.items.iter().zip(f.expected.plan.clean_set()) {
        assert_eq!(item.path, f.home.join(exp.rel()));
        assert_eq!(item.bytes, exp.bytes_alloc, "{}", exp.id);
        assert_eq!(item.rule, exp.rule_id());
        assert_eq!(item.result, ItemResult::Trashed);
        let tp = item.trash_path.as_ref().unwrap();
        assert!(tp.starts_with(&trash), "{tp:?}");
        assert!(tp.symlink_metadata().is_ok());
    }
    let total: u64 = f
        .expected
        .plan
        .clean_set()
        .iter()
        .map(|i| i.bytes_alloc)
        .sum();
    assert_eq!(entry.trashed_bytes, total);

    // Empty Trash deletes those items and only inside the folder Trash.
    let emptied = empty_tidy_trash(&f.env);
    assert_eq!(emptied.action, Action::EmptyTrash);
    assert_eq!(emptied.trashed_bytes, total);
    assert!(
        emptied
            .items
            .iter()
            .all(|i| i.result == ItemResult::Deleted)
    );
    assert_eq!(std::fs::read_dir(&trash).unwrap().count(), 0);
    assert_eq!(listing(&f.home), after);
}

#[test]
fn an_owner_that_starts_between_scan_and_clean_is_skipped() {
    let f = fixture();
    let plan = scan(&f.env);
    let idle = "cache.app:Library/Caches/com.example.idle";
    assert!(plan.item(idle).unwrap().ticked);
    // Idle starts after the scan.
    let mut running = f.expected.plan.running.clone();
    running.push("com.example.idle".into());
    f.probe.set(RunningSet::new(&running));

    let ticked: Vec<_> = plan.ticked().cloned().collect();
    let mut events = Vec::new();
    let entry = clean(&f.env, &ticked, &mut |e| events.push(e));

    let skipped: Vec<(&str, &str)> = events
        .iter()
        .filter_map(|e| match e {
            Event::ItemSkipped { id, reason } => Some((id.as_str(), reason.as_str())),
            _ => None,
        })
        .collect();
    // Idle owns its cache and its logs; both stay.
    let idle_log = "logs.app:Library/Logs/Idle/old.log";
    assert_eq!(
        skipped,
        [
            (idle, "Idle opened, skipped"),
            (idle_log, "Idle opened, skipped")
        ]
    );
    assert!(f.home.join("Library/Caches/com.example.idle").is_dir());
    assert!(f.home.join("Library/Logs/Idle/old.log").is_file());
    let rec = entry
        .items
        .iter()
        .find(|i| i.path == f.home.join("Library/Caches/com.example.idle"))
        .unwrap();
    assert_eq!(rec.result, ItemResult::Skipped);
    assert_eq!(rec.trash_path, None);
    // The others still moved.
    assert_eq!(entry.trashed().count(), ticked.len() - 2);
    assert_eq!(entry.skipped(), 2);
    // Chrome's cache was never ticked and is still there.
    assert!(f.home.join("Library/Caches/Google/Chrome/Default").is_dir());
    assert!(
        plan.items
            .iter()
            .filter(|i| i.class != Class::Safe)
            .all(|i| i.path.exists())
    );
}
