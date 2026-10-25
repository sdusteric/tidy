//! The Cleanup rules against small fake homes: one tree per rule family, a fixed clock and a
//! fixed process list. Nothing here touches the real home folder, the real Trash or the real
//! process list.
//!
//! File sizes are multiples of 4096 and filled with a non-zero pattern, so allocated bytes
//! equal logical bytes and every expected size is exact.

use std::fs::{File, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tempfile::TempDir;
use tidy_core::cleanup::{
    CleanupProgress, load_last_cleanup, recheck, recheck_paths, scan_cleanup, trash_summary,
};
use tidy_core::env::{Env, FixedProbe, TEST_NOW};
use tidy_core::model::{
    Action, Class, FileId, Group, HistoryEntry, HistoryItem, Item, ItemResult, Plan, RunningSet,
    Source, TrasherKind, from_unix_secs,
};
use tidy_core::store::{append_json_line, append_line};

/// 1 MiB: above the 1 MB row minimum and a multiple of 4096.
const MIB: u64 = 1 << 20;
/// 288 KiB: below 1 MB, a multiple of 4096.
const SMALL: u64 = 72 * 4096;
const DAY: u64 = 86_400;

struct Home {
    _tmp: TempDir,
    root: PathBuf,
    tidy: PathBuf,
}

impl Home {
    fn new() -> Home {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("home");
        let tidy = base.join("tidy");
        std::fs::create_dir_all(&root).unwrap();
        Home {
            _tmp: tmp,
            root,
            tidy,
        }
    }

    fn env(&self) -> (Env, Arc<FixedProbe>) {
        let probe = Arc::new(FixedProbe::default());
        let env = Env::for_tests(&self.root, &self.tidy).with_procs(probe.clone());
        (env, probe)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// Writes a file of `size` bytes last modified `age` seconds before the test clock.
    fn put(&self, rel: &str, size: u64, age: u64) -> &Home {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..size as usize).map(|i| (i % 251) as u8 + 1).collect();
        std::fs::write(&path, data).unwrap();
        set_age(&path, age);
        self
    }

    /// Writes a small marker file (a manifest, a lock file) of the given age.
    fn mark(&self, rel: &str, age: u64) -> &Home {
        self.put_text(rel, "x\n", age)
    }

    fn put_text(&self, rel: &str, text: &str, age: u64) -> &Home {
        let path = self.path(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        set_age(&path, age);
        self
    }

    fn app(&self, file: &str, bundle_id: &str) -> &Home {
        self.put_text(
            &format!("Applications/{file}/Contents/Info.plist"),
            &format!(
                "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>{bundle_id}</string>\
                 <key>CFBundleName</key><string>{}</string></dict></plist>",
                file.trim_end_matches(".app")
            ),
            400 * DAY,
        )
    }
}

fn set_age(path: &Path, age: u64) {
    let t = from_unix_secs(TEST_NOW - age);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(t).set_accessed(t))
        .unwrap();
}

fn scan(env: &Env) -> Plan {
    scan_cleanup(env, &|_| {}, &Arc::new(AtomicBool::new(false)))
}

fn ids(plan: &Plan) -> Vec<&str> {
    plan.items.iter().map(|i| i.id.as_str()).collect()
}

fn find<'a>(plan: &'a Plan, id: &str) -> &'a Item {
    plan.item(id)
        .unwrap_or_else(|| panic!("{id} not in {:#?}", ids(plan)))
}

fn absent(plan: &Plan, id: &str) {
    assert!(plan.item(id).is_none(), "{id} should not be offered");
}

fn assert_item(plan: &Plan, id: &str, class: Class, ticked: bool, bytes: u64) -> Item {
    let item = assert_alloc(plan, id, class, ticked, bytes);
    assert_eq!(item.bytes_logical, bytes, "{id} logical bytes");
    item
}

/// Like [`assert_item`] for items that hold a tiny file, whose logical size is below its
/// allocated block.
fn assert_alloc(plan: &Plan, id: &str, class: Class, ticked: bool, bytes: u64) -> Item {
    let item = find(plan, id).clone();
    assert_eq!(item.class, class, "{id} class");
    assert_eq!(item.ticked, ticked, "{id} ticked");
    assert_eq!(item.bytes_alloc, bytes, "{id} allocated bytes");
    item
}

// ---------- System and app caches ----------

#[test]
fn caches_of_known_apps_are_safe_and_unknown_ones_are_review() {
    let h = Home::new();
    h.app("Idle.app", "com.example.Idle");
    h.put("Library/Caches/com.example.Idle/data.bin", MIB, 3 * DAY);
    h.put(
        "Library/Caches/com.example.Idle/sub/more.bin",
        2 * MIB,
        3 * DAY,
    );
    h.put("Library/Caches/company.thebrowser.dia/x.bin", MIB, 5 * DAY);
    h.put("Library/Caches/mystery-cache/x.bin", MIB, 5 * DAY);
    h.put("Library/Caches/com.apple.Safari/x.bin", 4 * MIB, 5 * DAY);
    h.put("Library/Caches/com.apple.dt.Xcode/y.bin", 4 * MIB, 5 * DAY);
    h.put(
        "Library/Application Support/Caches/yarn/x.bin",
        MIB,
        5 * DAY,
    );
    let (env, _) = h.env();
    let plan = scan(&env);

    let idle = assert_item(
        &plan,
        "cache.app:Library/Caches/com.example.Idle",
        Class::Safe,
        true,
        3 * MIB,
    );
    assert_eq!(idle.group, Group::Caches);
    assert_eq!(idle.name, "Idle");
    assert_eq!(idle.blocked, None);
    assert_eq!(idle.path, h.path("Library/Caches/com.example.Idle"));
    assert_eq!(idle.newest_mtime, Some(TEST_NOW - 3 * DAY));
    // A name in the static table needs no installed app.
    let dia = assert_item(
        &plan,
        "cache.app:Library/Caches/company.thebrowser.dia",
        Class::Safe,
        true,
        MIB,
    );
    assert_eq!(dia.name, "Dia");
    assert_item(
        &plan,
        "cache.app:Library/Application Support/Caches/yarn",
        Class::Safe,
        true,
        MIB,
    );
    let unknown = assert_item(
        &plan,
        "cache.unknown:Library/Caches/mystery-cache",
        Class::Review,
        false,
        MIB,
    );
    assert_eq!(unknown.name, "mystery-cache");
    assert_eq!(unknown.reason, "Tidy couldn't tell which app owns this.");
    // macOS-managed caches never appear, under any rule.
    assert!(
        plan.items
            .iter()
            .all(|i| !i.id.contains("com.apple.")
                && !i.path.to_string_lossy().contains("com.apple.")),
        "{:?}",
        ids(&plan)
    );
    assert_eq!(plan.items.len(), 4);
    assert_eq!(plan.safe_bytes(), 3 * MIB + MIB + MIB);
    assert_eq!(plan.review_bytes(), MIB);
}

#[test]
fn a_cache_whose_owner_is_running_is_blocked_and_unticked() {
    let h = Home::new();
    h.app("Idle.app", "com.example.Idle");
    h.put("Library/Caches/com.example.Idle/data.bin", MIB, 3 * DAY);
    h.put(
        "Library/Caches/Google/Chrome/Default/Cache/a.bin",
        2 * MIB,
        3 * DAY,
    );
    h.put(
        "Library/Caches/Google/Chrome/Profile 1/Cache/a.bin",
        MIB,
        3 * DAY,
    );
    h.put("Library/Caches/Google/Chrome-headless/x.bin", MIB, 3 * DAY);
    h.put("Library/Caches/Google/Software Update/x.bin", MIB, 3 * DAY);
    let (env, probe) = h.env();

    let idle = scan(&env);
    for id in [
        "cache.chrome:Library/Caches/Google/Chrome/Default",
        "cache.chrome:Library/Caches/Google/Chrome/Profile 1",
        "cache.chrome:Library/Caches/Google/Chrome-headless",
    ] {
        let item = find(&idle, id);
        assert!(item.ticked && item.blocked.is_none(), "{id}");
    }
    assert_eq!(
        find(&idle, "cache.chrome:Library/Caches/Google/Chrome/Default").name,
        "Google Chrome (Default)"
    );
    // The Google folder itself and its other children are not offered.
    absent(&idle, "cache.app:Library/Caches/Google");
    assert_eq!(idle.items.len(), 4);

    probe.set(RunningSet::parse("com.google.Chrome"));
    let busy = scan(&env);
    for id in [
        "cache.chrome:Library/Caches/Google/Chrome/Default",
        "cache.chrome:Library/Caches/Google/Chrome/Profile 1",
        "cache.chrome:Library/Caches/Google/Chrome-headless",
    ] {
        let item = find(&busy, id);
        assert_eq!(
            item.blocked.as_deref(),
            Some("Quit Google Chrome first"),
            "{id}"
        );
        assert!(!item.ticked && !item.enabled(), "{id}");
        assert_eq!(item.class, Class::Safe, "{id}");
    }
    // Other owners are not affected.
    let idle_app = find(&busy, "cache.app:Library/Caches/com.example.Idle");
    assert!(idle_app.ticked);
    assert_eq!(busy.safe_bytes(), MIB);

    // The headless shell alone blocks the same rows.
    probe.set(RunningSet::parse("chrome-headless-shell"));
    let shell = scan(&env);
    assert!(
        find(&shell, "cache.chrome:Library/Caches/Google/Chrome/Default")
            .blocked
            .is_some()
    );

    // An unreadable process list blocks every owner.
    probe.set(RunningSet::unknown());
    let unknown = scan(&env);
    assert!(
        find(&unknown, "cache.app:Library/Caches/com.example.Idle")
            .blocked
            .is_some()
    );
}

#[test]
fn anything_modified_in_the_last_24_hours_is_not_offered() {
    let h = Home::new();
    h.put("Library/Caches/yarn/old.bin", MIB, 3 * DAY);
    h.put("Library/Caches/Cypress/old.bin", MIB, 3 * DAY);
    // One fresh file inside an otherwise old cache keeps the whole folder out.
    h.put("Library/Caches/Cypress/fresh.bin", SMALL, DAY / 2);
    h.put("Library/Caches/pre-commit/x.bin", MIB, DAY - 60);
    h.put("Library/Caches/go-build/x.bin", MIB, DAY + 60);
    let (env, _) = h.env();
    let plan = scan(&env);
    assert_eq!(
        ids(&plan),
        [
            "cache.app:Library/Caches/go-build",
            "cache.app:Library/Caches/yarn"
        ]
    );
}

#[test]
fn small_matches_fold_into_one_row_that_cleans_as_a_unit() {
    let h = Home::new();
    for name in ["yarn", "Cypress", "go-build", "pre-commit"] {
        h.put(&format!("Library/Caches/{name}/x.bin"), SMALL, 4 * DAY);
    }
    // A fifth tiny cache belongs to a running owner and is left out of the fold.
    h.put("Library/Caches/discord/x.bin", SMALL, 4 * DAY);
    h.put(
        "Library/Caches/big-cache-with-no-owner/x.bin",
        2 * MIB,
        4 * DAY,
    );
    let (env, probe) = h.env();
    probe.set(RunningSet::parse("discord"));
    let plan = scan(&env);

    let fold = assert_item(
        &plan,
        "cache.app.small:Library/Caches",
        Class::Safe,
        true,
        4 * SMALL,
    );
    assert_eq!(fold.name, "4 small items");
    assert_eq!(fold.path, h.path("Library/Caches"));
    assert_eq!(fold.members.len(), 4);
    assert!(
        fold.members
            .iter()
            .all(|m| m.starts_with(h.path("Library/Caches")))
    );
    assert!(!fold.members.contains(&h.path("Library/Caches/discord")));
    assert_eq!(fold.paths(), fold.members);
    assert_eq!(plan.items.len(), 2, "{:?}", ids(&plan));
    // A fold under 1 MB in total is not a row at all.
    let h = Home::new();
    for name in ["yarn", "Cypress", "go-build"] {
        h.put(&format!("Library/Caches/{name}/x.bin"), SMALL, 4 * DAY);
    }
    let (env, _) = h.env();
    assert!(scan(&env).items.is_empty());
}

#[test]
fn symlinks_are_never_followed_and_ignored_paths_are_left_alone() {
    let h = Home::new();
    h.put("Library/Caches/yarn/x.bin", MIB, 4 * DAY);
    h.put("Library/Caches/Cypress/x.bin", MIB, 4 * DAY);
    // A symlink to a big folder outside the root, in a cache folder's place.
    let outside = h.root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("huge.bin"), vec![1u8; 8 * MIB as usize]).unwrap();
    std::os::unix::fs::symlink(&outside, h.path("Library/Caches/linked")).unwrap();
    // A symlink inside a cache does not add the target's bytes.
    std::os::unix::fs::symlink(&outside, h.path("Library/Caches/yarn/link")).unwrap();
    let (mut env, _) = h.env();
    let plan = scan(&env);
    assert_eq!(
        ids(&plan),
        [
            "cache.app:Library/Caches/Cypress",
            "cache.app:Library/Caches/yarn"
        ]
    );
    assert_eq!(
        find(&plan, "cache.app:Library/Caches/yarn").bytes_alloc,
        MIB
    );

    env.settings.ignore(h.path("Library/Caches/yarn"));
    let plan = scan(&env);
    assert_eq!(ids(&plan), ["cache.app:Library/Caches/Cypress"]);
}

#[test]
fn electron_and_chromium_cache_folders_are_matched_by_name_only() {
    let h = Home::new();
    let slack = "Library/Application Support/Slack";
    // A Chromium user-data folder: markers, then the folders the rule may take.
    h.mark(&format!("{slack}/Preferences"), 50 * DAY);
    h.put(&format!("{slack}/Cache/Cache_Data/a.bin"), MIB, 5 * DAY);
    h.put(&format!("{slack}/Code Cache/js/b.bin"), MIB, 5 * DAY);
    h.put(&format!("{slack}/GPUCache/c.bin"), MIB, 5 * DAY);
    h.put(&format!("{slack}/Crashpad/completed/d.bin"), MIB, 5 * DAY);
    h.put(
        &format!("{slack}/Service Worker/ScriptCache/e.bin"),
        MIB,
        5 * DAY,
    );
    h.put(
        &format!("{slack}/Service Worker/CacheStorage/f.bin"),
        2 * MIB,
        5 * DAY,
    );
    // Siblings are never touched.
    h.put(
        &format!("{slack}/Local Storage/leveldb/g.bin"),
        4 * MIB,
        5 * DAY,
    );
    h.put(&format!("{slack}/Cookies"), 4 * MIB, 5 * DAY);
    h.put(&format!("{slack}/Crashpad/pending/h.bin"), 4 * MIB, 5 * DAY);
    h.put(
        &format!("{slack}/Service Worker/Database/i.bin"),
        4 * MIB,
        5 * DAY,
    );
    // Chrome: the user-data folder is Google/Chrome, with a profile folder.
    let chrome = "Library/Application Support/Google/Chrome";
    h.mark(&format!("{chrome}/Local State"), 50 * DAY);
    h.put(&format!("{chrome}/Default/GPUCache/a.bin"), MIB, 5 * DAY);
    h.put(
        &format!("{chrome}/Profile 2/Code Cache/a.bin"),
        MIB,
        5 * DAY,
    );
    h.put(&format!("{chrome}/Default/Cookies"), 4 * MIB, 5 * DAY);
    h.put(
        &format!("{chrome}/Default/IndexedDB/x/db.bin"),
        4 * MIB,
        5 * DAY,
    );
    h.put(&format!("{chrome}/Default/Login Data"), 4 * MIB, 5 * DAY);
    h.put(&format!("{chrome}/Default/History"), 4 * MIB, 5 * DAY);
    h.put(&format!("{chrome}/component_crx_cache/a.bin"), MIB, 5 * DAY);
    h.put(
        &format!("{chrome}/extensions_crx_cache/a.bin"),
        MIB,
        5 * DAY,
    );
    h.put(&format!("{chrome}/ShaderCache/a.bin"), MIB, 5 * DAY);
    h.put(&format!("{chrome}/GraphiteDawnCache/a.bin"), MIB, 5 * DAY);
    // An app folder without any Chromium marker is not an Electron app.
    h.put(
        "Library/Application Support/Plain/Cache/a.bin",
        4 * MIB,
        5 * DAY,
    );
    // An Electron app with an owner nobody knows.
    h.mark("Library/Application Support/Mystery/Cookies", 50 * DAY);
    h.put(
        "Library/Application Support/Mystery/Cache/a.bin",
        MIB,
        5 * DAY,
    );
    let (env, probe) = h.env();
    let plan = scan(&env);

    for sub in ["Cache", "Code Cache", "GPUCache", "Crashpad/completed"] {
        assert_item(
            &plan,
            &format!("cache.electron:{slack}/{sub}"),
            Class::Safe,
            true,
            MIB,
        );
    }
    let sw = assert_item(
        &plan,
        &format!("cache.electron.sw:{slack}/Service Worker/CacheStorage"),
        Class::Review,
        false,
        2 * MIB,
    );
    assert_eq!(
        sw.reason,
        "Some websites may load slower once or lose offline data."
    );
    // Service worker scripts are Review, unticked, with their own reason.
    let scripts = assert_item(
        &plan,
        &format!("cache.electron.scripts:{slack}/Service Worker/ScriptCache"),
        Class::Review,
        false,
        MIB,
    );
    assert_eq!(
        scripts.reason,
        "Some extensions may stop working until you turn them off and on again."
    );
    assert_item(
        &plan,
        &format!("cache.electron:{chrome}/Default/GPUCache"),
        Class::Safe,
        true,
        MIB,
    );
    assert_item(
        &plan,
        &format!("cache.electron:{chrome}/Profile 2/Code Cache"),
        Class::Safe,
        true,
        MIB,
    );
    for sub in [
        "component_crx_cache",
        "extensions_crx_cache",
        "ShaderCache",
        "GraphiteDawnCache",
    ] {
        assert_item(
            &plan,
            &format!("cache.chrome.crx:{chrome}/{sub}"),
            Class::Safe,
            true,
            MIB,
        );
    }
    assert_item(
        &plan,
        "cache.unknown:Library/Application Support/Mystery/Cache",
        Class::Review,
        false,
        MIB,
    );
    // Slack 4 Safe + CacheStorage + ScriptCache, Chrome 2 + 4, Mystery 1, and nothing from
    // siblings or the plain folder.
    assert_eq!(plan.items.len(), 4 + 1 + 1 + 2 + 4 + 1, "{:#?}", ids(&plan));
    for item in &plan.items {
        let p = item.path.to_string_lossy();
        for never in [
            "Local Storage",
            "Cookies",
            "IndexedDB",
            "Login Data",
            "History",
            "Plain",
            "pending",
            "Database",
        ] {
            assert!(!p.contains(never), "{p}");
        }
    }

    probe.set(RunningSet::parse("slack"));
    let busy = scan(&env);
    assert_eq!(
        find(&busy, &format!("cache.electron:{slack}/Cache"))
            .blocked
            .as_deref(),
        Some("Quit Slack first")
    );
    assert!(find(&busy, &format!("cache.electron:{chrome}/Default/GPUCache")).ticked);
}

// ---------- Logs ----------

#[test]
fn logs_are_files_older_than_14_days_and_crash_reports_older_than_30() {
    let h = Home::new();
    h.put("Library/Logs/Foo/old.log", MIB, 20 * DAY);
    h.put("Library/Logs/Foo/newer.log", MIB, 3 * DAY);
    h.put("Library/Logs/Foo/edge.log", MIB, 14 * DAY);
    h.put("Library/Logs/Foo/sub/deep.log", MIB, 40 * DAY);
    h.put("Library/Logs/Slack/run.log", 2 * MIB, 20 * DAY);
    h.put(
        "Library/Logs/DiagnosticReports/crash-old.ips",
        MIB,
        40 * DAY,
    );
    h.put(
        "Library/Logs/DiagnosticReports/crash-mid.ips",
        MIB,
        20 * DAY,
    );
    h.put("Library/Logs/Tidy/tidy.log", MIB, 90 * DAY);
    h.put("Library/Logs/loose.log", MIB, 90 * DAY);
    // Old small logs of one app fold into one row for that app.
    for n in 0..4 {
        h.put(&format!("Library/Logs/Bar/{n}.log"), SMALL, 30 * DAY);
    }
    let (env, probe) = h.env();
    probe.set(RunningSet::parse("slack"));
    let plan = scan(&env);

    let old = assert_item(
        &plan,
        "logs.app:Library/Logs/Foo/old.log",
        Class::Safe,
        true,
        MIB,
    );
    assert_eq!(old.group, Group::Logs);
    assert_eq!(old.name, "old.log");
    assert_eq!(old.reason, "Log from Foo, older than 14 days");
    let slack = find(&plan, "logs.app:Library/Logs/Slack/run.log");
    assert_eq!(slack.blocked.as_deref(), Some("Quit Slack first"));
    assert!(!slack.ticked);
    assert_item(
        &plan,
        "logs.diag:Library/Logs/DiagnosticReports/crash-old.ips",
        Class::Safe,
        true,
        MIB,
    );
    let fold = assert_item(
        &plan,
        "logs.app.small:Library/Logs/Bar",
        Class::Safe,
        true,
        4 * SMALL,
    );
    assert_eq!(fold.members.len(), 4);
    assert_eq!(fold.reason, "Log from Bar, older than 14 days");
    assert_eq!(plan.items.len(), 4, "{:?}", ids(&plan));
}

// ---------- Developer ----------

#[test]
fn xcode_rules_follow_age_owner_and_keep_newest() {
    let h = Home::new();
    let dd = "Library/Developer/Xcode/DerivedData";
    h.put(&format!("{dd}/MyApp-old/Build/a.bin"), 3 * MIB, 10 * DAY);
    h.put(&format!("{dd}/MyApp-new/Build/a.bin"), 3 * MIB, 3 * DAY);
    h.put(
        "Library/Developer/Xcode/UserData/Previews/p.bin",
        MIB,
        3 * DAY,
    );
    h.put("Library/Developer/CoreSimulator/Caches/c.bin", MIB, 3 * DAY);
    h.put(
        "Library/Developer/CoreSimulator/Devices/D1/data.bin",
        4 * MIB,
        3 * DAY,
    );
    h.put(
        "Library/Developer/Xcode/CodingAssistant/m.bin",
        2 * MIB,
        3 * DAY,
    );
    let ds = "Library/Developer/Xcode/iOS DeviceSupport";
    h.put(
        &format!("{ds}/iPhone15,2 17.4 (21E)/s.bin"),
        2 * MIB,
        40 * DAY,
    );
    h.put(
        &format!("{ds}/iPhone15,2 17.5 (21F)/s.bin"),
        2 * MIB,
        20 * DAY,
    );
    h.put(
        &format!("{ds}/iPhone15,2 16.0 (20A)/s.bin"),
        2 * MIB,
        90 * DAY,
    );
    h.put(
        &format!("{ds}/iPad13,1 17.5 (21F)/s.bin"),
        2 * MIB,
        90 * DAY,
    );
    let ar = "Library/Developer/Xcode/Archives";
    h.put(
        &format!("{ar}/2026-06-01/App.xcarchive/a.bin"),
        2 * MIB,
        100 * DAY,
    );
    h.put(
        &format!("{ar}/2026-09-01/App.xcarchive/a.bin"),
        2 * MIB,
        50 * DAY,
    );
    let (env, probe) = h.env();
    let plan = scan(&env);

    assert_item(
        &plan,
        &format!("dev.deriveddata:{dd}/MyApp-old"),
        Class::Safe,
        true,
        3 * MIB,
    );
    absent(&plan, &format!("dev.deriveddata:{dd}/MyApp-new"));
    assert_item(
        &plan,
        "dev.previews:Library/Developer/Xcode/UserData/Previews",
        Class::Safe,
        true,
        MIB,
    );
    assert_item(
        &plan,
        "dev.coresim-caches:Library/Developer/CoreSimulator/Caches",
        Class::Safe,
        true,
        MIB,
    );
    let sim = assert_item(
        &plan,
        "dev.simulators:Library/Developer/CoreSimulator/Devices",
        Class::ReportOnly,
        false,
        4 * MIB,
    );
    assert_eq!(
        sim.reason,
        "Remove unused simulators with: xcrun simctl delete unavailable"
    );
    assert_item(
        &plan,
        "dev.xcode-codingassistant:Library/Developer/Xcode/CodingAssistant",
        Class::ReportOnly,
        false,
        2 * MIB,
    );
    // Never the newest entry per device model.
    assert_item(
        &plan,
        &format!("dev.devicesupport:{ds}/iPhone15,2 17.4 (21E)"),
        Class::Review,
        false,
        2 * MIB,
    );
    assert_item(
        &plan,
        &format!("dev.devicesupport:{ds}/iPhone15,2 16.0 (20A)"),
        Class::Review,
        false,
        2 * MIB,
    );
    absent(
        &plan,
        &format!("dev.devicesupport:{ds}/iPhone15,2 17.5 (21F)"),
    );
    absent(
        &plan,
        &format!("dev.devicesupport:{ds}/iPad13,1 17.5 (21F)"),
    );
    assert_item(
        &plan,
        &format!("dev.archives:{ar}/2026-06-01/App.xcarchive"),
        Class::Review,
        false,
        2 * MIB,
    );
    absent(
        &plan,
        &format!("dev.archives:{ar}/2026-09-01/App.xcarchive"),
    );
    assert!(plan.items.iter().all(|i| i.group == Group::Developer));

    probe.set(RunningSet::parse("com.apple.dt.Xcode"));
    let busy = scan(&env);
    let item = find(&busy, &format!("dev.deriveddata:{dd}/MyApp-old"));
    assert_eq!(item.blocked.as_deref(), Some("Quit Xcode first"));
    assert!(!item.ticked);
    assert!(
        find(
            &busy,
            "dev.previews:Library/Developer/Xcode/UserData/Previews"
        )
        .blocked
        .is_some()
    );
    // CoreSimulatorService is a different owner.
    assert!(
        find(
            &busy,
            "dev.coresim-caches:Library/Developer/CoreSimulator/Caches"
        )
        .ticked
    );
}

#[test]
fn package_caches_follow_the_table() {
    let h = Home::new();
    h.put(".npm/_cacache/a.bin", 2 * MIB, 5 * DAY);
    h.put(".npm/_npx/b.bin", MIB, 5 * DAY);
    h.put("Library/Caches/node-gyp/c.bin", MIB, 5 * DAY);
    h.put(
        "Library/Caches/Homebrew/downloads/old.tar.gz",
        MIB,
        40 * DAY,
    );
    h.put("Library/Caches/Homebrew/downloads/new.tar.gz", MIB, 5 * DAY);
    h.put("Library/Caches/Homebrew/api/index.json", 4 * MIB, 40 * DAY);
    h.put("Library/Caches/pip/d.bin", MIB, 5 * DAY);
    h.put(".cache/pip/e.bin", MIB, 5 * DAY);
    h.put("Library/Caches/pnpm/f.bin", MIB, 5 * DAY);
    h.put(".cache/uv/g.bin", MIB, 5 * DAY);
    h.put(".bun/install/cache/h.bin", MIB, 5 * DAY);
    h.put(".gradle/caches/i.bin", MIB, 5 * DAY);
    h.put(".gradle/wrapper/j.bin", MIB, 5 * DAY);
    h.put(".m2/repository/k.bin", MIB, 5 * DAY);
    h.put(".cargo/registry/cache/l.bin", MIB, 5 * DAY);
    h.put(".cargo/registry/src/m.bin", 2 * MIB, 5 * DAY);
    h.put(".cargo/registry/index/n.bin", 4 * MIB, 5 * DAY);
    h.put(".cargo/bin/cargo", 4 * MIB, 5 * DAY);
    h.put(".rustup/toolchains/t.bin", 4 * MIB, 5 * DAY);
    h.put(".cursor/extensions/e.bin", 4 * MIB, 5 * DAY);
    h.put(".docker/d.bin", 4 * MIB, 5 * DAY);
    h.put(".claude/projects/p.jsonl", 4 * MIB, 5 * DAY);
    h.put(".cocoapods/repos/o.bin", MIB, 5 * DAY);
    h.put("Library/pnpm/store/v3/p.bin", 3 * MIB, 5 * DAY);
    h.put(".cache/gh/q.bin", MIB, 5 * DAY);
    h.put(".cache/yt-dlp/r.bin", MIB, 5 * DAY);
    h.put(".cache/some-unknown-tool/s.bin", 4 * MIB, 5 * DAY);
    h.put(
        "Library/Application Support/Zed/languages/z.bin",
        MIB,
        5 * DAY,
    );
    h.put("Library/Application Support/Zed/node/z.bin", MIB, 5 * DAY);
    h.put("Library/Application Support/Zed/db/z.bin", 4 * MIB, 5 * DAY);
    let (env, probe) = h.env();
    let plan = scan(&env);

    let safe = |id: &str, bytes: u64| {
        assert_item(&plan, id, Class::Safe, true, bytes);
    };
    let review = |id: &str, bytes: u64| {
        assert_item(&plan, id, Class::Review, false, bytes);
    };
    safe("dev.npm-cacache:.npm/_cacache", 2 * MIB);
    safe("dev.npx:.npm/_npx", MIB);
    safe("dev.node-gyp:Library/Caches/node-gyp", MIB);
    safe(
        "dev.brew-downloads:Library/Caches/Homebrew/downloads/old.tar.gz",
        MIB,
    );
    absent(
        &plan,
        "dev.brew-downloads:Library/Caches/Homebrew/downloads/new.tar.gz",
    );
    safe("dev.pip:Library/Caches/pip", MIB);
    safe("dev.pip:.cache/pip", MIB);
    safe("dev.pnpm:Library/Caches/pnpm", MIB);
    safe("dev.uv:.cache/uv", MIB);
    safe("dev.bun:.bun/install/cache", MIB);
    safe("dev.gradle-caches:.gradle/caches", MIB);
    safe("dev.cache:.cache/gh", MIB);
    safe("dev.cache:.cache/yt-dlp", MIB);
    review("dev.gradle-wrapper:.gradle/wrapper", MIB);
    review("dev.m2:.m2/repository", MIB);
    review("dev.cargo-registry:.cargo/registry/cache", MIB);
    review("dev.cargo-registry:.cargo/registry/src", 2 * MIB);
    review("dev.cocoapods-repos:.cocoapods/repos", MIB);
    review("dev.zed:Library/Application Support/Zed/languages", MIB);
    review("dev.zed:Library/Application Support/Zed/node", MIB);
    let store = assert_item(
        &plan,
        "dev.pnpm-store:Library/pnpm/store",
        Class::ReportOnly,
        false,
        3 * MIB,
    );
    assert_eq!(
        store.reason,
        "Reclaim unused packages with: pnpm store prune"
    );
    assert_eq!(plan.items.len(), 20, "{:#?}", ids(&plan));
    // Excluded and unlisted targets never appear.
    for needle in [
        ".cargo/bin",
        ".rustup",
        ".cursor",
        ".docker",
        ".claude",
        "some-unknown-tool",
        "registry/index",
        "Zed/db",
        "Homebrew/api",
    ] {
        assert!(
            !plan.items.iter().any(|i| i.id.contains(needle)),
            "{needle}"
        );
    }
    // The generic cache rule does not claim names with their own rule.
    assert!(!plan.items.iter().any(|i| i.rule_id().starts_with("cache.")));

    probe.set(RunningSet::parse("npm,cargo,java,brew,pnpm"));
    let busy = scan(&env);
    assert_eq!(
        find(&busy, "dev.npm-cacache:.npm/_cacache")
            .blocked
            .as_deref(),
        Some("Quit npm first")
    );
    assert_eq!(
        find(&busy, "dev.gradle-caches:.gradle/caches")
            .blocked
            .as_deref(),
        Some("Quit Gradle first")
    );
    assert_eq!(
        find(&busy, "dev.cargo-registry:.cargo/registry/src")
            .blocked
            .as_deref(),
        Some("Quit Cargo first")
    );
    assert_eq!(
        find(&busy, "dev.pnpm:Library/Caches/pnpm")
            .blocked
            .as_deref(),
        Some("Quit pnpm first")
    );
    assert!(
        find(
            &busy,
            "dev.brew-downloads:Library/Caches/Homebrew/downloads/old.tar.gz"
        )
        .blocked
        .is_some()
    );
    assert!(find(&busy, "dev.uv:.cache/uv").ticked);
    // The gradle wrapper and the m2 repository name no blocking process.
    assert!(find(&busy, "dev.m2:.m2/repository").blocked.is_none());
}

#[test]
fn playwright_agent_and_android_studio_versions_keep_the_newest() {
    let h = Home::new();
    let pw = "Library/Caches/ms-playwright";
    h.put(&format!("{pw}/chromium-1100/a.bin"), 2 * MIB, 90 * DAY);
    h.put(&format!("{pw}/chromium-1148/a.bin"), 2 * MIB, 30 * DAY);
    h.put(&format!("{pw}/ffmpeg-1011/a.bin"), 2 * MIB, 90 * DAY);
    h.put(&format!("{pw}/.links"), 1, 90 * DAY);
    let cl = ".local/share/claude/versions";
    h.put(&format!("{cl}/2.1.1/claude"), 2 * MIB, 90 * DAY);
    h.put(&format!("{cl}/2.1.2/claude"), 2 * MIB, 60 * DAY);
    h.put(&format!("{cl}/2.1.3/claude"), 2 * MIB, 30 * DAY);
    let cu = ".local/share/cursor-agent/versions";
    h.put(&format!("{cu}/2026.1.1/agent"), 2 * MIB, 90 * DAY);
    h.put(&format!("{cu}/2026.2.2/agent"), 2 * MIB, 60 * DAY);
    let g = "Library/Application Support/Google";
    h.put(&format!("{g}/AndroidStudio2023.3/s.bin"), 2 * MIB, 90 * DAY);
    h.put(&format!("{g}/AndroidStudio2024.1/s.bin"), 2 * MIB, 20 * DAY);
    let (env, probe) = h.env();
    let plan = scan(&env);

    assert_item(
        &plan,
        &format!("dev.playwright:{pw}/chromium-1100"),
        Class::Review,
        false,
        2 * MIB,
    );
    absent(&plan, &format!("dev.playwright:{pw}/chromium-1148"));
    absent(&plan, &format!("dev.playwright:{pw}/ffmpeg-1011"));
    assert_item(
        &plan,
        &format!("dev.agent-versions:{cl}/2.1.1"),
        Class::Review,
        false,
        2 * MIB,
    );
    assert_item(
        &plan,
        &format!("dev.agent-versions:{cl}/2.1.2"),
        Class::Review,
        false,
        2 * MIB,
    );
    absent(&plan, &format!("dev.agent-versions:{cl}/2.1.3"));
    assert_item(
        &plan,
        &format!("dev.agent-versions:{cu}/2026.1.1"),
        Class::Review,
        false,
        2 * MIB,
    );
    absent(&plan, &format!("dev.agent-versions:{cu}/2026.2.2"));
    assert_item(
        &plan,
        &format!("dev.androidstudio:{g}/AndroidStudio2023.3"),
        Class::Review,
        false,
        2 * MIB,
    );
    absent(&plan, &format!("dev.androidstudio:{g}/AndroidStudio2024.1"));
    assert_eq!(plan.items.len(), 5, "{:?}", ids(&plan));

    // A running process whose executable lies in a version folder protects that folder.
    probe.set(RunningSet::new([h
        .path(&format!("{cl}/2.1.2/claude"))
        .to_string_lossy()
        .into_owned()]));
    let busy = scan(&env);
    absent(&busy, &format!("dev.agent-versions:{cl}/2.1.2"));
    assert_item(
        &busy,
        &format!("dev.agent-versions:{cl}/2.1.1"),
        Class::Review,
        false,
        2 * MIB,
    );
    assert_eq!(busy.items.len(), 4, "{:?}", ids(&busy));
}

// ---------- Project build output ----------

const CARGO_TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n";

#[test]
fn rust_projects_are_offered_only_when_idle() {
    let h = Home::new();
    let p = "Documents/projects";
    // Idle for 60 days, build output 40 days old.
    h.mark(&format!("{p}/idle/Cargo.toml"), 60 * DAY);
    h.mark(&format!("{p}/idle/src/main.rs"), 60 * DAY);
    h.put(&format!("{p}/idle/target/debug/app"), 3 * MIB, 40 * DAY);
    // Touched yesterday: the build output is not offered however old it is.
    h.mark(&format!("{p}/active/Cargo.toml"), 60 * DAY);
    h.mark(&format!("{p}/active/src/main.rs"), DAY + 3600);
    h.put(&format!("{p}/active/target/debug/app"), 3 * MIB, 100 * DAY);
    // Just under 30 days.
    h.mark(&format!("{p}/recent/Cargo.toml"), 29 * DAY);
    h.put(&format!("{p}/recent/target/debug/app"), 3 * MIB, 100 * DAY);
    // A target with no Cargo.toml and no cargo tag is not a Rust build folder.
    h.mark(&format!("{p}/plain/readme.txt"), 90 * DAY);
    h.put(&format!("{p}/plain/target/x.bin"), 3 * MIB, 90 * DAY);
    // A cargo tag is enough without Cargo.toml in the parent (a workspace member layout).
    h.mark(&format!("{p}/tagged/readme.txt"), 90 * DAY);
    h.put_text(
        &format!("{p}/tagged/target/CACHEDIR.TAG"),
        CARGO_TAG,
        90 * DAY,
    );
    h.put(&format!("{p}/tagged/target/x.bin"), 3 * MIB, 90 * DAY);
    // A build folder inside .git is never entered.
    h.mark(&format!("{p}/idle/.git/HEAD"), 1);
    h.put(
        &format!("{p}/idle/.git/modules/target/x.bin"),
        3 * MIB,
        90 * DAY,
    );
    // The build output is not walked into: a project inside it is not found.
    h.mark(&format!("{p}/idle/target/debug/inner/Cargo.toml"), 90 * DAY);
    h.put(
        &format!("{p}/idle/target/debug/inner/target/y.bin"),
        3 * MIB,
        90 * DAY,
    );
    let (env, probe) = h.env();
    let plan = scan(&env);

    // 3 MiB of build output, an inner project (3 MiB and a one-block manifest).
    let idle = assert_alloc(
        &plan,
        &format!("proj.target:{p}/idle/target"),
        Class::Safe,
        true,
        6 * MIB + 4096,
    );
    assert_eq!(idle.name, "idle/target");
    assert_eq!(idle.group, Group::Developer);
    assert_eq!(idle.reason, "Build output, project not touched for 60 days");
    assert_alloc(
        &plan,
        &format!("proj.target:{p}/tagged/target"),
        Class::Safe,
        true,
        3 * MIB + 4096,
    );
    assert_eq!(plan.items.len(), 2, "{:?}", ids(&plan));

    probe.set(RunningSet::parse("rustc"));
    let busy = scan(&env);
    assert_eq!(
        find(&busy, &format!("proj.target:{p}/idle/target"))
            .blocked
            .as_deref(),
        Some("Quit Cargo first")
    );
}

#[test]
fn node_python_and_other_build_output_follow_table_6_3() {
    let h = Home::new();
    let p = "Documents/projects";
    let old = 90 * DAY;
    // node_modules with package.json; a nested node_modules is not walked into.
    h.mark(&format!("{p}/web/package.json"), old);
    h.put(&format!("{p}/web/node_modules/a/index.js"), 2 * MIB, old);
    h.put(
        &format!("{p}/web/node_modules/a/node_modules/b/index.js"),
        2 * MIB,
        old,
    );
    h.put(&format!("{p}/web/.next/cache/c.bin"), MIB, old);
    // No package.json: not a Node project.
    h.mark(&format!("{p}/nopkg/readme.md"), old);
    h.put(&format!("{p}/nopkg/node_modules/a.js"), 4 * MIB, old);
    // dist: only when the project's .gitignore names it.
    h.mark(&format!("{p}/lib/package.json"), old);
    h.put_text(&format!("{p}/lib/.gitignore"), "node_modules\n", old);
    h.put(&format!("{p}/lib/dist/index.js"), MIB, old);
    h.mark(&format!("{p}/lib2/package.json"), old);
    h.put_text(&format!("{p}/lib2/.gitignore"), "# build\n/dist/\n", old);
    h.put(&format!("{p}/lib2/dist/index.js"), MIB, old);
    // Swift package, CocoaPods with and without a lock file.
    h.mark(&format!("{p}/swift/Package.swift"), old);
    h.put(&format!("{p}/swift/.build/x.bin"), MIB, old);
    h.mark(&format!("{p}/ios/Podfile"), old);
    h.mark(&format!("{p}/ios/Podfile.lock"), old);
    h.put(&format!("{p}/ios/Pods/x.bin"), MIB, old);
    h.mark(&format!("{p}/ios2/Podfile"), old);
    h.put(&format!("{p}/ios2/Pods/x.bin"), MIB, old);
    // Gradle: .gradle and the Android build folder.
    h.mark(&format!("{p}/droid/settings.gradle.kts"), old);
    h.put(&format!("{p}/droid/.gradle/x.bin"), MIB, old);
    h.put(&format!("{p}/droid/build/x.bin"), MIB, old);
    // Python: venv needs pyvenv.cfg and a requirements file; pycache folds.
    h.mark(&format!("{p}/py/requirements.txt"), old);
    h.mark(&format!("{p}/py/app.py"), old);
    h.put_text(
        &format!("{p}/py/.venv/pyvenv.cfg"),
        "home = /usr/bin\n",
        old,
    );
    h.put(&format!("{p}/py/.venv/lib/x.bin"), 2 * MIB, old);
    for n in 0..4 {
        h.put(&format!("{p}/py/pkg{n}/__pycache__/m.pyc"), SMALL, 2 * DAY);
        h.mark(&format!("{p}/py/pkg{n}/m.py"), old);
    }
    h.mark(&format!("{p}/py2/app.py"), old);
    h.put(&format!("{p}/py2/venv/lib/x.bin"), 2 * MIB, old);
    // Xcode DerivedData beside a project, after 7 days.
    h.mark(&format!("{p}/mac/App.xcodeproj/project.pbxproj"), 10 * DAY);
    h.put(&format!("{p}/mac/DerivedData/x.bin"), MIB, 10 * DAY);
    h.mark(&format!("{p}/mac2/App.xcodeproj/project.pbxproj"), 3 * DAY);
    h.put(&format!("{p}/mac2/DerivedData/x.bin"), MIB, 10 * DAY);
    // PHP vendor needs composer.json and its lock.
    h.mark(&format!("{p}/php/composer.json"), old);
    h.mark(&format!("{p}/php/composer.lock"), old);
    h.put(&format!("{p}/php/vendor/x.bin"), MIB, old);
    h.mark(&format!("{p}/php2/composer.json"), old);
    h.put(&format!("{p}/php2/vendor/x.bin"), MIB, old);
    // A project nested a few levels down is found too.
    h.mark(&format!("{p}/mono/packages/app/package.json"), old);
    h.put(
        &format!("{p}/mono/packages/app/node_modules/x.js"),
        MIB,
        old,
    );
    let (env, _) = h.env();
    let plan = scan(&env);

    let safe = |id: String, bytes: u64| {
        assert_item(&plan, &id, Class::Safe, true, bytes);
    };
    let review = |id: String, bytes: u64| {
        assert_item(&plan, &id, Class::Review, false, bytes);
    };
    safe(format!("proj.node_modules:{p}/web/node_modules"), 4 * MIB);
    safe(format!("proj.web-cache:{p}/web/.next"), MIB);
    safe(format!("proj.swiftpm:{p}/swift/.build"), MIB);
    safe(format!("proj.pods:{p}/ios/Pods"), MIB);
    review(format!("proj.pods:{p}/ios2/Pods"), MIB);
    safe(format!("proj.gradle:{p}/droid/.gradle"), MIB);
    safe(format!("proj.gradle-build:{p}/droid/build"), MIB);
    assert_alloc(
        &plan,
        &format!("proj.venv:{p}/py/.venv"),
        Class::Safe,
        true,
        2 * MIB + 4096,
    );
    safe(format!("proj.pycache.small:{p}/py"), 4 * SMALL);
    safe(format!("proj.deriveddata:{p}/mac/DerivedData"), MIB);
    safe(
        format!("proj.node_modules:{p}/mono/packages/app/node_modules"),
        MIB,
    );
    review(format!("proj.dist:{p}/lib2/dist"), MIB);
    review(format!("proj.vendor:{p}/php/vendor"), MIB);
    let fold = find(&plan, &format!("proj.pycache.small:{p}/py"));
    assert_eq!(fold.members.len(), 4);
    assert_eq!(fold.name, "4 small items");
    for gone in ["nopkg", "lib/dist", "py2", "mac2", "php2"] {
        assert!(
            !plan.items.iter().any(|i| i.id.contains(gone)),
            "{gone} in {:?}",
            ids(&plan)
        );
    }
    assert_eq!(plan.items.len(), 13, "{:#?}", ids(&plan));
}

#[test]
fn project_roots_come_from_settings_and_stay_inside_the_scan_root() {
    let h = Home::new();
    h.mark("Developer/app/package.json", 90 * DAY);
    h.put("Developer/app/node_modules/x.js", MIB, 90 * DAY);
    h.mark("elsewhere/app/package.json", 90 * DAY);
    h.put("elsewhere/app/node_modules/x.js", MIB, 90 * DAY);
    let (mut env, _) = h.env();
    // Default roots include Developer; elsewhere is not a root.
    let plan = scan(&env);
    assert_eq!(ids(&plan), ["proj.node_modules:Developer/app/node_modules"]);
    env.settings.project_roots = vec![
        "elsewhere".into(),
        "/not/inside/the/root".into(),
        "../up".into(),
    ];
    let plan = scan(&env);
    assert_eq!(ids(&plan), ["proj.node_modules:elsewhere/app/node_modules"]);
}

// ---------- AI models and large app data ----------

#[test]
fn ai_models_are_review_or_report_only_and_never_ticked() {
    let h = Home::new();
    h.put(
        ".cache/huggingface/hub/models--org--name/blobs/b.bin",
        4 * MIB,
        20 * DAY,
    );
    h.put(
        ".cache/huggingface/hub/models--solo/blobs/b.bin",
        2 * MIB,
        20 * DAY,
    );
    h.put(
        ".cache/huggingface/hub/datasets--x/b.bin",
        4 * MIB,
        20 * DAY,
    );
    h.put(
        ".cache/lm-studio/models/pub/model-a/w.gguf",
        3 * MIB,
        20 * DAY,
    );
    h.put(".lmstudio/models/pub/model-b/w.gguf", 2 * MIB, 20 * DAY);
    h.put(".ollama/models/blobs/b.bin", 5 * MIB, 20 * DAY);
    h.put(".cache/whisper/w.bin", MIB, 20 * DAY);
    h.put(".cache/fluidaudio/f.bin", MIB, 20 * DAY);
    h.put(
        "Library/Application Support/FluidAudio/f.bin",
        MIB,
        20 * DAY,
    );
    h.put(".cache/hyperframes/chrome/c.bin", MIB, 20 * DAY);
    h.put(".cache/hyperframes/ffmpeg/c.bin", MIB, 20 * DAY);
    h.put(
        "Library/Application Support/LTXDesktop/models/ltx-1/m.bin",
        3 * MIB,
        20 * DAY,
    );
    h.put(
        "Library/Application Support/LTXDesktop/models/ltx-2/m.bin",
        2 * MIB,
        20 * DAY,
    );
    let ax = "Library/Application Support/anythingllm-desktop/storage";
    h.put(&format!("{ax}/models/m.bin"), 3 * MIB, 20 * DAY);
    h.put(&format!("{ax}/documents/d.bin"), 2 * MIB, 20 * DAY);
    h.put(&format!("{ax}/vector-cache/v.bin"), MIB, 20 * DAY);
    let chrome = "Library/Application Support/Google/Chrome";
    h.put(
        &format!("{chrome}/OptGuideOnDeviceModel/m.bin"),
        3 * MIB,
        20 * DAY,
    );
    h.put(&format!("{chrome}/SODALanguagePacks/m.bin"), MIB, 20 * DAY);
    h.put(&format!("{chrome}/Snapshots/m.bin"), MIB, 20 * DAY);
    h.put(".colima/default/disk.img", 8 * MIB, 20 * DAY);
    h.put(
        "Library/Application Support/Claude/vm_bundles/v/disk.img",
        6 * MIB,
        20 * DAY,
    );
    let (env, probe) = h.env();
    probe.set(RunningSet::parse("ollama,LM Studio"));
    let plan = scan(&env);

    let model = |id: &str, bytes: u64| {
        let i = assert_item(&plan, id, Class::Review, false, bytes);
        assert_eq!(i.group, Group::AiModels, "{id}");
        i
    };
    let hf = model(
        "ai.huggingface:.cache/huggingface/hub/models--org--name",
        4 * MIB,
    );
    assert_eq!(hf.name, "org/name");
    assert_eq!(
        model(
            "ai.huggingface:.cache/huggingface/hub/models--solo",
            2 * MIB
        )
        .name,
        "solo"
    );
    model("ai.lmstudio:.cache/lm-studio/models/pub/model-a", 3 * MIB);
    model("ai.lmstudio:.lmstudio/models/pub/model-b", 2 * MIB);
    model("ai.ollama:.ollama/models", 5 * MIB);
    model("ai.whisper:.cache/whisper", MIB);
    model("ai.fluidaudio:.cache/fluidaudio", MIB);
    model("ai.fluidaudio:Library/Application Support/FluidAudio", MIB);
    model("ai.hyperframes:.cache/hyperframes/chrome", MIB);
    model("ai.hyperframes:.cache/hyperframes/ffmpeg", MIB);
    model(
        "ai.ltx:Library/Application Support/LTXDesktop/models/ltx-1",
        3 * MIB,
    );
    model(
        "ai.ltx:Library/Application Support/LTXDesktop/models/ltx-2",
        2 * MIB,
    );
    model(&format!("ai.anythingllm:{ax}/models"), 3 * MIB);
    for sub in ["OptGuideOnDeviceModel", "SODALanguagePacks", "Snapshots"] {
        model(
            &format!("ai.chrome-models:{chrome}/{sub}"),
            if sub == "OptGuideOnDeviceModel" {
                3 * MIB
            } else {
                MIB
            },
        );
    }
    for (id, bytes) in [
        (format!("ai.anythingllm-data:{ax}/documents"), 2 * MIB),
        (format!("ai.anythingllm-data:{ax}/vector-cache"), MIB),
        ("ai.vm:.colima".to_string(), 8 * MIB),
        (
            "ai.vm:Library/Application Support/Claude/vm_bundles".to_string(),
            6 * MIB,
        ),
    ] {
        let i = assert_item(&plan, &id, Class::ReportOnly, false, bytes);
        assert_eq!(i.group, Group::AiModels, "{id}");
        assert!(!i.enabled());
    }
    assert_eq!(
        find(&plan, "ai.ollama:.ollama/models").blocked.as_deref(),
        Some("Quit Ollama first")
    );
    assert_eq!(
        find(&plan, "ai.lmstudio:.lmstudio/models/pub/model-b")
            .blocked
            .as_deref(),
        Some("Quit LM Studio first")
    );
    absent(&plan, &format!("ai.anythingllm-data:{ax}/models"));
    // Nothing in this group is ever ticked or part of the one-click clean.
    assert!(plan.group(Group::AiModels).all(|i| !i.ticked));
    assert_eq!(plan.safe_bytes(), 0);
    assert_eq!(plan.groups(), [Group::AiModels]);
}

// ---------- Global rules ----------

#[test]
fn rows_under_one_megabyte_are_not_offered_alone() {
    let h = Home::new();
    h.put(".npm/_cacache/a.bin", SMALL, 5 * DAY);
    h.put("Library/Caches/yarn/x.bin", 999_424, 5 * DAY);
    let (env, _) = h.env();
    assert!(scan(&env).items.is_empty());
}

#[test]
fn hard_links_count_once_in_an_item() {
    let h = Home::new();
    h.put("Library/Caches/yarn/a.bin", MIB, 5 * DAY);
    std::fs::hard_link(
        h.path("Library/Caches/yarn/a.bin"),
        h.path("Library/Caches/yarn/b.bin"),
    )
    .unwrap();
    let (env, _) = h.env();
    let plan = scan(&env);
    assert_item(
        &plan,
        "cache.app:Library/Caches/yarn",
        Class::Safe,
        true,
        MIB,
    );
    assert_eq!(plan.files_seen, 1);
}

#[test]
fn the_denylist_and_excluded_folders_are_never_scanned() {
    let h = Home::new();
    // Containers is denied even where a rule would otherwise reach it.
    h.put(
        "Library/Containers/com.example.App/Data/Library/Caches/x.bin",
        8 * MIB,
        5 * DAY,
    );
    h.put("Library/Caches/Tidy/last-large.json", 4 * MIB, 5 * DAY);
    h.put("Library/Logs/Tidy/tidy.log", 4 * MIB, 40 * DAY);
    h.put(
        "Library/Application Support/Tidy/history.jsonl",
        4 * MIB,
        40 * DAY,
    );
    h.put(".ssh/id_ed25519", 4 * MIB, 40 * DAY);
    h.put(".config/gh/hosts.yml", 4 * MIB, 40 * DAY);
    // A sqlite file inside a cache is not measured.
    h.put("Library/Caches/yarn/x.bin", MIB, 5 * DAY);
    h.put("Library/Caches/yarn/store.sqlite", 4 * MIB, 5 * DAY);
    let (env, _) = h.env();
    let plan = scan(&env);
    assert_eq!(ids(&plan), ["cache.app:Library/Caches/yarn"]);
    assert_eq!(plan.items[0].bytes_alloc, MIB);
}

#[test]
fn a_scan_is_deterministic_sorted_and_reports_progress() {
    let h = Home::new();
    h.put("Library/Caches/yarn/x.bin", MIB, 5 * DAY);
    h.put("Library/Caches/Cypress/x.bin", 3 * MIB, 5 * DAY);
    h.put("Library/Logs/Foo/old.log", 2 * MIB, 30 * DAY);
    h.put(".npm/_cacache/a.bin", 5 * MIB, 5 * DAY);
    h.put(".cache/whisper/w.bin", MIB, 20 * DAY);
    let (env, _) = h.env();
    let calls = AtomicUsize::new(0);
    let last = std::sync::Mutex::new(CleanupProgress::default());
    let plan = scan_cleanup(
        &env,
        &|p| {
            calls.fetch_add(1, Ordering::Relaxed);
            *last.lock().unwrap() = p;
        },
        &Arc::new(AtomicBool::new(false)),
    );
    assert!(calls.load(Ordering::Relaxed) >= 2);
    let last = last.lock().unwrap().clone();
    assert_eq!(last.files, 5);
    assert_eq!(last.bytes, 12 * MIB);
    assert_eq!(plan.files_seen, 5);
    assert_eq!(plan.root, h.root);
    assert_eq!(plan.scanned_at, TEST_NOW);
    // Group order, then size, then id.
    assert_eq!(
        ids(&plan),
        [
            "cache.app:Library/Caches/Cypress",
            "cache.app:Library/Caches/yarn",
            "logs.app:Library/Logs/Foo/old.log",
            "dev.npm-cacache:.npm/_cacache",
            "ai.whisper:.cache/whisper",
        ]
    );
    assert_eq!(scan(&env), plan);
}

#[test]
fn a_cancelled_scan_stops_and_saves_nothing() {
    let h = Home::new();
    h.put("Library/Caches/yarn/x.bin", MIB, 5 * DAY);
    let (env, _) = h.env();
    let cancel = Arc::new(AtomicBool::new(true));
    let plan = scan_cleanup(&env, &|_| {}, &cancel);
    assert!(plan.items.is_empty());
    assert!(load_last_cleanup(&env).is_none());
    assert!(!env.paths.last_cleanup_file().exists());
}

#[test]
fn the_last_plan_is_saved_and_loaded_for_the_same_root_only() {
    let h = Home::new();
    h.put("Library/Caches/yarn/x.bin", MIB, 5 * DAY);
    let (env, _) = h.env();
    assert!(load_last_cleanup(&env).is_none());
    let plan = scan(&env);
    assert_eq!(plan.items.len(), 1);
    assert_eq!(load_last_cleanup(&env), Some(plan.clone()));
    let other = Home::new();
    let env2 = Env::for_tests(&other.root, &h.tidy);
    assert_eq!(load_last_cleanup(&env2), None);
    // A damaged file reads as no plan.
    std::fs::write(env.paths.last_cleanup_file(), "{not json").unwrap();
    assert_eq!(load_last_cleanup(&env), None);
}

#[test]
fn the_trash_summary_counts_what_tidy_left_there() {
    let h = Home::new();
    let (env, _) = h.env();
    assert_eq!(trash_summary(&env).items, 0);
    let in_trash = env.paths.dir_trash();
    std::fs::create_dir_all(&in_trash).unwrap();
    std::fs::write(in_trash.join("a"), b"x").unwrap();
    let item = |name: &str, bytes: u64, result: ItemResult| HistoryItem {
        path: h.path(name),
        trash_path: Some(in_trash.join(name)),
        bytes,
        rule: "cache.app".into(),
        result,
        reason: None,
        // Read after the move, the way a clean records it.
        trash_id: FileId::of(&in_trash.join(name)).ok(),
    };
    let entry = HistoryEntry {
        id: HistoryEntry::make_id(TEST_NOW, 1),
        at: TEST_NOW,
        action: Action::Clean,
        source: Source::Cleanup,
        trasher: TrasherKind::Dir,
        items: vec![
            item("a", 100, ItemResult::Trashed),
            // Already emptied.
            item("gone", 200, ItemResult::Trashed),
            item("skipped", 300, ItemResult::Skipped),
        ],
        trashed_bytes: 300,
        message: None,
    };
    append_json_line(&env.paths.history_file(), &entry).unwrap();
    append_line(&env.paths.history_file(), "this line is damaged").unwrap();
    let summary = trash_summary(&env);
    assert_eq!((summary.items, summary.bytes), (1, 100));
    h.put("Library/Caches/yarn/x.bin", MIB, 5 * DAY);
    assert_eq!(scan(&env).trash, summary);
}

// ---------- Re-check at clean time ----------

fn recheck_home() -> (Home, Env, Arc<FixedProbe>, Plan) {
    let h = Home::new();
    h.app("Idle.app", "com.example.Idle");
    h.put("Library/Caches/com.example.Idle/data.bin", MIB, 3 * DAY);
    h.put(
        "Library/Caches/Google/Chrome/Default/Cache/a.bin",
        MIB,
        3 * DAY,
    );
    h.put("Library/Logs/Foo/old.log", MIB, 20 * DAY);
    h.put(".cache/whisper/w.bin", MIB, 20 * DAY);
    h.mark("Documents/projects/idle/Cargo.toml", 60 * DAY);
    h.put(
        "Documents/projects/idle/target/debug/app",
        2 * MIB,
        40 * DAY,
    );
    h.mark("Documents/projects/lib/package.json", 60 * DAY);
    h.put_text("Documents/projects/lib/.gitignore", "dist\n", 60 * DAY);
    h.put("Documents/projects/lib/dist/a.js", MIB, 60 * DAY);
    for name in ["yarn", "Cypress", "go-build", "pre-commit"] {
        h.put(&format!("Library/Caches/{name}/x.bin"), SMALL, 4 * DAY);
    }
    let (env, probe) = h.env();
    let plan = scan(&env);
    (h, env, probe, plan)
}

#[test]
fn recheck_passes_a_fresh_item_and_returns_its_path() {
    let (h, env, _, plan) = recheck_home();
    for item in &plan.items {
        if item.class == Class::ReportOnly {
            continue;
        }
        assert_eq!(recheck(&env, item), Ok(item.path.clone()), "{}", item.id);
    }
    let fold = find(&plan, "cache.app.small:Library/Caches");
    assert_eq!(recheck_paths(&env, fold).unwrap().len(), 4);
    let idle = find(&plan, "cache.app:Library/Caches/com.example.Idle");
    assert_eq!(
        recheck(&env, idle),
        Ok(h.path("Library/Caches/com.example.Idle"))
    );
}

#[test]
fn recheck_refuses_when_the_owner_started_running() {
    let (_h, env, probe, plan) = recheck_home();
    probe.set(RunningSet::parse("com.google.Chrome"));
    assert_eq!(
        recheck(
            &env,
            find(&plan, "cache.chrome:Library/Caches/Google/Chrome/Default")
        ),
        Err("Google Chrome opened, skipped".to_string())
    );
    probe.set(RunningSet::parse("com.example.idle"));
    assert_eq!(
        recheck(
            &env,
            find(&plan, "cache.app:Library/Caches/com.example.Idle")
        ),
        Err("Idle opened, skipped".to_string())
    );
    probe.set(RunningSet::parse("cargo"));
    assert_eq!(
        recheck(
            &env,
            find(&plan, "proj.target:Documents/projects/idle/target")
        ),
        Err("Cargo opened, skipped".to_string())
    );
    probe.set(RunningSet::parse("foo"));
    assert!(recheck(&env, find(&plan, "logs.app:Library/Logs/Foo/old.log")).is_ok());
    // An app that runs under a folder-derived owner blocks its logs.
    probe.set(RunningSet::parse("com.example.idle"));
    assert!(recheck(&env, find(&plan, "cache.app.small:Library/Caches")).is_ok());
    // An unreadable process list refuses everything that has an owner.
    probe.set(RunningSet::unknown());
    assert!(
        recheck(
            &env,
            find(&plan, "cache.app:Library/Caches/com.example.Idle")
        )
        .is_err()
    );
    assert!(recheck(&env, find(&plan, "ai.whisper:.cache/whisper")).is_err());
}

#[test]
fn recheck_refuses_a_path_that_changed_since_the_scan() {
    let (h, env, _, plan) = recheck_home();
    // Gone.
    let idle = find(&plan, "cache.app:Library/Caches/com.example.Idle");
    std::fs::remove_dir_all(&idle.path).unwrap();
    assert_eq!(recheck(&env, idle), Err("No longer there".to_string()));
    // Replaced by a symlink to a folder outside the root.
    let outside = h.root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let chrome = find(&plan, "cache.chrome:Library/Caches/Google/Chrome/Default");
    std::fs::remove_dir_all(&chrome.path).unwrap();
    std::os::unix::fs::symlink(&outside, &chrome.path).unwrap();
    assert!(recheck(&env, chrome).is_err());
    // A symlink that stays inside the root is refused too.
    let whisper = find(&plan, "ai.whisper:.cache/whisper");
    std::fs::remove_dir_all(&whisper.path).unwrap();
    std::os::unix::fs::symlink(h.path("Library"), &whisper.path).unwrap();
    assert_eq!(
        recheck(&env, whisper),
        Err("Now a link, skipped".to_string())
    );
    // Put on the ignore list after the scan.
    let log = find(&plan, "logs.app:Library/Logs/Foo/old.log");
    let mut ignoring = env.clone();
    ignoring.settings.ignore(h.path("Library/Logs/Foo"));
    assert_eq!(
        recheck(&ignoring, log),
        Err("On your ignore list".to_string())
    );
    assert!(recheck(&env, log).is_ok());
}

/// A folder above the item swapped for a link to another folder inside the root, after the
/// scan: the re-check refuses it and Clean moves nothing, so the folder the link points to
/// keeps its contents.
#[test]
fn a_parent_swapped_for_a_link_inside_the_root_is_refused() {
    let (h, env, _, plan) = recheck_home();
    let idle = find(&plan, "cache.app:Library/Caches/com.example.Idle").clone();
    h.put("Documents/decoy/com.example.Idle/SENTINEL", MIB, 90 * DAY);
    std::fs::rename(h.path("Library/Caches"), h.path("Library/Caches-moved")).unwrap();
    std::os::unix::fs::symlink(h.path("Documents/decoy"), h.path("Library/Caches")).unwrap();
    assert_eq!(recheck(&env, &idle), Err("Now a link, skipped".to_string()));
    let entry = tidy_core::clean::clean(&env, std::slice::from_ref(&idle), &mut |_| {});
    assert_eq!(entry.items[0].result, ItemResult::Skipped);
    assert_eq!(
        entry.items[0].reason.as_deref(),
        Some("Now a link, skipped")
    );
    assert!(
        h.path("Documents/decoy/com.example.Idle/SENTINEL")
            .is_file()
    );
    assert!(!env.paths.dir_trash().exists());
    // A fresh scan never lists a folder reached through the link either.
    let again = scan(&env);
    assert!(
        again
            .items
            .iter()
            .all(|i| !i.path.starts_with(h.path("Library/Caches"))),
        "{:#?}",
        ids(&again)
    );
}

/// A two-part Chromium target ("Crashpad/completed") whose first part is a link to a folder
/// of the user's elsewhere in the home folder is never offered, so Clean cannot reach it.
#[test]
fn a_link_inside_a_chromium_target_is_not_followed() {
    let h = Home::new();
    for app in ["FooApp", "Slack"] {
        let dir = format!("Library/Application Support/{app}");
        h.mark(&format!("{dir}/Preferences"), 50 * DAY);
        h.put(&format!("{dir}/GPUCache/c.bin"), MIB, 5 * DAY);
        std::fs::create_dir_all(h.path(&format!("Projects/{app}/completed"))).unwrap();
        std::fs::create_dir_all(h.path(&format!("Projects/{app}/ScriptCache"))).unwrap();
        h.put(
            &format!("Projects/{app}/completed/thesis-draft.bin"),
            3 * MIB,
            90 * DAY,
        );
        h.put(
            &format!("Projects/{app}/ScriptCache/notes.bin"),
            3 * MIB,
            90 * DAY,
        );
        std::os::unix::fs::symlink(
            h.path(&format!("Projects/{app}")),
            h.path(&format!("{dir}/Crashpad")),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            h.path(&format!("Projects/{app}")),
            h.path(&format!("{dir}/Service Worker")),
        )
        .unwrap();
    }
    let (env, _) = h.env();
    let plan = scan(&env);
    // The real cache folders are still offered.
    find(
        &plan,
        "cache.electron:Library/Application Support/Slack/GPUCache",
    );
    find(
        &plan,
        "cache.unknown:Library/Application Support/FooApp/GPUCache",
    );
    for item in &plan.items {
        for part in ["Crashpad", "Service Worker", "Projects"] {
            assert!(!item.id.contains(part), "{} should not be offered", item.id);
        }
    }
    // Even a hand-made row for the linked path is refused at clean time.
    let mut forged = find(
        &plan,
        "cache.electron:Library/Application Support/Slack/GPUCache",
    )
    .clone();
    forged.id = "cache.electron:Library/Application Support/Slack/Crashpad/completed".into();
    forged.path = h.path("Library/Application Support/Slack/Crashpad/completed");
    assert_eq!(
        recheck(&env, &forged),
        Err("Now a link, skipped".to_string())
    );
    let entry = tidy_core::clean::clean(&env, &[forged], &mut |_| {});
    assert_eq!(entry.items[0].result, ItemResult::Skipped);
    assert!(
        h.path("Projects/Slack/completed/thesis-draft.bin")
            .is_file()
    );
    assert!(
        h.path("Projects/FooApp/completed/thesis-draft.bin")
            .is_file()
    );
}

#[test]
fn recheck_requires_the_markers_to_still_be_there() {
    let (h, env, _, plan) = recheck_home();
    let target = find(&plan, "proj.target:Documents/projects/idle/target");
    std::fs::remove_file(h.path("Documents/projects/idle/Cargo.toml")).unwrap();
    assert_eq!(
        recheck(&env, target),
        Err("The project files are gone, skipped".to_string())
    );
    let dist = find(&plan, "proj.dist:Documents/projects/lib/dist");
    assert!(recheck(&env, dist).is_ok());
    std::fs::write(
        h.path("Documents/projects/lib/.gitignore"),
        "node_modules\n",
    )
    .unwrap();
    assert_eq!(
        recheck(&env, dist),
        Err("The project files are gone, skipped".to_string())
    );
}

#[test]
fn recheck_refuses_report_only_denied_and_misplaced_items() {
    let (h, env, _, plan) = recheck_home();
    let base = find(&plan, "cache.app:Library/Caches/com.example.Idle").clone();
    // Report-only rows are never moved.
    let mut report = base.clone();
    report.class = Class::ReportOnly;
    assert_eq!(
        recheck(&env, &report),
        Err("Tidy won't remove this".to_string())
    );
    // A tampered plan that points into a denied folder.
    h.put("Library/Containers/com.x/Data/a.bin", MIB, 40 * DAY);
    let mut denied = base.clone();
    denied.path = h.path("Library/Containers/com.x");
    assert_eq!(
        recheck(&env, &denied),
        Err("Tidy never touches this folder".to_string())
    );
    // Outside the root.
    let outside = h.root.parent().unwrap().join("outside2");
    std::fs::create_dir_all(&outside).unwrap();
    let mut out = base.clone();
    out.path = outside;
    assert!(recheck(&env, &out).is_err());
    // The root itself.
    let mut root = base.clone();
    root.path = h.root.clone();
    assert!(recheck(&env, &root).is_err());
    // A real folder that the rule would never produce.
    h.put("Documents/notes/n.bin", MIB, 40 * DAY);
    let mut misplaced = base.clone();
    misplaced.path = h.path("Documents/notes");
    assert_eq!(
        recheck(&env, &misplaced),
        Err("Not where the rule expects it, skipped".to_string())
    );
    // An unknown rule.
    let mut unknown = base.clone();
    unknown.id = "nope:Library/Caches/com.example.Idle".into();
    assert!(recheck(&env, &unknown).is_err());
    // Tidy's own folders.
    h.put("Library/Caches/Tidy/a.bin", MIB, 40 * DAY);
    let mut own = base;
    own.path = h.path("Library/Caches/Tidy");
    assert!(recheck(&env, &own).is_err());
}

#[test]
fn a_folded_row_keeps_the_members_that_still_pass() {
    let (h, env, _, plan) = recheck_home();
    let fold = find(&plan, "cache.app.small:Library/Caches").clone();
    std::fs::remove_dir_all(h.path("Library/Caches/yarn")).unwrap();
    let left = recheck_paths(&env, &fold).unwrap();
    assert_eq!(left.len(), 3);
    assert!(!left.contains(&h.path("Library/Caches/yarn")));
    for name in ["Cypress", "go-build", "pre-commit"] {
        std::fs::remove_dir_all(h.path(&format!("Library/Caches/{name}"))).unwrap();
    }
    assert_eq!(
        recheck_paths(&env, &fold),
        Err("No longer there".to_string())
    );
    assert_eq!(recheck(&env, &fold), Err("No longer there".to_string()));
}

#[test]
fn the_fixed_clock_decides_ages() {
    let h = Home::new();
    h.put("Library/Logs/Foo/a.log", MIB, 20 * DAY);
    let (env, _) = h.env();
    assert_eq!(scan(&env).items.len(), 1);
    // Six days earlier the same file is only 14 days old, which is not older than 14 days.
    let clock = Arc::new(tidy_core::env::FixedClock::new(TEST_NOW - 6 * DAY));
    let early = env.clone().with_clock(clock.clone());
    assert!(scan(&early).items.is_empty());
    clock.set(TEST_NOW - 5 * DAY);
    assert_eq!(scan(&early).items.len(), 1);
}

// ---------- Project guard ----------

/// A version 2 Git index that tracks `paths`, written the way Git writes one (SHA-1 names).
fn git_index(paths: &[&str]) -> Vec<u8> {
    let mut b = b"DIRC".to_vec();
    b.extend_from_slice(&2u32.to_be_bytes());
    b.extend_from_slice(&(paths.len() as u32).to_be_bytes());
    for p in paths {
        let start = b.len();
        b.extend_from_slice(&[0u8; 40]);
        b.extend_from_slice(&[0x11u8; 20]);
        b.extend_from_slice(&(p.len() as u16).to_be_bytes());
        b.extend_from_slice(p.as_bytes());
        let total = (b.len() - start + 8) & !7;
        b.resize(start + total, 0);
    }
    b.extend_from_slice(&[0u8; 20]);
    b
}

#[test]
fn build_output_holding_keys_repositories_or_tracked_files_is_never_offered() {
    let h = Home::new();
    let p = "Documents/projects";
    let rust = |name: &str| {
        h.mark(&format!("{p}/{name}/Cargo.toml"), 60 * DAY);
        h.put(&format!("{p}/{name}/target/debug/app"), 2 * MIB, 40 * DAY);
    };
    // A plain idle project, offered as before.
    rust("plain");
    // An Anchor program: its deploy key sits in target/deploy and is not rebuilt.
    rust("anchor");
    h.mark(
        &format!("{p}/anchor/target/deploy/vault-keypair.json"),
        40 * DAY,
    );
    // A repository of its own inside the build output (a folder or a `.git` file).
    rust("nested");
    h.mark(&format!("{p}/nested/target/vendor/dep/.git/HEAD"), 40 * DAY);
    rust("linked");
    h.mark(&format!("{p}/linked/target/wt/.git"), 40 * DAY);
    // Git tracks a file inside `dist`, even though .gitignore names it.
    h.mark(&format!("{p}/site/package.json"), 60 * DAY);
    h.put_text(&format!("{p}/site/.gitignore"), "dist\n", 60 * DAY);
    h.put(&format!("{p}/site/dist/app.js"), MIB, 60 * DAY);
    h.put(&format!("{p}/site/dist/keep.js"), MIB, 60 * DAY);
    let index = h.path(&format!("{p}/site/.git/index"));
    std::fs::create_dir_all(index.parent().unwrap()).unwrap();
    std::fs::write(
        &index,
        git_index(&[".gitignore", "dist/keep.js", "package.json"]),
    )
    .unwrap();
    set_age(&index, 60 * DAY);
    // A repository whose index tracks only the sources: its build output is offered.
    rust("clean-repo");
    let index2 = h.path(&format!("{p}/clean-repo/.git/index"));
    std::fs::create_dir_all(index2.parent().unwrap()).unwrap();
    std::fs::write(
        &index2,
        git_index(&["Cargo.toml", "src/main.rs", "targets.txt"]),
    )
    .unwrap();
    set_age(&index2, 60 * DAY);
    // An index Tidy cannot read: it cannot tell what Git tracks, so nothing is offered.
    rust("broken");
    h.put_text(&format!("{p}/broken/.git/index"), "not an index", 60 * DAY);
    let (env, _) = h.env();
    let plan = scan(&env);

    find(&plan, &format!("proj.target:{p}/plain/target"));
    find(&plan, &format!("proj.target:{p}/clean-repo/target"));
    for id in [
        format!("proj.target:{p}/anchor/target"),
        format!("proj.target:{p}/nested/target"),
        format!("proj.target:{p}/linked/target"),
        format!("proj.dist:{p}/site/dist"),
        format!("proj.target:{p}/broken/target"),
    ] {
        absent(&plan, &id);
    }
    assert_eq!(plan.items.len(), 2, "{:?}", ids(&plan));

    // Right before the move the guard is read again: a key that shows up after the scan
    // stops the move.
    let plain = find(&plan, &format!("proj.target:{p}/plain/target")).clone();
    assert_eq!(recheck(&env, &plain), Ok(plain.path.clone()));
    h.mark(
        &format!("{p}/plain/target/deploy/prog-keypair.json"),
        40 * DAY,
    );
    assert_eq!(
        recheck(&env, &plain),
        Err("Holds a keypair file, skipped".to_string())
    );
    let repo = find(&plan, &format!("proj.target:{p}/clean-repo/target")).clone();
    std::fs::write(
        &index2,
        git_index(&["Cargo.toml", "target/release/notes.txt"]),
    )
    .unwrap();
    assert_eq!(
        recheck(&env, &repo),
        Err("Git tracks files inside it, skipped".to_string())
    );
    std::fs::write(&index2, b"DIRC broken").unwrap();
    assert_eq!(
        recheck(&env, &repo),
        Err("Tidy couldn't read the project's Git index, skipped".to_string())
    );
    std::fs::remove_file(&index2).unwrap();
    assert_eq!(recheck(&env, &repo), Ok(repo.path.clone()));
    h.mark(&format!("{p}/clean-repo/target/sub/.git/HEAD"), 40 * DAY);
    assert_eq!(
        recheck(&env, &repo),
        Err("Holds a Git repository, skipped".to_string())
    );
}

// ---------- Unknown process list ----------

#[test]
fn an_unreadable_process_list_blocks_every_owner_and_every_move() {
    let (_h, env, probe, plan) = recheck_home();
    probe.set(RunningSet::unknown());
    // At scan time an item with an owner is blocked, never treated as idle.
    let busy = scan(&env);
    let idle = find(&busy, "cache.app:Library/Caches/com.example.Idle");
    assert!(!idle.ticked);
    assert_eq!(
        idle.blocked.as_deref(),
        Some("Tidy couldn't check whether Idle is open")
    );
    // At clean time every item is skipped, with or without an owner.
    for item in plan.items.iter().filter(|i| i.class != Class::ReportOnly) {
        assert_eq!(
            recheck(&env, item),
            Err(tidy_core::cleanup::PROCS_UNKNOWN.to_string()),
            "{}",
            item.id
        );
    }
}

// ---------- More tool caches (P2) ----------

#[test]
fn more_tool_caches_follow_their_owners() {
    let h = Home::new();
    for name in [
        "go-build",
        "gopls",
        "composer",
        "typescript",
        "deno",
        "golangci-lint",
    ] {
        h.put(&format!("Library/Caches/{name}/x.bin"), MIB, 5 * DAY);
    }
    // Poetry: downloads are Safe, project environments are Review.
    h.put(
        "Library/Caches/pypoetry/cache/repositories/x.bin",
        MIB,
        5 * DAY,
    );
    h.put("Library/Caches/pypoetry/artifacts/a/b.whl", MIB, 5 * DAY);
    h.put(
        "Library/Caches/pypoetry/virtualenvs/app-AbCd-py3.12/lib/x.bin",
        2 * MIB,
        40 * DAY,
    );
    h.put(
        "Library/Caches/pypoetry/virtualenvs/fresh-XyZ-py3.12/lib/x.bin",
        2 * MIB,
        5 * DAY,
    );
    // JetBrains: caches and indexes are Safe; Local History and the rest are never offered.
    let jb = "Library/Caches/JetBrains/IntelliJIdea2025.2";
    h.put(&format!("{jb}/caches/content.dat"), 2 * MIB, 5 * DAY);
    h.put(&format!("{jb}/index/stubs/s.bin"), 3 * MIB, 5 * DAY);
    h.put(
        &format!("{jb}/LocalHistory/changes.storageData"),
        4 * MIB,
        5 * DAY,
    );
    h.put(&format!("{jb}/tmp/t.bin"), MIB, 5 * DAY);
    // Ruby and Python version managers.
    h.put(".gem/ruby/3.3.0/cache/rails-8.0.0.gem", MIB, 5 * DAY);
    h.put(
        ".gem/ruby/3.3.0/gems/rails-8.0.0/lib/rails.rb",
        MIB,
        5 * DAY,
    );
    h.put(
        ".rbenv/versions/3.3.5/lib/ruby/gems/3.3.0/cache/rack-3.1.gem",
        MIB,
        5 * DAY,
    );
    h.put(".bundle/cache/compact_index/x.bin", MIB, 5 * DAY);
    h.put(".pyenv/cache/Python-3.12.7.tar.xz", MIB, 5 * DAY);
    h.put(".rbenv/cache/ruby-3.3.5.tar.gz", MIB, 5 * DAY);
    h.put(".pyenv/versions/3.12.7/bin/python3.12", MIB, 5 * DAY);
    // Tool caches in ~/.cache.
    h.put(".cache/pre-commit/repo1/x.bin", MIB, 5 * DAY);
    h.put(
        ".cache/puppeteer/chrome/mac_arm-131/x.bin",
        2 * MIB,
        5 * DAY,
    );
    h.put(".cache/selenium/chromedriver/x.bin", MIB, 5 * DAY);
    h.put("Library/Caches/electron/electron-v33.zip", 2 * MIB, 5 * DAY);
    // Go modules are read-only and removed by Go's own command.
    h.put("go/pkg/mod/github.com/x/y@v1.0.0/y.go", MIB, 5 * DAY);
    let (env, probe) = h.env();
    let plan = scan(&env);

    for (name, owner) in [
        ("go-build", "Go"),
        ("gopls", "gopls"),
        ("composer", "Composer"),
        ("typescript", "TypeScript"),
        ("deno", "Deno"),
        ("golangci-lint", "golangci-lint"),
    ] {
        let item = assert_item(
            &plan,
            &format!("cache.app:Library/Caches/{name}"),
            Class::Safe,
            true,
            MIB,
        );
        assert_eq!(item.name, owner);
    }
    assert_item(
        &plan,
        "dev.poetry:Library/Caches/pypoetry/cache",
        Class::Safe,
        true,
        MIB,
    );
    assert_item(
        &plan,
        "dev.poetry:Library/Caches/pypoetry/artifacts",
        Class::Safe,
        true,
        MIB,
    );
    let venv = assert_item(
        &plan,
        "dev.poetry-venvs:Library/Caches/pypoetry/virtualenvs/app-AbCd-py3.12",
        Class::Review,
        false,
        2 * MIB,
    );
    assert_eq!(venv.name, "Poetry environment: app-AbCd-py3.12");
    // An environment used in the last 30 days is not offered.
    absent(
        &plan,
        "dev.poetry-venvs:Library/Caches/pypoetry/virtualenvs/fresh-XyZ-py3.12",
    );
    absent(&plan, "cache.app:Library/Caches/pypoetry");
    let caches = assert_item(
        &plan,
        &format!("dev.jetbrains:{jb}/caches"),
        Class::Safe,
        true,
        2 * MIB,
    );
    assert_eq!(caches.name, "IntelliJIdea2025.2: caches");
    assert_item(
        &plan,
        &format!("dev.jetbrains:{jb}/index"),
        Class::Safe,
        true,
        3 * MIB,
    );
    for id in [
        "cache.unknown:Library/Caches/JetBrains",
        "cache.app:Library/Caches/JetBrains",
    ] {
        absent(&plan, id);
    }
    assert!(
        plan.items
            .iter()
            .all(|i| !i.path.to_string_lossy().contains("LocalHistory"))
    );
    let gem = assert_item(
        &plan,
        "dev.gem:.gem/ruby/3.3.0/cache",
        Class::Safe,
        true,
        MIB,
    );
    assert_eq!(gem.name, "Gem downloads (cache)");
    assert_item(
        &plan,
        "dev.gem:.rbenv/versions/3.3.5/lib/ruby/gems/3.3.0/cache",
        Class::Safe,
        true,
        MIB,
    );
    assert_item(&plan, "dev.bundler:.bundle/cache", Class::Safe, true, MIB);
    assert_item(
        &plan,
        "dev.pyenv-cache:.pyenv/cache",
        Class::Safe,
        true,
        MIB,
    );
    assert_item(
        &plan,
        "dev.rbenv-cache:.rbenv/cache",
        Class::Safe,
        true,
        MIB,
    );
    assert_item(&plan, "dev.cache:.cache/pre-commit", Class::Safe, true, MIB);
    for (id, bytes) in [
        ("dev.tool-downloads:.cache/puppeteer", 2 * MIB),
        ("dev.tool-downloads:.cache/selenium", MIB),
        ("dev.tool-downloads:Library/Caches/electron", 2 * MIB),
    ] {
        assert_item(&plan, id, Class::Review, false, bytes);
    }
    let go = assert_item(
        &plan,
        "dev.go-modcache:go/pkg/mod",
        Class::ReportOnly,
        false,
        MIB,
    );
    assert_eq!(
        go.reason,
        "Remove downloaded Go modules with: go clean -modcache"
    );
    // Installed gems and Python versions are never offered.
    for i in &plan.items {
        let p = i.path.to_string_lossy();
        assert!(
            !p.contains("/gems/rails") && !p.contains(".pyenv/versions"),
            "{p}"
        );
    }

    // Each owner blocks its own cache while it runs.
    probe.set(RunningSet::parse("gem,pyenv,poetry,idea,composer,gopls"));
    let busy = scan(&env);
    for (id, quit) in [
        ("dev.gem:.gem/ruby/3.3.0/cache", "Quit RubyGems first"),
        ("dev.pyenv-cache:.pyenv/cache", "Quit pyenv first"),
        (
            "dev.poetry:Library/Caches/pypoetry/cache",
            "Quit Poetry first",
        ),
        (
            "dev.jetbrains:Library/Caches/JetBrains/IntelliJIdea2025.2/caches",
            "Quit your JetBrains IDE first",
        ),
        ("cache.app:Library/Caches/composer", "Quit Composer first"),
        ("cache.app:Library/Caches/gopls", "Quit gopls first"),
        ("cache.app:Library/Caches/go-build", "Quit Go first"),
    ] {
        let item = find(&busy, id);
        assert_eq!(item.blocked.as_deref(), Some(quit), "{id}");
        assert!(!item.ticked, "{id}");
    }
    // A browser build that a test runs right now is in use and not offered.
    probe.set(RunningSet::new([h
        .path(".cache/puppeteer/chrome/mac_arm-131/chrome")
        .to_string_lossy()
        .into_owned()]));
    absent(&scan(&env), "dev.tool-downloads:.cache/puppeteer");
}

// ---------- Old installers ----------

#[test]
fn old_installers_in_downloads_are_review_once_downloads_is_allowed() {
    let h = Home::new();
    h.put("Downloads/Figma-125.dmg", 2 * MIB, 45 * DAY);
    h.put("Downloads/Driver.PKG", MIB, 31 * DAY);
    h.put("Downloads/New.dmg", 2 * MIB, 10 * DAY);
    h.put("Downloads/photos.zip", 2 * MIB, 90 * DAY);
    h.put("Downloads/Old/Inside.dmg", 2 * MIB, 90 * DAY);
    let (env, _) = h.env();
    // Before a Large files scan has let Tidy into Downloads, the folder is not read.
    let plan = scan(&env);
    assert!(
        plan.items.iter().all(|i| i.group != Group::Installers),
        "{:?}",
        ids(&plan)
    );

    let mut env = env;
    env.settings.large_scanned_once = true;
    let plan = scan(&env);
    let dmg = assert_item(
        &plan,
        "downloads.installers:Downloads/Figma-125.dmg",
        Class::Review,
        false,
        2 * MIB,
    );
    assert_eq!(dmg.group, Group::Installers);
    assert_eq!(dmg.name, "Figma-125.dmg");
    assert_eq!(dmg.reason, "In Downloads, not changed for 45 days");
    assert_item(
        &plan,
        "downloads.installers:Downloads/Driver.PKG",
        Class::Review,
        false,
        MIB,
    );
    let installers: Vec<&str> = plan
        .items
        .iter()
        .filter(|i| i.group == Group::Installers)
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(installers.len(), 2, "{installers:?}");
    // Never part of the one-click clean.
    assert!(plan.ticked().all(|i| i.group != Group::Installers));
    // The re-check holds them to the same pattern.
    let item = find(&plan, "downloads.installers:Downloads/Figma-125.dmg").clone();
    assert_eq!(recheck(&env, &item), Ok(item.path.clone()));
}

// ---------- Time budget ----------

#[test]
fn a_rule_that_runs_out_of_time_is_skipped_and_the_rest_goes_on() {
    use std::time::Duration;
    use tidy_core::cleanup::{ScanBudget, scan_cleanup_within, timed_out_line};
    let h = Home::new();
    h.app("Idle.app", "com.example.Idle");
    h.put("Library/Caches/com.example.Idle/data.bin", MIB, 3 * DAY);
    h.put(".npm/_cacache/a.bin", MIB, 5 * DAY);
    h.put("Library/Logs/Foo/old.log", MIB, 20 * DAY);
    let (env, _) = h.env();
    let budget = ScanBudget {
        rules: vec![("dev.npm-cacache", Duration::ZERO)],
        ..ScanBudget::default()
    };
    let plan = scan_cleanup_within(&env, &|_| {}, &Arc::new(AtomicBool::new(false)), &budget);
    absent(&plan, "dev.npm-cacache:.npm/_cacache");
    find(&plan, "cache.app:Library/Caches/com.example.Idle");
    find(&plan, "logs.app:Library/Logs/Foo/old.log");
    assert_eq!(plan.skipped, [timed_out_line("dev.npm-cacache")]);
    assert_eq!(
        plan.skipped[0],
        "Skipped npm cache because it took too long. Scan again to try it."
    );
}

#[test]
fn a_slow_project_walk_stops_part_way_and_offers_nothing_from_it() {
    use std::time::Duration;
    use tidy_core::cleanup::{ScanBudget, scan_cleanup_within};
    let h = Home::new();
    let p = "Documents/projects";
    // Many folders, so the walk takes longer than a millisecond.
    for i in 0..300 {
        h.mark(&format!("{p}/big/src/m{i}/mod.rs"), 60 * DAY);
    }
    h.mark(&format!("{p}/big/Cargo.toml"), 60 * DAY);
    h.put(&format!("{p}/big/target/debug/app"), 2 * MIB, 40 * DAY);
    h.app("Idle.app", "com.example.Idle");
    h.put("Library/Caches/com.example.Idle/data.bin", MIB, 3 * DAY);
    let (env, _) = h.env();
    let budget = ScanBudget {
        rules: vec![("proj", Duration::from_millis(1))],
        ..ScanBudget::default()
    };
    let plan = scan_cleanup_within(&env, &|_| {}, &Arc::new(AtomicBool::new(false)), &budget);
    absent(&plan, &format!("proj.target:{p}/big/target"));
    find(&plan, "cache.app:Library/Caches/com.example.Idle");
    assert_eq!(
        plan.skipped,
        ["Skipped build output in your projects because it took too long. Scan again to try it."]
    );
    // With the normal budget the same project is offered.
    let plan = scan(&env);
    find(&plan, &format!("proj.target:{p}/big/target"));
    assert!(plan.skipped.is_empty());
}
