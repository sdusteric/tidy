//! The standard fake home: every case in the design's "What the standard fixture contains",
//! and the results the scans must produce for it.
//!
//! The expected results are written by hand from the rules in design sections 6.1 to 6.3 and
//! 7, not by running any scan. Which paths belong to which rule, their class, whether they are
//! ticked and why one is blocked are stated here as literals. Only the byte counts come from
//! the [`Ledger`], which holds the sizes the builder declared (and the build test proves the
//! disk agrees with them).
//!
//! Layout under `<dir>`:
//!
//! - `home/` is the fake home folder (`TIDY_SCAN_ROOT`).
//! - `outside/` holds a file that a symlink inside `home/` points at.
//! - The `tidy-fixture-home` binary adds `tidy/` (`TIDY_HOME`).
//!
//! Every cleanup item is made of files of 3 MiB or less, so none of them is a large file on
//! its own; the large-file results come only from the files built for that purpose.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tidy_core::model::{Class, Group, LargeKind};

use crate::tree::{Ledger, Tree};

/// The clock the standard fixture uses unless a caller picks one: the same value as
/// `tidy_core::env::TEST_NOW` (2026-10-23T14:02:11Z).
pub const FIXTURE_NOW: u64 = 1_792_764_131;

/// One mebibyte.
pub const MIB: u64 = 1_048_576;
const KIB: u64 = 1024;

/// The threshold `tidyctl large --min-mb 4` uses: 4 MB, in bytes (MB is 1,000,000).
pub const LARGE_MIN_BYTES: u64 = 4_000_000;
/// Days after which an item counts as not opened lately.
pub const NOT_OPENED_DAYS: u32 = 90;

/// The process names the standard fixture runs with (`TIDY_TEST_RUNNING`).
pub const RUNNING: [&str; 1] = ["com.google.Chrome"];

/// The blocked reason for an item whose owner is running.
pub const BLOCKED_CHROME: &str = "Quit Google Chrome first";

/// Rule ids used in item ids (`<rule id>:<path relative to the scan root>`). They are the ids
/// of the rule table in `tidy_core::rules` (project build output uses the `proj.*` ids).
pub mod rule {
    pub const CACHE_APP: &str = "cache.app";
    pub const CACHE_CHROME: &str = "cache.chrome";
    pub const CACHE_UNKNOWN: &str = "cache.unknown";
    pub const LOGS_APP: &str = "logs.app";
    pub const LOGS_CRASH: &str = "logs.diag";
    pub const DERIVED_DATA: &str = "dev.deriveddata";
    pub const BUILD_TARGET: &str = "proj.target";
    pub const NODE_MODULES: &str = "proj.node_modules";
    pub const BUILD_OUTPUT: &str = "proj.dist";
    pub const NPM_CACHE: &str = "dev.npm-cacache";
    pub const HUGGINGFACE: &str = "ai.huggingface";
    pub const VM_DISK: &str = "ai.vm";
}

/// Folders the Large files scan never walks: the ones the hard denylist (design section 7)
/// hides, as the fixture uses them, and Tidy's own folders (`tidy_core::paths::OWN_FOLDERS`).
/// Their bytes are in no folder total.
const NOT_SCANNED: [&str; 6] = [
    "Library/Containers",
    "Library/Group Containers",
    ".ssh",
    ".config",
    "Library/Caches/Tidy",
    "Library/Logs/Tidy",
];

/// One item the cleanup scan must report. The JSON form is what `expected-plan.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedItem {
    /// `<rule id>:<path relative to the scan root>`.
    pub id: String,
    pub group: Group,
    pub class: Class,
    pub ticked: bool,
    /// Why the item cannot be ticked now ("Quit Google Chrome first"), or None.
    pub blocked: Option<String>,
    pub bytes_alloc: u64,
}

impl ExpectedItem {
    pub fn rule_id(&self) -> &str {
        self.id.split_once(':').map_or("", |(r, _)| r)
    }

    /// The item's path relative to the scan root.
    pub fn rel(&self) -> &str {
        self.id.split_once(':').map_or("", |(_, p)| p)
    }
}

/// `expected-plan.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedPlan {
    /// The value for `TIDY_TEST_NOW`.
    pub now: u64,
    /// The value for `TIDY_TEST_RUNNING`, split on commas.
    pub running: Vec<String>,
    /// Every item the final plan must hold and nothing else, sorted the way `Plan::sort`
    /// sorts: group order, then allocated bytes descending, then id.
    pub items: Vec<ExpectedItem>,
    /// Paths relative to the scan root that must not appear anywhere in the scan output: the
    /// excluded folders and the matches the rules decline to offer.
    pub never: Vec<String>,
}

impl ExpectedPlan {
    /// The ticked items that nothing blocks, which is what `clean --safe` moves.
    pub fn clean_set(&self) -> Vec<&ExpectedItem> {
        self.items
            .iter()
            .filter(|i| i.ticked && i.blocked.is_none() && i.class == Class::Safe)
            .collect()
    }
}

/// One item `tidyctl large` must list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedLargeItem {
    /// Path relative to the scan root.
    pub rel: String,
    pub name: String,
    /// `file` or `package`.
    pub kind: LargeKind,
    pub bytes_alloc: u64,
    pub bytes_logical: u64,
    /// True for items under `Library`.
    pub app_data: bool,
}

/// The total a folder must report in the `--folders` tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedFolder {
    /// Path relative to the scan root. Empty for the root.
    pub rel: String,
    pub bytes_alloc: u64,
    pub bytes_logical: u64,
}

/// `expected-large.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedLarge {
    pub now: u64,
    /// The threshold of `--min-mb 4`, in bytes.
    pub min_bytes: u64,
    pub not_opened_days: u32,
    /// Exactly the items of `large --min-mb 4`, largest first (ties by path).
    pub items: Vec<ExpectedLargeItem>,
    /// The `rel` of the items `--not-opened` lists.
    pub not_opened: Vec<String>,
    /// Folder totals that `--folders` must report. A hard-linked file counts once, a sparse
    /// file counts its allocated bytes (none), and the denylisted folders are not counted.
    pub folders: Vec<ExpectedFolder>,
    /// Existing paths relative to the scan root that must not be listed as items: a sparse
    /// file with a large logical size, and the hard-linked pair (each link is under 4 MB).
    pub absent_items: Vec<String>,
    /// Substrings that must not appear anywhere in the scan output: the denylisted folders and
    /// what only a followed symlink would reach.
    pub never: Vec<String>,
}

/// Everything the standard fixture expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    pub plan: ExpectedPlan,
    pub large: ExpectedLarge,
}

/// Builds fake homes.
pub struct FakeHome;

impl FakeHome {
    /// Builds the standard fixture under `dir` (`home/` and `outside/`) with times relative to
    /// `now`, and returns the expected results. `dir` must pass [`crate::check_target_dir`].
    pub fn standard(dir: &Path, now: u64) -> Result<Expected> {
        Ok(Self::standard_with_ledger(dir, now)?.0)
    }

    /// Like [`FakeHome::standard`], also returning the ledger of the `home/` tree.
    pub fn standard_with_ledger(dir: &Path, now: u64) -> Result<(Expected, Ledger)> {
        let dir = crate::check_target_dir(dir)?;
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        // A file outside the home that a symlink inside it points at.
        let mut outside = Tree::new(dir.join("outside"), now)?;
        outside.file("huge-outside.bin", 8 * MIB)?;
        outside.finish()?;

        let mut t = Tree::new(dir.join("home"), now)?;
        build_home(&mut t, &dir.join("outside"))?;
        let ledger = t.finish()?;
        let expected = expected_for(&ledger, now);
        Ok((expected, ledger))
    }
}

/// Files named `part-<n>.bin` under `base`, one per entry of `mib`.
fn parts(t: &mut Tree, base: &str, mib: &[u64]) -> Result<()> {
    for (i, m) in mib.iter().enumerate() {
        t.file(&format!("{base}/part-{i}.bin"), m * MIB)?;
    }
    Ok(())
}

fn build_home(t: &mut Tree, outside: &Path) -> Result<()> {
    // Apps that exist on this fake Mac. Idle.app owns the safe cache below.
    t.app("Applications/Idle.app", "com.example.idle", "Idle")?;
    t.app("Applications/Big.app", "com.example.big", "Big")?;
    // Big.app: many small files that add up past the large-file threshold. One item.
    for i in 0..64 {
        t.file(
            &format!("Applications/Big.app/Contents/Resources/res-{i:02}.dat"),
            96 * KIB,
        )?;
    }

    // --- System and app caches ---
    // Safe: the cache of an installed app that is not running.
    parts(t, "Library/Caches/com.example.idle", &[2, 2, 2])?;
    // Blocked: Chrome is in TIDY_TEST_RUNNING.
    parts(t, "Library/Caches/Google/Chrome/Default", &[3, 2])?;
    // Excluded: macOS-managed.
    parts(t, "Library/Caches/com.apple.nsurlsessiond", &[3])?;
    // Review: no known owner.
    parts(t, "Library/Caches/mystery-cache", &[3])?;
    // Excluded: Tidy's own folders.
    parts(t, "Library/Caches/Tidy", &[2])?;
    parts(t, "Library/Logs/Tidy", &[2])?;
    // A symlink to a folder outside the home. It must never be followed.
    t.symlink("Documents/linkdir", outside)?;

    // --- Logs ---
    t.file("Library/Logs/Idle/old.log", 2 * MIB)?;
    t.age_days("Library/Logs/Idle/old.log", 20)?;
    t.file("Library/Logs/Idle/new.log", 2 * MIB)?;
    t.age_days("Library/Logs/Idle/new.log", 3)?;
    t.file("Library/Logs/DiagnosticReports/crash-old.ips", 3 * MIB)?;
    t.age_days("Library/Logs/DiagnosticReports/crash-old.ips", 45)?;
    t.file("Library/Logs/DiagnosticReports/crash-new.ips", 3 * MIB)?;
    t.age_days("Library/Logs/DiagnosticReports/crash-new.ips", 10)?;

    // --- Developer ---
    let derived = "Library/Developer/Xcode/DerivedData";
    parts(t, &format!("{derived}/OldProj-abc"), &[3, 3])?;
    t.age_days(&format!("{derived}/OldProj-abc"), 10)?;
    parts(t, &format!("{derived}/NewProj-def"), &[3, 3])?;
    t.age_days(&format!("{derived}/NewProj-def"), 2)?;

    // An idle Rust project: the target folder is offered.
    let cargo = "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n";
    let p = "Documents/projects";
    t.text(&format!("{p}/idle-rs/Cargo.toml"), cargo)?;
    t.text(&format!("{p}/idle-rs/src/main.rs"), "fn main() {}\n")?;
    t.text(
        &format!("{p}/idle-rs/target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n# This file is a cache directory tag created by cargo.\n",
    )?;
    parts(t, &format!("{p}/idle-rs/target/debug/deps"), &[2, 2, 2])?;
    // An active Rust project: a source file changed yesterday, so nothing is offered.
    t.text(&format!("{p}/active-rs/Cargo.toml"), cargo)?;
    t.text(&format!("{p}/active-rs/src/main.rs"), "fn main() {}\n")?;
    t.age_days(&format!("{p}/active-rs/src/main.rs"), 1)?;
    parts(t, &format!("{p}/active-rs/target/debug/deps"), &[3, 2])?;
    // An old node project: node_modules is offered.
    t.text(
        &format!("{p}/web-old/package.json"),
        "{\"name\":\"web-old\"}\n",
    )?;
    t.text(
        &format!("{p}/web-old/node_modules/pkg-a/package.json"),
        "{\"name\":\"pkg-a\"}\n",
    )?;
    parts(
        t,
        &format!("{p}/web-old/node_modules/pkg-a/lib"),
        &[2, 2, 2],
    )?;
    // dist that .gitignore does not name: not offered.
    t.text(
        &format!("{p}/site-a/package.json"),
        "{\"name\":\"site-a\"}\n",
    )?;
    t.text(&format!("{p}/site-a/.gitignore"), "*.log\n")?;
    parts(t, &format!("{p}/site-a/dist"), &[3])?;
    // dist that .gitignore names: Review.
    t.text(
        &format!("{p}/site-b/package.json"),
        "{\"name\":\"site-b\"}\n",
    )?;
    t.text(&format!("{p}/site-b/.gitignore"), "dist/\n")?;
    parts(t, &format!("{p}/site-b/dist"), &[3])?;

    parts(t, ".npm/_cacache", &[2, 2, 2])?;

    // --- AI models and large app data ---
    parts(
        t,
        ".cache/huggingface/hub/models--acme--tiny-model/blobs",
        &[2, 2],
    )?;
    parts(t, ".colima/_lima/colima", &[3, 3])?;

    // --- Large files ---
    // A file opened 3 days ago and one 200 days ago.
    t.file("Movies/trip.mov", 8 * MIB)?;
    t.age_days("Movies/trip.mov", 3)?;
    t.file("Downloads/old-installer.dmg", 6 * MIB)?;
    t.age_days("Downloads/old-installer.dmg", 200)?;
    // App data under Library.
    t.file(
        "Library/Application Support/FixtureStudio/models/big.gguf",
        7 * MIB,
    )?;
    // A sparse file: 64 MiB long, nothing on disk.
    t.sparse("Documents/sparse.img", 64 * MIB)?;
    // A hard-linked pair of 3 MiB: 3 MiB on disk once, and neither link reaches 4 MB.
    t.file("Pictures/pair/original.bin", 3 * MIB)?;
    t.hard_link("Pictures/pair/original.bin", "Pictures/pair/linked.bin")?;

    // --- Denylisted folders, never scanned ---
    t.file(
        "Library/Containers/com.example.sandboxed/Data/container-huge.db",
        10 * MIB,
    )?;
    t.file("Library/Group Containers/group.example/shared.db", 5 * MIB)?;
    t.text(".ssh/id_fixture", "not a key\n")?;
    t.file(".config/fixture/settings.bin", 2 * MIB)?;
    Ok(())
}

fn expected_for(ledger: &Ledger, now: u64) -> Expected {
    // (rule, path, group, class, ticked, blocked)
    type Spec = (
        &'static str,
        &'static str,
        Group,
        Class,
        bool,
        Option<&'static str>,
    );
    let specs: [Spec; 12] = [
        (
            rule::CACHE_APP,
            "Library/Caches/com.example.idle",
            Group::Caches,
            Class::Safe,
            true,
            None,
        ),
        (
            rule::CACHE_CHROME,
            "Library/Caches/Google/Chrome/Default",
            Group::Caches,
            Class::Safe,
            false,
            Some(BLOCKED_CHROME),
        ),
        (
            rule::CACHE_UNKNOWN,
            "Library/Caches/mystery-cache",
            Group::Caches,
            Class::Review,
            false,
            None,
        ),
        (
            rule::LOGS_APP,
            "Library/Logs/Idle/old.log",
            Group::Logs,
            Class::Safe,
            true,
            None,
        ),
        (
            rule::LOGS_CRASH,
            "Library/Logs/DiagnosticReports/crash-old.ips",
            Group::Logs,
            Class::Safe,
            true,
            None,
        ),
        (
            rule::DERIVED_DATA,
            "Library/Developer/Xcode/DerivedData/OldProj-abc",
            Group::Developer,
            Class::Safe,
            true,
            None,
        ),
        (
            rule::BUILD_TARGET,
            "Documents/projects/idle-rs/target",
            Group::Developer,
            Class::Safe,
            true,
            None,
        ),
        (
            rule::NODE_MODULES,
            "Documents/projects/web-old/node_modules",
            Group::Developer,
            Class::Safe,
            true,
            None,
        ),
        (
            rule::BUILD_OUTPUT,
            "Documents/projects/site-b/dist",
            Group::Developer,
            Class::Review,
            false,
            None,
        ),
        (
            rule::NPM_CACHE,
            ".npm/_cacache",
            Group::Developer,
            Class::Safe,
            true,
            None,
        ),
        (
            rule::HUGGINGFACE,
            ".cache/huggingface/hub/models--acme--tiny-model",
            Group::AiModels,
            Class::Review,
            false,
            None,
        ),
        (
            rule::VM_DISK,
            ".colima",
            Group::AiModels,
            Class::ReportOnly,
            false,
            None,
        ),
    ];
    let mut items: Vec<ExpectedItem> = specs
        .iter()
        .map(|(rule, rel, group, class, ticked, blocked)| ExpectedItem {
            id: format!("{rule}:{rel}"),
            group: *group,
            class: *class,
            ticked: *ticked,
            blocked: blocked.map(str::to_string),
            bytes_alloc: ledger.alloc_under(rel),
        })
        .collect();
    items.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then(b.bytes_alloc.cmp(&a.bytes_alloc))
            .then(a.id.cmp(&b.id))
    });

    let plan = ExpectedPlan {
        now,
        running: RUNNING.iter().map(|s| s.to_string()).collect(),
        items,
        never: [
            // Excluded.
            "Library/Caches/com.apple.nsurlsessiond",
            "Library/Caches/Tidy",
            "Library/Logs/Tidy",
            // Matches the rules decline to offer: too new, active, or not named in .gitignore.
            "Library/Logs/Idle/new.log",
            "Library/Logs/DiagnosticReports/crash-new.ips",
            "Library/Developer/Xcode/DerivedData/NewProj-def",
            "Documents/projects/active-rs",
            "Documents/projects/site-a",
            // Denylisted and symlinked.
            "Library/Containers",
            "Library/Group Containers",
            ".ssh",
            ".config",
            "Documents/linkdir",
            "huge-outside.bin",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    };

    let big_app = "Applications/Big.app";
    let large_specs: [(&str, &str, LargeKind, bool); 4] = [
        ("Movies/trip.mov", "trip.mov", LargeKind::File, false),
        (
            "Downloads/old-installer.dmg",
            "old-installer.dmg",
            LargeKind::File,
            false,
        ),
        (
            "Library/Application Support/FixtureStudio/models/big.gguf",
            "big.gguf",
            LargeKind::File,
            true,
        ),
        (big_app, "Big.app", LargeKind::Package, false),
    ];
    let mut large_items: Vec<ExpectedLargeItem> = large_specs
        .iter()
        .map(|(rel, name, kind, app_data)| ExpectedLargeItem {
            rel: rel.to_string(),
            name: name.to_string(),
            kind: *kind,
            bytes_alloc: ledger.alloc_under(rel),
            bytes_logical: ledger.logical_under(rel),
            app_data: *app_data,
        })
        .collect();
    large_items.sort_by(|a, b| b.bytes_alloc.cmp(&a.bytes_alloc).then(a.rel.cmp(&b.rel)));

    let folder_rels = [
        "",
        ".cache",
        ".colima",
        ".npm",
        "Applications",
        "Applications/Big.app",
        "Applications/Idle.app",
        "Documents",
        "Documents/projects",
        "Downloads",
        "Library",
        "Library/Application Support",
        "Library/Caches",
        "Library/Logs",
        "Movies",
        "Pictures",
        "Pictures/pair",
    ];
    let folders = folder_rels
        .iter()
        .map(|rel| ExpectedFolder {
            rel: rel.to_string(),
            bytes_alloc: ledger.alloc_under_excluding(rel, &NOT_SCANNED),
            bytes_logical: ledger.logical_under_excluding(rel, &NOT_SCANNED),
        })
        .collect();

    let large = ExpectedLarge {
        now,
        min_bytes: LARGE_MIN_BYTES,
        not_opened_days: NOT_OPENED_DAYS,
        items: large_items,
        not_opened: vec!["Downloads/old-installer.dmg".to_string()],
        folders,
        absent_items: vec![
            "Documents/sparse.img".to_string(),
            "Pictures/pair/original.bin".to_string(),
            "Pictures/pair/linked.bin".to_string(),
        ],
        never: [
            "Library/Containers",
            "Library/Group Containers",
            ".ssh",
            ".config",
            "Documents/linkdir/",
            "huge-outside.bin",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
    };

    Expected { plan, large }
}
