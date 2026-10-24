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
use tidy_core::model::{
    Class, FolderNode, Group, Item, LargeItem, LargeKind, LargeScan, MB, Plan, TrashSummary,
    UseSource,
};
use tidy_core::settings::Settings;

use crate::app::{LargeFilter, LargeMode, Notice, Screen, install_demo, model};
use crate::macos::DiskInfo;

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
}

impl Scenario {
    pub const ALL: [Scenario; 7] = [
        Scenario::Cleanup,
        Scenario::CleanupDone,
        Scenario::CleanupEmpty,
        Scenario::Large,
        Scenario::LargeNotOpened,
        Scenario::LargeFolders,
        Scenario::LargeEmpty,
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
        }
    }

    fn screen(self) -> Screen {
        match self {
            Scenario::Cleanup | Scenario::CleanupDone | Scenario::CleanupEmpty => Screen::Cleanup,
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
}

impl Global for DemoState {}

impl DemoState {
    pub fn new(scenario: Scenario) -> DemoState {
        DemoState {
            scenario,
            now_unix: NOW,
            hovered: match scenario {
                Scenario::Cleanup => Some("spotify:Library/Caches/com.spotify.client".to_string()),
                Scenario::Large | Scenario::LargeNotOpened => {
                    Some("Movies/2025/trip-raw.mov".to_string())
                }
                _ => None,
            },
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
            "spotify:Library/Caches/com.spotify.client",
            Caches,
            Safe,
            "Spotify",
            "Cache, rebuilt by the app",
            None,
            1_400 * MB,
        ),
        clean_item(
            "chrome:Library/Caches/Google/Chrome",
            Caches,
            Safe,
            "Google Chrome",
            "Cache, rebuilt by the app",
            Some("Quit Google Chrome first"),
            1_200 * MB,
        ),
        clean_item(
            "slack:Library/Caches/com.tinyspeck.slackmacgap",
            Caches,
            Safe,
            "Slack",
            "Cache, rebuilt by the app",
            None,
            842 * MB,
        ),
        clean_item(
            "unknown:Library/Caches/com.example.updater",
            Caches,
            Review,
            "com.example.updater",
            "Cache of an app Tidy doesn't know",
            None,
            210 * MB,
        ),
        clean_item(
            "logs:Library/Logs",
            Logs,
            Safe,
            "App logs",
            "Logs older than 14 days",
            None,
            164 * MB,
        ),
        clean_item(
            "crash:Library/Logs/DiagnosticReports",
            Logs,
            Safe,
            "Crash reports",
            "Crash reports older than 30 days",
            None,
            38 * MB,
        ),
        clean_item(
            "xcode:Library/Developer/Xcode/DerivedData",
            Developer,
            Safe,
            "Xcode DerivedData",
            "Build output, not used for 7 days",
            None,
            2_000 * MB,
        ),
        clean_item(
            "rust:Documents/projects/tidy/target",
            Developer,
            Safe,
            "tidy (Rust target)",
            "Build output, project not touched for 41 days",
            None,
            1_100 * MB,
        ),
        clean_item(
            "npm:.npm/_cacache",
            Developer,
            Safe,
            "npm cache",
            "Package cache, downloaded again when needed",
            None,
            800 * MB,
        ),
        clean_item(
            "dist:Documents/projects/web-dashboard/dist",
            Developer,
            Review,
            "web-dashboard (dist)",
            "Build output listed in .gitignore",
            None,
            96 * MB,
        ),
        clean_item(
            "hf:.cache/huggingface",
            AiModels,
            Review,
            "Hugging Face models",
            "Downloaded models, downloaded again if you use them",
            None,
            18_400 * MB,
        ),
        clean_item(
            "ltx:Library/Application Support/LTX Studio",
            AiModels,
            ReportOnly,
            "LTX Studio models",
            "Large app data",
            None,
            66_900 * MB,
        ),
        clean_item(
            "vm:Library/Containers/com.utmapp.UTM",
            AiModels,
            ReportOnly,
            "Virtual machine disks",
            "Disk images of virtual machines",
            None,
            24_100 * MB,
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
        Scenario::Cleanup | Scenario::CleanupDone => (Some(cleanup_plan()), None),
        Scenario::CleanupEmpty => (None, None),
        Scenario::Large | Scenario::LargeNotOpened | Scenario::LargeFolders => {
            (None, Some(large_scan()))
        }
        Scenario::LargeEmpty => (None, None),
    };
    m.load_demo(scenario.screen(), plan, large, Some(disk()));
    match scenario {
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
        Scenario::LargeNotOpened => m.large.filter = LargeFilter::NotOpened,
        Scenario::LargeFolders => {
            m.large.mode = LargeMode::Folders;
            m.large.folder = "Library".to_string();
        }
        _ => {}
    }
}

/// Starts the demo inside `app.run`, after `gpui_kit::init` and `tidy_app::init`: no core, a
/// tokio runtime (so SIGTERM quits cleanly), fixed state, then the window. Never activates the
/// app and quits itself after `TIDY_DEMO_EXIT_SECS` (default 30).
pub fn start(scenario: Scenario, rt: tokio::runtime::Runtime, cx: &mut App) {
    set_accessory_policy();
    // One-shot motions render their end state on the only frame.
    cx.set_reduce_motion(true);

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
        assert_eq!(Scenario::ALL.len(), 7);
    }

    #[test]
    fn the_cleanup_plan_adds_up() {
        let plan = cleanup_plan();
        assert_eq!(plan.safe_bytes(), 6_344 * MB);
        assert_eq!(crate::format::size(plan.safe_bytes()), "6.3 GB");
        // One blocked row, two Review rows in Caches and Developer plus Hugging Face.
        assert_eq!(plan.items.iter().filter(|i| i.blocked.is_some()).count(), 1);
        assert_eq!(
            plan.items
                .iter()
                .filter(|i| i.class == Class::Review)
                .count(),
            3
        );
        assert_eq!(
            plan.items
                .iter()
                .filter(|i| i.class == Class::ReportOnly)
                .count(),
            2
        );
        assert_eq!(plan.groups().len(), 4);
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
