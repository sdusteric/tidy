//! Demo mode: a fixed, deterministic UI state for screenshots. `TIDY_DEMO=<scenario>` starts the
//! app without a core, with fixed data, a fixed clock and a forced hover, so `scripts/shots.sh`
//! can capture the same pixels every time without showing a window.
//!
//! # Contract for renderers
//!
//! A demo window is created hidden (`show: false`) and paints exactly one frame, at creation. It
//! never redraws, never receives a hover event and never runs an async task to completion.
//! Everything below must therefore be decided synchronously, from [`DemoState`], on that first
//! frame. When [`state`] returns `Some`, every renderer honors these rules:
//!
//! - Clock: use [`crate::app::now_unix`], which returns `DemoState::now_unix`. Relative times
//!   ("Scanned 2 min ago", "Modified 8 months ago") depend on it.
//! - Hover: the view starts with `DemoState::hovered` set, so that row is drawn hovered (its
//!   actions shown). Nothing can really hover a hidden window.
//! - Motion: `cx.set_reduce_motion(true)` is set at startup, so one-shot motions render their end
//!   state. Animations must not depend on a second frame.
//! - No core, no lock, no disk access: `send` returns false, nothing is persisted, nothing is
//!   scanned, no volume is read, no App Nap call is made. The model is filled directly by
//!   [`TidyModel::load_demo`].
//!
//! # Rows on the first frame
//!
//! The rows are drawn through the same `uniform_list` as in the real app. If a capture shows rows
//! missing on that first frame, switch the list to a plain column when `state(cx)` is `Some` and
//! record the choice here. (Not needed so far: the first frame lays out all rows.)

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;
use tidy_core::model::{Action, HistoryEntry, HistoryItem, ItemResult, Source, TrasherKind};
use tidy_core::model::{
    AppMemory, CheckId, CheckResult, Checkup, CheckupCleanup, CheckupMemory, CheckupSecurity,
    ChromeExtension, Class, FolderNode, Group, Item, LargeItem, LargeKind, LargeScan, LaunchKind,
    LoginItem, MB, Mark, MemoryKind, MemorySnapshot, PageStats, Plan, Pressure, Scope,
    SecurityReport, Signer, Status, SwapUsage, TrashSummary, UseSource,
};
use tidy_core::reminder::ReminderDate;
use tidy_core::settings::Settings;

use crate::app::{LargeFilter, LargeMode, Notice, Screen, install_demo, model};
use crate::macos::{DiskInfo, Permission};

/// The fixed "now" in Unix seconds that demo data is relative to.
pub const NOW: u64 = 1_792_764_131;

/// The fake home folder that demo paths are shown under (`~`).
const ROOT: &str = "/Users/demo";

/// Default seconds before a demo process quits itself (`TIDY_DEMO_EXIT_SECS` overrides it).
pub const DEFAULT_EXIT_SECS: u64 = 30;

/// gpui makes a window with a transparent title bar 1 pt taller than asked (Haul measured 669 pt
/// asked gave a 670 pt capture), so on-screen demo sizes ask for 1 pt less. An offscreen export
/// (`render_to_image`) renders exactly the asked frame, so it asks for the full 620 pt and the
/// PNG is 1800 x 1240 px at 2x.
const TRANSPARENT_TITLEBAR_EXTRA: f32 = 1.;

const DAY: u64 = 86_400;
const GB: u64 = 1_000 * MB;

/// Which screen a demo process shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// Cleanup after a scan: five groups, a blocked row, a Review row, Report only rows.
    Cleanup,
    /// Cleanup after a clean: the notice band, the safe rows gone, the Trash total.
    CleanupDone,
    /// Cleanup before the first scan.
    CleanupEmpty,
    /// Large files, Files mode, Biggest.
    Large,
    /// Large files, Files mode, Not opened lately.
    LargeNotOpened,
    /// Large files, Folders mode, one level down.
    LargeFolders,
    /// Large files before the first scan.
    LargeEmpty,
    /// Check up after a run: three cards filled in and the reminder on.
    Checkup,
    /// Memory with normal pressure: figures, a graph and the top apps.
    Memory,
    /// Memory with tight pressure and a row that is still open after Quit.
    MemoryTight,
    /// Memory with tight pressure while one app relaunches and another was just opened again.
    MemoryRelaunch,
    /// Memory with very tight pressure and an app that did not quit in time for Relaunch.
    MemoryCritical,
    /// Security after a second check: marks, amber buttons and a gray row.
    Security,
    /// Security on the first run: the first-check line and no marks.
    SecurityFirst,
    /// Security scrolled to the login items.
    SecurityItems,
    /// Cleanup with the details popover of a Safe row open (design 17.1).
    CleanupWhy,
    /// Check up with the System Data sheet open (design 17.2).
    SystemData,
    /// Check up scrolled to the System Data row and "What Tidy never does" (design 17.3).
    CheckupTrust,
    /// The System Data sheet scrolled to its report-only rows (Docker, snapshots).
    SystemDataReport,
    /// Cleanup with a Review row ticked and the Move to Trash receipt open.
    CleanupConfirm,
    /// Cleanup with the History sheet open: four entries, the newest one opened. Its items are
    /// in the Trash and can be put back from Finder, and one skipped item is still in place
    /// with Reveal.
    History,
    /// Cleanup with the keyboard on a row checkbox: the focus ring, and the row's actions.
    CleanupFocus,
}

impl Scenario {
    pub const ALL: [Scenario; 22] = [
        Scenario::Cleanup,
        Scenario::CleanupDone,
        Scenario::CleanupEmpty,
        Scenario::Large,
        Scenario::LargeNotOpened,
        Scenario::LargeFolders,
        Scenario::LargeEmpty,
        Scenario::Checkup,
        Scenario::Memory,
        Scenario::MemoryTight,
        Scenario::MemoryRelaunch,
        Scenario::MemoryCritical,
        Scenario::Security,
        Scenario::SecurityFirst,
        Scenario::SecurityItems,
        Scenario::CleanupWhy,
        Scenario::SystemData,
        Scenario::CheckupTrust,
        Scenario::SystemDataReport,
        Scenario::CleanupConfirm,
        Scenario::History,
        Scenario::CleanupFocus,
    ];

    /// Parses a `TIDY_DEMO` value (case-insensitive). None for an unknown name.
    pub fn parse(s: &str) -> Option<Scenario> {
        let s = s.trim().to_ascii_lowercase();
        Scenario::ALL.into_iter().find(|sc| sc.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Scenario::Cleanup => "cleanup",
            Scenario::CleanupDone => "cleanup-done",
            Scenario::CleanupEmpty => "cleanup-empty",
            Scenario::Large => "large",
            Scenario::LargeNotOpened => "large-not-opened",
            Scenario::LargeFolders => "large-folders",
            Scenario::LargeEmpty => "large-empty",
            Scenario::Checkup => "checkup",
            Scenario::Memory => "memory",
            Scenario::MemoryTight => "memory-tight",
            Scenario::MemoryRelaunch => "memory-relaunch",
            Scenario::MemoryCritical => "memory-critical",
            Scenario::Security => "security",
            Scenario::SecurityFirst => "security-first",
            Scenario::SecurityItems => "security-items",
            Scenario::CleanupWhy => "cleanup-why",
            Scenario::SystemData => "system-data",
            Scenario::CheckupTrust => "checkup-trust",
            Scenario::SystemDataReport => "system-data-report",
            Scenario::CleanupConfirm => "cleanup-confirm",
            Scenario::History => "history",
            Scenario::CleanupFocus => "cleanup-focus",
        }
    }

    fn screen(self) -> Screen {
        match self {
            Scenario::Cleanup
            | Scenario::CleanupDone
            | Scenario::CleanupEmpty
            | Scenario::CleanupWhy
            | Scenario::CleanupConfirm
            | Scenario::History
            | Scenario::CleanupFocus => Screen::Cleanup,
            Scenario::Checkup
            | Scenario::SystemData
            | Scenario::SystemDataReport
            | Scenario::CheckupTrust => Screen::Checkup,
            Scenario::Memory
            | Scenario::MemoryTight
            | Scenario::MemoryRelaunch
            | Scenario::MemoryCritical => Screen::Memory,
            Scenario::Security | Scenario::SecurityFirst | Scenario::SecurityItems => {
                Screen::Security
            }
            _ => Screen::Large,
        }
    }
}

/// True when a `TIDY_DEMO` value turns demo mode on. Empty or blank values and the usual "off"
/// words (`0`, `false`, `off`, `no`) leave it off. This is the single test used by both
/// [`active`] and [`crate::app::background_from`], so the two can never disagree.
pub fn is_on(raw: &str) -> bool {
    let v = raw.trim().to_ascii_lowercase();
    !matches!(v.as_str(), "" | "0" | "false" | "off" | "no")
}

/// The scenario for a `TIDY_DEMO` value, or None when the value turns demo mode off. An unknown
/// name runs `Cleanup` rather than the real app, so a typo can never start a core.
pub fn scenario_from(raw: Option<&str>) -> Option<Scenario> {
    let raw = raw?;
    if !is_on(raw) {
        return None;
    }
    Some(Scenario::parse(raw).unwrap_or_else(|| {
        eprintln!("tidy: unknown TIDY_DEMO value {raw:?}, using cleanup");
        Scenario::Cleanup
    }))
}

/// The scenario from `TIDY_DEMO`, or None when demo mode is off.
pub fn active() -> Option<Scenario> {
    static CACHED: std::sync::OnceLock<Option<Scenario>> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| scenario_from(std::env::var("TIDY_DEMO").ok().as_deref()))
}

/// The size to ask for when opening a scenario's main window: 900 x 620 pt, less the title bar
/// correction on screen.
pub fn main_size(_s: Scenario) -> Option<(f32, f32)> {
    main_size_for(exports_shot())
}

/// The test behind [`main_size`]. `export` is true for an offscreen export, whose frame is
/// exactly the asked size, so it gets no title bar correction.
pub fn main_size_for(export: bool) -> Option<(f32, f32)> {
    let extra = if export {
        0.
    } else {
        TRANSPARENT_TITLEBAR_EXTRA
    };
    Some((
        crate::theme::metrics::MAIN_W,
        crate::theme::metrics::MAIN_H - extra,
    ))
}

/// Where an exporting demo opens its windows: far outside every display. Such a window has no
/// screen, and gpui then renders it at scale 2 (gpui-pre-macos `get_scale_factor` returns 2.0
/// when `[NSWindow screen]` is nil), so the shot is 1800 x 1240 px on any display. The window is
/// never shown either way.
const OFFSCREEN_ORIGIN: f32 = -30_000.;

/// The scale an exported shot must render at.
pub const SHOT_SCALE: f32 = 2.;

/// True when this process renders its own shot: a `shot-export` build in demo mode with
/// `TIDY_DEMO_SHOT` set to a file.
pub fn exports_shot() -> bool {
    shot_export_from(
        cfg!(feature = "shot-export"),
        active().is_some(),
        std::env::var_os("TIDY_DEMO_SHOT").as_deref(),
    )
}

/// The test behind [`exports_shot`].
pub fn shot_export_from(feature: bool, demo: bool, shot: Option<&std::ffi::OsStr>) -> bool {
    feature && demo && shot.is_some_and(|p| !p.is_empty())
}

/// The bounds a window of `size` opens with: centered on the main display, or, for an offscreen
/// export, outside every display so it renders at [`SHOT_SCALE`].
pub fn window_bounds(size: Size<Pixels>, cx: &App) -> Bounds<Pixels> {
    if exports_shot() {
        Bounds {
            origin: point(px(OFFSCREEN_ORIGIN), px(OFFSCREEN_ORIGIN)),
            size,
        }
    } else {
        Bounds::centered(None, size, cx)
    }
}

/// Fixed state read by the renderers. See the module docs for the contract.
pub struct DemoState {
    pub scenario: Scenario,
    /// Fixed Unix time in seconds (`NOW`).
    pub now_unix: u64,
    /// The row key drawn hovered (nothing can really hover a hidden window).
    pub hovered: Option<String>,
    /// How far the Security list starts scrolled, in points (a hidden window can't scroll).
    pub scroll_y: f32,
    /// How far the Check up screen starts scrolled, in points.
    pub checkup_scroll_y: f32,
    /// How far the System Data sheet starts scrolled, in points.
    pub sysdata_scroll_y: f32,
    /// The item whose details popover is open, and where it was asked for (x, y in points).
    pub details: Option<(String, f32, f32)>,
    /// The Move to Trash receipt starts open.
    pub confirm: bool,
    /// The History sheet starts open, with its newest entry expanded.
    pub history: bool,
    /// The row (its `RowFocus` key) whose first control gets the keyboard focus, drawn with
    /// the focus ring. A hidden window has no keyboard, so the export moves the focus itself.
    pub focus_row: Option<String>,
}

impl Global for DemoState {}

impl DemoState {
    pub fn new(scenario: Scenario) -> DemoState {
        DemoState {
            scenario,
            now_unix: NOW,
            hovered: match scenario {
                Scenario::Cleanup | Scenario::CleanupWhy => {
                    Some("cache.app:Library/Caches/com.spotify.client".to_string())
                }
                Scenario::Large | Scenario::LargeNotOpened => {
                    Some("Movies/2025/trip-raw.mov".to_string())
                }
                Scenario::SecurityItems => Some("user:com.dropbox.sync-agent".to_string()),
                _ => None,
            },
            scroll_y: match scenario {
                Scenario::SecurityItems => 372.,
                _ => 0.,
            },
            checkup_scroll_y: match scenario {
                Scenario::CheckupTrust => 330.,
                _ => 0.,
            },
            sysdata_scroll_y: match scenario {
                Scenario::SystemDataReport => 470.,
                _ => 0.,
            },
            // Where the Spotify row's Details button sits: the popover's top right corner
            // hangs under it.
            confirm: scenario == Scenario::CleanupConfirm,
            history: scenario == Scenario::History,
            focus_row: (scenario == Scenario::CleanupFocus)
                .then(|| "clean:cache.app:Library/Caches/com.spotify.client".to_string()),
            details: (scenario == Scenario::CleanupWhy).then(|| {
                (
                    "cache.app:Library/Caches/com.spotify.client".to_string(),
                    830.,
                    185.,
                )
            }),
        }
    }
}

/// The demo state, or None when demo mode is off.
pub fn state(cx: &App) -> Option<&DemoState> {
    cx.try_global::<DemoState>()
}

// ---------- data ----------

/// Settings for a demo: the screens have been used before.
pub fn settings() -> Settings {
    Settings {
        cleanup_scanned_once: true,
        large_scanned_once: true,
        ..Settings::default()
    }
}

/// The volume the sidebar shows.
pub fn disk() -> DiskInfo {
    DiskInfo {
        name: "Macintosh HD".to_string(),
        total: 1_000 * GB,
        available: 212 * GB,
    }
}

#[allow(clippy::too_many_arguments)]
fn clean_item(
    id: &str,
    group: Group,
    class: Class,
    name: &str,
    reason: &str,
    blocked: Option<&str>,
    bytes: u64,
) -> Item {
    Item {
        id: id.to_string(),
        group,
        class,
        ticked: class == Class::Safe && blocked.is_none(),
        blocked: blocked.map(str::to_string),
        name: name.to_string(),
        reason: reason.to_string(),
        path: PathBuf::from(ROOT).join(id.split_once(':').map_or(id, |(_, p)| p)),
        bytes_alloc: bytes,
        bytes_logical: bytes,
        newest_mtime: None,
        members: Vec::new(),
    }
}

/// The ids of the Safe rows, which the cleanup-done scenario removes.
fn safe_ids(plan: &Plan) -> Vec<String> {
    plan.items
        .iter()
        .filter(|i| i.ticked)
        .map(|i| i.id.clone())
        .collect()
}

/// The Cleanup plan of the `cleanup` scenario. Safe items add up to 6.3 GB.
pub fn cleanup_plan() -> Plan {
    use Class::*;
    use Group::*;
    let mut plan = Plan::empty(ROOT, NOW - 120);
    plan.files_seen = 412_800;
    plan.trash = TrashSummary {
        items: 214,
        bytes: 2_100 * MB,
    };
    plan.items = vec![
        clean_item(
            "cache.app:Library/Caches/com.spotify.client",
            Caches,
            Safe,
            "Spotify",
            "Cache, rebuilt by the app",
            None,
            1_400 * MB,
        ),
        clean_item(
            "cache.chrome:Library/Caches/Google/Chrome/Default",
            Caches,
            Safe,
            "Google Chrome",
            "Cache, rebuilt by the app",
            Some("Quit Google Chrome first"),
            1_200 * MB,
        ),
        clean_item(
            "cache.app:Library/Caches/com.tinyspeck.slackmacgap",
            Caches,
            Safe,
            "Slack",
            "Cache, rebuilt by the app",
            None,
            842 * MB,
        ),
        clean_item(
            "cache.unknown:Library/Caches/com.example.updater",
            Caches,
            Review,
            "com.example.updater",
            "Cache of an app Tidy doesn't know",
            None,
            210 * MB,
        ),
        clean_item(
            "logs.app.small:Library/Logs",
            Logs,
            Safe,
            "App logs",
            "Logs older than 14 days",
            None,
            164 * MB,
        ),
        clean_item(
            "logs.diag.small:Library/Logs/DiagnosticReports",
            Logs,
            Safe,
            "Crash reports",
            "Crash reports older than 30 days",
            None,
            38 * MB,
        ),
        clean_item(
            "dev.deriveddata.small:Library/Developer/Xcode/DerivedData",
            Developer,
            Safe,
            "Xcode DerivedData",
            "Build output, not used for 7 days",
            None,
            2_000 * MB,
        ),
        clean_item(
            "proj.target:Documents/projects/tidy/target",
            Developer,
            Safe,
            "tidy (Rust target)",
            "Build output, project not touched for 41 days",
            None,
            1_100 * MB,
        ),
        clean_item(
            "dev.npm-cacache:.npm/_cacache",
            Developer,
            Safe,
            "npm cache",
            "Package cache, downloaded again when needed",
            None,
            800 * MB,
        ),
        clean_item(
            "proj.dist:Documents/projects/web-dashboard/dist",
            Developer,
            Review,
            "web-dashboard (dist)",
            "Build output listed in .gitignore",
            None,
            96 * MB,
        ),
        clean_item(
            "ai.huggingface.small:.cache/huggingface/hub",
            AiModels,
            Review,
            "Hugging Face models",
            "Downloaded models, downloaded again if you use them",
            None,
            18_400 * MB,
        ),
        clean_item(
            "ai.ltx.small:Library/Application Support/LTXDesktop/models",
            AiModels,
            ReportOnly,
            "LTX Studio models",
            "Large app data",
            None,
            66_900 * MB,
        ),
        clean_item(
            "ai.vm:.colima",
            AiModels,
            ReportOnly,
            "Virtual machine disks",
            "Disk images of virtual machines",
            None,
            24_100 * MB,
        ),
        clean_item(
            "downloads.installers:Downloads/Docker.dmg",
            Installers,
            Review,
            "Docker.dmg",
            "In Downloads, not changed for 74 days",
            None,
            640 * MB,
        ),
        clean_item(
            "downloads.installers:Downloads/Zoom.pkg",
            Installers,
            Review,
            "Zoom.pkg",
            "In Downloads, not changed for 41 days",
            None,
            96 * MB,
        ),
    ];
    plan.sort();
    plan
}

#[allow(clippy::too_many_arguments)]
fn large_item(
    rel: &str,
    mb: u64,
    kind: LargeKind,
    days_ago: u64,
    source: UseSource,
    app_data: bool,
) -> LargeItem {
    LargeItem {
        path: PathBuf::from(ROOT).join(rel),
        rel: rel.to_string(),
        name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
        kind,
        bytes_alloc: mb * MB,
        bytes_logical: mb * MB,
        last_used: Some(NOW - days_ago * DAY),
        used_source: source,
        app_data,
    }
}

fn node(
    name: &str,
    rel: &str,
    mb: u64,
    kind: LargeKind,
    files: u64,
    children: Vec<FolderNode>,
) -> FolderNode {
    FolderNode {
        name: name.to_string(),
        rel: rel.to_string(),
        kind: Some(kind),
        bytes_alloc: mb * MB,
        bytes_logical: mb * MB,
        files,
        children,
        rest_count: 0,
        rest_alloc: 0,
    }
}

/// The Large files result of the large scenarios.
pub fn large_scan() -> LargeScan {
    use LargeKind::*;
    use UseSource::*;
    let mut scan = LargeScan::empty(ROOT, NOW - 2 * 3_600, 50 * MB);
    scan.files_seen = 1_204_000;
    scan.items = vec![
        large_item(
            "Library/Application Support/LTX Studio/models/ltx-video-13b.safetensors",
            38_200,
            File,
            240,
            FileTimes,
            true,
        ),
        large_item(
            "Movies/2025/trip-raw.mov",
            12_400,
            File,
            61,
            Spotlight,
            false,
        ),
        large_item(
            "Downloads/ubuntu-24.04-desktop-amd64.iso",
            6_100,
            File,
            370,
            FileTimes,
            false,
        ),
        large_item(
            "Applications/Final Cut Pro.app",
            4_900,
            Package,
            3,
            Spotlight,
            false,
        ),
        large_item(
            "Documents/Archive/backup-2023.zip",
            3_200,
            File,
            730,
            FileTimes,
            false,
        ),
        large_item(
            "Movies/Screen Recordings/demo-2025-04.mov",
            1_800,
            File,
            185,
            FileTimes,
            false,
        ),
        large_item("Downloads/Docker.dmg", 980, File, 270, FileTimes, false),
        large_item(
            "Library/Caches/pip/wheels.tar",
            760,
            File,
            120,
            FileTimes,
            true,
        ),
        large_item(
            "Documents/Taxes/scans-2024.pdf",
            540,
            File,
            21,
            Spotlight,
            false,
        ),
        // Below the 500 MB default, shown when the threshold is lowered.
        large_item("Pictures/export-batch.zip", 310, File, 14, FileTimes, false),
    ];
    let support = node(
        "Application Support",
        "Library/Application Support",
        71_600,
        Folder,
        88_410,
        vec![
            node(
                "LTX Studio",
                "Library/Application Support/LTX Studio",
                66_900,
                Folder,
                1_204,
                vec![],
            ),
            node(
                "Google",
                "Library/Application Support/Google",
                2_100,
                Folder,
                31_800,
                vec![],
            ),
            node(
                "Code",
                "Library/Application Support/Code",
                1_020,
                Folder,
                20_140,
                vec![],
            ),
        ],
    );
    let mut library = node(
        "Library",
        "Library",
        96_400,
        Folder,
        412_000,
        vec![
            support,
            node("Caches", "Library/Caches", 14_200, Folder, 190_300, vec![]),
            node(
                "Developer",
                "Library/Developer",
                9_600,
                Folder,
                61_900,
                vec![],
            ),
            node("Logs", "Library/Logs", 420, Folder, 3_100, vec![]),
        ],
    );
    library.rest_count = 38;
    library.rest_alloc = 1_100 * MB;
    let mut tree = node(
        "Home",
        "",
        190_000,
        Folder,
        1_204_000,
        vec![
            library,
            node("Movies", "Movies", 41_000, Folder, 640, vec![]),
            node("Documents", "Documents", 21_000, Folder, 52_300, vec![]),
            node("Downloads", "Downloads", 12_000, Folder, 410, vec![]),
            node(
                "Applications",
                "Applications",
                11_800,
                Folder,
                94_000,
                vec![],
            ),
        ],
    );
    tree.rest_count = 1_204;
    tree.rest_alloc = 3_100 * MB;
    scan.tree = tree;
    scan
}

// ---------- Memory, Security and Check up data ----------

fn mem_row(name: &str, mb: u64, helpers: u32, quittable: bool) -> AppMemory {
    let pid = 1_000 + name.len() as i32 * 7;
    AppMemory {
        id: format!("app:/Applications/{name}.app"),
        name: name.to_string(),
        kind: MemoryKind::App,
        pid,
        pids: std::iter::once(pid)
            .chain((0..helpers as i32).map(|i| pid + 100 + i))
            .collect(),
        bundle_id: None,
        app_path: Some(PathBuf::from(format!("/Applications/{name}.app"))),
        footprint: mb * MB,
        helpers,
        quittable,
        members: Vec::new(),
    }
}

/// The id of a demo Memory row, for the rows that show a Quit state.
pub fn mem_id(name: &str) -> String {
    format!("app:/Applications/{name}.app")
}

/// The Memory snapshot of the memory scenarios: 16 GB, a little swap, and the apps a developer
/// would have open.
pub fn memory_snapshot(pressure: Pressure) -> MemorySnapshot {
    let tight = pressure != Pressure::Normal;
    let (swap_total, swap_used, compressed) = if tight {
        (8_192 * MB, 4_800 * MB, 5_900 * MB)
    } else {
        (2_048 * MB, 320 * MB, 3_400 * MB)
    };
    let tighter = pressure == Pressure::Critical;
    let mut chrome = mem_row(
        "Google Chrome",
        if tighter {
            7_400
        } else if tight {
            5_600
        } else {
            4_200
        },
        14,
        true,
    );
    // Above 4 GB, so its row shows Chrome's own Memory Saver setting.
    chrome.bundle_id = Some("com.google.Chrome".to_string());
    let mut apps = vec![
        chrome,
        mem_row("Xcode", if tight { 4_300 } else { 3_100 }, 6, true),
        mem_row("Slack", 1_400, 5, true),
        mem_row("Spotify", 920, 4, true),
        mem_row("Safari", 780, 3, true),
        AppMemory {
            id: "system".to_string(),
            name: "System processes".to_string(),
            kind: MemoryKind::System,
            pid: 0,
            pids: vec![],
            bundle_id: None,
            app_path: None,
            // As on a real Mac: the size `top` gave for the processes Tidy cannot read
            // itself, and the row names the large ones inside.
            footprint: if tight { 2_600 * MB } else { 2_100 * MB },
            helpers: 0,
            quittable: false,
            members: vec![
                "WindowServer".to_string(),
                "kernel_task".to_string(),
                "launchd".to_string(),
                "mds_stores".to_string(),
            ],
        },
        mem_row("Messages", 310, 1, true),
        mem_row("Finder", 250, 0, false),
        mem_row("Notes", 190, 1, true),
    ];
    apps.sort_by_key(|a| std::cmp::Reverse(a.footprint));
    MemorySnapshot {
        at: NOW,
        pressure,
        pressure_level: match pressure {
            Pressure::Normal => 1,
            Pressure::Warning => 2,
            Pressure::Critical => 4,
        },
        total_bytes: 16_000 * MB,
        swap: SwapUsage {
            total: swap_total,
            used: swap_used,
            free: swap_total - swap_used,
        },
        compressed_bytes: compressed,
        pages: PageStats::default(),
        apps,
    }
}

/// Sixty samples (two minutes): normal, a short stretch of warning, normal again. The tight
/// scenario ends in warning.
pub fn pressure_history(tight: bool) -> Vec<Pressure> {
    (0..60)
        .map(|i| match (tight, i) {
            (false, 28..=37) => Pressure::Warning,
            (true, 40..=44) => Pressure::Critical,
            (true, 22..=59) => Pressure::Warning,
            _ => Pressure::Normal,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn login_item(
    label: &str,
    program: &str,
    scope: Scope,
    kind: LaunchKind,
    signer: Signer,
    notarized: Option<bool>,
    runs_at_load: bool,
    mark: Option<Mark>,
) -> LoginItem {
    let dir = match (scope, kind) {
        (Scope::User, _) => "/Users/demo/Library/LaunchAgents",
        (Scope::System, LaunchKind::Agent) => "/Library/LaunchAgents",
        (Scope::System, LaunchKind::Daemon) => "/Library/LaunchDaemons",
    };
    LoginItem {
        label: label.to_string(),
        plist: Some(PathBuf::from(format!("{dir}/{label}.plist"))),
        program: Some(PathBuf::from(program)),
        runs_at_load,
        scope,
        kind,
        signer,
        notarized,
        mark,
    }
}

/// A Security report. `first_run` has no marks and says it is the first check.
pub fn security_report(first_run: bool) -> SecurityReport {
    let marked = |m: Mark| (!first_run).then_some(m);
    let mut r = SecurityReport::empty(NOW - 2 * 60);
    r.first_run = first_run;
    r.compared_to = (!first_run).then(|| "20261021-090000".to_string());
    r.compared_at = (!first_run).then_some(NOW - 2 * 86_400);
    r.updates_checked = false;

    let green = |id, sentence: &str| CheckResult::new(id, Status::Green, sentence);
    let mut firewall = CheckResult::new(
        CheckId::Firewall,
        Status::Amber,
        "Firewall is off. Your Mac accepts incoming connections from apps that ask for them. Most home networks are fine either way.",
    );
    firewall.mark = marked(Mark::Changed);
    let mut login = CheckResult::new(
        CheckId::LoginItems,
        Status::Amber,
        "2 of 4 items need a look: one is new since the last check and one is not signed.",
    );
    login.mark = marked(Mark::New);
    let mut chrome = CheckResult::new(
        CheckId::ChromeExtensions,
        Status::Amber,
        "1 extension was added since the last check. Open chrome://extensions in Chrome to review it.",
    );
    chrome.mark = marked(Mark::New);
    let mut xprotect = green(
        CheckId::XProtect,
        "XProtect was updated 12 days ago (version 5321).",
    );
    xprotect.detail = Some("5321".to_string());
    r.checks = vec![
        green(
            CheckId::FileVault,
            "FileVault is on. Your disk is encrypted.",
        ),
        firewall,
        green(
            CheckId::Gatekeeper,
            "Gatekeeper is on. Apps from outside the App Store need an identified developer.",
        ),
        green(
            CheckId::Sip,
            "System Integrity Protection is on. macOS system files are protected.",
        ),
        xprotect,
        CheckResult::new(
            CheckId::MacosUpdates,
            Status::Gray,
            tidy_core::security::system::UPDATES_NOT_CHECKED,
        ),
        login,
        chrome,
    ];
    r.login_items = vec![
        login_item(
            "com.dropbox.sync-agent",
            "/Applications/Dropbox.app/Contents/MacOS/Dropbox",
            Scope::User,
            LaunchKind::Agent,
            Signer::Team("Dropbox, Inc.".to_string()),
            Some(true),
            true,
            marked(Mark::New),
        ),
        login_item(
            "com.example.backup-helper",
            "/Users/demo/bin/backup-helper",
            Scope::User,
            LaunchKind::Agent,
            Signer::Unsigned,
            Some(false),
            true,
            None,
        ),
        login_item(
            "com.google.keystone.agent",
            "/Users/demo/Library/Google/GoogleSoftwareUpdate/GoogleSoftwareUpdate.bundle/Contents/Resources/GoogleSoftwareUpdateAgent.app/Contents/MacOS/GoogleSoftwareUpdateAgent",
            Scope::User,
            LaunchKind::Agent,
            Signer::Team("Google LLC".to_string()),
            Some(true),
            true,
            None,
        ),
        login_item(
            "at.obdev.littlesnitch.daemon",
            "/Library/Application Support/Objective Development/Little Snitch/Components/at.obdev.littlesnitchd",
            Scope::System,
            LaunchKind::Daemon,
            Signer::Team("Objective Development Software GmbH".to_string()),
            Some(true),
            true,
            None,
        ),
    ];
    r.extensions = vec![
        ChromeExtension {
            id: "a".repeat(32),
            name: "Reader View".to_string(),
            version: Some("2.1.4".to_string()),
            profile: "Default".to_string(),
            enabled: true,
            mark: marked(Mark::New),
        },
        ChromeExtension {
            id: "b".repeat(32),
            name: "Password manager".to_string(),
            version: Some("8.12.0".to_string()),
            profile: "Default".to_string(),
            enabled: true,
            mark: None,
        },
    ];
    r
}

/// The last check up of the Check up scenario: two days ago.
pub fn last_checkup() -> Checkup {
    let report = security_report(false);
    Checkup {
        at: NOW - 2 * DAY - 600,
        include_updates: false,
        cleanup: Some(CheckupCleanup {
            safe_bytes: 6_344 * MB,
            safe_items: 7,
            review_bytes: 18_706 * MB,
        }),
        security: Some(CheckupSecurity::from_report(&report)),
        memory: Some(CheckupMemory {
            pressure: Pressure::Normal,
            swap_used: 320 * MB,
            compressed_bytes: 3_400 * MB,
        }),
    }
}

/// When the reminder fires in the Check up scenario: 10:00 seven days after the last check up.
pub fn reminder_date() -> ReminderDate {
    ReminderDate {
        at: NOW + 5 * DAY,
        year: 2026,
        month: 10,
        day: 28,
        hour: 10,
        minute: 0,
    }
}

/// The System Data report of the `system-data` scenario.
pub fn system_data_report() -> tidy_core::sysdata::SystemDataReport {
    use tidy_core::sysdata::{Snapshots, SysKind, SysRow, SysSize, snapshot_row};
    let row = |kind: SysKind, bytes: u64, note: &str| SysRow {
        kind,
        title: kind.title().to_string(),
        note: note.to_string(),
        size: SysSize::Measured(bytes),
        report_only: false,
        commands: Vec::new(),
    };
    let mut sims = row(
        SysKind::Simulators,
        14_800 * MB,
        "Simulator devices and what you installed on them.",
    );
    sims.commands = vec![tidy_core::sysdata::CopyCommand {
        label: "Remove simulators whose runtime is gone".to_string(),
        command: "xcrun simctl delete unavailable".to_string(),
    }];
    let docker = SysRow {
        kind: SysKind::Docker,
        title: SysKind::Docker.title().to_string(),
        note: "Docker keeps images, containers and build cache in one disk image inside its app container, which Tidy doesn't open. Run these in Terminal to see and reclaim that space.".to_string(),
        size: SysSize::NotMeasured,
        report_only: true,
        commands: vec![
            tidy_core::sysdata::CopyCommand {
                label: "See what Docker uses".to_string(),
                command: "docker system df".to_string(),
            },
            tidy_core::sysdata::CopyCommand {
                label: "Remove stopped containers, unused images and build cache".to_string(),
                command: "docker system prune".to_string(),
            },
        ],
    };
    let snapshots = snapshot_row(&Some(Snapshots {
        names: vec![
            "com.apple.TimeMachine.2026-10-25-081502.local".to_string(),
            "com.apple.TimeMachine.2026-10-25-101112.local".to_string(),
        ],
    }));
    let mut backups = row(SysKind::IosBackups, 0, "");
    backups.size = SysSize::NeedsFullDiskAccess;
    backups.note = "macOS keeps this folder behind Full Disk Access. Finder shows and removes them: select the device, then Manage Backups.".to_string();
    tidy_core::sysdata::SystemDataReport {
        at: NOW - 20,
        rows: vec![
            row(
                SysKind::Caches,
                9_200 * MB,
                "Apps keep these to load faster. Cleanup offers the ones Tidy knows are safe.",
            ),
            row(
                SysKind::Logs,
                640 * MB,
                "App logs and crash reports. Cleanup offers the old ones.",
            ),
            row(
                SysKind::Xcode,
                21_400 * MB,
                "Build data, archives and device symbols. Cleanup offers old build data; archives and device symbols wait for your review.",
            ),
            sims,
            row(
                SysKind::VirtualMachines,
                12_100 * MB,
                "Disks of Colima, Lima, Android emulators and Claude. Remove them from the app that made them.",
            ),
            backups,
            docker,
            snapshots,
        ],
    }
}

// ---------- startup ----------

/// How long the offscreen export waits before reading the window's frame. The only frame is
/// painted when the window is created; this is a margin, as in `scripts/shots.sh`.
#[cfg(feature = "shot-export")]
const SHOT_DELAY: Duration = Duration::from_millis(800);

/// Offscreen screenshot (feature `shot-export`, `TIDY_DEMO_SHOT=<file.png>`): renders the demo
/// window's last painted scene (the first and only frame of a hidden window) into a Metal
/// texture with `Window::render_to_image`, writes it as a PNG, then quits. It needs no Screen
/// Recording permission and works while the screen is locked, where `screencapture -l` cannot
/// read any window. The PNG has no traffic lights (AppKit draws those, not gpui) and square
/// corners.
#[cfg(feature = "shot-export")]
fn export_shot(path: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(SHOT_DELAY).await;
        cx.update(|cx| {
            match render_first_window(cx).and_then(|img| write_png(&img, &path)) {
                Ok((w, h)) => tracing::info!("demo shot written: {} ({w}x{h})", path.display()),
                Err(e) => tracing::error!("demo shot failed: {e:#}"),
            }
            cx.quit();
        });
    })
    .detach();
}

/// Draws the demo window once more and renders that frame. Refuses unless the window renders at
/// [`SHOT_SCALE`], so a shot never comes out at the wrong size (see [`window_bounds`]).
#[cfg(feature = "shot-export")]
fn render_first_window(cx: &mut App) -> anyhow::Result<image::RgbaImage> {
    let handle = cx
        .windows()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no demo window to render"))?;
    handle.update(cx, |_, window, cx| {
        let scale = window.scale_factor();
        if (scale - SHOT_SCALE).abs() > f32::EPSILON {
            anyhow::bail!("the demo window renders at scale {scale}, not {SHOT_SCALE}");
        }
        // The state is fixed, so this frame is the creation frame, drawn for certain.
        window.draw(cx).clear(cx);
        if let Some(key) = state(cx).and_then(|d| d.focus_row.clone()) {
            // Put the keyboard on the row's first control, as one Tab from the row would. Two
            // draws: the first registers the tab stops, the second shows the ring and actions.
            crate::views::RowFocus::handle(&key, cx).focus(window, cx);
            window.focus_next(cx);
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        }
        window.render_to_image()
    })?
}

/// Writes next to `path` first and renames, so a reader never sees a half-written file.
#[cfg(feature = "shot-export")]
fn write_png(img: &image::RgbaImage, path: &std::path::Path) -> anyhow::Result<(u32, u32)> {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    img.save_with_format(&part, image::ImageFormat::Png)?;
    std::fs::rename(&part, path)?;
    Ok(img.dimensions())
}

fn exit_after() -> Duration {
    let secs = std::env::var("TIDY_DEMO_EXIT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_EXIT_SECS);
    Duration::from_secs(secs)
}

/// Keeps the app out of the Dock and the application switcher, so a demo launch is invisible.
fn set_accessory_policy() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

/// Fills the model for a scenario.
fn load(scenario: Scenario, m: &mut crate::app::TidyModel) {
    let (plan, large) = match scenario {
        Scenario::Cleanup
        | Scenario::CleanupDone
        | Scenario::CleanupWhy
        | Scenario::CleanupConfirm
        | Scenario::History
        | Scenario::CleanupFocus
        | Scenario::Checkup
        | Scenario::SystemData
        | Scenario::SystemDataReport
        | Scenario::CheckupTrust => (Some(cleanup_plan()), None),
        Scenario::Large | Scenario::LargeNotOpened | Scenario::LargeFolders => {
            (None, Some(large_scan()))
        }
        _ => (None, None),
    };
    m.load_demo(scenario.screen(), plan, large, Some(disk()));
    match scenario {
        Scenario::CleanupConfirm => {
            // One Review row is ticked, which is what makes Clean ask first.
            m.cleanup
                .ticked
                .insert("cache.unknown:Library/Caches/com.example.updater".to_string());
        }
        Scenario::History => {
            let (entries, present) = history_entries();
            m.history.entries = Some(entries);
            m.history.present = present;
        }
        Scenario::CleanupDone => {
            if let Some(plan) = &m.cleanup.plan {
                for id in safe_ids(plan) {
                    m.cleanup.gone.insert(id);
                }
            }
            m.cleanup.trash = TrashSummary {
                items: 221,
                bytes: 8_444 * MB,
            };
            m.cleanup.notice = Some(Notice::Moved { bytes: 6_344 * MB });
        }
        Scenario::Checkup
        | Scenario::SystemData
        | Scenario::SystemDataReport
        | Scenario::CheckupTrust => {
            if matches!(scenario, Scenario::SystemData | Scenario::SystemDataReport) {
                m.system_data.open = true;
                m.system_data.report = Some(system_data_report());
            }
            m.settings.reminder_enabled = true;
            m.checkup.last = Some(last_checkup());
            m.checkup.loaded = true;
            m.checkup.permission = Some(Permission::Granted);
            m.checkup.next_reminder = Some(reminder_date());
            m.security.report = Some(security_report(false));
            m.memory.snapshot = Some(memory_snapshot(Pressure::Normal));
        }
        Scenario::Memory
        | Scenario::MemoryTight
        | Scenario::MemoryRelaunch
        | Scenario::MemoryCritical => {
            use crate::views::memory::QuitState;
            let pressure = match scenario {
                Scenario::Memory => Pressure::Normal,
                Scenario::MemoryCritical => Pressure::Critical,
                _ => Pressure::Warning,
            };
            m.memory.snapshot = Some(memory_snapshot(pressure));
            m.memory.history = pressure_history(pressure != Pressure::Normal);
            if pressure == Pressure::Critical {
                // The last 16 seconds went very tight.
                for p in m.memory.history.iter_mut().skip(52) {
                    *p = Pressure::Critical;
                }
            }
            // Rows show the states of Quit and Relaunch.
            let states: &[(&str, QuitState)] = match scenario {
                Scenario::Memory => &[("Slack", QuitState::Requested)],
                Scenario::MemoryTight => &[("Spotify", QuitState::StillOpen)],
                Scenario::MemoryRelaunch => &[
                    ("Xcode", QuitState::Relaunching),
                    ("Slack", QuitState::Reopened),
                ],
                _ => &[("Spotify", QuitState::RelaunchStillOpen)],
            };
            for (name, state) in states {
                let pid = m
                    .memory
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.app(&mem_id(name)))
                    .map_or(0, |a| a.pid);
                m.memory.quits.insert(
                    mem_id(name),
                    crate::views::memory::Quit { state: *state, pid },
                );
            }
        }
        Scenario::Security | Scenario::SecurityItems => {
            m.security.report = Some(security_report(false));
            m.security.loaded = true;
        }
        Scenario::SecurityFirst => {
            m.security.report = Some(security_report(true));
            m.security.loaded = true;
        }
        Scenario::LargeNotOpened => m.large.filter = LargeFilter::NotOpened,
        Scenario::LargeFolders => {
            m.large.mode = LargeMode::Folders;
            m.large.folder = "Library".to_string();
        }
        _ => {}
    }
}

/// The id of the History entry the `history` scenario opens.
pub fn history_open_entry() -> String {
    HistoryEntry::make_id(NOW - 2 * 3_600, 1)
}

/// Four History entries, oldest first as the core stores them, and the original paths that are
/// still in place (the skipped Chrome cache, which gets a Reveal button). The newest entry's
/// moved items are in the Trash, where Finder's Put Back restores them.
pub fn history_entries() -> (Vec<HistoryEntry>, std::collections::HashSet<PathBuf>) {
    let item = |rel: &str, bytes: u64, rule: &str, result: ItemResult, reason: Option<&str>| {
        let path = PathBuf::from(ROOT).join(rel);
        HistoryItem {
            trash_path: (result == ItemResult::Trashed).then(|| {
                PathBuf::from(ROOT)
                    .join(".Trash")
                    .join(rel.rsplit('/').next().unwrap_or(rel))
            }),
            path,
            bytes,
            rule: rule.to_string(),
            result,
            reason: reason.map(str::to_string),
            trash_id: None,
        }
    };
    let entry = |at: u64, seq: u32, action: Action, source: Source, items: Vec<HistoryItem>| {
        let trashed_bytes = items
            .iter()
            .filter(|i| matches!(i.result, ItemResult::Trashed | ItemResult::Deleted))
            .map(|i| i.bytes)
            .sum();
        HistoryEntry {
            id: HistoryEntry::make_id(at, seq),
            at,
            action,
            source,
            trasher: TrasherKind::System,
            items,
            trashed_bytes,
            message: None,
        }
    };
    let skipped_chrome = PathBuf::from(ROOT).join("Library/Caches/Google/Chrome");
    let entries = vec![
        entry(
            NOW - 9 * DAY,
            1,
            Action::Clean,
            Source::Cleanup,
            vec![
                item(
                    "Library/Caches/pip",
                    640 * MB,
                    "dev.pip",
                    ItemResult::Deleted,
                    None,
                ),
                item(
                    "Library/Logs/DiagnosticReports",
                    90 * MB,
                    "logs.diagnostic",
                    ItemResult::Deleted,
                    None,
                ),
            ],
        ),
        entry(
            NOW - 5 * DAY,
            1,
            Action::Trash,
            Source::Large,
            vec![item(
                "Movies/2024/old-render.mov",
                4_100 * MB,
                "large",
                ItemResult::Trashed,
                None,
            )],
        ),
        entry(
            NOW - 3 * DAY,
            1,
            Action::EmptyTrash,
            Source::Cleanup,
            vec![
                item(
                    ".Trash/old-render.mov",
                    4_100 * MB,
                    "trash",
                    ItemResult::Deleted,
                    None,
                ),
                item(
                    ".Trash/node-gyp",
                    135 * MB,
                    "trash",
                    ItemResult::Deleted,
                    None,
                ),
            ],
        ),
        entry(
            NOW - 2 * 3_600,
            1,
            Action::Clean,
            Source::Cleanup,
            vec![
                item(
                    "Library/Caches/node-gyp",
                    135 * MB,
                    "dev.node-gyp",
                    ItemResult::Trashed,
                    None,
                ),
                item(
                    "Library/Caches/com.spotify.client",
                    1_400 * MB,
                    "cache.app",
                    ItemResult::Trashed,
                    None,
                ),
                item(
                    "Library/Caches/Google/Chrome",
                    1_200 * MB,
                    "cache.chrome",
                    ItemResult::Skipped,
                    Some("Google Chrome is running"),
                ),
            ],
        ),
    ];
    (entries, [skipped_chrome].into_iter().collect())
}

/// Starts the demo inside `app.run`, after `gpui_kit::init` and `tidy_app::init`: no core, a
/// tokio runtime (so SIGTERM quits cleanly), fixed state, then the window. Never activates the
/// app and quits itself after `TIDY_DEMO_EXIT_SECS` (default 30).
pub fn start(scenario: Scenario, rt: tokio::runtime::Runtime, cx: &mut App) {
    set_accessory_policy();
    // One-shot motions render their end state on the only frame.
    cx.set_reduce_motion(true);
    // The focus scenario is a keyboard scenario: the ring is drawn.
    if scenario == Scenario::CleanupFocus {
        crate::theme::set_ring_visible(true);
    }

    install_demo(rt, settings(), cx);
    cx.set_global(DemoState::new(scenario));
    let m = model(cx);
    m.update(cx, |m, _| {
        m.settings = settings();
        load(scenario, m);
    });
    crate::quit_on_signals(cx);

    if crate::open_main_window(cx).is_some() {
        tracing::info!("demo window opened scenario={}", scenario.name());
    } else {
        tracing::error!("demo window did not open scenario={}", scenario.name());
    }

    #[cfg(feature = "shot-export")]
    if let Some(path) = std::env::var_os("TIDY_DEMO_SHOT").filter(|p| !p.is_empty()) {
        export_shot(PathBuf::from(path), cx);
    }

    let after = exit_after();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(after).await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn scenario_names_round_trip() {
        for s in Scenario::ALL {
            assert_eq!(Scenario::parse(s.name()), Some(s));
        }
        assert_eq!(
            Scenario::parse(" LARGE-folders "),
            Some(Scenario::LargeFolders)
        );
        assert_eq!(Scenario::parse("nope"), None);
        assert_eq!(Scenario::ALL.len(), 22);
    }

    #[test]
    fn the_cleanup_plan_adds_up() {
        let plan = cleanup_plan();
        assert_eq!(plan.safe_bytes(), 6_344 * MB);
        assert_eq!(crate::format::size(plan.safe_bytes()), "6.3 GB");
        // One blocked row, two Review rows in Caches and Developer plus Hugging Face, and two
        // old installers (Review, never ticked).
        assert_eq!(plan.items.iter().filter(|i| i.blocked.is_some()).count(), 1);
        assert_eq!(
            plan.items
                .iter()
                .filter(|i| i.class == Class::Review)
                .count(),
            5
        );
        assert!(
            plan.group(Group::Installers)
                .all(|i| i.class == Class::Review && !i.ticked)
        );
        assert_eq!(
            plan.items
                .iter()
                .filter(|i| i.class == Class::ReportOnly)
                .count(),
            2
        );
        assert_eq!(plan.groups().len(), 5);
        // The blocked Chrome item is Safe by class but not ticked.
        let chrome = plan.items.iter().find(|i| i.blocked.is_some()).unwrap();
        assert!(!chrome.ticked && chrome.class == Class::Safe);
    }

    #[test]
    fn the_demo_scan_has_a_new_and_an_old_item_for_each_filter() {
        let scan = large_scan();
        let at_default = scan.biggest(500 * MB);
        assert_eq!(at_default.len(), 9);
        let old = scan.not_opened(500 * MB, NOW, 90);
        assert!(!old.is_empty() && old.len() < at_default.len());
        assert!(scan.tree.find("Library/Application Support").is_some());
        // A hover target exists in the data.
        assert!(
            scan.items
                .iter()
                .any(|i| i.rel == "Movies/2025/trip-raw.mov")
        );
        assert!(
            cleanup_plan().items.iter().any(
                |i| Some(i.id.as_str()) == DemoState::new(Scenario::Cleanup).hovered.as_deref()
            )
        );
    }

    #[test]
    fn window_size_is_900_by_620_less_the_on_screen_correction() {
        assert_eq!(main_size_for(true), Some((900., 620.)));
        assert_eq!(main_size_for(false), Some((900., 619.)));
        assert_eq!(SHOT_SCALE * 900., 1800.);
        assert_eq!(SHOT_SCALE * 620., 1240.);
    }

    #[test]
    fn only_an_export_opens_offscreen() {
        use std::ffi::OsStr;
        let shot = Some(OsStr::new("/tmp/x.png"));
        assert!(shot_export_from(true, true, shot));
        assert!(!shot_export_from(false, true, shot));
        assert!(!shot_export_from(true, false, shot));
        assert!(!shot_export_from(true, true, None));
        assert!(!shot_export_from(true, true, Some(OsStr::new(""))));
        // The test binary has no TIDY_DEMO_SHOT export.
        assert!(!exports_shot());
    }

    #[test]
    fn the_security_demo_has_marks_a_gray_row_and_a_first_run_variant() {
        let r = security_report(false);
        assert_eq!(r.amber(), 3);
        assert_eq!(r.gray(), 1);
        assert_eq!(r.headline(), "3 things to look at");
        assert_eq!(r.checks.len(), CheckId::ALL.len());
        assert!(r.checks.iter().any(|c| c.mark == Some(Mark::Changed)));
        assert!(r.login_items.iter().any(|i| i.mark == Some(Mark::New)));
        assert!(r.login_items.iter().any(|i| i.signer == Signer::Unsigned));
        // The gray row has no button that asks for access.
        assert!(
            r.checks
                .iter()
                .filter(|c| c.status == Status::Gray)
                .all(|c| c.action.is_none())
        );
        let first = security_report(true);
        assert!(first.first_run && first.compared_to.is_none());
        assert!(first.checks.iter().all(|c| c.mark.is_none()));
        assert!(first.login_items.iter().all(|i| i.mark.is_none()));
        assert!(first.extensions.iter().all(|e| e.mark.is_none()));
        // The hover target of the items scenario exists.
        let hovered = DemoState::new(Scenario::SecurityItems).hovered.unwrap();
        assert!(r.login_items.iter().any(|i| i.key() == hovered));
    }

    #[test]
    fn the_memory_demo_adds_up_and_protects_finder_and_the_system_row() {
        for p in [Pressure::Normal, Pressure::Warning] {
            let s = memory_snapshot(p);
            assert!(s.apps.windows(2).all(|w| w[0].footprint >= w[1].footprint));
            let finder = s.apps.iter().find(|a| a.name == "Finder").unwrap();
            assert!(!finder.quittable);
            let system = s
                .apps
                .iter()
                .find(|a| a.kind == MemoryKind::System)
                .unwrap();
            assert!(!system.quittable);
            assert!(s.swap.used <= s.swap.total);
        }
        assert_eq!(pressure_history(false).len(), 60);
        assert!(pressure_history(false).contains(&Pressure::Warning));
        assert_eq!(pressure_history(true).last(), Some(&Pressure::Warning));
        // The rows that show a Quit state exist in the snapshot.
        assert!(
            memory_snapshot(Pressure::Normal)
                .app(&mem_id("Slack"))
                .is_some()
        );
        assert!(
            memory_snapshot(Pressure::Warning)
                .app(&mem_id("Spotify"))
                .is_some()
        );
        // Chrome is above 4 GB at every pressure, so it shows its Memory Saver tip.
        for p in [Pressure::Normal, Pressure::Warning, Pressure::Critical] {
            let s = memory_snapshot(p);
            let chrome = s.app(&mem_id("Google Chrome")).unwrap();
            assert!(crate::views::memory::browser_tip(chrome).is_some());
            assert!(s.app(&mem_id("Xcode")).is_some());
        }
    }

    #[test]
    fn the_checkup_demo_is_consistent() {
        let c = last_checkup();
        assert_eq!(c.cleanup.unwrap().safe_bytes, cleanup_plan().safe_bytes());
        assert_eq!(
            c.security.as_ref().unwrap().amber,
            security_report(false).amber()
        );
        assert_eq!(reminder_date().hour, 10);
    }

    #[test]
    fn demo_text_has_no_em_dashes_or_scare_words() {
        let mut texts: Vec<String> = Vec::new();
        let r = security_report(false);
        for c in &r.checks {
            texts.push(c.sentence.clone());
        }
        for i in &r.login_items {
            texts.push(i.label.clone());
        }
        for a in memory_snapshot(Pressure::Normal).apps {
            texts.push(a.name);
        }
        for t in texts {
            let lower = t.to_lowercase();
            for bad in [
                "\u{2014}",
                &["thre", "at"].concat(),
                &["infe", "cted"].concat(),
                &["dan", "ger"].concat(),
                &["risk", " score"].concat(),
            ] {
                assert!(!lower.contains(bad), "{t}");
            }
        }
    }

    #[test]
    fn demo_text_has_no_em_dashes() {
        let mut texts: Vec<String> = Vec::new();
        for i in cleanup_plan().items {
            texts.push(i.name);
            texts.push(i.reason);
        }
        for i in large_scan().items {
            texts.push(i.name);
        }
        assert!(texts.iter().all(|t| !t.contains('\u{2014}')));
    }
}
