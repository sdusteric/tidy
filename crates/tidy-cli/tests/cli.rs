//! The built `tidyctl` against the standard fixture, checking M1 acceptance criteria 2, 3
//! and 4 (design section 13): the exact plan, a clean that moves exactly the safe set into the
//! folder Trash, the refusal without `TIDY_HOME`, and the exact large-file results.

mod common;

use common::{Fixture, listing, names, pick, under};
use serde_json::Value;
use tidy_core::InstanceLock;

const PLAN_KEYS: [&str; 6] = ["id", "group", "class", "ticked", "blocked", "bytes_alloc"];
const LARGE_KEYS: [&str; 6] = [
    "rel",
    "name",
    "kind",
    "bytes_alloc",
    "bytes_logical",
    "app_data",
];

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn scan_prints_exactly_the_expected_plan() {
    let f = Fixture::standard();
    let run = f.tidyctl(&["scan"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let plan = run.last();
    assert_eq!(plan["event"], "plan");
    let expected = f.read_json("expected-plan.json");
    let got: Vec<Value> = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| pick(i, &PLAN_KEYS))
        .collect();
    assert_eq!(Value::Array(got), expected["items"]);
    // No excluded, declined or denylisted path appears anywhere in the output.
    for never in strings(&expected["never"]) {
        assert!(!run.stdout.contains(&never), "{never} in the output");
    }
    // Every line before the plan is progress.
    assert!(
        run.lines[..run.lines.len() - 1]
            .iter()
            .all(|l| l["event"] == "progress")
    );
}

#[test]
fn clean_safe_moves_exactly_the_safe_set() {
    let f = Fixture::standard();
    let before = listing(&f.home);
    let run = f.tidyctl(&["clean", "--safe"]);
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    let done = run.last();
    assert_eq!(done["event"], "done");
    assert_eq!(done["action"], "clean");
    assert_eq!(done["trasher"], "dir");
    assert!(run.events("skipped").is_empty());

    let safe = f.expected.plan.clean_set();
    let safe_rels: Vec<&str> = safe.iter().map(|i| i.rel()).collect();
    assert_eq!(run.events("trashed").len(), safe.len());

    // The folder Trash holds exactly the safe items.
    let mut want: Vec<String> = safe_rels
        .iter()
        .map(|r| r.rsplit('/').next().unwrap().to_string())
        .collect();
    want.sort();
    assert_eq!(names(&f.dir_trash()), want);

    // The fake home differs only by those paths and what was inside them.
    let after = listing(&f.home);
    let removed: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    assert!(
        after.keys().all(|k| before.contains_key(k)),
        "something was added"
    );
    for p in &removed {
        assert!(safe_rels.iter().any(|s| under(p, s)), "{p} moved");
    }
    for s in &safe_rels {
        assert!(removed.iter().any(|p| p.as_str() == *s), "{s} not moved");
    }
    for (p, meta) in &after {
        assert_eq!(before.get(p), Some(meta), "{p} changed");
    }

    // One clean entry in history.jsonl, folder Trash, with each item's bytes.
    let text = std::fs::read_to_string(f.history_file()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("\"trasher\":\"dir\""), "{}", lines[0]);
    let entry: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(entry["action"], "clean");
    let items = entry["items"].as_array().unwrap();
    assert_eq!(items.len(), safe.len());
    for (item, exp) in items.iter().zip(&safe) {
        assert_eq!(
            item["path"].as_str().unwrap(),
            f.home.join(exp.rel()).to_str().unwrap()
        );
        assert_eq!(item["bytes"], exp.bytes_alloc, "{}", exp.id);
        assert_eq!(item["result"], "trashed");
        assert!(
            item["trash_path"]
                .as_str()
                .unwrap()
                .starts_with(f.dir_trash().to_str().unwrap())
        );
    }
    let total: u64 = safe.iter().map(|i| i.bytes_alloc).sum();
    assert_eq!(entry["trashed_bytes"], total);

    // history reads it back and counts what is still in the Trash.
    let hist = f.tidyctl(&["history"]);
    assert_eq!(hist.code, 0);
    assert_eq!(hist.events("history").len(), 1);
    assert_eq!(hist.last()["in_trash_items"], safe.len());
    assert_eq!(hist.last()["in_trash_bytes"], total);

    // Empty Trash deletes those items only.
    let emptied = f.tidyctl(&["empty-trash"]);
    assert_eq!(emptied.code, 0, "{}", emptied.stdout);
    assert_eq!(emptied.last()["action"], "empty_trash");
    assert_eq!(emptied.last()["trashed_bytes"], total);
    assert!(names(&f.dir_trash()).is_empty());
    assert_eq!(listing(&f.home), after);
    let hist = f.tidyctl(&["history", "--last", "1"]);
    assert_eq!(hist.events("history").len(), 1);
    assert_eq!(hist.events("history")[0]["action"], "empty_trash");
    assert_eq!(hist.last()["in_trash_items"], 0);
}

#[test]
fn destructive_commands_refuse_without_tidy_home() {
    let f = Fixture::standard();
    let before = listing(&f.home);
    // The hooks without TIDY_HOME, and HOME at an empty temp folder.
    let vars: Vec<(String, String)> = f
        .test_env()
        .into_iter()
        .filter(|(k, _)| k != "TIDY_HOME")
        .collect();
    let target = f.home.join("Movies/trip.mov");
    for args in [
        vec!["clean", "--safe"],
        vec![
            "clean",
            "--ids",
            "cache.app:Library/Caches/com.example.idle",
        ],
        vec!["trash", target.to_str().unwrap()],
        vec!["empty-trash"],
        vec!["uninstall", "com.example.box"],
    ] {
        let run = f.tidyctl_with(&args, &vars);
        assert_eq!(run.code, 5, "{args:?}: {}", run.stdout);
        assert_eq!(run.lines.len(), 1);
        assert_eq!(run.last()["event"], "error");
        assert_eq!(
            run.last()["message"],
            "Refusing to use the real home folder. Set TIDY_HOME, or pass --real."
        );
        assert!(run.stderr.contains("Refusing to use the real home folder"));
    }
    // An empty TIDY_HOME counts as unset.
    let mut empty = vars.clone();
    empty.push(("TIDY_HOME".into(), String::new()));
    assert_eq!(f.tidyctl_with(&["clean", "--safe"], &empty).code, 5);
    // Nothing moved, nothing was created.
    assert_eq!(listing(&f.home), before);
    assert!(listing(&f.tidy).is_empty());
    assert!(listing(&f.fake_user_home).is_empty());
}

#[test]
fn large_lists_exactly_the_expected_items() {
    let f = Fixture::standard();
    let expected = f.read_json("expected-large.json");
    let never = strings(&expected["never"]);
    let absent = strings(&expected["absent_items"]);

    let run = f.tidyctl(&["large", "--min-mb", "4"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let res = run.last();
    assert_eq!(res["event"], "large");
    assert_eq!(res["min_bytes"], expected["min_bytes"]);
    let got: Vec<Value> = res["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| pick(i, &LARGE_KEYS))
        .collect();
    assert_eq!(Value::Array(got), expected["items"]);
    let rels: Vec<&str> = res["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["rel"].as_str().unwrap())
        .collect();
    assert!(rels.contains(&"Applications/Big.app"));
    for a in &absent {
        assert!(!rels.iter().any(|r| under(r, a)), "{a} listed");
    }
    for n in &never {
        assert!(!run.stdout.contains(n.as_str()), "{n} in the output");
    }

    let run = f.tidyctl(&["large", "--min-mb", "4", "--not-opened"]);
    assert_eq!(run.code, 0);
    let rels: Vec<String> = run.last()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["rel"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(rels, strings(&expected["not_opened"]));

    let run = f.tidyctl(&["large", "--min-mb", "4", "--folders"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let res = run.last();
    assert_eq!(res["event"], "folders");
    let folders = res["folders"].as_array().unwrap();
    for want in expected["folders"].as_array().unwrap() {
        let rel = want["rel"].as_str().unwrap();
        let got = folders
            .iter()
            .find(|f| f["rel"] == rel)
            .unwrap_or_else(|| panic!("no total for {rel:?}"));
        assert_eq!(got["bytes_alloc"], want["bytes_alloc"], "{rel:?} allocated");
        assert_eq!(
            got["bytes_logical"], want["bytes_logical"],
            "{rel:?} logical"
        );
    }
    for n in &never {
        assert!(
            !run.stdout.contains(n.as_str()),
            "{n} in the folders output"
        );
    }
    // The rows at the home folder are the children over the threshold, largest first.
    let rows = res["rows"].as_array().unwrap();
    assert!(!rows.is_empty());
    assert!(
        rows.iter()
            .all(|r| r["bytes_alloc"].as_u64().unwrap() >= 4_000_000)
    );
    // Drilling into a folder shows its breadcrumb.
    let run = f.tidyctl(&["large", "--min-mb", "4", "--folders", "Library/Caches"]);
    assert_eq!(run.code, 0);
    let crumbs: Vec<&str> = run.last()["crumbs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(crumbs, ["Home", "Library", "Caches"]);
    let run = f.tidyctl(&["large", "--folders", "No/Such/Folder"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.last()["event"], "error");
}

#[test]
fn a_held_lock_stops_destructive_commands_only() {
    let f = Fixture::standard();
    let before = listing(&f.home);
    let lock = InstanceLock::acquire(&f.tidy.join("data/lock")).unwrap();
    let run = f.tidyctl(&["clean", "--safe"]);
    assert_eq!(run.code, 4, "{}", run.stdout);
    assert_eq!(run.last()["event"], "error");
    assert_eq!(f.tidyctl(&["empty-trash"]).code, 4);
    // Read-only commands never take the lock.
    assert_eq!(f.tidyctl(&["scan"]).code, 0);
    assert_eq!(f.tidyctl(&["history"]).code, 0);
    drop(lock);
    assert_eq!(listing(&f.home), before);
}

#[test]
fn clean_ids_moves_only_the_named_items() {
    let f = Fixture::standard();
    let id = "dev.npm-cacache:.npm/_cacache";
    let run = f.tidyctl(&["clean", "--ids", id]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(names(&f.dir_trash()), ["_cacache"]);
    assert!(!f.home.join(".npm/_cacache").exists());
    assert!(f.home.join("Library/Caches/com.example.idle").is_dir());

    // A blocked item, a report-only item and an unknown id are skipped: exit 3.
    let run = f.tidyctl(&[
        "clean",
        "--ids",
        "cache.chrome:Library/Caches/Google/Chrome/Default,ai.vm:.colima,bogus:x",
    ]);
    assert_eq!(run.code, 3, "{}", run.stdout);
    let skipped: Vec<&str> = run
        .events("skipped")
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(skipped.len(), 3);
    assert!(skipped.contains(&"bogus:x"));
    assert!(f.home.join("Library/Caches/Google/Chrome/Default").is_dir());
    assert!(f.home.join(".colima").is_dir());
    assert_eq!(names(&f.dir_trash()), ["_cacache"]);
}

#[test]
fn trash_moves_paths_inside_the_root_only() {
    let f = Fixture::standard();
    let movie = f.home.join("Movies/trip.mov");
    let run = f.tidyctl(&["trash", movie.to_str().unwrap()]);
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(run.last()["action"], "trash");
    assert_eq!(run.last()["source"], "large");
    assert_eq!(run.last()["items"][0]["bytes"], 8 * 1024 * 1024);
    assert!(!movie.exists());
    assert_eq!(names(&f.dir_trash()), ["trip.mov"]);

    let before = (listing(&f.home), listing(&f.dir.join("outside")));
    let outside = f.dir.join("outside/huge-outside.bin");
    let denied = f.home.join("Library/Containers");
    let link = f.home.join("Documents/linkdir/huge-outside.bin");
    let run = f.tidyctl(&[
        "trash",
        outside.to_str().unwrap(),
        denied.to_str().unwrap(),
        link.to_str().unwrap(),
        f.home.to_str().unwrap(),
        "Movies",
    ]);
    assert_eq!(run.code, 3, "{}", run.stdout);
    assert_eq!(run.events("skipped").len(), 5);
    assert!(run.events("trashed").is_empty());
    assert_eq!((listing(&f.home), listing(&f.dir.join("outside"))), before);
}

#[test]
fn bad_usage_exits_2() {
    let f = Fixture::standard();
    for args in [
        vec![],
        vec!["bogus"],
        vec!["clean"],
        vec!["clean", "--safe", "--ids", "a:b"],
        vec!["large", "--min-mb", "four"],
        vec!["trash"],
    ] {
        let run = f.tidyctl(&args);
        assert_eq!(run.code, 2, "{args:?}");
        assert_eq!(run.last()["event"], "error");
        assert!(run.stderr.contains("usage"));
    }
    let help = f.tidyctl(&["--help"]);
    assert_eq!(help.code, 0);
}

/// Writes `Applications/Box.app` (bundle id `com.example.box`) and two of its leftovers into
/// the fixture home, and returns the app's path.
fn put_box(f: &Fixture) -> std::path::PathBuf {
    let app = f.home.join("Applications/Box.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::write(app.join("Contents/MacOS/Box"), vec![1u8; 8192]).unwrap();
    std::fs::write(
        app.join("Contents/Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>com.example.box</string>\
         <key>CFBundleName</key><string>Box</string>\
         <key>CFBundleExecutable</key><string>Box</string></dict></plist>",
    )
    .unwrap();
    for rel in [
        "Library/Caches/com.example.box/a.bin",
        "Library/Containers/com.example.box/Data/b.bin",
    ] {
        let p = f.home.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![2u8; 4096]).unwrap();
    }
    app
}

#[test]
fn apps_and_leftovers_list_the_fixture_app() {
    let f = Fixture::standard();
    let app = put_box(&f);
    let run = f.tidyctl(&["apps"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let apps = run.last();
    assert_eq!(apps["event"], "apps");
    let found: Vec<&str> = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["bundle_id"].as_str().unwrap())
        .collect();
    assert!(found.contains(&"com.example.box"), "{found:?}");

    let run = f.tidyctl(&["leftovers", "com.example.box"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let plan = run.last();
    assert_eq!(plan["event"], "leftovers");
    let kinds: Vec<&str> = plan["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["app", "caches", "container"]);
    assert_eq!(plan["app"]["path"], app.to_str().unwrap());

    let run = f.tidyctl(&["leftovers", "com.example.nothing"]);
    assert_eq!(run.code, 1);
    assert_eq!(run.last()["what"], "app");
}

#[test]
fn uninstall_moves_the_app_and_its_leftovers() {
    let f = Fixture::standard();
    let app = put_box(&f);
    let before = listing(&f.home);
    let run = f.tidyctl(&["uninstall", "com.example.box"]);
    assert_eq!(run.code, 0, "{}\n{}", run.stdout, run.stderr);
    let done = run.last();
    assert_eq!(done["event"], "done");
    assert_eq!(done["action"], "uninstall");
    assert_eq!(done["source"], "uninstaller");
    assert_eq!(done["trasher"], "dir");
    assert_eq!(run.events("trashed").len(), 3);
    assert_eq!(
        names(&f.dir_trash()),
        ["Box.app", "com.example.box", "com.example.box 2"]
    );
    assert!(!app.exists());
    // Nothing else in the fixture home moved.
    let after = listing(&f.home);
    for p in before.keys().filter(|k| !after.contains_key(*k)) {
        assert!(
            p.contains("Box.app") || p.contains("com.example.box"),
            "{p} moved"
        );
    }
}
