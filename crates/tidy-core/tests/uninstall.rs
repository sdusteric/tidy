//! The uninstaller (design section 19) against fixture homes in temp folders: which apps are
//! listed, which Library entries count as an app's leftovers, and what the move does. Nothing
//! here reads the real `~/Library` or `/Applications`, and every move goes to a `DirTrash`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidy_core::env::{Env, FixedOpenFiles, FixedProbe};
use tidy_core::model::{Action, ItemResult, RunningSet, Signer, Source};
use tidy_core::security::codesign::{Signature, Signatures};
use tidy_core::uninstall::{
    self, LeftoverKind, NOT_FOUND_AGAIN, UninstallPlan, find_leftovers, list_apps,
};

// ---------- Fixtures ----------

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    tidy: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let tidy = dir.path().join("tidy");
        fs::create_dir_all(home.join("Library")).unwrap();
        fs::create_dir_all(&tidy).unwrap();
        Fixture {
            _dir: dir,
            home,
            tidy,
        }
    }

    fn env(&self) -> Env {
        Env::for_tests(&self.home, &self.tidy)
    }

    /// Writes `Applications/<rel>.app` with an XML Info.plist and a 64 KB executable.
    fn app(&self, rel: &str, id: &str, name: &str, exe: &str) -> PathBuf {
        let app = self.home.join("Applications").join(format!("{rel}.app"));
        write_bundle(&app, id, name, exe);
        app
    }

    fn lib(&self, rel: &str) -> PathBuf {
        self.home.join("Library").join(rel)
    }

    /// A folder under the Library holding one 8 KB file.
    fn folder(&self, rel: &str) -> PathBuf {
        let p = self.lib(rel);
        fill(&p.join("data.bin"), 8192);
        p
    }

    /// A 4 KB file under the Library.
    fn file(&self, rel: &str) -> PathBuf {
        let p = self.lib(rel);
        fill(&p, 4096);
        p
    }
}

fn write_bundle(app: &Path, id: &str, name: &str, exe: &str) {
    let contents = app.join("Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleName</key><string>{name}</string>
<key>CFBundleExecutable</key><string>{exe}</string>
<key>CFBundleShortVersionString</key><string>1.2.3</string>
</dict></plist>"#
        ),
    )
    .unwrap();
    fill(&contents.join("MacOS").join(exe), 65536);
}

fn fill(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; bytes]).unwrap();
}

/// Answers signature checks from a table by app path; everything else is unsigned.
#[derive(Default)]
struct FakeSigner(HashMap<PathBuf, Signature>);

impl FakeSigner {
    fn team(mut self, app: &Path, team_id: &str, name: &str) -> FakeSigner {
        self.0.insert(
            app.to_path_buf(),
            Signature {
                signer: Signer::Team(name.to_string()),
                notarized: Some(true),
                team_id: Some(team_id.to_string()),
            },
        );
        self
    }

    fn apple(mut self, app: &Path) -> FakeSigner {
        self.0.insert(
            app.to_path_buf(),
            Signature {
                signer: Signer::Apple,
                notarized: None,
                team_id: None,
            },
        );
        self
    }
}

impl Signatures for FakeSigner {
    fn check(&self, program: &Path) -> Signature {
        self.0.get(program).cloned().unwrap_or(Signature {
            signer: Signer::Unsigned,
            notarized: Some(false),
            team_id: None,
        })
    }
}

fn plan(f: &Fixture, signer: &FakeSigner, app: &Path) -> UninstallPlan {
    find_leftovers(&f.env(), signer, app).expect("the app is listed")
}

fn paths(plan: &UninstallPlan) -> Vec<PathBuf> {
    plan.items.iter().map(|i| i.path.clone()).collect()
}

fn item<'a>(plan: &'a UninstallPlan, path: &Path) -> &'a uninstall::Leftover {
    plan.items
        .iter()
        .find(|i| i.path == path)
        .unwrap_or_else(|| panic!("{} is not in the plan: {:?}", path.display(), paths(plan)))
}

// ---------- Listing ----------

#[test]
fn lists_apps_and_leaves_out_apple_and_tidy() {
    let f = Fixture::new();
    let slack = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let nested = f.app("Utilities/Thing", "com.example.thing", "Thing", "Thing");
    let pages = f.app("Pages", "com.apple.iWork.Pages", "Pages", "Pages");
    let signed_by_apple = f.app("Odd", "com.example.odd", "Odd", "Odd");
    f.app("Tidy", "dev.sdusteric.tidy", "Tidy", "tidy");
    f.app("Tidy Shots", "dev.sdusteric.tidy.shots", "Tidy", "tidy");
    // A link to an app is not followed, and a folder with no Info.plist is not an app.
    std::os::unix::fs::symlink(&slack, f.home.join("Applications/Link.app")).unwrap();
    fs::create_dir_all(f.home.join("Applications/Empty.app/Contents")).unwrap();
    let signer = FakeSigner::default().apple(&signed_by_apple);

    let list = list_apps(&f.env(), &signer);
    let found: Vec<&Path> = list.apps.iter().map(|a| a.path.as_path()).collect();
    assert_eq!(found, vec![slack.as_path(), nested.as_path()]);
    assert_eq!(list.left_out, 4);
    assert!(!found.contains(&pages.as_path()));
    let s = &list.apps[0];
    assert_eq!(s.name, "Slack");
    assert_eq!(s.version.as_deref(), Some("1.2.3"));
    assert!(s.bytes >= 65536, "{}", s.bytes);
    assert!(s.movable);
}

// ---------- Matching ----------

#[test]
fn finds_each_kind_by_bundle_id() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let id = "com.tinyspeck.slackmacgap";
    let want = [
        (
            f.folder(&format!("Application Support/{id}")),
            LeftoverKind::AppSupport,
        ),
        (f.folder(&format!("Caches/{id}")), LeftoverKind::Caches),
        (
            f.folder(&format!("Caches/{id}.ShipIt")),
            LeftoverKind::Caches,
        ),
        (
            f.file(&format!("Preferences/{id}.plist")),
            LeftoverKind::Preferences,
        ),
        (
            f.file(&format!("Preferences/ByHost/{id}.0A1B2C3D.plist")),
            LeftoverKind::Preferences,
        ),
        (
            f.folder(&format!("Containers/{id}")),
            LeftoverKind::Container,
        ),
        (
            f.folder(&format!("Application Scripts/{id}")),
            LeftoverKind::AppScripts,
        ),
        (
            f.folder(&format!("Saved Application State/{id}.savedState")),
            LeftoverKind::SavedState,
        ),
        (f.folder(&format!("Logs/{id}")), LeftoverKind::Logs),
        (
            f.folder(&format!("HTTPStorages/{id}")),
            LeftoverKind::WebData,
        ),
        (
            f.file(&format!("HTTPStorages/{id}.binarycookies")),
            LeftoverKind::WebData,
        ),
        (f.folder(&format!("WebKit/{id}")), LeftoverKind::WebKit),
        (
            f.file(&format!("LaunchAgents/{id}.updater.plist")),
            LeftoverKind::LaunchAgent,
        ),
    ];
    // Look-alikes: no dot boundary, another vendor, and an unrelated preference.
    f.folder("Caches/com.tinyspeck.slackmacgap2");
    f.folder("Caches/com.tinyspeckle");
    f.file("Preferences/com.other.app.plist");

    let plan = plan(&f, &FakeSigner::default(), &app);
    assert_eq!(plan.items[0].kind, LeftoverKind::App);
    assert_eq!(plan.items[0].path, app);
    assert!(plan.items[0].ticked);
    let mut found = paths(&plan);
    found.remove(0);
    let mut expected: Vec<PathBuf> = want.iter().map(|(p, _)| p.clone()).collect();
    found.sort();
    expected.sort();
    assert_eq!(found, expected);
    for (p, kind) in &want {
        let i = item(&plan, p);
        assert_eq!(i.kind, *kind, "{}", p.display());
        assert!(i.ticked && i.selectable, "{}", p.display());
        assert!(!i.by_name);
    }
    assert_eq!(item(&plan, &want[0].0).bytes, Some(8192));
    assert!(plan.blocked.is_none());
}

#[test]
fn helper_ids_match() {
    let f = Fixture::new();
    let app = f.app("Box", "com.example.box", "Box", "Box");
    write_bundle(
        &app.join("Contents/Library/LoginItems/Box Helper.app"),
        "net.example.boxhelper",
        "Box Helper",
        "BoxHelper",
    );
    write_bundle(
        &app.join("Contents/PlugIns/Share.appex"),
        "org.example.share",
        "Share",
        "Share",
    );
    let helper = f.folder("Containers/net.example.boxhelper");
    let share = f.folder("Containers/org.example.share");
    let plan = plan(&f, &FakeSigner::default(), &app);
    assert!(item(&plan, &helper).ticked);
    assert!(item(&plan, &share).ticked);

    // A running helper blocks the whole uninstall.
    let env = f
        .env()
        .with_procs(Arc::new(FixedProbe::new(RunningSet::new(["BoxHelper"]))));
    let p = find_leftovers(&env, &FakeSigner::default(), &app).unwrap();
    assert_eq!(p.blocked.as_deref(), Some("Quit Box first"));
}

#[test]
fn longer_id_of_another_app_wins() {
    let f = Fixture::new();
    let chrome = f.app(
        "Google Chrome",
        "com.google.Chrome",
        "Google Chrome",
        "Google Chrome",
    );
    f.app(
        "Google Chrome Canary",
        "com.google.Chrome.canary",
        "Google Chrome Canary",
        "Google Chrome Canary",
    );
    let own = f.folder("Caches/com.google.Chrome");
    let ours_too = f.file("Preferences/com.google.Chrome.helper.plist");
    let canary = f.folder("Caches/com.google.Chrome.canary");
    let canary_prefs = f.file("Preferences/com.google.Chrome.canary.plist");
    let plan = plan(&f, &FakeSigner::default(), &chrome);
    let found = paths(&plan);
    assert!(found.contains(&own));
    assert!(found.contains(&ours_too));
    assert!(!found.contains(&canary), "{found:?}");
    assert!(!found.contains(&canary_prefs), "{found:?}");
}

#[test]
fn names_match_only_when_unique_and_not_vendor() {
    let f = Fixture::new();
    let code = f.app(
        "Visual Studio Code",
        "com.microsoft.VSCode",
        "Code",
        "Electron",
    );
    let notes = f.app("Notes Pro", "com.example.notespro", "Notes", "Notes");
    // Another installed app with the same name, so "Notes" folders are nobody's for sure.
    f.app(
        "Other Notes",
        "org.example.othernotes",
        "Notes",
        "OtherNotes",
    );
    let support = f.folder("Application Support/Code");
    let logs = f.folder("Logs/Code");
    let caches = f.folder("Caches/Visual Studio Code");
    let electron = f.folder("Application Support/Electron");
    let notes_support = f.folder("Application Support/Notes");
    // Name matches only count where apps keep folders by name.
    let prefs = f.file("Preferences/Code.plist");

    let p = plan(&f, &FakeSigner::default(), &code);
    for path in [&support, &logs, &caches] {
        let i = item(&p, path);
        assert!(i.by_name && i.ticked, "{}", path.display());
    }
    let found = paths(&p);
    assert!(!found.contains(&electron), "vendor word");
    assert!(!found.contains(&prefs));

    let p = plan(&f, &FakeSigner::default(), &notes);
    assert!(!paths(&p).contains(&notes_support), "shared name");
}

#[test]
fn shared_team_container_is_review() {
    let f = Fixture::new();
    let word = f.app(
        "Microsoft Word",
        "com.microsoft.Word",
        "Microsoft Word",
        "Microsoft Word",
    );
    let excel = f.app(
        "Microsoft Excel",
        "com.microsoft.Excel",
        "Microsoft Excel",
        "Microsoft Excel",
    );
    let signer = FakeSigner::default()
        .team(&word, "UBF8T346G9", "Microsoft Corporation")
        .team(&excel, "UBF8T346G9", "Microsoft Corporation");
    let shared = f.folder("Group Containers/UBF8T346G9.Office");
    let own = f.folder("Group Containers/UBF8T346G9.com.microsoft.Word");
    let excels = f.folder("Group Containers/UBF8T346G9.com.microsoft.Excel");
    let other_team = f.folder("Group Containers/ABCDE12345.Office");

    let p = plan(&f, &signer, &word);
    let s = item(&p, &shared);
    assert_eq!(s.kind, LeftoverKind::GroupContainer);
    assert!(!s.ticked && s.selectable);
    assert_eq!(s.note.as_deref(), Some("Shared with Microsoft Excel"));
    assert!(item(&p, &own).ticked);
    let found = paths(&p);
    assert!(!found.contains(&excels));
    assert!(!found.contains(&other_team));
}

#[test]
fn own_team_container_is_ticked() {
    let f = Fixture::new();
    let app = f.app("Things", "com.culturedcode.ThingsMac", "Things", "Things");
    let signer = FakeSigner::default().team(&app, "JLMPQHK86H", "Cultured Code");
    let group = f.folder("Group Containers/JLMPQHK86H.com.culturedcode.ThingsMac");
    let team_wide = f.folder("Group Containers/JLMPQHK86H.shared");
    let by_group = f.folder("Group Containers/group.com.culturedcode.ThingsMac");
    let p = plan(&f, &signer, &app);
    for path in [&group, &team_wide, &by_group] {
        assert!(item(&p, path).ticked, "{}", path.display());
    }
    assert_eq!(p.app.team_id.as_deref(), Some("JLMPQHK86H"));
}

#[test]
fn launch_agent_by_program_path() {
    let f = Fixture::new();
    let app = f.app("Sync", "com.example.sync", "Sync", "Sync");
    let agent = f.lib("LaunchAgents/net.vendor.agent.plist");
    fs::create_dir_all(agent.parent().unwrap()).unwrap();
    fs::write(
        &agent,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>net.vendor.agent</string>
<key>ProgramArguments</key><array><string>{}/Contents/MacOS/Sync</string></array>
</dict></plist>"#,
            app.display()
        ),
    )
    .unwrap();
    let unrelated = f.lib("LaunchAgents/net.vendor.other.plist");
    fs::write(
        &unrelated,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Label</key><string>net.vendor.other</string>
<key>Program</key><string>/usr/local/bin/other</string></dict></plist>"#,
    )
    .unwrap();
    let p = plan(&f, &FakeSigner::default(), &app);
    assert_eq!(item(&p, &agent).kind, LeftoverKind::LaunchAgent);
    assert!(!paths(&p).contains(&unrelated));
}

#[test]
fn system_library_is_report_only() {
    let f = Fixture::new();
    let app = f.app("Sync", "com.example.sync", "Sync", "Sync");
    let env = f.env();
    let system = uninstall::system_library(&env);
    assert_eq!(system, f.tidy.join("system/Library"));
    let daemon = system.join("LaunchDaemons/com.example.sync.helper.plist");
    fill(&daemon, 4096);
    let tool = system.join("PrivilegedHelperTools/com.example.sync.helper");
    fill(&tool, 4096);
    let p = plan(&f, &FakeSigner::default(), &app);
    for path in [&daemon, &tool] {
        let i = item(&p, path);
        assert_eq!(i.kind, LeftoverKind::System);
        assert!(!i.ticked && !i.selectable);
        assert_eq!(i.note.as_deref(), Some(uninstall::NEEDS_ADMIN_NOTE));
    }
    // Report only rows come last.
    assert_eq!(p.items.last().unwrap().kind, LeftoverKind::System);
}

#[test]
fn root_owned_app_is_offered_through_finder() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let app = f.app("Store", "com.example.store", "Store", "Store");
    let cache = f.folder("Caches/com.example.store");
    fs::set_permissions(&app, fs::Permissions::from_mode(0o555)).unwrap();
    let p = plan(&f, &FakeSigner::default(), &app);
    let a = &p.items[0];
    assert!(a.selectable && a.ticked);
    assert_eq!(a.note.as_deref(), Some(uninstall::APP_FINDER_NOTE));
    assert!(!p.app.movable);
    assert!(item(&p, &cache).ticked);
    fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn running_app_is_blocked() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let env = f
        .env()
        .with_procs(Arc::new(FixedProbe::new(RunningSet::new([
            "com.tinyspeck.slackmacgap",
        ]))));
    let p = find_leftovers(&env, &FakeSigner::default(), &app).unwrap();
    assert_eq!(p.blocked.as_deref(), Some("Quit Slack first"));
    let env = f
        .env()
        .with_procs(Arc::new(FixedProbe::new(RunningSet::unknown())));
    let p = find_leftovers(&env, &FakeSigner::default(), &app).unwrap();
    assert_eq!(
        p.blocked.as_deref(),
        Some("Tidy couldn't check whether Slack is open")
    );
}

#[test]
fn apple_and_unlisted_apps_have_no_plan() {
    let f = Fixture::new();
    let pages = f.app("Pages", "com.apple.iWork.Pages", "Pages", "Pages");
    assert!(find_leftovers(&f.env(), &FakeSigner::default(), &pages).is_none());
    let elsewhere = f.home.join("Downloads/Thing.app");
    write_bundle(&elsewhere, "com.example.thing", "Thing", "Thing");
    assert!(find_leftovers(&f.env(), &FakeSigner::default(), &elsewhere).is_none());
}

#[test]
fn tidys_own_folders_are_never_leftovers() {
    let f = Fixture::new();
    // A third-party app called Tidy, and Tidy's own folders inside the Library, as in
    // production (~/Library/Caches/Tidy).
    let app = f.app("Tidy Up", "com.example.tidyup", "Tidy", "TidyUp");
    let own = f.lib("Caches/Tidy");
    fs::create_dir_all(own.join("data")).unwrap();
    let theirs = f.folder("Logs/Tidy");
    let env = Env::for_tests(&f.home, &own);
    let p = find_leftovers(&env, &FakeSigner::default(), &app).unwrap();
    assert!(!paths(&p).contains(&own), "{:?}", paths(&p));
    assert!(item(&p, &theirs).by_name);
}

// ---------- Moving ----------

fn listing(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            out.push(p.clone());
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn ticked_ids(plan: &UninstallPlan) -> Vec<String> {
    plan.items
        .iter()
        .filter(|i| i.ticked)
        .map(|i| i.id.clone())
        .collect()
}

#[test]
fn moves_app_and_ticked_leftovers_to_dir_trash() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let other = f.app("Notes Pro", "com.example.notespro", "Notes Pro", "NotesPro");
    let cache = f.folder("Caches/com.tinyspeck.slackmacgap");
    let prefs = f.file("Preferences/com.tinyspeck.slackmacgap.plist");
    let container = f.folder("Containers/com.tinyspeck.slackmacgap");
    let kept = f.folder("Caches/com.example.notespro");
    let env = f.env();
    let signer = FakeSigner::default();
    let plan = find_leftovers(&env, &signer, &app).unwrap();
    // Leave the preferences: the user unticked them.
    let ids: Vec<String> = ticked_ids(&plan)
        .into_iter()
        .filter(|id| Path::new(id) != prefs)
        .collect();
    let before = listing(&f.home);
    let mut events = Vec::new();
    let entry = uninstall::uninstall(&env, &signer, &app, &ids, &mut |e| events.push(e));

    assert_eq!(entry.action, Action::Uninstall);
    assert_eq!(entry.source, Source::Uninstaller);
    let moved: Vec<&Path> = entry
        .items
        .iter()
        .filter(|i| i.result == ItemResult::Trashed)
        .map(|i| i.path.as_path())
        .collect();
    assert_eq!(
        moved,
        vec![app.as_path(), cache.as_path(), container.as_path()]
    );
    assert!(entry.trashed_bytes >= 65536 + 2 * 8192);
    assert!(!app.exists() && !cache.exists() && !container.exists());
    assert!(prefs.exists() && kept.exists() && other.exists());
    let after = listing(&f.home);
    let gone: Vec<&PathBuf> = before.iter().filter(|p| !after.contains(p)).collect();
    assert!(
        gone.iter()
            .all(|p| p.starts_with(&app) || p.starts_with(&cache) || p.starts_with(&container)),
        "{gone:?}"
    );
    assert!(f.tidy.join("data/trash/Slack.app").exists());
    let history = fs::read_to_string(f.tidy.join("data/history.jsonl")).unwrap();
    assert!(history.contains(r#""action":"uninstall""#), "{history}");
    assert!(history.contains(r#""source":"uninstaller""#), "{history}");

    // Empty Trash counts them like any other move.
    let summary = tidy_core::history::trash_summary(&env.paths, env.trasher.kind());
    assert_eq!(summary.items, 3);
}

#[test]
fn unknown_ids_are_skipped() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let other = f.app("Notes Pro", "com.example.notespro", "Notes Pro", "NotesPro");
    let elsewhere = f.folder("Caches/com.example.notespro");
    let outside = f.home.join("Documents/keep.txt");
    fill(&outside, 4096);
    let ids = vec![
        other.to_string_lossy().into_owned(),
        elsewhere.to_string_lossy().into_owned(),
        outside.to_string_lossy().into_owned(),
    ];
    let entry = uninstall::uninstall(&f.env(), &FakeSigner::default(), &app, &ids, &mut |_| {});
    assert_eq!(entry.items.len(), 3);
    for i in &entry.items {
        assert_eq!(i.result, ItemResult::Skipped, "{}", i.path.display());
        assert_eq!(i.reason.as_deref(), Some(NOT_FOUND_AGAIN));
    }
    assert!(other.exists() && elsewhere.exists() && outside.exists() && app.exists());
}

#[test]
fn running_at_move_time_skips_everything() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let cache = f.folder("Caches/com.tinyspeck.slackmacgap");
    let probe = Arc::new(FixedProbe::default());
    let env = f.env().with_procs(probe.clone());
    let plan = find_leftovers(&env, &FakeSigner::default(), &app).unwrap();
    assert!(plan.blocked.is_none());
    probe.set(RunningSet::new(["Slack"]));
    let entry = uninstall::uninstall(
        &env,
        &FakeSigner::default(),
        &app,
        &ticked_ids(&plan),
        &mut |_| {},
    );
    assert!(entry.items.iter().all(|i| i.result == ItemResult::Skipped));
    assert_eq!(entry.items[0].reason.as_deref(), Some("Quit Slack first"));
    assert!(app.exists() && cache.exists());
}

#[test]
fn linked_library_folder_is_refused() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    // Library/Caches points outside the home folder.
    let outside = f.tidy.join("elsewhere");
    fill(&outside.join("com.tinyspeck.slackmacgap/data.bin"), 4096);
    std::os::unix::fs::symlink(&outside, f.lib("Caches")).unwrap();
    let p = plan(&f, &FakeSigner::default(), &app);
    assert!(
        p.items.iter().all(|i| !i.path.starts_with(f.lib("Caches"))),
        "{:?}",
        paths(&p)
    );
    let id = f
        .lib("Caches/com.tinyspeck.slackmacgap")
        .to_string_lossy()
        .into_owned();
    let entry = uninstall::uninstall(&f.env(), &FakeSigner::default(), &app, &[id], &mut |_| {});
    assert_eq!(entry.items[0].result, ItemResult::Skipped);
    assert!(outside.join("com.tinyspeck.slackmacgap/data.bin").exists());
}

#[test]
fn report_only_is_never_moved() {
    let f = Fixture::new();
    let app = f.app("Sync", "com.example.sync", "Sync", "Sync");
    let env = f.env();
    let daemon = uninstall::system_library(&env).join("LaunchDaemons/com.example.sync.plist");
    fill(&daemon, 4096);
    let id = daemon.to_string_lossy().into_owned();
    let entry = uninstall::uninstall(&env, &FakeSigner::default(), &app, &[id], &mut |_| {});
    assert_eq!(entry.items[0].result, ItemResult::Skipped);
    assert!(daemon.exists());
}

#[test]
fn ignore_list_and_open_files_skip() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let cache = f.folder("Caches/com.tinyspeck.slackmacgap");
    let logs = f.folder("Logs/com.tinyspeck.slackmacgap");
    let mut settings = tidy_core::settings::Settings::default();
    settings.ignored_paths.push(cache.clone());
    let open =
        FixedOpenFiles::default().with(logs.canonicalize().unwrap().join("data.bin"), "Console");
    let env = f
        .env()
        .with_settings(settings)
        .with_open_files(Arc::new(open));
    let signer = FakeSigner::default();
    let plan = find_leftovers(&env, &signer, &app).unwrap();
    let ids: Vec<String> = ticked_ids(&plan)
        .into_iter()
        .filter(|id| Path::new(id) != app)
        .collect();
    let entry = uninstall::uninstall(&env, &signer, &app, &ids, &mut |_| {});
    let reason = |p: &Path| {
        entry
            .items
            .iter()
            .find(|i| i.path == p)
            .and_then(|i| i.reason.clone())
    };
    assert_eq!(reason(&cache).as_deref(), Some("On your ignore list"));
    assert_eq!(reason(&logs).as_deref(), Some("Open in Console, skipped"));
    assert!(cache.exists() && logs.exists());
}

/// A Trash that refuses everything the way macOS refuses an app it protects.
struct RefusingTrash;

impl tidy_core::env::Trasher for RefusingTrash {
    fn trash(&self, _path: &Path) -> std::io::Result<Option<PathBuf>> {
        Err(std::io::Error::from_raw_os_error(libc::EPERM))
    }
}

/// Stands in for Finder: records what it was asked to move and moves nothing, or fails with
/// the sentence Finder's error maps to.
#[derive(Default)]
struct FakeFinder {
    asked: std::sync::Mutex<Vec<PathBuf>>,
    fails_with: Option<String>,
}

impl tidy_core::env::Trasher for FakeFinder {
    fn trash(&self, path: &Path) -> std::io::Result<Option<PathBuf>> {
        self.asked.lock().unwrap().push(path.to_path_buf());
        match &self.fails_with {
            Some(m) => Err(std::io::Error::other(m.clone())),
            None => Ok(Some(
                PathBuf::from("/fake/.Trash").join(path.file_name().unwrap()),
            )),
        }
    }
}

#[test]
fn a_refused_app_move_goes_to_finder() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let finder = Arc::new(FakeFinder::default());
    let env = f
        .env()
        .with_trasher(Arc::new(RefusingTrash))
        .with_admin_trasher(finder.clone());
    let id = app.to_string_lossy().into_owned();
    let entry = uninstall::uninstall(&env, &FakeSigner::default(), &app, &[id], &mut |_| {});
    assert_eq!(
        entry.items[0].result,
        ItemResult::Trashed,
        "{:?}",
        entry.items[0]
    );
    assert_eq!(
        entry.items[0].trash_path.as_deref(),
        Some(Path::new("/fake/.Trash/Slack.app"))
    );
    assert_eq!(*finder.asked.lock().unwrap(), [app.canonicalize().unwrap()]);
}

#[test]
fn a_needs_admin_app_goes_straight_to_finder_and_leftovers_never_do() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let app = f.app("Store", "com.example.store", "Store", "Store");
    let cache = f.folder("Caches/com.example.store");
    fs::set_permissions(&app, fs::Permissions::from_mode(0o555)).unwrap();
    let finder = Arc::new(FakeFinder::default());
    let env = f.env().with_admin_trasher(finder.clone());
    let p = find_leftovers(&env, &FakeSigner::default(), &app).unwrap();
    let entry = uninstall::uninstall(
        &env,
        &FakeSigner::default(),
        &app,
        &ticked_ids(&p),
        &mut |_| {},
    );
    fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        entry.items.iter().all(|i| i.result == ItemResult::Trashed),
        "{:?}",
        entry.items
    );
    // Finder was asked for the app only; the cache went to Tidy's own Trash.
    assert_eq!(*finder.asked.lock().unwrap(), [app.canonicalize().unwrap()]);
    assert!(!cache.exists());
}

#[test]
fn a_canceled_finder_prompt_keeps_the_app_and_says_why() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let finder = Arc::new(FakeFinder {
        fails_with: Some(tidy_core::trash::FINDER_CANCELED.to_string()),
        ..FakeFinder::default()
    });
    let env = f
        .env()
        .with_trasher(Arc::new(RefusingTrash))
        .with_admin_trasher(finder);
    let id = app.to_string_lossy().into_owned();
    let entry = uninstall::uninstall(&env, &FakeSigner::default(), &app, &[id], &mut |_| {});
    assert_eq!(entry.items[0].result, ItemResult::Failed);
    assert_eq!(
        entry.items[0].reason.as_deref(),
        Some(tidy_core::trash::FINDER_CANCELED)
    );
    assert!(app.exists());
}

#[test]
fn the_test_env_never_reaches_finder() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    assert!(f.env().admin_trasher.trash(&app).is_err());
    assert!(app.exists());
}

#[test]
fn empty_and_dotless_ids_match_nothing() {
    let f = Fixture::new();
    let app = f.app("Blank", "", "Blank", "Blank");
    let odd = f.app("Odd", "odd", "Odd", "Odd");
    let host = f.app("Host", "com.example.host", "Host", "Host");
    write_bundle(
        &host.join("Contents/PlugIns/Empty.appex"),
        "",
        "Empty",
        "Empty",
    );
    let global = f.file("Preferences/.GlobalPreferences.plist");
    let by_host = f.file("Preferences/ByHost/.GlobalPreferences.0A1B.plist");
    let odd_cache = f.folder("Caches/odd");
    let list = list_apps(&f.env(), &FakeSigner::default());
    assert!(!list.apps.iter().any(|a| a.path == app || a.path == odd));
    let p = plan(&f, &FakeSigner::default(), &host);
    for path in [&global, &by_host, &odd_cache] {
        assert!(!paths(&p).contains(path), "{}", path.display());
    }
}

#[test]
fn a_second_copy_of_the_app_keeps_its_data_unticked() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    f.app(
        "Utilities/Slack 2",
        "com.tinyspeck.slackmacgap",
        "Slack",
        "Slack",
    );
    let cache = f.folder("Caches/com.tinyspeck.slackmacgap");
    let p = plan(&f, &FakeSigner::default(), &app);
    let c = item(&p, &cache);
    assert!(!c.ticked && c.selectable);
    assert_eq!(c.note.as_deref(), Some("Also used by Slack 2"));
    assert!(p.items[0].ticked, "the app itself is still ticked");
}

#[test]
fn apple_ids_inside_a_third_party_app_are_never_matched() {
    let f = Fixture::new();
    let app = f.app("Box", "com.example.box", "Box", "Box");
    write_bundle(
        &app.join("Contents/PlugIns/Spot.appex"),
        "com.apple.spotlight.box",
        "Spot",
        "Spot",
    );
    let apple = f.folder("Containers/com.apple.spotlight.box");
    let p = plan(&f, &FakeSigner::default(), &app);
    assert!(!paths(&p).contains(&apple));
}

#[test]
fn a_launch_agent_path_with_dot_dot_is_not_the_apps() {
    let f = Fixture::new();
    let app = f.app("Sync", "com.example.sync", "Sync", "Sync");
    f.app("Other", "org.example.other", "Other", "Other");
    let agent = f.lib("LaunchAgents/net.vendor.agent.plist");
    fs::create_dir_all(agent.parent().unwrap()).unwrap();
    fs::write(
        &agent,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Label</key><string>net.vendor.agent</string>
<key>Program</key><string>{}/../Other.app/Contents/MacOS/Other</string>
</dict></plist>"#,
            app.display()
        ),
    )
    .unwrap();
    let p = plan(&f, &FakeSigner::default(), &app);
    assert!(!paths(&p).contains(&agent));
}

#[test]
fn a_stopped_uninstall_moves_nothing_more() {
    let f = Fixture::new();
    let app = f.app("Slack", "com.tinyspeck.slackmacgap", "Slack", "Slack");
    let cache = f.folder("Caches/com.tinyspeck.slackmacgap");
    let env = f.env();
    let p = plan(&f, &FakeSigner::default(), &app);
    let stop = std::sync::atomic::AtomicBool::new(true);
    let entry = uninstall::uninstall_until(
        &env,
        &FakeSigner::default(),
        &app,
        &ticked_ids(&p),
        &mut |_| {},
        Some(&stop),
    );
    assert!(entry.items.iter().all(|i| i.result == ItemResult::Skipped));
    assert!(app.exists() && cache.exists());
}

#[test]
fn app_store_apps_say_app_store_not_apples_signing_name() {
    let f = Fixture::new();
    let app = f.app("Store", "com.example.store", "Store", "Store");
    let signer = FakeSigner::default().team(&app, "ABCDE12345", "Apple Mac OS Application Signing");
    let list = list_apps(&f.env(), &signer);
    assert_eq!(list.apps[0].team_name.as_deref(), Some("App Store"));
    assert_eq!(list.apps[0].team_id.as_deref(), Some("ABCDE12345"));
}
