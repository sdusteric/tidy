//! App-wide state: globals, the Tidy model (everything the screens show), the event pump, the
//! heartbeat, actions, key bindings, menus and quit.
//!
//! The app never trashes anything itself. Every change to the disk goes out as a
//! [`Command`]; the model only reflects the [`Event`]s that come back.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::*;
use tidy_core::api::{Command, CoreHandle, Event, ScanKind};
use tidy_core::env::Clock;
use tidy_core::model::{
    Class, FolderNode, Group as CleanGroup, HistoryEntry, Item as CleanItem, ItemResult, LargeItem,
    LargeKind, LargeScan, Plan, RunningSet, SettingsLink, Source, TrashSummary, UseSource,
};
use tidy_core::settings::Settings;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::format;
use crate::macos::{self, DiskInfo, Notifier, Terminator};
use crate::motion;
use crate::theme::motion::NOTE_OUT;
use crate::views::checkup::CheckupState;
use crate::views::main_window::open_main_window;
use crate::views::memory::MemoryState;
use crate::views::security::SecurityState;
use crate::views::trust::SystemDataState;

actions!(
    tidy,
    [
        Quit,
        CheckForUpdates,
        CloseWindow,
        GoCheckup,
        GoCleanup,
        GoLarge,
        GoMemory,
        GoSecurity,
        Rescan,
        ParentFolder,
        TrashSelection,
        DialogConfirm,
        DialogCancel
    ]
);

/// Key context of the main window's root element.
pub const ROOT_CONTEXT: &str = "Tidy";
/// How often the pump may update the UI.
pub const PUMP_PERIOD: Duration = Duration::from_millis(100);
/// How often the Cleanup screen re-checks which apps are running (C6).
pub const RUNNING_PERIOD: Duration = Duration::from_secs(3);
/// How often the Memory screen asks for a new snapshot, while it is the visible screen and
/// never otherwise (design section 5.3). It is the only screen that polls.
pub const MEMORY_PERIOD: Duration = Duration::from_secs(2);
/// How long a polite Quit may take before the row says "Still open" (design section 5.3).
pub const QUIT_WAIT: Duration = Duration::from_secs(10);
/// A Cleanup scan older than this is repeated when the screen opens (after the first one).
pub const STALE_SCAN_SECS: u64 = 600;
/// Bound on the synchronous shutdown when quitting.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// The sentence under a failed Empty Trash (design 5.1).
pub const EMPTY_TRASH_REFUSED: &str =
    "macOS didn't let Tidy empty the Trash. Empty it from Finder.";

// ---------- Globals ----------

/// The tokio runtime the core runs on. Dropping it shuts tokio down.
pub struct Tokio(pub tokio::runtime::Runtime);
impl Global for Tokio {}

/// Sends commands to the core.
pub struct Core(pub CoreHandle);
impl Global for Core {}

#[derive(Default)]
pub struct MainWindow(pub Option<AnyWindowHandle>);
impl Global for MainWindow {}

/// The shared Tidy model. It outlives the main window, so closing and reopening the window
/// keeps the pump working.
pub struct Model(pub Entity<TidyModel>);
impl Global for Model {}

/// The clock the screens use for "Scanned 2 min ago" and the "not opened" dates. Under
/// `TIDY_TEST_NOW` it is the core's fixed clock, so the labels match the data.
pub struct Now(pub Arc<dyn Clock>);
impl Global for Now {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenKind {
    /// Open the file with its default app.
    Open,
    /// Show it in Finder.
    Reveal,
}

/// Opens or reveals a path.
pub type OpenFn = Box<dyn Fn(OpenKind, &Path, &App)>;

/// Opens or reveals files. Tests replace it to record calls.
pub struct Opener(pub OpenFn);
impl Global for Opener {}

impl Opener {
    pub fn system() -> Opener {
        Opener(Box::new(|kind, path, cx| match kind {
            OpenKind::Open => cx.open_with_system(path),
            OpenKind::Reveal => cx.reveal_path(path),
        }))
    }
}

/// Opens a System Settings page. Only ever called from a click; tests replace it to record.
pub type SettingsFn = Box<dyn Fn(SettingsLink, &App)>;
pub struct SettingsOpener(pub SettingsFn);
impl Global for SettingsOpener {}

impl SettingsOpener {
    pub fn system() -> SettingsOpener {
        SettingsOpener(Box::new(|link, cx| cx.open_url(link.url())))
    }
}

/// Opens a System Settings page through the `SettingsOpener` global (the system one by default).
pub fn open_settings(link: SettingsLink, cx: &App) {
    match cx.try_global::<SettingsOpener>() {
        Some(opener) => (opener.0)(link, cx),
        None => SettingsOpener::system().0(link, cx),
    }
}

/// Asks apps to quit and gives their icons (the Memory screen). Tests replace it with a fake,
/// so no test ever signals a process.
pub struct Terminate(pub Rc<dyn Terminator>);
impl Global for Terminate {}

/// The weekly reminder's notification. Tests replace it with a fake.
pub struct Notify(pub Rc<dyn Notifier>);
impl Global for Notify {}

/// The installed [`Terminator`], if any. Without one (a demo) a Quit does nothing.
pub fn terminator(cx: &App) -> Option<Rc<dyn Terminator>> {
    cx.try_global::<Terminate>().map(|t| t.0.clone())
}

/// The installed [`Notifier`], if any. Without one (a demo) nothing is scheduled.
pub fn notifier(cx: &App) -> Option<Rc<dyn Notifier>> {
    cx.try_global::<Notify>().map(|n| n.0.clone())
}

#[derive(Default)]
struct ShutdownState {
    done: bool,
}
impl Global for ShutdownState {}

/// True when TIDY_BACKGROUND=1 or TIDY_DEMO is set: never activate the app or raise windows.
/// A demo can never show a window, even when TIDY_BACKGROUND is missing.
pub fn background_mode() -> bool {
    background_from(
        std::env::var("TIDY_BACKGROUND").ok().as_deref(),
        std::env::var("TIDY_DEMO").ok().as_deref(),
    )
}

/// The decision behind `background_mode`, from the two variables' values.
pub fn background_from(background: Option<&str>, demo: Option<&str>) -> bool {
    background.is_some_and(|v| !v.is_empty() && v != "0") || demo.is_some_and(crate::demo::is_on)
}

/// Sends a command to the core. Returns false when there is no core or it stopped.
pub fn send(cx: &App, cmd: Command) -> bool {
    match cx.try_global::<Core>() {
        Some(core) => core.0.send(cmd),
        None => {
            tracing::warn!("no core to send {cmd:?} to");
            false
        }
    }
}

/// Opens or reveals a path through the `Opener` global (the system one by default).
pub fn open_path(kind: OpenKind, path: &Path, cx: &App) {
    match cx.try_global::<Opener>() {
        Some(opener) => (opener.0)(kind, path, cx),
        None => Opener::system().0(kind, path, cx),
    }
}

/// The current time in Unix seconds: the demo's fixed clock, the core's clock, or the system.
pub fn now_unix(cx: &App) -> u64 {
    if let Some(demo) = crate::demo::state(cx) {
        return demo.now_unix;
    }
    match cx.try_global::<Now>() {
        Some(now) => now.0.unix_now(),
        None => tidy_core::model::unix_secs(std::time::SystemTime::now()),
    }
}

/// The model, created on first use.
pub fn model(cx: &mut App) -> Entity<TidyModel> {
    if let Some(m) = cx.try_global::<Model>() {
        return m.0.clone();
    }
    let m = cx.new(|_| TidyModel::new(Settings::default()));
    cx.set_global(Model(m.clone()));
    m
}

// ---------- The model ----------

/// The five screens, in sidebar order (design section 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Screen {
    Checkup,
    Cleanup,
    Large,
    Memory,
    Security,
}

impl Screen {
    pub const ALL: [Screen; 5] = [
        Screen::Checkup,
        Screen::Cleanup,
        Screen::Large,
        Screen::Memory,
        Screen::Security,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Screen::Checkup => "Check up",
            Screen::Cleanup => "Cleanup",
            Screen::Large => "Large files",
            Screen::Memory => "Memory",
            Screen::Security => "Security",
        }
    }

    /// The shortcut shown in the Go menu: Cmd-1 to Cmd-5 in sidebar order.
    pub fn shortcut(self) -> &'static str {
        match self {
            Screen::Checkup => "\u{2318}1",
            Screen::Cleanup => "\u{2318}2",
            Screen::Large => "\u{2318}3",
            Screen::Memory => "\u{2318}4",
            Screen::Security => "\u{2318}5",
        }
    }
}

/// The band under the toolbar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// "Moved 6.3 GB to the Trash. Empty the Trash to free the space."
    Moved { bytes: u64 },
    /// A sentence about something that failed.
    Error(String),
    /// A plain sentence about something that happened.
    Info(String),
}

/// The tri-state of a group checkbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    Off,
    On,
    Mixed,
}

/// A running clean or trash: `(done, total)`.
pub type Progress = (usize, usize);

#[derive(Default)]
pub struct CleanupState {
    pub plan: Option<Plan>,
    /// Ids the user has ticked (starts as the plan's Safe items).
    pub ticked: HashSet<String>,
    pub collapsed: HashSet<CleanGroup>,
    group_init: HashSet<CleanGroup>,
    /// Current blocked reasons by item id ("Quit Google Chrome first").
    pub blocked: HashMap<String, String>,
    /// The reasons the plan came with, to re-block an item whose app starts again.
    watch: HashMap<String, String>,
    /// Why an item was skipped at clean time, by id. The row stays with this line.
    pub skipped: HashMap<String, String>,
    /// Rows fading out because their item reached the Trash.
    pub leaving: HashSet<String>,
    /// Rows that are gone from the list.
    pub gone: HashSet<String>,
    pub running: RunningSet,
    /// True once the core has sent a process list, so `running` is real and not the default.
    running_known: bool,
    /// A scan runs: the files counted so far.
    pub scanning: Option<u64>,
    pub cleaning: Option<Progress>,
    pub notice: Option<Notice>,
    /// A dismissed notice that is still fading out (200 ms).
    pub notice_leaving: Option<Notice>,
    /// Items Tidy moved to the Trash that are still there.
    pub trash: TrashSummary,
}

impl CleanupState {
    /// Items of a group that are still in the list (fading ones included).
    pub fn visible_items(&self, group: CleanGroup) -> Vec<&CleanItem> {
        match &self.plan {
            Some(p) => p
                .group(group)
                .filter(|i| !self.gone.contains(&i.id))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn visible_count(&self) -> usize {
        self.plan.as_ref().map_or(0, |p| {
            p.items
                .iter()
                .filter(|i| !self.gone.contains(&i.id))
                .count()
        })
    }

    /// The groups with at least one item left, in display order.
    pub fn groups(&self) -> Vec<CleanGroup> {
        CleanGroup::ALL
            .into_iter()
            .filter(|g| !self.visible_items(*g).is_empty())
            .collect()
    }

    /// What fails a guard of `id` right now: its app is running (blocked), or the last clean
    /// skipped it at the check before the move.
    pub fn guard_state(&self, id: &str) -> tidy_core::explain::GuardState {
        tidy_core::explain::GuardState {
            owner_running: self.blocked.contains_key(id),
            recheck_failed: self.skipped.contains_key(id),
        }
    }

    pub fn blocked_reason(&self, item: &CleanItem) -> Option<&str> {
        self.blocked.get(&item.id).map(String::as_str)
    }

    /// Whether the user could tick the item now.
    pub fn enabled(&self, item: &CleanItem) -> bool {
        item.class.selectable() && !self.blocked.contains_key(&item.id)
    }

    pub fn is_ticked(&self, item: &CleanItem) -> bool {
        self.ticked.contains(&item.id)
    }

    /// The ticked items still in the list, in plan order.
    pub fn ticked_items(&self) -> Vec<&CleanItem> {
        match &self.plan {
            Some(p) => p
                .items
                .iter()
                .filter(|i| {
                    self.ticked.contains(&i.id)
                        && self.enabled(i)
                        && !self.gone.contains(&i.id)
                        && !self.leaving.contains(&i.id)
                })
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn ticked_bytes(&self) -> u64 {
        self.ticked_items().iter().map(|i| i.bytes_alloc).sum()
    }

    pub fn any_review_ticked(&self) -> bool {
        self.ticked_items().iter().any(|i| i.class == Class::Review)
    }

    /// The Review items still unticked: the "34 GB more to review" figure.
    pub fn review_bytes(&self) -> u64 {
        match &self.plan {
            Some(p) => p
                .items
                .iter()
                .filter(|i| {
                    i.class == Class::Review
                        && !self.gone.contains(&i.id)
                        && !self.ticked.contains(&i.id)
                })
                .map(|i| i.bytes_alloc)
                .sum(),
            None => 0,
        }
    }

    /// The Safe total after a scan, for the sidebar: what the plan offered ticked, less what has
    /// left the list since.
    pub fn safe_bytes(&self) -> Option<u64> {
        self.plan.as_ref().map(|p| {
            p.items
                .iter()
                .filter(|i| i.class == Class::Safe && i.blocked.is_none())
                .filter(|i| !self.gone.contains(&i.id))
                .map(|i| i.bytes_alloc)
                .sum()
        })
    }

    pub fn group_bytes(&self, group: CleanGroup) -> u64 {
        self.visible_items(group)
            .iter()
            .map(|i| i.bytes_alloc)
            .sum()
    }

    /// The state of a group checkbox, or None when no item of the group can be ticked.
    pub fn group_tri(&self, group: CleanGroup) -> Option<Tri> {
        let items = self.visible_items(group);
        let enabled: Vec<&&CleanItem> = items.iter().filter(|i| self.enabled(i)).collect();
        if enabled.is_empty() {
            return None;
        }
        let on = enabled.iter().filter(|i| self.is_ticked(i)).count();
        Some(if on == 0 {
            Tri::Off
        } else if on == enabled.len() {
            Tri::On
        } else {
            Tri::Mixed
        })
    }

    /// Whether the empty state shows: no plan, or a plan with nothing left.
    pub fn is_empty_result(&self) -> bool {
        self.plan.is_some() && self.visible_count() == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LargeMode {
    Files,
    Folders,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LargeFilter {
    Biggest,
    NotOpened,
}

/// One row of the Large files list.
#[derive(Clone, Debug, PartialEq)]
pub enum LargeRow {
    Item(LargeEntry),
    /// "1,204 smaller items, 3.1 GB": no checkbox, no actions.
    Rest {
        count: u64,
        bytes: u64,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct LargeEntry {
    /// Relative to the scan root with `/` separators. The row's id.
    pub rel: String,
    pub name: String,
    pub path: PathBuf,
    pub kind: LargeKind,
    pub bytes: u64,
    pub last_used: Option<u64>,
    pub source: UseSource,
    pub app_data: bool,
    /// Files inside (Folders mode).
    pub files: u64,
    /// Folders mode: the size as a share of the open folder, 0 to 1.
    pub share: Option<f32>,
    /// Folders mode: a double click or the chevron opens it.
    pub drill: bool,
}

pub struct LargeState {
    pub scan: Option<LargeScan>,
    pub mode: LargeMode,
    pub filter: LargeFilter,
    /// The threshold in bytes. Changing it refilters the saved scan.
    pub min: u64,
    /// The open folder in Folders mode: `/`-separated, empty for the home folder.
    pub folder: String,
    /// Ticked rows, by `rel`.
    pub selected: BTreeSet<String>,
    pub skipped: HashMap<String, String>,
    pub leaving: HashSet<String>,
    pub gone: HashSet<String>,
    pub scanning: Option<u64>,
    pub trashing: Option<Progress>,
    pub notice: Option<Notice>,
    /// A dismissed notice that is still fading out (200 ms).
    pub notice_leaving: Option<Notice>,
}

impl LargeState {
    pub fn new(min: u64) -> LargeState {
        LargeState {
            scan: None,
            mode: LargeMode::Files,
            filter: LargeFilter::Biggest,
            min,
            folder: String::new(),
            selected: BTreeSet::new(),
            skipped: HashMap::new(),
            leaving: HashSet::new(),
            gone: HashSet::new(),
            scanning: None,
            trashing: None,
            notice: None,
            notice_leaving: None,
        }
    }

    /// The rows for the current mode, filter, threshold and folder. Rows of items already
    /// trashed are left out. `days` is the "not opened" age.
    pub fn rows(&self, now: u64, days: u32) -> Vec<LargeRow> {
        let Some(scan) = &self.scan else {
            return Vec::new();
        };
        match self.mode {
            LargeMode::Files => {
                let items: Vec<&LargeItem> = match self.filter {
                    LargeFilter::Biggest => scan.biggest(self.min),
                    LargeFilter::NotOpened => scan.not_opened(self.min, now, days),
                };
                items
                    .into_iter()
                    .filter(|i| !self.gone.contains(&i.rel))
                    .map(|i| {
                        LargeRow::Item(LargeEntry {
                            rel: i.rel.clone(),
                            name: i.name.clone(),
                            path: i.path.clone(),
                            kind: i.kind,
                            bytes: i.bytes_alloc,
                            last_used: i.last_used,
                            source: i.used_source,
                            app_data: i.app_data,
                            files: 0,
                            share: None,
                            drill: false,
                        })
                    })
                    .collect()
            }
            LargeMode::Folders => {
                let node = scan.tree.find(&self.folder).unwrap_or(&scan.tree);
                let (shown, rest_count, rest_alloc) = node.split_at(self.min);
                let total = node.bytes_alloc.max(1) as f64;
                let mut rows: Vec<LargeRow> = shown
                    .into_iter()
                    .filter(|c| !self.gone.contains(&c.rel))
                    .map(|c| {
                        let kind = c.kind.unwrap_or(LargeKind::Folder);
                        LargeRow::Item(LargeEntry {
                            rel: c.rel.clone(),
                            name: c.name.clone(),
                            path: scan.root.join(&c.rel),
                            kind,
                            bytes: c.bytes_alloc,
                            last_used: None,
                            source: UseSource::FileTimes,
                            app_data: c.rel == "Library" || c.rel.starts_with("Library/"),
                            files: c.files,
                            share: Some((c.bytes_alloc as f64 / total).clamp(0., 1.) as f32),
                            drill: kind == LargeKind::Folder && has_contents(c),
                        })
                    })
                    .collect();
                if rest_count > 0 {
                    rows.push(LargeRow::Rest {
                        count: rest_count,
                        bytes: rest_alloc,
                    });
                }
                rows
            }
        }
    }

    /// The breadcrumb segments in Folders mode: (label, rel) from "Home" down to the open
    /// folder.
    pub fn breadcrumb(&self) -> Vec<(String, String)> {
        let mut out = vec![("Home".to_string(), String::new())];
        let mut rel = String::new();
        for part in self.folder.split('/').filter(|p| !p.is_empty()) {
            if !rel.is_empty() {
                rel.push('/');
            }
            rel.push_str(part);
            out.push((part.to_string(), rel.clone()));
        }
        out
    }

    /// The rows that are ticked and visible, in list order.
    pub fn selected_entries(&self, rows: &[LargeRow]) -> Vec<LargeEntry> {
        rows.iter()
            .filter_map(|r| match r {
                LargeRow::Item(e) if self.selected.contains(&e.rel) => Some(e.clone()),
                _ => None,
            })
            .collect()
    }

    /// The count for the sidebar: items at or above the threshold.
    pub fn item_count(&self) -> Option<usize> {
        self.scan.as_ref().map(|s| {
            s.biggest(self.min)
                .into_iter()
                .filter(|i| !self.gone.contains(&i.rel))
                .count()
        })
    }

    /// The `rel` of a path under the scan root, if it is one.
    fn rel_of(&self, path: &Path) -> Option<String> {
        let scan = self.scan.as_ref()?;
        let rel = path.strip_prefix(&scan.root).ok()?;
        Some(rel.to_string_lossy().replace('\\', "/"))
    }
}

fn has_contents(node: &FolderNode) -> bool {
    !node.children.is_empty() || node.rest_count > 0
}

/// The History sheet's data.
#[derive(Default)]
pub struct HistoryState {
    /// None until the core has answered.
    pub entries: Option<Vec<HistoryEntry>>,
    /// Item paths that are still at their original place (checked when the entries load).
    pub present: HashSet<PathBuf>,
}

pub struct TidyModel {
    pub screen: Screen,
    pub settings: Settings,
    pub cleanup: CleanupState,
    pub large: LargeState,
    pub history: HistoryState,
    pub memory: MemoryState,
    pub security: SecurityState,
    pub checkup: CheckupState,
    /// The System Data sheet (design 17.2).
    pub system_data: SystemDataState,
    pub disk: Option<DiskInfo>,
    /// Number of applied batches (tests check the pump's throttle with it).
    pub applies: usize,
    activity: Option<macos::ActivityToken>,
}

impl TidyModel {
    pub fn new(settings: Settings) -> TidyModel {
        let min = settings.large_min_bytes;
        TidyModel {
            screen: Screen::Cleanup,
            settings,
            cleanup: CleanupState::default(),
            large: LargeState::new(min),
            history: HistoryState::default(),
            memory: MemoryState::default(),
            security: SecurityState::default(),
            checkup: CheckupState::default(),
            system_data: SystemDataState::default(),
            disk: None,
            applies: 0,
            activity: None,
        }
    }

    /// Loads fixed demo data directly. It makes no App Nap or disk call and starts no timer.
    pub fn load_demo(
        &mut self,
        screen: Screen,
        plan: Option<Plan>,
        large: Option<LargeScan>,
        disk: Option<DiskInfo>,
    ) {
        self.screen = screen;
        if let Some(plan) = plan {
            self.set_plan(plan);
        }
        self.large.scan = large;
        self.disk = disk;
    }

    /// The saved result of the last Large files scan, shown until a new scan arrives.
    pub fn load_cached_large(&mut self, scan: LargeScan) {
        self.large.scan = Some(scan);
    }

    // ----- screens -----

    pub fn select_screen(&mut self, screen: Screen, cx: &mut Context<Self>) {
        if self.screen != screen {
            self.screen = screen;
            self.enter_screen(cx);
            cx.notify();
        }
        self.maybe_auto_scan(cx);
    }

    /// What a screen does when it becomes the visible one. Everything here only reads: the
    /// Memory snapshot, the last saved Security report, the last check up, the notification
    /// permission (never a prompt). Nothing scans or runs a check on its own.
    fn enter_screen(&mut self, cx: &mut Context<Self>) {
        match self.screen {
            Screen::Memory => self.enter_memory(cx),
            Screen::Security => self.load_last_security(cx),
            Screen::Checkup => self.enter_checkup(cx),
            Screen::Cleanup | Screen::Large => {}
        }
    }

    /// The window opened (or opened again) on whatever screen was selected: do what entering
    /// it does. Not called in demo mode.
    pub fn on_view_opened(&mut self, cx: &mut Context<Self>) {
        self.enter_screen(cx);
        self.maybe_auto_scan(cx);
    }

    /// Later visits to Cleanup rescan on their own when the last scan is older than 10 minutes
    /// (C3). The first scan is always the user's click.
    pub fn maybe_auto_scan(&mut self, cx: &mut Context<Self>) {
        if self.screen != Screen::Cleanup
            || !self.settings.cleanup_scanned_once
            || self.cleanup.scanning.is_some()
            || self.cleanup.cleaning.is_some()
        {
            return;
        }
        let stale = match &self.cleanup.plan {
            None => true,
            Some(p) => now_unix(cx).saturating_sub(p.scanned_at) > STALE_SCAN_SECS,
        };
        if stale {
            self.scan_cleanup(cx);
        }
    }

    /// Cmd-R: rescan the current screen.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        match self.screen {
            Screen::Checkup => self.run_checkup(cx),
            Screen::Cleanup => self.scan_cleanup(cx),
            Screen::Large => self.scan_large(cx),
            Screen::Memory => self.snapshot_memory(cx),
            Screen::Security => self.run_security(false, cx),
        }
    }

    pub(crate) fn remember(&mut self, edit: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let mut next = self.settings.clone();
        edit(&mut next);
        if next != self.settings {
            self.settings = next.clone();
            send(cx, Command::UpdateSettings(next));
        }
    }

    // ----- Cleanup -----

    pub fn scan_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.cleanup.scanning.is_some() || self.cleanup.cleaning.is_some() {
            return;
        }
        if send(cx, Command::ScanCleanup) {
            self.cleanup.scanning = Some(0);
            self.begin_activity();
            // After the first explicit scan, later visits may scan on their own.
            self.remember(|s| s.cleanup_scanned_once = true, cx);
            cx.notify();
        }
    }

    pub fn toggle_item(&mut self, id: &str, cx: &mut Context<Self>) {
        let enabled = self
            .cleanup
            .plan
            .as_ref()
            .and_then(|p| p.item(id))
            .is_some_and(|i| self.cleanup.enabled(i));
        if !enabled || self.cleanup.cleaning.is_some() {
            return;
        }
        if !self.cleanup.ticked.remove(id) {
            self.cleanup.ticked.insert(id.to_string());
        }
        self.cleanup.skipped.remove(id);
        cx.notify();
    }

    /// Ticks every tickable item of the group, or clears them when all are ticked.
    pub fn toggle_group(&mut self, group: CleanGroup, cx: &mut Context<Self>) {
        if self.cleanup.cleaning.is_some() {
            return;
        }
        let Some(tri) = self.cleanup.group_tri(group) else {
            return;
        };
        let ids: Vec<String> = self
            .cleanup
            .visible_items(group)
            .into_iter()
            .filter(|i| self.cleanup.enabled(i))
            .map(|i| i.id.clone())
            .collect();
        for id in ids {
            if tri == Tri::On {
                self.cleanup.ticked.remove(&id);
            } else {
                self.cleanup.ticked.insert(id);
            }
        }
        cx.notify();
    }

    pub fn toggle_collapsed(&mut self, group: CleanGroup, cx: &mut Context<Self>) {
        if !self.cleanup.collapsed.remove(&group) {
            self.cleanup.collapsed.insert(group);
        }
        cx.notify();
    }

    /// Sends `Clean` for the ticked items. Returns false when nothing was sent.
    pub fn clean_ticked(&mut self, cx: &mut Context<Self>) -> bool {
        if self.cleanup.cleaning.is_some() {
            return false;
        }
        let ids: Vec<String> = self
            .cleanup
            .ticked_items()
            .into_iter()
            .map(|i| i.id.clone())
            .collect();
        if ids.is_empty() {
            return false;
        }
        let total = ids.len();
        if !send(cx, Command::Clean { item_ids: ids }) {
            return false;
        }
        self.cleanup.cleaning = Some((0, total));
        self.cleanup.notice = None;
        self.begin_activity();
        cx.notify();
        true
    }

    /// "Never show this again": the item leaves the list and its path goes on the ignore list.
    pub fn ignore_item(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(item) = self.cleanup.plan.as_ref().and_then(|p| p.item(id)) else {
            return;
        };
        let path = item.path.clone();
        if send(cx, Command::Ignore { path }) {
            self.cleanup.gone.insert(id.to_string());
            self.cleanup.ticked.remove(id);
            cx.notify();
        }
    }

    pub fn empty_trash(&mut self, cx: &mut Context<Self>) {
        send(cx, Command::EmptyTidyTrash);
    }

    /// Dismisses the band: it fades out over 200 ms (at once under Reduce Motion).
    pub fn dismiss_cleanup_notice(&mut self, cx: &mut Context<Self>) {
        let Some(notice) = self.cleanup.notice.take() else {
            return;
        };
        if !cx.reduce_motion() {
            self.cleanup.notice_leaving = Some(notice);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTE_OUT).await;
                let _ = this.update(cx, |m, cx| {
                    m.cleanup.notice_leaving = None;
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// The apps that block items may have quit or started: re-check every blocked item.
    fn refresh_blocked(&mut self) {
        let watch: Vec<(String, String)> = self
            .cleanup
            .watch
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (id, reason) in watch {
            let owner = format::blocked_owner(&reason).unwrap_or(&reason);
            let running = format::owner_running(owner, &self.cleanup.running);
            let safe = self
                .cleanup
                .plan
                .as_ref()
                .and_then(|p| p.item(&id))
                .is_some_and(|i| i.class == Class::Safe);
            if running {
                self.cleanup.blocked.insert(id.clone(), reason);
                self.cleanup.ticked.remove(&id);
            } else if self.cleanup.blocked.remove(&id).is_some() && safe {
                self.cleanup.ticked.insert(id);
            }
        }
    }

    /// Whether the 3 second process re-check has anything to do.
    pub fn watches_running(&self) -> bool {
        !self.cleanup.watch.is_empty() && self.cleanup.plan.is_some()
    }

    fn set_plan(&mut self, plan: Plan) {
        let c = &mut self.cleanup;
        c.ticked = plan.ticked().map(|i| i.id.clone()).collect();
        c.blocked = plan
            .items
            .iter()
            .filter_map(|i| i.blocked.clone().map(|b| (i.id.clone(), b)))
            .collect();
        c.watch = c.blocked.clone();
        c.skipped.clear();
        c.leaving.clear();
        c.gone.clear();
        // What the scan could not do (a folder macOS kept closed, a rule that ran out of
        // time) is said once, in the band.
        c.notice = (!plan.skipped.is_empty()).then(|| Notice::Error(plan.skipped.join(" ")));
        c.notice_leaving = None;
        c.trash = plan.trash;
        c.scanning = None;
        for g in plan.groups() {
            if c.group_init.insert(g) && !g.starts_expanded() {
                c.collapsed.insert(g);
            }
        }
        c.plan = Some(plan);
        // The plan's blocked reasons come from the scan's own process snapshot. The core only
        // sends RunningChanged when the set changes, so an app that quit during the scan would
        // keep its items blocked until the next rescan. Apply the latest set now.
        if self.cleanup.running_known {
            self.refresh_blocked();
        }
    }

    // ----- Large files -----

    pub fn scan_large(&mut self, cx: &mut Context<Self>) {
        if self.large.scanning.is_some() || self.large.trashing.is_some() {
            return;
        }
        if send(cx, Command::ScanLarge { record_min: None }) {
            self.large.scanning = Some(0);
            self.begin_activity();
            self.remember(|s| s.large_scanned_once = true, cx);
            cx.notify();
        }
    }

    /// Stop on the toolbar: cancels the running scan.
    pub fn stop_scan(&mut self, cx: &mut Context<Self>) {
        send(cx, Command::Cancel);
    }

    pub fn set_mode(&mut self, mode: LargeMode, cx: &mut Context<Self>) {
        if self.large.mode != mode {
            self.large.mode = mode;
            self.large.selected.clear();
            self.large.folder.clear();
            cx.notify();
        }
    }

    pub fn set_filter(&mut self, filter: LargeFilter, cx: &mut Context<Self>) {
        if self.large.filter != filter {
            self.large.filter = filter;
            self.retain_visible_selection(cx);
            cx.notify();
        }
    }

    /// Changes the threshold. The saved scan is refiltered; nothing is rescanned.
    pub fn set_threshold(&mut self, min: u64, cx: &mut Context<Self>) {
        if self.large.min != min {
            self.large.min = min;
            self.retain_visible_selection(cx);
            self.remember(|s| s.large_min_bytes = min, cx);
            cx.notify();
        }
    }

    fn retain_visible_selection(&mut self, cx: &App) {
        let now = now_unix(cx);
        let rows = self.large.rows(now, self.settings.not_opened_days);
        let visible: HashSet<&str> = rows
            .iter()
            .filter_map(|r| match r {
                LargeRow::Item(e) => Some(e.rel.as_str()),
                _ => None,
            })
            .collect();
        self.large.selected.retain(|r| visible.contains(r.as_str()));
    }

    pub fn open_folder(&mut self, rel: &str, cx: &mut Context<Self>) {
        if self.large.mode != LargeMode::Folders {
            return;
        }
        let rel = rel.trim_matches('/').to_string();
        if self
            .large
            .scan
            .as_ref()
            .is_some_and(|s| s.tree.find(&rel).is_some())
            && self.large.folder != rel
        {
            self.large.folder = rel;
            self.large.selected.clear();
            cx.notify();
        }
    }

    /// Cmd-Up: the parent of the open folder.
    pub fn parent_folder(&mut self, cx: &mut Context<Self>) {
        if self.large.mode != LargeMode::Folders || self.large.folder.is_empty() {
            return;
        }
        let parent = self
            .large
            .folder
            .rsplit_once('/')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        self.large.folder = parent;
        self.large.selected.clear();
        cx.notify();
    }

    pub fn toggle_select(&mut self, rel: &str, cx: &mut Context<Self>) {
        if self.large.trashing.is_some() {
            return;
        }
        if !self.large.selected.remove(rel) {
            self.large.selected.insert(rel.to_string());
        }
        cx.notify();
    }

    /// Sends `TrashPaths` for these rows (by `rel`). Returns false when nothing was sent.
    pub fn trash_rels(&mut self, rels: &[String], cx: &mut Context<Self>) -> bool {
        let Some(root) = self.large.scan.as_ref().map(|s| s.root.clone()) else {
            return false;
        };
        if rels.is_empty() || self.large.trashing.is_some() {
            return false;
        }
        let paths: Vec<PathBuf> = rels.iter().map(|r| root.join(r)).collect();
        let total = paths.len();
        if !send(cx, Command::TrashPaths { paths }) {
            return false;
        }
        self.large.trashing = Some((0, total));
        self.large.notice = None;
        self.begin_activity();
        cx.notify();
        true
    }

    /// Dismisses the band: it fades out over 200 ms (at once under Reduce Motion).
    pub fn dismiss_large_notice(&mut self, cx: &mut Context<Self>) {
        let Some(notice) = self.large.notice.take() else {
            return;
        };
        if !cx.reduce_motion() {
            self.large.notice_leaving = Some(notice);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTE_OUT).await;
                let _ = this.update(cx, |m, cx| {
                    m.large.notice_leaving = None;
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    // ----- History -----

    /// Asks the core for the history and fills `history` when the answer arrives.
    pub fn load_history(&mut self, cx: &mut Context<Self>) {
        let (reply, rx) = tokio::sync::oneshot::channel();
        if !send(cx, Command::LoadHistory { reply }) {
            self.set_history(Vec::new());
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let entries = rx.await.unwrap_or_default();
            let _ = this.update(cx, |m, cx| {
                m.set_history(entries);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn set_history(&mut self, entries: Vec<HistoryEntry>) {
        self.history.present = entries
            .iter()
            .flat_map(|e| e.items.iter())
            .filter(|i| matches!(i.result, ItemResult::Skipped | ItemResult::Failed))
            .filter(|i| i.path.exists())
            .map(|i| i.path.clone())
            .collect();
        self.history.entries = Some(entries);
    }

    // ----- events -----

    pub(crate) fn begin_activity(&mut self) {
        if self.activity.is_none() && crate::demo::active().is_none() {
            self.activity = Some(macos::ActivityToken::begin("Scanning"));
        }
    }

    pub(crate) fn end_activity_if_idle(&mut self) {
        let busy = self.cleanup.scanning.is_some()
            || self.cleanup.cleaning.is_some()
            || self.large.scanning.is_some()
            || self.large.trashing.is_some()
            || self.security.running.is_some()
            || self.checkup.running;
        if !busy {
            self.activity = None;
        }
    }

    /// Applies one coalesced batch of core events.
    pub fn apply(&mut self, batch: Batch, cx: &mut Context<Self>) {
        let mut leaving_cleanup = Vec::new();
        let mut leaving_large = Vec::new();
        let mut refresh_disk = false;
        for event in batch.events {
            match event {
                Event::ScanProgress { kind, files, .. } => match kind {
                    ScanKind::Cleanup => {
                        if self.cleanup.scanning.is_some() {
                            self.cleanup.scanning = Some(files);
                        }
                    }
                    ScanKind::Large => {
                        if self.large.scanning.is_some() {
                            self.large.scanning = Some(files);
                        }
                    }
                },
                Event::CleanupPlan(plan) => {
                    self.set_plan(plan);
                    self.checkup.parts.cleanup = true;
                    refresh_disk = true;
                }
                Event::LargeResult(scan) => {
                    self.large.scan = Some(scan);
                    self.large.scanning = None;
                    self.large.selected.clear();
                    self.large.skipped.clear();
                    self.large.leaving.clear();
                    self.large.gone.clear();
                    self.large.notice = None;
                    self.large.notice_leaving = None;
                    let tree_has_folder = self
                        .large
                        .scan
                        .as_ref()
                        .is_some_and(|s| s.tree.find(&self.large.folder).is_some());
                    if !tree_has_folder {
                        self.large.folder.clear();
                    }
                    refresh_disk = true;
                }
                Event::ScanStopped { kind } => match kind {
                    ScanKind::Cleanup => self.cleanup.scanning = None,
                    ScanKind::Large => self.large.scanning = None,
                },
                Event::CleanProgress { done, total } => {
                    if self.cleanup.cleaning.is_some() {
                        self.cleanup.cleaning = Some((done, total));
                    } else if self.large.trashing.is_some() {
                        self.large.trashing = Some((done, total));
                    }
                }
                Event::ItemTrashed { id, .. } => {
                    if self.security.is_trashing(Path::new(&id)) {
                        self.security.on_trashed();
                    } else if self
                        .cleanup
                        .plan
                        .as_ref()
                        .is_some_and(|p| p.item(&id).is_some())
                    {
                        self.cleanup.skipped.remove(&id);
                        self.cleanup.ticked.remove(&id);
                        self.cleanup.leaving.insert(id.clone());
                        leaving_cleanup.push(id);
                    } else if let Some(rel) = self.large.rel_of(Path::new(&id)) {
                        self.large.selected.remove(&rel);
                        self.large.skipped.remove(&rel);
                        self.large.leaving.insert(rel.clone());
                        leaving_large.push(rel);
                    }
                }
                Event::ItemSkipped { id, reason } => {
                    if self.security.is_trashing(Path::new(&id)) {
                        self.security.on_skipped(reason);
                    } else if self
                        .cleanup
                        .plan
                        .as_ref()
                        .is_some_and(|p| p.item(&id).is_some())
                    {
                        self.cleanup.leaving.remove(&id);
                        self.cleanup.ticked.remove(&id);
                        self.cleanup.skipped.insert(id, reason);
                    } else if let Some(rel) = self.large.rel_of(Path::new(&id)) {
                        self.large.leaving.remove(&rel);
                        self.large.selected.remove(&rel);
                        self.large.skipped.insert(rel, reason);
                    }
                }
                Event::CleanDone(entry) if self.security_trash_done(&entry) => {
                    // A login item went to the Trash from the Security screen.
                }
                Event::CleanDone(entry) => {
                    self.cleanup.trash.items += entry.trashed().count() as u64;
                    self.cleanup.trash.bytes += entry.trashed_bytes;
                    let notice = Notice::Moved {
                        bytes: entry.trashed_bytes,
                    };
                    match entry.source {
                        Source::Cleanup => {
                            self.cleanup.cleaning = None;
                            self.cleanup.notice = Some(notice);
                        }
                        Source::Large => {
                            self.large.trashing = None;
                            self.large.notice = Some(notice);
                        }
                    }
                    refresh_disk = true;
                }
                Event::EmptyTrashDone(entry) => {
                    let failed = entry.message.is_some()
                        || entry.items.iter().any(|i| i.result == ItemResult::Failed);
                    let deleted = entry
                        .items
                        .iter()
                        .filter(|i| i.result == ItemResult::Deleted)
                        .count() as u64;
                    let t = &mut self.cleanup.trash;
                    t.items = t.items.saturating_sub(deleted);
                    t.bytes = t.bytes.saturating_sub(entry.trashed_bytes);
                    if failed {
                        self.cleanup.notice = Some(Notice::Error(EMPTY_TRASH_REFUSED.to_string()));
                    } else {
                        *t = TrashSummary::default();
                        if let Some(plan) = &self.cleanup.plan {
                            for item in plan.group(CleanGroup::Trash) {
                                self.cleanup.gone.insert(item.id.clone());
                            }
                        }
                        self.cleanup.notice = None;
                    }
                    refresh_disk = true;
                }
                Event::RunningChanged(set) => {
                    self.cleanup.running = set;
                    self.cleanup.running_known = true;
                    self.refresh_blocked();
                }
                Event::SettingsChanged(settings) => {
                    self.settings = settings;
                }
                Event::MemorySnapshot(snapshot) => self.on_memory_snapshot(snapshot, cx),
                Event::SecurityProgress { done, total } => {
                    self.security.running = Some((done, total));
                }
                Event::SecurityResult(report) => self.on_security_result(report),
                Event::CheckupDone { checkup, reminder } => {
                    self.on_checkup_done(checkup, reminder, cx)
                }
                Event::Failed { what, message } => match what.as_str() {
                    "scan" => {
                        self.cleanup.scanning = None;
                        self.cleanup.notice = Some(Notice::Error(message));
                    }
                    "large" => {
                        self.large.scanning = None;
                        self.large.notice = Some(Notice::Error(message));
                    }
                    "clean" => {
                        self.cleanup.cleaning = None;
                        self.cleanup.notice = Some(Notice::Error(message));
                    }
                    "trash" => {
                        if self.security.trashing.take().is_some() {
                            self.security.notice = Some(Notice::Error(message));
                        } else {
                            self.large.trashing = None;
                            self.large.notice = Some(Notice::Error(message));
                        }
                    }
                    "empty_trash" => {
                        self.cleanup.notice = Some(Notice::Error(if message.is_empty() {
                            EMPTY_TRASH_REFUSED.to_string()
                        } else {
                            message
                        }));
                    }
                    "memory" => self.memory.notice = Some(Notice::Error(message)),
                    "security" => {
                        self.security.running = None;
                        self.security.notice = Some(Notice::Error(message));
                    }
                    "checkup" => {
                        self.checkup.running = false;
                        self.security.running = None;
                        self.checkup.notice = Some(Notice::Error(message));
                    }
                    other => tracing::warn!("core failure ({other}): {message}"),
                },
            }
        }
        self.end_activity_if_idle();
        if refresh_disk && crate::demo::active().is_none() {
            self.refresh_disk();
        }
        self.fade_out(leaving_cleanup, leaving_large, cx);
        self.applies += 1;
        cx.notify();
    }

    /// Rows whose item reached the Trash fade for 140 ms, then leave the list. Under Reduce
    /// Motion they leave at once.
    fn fade_out(&mut self, cleanup: Vec<String>, large: Vec<String>, cx: &mut Context<Self>) {
        if cleanup.is_empty() && large.is_empty() {
            return;
        }
        let Some(delay) = motion::remove_delay(cx.reduce_motion()) else {
            self.finish_fade(&cleanup, &large);
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |m, cx| {
                m.finish_fade(&cleanup, &large);
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_fade(&mut self, cleanup: &[String], large: &[String]) {
        for id in cleanup {
            // A row that was skipped meanwhile was restored, not removed.
            if self.cleanup.leaving.remove(id) {
                self.cleanup.gone.insert(id.clone());
            }
        }
        for rel in large {
            if self.large.leaving.remove(rel) {
                self.large.gone.insert(rel.clone());
            }
        }
    }

    fn refresh_disk(&mut self) {
        let root = self
            .large
            .scan
            .as_ref()
            .map(|s| s.root.clone())
            .or_else(|| self.cleanup.plan.as_ref().map(|p| p.root.clone()));
        if let Some(info) = root.and_then(|root| macos::disk_info(&root)) {
            self.disk = Some(info);
        }
    }

    /// Reads the volume size for the sidebar once, at start. Not called in demo mode.
    pub fn load_disk(&mut self, home: &Path) {
        self.disk = macos::disk_info(home);
    }
}

// ---------- Event batches and the pump ----------

/// Core events drained in one go. Progress events are coalesced: the latest one of each kind
/// wins.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    pub events: Vec<Event>,
    /// How many events went into this batch.
    pub pushed: usize,
}

impl Batch {
    pub fn push(&mut self, event: Event) {
        self.pushed += 1;
        match &event {
            Event::ScanProgress { kind, .. } => {
                let kind = *kind;
                self.events
                    .retain(|e| !matches!(e, Event::ScanProgress { kind: k, .. } if *k == kind));
            }
            Event::CleanProgress { .. } => {
                self.events
                    .retain(|e| !matches!(e, Event::CleanProgress { .. }));
            }
            _ => {}
        }
        self.events.push(event);
    }
}

/// Drains core events on the foreground executor. After each applied batch it waits `period`,
/// so the UI updates at most 1/period times per second however fast the core sends. Ends when
/// the channel closes or the model is gone.
pub fn spawn_pump(
    model: WeakEntity<TidyModel>,
    mut rx: UnboundedReceiver<Event>,
    period: Duration,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        loop {
            let Some(first) = rx.recv().await else { break };
            let mut batch = Batch::default();
            batch.push(first);
            while let Ok(ev) = rx.try_recv() {
                batch.push(ev);
            }
            if model.update(cx, |m, cx| m.apply(batch, cx)).is_err() {
                break;
            }
            cx.background_executor().timer(period).await;
        }
    })
    .detach();
}

/// How often the heartbeat probes the main thread.
const HEARTBEAT_TICK: Duration = Duration::from_millis(500);
/// How often the heartbeat writes its line.
const HEARTBEAT_REPORT: Duration = Duration::from_secs(10);

/// Logs the main thread's worst lag every 10 seconds (`heartbeat max_lag_ms=N`).
///
/// A tokio task stamps a probe every 500 ms and hands it to a foreground task; the lag is the
/// time from the stamp until the main thread runs that task. That is the delay any UI work
/// (input, a repaint) would see. Needs the `Tokio` global.
pub fn start_heartbeat(cx: &mut App) {
    let Some(handle) = cx.try_global::<Tokio>().map(|rt| rt.0.handle().clone()) else {
        tracing::warn!("no runtime: the heartbeat is off");
        return;
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Instant>();
    handle.spawn(async move {
        let mut tick = tokio::time::interval(HEARTBEAT_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if tx.send(Instant::now()).is_err() {
                return;
            }
        }
    });
    cx.spawn(async move |_cx| {
        let mut max_lag = Duration::ZERO;
        let mut since = Instant::now();
        while let Some(stamped) = rx.recv().await {
            max_lag = max_lag.max(stamped.elapsed());
            if since.elapsed() >= HEARTBEAT_REPORT {
                tracing::info!("heartbeat max_lag_ms={}", max_lag.as_millis());
                max_lag = Duration::ZERO;
                since = Instant::now();
            }
        }
    })
    .detach();
}

// ---------- Setup ----------

/// Binds keys, registers app-wide actions, sets the menus and the quit hook.
/// Call after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-1", GoCheckup, Some(ROOT_CONTEXT)),
        KeyBinding::new("cmd-2", GoCleanup, Some(ROOT_CONTEXT)),
        KeyBinding::new("cmd-3", GoLarge, Some(ROOT_CONTEXT)),
        KeyBinding::new("cmd-4", GoMemory, Some(ROOT_CONTEXT)),
        KeyBinding::new("cmd-5", GoSecurity, Some(ROOT_CONTEXT)),
        KeyBinding::new("cmd-r", Rescan, Some(ROOT_CONTEXT)),
        KeyBinding::new("cmd-up", ParentFolder, Some(ROOT_CONTEXT)),
        KeyBinding::new("cmd-backspace", TrashSelection, Some(ROOT_CONTEXT)),
        KeyBinding::new("enter", DialogConfirm, Some(ROOT_CONTEXT)),
        KeyBinding::new("escape", DialogCancel, Some(ROOT_CONTEXT)),
    ]);
    // Components (the tooltip) follow the system appearance too.
    gpui_kit::component::theme::Theme::sync_system_appearance(None, cx);
    crate::theme::sync_component_tokens(cx);
    cx.on_action(|_: &Quit, cx| {
        graceful_shutdown(cx);
        cx.quit();
    });
    cx.on_action(|_: &CloseWindow, cx| close_active_window(cx));
    cx.set_menus(vec![
        Menu {
            name: "Tidy".into(),
            items: vec![
                // Greyed out unless the updater runs (crate::updates::install adds its handler).
                MenuItem::action("Check for Updates\u{2026}", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Quit Tidy", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "Go".into(),
            items: vec![
                MenuItem::action("Check Up", GoCheckup),
                MenuItem::action("Cleanup", GoCleanup),
                MenuItem::action("Large Files", GoLarge),
                MenuItem::action("Memory", GoMemory),
                MenuItem::action("Security", GoSecurity),
                MenuItem::separator(),
                MenuItem::action("Rescan", Rescan),
            ],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![MenuItem::action("Close Window", CloseWindow)],
            disabled: false,
        },
    ]);
    // Quit from the Dock, AppleScript or logout also stops the core cleanly.
    cx.on_app_quit(|cx| {
        graceful_shutdown(cx);
        async {}
    })
    .detach();
}

/// Sets the globals, starts the event pump and the heartbeat. `main` calls this inside `run()`
/// after `init`, then opens the main window.
#[allow(clippy::too_many_arguments)]
pub fn install(
    rt: tokio::runtime::Runtime,
    core: CoreHandle,
    settings: Settings,
    clock: Arc<dyn Clock>,
    cached_large: Option<LargeScan>,
    home: &Path,
    events: UnboundedReceiver<Event>,
    cx: &mut App,
) {
    cx.set_global(Tokio(rt));
    cx.set_global(Core(core));
    cx.set_global(Now(clock));
    cx.set_global(Opener::system());
    cx.set_global(SettingsOpener::system());
    cx.set_global(Terminate(Rc::new(macos::SystemTerminator)));
    cx.set_global(Notify(Rc::new(macos::SystemNotifier)));
    let m = cx.new(|_| {
        let mut m = TidyModel::new(settings);
        if let Some(scan) = cached_large {
            m.load_cached_large(scan);
        }
        m.load_disk(home);
        m
    });
    cx.set_global(Model(m.clone()));
    spawn_pump(m.downgrade(), events, PUMP_PERIOD, cx);
    start_heartbeat(cx);
    // The weekly reminder: look up the permission (never prompting) and reschedule the single
    // notification, and open Check up when the user clicks the reminder.
    crate::views::checkup::watch_reminder_clicks(macos::take_reminder_clicks(), m.downgrade(), cx);
    m.update(cx, |m, cx| m.start_reminder(cx));
}

/// Demo mode's counterpart of `install`: sets the globals the views read, with no core, no event
/// pump and no heartbeat. `send` then returns false.
pub fn install_demo(rt: tokio::runtime::Runtime, settings: Settings, cx: &mut App) {
    cx.set_global(Tokio(rt));
    let m = cx.new(|_| TidyModel::new(settings));
    cx.set_global(Model(m));
}

/// Dock click on a running app: show the main window again.
pub fn reopen(cx: &mut App) {
    open_main_window(cx);
}

/// Closes the key window. The app keeps running; a Dock click reopens it.
pub fn close_active_window(cx: &mut App) {
    if let Some(window) = cx.active_window() {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    }
}

/// Quits like Cmd-Q (the same graceful core shutdown, then `cx.quit()`) when the process
/// receives SIGTERM or SIGINT. Scripts and `kill` can then quit Tidy cleanly without an Apple
/// Event, which would need the Automation permission and could show a consent dialog. Call
/// after `install`, which provides the tokio runtime.
pub fn quit_on_signals(cx: &mut App) {
    use tokio::signal::unix::{SignalKind, signal};

    let Some(handle) = cx.try_global::<Tokio>().map(|rt| rt.0.handle().clone()) else {
        tracing::warn!("no runtime: SIGTERM will not quit Tidy gracefully");
        return;
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
    {
        // Signal streams register with the runtime they are created in.
        let _enter = handle.enter();
        for (kind, name) in [
            (SignalKind::terminate(), "SIGTERM"),
            (SignalKind::interrupt(), "SIGINT"),
        ] {
            match signal(kind) {
                Ok(mut stream) => {
                    let tx = tx.clone();
                    handle.spawn(async move {
                        if stream.recv().await.is_some() {
                            let _ = tx.send(name);
                        }
                    });
                }
                Err(e) => tracing::warn!("cannot listen for {name}: {e}"),
            }
        }
    }
    drop(tx);
    cx.spawn(async move |cx| {
        if let Some(name) = rx.recv().await {
            tracing::info!("quit requested by {name}");
            cx.update(|cx| {
                graceful_shutdown(cx);
                cx.quit();
            });
        }
    })
    .detach();
}

/// Stops the core and waits for it, at most 3 seconds, on the calling (main) thread. GPUI only
/// waits 200 ms for quit futures, so this blocks instead. Runs once; later calls return at once.
pub fn graceful_shutdown(cx: &mut App) {
    if cx.try_global::<ShutdownState>().is_some_and(|s| s.done) {
        return;
    }
    cx.set_global(ShutdownState { done: true });
    let Some(core) = cx.try_global::<Core>().map(|c| c.0.clone()) else {
        return;
    };
    match cx.try_global::<Tokio>() {
        Some(rt) => {
            let started = Instant::now();
            let finished = rt
                .0
                .block_on(async { tokio::time::timeout(SHUTDOWN_TIMEOUT, core.shutdown()).await })
                .is_ok();
            tracing::info!(
                "shutdown finished={finished} took_ms={}",
                started.elapsed().as_millis()
            );
        }
        None => {
            let (tx, _rx) = tokio::sync::oneshot::channel();
            core.send(Command::Shutdown(tx));
        }
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use core::prelude::v1::test; // gpui_kit::* also exports `test`

    #[test]
    fn demo_forces_background_mode() {
        assert!(!background_from(None, None));
        assert!(!background_from(Some("0"), Some("")));
        assert!(background_from(Some("1"), None));
        assert!(background_from(None, Some("cleanup")));
        assert!(background_from(Some("0"), Some("large")));
    }

    #[test]
    fn demo_and_background_agree_on_every_tidy_demo_value() {
        for v in [
            "",
            " ",
            "\t",
            "0",
            "false",
            "off",
            "OFF",
            "no",
            "cleanup",
            " cleanup ",
            "large",
            "typo",
        ] {
            let demo_on = crate::demo::scenario_from(Some(v)).is_some();
            assert_eq!(demo_on, background_from(None, Some(v)), "TIDY_DEMO={v:?}");
        }
        assert_eq!(crate::demo::scenario_from(None), None);
        assert_eq!(crate::demo::scenario_from(Some(" ")), None);
        assert_eq!(crate::demo::scenario_from(Some("false")), None);
        assert_eq!(
            crate::demo::scenario_from(Some(" Large-Folders ")),
            Some(crate::demo::Scenario::LargeFolders)
        );
        assert_eq!(
            crate::demo::scenario_from(Some("typo")),
            Some(crate::demo::Scenario::Cleanup)
        );
    }

    #[test]
    fn progress_events_coalesce() {
        let mut b = Batch::default();
        let progress = |kind, files| Event::ScanProgress {
            kind,
            files,
            bytes: 0,
            current: None,
        };
        b.push(progress(ScanKind::Cleanup, 1));
        b.push(progress(ScanKind::Large, 5));
        b.push(progress(ScanKind::Cleanup, 9));
        b.push(Event::CleanProgress { done: 1, total: 4 });
        b.push(Event::CleanProgress { done: 2, total: 4 });
        assert_eq!(b.pushed, 5);
        assert_eq!(b.events.len(), 3);
        assert!(b.events.contains(&progress(ScanKind::Cleanup, 9)));
        assert!(b.events.contains(&progress(ScanKind::Large, 5)));
        assert!(
            b.events
                .contains(&Event::CleanProgress { done: 2, total: 4 })
        );
    }

    #[test]
    fn other_events_keep_their_order() {
        let mut b = Batch::default();
        b.push(Event::ItemTrashed {
            id: "a".into(),
            trash_path: None,
        });
        b.push(Event::ItemSkipped {
            id: "b".into(),
            reason: "x".into(),
        });
        assert!(matches!(b.events[0], Event::ItemTrashed { .. }));
        assert!(matches!(b.events[1], Event::ItemSkipped { .. }));
    }
}
