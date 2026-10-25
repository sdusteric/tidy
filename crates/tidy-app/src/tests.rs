//! Headless UI tests on GPUI's TestPlatform. No real window is shown and the app never takes
//! focus. The core is a fake: a channel the test reads. The app only sends commands, so there is
//! no trasher here at all, and `Opener` is a recorder. Run with `--test-threads=1`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core::prelude::v1::test; // the gpui_kit glob also exports a `test` macro
use gpui_kit::*;
use tidy_core::api::{Command, CoreHandle, Event, ScanKind};
use tidy_core::env::FixedClock;
use tidy_core::model::{
    Action, AppMemory, CheckId, CheckResult, Checkup, CheckupCleanup, CheckupMemory,
    CheckupSecurity, ChromeExtension, Class, FolderNode, Group, HistoryEntry, HistoryItem, Item,
    ItemResult, LargeItem, LargeKind, LargeScan, LaunchKind, LoginItem, MB, Mark, MemoryKind,
    MemorySnapshot, Plan, Pressure, RunningSet, Scope, SecurityReport, SettingsLink, Signer,
    Source, Status, SwapUsage, TrashSummary, TrasherKind, UseSource,
};
use tidy_core::reminder::ReminderDate;
use tidy_core::settings::Settings;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{
    Core, EMPTY_TRASH_REFUSED, LargeFilter, LargeMode, MEMORY_PERIOD, Model, Notice, Notify,
    OpenKind, Opener, PUMP_PERIOD, QUIT_WAIT, RUNNING_PERIOD, STALE_SCAN_SECS, Screen,
    SettingsOpener, Terminate, Tokio, graceful_shutdown, spawn_pump,
};
use crate::macos::Permission;
use crate::macos::fakes::{FakeNotifier, FakeTerminator};
use crate::views::cleanup::{CleanRow, MenuEntry, cleanup_rows};
use crate::views::dialogs::{APP_DATA_NOTE, CLONE_NOTE, DialogKind};
use crate::views::main_window::{TidyView, open_main_window};

const NOW: u64 = 1_792_764_131;
const DAY: u64 = 86_400;
const GB: u64 = 1_000 * MB;

type Opened = Rc<RefCell<Vec<(OpenKind, PathBuf)>>>;

struct Harness {
    commands: UnboundedReceiver<Command>,
    events: UnboundedSender<Event>,
    opened: Opened,
    /// The System Settings pages the app was asked to open.
    settings_opened: Rc<RefCell<Vec<SettingsLink>>>,
    /// Fakes: no test ever signals a process or touches the notification center.
    terminator: Rc<FakeTerminator>,
    notifier: Rc<FakeNotifier>,
    clock: Arc<FixedClock>,
}

impl Harness {
    fn drain(&mut self) -> Vec<Command> {
        let mut out = Vec::new();
        while let Ok(c) = self.commands.try_recv() {
            out.push(c);
        }
        out
    }
}

fn setup_with(cx: &mut TestAppContext, settings: Settings) -> Harness {
    let (tx, commands) = unbounded_channel();
    let (events, event_rx) = unbounded_channel();
    let opened: Opened = Rc::new(RefCell::new(Vec::new()));
    let rec = opened.clone();
    let settings_opened: Rc<RefCell<Vec<SettingsLink>>> = Rc::new(RefCell::new(Vec::new()));
    let settings_rec = settings_opened.clone();
    let terminator = Rc::new(FakeTerminator::default());
    let notifier = Rc::new(FakeNotifier::default());
    let (term_global, notify_global) = (terminator.clone(), notifier.clone());
    let clock = Arc::new(FixedClock::new(NOW));
    let now_clock = clock.clone();
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::init(cx);
        cx.set_global(Core(CoreHandle::new(tx)));
        cx.set_global(crate::app::Now(now_clock));
        cx.set_global(Opener(Box::new(move |kind, path: &Path, _cx| {
            rec.borrow_mut().push((kind, path.to_path_buf()))
        })));
        cx.set_global(SettingsOpener(Box::new(move |link, _cx| {
            settings_rec.borrow_mut().push(link)
        })));
        cx.set_global(Terminate(term_global));
        cx.set_global(Notify(notify_global));
        let m = cx.new(|_| crate::app::TidyModel::new(settings));
        cx.set_global(Model(m.clone()));
        spawn_pump(m.downgrade(), event_rx, PUMP_PERIOD, cx);
    });
    Harness {
        commands,
        events,
        opened,
        settings_opened,
        terminator,
        notifier,
        clock,
    }
}

fn setup(cx: &mut TestAppContext) -> Harness {
    setup_with(cx, Settings::default())
}

/// Opens the real main window (through gpui_kit::open_window, so it has a Root).
fn open_main(cx: &mut TestAppContext) -> (Entity<TidyView>, &mut VisualTestContext) {
    let handle = cx.update(open_main_window).expect("main window");
    let view = cx
        .update(|cx| {
            handle.update(cx, |_, window, cx| {
                window
                    .root::<gpui_kit::base::Root>()
                    .flatten()
                    .and_then(|r| r.read(cx).view().clone().downcast::<TidyView>().ok())
            })
        })
        .unwrap()
        .expect("TidyView root");
    let vcx = VisualTestContext::from_window(handle, cx).into_mut();
    vcx.run_until_parked();
    (view, vcx)
}

fn model(cx: &mut VisualTestContext) -> Entity<crate::app::TidyModel> {
    cx.update(|_, cx| crate::app::model(cx))
}

/// Sends events through the pump and lets it apply them.
fn push(h: &Harness, cx: &mut VisualTestContext, events: Vec<Event>) {
    for e in events {
        h.events.send(e).unwrap();
    }
    cx.executor().advance_clock(PUMP_PERIOD);
    cx.run_until_parked();
}

/// Like [`push`] but without moving the clock, for tests that measure a fade.
fn push_now(h: &Harness, cx: &mut VisualTestContext, events: Vec<Event>) {
    for e in events {
        h.events.send(e).unwrap();
    }
    cx.run_until_parked();
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    cx.debug_bounds(leak(selector.to_string()))
}

fn has(cx: &mut VisualTestContext, selector: &str) -> bool {
    bounds(cx, selector).is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let b = bounds(cx, selector).unwrap_or_else(|| panic!("no element {selector}"));
    cx.simulate_click(b.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Moves the pointer onto an element, so its row shows the hover actions.
fn hover(cx: &mut VisualTestContext, selector: &str) {
    let b = bounds(cx, selector).unwrap_or_else(|| panic!("no element {selector}"));
    // Left of center: away from the trailing action buttons.
    let pos = point(b.origin.x + px(200.), b.center().y);
    cx.simulate_mouse_move(pos, None, Modifiers::none());
    cx.run_until_parked();
}

fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    cx.simulate_keystrokes(keystrokes);
    cx.run_until_parked();
}

fn advance(cx: &mut VisualTestContext, ms: u64) {
    cx.executor().advance_clock(Duration::from_millis(ms));
    cx.run_until_parked();
}

// ---------- fixtures ----------

fn item(id: &str, group: Group, class: Class, bytes: u64, blocked: Option<&str>) -> Item {
    let name = match id {
        "a" => "Spotify".to_string(),
        "b" => "Hugging Face".to_string(),
        "chrome" => "Google Chrome".to_string(),
        other => other.to_string(),
    };
    Item {
        id: format!("r:{id}"),
        group,
        class,
        ticked: class == Class::Safe && blocked.is_none(),
        blocked: blocked.map(str::to_string),
        name,
        reason: format!("reason of {id}"),
        path: PathBuf::from("/fake").join(id),
        bytes_alloc: bytes,
        bytes_logical: bytes,
        newest_mtime: None,
        members: vec![],
    }
}

/// Safe: a (3 GB), log (500 MB). Blocked Safe: chrome (1 GB). Review: b (2 GB). Report only: vm
/// (9 GB, in the collapsed AI models group).
fn plan() -> Plan {
    let mut plan = Plan::empty("/fake", NOW - 120);
    plan.trash = TrashSummary {
        items: 214,
        bytes: 2_100 * MB,
    };
    plan.items = vec![
        item("a", Group::Caches, Class::Safe, 3 * GB, None),
        item("b", Group::Caches, Class::Review, 2 * GB, None),
        item(
            "chrome",
            Group::Caches,
            Class::Safe,
            GB,
            Some("Quit Google Chrome first"),
        ),
        item("log", Group::Logs, Class::Safe, 500 * MB, None),
        item("vm", Group::AiModels, Class::ReportOnly, 9 * GB, None),
    ];
    plan.sort();
    plan
}

fn large_item(rel: &str, mb: u64, days_ago: u64, app_data: bool, kind: LargeKind) -> LargeItem {
    LargeItem {
        path: PathBuf::from("/fake").join(rel),
        rel: rel.to_string(),
        name: rel.rsplit('/').next().unwrap().to_string(),
        kind,
        bytes_alloc: mb * MB,
        bytes_logical: mb * MB,
        last_used: Some(NOW - days_ago * DAY),
        used_source: UseSource::FileTimes,
        app_data,
    }
}

fn node(name: &str, rel: &str, mb: u64, kind: LargeKind, children: Vec<FolderNode>) -> FolderNode {
    FolderNode {
        name: name.into(),
        rel: rel.into(),
        kind: Some(kind),
        bytes_alloc: mb * MB,
        bytes_logical: mb * MB,
        files: 10,
        children,
        rest_count: 0,
        rest_alloc: 0,
    }
}

const BIG: &str = "Library/Application Support/Models/big.bin";

fn large_scan() -> LargeScan {
    let mut scan = LargeScan::empty("/fake", NOW - 2 * 3_600, 50 * MB);
    scan.items = vec![
        large_item("Movies/trip.mov", 900, 5, false, LargeKind::File),
        large_item(BIG, 700, 400, true, LargeKind::File),
        large_item("Apps/Big.app", 300, 200, false, LargeKind::Package),
        large_item("Downloads/old.zip", 120, 3, false, LargeKind::File),
    ];
    let support = node(
        "Application Support",
        "Library/Application Support",
        700,
        LargeKind::Folder,
        vec![],
    );
    let library = node("Library", "Library", 700, LargeKind::Folder, vec![support]);
    scan.tree = node(
        "Home",
        "",
        2_000,
        LargeKind::Folder,
        vec![
            node("Movies", "Movies", 900, LargeKind::Folder, vec![]),
            library,
            node("Apps", "Apps", 300, LargeKind::Folder, vec![]),
            node("Downloads", "Downloads", 120, LargeKind::Folder, vec![]),
        ],
    );
    scan
}

fn entry(source: Source, action: Action, items: Vec<HistoryItem>, bytes: u64) -> HistoryEntry {
    HistoryEntry {
        id: "2026-10-23T14:02:11Z-1".into(),
        at: NOW,
        action,
        source,
        trasher: TrasherKind::Dir,
        items,
        trashed_bytes: bytes,
        message: None,
    }
}

fn history_item(path: &str, result: ItemResult, reason: Option<&str>) -> HistoryItem {
    HistoryItem {
        path: PathBuf::from(path),
        trash_path: None,
        bytes: 1_000,
        rule: "r".into(),
        result,
        reason: reason.map(str::to_string),
        trash_id: None,
    }
}

/// Opens the window with the plan loaded.
fn open_with_plan(cx: &mut TestAppContext) -> (Harness, Entity<TidyView>, &mut VisualTestContext) {
    let mut h = setup(cx);
    let (view, cx) = open_main(cx);
    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    h.drain();
    (h, view, cx)
}

/// Opens the window on Large files with the scan loaded.
fn open_with_scan(cx: &mut TestAppContext) -> (Harness, Entity<TidyView>, &mut VisualTestContext) {
    let mut h = setup(cx);
    let (view, cx) = open_main(cx);
    push(&h, cx, vec![Event::LargeResult(large_scan())]);
    keys(cx, "cmd-3");
    h.drain();
    (h, view, cx)
}

fn dialog_of(
    view: &Entity<TidyView>,
    cx: &mut VisualTestContext,
) -> Option<crate::views::dialogs::DialogSpec> {
    view.read_with(cx, |v, _| v.dialog.clone())
}

fn cleanup_ids(cx: &mut VisualTestContext) -> Vec<String> {
    model(cx).read_with(cx, |m, _| {
        cleanup_rows(&m.cleanup)
            .into_iter()
            .filter_map(|r| match r {
                CleanRow::Item(i) => Some(i.id),
                _ => None,
            })
            .collect()
    })
}

// ---------- Cleanup: empty state, scanning, rows ----------

#[::gpui::test]
fn cleanup_starts_with_an_empty_state_and_scan_sends_the_command(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (view, cx) = open_main(cx);
    assert!(
        has(cx, "empty-state"),
        "no automatic scan, so the empty state shows"
    );
    assert!(!has(cx, "group-Caches"));
    assert!(
        h.drain().is_empty(),
        "opening the window sends nothing (C3)"
    );

    click(cx, "empty-action");
    let cmds = h.drain();
    assert!(matches!(cmds[0], Command::ScanCleanup), "{cmds:?}");
    // The first explicit scan is remembered, so later visits may scan on their own.
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Command::UpdateSettings(s) if s.cleanup_scanned_once)),
        "{cmds:?}"
    );
    assert_eq!(model(cx).read_with(cx, |m, _| m.cleanup.scanning), Some(0));
    assert!(
        has(cx, "scanning"),
        "the toolbar shows the spinner and the count"
    );

    // A second click while scanning sends nothing.
    click(cx, "empty-action");
    assert!(h.drain().is_empty());

    push(
        &h,
        cx,
        vec![Event::ScanProgress {
            kind: ScanKind::Cleanup,
            files: 41_200,
            bytes: 0,
            current: None,
        }],
    );
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.cleanup.scanning),
        Some(41_200)
    );
    let _ = view;

    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    assert_eq!(model(cx).read_with(cx, |m, _| m.cleanup.scanning), None);
    assert!(has(cx, "group-Caches") && !has(cx, "empty-state"));
}

#[::gpui::test]
fn an_empty_scan_shows_the_nothing_to_clean_state(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    push(&h, cx, vec![Event::CleanupPlan(Plan::empty("/fake", NOW))]);
    assert!(has(cx, "empty-state"));
    assert!(!has(cx, "clean-button"), "no primary button without items");
    click(cx, "empty-action");
    assert!(matches!(h.drain()[0], Command::ScanCleanup));
}

#[::gpui::test]
fn rows_groups_and_totals(cx: &mut TestAppContext) {
    let (_h, view, cx) = open_with_plan(cx);
    // Groups with items show; AI models starts collapsed (C5), so its row is not drawn.
    for g in ["Caches", "Logs", "AiModels"] {
        assert!(has(cx, &format!("group-{g}")), "group {g}");
    }
    assert!(!has(cx, "group-Developer"), "empty groups are hidden");
    for id in ["r:a", "r:b", "r:chrome", "r:log"] {
        assert!(has(cx, &format!("clean-row:{id}")), "row {id}");
    }
    assert!(!has(cx, "clean-row:r:vm"), "collapsed group");

    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.button_label(), "Clean 3.5 GB");
    assert_eq!(snap.figure(), "3.5 GB");
    assert_eq!(snap.status_left(), "2 selected, 3.5 GB");
    assert_eq!(
        snap.summary_line(NOW),
        "2 GB more to review \u{b7} Scanned 2 min ago"
    );
    assert_eq!(
        snap.trash_line().as_deref(),
        Some("In the Trash from Tidy: 2.1 GB")
    );
    assert!(snap.can_clean());
    assert!(has(cx, "figure") && has(cx, "summary-line") && has(cx, "status-bar"));

    // Expanding the collapsed group shows its Report only row, which has no checkbox.
    click(cx, "group-AiModels");
    assert!(has(cx, "clean-row:r:vm"));
    assert!(
        !has(cx, "clean-check:r:vm"),
        "Report only rows have no checkbox"
    );
    click(cx, "group-AiModels");
    assert!(!has(cx, "clean-row:r:vm"));
}

#[::gpui::test]
fn ticks_update_the_totals_and_the_group_checkbox(cx: &mut TestAppContext) {
    use crate::app::Tri;
    let (mut h, view, cx) = open_with_plan(cx);
    let tri =
        |cx: &mut VisualTestContext, g| model(cx).read_with(cx, |m, _| m.cleanup.group_tri(g));
    assert_eq!(tri(cx, Group::Caches), Some(Tri::Mixed));
    assert_eq!(tri(cx, Group::Logs), Some(Tri::On));
    assert_eq!(
        tri(cx, Group::AiModels),
        None,
        "nothing to tick in a Report only group"
    );

    click(cx, "clean-check:r:a");
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.button_label(), "Clean 500 MB");
    assert_eq!(snap.status_left(), "1 selected, 500 MB");
    assert_eq!(tri(cx, Group::Caches), Some(Tri::Off));

    // The blocked row cannot be ticked.
    click(cx, "clean-check:r:chrome");
    assert!(!model(cx).read_with(cx, |m, _| m.cleanup.ticked.contains("r:chrome")));

    click(cx, "clean-check:r:log");
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert!(
        snap.status_left().ends_with(" items, none selected"),
        "{}",
        snap.status_left()
    );
    assert_eq!(snap.figure(), "Nothing selected");
    assert_eq!(snap.button_label(), "Clean");
    assert!(!snap.can_clean(), "the button is disabled at zero");
    click(cx, "clean-button");
    assert!(h.drain().is_empty(), "a disabled button sends nothing");

    // The group checkbox ticks every tickable item, Review included.
    click(cx, "group-check-Caches");
    assert_eq!(tri(cx, Group::Caches), Some(Tri::On));
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(
        snap.button_label(),
        "Clean selected",
        "a Review item is ticked"
    );
    assert_eq!(snap.status_left(), "2 selected, 5 GB");
    click(cx, "group-check-Caches");
    assert_eq!(tri(cx, Group::Caches), Some(Tri::Off));

    // A click on the row itself ticks it too.
    click(cx, "clean-row:r:log");
    assert!(model(cx).read_with(cx, |m, _| m.cleanup.ticked.contains("r:log")));
}

#[::gpui::test]
fn blocked_rows_say_why_and_unblock_when_the_app_quits(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    let meta = |cx: &mut VisualTestContext| {
        model(cx).read_with(cx, |m, _| {
            cleanup_rows(&m.cleanup)
                .into_iter()
                .find_map(|r| match r {
                    CleanRow::Item(i) if i.id == "r:chrome" => Some((i.meta, i.warn, i.enabled)),
                    _ => None,
                })
                .unwrap()
        })
    };
    assert_eq!(
        meta(cx),
        ("Quit Google Chrome first".to_string(), true, false)
    );

    // While Cleanup is visible and something is blocked, the app asks the core every 3 s.
    advance(cx, RUNNING_PERIOD.as_millis() as u64 + 10);
    assert!(
        h.drain().iter().any(|c| matches!(c, Command::CheckRunning)),
        "CheckRunning every 3 seconds"
    );
    push(
        &h,
        cx,
        vec![Event::RunningChanged(RunningSet::new([
            "com.google.Chrome",
        ]))],
    );
    assert!(!meta(cx).2, "Chrome still runs");

    push(&h, cx, vec![Event::RunningChanged(RunningSet::default())]);
    assert_eq!(meta(cx), ("reason of chrome".to_string(), false, true));
    // A Safe item is ticked again when its app quits.
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.status_left(), "3 selected, 4.5 GB");

    // And blocked again when the app starts again.
    push(
        &h,
        cx,
        vec![Event::RunningChanged(RunningSet::new([
            "com.google.Chrome",
        ]))],
    );
    assert_eq!(meta(cx).0, "Quit Google Chrome first");
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.status_left(), "2 selected, 3.5 GB");

    // Off the Cleanup screen the check stops.
    keys(cx, "cmd-3");
    h.drain();
    advance(cx, RUNNING_PERIOD.as_millis() as u64 + 10);
    assert!(!h.drain().iter().any(|c| matches!(c, Command::CheckRunning)));
}

#[::gpui::test]
fn a_new_plan_uses_the_latest_process_list(cx: &mut TestAppContext) {
    let (h, view, cx) = open_with_plan(cx);
    // Chrome quits. The core reports it once and will not repeat an unchanged set.
    push(&h, cx, vec![Event::RunningChanged(RunningSet::default())]);
    // A rescan whose snapshot was taken while Chrome still ran.
    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    let enabled = model(cx).read_with(cx, |m, _| {
        cleanup_rows(&m.cleanup)
            .into_iter()
            .any(|r| matches!(r, CleanRow::Item(i) if i.id == "r:chrome" && i.enabled))
    });
    assert!(enabled, "Chrome is not running, so its item is not blocked");
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.status_left(), "3 selected, 4.5 GB");
}

// ---------- Cleanup: clean, dialog, fade ----------

#[::gpui::test]
fn safe_items_clean_at_once_without_a_dialog(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    click(cx, "clean-button");
    assert!(
        dialog_of(&view, cx).is_none(),
        "everything goes to the Trash, so no dialog (C7)"
    );
    let cmds = h.drain();
    assert_eq!(cmds.len(), 1, "{cmds:?}");
    match &cmds[0] {
        Command::Clean { item_ids } => assert_eq!(item_ids, &["r:a", "r:log"]),
        other => panic!("{other:?}"),
    }
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.button_label(), "Cleaning 0 of 2");
    assert!(!snap.can_clean());
    push(&h, cx, vec![Event::CleanProgress { done: 1, total: 2 }]);
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.button_label(), "Cleaning 1 of 2");
    // While cleaning, ticks are frozen.
    click(cx, "clean-check:r:a");
    assert!(model(cx).read_with(cx, |m, _| m.cleanup.ticked.contains("r:a")));
}

#[::gpui::test]
fn review_items_ask_first(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    click(cx, "clean-check:r:b");
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(snap.button_label(), "Clean selected");

    click(cx, "clean-button");
    assert!(
        h.drain().is_empty(),
        "nothing is sent before the dialog is confirmed"
    );
    let spec = dialog_of(&view, cx).expect("a confirm dialog");
    assert_eq!(spec.kind, DialogKind::CleanReview);
    assert_eq!(spec.title, "Move these to the Trash?");
    assert_eq!(
        spec.body.as_deref(),
        Some("You can put them back from the Trash until you empty it.")
    );
    assert_eq!(
        spec.rows,
        vec![("Hugging Face".to_string(), "2 GB".to_string())]
    );
    assert_eq!(spec.confirm, "Move to Trash");
    assert!(has(cx, "dialog") && has(cx, "dialog-title") && has(cx, "dialog-list"));
    assert!(has(cx, "dialog-cancel") && has(cx, "dialog-confirm"));

    // Cancel closes it and sends nothing.
    click(cx, "dialog-cancel");
    assert!(dialog_of(&view, cx).is_none() && h.drain().is_empty());

    // Escape cancels too.
    click(cx, "clean-button");
    keys(cx, "escape");
    assert!(dialog_of(&view, cx).is_none() && h.drain().is_empty());

    // Move to Trash sends Clean for every ticked item, in list order.
    click(cx, "clean-button");
    click(cx, "dialog-confirm");
    assert!(dialog_of(&view, cx).is_none());
    let cmds = h.drain();
    match &cmds[..] {
        [Command::Clean { item_ids }] => assert_eq!(item_ids, &["r:a", "r:b", "r:log"]),
        other => panic!("{other:?}"),
    }
}

#[::gpui::test]
fn return_confirms_the_dialog(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    click(cx, "clean-check:r:b");
    click(cx, "clean-button");
    assert!(dialog_of(&view, cx).is_some());
    keys(cx, "enter");
    assert!(dialog_of(&view, cx).is_none());
    assert!(matches!(h.drain()[..], [Command::Clean { .. }]));
}

#[::gpui::test]
fn rows_fade_for_140_ms_after_the_trash_and_come_back_when_skipped(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = open_with_plan(cx);
    click(cx, "clean-button");
    assert!(matches!(h.drain()[..], [Command::Clean { .. }]));

    // The command goes out first; the row fades only when its item reaches the Trash.
    push_now(
        &h,
        cx,
        vec![
            Event::ItemTrashed {
                id: "r:a".into(),
                trash_path: Some(PathBuf::from("/trash/a")),
            },
            Event::ItemSkipped {
                id: "r:log".into(),
                reason: "Google Chrome opened, skipped".into(),
            },
        ],
    );
    let leaving = model(cx).read_with(cx, |m, _| m.cleanup.leaving.clone());
    assert!(leaving.contains("r:a") && !leaving.contains("r:log"));
    assert!(has(cx, "clean-row:r:a"), "still drawn while it fades");
    advance(cx, 100);
    assert!(has(cx, "clean-row:r:a"), "100 ms is inside the fade");
    advance(cx, 60);
    assert!(!has(cx, "clean-row:r:a"), "gone after 140 ms");
    assert!(!cleanup_ids(cx).contains(&"r:a".to_string()));

    // The skipped row stays, unticked, with the reason in the warning color.
    assert!(has(cx, "clean-row:r:log"));
    let (meta, warn, ticked) = model(cx).read_with(cx, |m, _| {
        let row = cleanup_rows(&m.cleanup)
            .into_iter()
            .find_map(|r| match r {
                CleanRow::Item(i) if i.id == "r:log" => Some(i),
                _ => None,
            })
            .unwrap();
        (row.meta, row.warn, row.ticked)
    });
    assert_eq!(meta, "Google Chrome opened, skipped");
    assert!(warn && !ticked);
}

#[::gpui::test]
fn rows_leave_at_once_under_reduce_motion(cx: &mut TestAppContext) {
    let (_h, _view, cx) = open_with_plan(cx);
    cx.update(|_, cx| cx.set_reduce_motion(true));
    click(cx, "clean-button");
    push(
        &_h,
        cx,
        vec![Event::ItemTrashed {
            id: "r:a".into(),
            trash_path: None,
        }],
    );
    assert!(!has(cx, "clean-row:r:a"));
    assert!(model(cx).read_with(cx, |m, _| m.cleanup.leaving.is_empty()));
}

#[::gpui::test]
fn done_shows_the_notice_and_the_trash_total(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    click(cx, "clean-button");
    h.drain();
    let done = entry(
        Source::Cleanup,
        Action::Clean,
        vec![
            history_item("/fake/a", ItemResult::Trashed, None),
            history_item("/fake/log", ItemResult::Trashed, None),
        ],
        3_500 * MB,
    );
    push(
        &h,
        cx,
        vec![
            Event::ItemTrashed {
                id: "r:a".into(),
                trash_path: None,
            },
            Event::ItemTrashed {
                id: "r:log".into(),
                trash_path: None,
            },
            Event::CleanDone(done),
        ],
    );
    advance(cx, 200);
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.cleanup.notice.clone()),
        Some(Notice::Moved { bytes: 3_500 * MB })
    );
    assert!(has(cx, "notice") && has(cx, "notice-empty-trash") && has(cx, "notice-history"));
    let snap = view.read_with(cx, |v, cx| v.cleanup_snapshot(cx));
    assert_eq!(
        snap.trash_line().as_deref(),
        Some("In the Trash from Tidy: 5.6 GB")
    );
    assert_eq!(snap.button_label(), "Clean");
    assert_eq!(snap.figure(), "Nothing selected");
    // A safe total of zero leaves the sidebar row without a figure, not "0 KB".
    assert!(!has(cx, "sidebar-cleanup-figure"));

    // The notice stays until dismissed, then fades out over 200 ms.
    advance(cx, 5_000);
    assert!(has(cx, "notice"));
    click(cx, "notice-dismiss");
    assert!(has(cx, "notice"), "still fading");
    advance(cx, 100);
    assert!(has(cx, "notice"));
    advance(cx, 110);
    assert!(!has(cx, "notice"));

    // The next scan clears it as well.
    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    assert!(!has(cx, "notice"));
}

#[::gpui::test]
fn the_notice_leaves_at_once_under_reduce_motion(cx: &mut TestAppContext) {
    let (h, _view, cx) = open_with_plan(cx);
    cx.update(|_, cx| cx.set_reduce_motion(true));
    push(
        &h,
        cx,
        vec![Event::CleanDone(entry(
            Source::Cleanup,
            Action::Clean,
            vec![],
            GB,
        ))],
    );
    assert!(has(cx, "notice"));
    click(cx, "notice-dismiss");
    assert!(!has(cx, "notice"));
}

#[::gpui::test]
fn the_notice_buttons_open_the_empty_trash_dialog_and_history(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    push(
        &h,
        cx,
        vec![Event::CleanDone(entry(
            Source::Cleanup,
            Action::Clean,
            vec![],
            6 * GB,
        ))],
    );
    click(cx, "notice-empty-trash");
    assert!(dialog_of(&view, cx).is_some_and(|d| d.kind == DialogKind::EmptyTrash));
    keys(cx, "escape");
    click(cx, "notice-history");
    assert!(view.read_with(cx, |v, _| v.history_open));
    assert!(
        h.drain()
            .iter()
            .any(|c| matches!(c, Command::LoadHistory { .. }))
    );
}

// ---------- Cleanup: Empty Trash, History ----------

#[::gpui::test]
fn empty_trash_asks_then_sends_and_reports_a_refusal(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    assert!(has(cx, "empty-trash") && has(cx, "trash-total"));
    click(cx, "empty-trash");
    let spec = dialog_of(&view, cx).expect("dialog");
    assert_eq!(spec.title, "Empty the Trash?");
    assert_eq!(
        spec.body.as_deref(),
        Some(
            "Permanently delete the 214 items Tidy moved to the Trash (2.1 GB)? This can't be undone."
        )
    );
    assert!(spec.destructive);
    assert_eq!(spec.confirm, "Empty Trash");
    assert!(h.drain().is_empty());
    click(cx, "dialog-confirm");
    assert!(matches!(h.drain()[..], [Command::EmptyTidyTrash]));

    // macOS refuses: the fixed sentence shows and the totals stay.
    let mut refused = entry(Source::Cleanup, Action::EmptyTrash, vec![], 0);
    refused.message = Some("Operation not permitted".into());
    push(&h, cx, vec![Event::EmptyTrashDone(refused)]);
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.cleanup.notice.clone()),
        Some(Notice::Error(EMPTY_TRASH_REFUSED.to_string()))
    );
    assert_eq!(model(cx).read_with(cx, |m, _| m.cleanup.trash.items), 214);
    assert!(has(cx, "notice"));

    // Success clears the Trash total and the Trash row of the status bar.
    let ok = entry(
        Source::Cleanup,
        Action::EmptyTrash,
        vec![history_item("/trash/x", ItemResult::Deleted, None)],
        2_100 * MB,
    );
    push(&h, cx, vec![Event::EmptyTrashDone(ok)]);
    assert_eq!(model(cx).read_with(cx, |m, _| m.cleanup.trash.items), 0);
    assert!(!has(cx, "empty-trash"));
}

#[::gpui::test]
fn history_sheet_loads_lists_and_expands(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    click(cx, "history-button");
    let cmds = h.drain();
    let reply = match cmds.into_iter().next() {
        Some(Command::LoadHistory { reply }) => reply,
        other => panic!("{other:?}"),
    };
    assert!(view.read_with(cx, |v, _| v.history_open));
    assert!(has(cx, "history"));
    let dir = std::env::temp_dir().join(format!("tidy-ui-history-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let present = dir.join("still-here");
    std::fs::write(&present, b"x").unwrap();
    let skipped = history_item(
        present.to_str().unwrap(),
        ItemResult::Skipped,
        Some("Google Chrome is running"),
    );
    let e = entry(
        Source::Cleanup,
        Action::Clean,
        vec![history_item("/fake/a", ItemResult::Trashed, None), skipped],
        3 * GB,
    );
    reply.send(vec![e]).unwrap();
    cx.run_until_parked();
    assert!(has(cx, "history-entry-0"));
    assert!(!has(cx, "history-item-0-0"), "items open on a click");
    click(cx, "history-entry-0");
    assert!(has(cx, "history-item-0-0") && has(cx, "history-item-0-1"));
    // Reveal is offered only for the item still at its original path.
    assert!(!has(cx, "history-reveal-0-0"));
    assert!(has(cx, "history-reveal-0-1"));
    click(cx, "history-reveal-0-1");
    assert_eq!(
        h.opened.borrow().as_slice(),
        &[(OpenKind::Reveal, present.clone())]
    );
    keys(cx, "escape");
    assert!(!view.read_with(cx, |v, _| v.history_open));
    let _ = std::fs::remove_file(&present);
    let _ = std::fs::remove_dir(&dir);
}

#[::gpui::test]
fn history_with_no_entries_says_so(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    click(cx, "history-button");
    if let Some(Command::LoadHistory { reply }) = h.drain().into_iter().next() {
        reply.send(Vec::new()).unwrap();
    }
    cx.run_until_parked();
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.history.entries.as_ref().map(Vec::len)),
        Some(0)
    );
    click(cx, "history-close");
    assert!(!view.read_with(cx, |v, _| v.history_open));
}

// ---------- Cleanup: row menu and Reveal ----------

#[::gpui::test]
fn the_more_menu_has_never_show_and_copy_path(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    hover(cx, "clean-row:r:a");
    assert!(
        has(cx, "clean-reveal:r:a") && has(cx, "clean-more:r:a"),
        "hover shows the actions"
    );
    assert!(
        !has(cx, "clean-reveal:r:log"),
        "only the hovered row shows them"
    );

    click(cx, "clean-more:r:a");
    let entries = view.read_with(cx, |v, _| v.menu.as_ref().map(|m| m.entries.clone()));
    assert_eq!(
        entries,
        Some(vec![
            MenuEntry::ShowDetails,
            MenuEntry::NeverShow,
            MenuEntry::CopyPath
        ])
    );
    assert!(has(cx, "row-menu") && has(cx, "menu-never-show") && has(cx, "menu-copy-path"));

    click(cx, "menu-copy-path");
    assert!(view.read_with(cx, |v, _| v.menu.is_none()));
    let clip = cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip.as_deref(), Some("/fake/a"));
    assert!(h.drain().is_empty());

    hover(cx, "clean-row:r:a");
    click(cx, "clean-more:r:a");
    click(cx, "menu-never-show");
    let cmds = h.drain();
    assert!(
        matches!(&cmds[..], [Command::Ignore { path }] if path == Path::new("/fake/a")),
        "{cmds:?}"
    );
    assert!(!has(cx, "clean-row:r:a"), "the row leaves the list");

    // Escape closes an open menu without doing anything.
    hover(cx, "clean-row:r:log");
    click(cx, "clean-more:r:log");
    assert!(view.read_with(cx, |v, _| v.menu.is_some()));
    keys(cx, "escape");
    assert!(view.read_with(cx, |v, _| v.menu.is_none()) && h.drain().is_empty());
}

// ---------- Trust surfaces (design 17) ----------

#[::gpui::test]
fn every_row_opens_its_details_safe_ones_included(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    hover(cx, "clean-row:r:a");
    assert!(
        has(cx, "clean-details:r:a"),
        "the Details button is a hover action"
    );
    click(cx, "clean-details:r:a");
    assert_eq!(
        view.read_with(cx, |v, _| v.details.as_ref().map(|d| d.id.clone())),
        Some("r:a".to_string())
    );
    assert!(has(cx, "details") && has(cx, "details-verdict"));
    // Opening details neither ticks nor moves anything.
    assert!(h.drain().is_empty());
    keys(cx, "escape");
    assert!(view.read_with(cx, |v, _| v.details.is_none()));

    // The same popover from the "..." menu, for a Review row.
    hover(cx, "clean-row:r:b");
    click(cx, "clean-more:r:b");
    click(cx, "menu-show-details");
    assert!(view.read_with(cx, |v, _| v.menu.is_none()));
    assert_eq!(
        view.read_with(cx, |v, _| v.details.as_ref().map(|d| d.id.clone())),
        Some("r:b".to_string())
    );
    assert!(has(cx, "details"));
    assert!(h.drain().is_empty());
}

#[::gpui::test]
fn the_system_data_sheet_measures_on_open_and_only_copies_commands(cx: &mut TestAppContext) {
    use tidy_core::sysdata::{CopyCommand, SysKind, SysRow, SysSize, SystemDataReport};
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    keys(cx, "cmd-1");
    h.drain();
    assert!(has(cx, "never"), "Check up lists what Tidy never does");
    assert!(has(cx, "sysdata-open"));
    assert!(!has(cx, "system-data"));

    click(cx, "sysdata-open");
    assert!(model(cx).read_with(cx, |m, _| m.system_data.open && m.system_data.measuring));
    let reply = match h.drain().into_iter().next() {
        Some(Command::MeasureSystemData { reply }) => reply,
        other => panic!("{other:?}"),
    };
    assert!(has(cx, "system-data"));
    let report = SystemDataReport {
        at: NOW,
        rows: vec![
            SysRow {
                kind: SysKind::Caches,
                title: "Caches".into(),
                note: "n".into(),
                size: SysSize::Measured(3 * GB),
                report_only: false,
                commands: vec![],
            },
            SysRow {
                kind: SysKind::Docker,
                title: "Docker".into(),
                note: "n".into(),
                size: SysSize::NotMeasured,
                report_only: true,
                commands: vec![CopyCommand {
                    label: "See what Docker uses".into(),
                    command: "docker system df".into(),
                }],
            },
        ],
    };
    reply.send(report).unwrap();
    cx.run_until_parked();
    assert!(model(cx).read_with(cx, |m, _| !m.system_data.measuring));
    assert!(has(cx, "sysdata-row:caches") && has(cx, "sysdata-row:docker"));
    assert!(has(cx, "sysdata-row:rest"), "the rest has its own row");
    assert!(has(cx, "sysdata-footer"));

    click(cx, "copy:docker-0");
    let clip = cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip.as_deref(), Some("docker system df"));
    assert!(h.drain().is_empty(), "copying runs nothing");

    keys(cx, "escape");
    assert!(model(cx).read_with(cx, |m, _| !m.system_data.open));
}

#[::gpui::test]
fn the_sidebar_link_opens_the_system_data_sheet(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    model(cx).update(cx, |m, cx| {
        m.disk = Some(crate::macos::DiskInfo {
            name: "Macintosh HD".into(),
            total: 1_000 * GB,
            available: 212 * GB,
        });
        cx.notify();
    });
    cx.run_until_parked();
    h.drain();
    click(cx, "sysdata-link");
    assert!(model(cx).read_with(cx, |m, _| m.system_data.open));
    assert!(matches!(
        h.drain().as_slice(),
        [Command::MeasureSystemData { .. }]
    ));
    click(cx, "sysdata-close");
    assert!(model(cx).read_with(cx, |m, _| !m.system_data.open));
}

#[::gpui::test]
fn reveal_goes_through_the_opener(cx: &mut TestAppContext) {
    let (h, _view, cx) = open_with_plan(cx);
    hover(cx, "clean-row:r:log");
    click(cx, "clean-reveal:r:log");
    assert_eq!(
        h.opened.borrow().as_slice(),
        &[(OpenKind::Reveal, PathBuf::from("/fake/log"))]
    );
}

// ---------- Sidebar, shortcuts, windows ----------

#[::gpui::test]
fn sidebar_switches_screens_and_shows_its_figures(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    push(&h, cx, vec![Event::LargeResult(large_scan())]);
    assert!(has(cx, "sidebar") && has(cx, "sidebar-cleanup") && has(cx, "sidebar-large"));
    assert!(has(cx, "sidebar-cleanup-figure") && has(cx, "sidebar-large-figure"));
    let figures = model(cx).read_with(cx, |m, _| {
        (
            crate::views::sidebar::figure_for(Screen::Cleanup, m),
            crate::views::sidebar::figure_for(Screen::Large, m),
        )
    });
    assert_eq!(figures, (Some("3.5 GB".to_string()), Some("2".to_string())));

    keys(cx, "cmd-3");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Large);
    assert!(has(cx, "large") && !has(cx, "cleanup"));
    keys(cx, "cmd-2");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Cleanup);
    click(cx, "sidebar-large");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Large);
    click(cx, "sidebar-cleanup");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Cleanup);
    let _ = (&mut h, view);
}

#[::gpui::test]
fn the_sidebar_shows_the_disk(cx: &mut TestAppContext) {
    let _h = setup(cx);
    let (_view, cx) = open_main(cx);
    assert!(!has(cx, "disk"), "no volume info in tests");
    model(cx).update(cx, |m, cx| {
        m.disk = Some(crate::macos::DiskInfo {
            name: "Macintosh HD".into(),
            total: 1_000 * GB,
            available: 212 * GB,
        });
        cx.notify();
    });
    cx.run_until_parked();
    assert!(has(cx, "disk"));
    assert_eq!(
        crate::format::disk_line(212 * GB, 1_000 * GB),
        "212 GB available of 1 TB"
    );
}

#[::gpui::test]
fn cmd_r_rescans_the_current_screen(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = open_with_plan(cx);
    keys(cx, "cmd-r");
    assert!(matches!(h.drain()[0], Command::ScanCleanup));
    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    keys(cx, "cmd-3");
    keys(cx, "cmd-r");
    let cmds = h.drain();
    assert!(
        matches!(cmds[0], Command::ScanLarge { record_min: None }),
        "{cmds:?}"
    );
}

#[::gpui::test]
fn cleanup_scans_again_on_its_own_only_after_the_first_scan_and_when_stale(
    cx: &mut TestAppContext,
) {
    let settings = Settings {
        cleanup_scanned_once: true,
        ..Settings::default()
    };
    let mut h = setup_with(cx, settings);
    let (_view, cx) = open_main(cx);
    // Scanned before and nothing loaded: the screen scans when it opens.
    assert!(matches!(h.drain()[0], Command::ScanCleanup));
    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    // A fresh result is reused.
    keys(cx, "cmd-3");
    keys(cx, "cmd-2");
    assert!(h.drain().is_empty());
    // After 10 minutes it scans again.
    h.clock.advance(STALE_SCAN_SECS + 1);
    keys(cx, "cmd-3");
    keys(cx, "cmd-2");
    assert!(matches!(h.drain()[0], Command::ScanCleanup));
}

#[::gpui::test]
fn cmd_w_closes_the_window_and_the_app_keeps_running(cx: &mut TestAppContext) {
    let _h = setup(cx);
    let (_view, cx) = open_main(cx);
    let first = cx
        .update(|_, cx| cx.global::<crate::app::MainWindow>().0)
        .unwrap();
    TestAppContext::update(cx, |cx| {
        first.update(cx, |_, w, _| w.remove_window()).unwrap()
    });
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 0, "closing the window does not quit");
    TestAppContext::update(cx, crate::reopen);
    cx.run_until_parked();
    assert_eq!(
        cx.windows().len(),
        1,
        "a Dock click reopens the main window"
    );
    // The model outlived the window.
    assert!(TestAppContext::update(cx, |cx| cx
        .try_global::<Model>()
        .is_some()));
}

#[::gpui::test]
fn graceful_shutdown_waits_for_the_core_once(cx: &mut TestAppContext) {
    let (tx, mut rx) = unbounded_channel::<Command>();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let replies = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let r2 = replies.clone();
    rt.spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if let Command::Shutdown(done) = cmd {
                tokio::time::sleep(Duration::from_millis(50)).await;
                r2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = done.send(());
            }
        }
    });
    cx.update(|cx| {
        cx.set_global(Core(CoreHandle::new(tx)));
        cx.set_global(Tokio(rt));
    });
    let t0 = Instant::now();
    cx.update(graceful_shutdown);
    assert_eq!(replies.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        t0.elapsed() >= Duration::from_millis(50),
        "it waited for the core"
    );
    cx.update(graceful_shutdown);
    assert_eq!(
        replies.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the second call does nothing"
    );
}

#[::gpui::test]
fn progress_events_do_not_flood_repaints(cx: &mut TestAppContext) {
    let h = setup(cx);
    let (_view, cx) = open_main(cx);
    let before = model(cx).read_with(cx, |m, _| m.applies);
    let events: Vec<Event> = (0..200)
        .map(|i| Event::ScanProgress {
            kind: ScanKind::Large,
            files: i * 100,
            bytes: 0,
            current: None,
        })
        .collect();
    push(&h, cx, events);
    let after = model(cx).read_with(cx, |m, _| m.applies);
    assert_eq!(after - before, 1, "200 events, one update");
}

#[::gpui::test]
fn a_failure_shows_a_notice_and_ends_the_busy_state(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = open_with_plan(cx);
    click(cx, "rescan-button");
    assert!(matches!(h.drain()[0], Command::ScanCleanup));
    assert!(model(cx).read_with(cx, |m, _| m.cleanup.scanning.is_some()));
    push(
        &h,
        cx,
        vec![Event::Failed {
            what: "scan".into(),
            message: "Tidy couldn't read the home folder.".into(),
        }],
    );
    assert!(model(cx).read_with(cx, |m, _| m.cleanup.scanning.is_none()));
    assert!(has(cx, "notice"));
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.cleanup.notice.clone()),
        Some(Notice::Error("Tidy couldn't read the home folder.".into()))
    );
}

#[::gpui::test]
fn stop_cancels_a_running_scan(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = open_with_plan(cx);
    keys(cx, "cmd-r");
    h.drain();
    click(cx, "scan-stop");
    assert!(matches!(h.drain()[..], [Command::Cancel]));
    push(
        &h,
        cx,
        vec![Event::ScanStopped {
            kind: ScanKind::Cleanup,
        }],
    );
    assert!(model(cx).read_with(cx, |m, _| m.cleanup.scanning.is_none()));
    assert!(
        model(cx).read_with(cx, |m, _| m.cleanup.plan.is_some()),
        "the old plan stays"
    );
}

// ---------- Large files ----------

#[::gpui::test]
fn large_starts_with_an_empty_state_and_scans_on_a_click(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (view, cx) = open_main(cx);
    keys(cx, "cmd-3");
    assert!(has(cx, "large") && has(cx, "empty-state"));
    assert!(!has(cx, "threshold"), "no controls before the first scan");
    assert!(h.drain().is_empty(), "visiting sends nothing (C11)");
    let snap = view.read_with(cx, |v, cx| v.large_snapshot(cx, NOW));
    assert!(!snap.has_scan);
    assert!(crate::views::large::empty_body(snap.min).contains("over 500 MB"));
    click(cx, "empty-action");
    let cmds = h.drain();
    assert!(
        matches!(cmds[0], Command::ScanLarge { record_min: None }),
        "{cmds:?}"
    );
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Command::UpdateSettings(s) if s.large_scanned_once)),
        "{cmds:?}"
    );
    assert!(has(cx, "scanning"));
    push(&h, cx, vec![Event::LargeResult(large_scan())]);
    assert!(has(cx, "threshold") && !has(cx, "empty-state"));
}

#[::gpui::test]
fn files_mode_lists_biggest_and_the_threshold_refilters_without_a_rescan(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_scan(cx);
    assert!(has(cx, "large-row:Movies/trip.mov") && has(cx, BIG_ROW));
    assert!(
        !has(cx, "large-row:Apps/Big.app"),
        "300 MB is under the 500 MB default"
    );
    let snap = view.read_with(cx, |v, cx| v.large_snapshot(cx, NOW));
    assert_eq!(snap.status_left(), "2 items, up to 1.6 GB");
    assert_eq!(snap.threshold_label(), "Over 500 MB");
    assert!(has(cx, "scanned-at"));

    click(cx, "threshold");
    assert!(view.read_with(cx, |v, _| v.threshold_menu.is_some()));
    assert!(has(cx, "threshold-menu"));
    click(cx, "threshold-option-0"); // 100 MB
    assert!(view.read_with(cx, |v, _| v.threshold_menu.is_none()));
    assert_eq!(model(cx).read_with(cx, |m, _| m.large.min), 100 * MB);
    assert!(has(cx, "large-row:Apps/Big.app") && has(cx, "large-row:Downloads/old.zip"));
    let snap = view.read_with(cx, |v, cx| v.large_snapshot(cx, NOW));
    assert_eq!(snap.threshold_label(), "Over 100 MB");
    assert_eq!(snap.status_left(), "4 items, up to 2 GB");
    let cmds = h.drain();
    assert!(
        !cmds.iter().any(|c| matches!(c, Command::ScanLarge { .. })),
        "no rescan: {cmds:?}"
    );
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Command::UpdateSettings(s) if s.large_min_bytes == 100 * MB)),
        "the choice is saved: {cmds:?}"
    );

    // 5 GB leaves nothing; the list says so instead of showing blank space.
    click(cx, "threshold");
    click(cx, "threshold-option-3");
    assert!(has(cx, "large-none"));
    click(cx, "threshold");
    keys(cx, "escape");
    assert!(view.read_with(cx, |v, _| v.threshold_menu.is_none()));
}

const BIG_ROW: &str = "large-row:Library/Application Support/Models/big.bin";

#[::gpui::test]
fn not_opened_lately_keeps_the_old_items(cx: &mut TestAppContext) {
    let (_h, view, cx) = open_with_scan(cx);
    click(cx, "threshold");
    click(cx, "threshold-option-0");
    click(cx, "filter-not-opened");
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.large.filter),
        LargeFilter::NotOpened
    );
    assert!(has(cx, BIG_ROW) && has(cx, "large-row:Apps/Big.app"));
    assert!(!has(cx, "large-row:Movies/trip.mov"), "opened 5 days ago");
    assert!(!has(cx, "large-row:Downloads/old.zip"));
    let snap = view.read_with(cx, |v, cx| v.large_snapshot(cx, NOW));
    assert_eq!(snap.status_left(), "2 items, up to 1 GB");
    click(cx, "filter-biggest");
    assert!(has(cx, "large-row:Movies/trip.mov"));
}

#[::gpui::test]
fn rows_carry_tags_and_dates_in_the_data(cx: &mut TestAppContext) {
    let (_h, view, cx) = open_with_scan(cx);
    click(cx, "threshold");
    click(cx, "threshold-option-0");
    let rows = view.read_with(cx, |v, cx| v.large_snapshot(cx, NOW).rows.clone());
    let find = |rel: &str| {
        rows.iter()
            .find_map(|r| match r {
                crate::app::LargeRow::Item(e) if e.rel == rel => Some(e.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert!(find(BIG).app_data);
    assert_eq!(find("Apps/Big.app").kind, LargeKind::Package);
    assert_eq!(
        crate::format::use_phrase(find("Apps/Big.app").last_used, UseSource::FileTimes, NOW),
        "Modified 6 months ago"
    );
}

#[::gpui::test]
fn folders_mode_drills_down_with_a_breadcrumb_and_cmd_up(cx: &mut TestAppContext) {
    let (_h, view, cx) = open_with_scan(cx);
    click(cx, "threshold");
    click(cx, "threshold-option-0");
    click(cx, "mode-folders");
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.large.mode),
        LargeMode::Folders
    );
    assert!(
        !has(cx, "filter-biggest"),
        "the filter is hidden in Folders mode"
    );
    assert!(has(cx, "breadcrumb") && has(cx, "crumb-0") && !has(cx, "crumb-1"));
    assert!(has(cx, "large-row:Movies") && has(cx, "large-row:Library"));
    assert!(
        has(cx, "large-open:Library"),
        "a folder with contents has a chevron"
    );
    assert!(!has(cx, "large-open:Movies"), "a leaf does not");

    click(cx, "large-open:Library");
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.large.folder.clone()),
        "Library"
    );
    assert!(has(cx, "crumb-1") && has(cx, "large-row:Library/Application Support"));
    assert!(!has(cx, "large-row:Movies"));
    let crumbs = view.read_with(cx, |v, cx| v.large_snapshot(cx, NOW).crumbs.clone());
    assert_eq!(
        crumbs.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["Home", "Library"]
    );

    // The earlier segment is a button; Cmd-Up does the same from the keyboard.
    click(cx, "crumb-0");
    assert_eq!(model(cx).read_with(cx, |m, _| m.large.folder.clone()), "");
    click(cx, "large-open:Library");
    keys(cx, "cmd-up");
    assert_eq!(model(cx).read_with(cx, |m, _| m.large.folder.clone()), "");
    assert!(has(cx, "large-row:Movies"));

    // Going back to Files mode returns to the home folder's list.
    click(cx, "mode-files");
    assert!(has(cx, "filter-biggest") && !has(cx, "breadcrumb"));
}

#[::gpui::test]
fn a_double_click_opens_a_folder_and_a_single_click_only_ticks_it(cx: &mut TestAppContext) {
    let (_h, _view, cx) = open_with_scan(cx);
    click(cx, "threshold");
    click(cx, "threshold-option-0");
    click(cx, "mode-folders");
    let b = bounds(cx, "large-row:Library").expect("row");
    let pos = point(b.origin.x + px(300.), b.center().y);
    cx.simulate_click(pos, Modifiers::none());
    assert_eq!(model(cx).read_with(cx, |m, _| m.large.folder.clone()), "");
    assert!(model(cx).read_with(cx, |m, _| m.large.selected.contains("Library")));
    for click_count in [1, 2] {
        cx.simulate_event(MouseDownEvent {
            position: pos,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position: pos,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
        });
    }
    cx.run_until_parked();
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.large.folder.clone()),
        "Library"
    );
    assert!(
        model(cx).read_with(cx, |m, _| m.large.selected.is_empty()),
        "opening a folder clears the selection"
    );
}

#[::gpui::test]
fn folders_mode_folds_small_children_into_one_row(cx: &mut TestAppContext) {
    let (_h, _view, cx) = open_with_scan(cx);
    click(cx, "mode-folders");
    // At 500 MB the home folder keeps Movies and Library; Apps and Downloads fold.
    assert!(has(cx, "large-row:Movies") && has(cx, "large-row:Library"));
    assert!(!has(cx, "large-row:Apps"));
    assert!(has(cx, "large-rest"));
    assert!(
        !has(cx, "large-check:Apps"),
        "the folded row has no checkbox"
    );
}

#[::gpui::test]
fn moving_to_trash_always_asks_and_names_the_risks(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_scan(cx);
    click(cx, "large-check:Movies/trip.mov");
    click(cx, &format!("large-check:{BIG}"));
    let snap = view.read_with(cx, |v, cx| v.large_snapshot(cx, NOW));
    assert_eq!(snap.status_left(), "2 selected, 1.6 GB");

    // The status bar button and Cmd-Backspace both open the same dialog.
    click(cx, "move-to-trash");
    assert!(dialog_of(&view, cx).is_some());
    keys(cx, "escape");
    assert!(dialog_of(&view, cx).is_none() && h.drain().is_empty());
    keys(cx, "cmd-backspace");
    let spec = dialog_of(&view, cx).expect("dialog");
    assert_eq!(
        spec.kind,
        DialogKind::TrashLarge {
            rels: vec!["Movies/trip.mov".to_string(), BIG.to_string()]
        }
    );
    assert_eq!(spec.title, "Move these 2 items to the Trash?");
    assert_eq!(
        spec.rows,
        vec![
            ("trip.mov".to_string(), "900 MB".to_string()),
            ("big.bin".to_string(), "700 MB".to_string())
        ]
    );
    // App data and over 1 GB: both notes.
    assert_eq!(
        spec.notes,
        vec![APP_DATA_NOTE.to_string(), CLONE_NOTE.to_string()]
    );
    assert!(h.drain().is_empty(), "nothing moves before Move to Trash");

    keys(cx, "enter");
    let cmds = h.drain();
    match &cmds[..] {
        [Command::TrashPaths { paths }] => assert_eq!(
            paths,
            &[
                PathBuf::from("/fake/Movies/trip.mov"),
                PathBuf::from(format!("/fake/{BIG}"))
            ]
        ),
        other => panic!("{other:?}"),
    }

    // The core reports each path; the rows fade and leave.
    push(
        &h,
        cx,
        vec![
            Event::CleanProgress { done: 1, total: 2 },
            Event::ItemTrashed {
                id: "/fake/Movies/trip.mov".into(),
                trash_path: None,
            },
            Event::ItemSkipped {
                id: format!("/fake/{BIG}"),
                reason: "The file changed".into(),
            },
        ],
    );
    assert!(
        has(cx, "large-row:Movies/trip.mov"),
        "still drawn while it fades"
    );
    advance(cx, 160);
    assert!(!has(cx, "large-row:Movies/trip.mov"));
    assert!(has(cx, BIG_ROW), "the skipped row stays");
    assert!(model(cx).read_with(cx, |m, _| m.large.selected.is_empty()));
    push(
        &h,
        cx,
        vec![Event::CleanDone(entry(
            Source::Large,
            Action::Trash,
            vec![history_item(
                "/fake/Movies/trip.mov",
                ItemResult::Trashed,
                None,
            )],
            900 * MB,
        ))],
    );
    assert!(has(cx, "notice") && has(cx, "notice-history") && !has(cx, "notice-empty-trash"));
    assert!(model(cx).read_with(cx, |m, _| m.large.trashing.is_none()));
}

#[::gpui::test]
fn a_small_selection_has_no_clone_note_and_hover_trash_selects_one_row(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_scan(cx);
    click(cx, "threshold");
    click(cx, "threshold-option-0");
    click(cx, "large-check:Movies/trip.mov");
    h.drain();
    hover(cx, "large-row:Downloads/old.zip");
    click(cx, "large-trash:Downloads/old.zip");
    let spec = dialog_of(&view, cx).expect("dialog");
    assert_eq!(spec.title, "Move this to the Trash?");
    assert_eq!(
        spec.rows,
        vec![("old.zip".to_string(), "120 MB".to_string())]
    );
    assert!(spec.notes.is_empty());
    assert_eq!(
        model(cx).read_with(cx, |m, _| m
            .large
            .selected
            .iter()
            .cloned()
            .collect::<Vec<_>>()),
        ["Downloads/old.zip"],
        "the hover button selects that one row"
    );
    click(cx, "dialog-confirm");
    let cmds = h.drain();
    assert!(
        matches!(&cmds[..], [Command::TrashPaths { paths }] if paths == &[PathBuf::from("/fake/Downloads/old.zip")]),
        "{cmds:?}"
    );
}

#[::gpui::test]
fn the_trash_button_is_disabled_without_a_selection(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_scan(cx);
    click(cx, "move-to-trash");
    keys(cx, "cmd-backspace");
    assert!(dialog_of(&view, cx).is_none() && h.drain().is_empty());
}

#[::gpui::test]
fn large_reveal_goes_through_the_opener(cx: &mut TestAppContext) {
    let (h, _view, cx) = open_with_scan(cx);
    hover(cx, "large-row:Movies/trip.mov");
    click(cx, "large-reveal:Movies/trip.mov");
    assert_eq!(
        h.opened.borrow().as_slice(),
        &[(OpenKind::Reveal, PathBuf::from("/fake/Movies/trip.mov"))]
    );
}

#[::gpui::test]
fn a_new_scan_replaces_the_result_and_stop_cancels(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = open_with_scan(cx);
    click(cx, "large-rescan");
    assert!(matches!(h.drain()[0], Command::ScanLarge { .. }));
    // The previous result stays while the new scan runs.
    assert!(has(cx, "scanning") && has(cx, "large-row:Movies/trip.mov"));
    click(cx, "scan-stop");
    assert!(matches!(h.drain()[..], [Command::Cancel]));
    push(
        &h,
        cx,
        vec![Event::ScanStopped {
            kind: ScanKind::Large,
        }],
    );
    assert!(!has(cx, "scanning") && has(cx, "large-row:Movies/trip.mov"));
}

#[::gpui::test]
fn the_app_only_sends_commands_it_never_touches_the_disk(cx: &mut TestAppContext) {
    // Every action that would move a file ends in a Command on the fake core channel, and
    // the paths in it are the fake scan root's, not anything on this machine.
    let (mut h, _view, cx) = open_with_scan(cx);
    click(cx, "large-check:Movies/trip.mov");
    click(cx, "move-to-trash");
    click(cx, "dialog-confirm");
    let cmds = h.drain();
    match &cmds[..] {
        [Command::TrashPaths { paths }] => {
            assert!(paths.iter().all(|p| p.starts_with("/fake")), "{paths:?}");
            assert!(!Path::new("/fake").exists());
        }
        other => panic!("{other:?}"),
    }
}

// ====================================================================================
// M2: sidebar, Memory, Security, Check up
// ====================================================================================

/// Commands of one kind, in order, out of a drained list.
fn count_cmds(cmds: &[Command], pick: impl Fn(&Command) -> bool) -> usize {
    cmds.iter().filter(|c| pick(c)).count()
}

fn is_snapshot(c: &Command) -> bool {
    matches!(c, Command::SnapshotMemory)
}

fn date(day: i8) -> ReminderDate {
    ReminderDate {
        at: NOW + 7 * DAY,
        year: 2026,
        month: 10,
        day,
        hour: 10,
        minute: 0,
    }
}

/// Answers the first `NextReminder` in `cmds` with `d`. Returns false when there was none.
fn answer_next_reminder(cmds: Vec<Command>, d: ReminderDate) -> bool {
    for c in cmds {
        if let Command::NextReminder { reply } = c {
            let _ = reply.send(d);
            return true;
        }
    }
    false
}

// ---------- sidebar and shortcuts ----------

#[::gpui::test]
fn the_sidebar_lists_five_screens_in_order_and_cmd_1_to_5_switch(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    let titles: Vec<&str> = Screen::ALL.iter().map(|s| s.title()).collect();
    assert_eq!(
        titles,
        ["Check up", "Cleanup", "Large files", "Memory", "Security"]
    );
    let shortcuts: Vec<&str> = Screen::ALL.iter().map(|s| s.shortcut()).collect();
    assert_eq!(
        shortcuts,
        [
            "\u{2318}1",
            "\u{2318}2",
            "\u{2318}3",
            "\u{2318}4",
            "\u{2318}5"
        ]
    );
    // Top to bottom in the sidebar.
    let ys: Vec<Pixels> = Screen::ALL
        .iter()
        .map(|s| {
            bounds(cx, crate::views::sidebar::selector_for(*s))
                .expect("row")
                .origin
                .y
        })
        .collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");

    for (keystroke, screen, root) in [
        ("cmd-1", Screen::Checkup, "checkup"),
        ("cmd-2", Screen::Cleanup, "cleanup"),
        ("cmd-3", Screen::Large, "large"),
        ("cmd-4", Screen::Memory, "memory"),
        ("cmd-5", Screen::Security, "security"),
    ] {
        keys(cx, keystroke);
        assert_eq!(
            model(cx).read_with(cx, |m, _| m.screen),
            screen,
            "{keystroke}"
        );
        assert!(has(cx, root), "{keystroke} shows {root}");
    }
    // Clicking a row switches too.
    click(cx, "sidebar-checkup");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Checkup);
    click(cx, "sidebar-security");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Security);
    let _ = h.drain();
}

#[::gpui::test]
fn cmd_r_rescans_each_new_screen(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    keys(cx, "cmd-4");
    let cmds = h.drain();
    assert_eq!(count_cmds(&cmds, is_snapshot), 1, "{cmds:?}");
    keys(cx, "cmd-r");
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 1);
    keys(cx, "cmd-5");
    h.drain();
    keys(cx, "cmd-r");
    let cmds = h.drain();
    assert!(
        cmds.iter().any(|c| matches!(
            c,
            Command::RunSecurity {
                include_updates: false
            }
        )),
        "{cmds:?}"
    );
    keys(cx, "cmd-1");
    h.drain();
    keys(cx, "cmd-r");
    let cmds = h.drain();
    assert!(
        cmds.iter().any(|c| matches!(
            c,
            Command::RunCheckup {
                include_updates: false
            }
        )),
        "{cmds:?}"
    );
}

// ---------- Memory ----------

fn mem_app(name: &str, pid: i32, mb: u64, helpers: u32) -> AppMemory {
    AppMemory {
        id: format!("app:/Applications/{name}.app"),
        name: name.to_string(),
        kind: MemoryKind::App,
        pid,
        pids: std::iter::once(pid)
            .chain((0..helpers as i32).map(|i| pid + 1000 + i))
            .collect(),
        bundle_id: Some(format!("com.example.{}", name.to_lowercase())),
        app_path: Some(PathBuf::from(format!("/Applications/{name}.app"))),
        footprint: mb * MB,
        helpers,
        quittable: true,
        members: Vec::new(),
    }
}

fn id_of(name: &str) -> String {
    format!("app:/Applications/{name}.app")
}

fn mem_snapshot(pressure: Pressure) -> MemorySnapshot {
    let system = AppMemory {
        id: "system".into(),
        name: "System processes".into(),
        kind: MemoryKind::System,
        pid: 0,
        pids: vec![],
        bundle_id: None,
        app_path: None,
        footprint: 900 * MB,
        helpers: 0,
        quittable: false,
        members: vec!["WindowServer".into(), "kernel_task".into()],
    };
    // Finder is marked quittable on purpose: the app must not offer Quit for it anyway.
    let finder = mem_app("Finder", 300, 700, 0);
    MemorySnapshot {
        at: NOW,
        pressure,
        pressure_level: match pressure {
            Pressure::Normal => 1,
            Pressure::Warning => 2,
            Pressure::Critical => 4,
        },
        total_bytes: 16 * GB,
        swap: SwapUsage {
            total: 2 * GB,
            used: 1_200 * MB,
            free: 800 * MB,
        },
        compressed_bytes: 3_400 * MB,
        pages: Default::default(),
        apps: vec![
            mem_app("Chrome", 100, 4_200, 12),
            mem_app("Slack", 101, 1_100, 3),
            system,
            finder,
        ],
    }
}

fn on_memory(cx: &mut TestAppContext) -> (Harness, Entity<TidyView>, &mut VisualTestContext) {
    let mut h = setup(cx);
    let (view, cx) = open_main(cx);
    keys(cx, "cmd-4");
    h.drain();
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Normal))],
    );
    (h, view, cx)
}

#[::gpui::test]
fn memory_polls_every_2_seconds_only_while_it_is_visible(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    // Cleanup is showing: nothing polls, however long we wait.
    advance(cx, 20_000);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 0);

    // Opening the screen reads once at once, then every 2 seconds.
    keys(cx, "cmd-4");
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 1);
    advance(cx, MEMORY_PERIOD.as_millis() as u64 - 100);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 0, "not before 2 s");
    advance(cx, 100);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 1);
    advance(cx, 6_000);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 3);

    // Leaving the screen stops it.
    keys(cx, "cmd-2");
    h.drain();
    advance(cx, 30_000);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 0);
    keys(cx, "cmd-3");
    advance(cx, 30_000);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 0);
}

#[::gpui::test]
fn memory_does_not_poll_while_macos_is_not_showing_the_window(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_memory(cx);
    // Minimized, hidden, covered or on another Space: no reads at all.
    cx.simulate_visibility_change(gpui::WindowVisibility::Hidden);
    advance(cx, 20_000);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 0);
    // Back in view: one read at once, then every 2 seconds again.
    cx.simulate_visibility_change(gpui::WindowVisibility::Visible);
    cx.run_until_parked();
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 1);
    advance(cx, MEMORY_PERIOD.as_millis() as u64);
    assert_eq!(count_cmds(&h.drain(), is_snapshot), 1);
}

#[::gpui::test]
fn memory_shows_the_verdict_three_figures_and_the_top_apps(cx: &mut TestAppContext) {
    let (_h, _view, cx) = on_memory(cx);
    for sel in [
        "memory-verdict",
        "memory-figure-pressure",
        "memory-figure-swap",
        "memory-figure-compressed",
        "memory-graph",
    ] {
        assert!(has(cx, sel), "{sel}");
    }
    let (rows, snapshot) = model(cx).read_with(cx, |m, _| {
        (
            crate::views::memory::memory_rows(&m.memory),
            m.memory.snapshot.clone().unwrap(),
        )
    });
    assert_eq!(snapshot.verdict(), Pressure::Normal.verdict());
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Chrome", "Slack", "System processes", "Finder"]);
    assert_eq!(rows[0].meta().0, "12 helpers");
    assert_eq!(crate::format::size(rows[0].bytes), "4.2 GB");
    assert_eq!(crate::format::size(snapshot.swap.used), "1.2 GB");
    assert_eq!(crate::format::size(snapshot.compressed_bytes), "3.4 GB");
    // A Quit button on the apps; none on the System row or on Finder.
    assert!(has(cx, &format!("memory-quit:{}", id_of("Chrome"))));
    assert!(has(cx, &format!("memory-quit:{}", id_of("Slack"))));
    assert!(has(cx, "memory-row:system") && !has(cx, "memory-quit:system"));
    assert!(has(cx, &format!("memory-row:{}", id_of("Finder"))));
    assert!(!has(cx, &format!("memory-quit:{}", id_of("Finder"))));
}

#[::gpui::test]
fn the_verdict_follows_the_pressure(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    for p in [Pressure::Warning, Pressure::Critical, Pressure::Normal] {
        push(&h, cx, vec![Event::MemorySnapshot(mem_snapshot(p))]);
        let shown = model(cx).read_with(cx, |m, _| m.memory.snapshot.as_ref().unwrap().verdict());
        assert_eq!(shown, p.verdict());
        let (first, rest) = crate::views::memory::verdict_parts(p);
        assert!(p.verdict().starts_with(first) && p.verdict().ends_with(rest));
    }
}

#[::gpui::test]
fn quit_sends_a_polite_terminate_then_says_quit_requested(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    click(cx, &format!("memory-quit:{}", id_of("Chrome")));
    // Exactly one polite request, for the main process of that row. Nothing else was signalled.
    assert_eq!(*h.terminator.polite.borrow(), [100]);
    assert!(h.terminator.forced.borrow().is_empty());
    let rows = model(cx).read_with(cx, |m, _| crate::views::memory::memory_rows(&m.memory));
    assert_eq!(rows[0].meta(), ("Quit requested".to_string(), false));
    // The button is gone while the request is out.
    assert!(!has(cx, &format!("memory-quit:{}", id_of("Chrome"))));
    // Pressing it again (through the model) does not send a second request.
    model(cx).update(cx, |m, cx| m.quit_app(&id_of("Chrome"), cx));
    assert_eq!(*h.terminator.polite.borrow(), [100]);
}

#[::gpui::test]
fn after_10_seconds_the_row_says_still_open_and_force_quit_needs_a_confirm(
    cx: &mut TestAppContext,
) {
    let (h, view, cx) = on_memory(cx);
    let id = id_of("Chrome");
    click(cx, &format!("memory-quit:{id}"));
    advance(cx, QUIT_WAIT.as_millis() as u64 - 100);
    let meta = |cx: &mut VisualTestContext| {
        model(cx).read_with(cx, |m, _| {
            crate::views::memory::memory_rows(&m.memory)[0].meta()
        })
    };
    assert_eq!(meta(cx).0, "Quit requested", "not yet after 9.9 s");
    advance(cx, 100);
    assert_eq!(
        meta(cx),
        (
            "Still open. The app may be asking to save.".to_string(),
            true
        )
    );
    // Force Quit is only behind the "..." menu and a confirm.
    assert!(h.terminator.forced.borrow().is_empty());
    assert!(!has(cx, "memory-force-quit"));
    click(cx, &format!("memory-more:{id}"));
    assert!(has(cx, "memory-force-quit"));
    click(cx, "memory-force-quit");
    let dialog = dialog_of(&view, cx).expect("a dialog is open");
    assert_eq!(dialog.title, "Force Quit Chrome?");
    assert_eq!(
        dialog.body.as_deref(),
        Some("Unsaved changes in Chrome will be lost.")
    );
    assert!(dialog.destructive);
    assert!(
        h.terminator.forced.borrow().is_empty(),
        "nothing before the confirm"
    );
    // Cancel does nothing.
    click(cx, "dialog-cancel");
    assert!(h.terminator.forced.borrow().is_empty());
    // Confirm forces the main process of that row, and only it.
    click(cx, &format!("memory-more:{id}"));
    click(cx, "memory-force-quit");
    click(cx, "dialog-confirm");
    assert_eq!(*h.terminator.forced.borrow(), [100]);
    assert_eq!(*h.terminator.polite.borrow(), [100]);
}

#[::gpui::test]
fn a_row_goes_when_the_app_exits(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    let id = id_of("Slack");
    click(cx, &format!("memory-quit:{id}"));
    // The app exits: the next snapshot may still list it for a moment, but the row is gone.
    h.terminator.exited.borrow_mut().insert(101);
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Normal))],
    );
    assert!(!has(cx, &format!("memory-row:{id}")));
    // And a snapshot without it keeps it gone.
    let mut without = mem_snapshot(Pressure::Normal);
    without.apps.retain(|a| a.pid != 101);
    push(&h, cx, vec![Event::MemorySnapshot(without)]);
    assert!(!has(cx, &format!("memory-row:{id}")));
    assert!(model(cx).read_with(cx, |m, _| m.memory.quits.is_empty()
        && m.memory.exited.is_empty()));
}

#[::gpui::test]
fn an_exit_seen_by_the_timer_removes_the_row_too(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    let id = id_of("Slack");
    click(cx, &format!("memory-quit:{id}"));
    h.terminator.exited.borrow_mut().insert(101);
    advance(cx, QUIT_WAIT.as_millis() as u64);
    assert!(!has(cx, &format!("memory-row:{id}")));
}

#[::gpui::test]
fn relaunch_quits_politely_then_opens_the_same_bundle_once_it_has_exited(cx: &mut TestAppContext) {
    use crate::views::memory::{RELAUNCH_POLL, RELAUNCHING, REOPENED, REOPENED_NOTE};
    let (h, _view, cx) = on_memory(cx);
    let id = id_of("Slack");
    let bundle = PathBuf::from("/Applications/Slack.app");
    h.terminator
        .bundles
        .borrow_mut()
        .insert(101, bundle.clone());
    assert!(has(cx, &format!("memory-relaunch:{id}")));
    click(cx, &format!("memory-relaunch:{id}"));
    // One polite request, nothing forced, nothing opened yet.
    assert_eq!(*h.terminator.polite.borrow(), [101]);
    assert!(h.terminator.forced.borrow().is_empty());
    assert!(h.terminator.opened.borrow().is_empty());
    let meta = |cx: &mut VisualTestContext| {
        model(cx).read_with(cx, |m, _| {
            crate::views::memory::memory_rows(&m.memory)
                .into_iter()
                .find(|r| r.name == "Slack")
                .map(|r| r.meta())
        })
    };
    assert_eq!(meta(cx), Some((RELAUNCHING.to_string(), false)));
    assert!(!has(cx, &format!("memory-quit:{id}")));
    assert!(!has(cx, &format!("memory-relaunch:{id}")));
    // A second press while it waits sends nothing.
    model(cx).update(cx, |m, cx| m.relaunch_app(&id, cx));
    model(cx).update(cx, |m, cx| m.quit_app(&id, cx));
    assert_eq!(*h.terminator.polite.borrow(), [101]);
    // A snapshot while it waits keeps the row (Relaunch never hides it).
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Normal))],
    );
    assert!(has(cx, &format!("memory-row:{id}")));
    // The app exits: at the next look the bundle opens again.
    h.terminator.exited.borrow_mut().insert(101);
    advance(cx, RELAUNCH_POLL.as_millis() as u64);
    assert_eq!(*h.terminator.opened.borrow(), [bundle]);
    assert_eq!(meta(cx), Some((REOPENED.to_string(), false)));
    // The note goes after a few seconds; nothing else is signalled or opened.
    advance(cx, REOPENED_NOTE.as_millis() as u64);
    assert_eq!(meta(cx), Some(("3 helpers".to_string(), false)));
    assert_eq!(h.terminator.opened.borrow().len(), 1);
    assert_eq!(*h.terminator.polite.borrow(), [101]);
}

#[::gpui::test]
fn an_app_that_does_not_quit_in_time_is_left_open_and_never_forced(cx: &mut TestAppContext) {
    use crate::views::memory::RELAUNCH_STILL_OPEN;
    let (h, _view, cx) = on_memory(cx);
    let id = id_of("Chrome");
    h.terminator
        .bundles
        .borrow_mut()
        .insert(100, PathBuf::from("/Applications/Chrome.app"));
    click(cx, &format!("memory-relaunch:{id}"));
    advance(cx, QUIT_WAIT.as_millis() as u64 - 300);
    let rows = model(cx).read_with(cx, |m, _| crate::views::memory::memory_rows(&m.memory));
    assert_eq!(rows[0].meta().0, crate::views::memory::RELAUNCHING);
    advance(cx, 300);
    let rows = model(cx).read_with(cx, |m, _| crate::views::memory::memory_rows(&m.memory));
    assert_eq!(rows[0].meta(), (RELAUNCH_STILL_OPEN.to_string(), true));
    // No Force Quit, no open, and the buttons are back to try again.
    assert!(h.terminator.forced.borrow().is_empty());
    assert!(h.terminator.opened.borrow().is_empty());
    assert!(!has(cx, &format!("memory-more:{id}")));
    assert!(has(cx, &format!("memory-relaunch:{id}")));
    assert!(has(cx, &format!("memory-quit:{id}")));
    // Even if it quits later, Tidy does not open it on its own any more.
    h.terminator.exited.borrow_mut().insert(100);
    advance(cx, 20_000);
    assert!(h.terminator.opened.borrow().is_empty());
}

#[::gpui::test]
fn relaunch_needs_a_bundle_and_says_when_macos_does_not_open_it(cx: &mut TestAppContext) {
    use crate::views::memory::{QUIT_REFUSED, REOPEN_FAILED};
    let (h, _view, cx) = on_memory(cx);
    // No bundle from the running app: nothing is sent.
    let id = id_of("Slack");
    click(cx, &format!("memory-relaunch:{id}"));
    assert!(h.terminator.polite.borrow().is_empty());
    let meta = |cx: &mut VisualTestContext, name: &str| {
        model(cx).read_with(cx, |m, _| {
            crate::views::memory::memory_rows(&m.memory)
                .into_iter()
                .find(|r| r.name == name)
                .unwrap()
                .meta()
        })
    };
    assert_eq!(meta(cx, "Slack"), (QUIT_REFUSED.to_string(), true));
    // The app quits but the open fails.
    h.terminator
        .bundles
        .borrow_mut()
        .insert(100, PathBuf::from("/Applications/Chrome.app"));
    h.terminator.open_fails.set(true);
    click(cx, &format!("memory-relaunch:{}", id_of("Chrome")));
    h.terminator.exited.borrow_mut().insert(100);
    advance(cx, 300);
    assert_eq!(meta(cx, "Chrome"), (REOPEN_FAILED.to_string(), true));
}

#[::gpui::test]
fn relaunch_is_never_offered_for_finder_tidy_or_system_rows(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    let mut snapshot = mem_snapshot(Pressure::Normal);
    let mut tidy = mem_app("Tidy", std::process::id() as i32, 80, 0);
    tidy.bundle_id = Some("dev.sdusteric.tidy".into());
    let mut ws = mem_app("WindowServer", 400, 300, 0);
    ws.kind = MemoryKind::Process;
    ws.id = "pid:400".into();
    snapshot.apps.extend([tidy, ws]);
    push(&h, cx, vec![Event::MemorySnapshot(snapshot)]);
    for id in [
        id_of("Finder"),
        id_of("Tidy"),
        "pid:400".to_string(),
        "system".to_string(),
    ] {
        assert!(!has(cx, &format!("memory-relaunch:{id}")), "{id}");
        model(cx).update(cx, |m, cx| m.relaunch_app(&id, cx));
    }
    assert!(h.terminator.polite.borrow().is_empty());
    assert!(h.terminator.opened.borrow().is_empty());
}

#[::gpui::test]
fn what_would_help_shows_only_when_pressure_is_not_normal(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    assert!(!has(cx, "memory-help"));
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Warning))],
    );
    assert!(has(cx, "memory-help"));
    let line = model(cx).read_with(cx, |m, _| {
        let rows = crate::views::memory::memory_rows(&m.memory);
        crate::views::memory::help_line(m.memory.snapshot.as_ref().unwrap(), &rows).unwrap()
    });
    assert!(
        line.starts_with("Quit or relaunch Chrome (4.2 GB) or Slack"),
        "{line}"
    );
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Normal))],
    );
    assert!(!has(cx, "memory-help"));
}

#[::gpui::test]
fn tidy_finder_and_system_can_never_be_quit(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    // Even straight through the model, these rows are refused.
    let mut snapshot = mem_snapshot(Pressure::Normal);
    let own = std::process::id() as i32;
    let mut tidy = mem_app("Tidy", own, 80, 0);
    tidy.bundle_id = Some("dev.sdusteric.tidy".into());
    let mut ws = mem_app("WindowServer", 400, 300, 0);
    ws.kind = MemoryKind::Process;
    ws.id = "pid:400".into();
    let mut login = mem_app("loginwindow", 401, 30, 0);
    login.kind = MemoryKind::Process;
    login.id = "pid:401".into();
    snapshot.apps.extend([tidy, ws, login]);
    push(&h, cx, vec![Event::MemorySnapshot(snapshot)]);
    let tidy_id = id_of("Tidy");
    for id in [
        id_of("Finder"),
        tidy_id.clone(),
        "pid:400".to_string(),
        "pid:401".to_string(),
        "system".to_string(),
    ] {
        assert!(!has(cx, &format!("memory-quit:{id}")), "{id}");
        model(cx).update(cx, |m, cx| m.quit_app(&id, cx));
        model(cx).update(cx, |m, cx| m.force_quit(&id, cx));
    }
    assert!(h.terminator.polite.borrow().is_empty());
    assert!(h.terminator.forced.borrow().is_empty());
}

#[::gpui::test]
fn the_pressure_graph_covers_only_the_time_the_screen_is_open(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    let history =
        |cx: &mut VisualTestContext| model(cx).read_with(cx, |m, _| m.memory.history.clone());
    assert_eq!(history(cx), [Pressure::Normal]);
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Warning))],
    );
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Warning))],
    );
    assert_eq!(
        history(cx),
        [Pressure::Normal, Pressure::Warning, Pressure::Warning]
    );
    // A snapshot that arrives while another screen is showing (a check up) is not a sample.
    keys(cx, "cmd-2");
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Critical))],
    );
    assert_eq!(history(cx).len(), 3);
    // Coming back starts a new graph.
    keys(cx, "cmd-4");
    assert!(history(cx).is_empty());
    push(
        &h,
        cx,
        vec![Event::MemorySnapshot(mem_snapshot(Pressure::Normal))],
    );
    assert_eq!(history(cx), [Pressure::Normal]);
}

#[::gpui::test]
fn a_memory_failure_shows_a_notice(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_memory(cx);
    push(
        &h,
        cx,
        vec![Event::Failed {
            what: "memory".into(),
            message: "Tidy couldn't read memory.".into(),
        }],
    );
    assert!(has(cx, "notice"));
    click(cx, "notice-dismiss");
    advance(cx, 300);
    assert!(!has(cx, "notice"));
}

// ---------- Security ----------

fn check(id: CheckId, status: Status, sentence: &str) -> CheckResult {
    CheckResult::new(id, status, sentence)
}

fn agent(
    label: &str,
    scope: Scope,
    kind: LaunchKind,
    signer: Signer,
    mark: Option<Mark>,
) -> LoginItem {
    let dir = match (scope, kind) {
        (Scope::User, _) => "/fake/Library/LaunchAgents",
        (Scope::System, LaunchKind::Agent) => "/Library/LaunchAgents",
        (Scope::System, LaunchKind::Daemon) => "/Library/LaunchDaemons",
    };
    LoginItem {
        label: label.to_string(),
        plist: Some(PathBuf::from(format!("{dir}/{label}.plist"))),
        program: Some(PathBuf::from(format!(
            "/Applications/{label}.app/Contents/MacOS/run"
        ))),
        runs_at_load: true,
        scope,
        kind,
        notarized: match &signer {
            Signer::Apple => None,
            Signer::Team(_) => Some(true),
            _ => Some(false),
        },
        signer,
        mark,
    }
}

fn security_report(first_run: bool) -> SecurityReport {
    let mut r = SecurityReport::empty(NOW - 120);
    r.first_run = first_run;
    r.compared_to = (!first_run).then(|| "20261021-090000".to_string());
    r.compared_at = (!first_run).then_some(NOW - 2 * 86_400);
    let mut firewall = check(
        CheckId::Firewall,
        Status::Amber,
        "Firewall is off. Your Mac accepts incoming connections from apps that ask for them.",
    );
    firewall.mark = (!first_run).then_some(Mark::Changed);
    let mut login = check(
        CheckId::LoginItems,
        Status::Amber,
        "1 item is new since the last check.",
    );
    login.mark = (!first_run).then_some(Mark::New);
    r.checks = vec![
        check(CheckId::FileVault, Status::Green, "FileVault is on."),
        firewall,
        check(CheckId::Gatekeeper, Status::Green, "Gatekeeper is on."),
        check(
            CheckId::Sip,
            Status::Green,
            "System Integrity Protection is on.",
        ),
        check(
            CheckId::XProtect,
            Status::Green,
            "XProtect updated 12 days ago.",
        ),
        check(
            CheckId::MacosUpdates,
            Status::Gray,
            "Checked on demand. Press Check Now to look for updates.",
        ),
        login,
        check(
            CheckId::ChromeExtensions,
            Status::Green,
            "No new extensions.",
        ),
    ];
    r.login_items = vec![
        agent(
            "com.example.sync",
            Scope::User,
            LaunchKind::Agent,
            Signer::Team("Example Inc".into()),
            (!first_run).then_some(Mark::New),
        ),
        agent(
            "com.example.unknown",
            Scope::System,
            LaunchKind::Daemon,
            Signer::Unsigned,
            None,
        ),
    ];
    r.extensions = vec![ChromeExtension {
        id: "a".repeat(32),
        name: "Reader View".into(),
        version: Some("2.1".into()),
        profile: "Default".into(),
        enabled: true,
        mark: (!first_run).then_some(Mark::New),
    }];
    r
}

fn on_security(cx: &mut TestAppContext) -> (Harness, Entity<TidyView>, &mut VisualTestContext) {
    let mut h = setup(cx);
    let (view, cx) = open_main(cx);
    keys(cx, "cmd-5");
    h.drain();
    (h, view, cx)
}

#[::gpui::test]
fn security_starts_empty_and_a_click_runs_the_checks_without_updates(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_security(cx);
    assert!(has(cx, "empty-state"));
    // Entering the screen sent nothing that checks anything.
    click(cx, "empty-action");
    let cmds = h.drain();
    assert!(
        matches!(
            &cmds[..],
            [Command::RunSecurity {
                include_updates: false
            }]
        ),
        "{cmds:?}"
    );
    // Progress shows in the toolbar; the result replaces the empty state.
    push(&h, cx, vec![Event::SecurityProgress { done: 3, total: 12 }]);
    assert!(has(cx, "busy"));
    push(&h, cx, vec![Event::SecurityResult(security_report(true))]);
    assert!(!has(cx, "busy") && !has(cx, "empty-state"));
    for id in CheckId::ALL {
        assert!(has(cx, &format!("sec-check:{id:?}")), "{id:?}");
    }
}

#[::gpui::test]
fn the_saved_report_shows_before_a_new_run(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    keys(cx, "cmd-5");
    let mut answered = false;
    for c in h.drain() {
        if let Command::LastSecurity { reply } = c {
            let _ = reply.send(Some(security_report(false)));
            answered = true;
        }
    }
    assert!(answered, "entering the screen asks for the saved report");
    cx.run_until_parked();
    advance(cx, 100);
    assert!(has(cx, "sec-check:Firewall"));
    assert!(h.drain().is_empty(), "showing it ran nothing");
}

#[::gpui::test]
fn rows_have_dots_marks_and_amber_buttons_but_gray_rows_have_none(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_security(cx);
    push(&h, cx, vec![Event::SecurityResult(security_report(false))]);
    // Amber rows with a link have a button; green and gray rows do not.
    assert!(has(cx, "sec-action:Firewall"));
    assert!(has(cx, "sec-action:LoginItems"));
    for id in [
        "FileVault",
        "Gatekeeper",
        "Sip",
        "XProtect",
        "MacosUpdates",
        "ChromeExtensions",
    ] {
        assert!(!has(cx, &format!("sec-action:{id}")), "{id}");
    }
    // New and Changed marks.
    assert!(has(cx, "sec-mark:Firewall") && has(cx, "sec-mark:LoginItems"));
    assert!(has(cx, "sec-login-mark:user:com.example.sync"));
    assert!(!has(cx, "sec-login-mark:system:com.example.unknown"));
    assert!(!has(cx, "sec-mark:FileVault"));
    // The summary says what to look at and what was compared.
    let line = model(cx).read_with(cx, |m, _| {
        crate::views::security::summary_line(m.security.report.as_ref().unwrap(), NOW)
    });
    assert_eq!(
        line,
        "New and changed since your check 2 days ago \u{b7} 1 not checked"
    );
    let status = model(cx).read_with(cx, |m, _| {
        crate::views::security::status_line(m.security.report.as_ref().unwrap(), NOW)
    });
    assert_eq!(status, "8 checks, 2 to look at, checked 2 min ago");
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.security.report.as_ref().unwrap().headline()),
        "2 things to look at"
    );
    // The row sentences carry the plain wording.
    let sentence = model(cx).read_with(cx, |m, _| {
        m.security
            .report
            .as_ref()
            .unwrap()
            .check(CheckId::Firewall)
            .unwrap()
            .sentence
            .clone()
    });
    assert!(sentence.starts_with("Firewall is off."));
}

#[::gpui::test]
fn the_first_run_says_so(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_security(cx);
    push(&h, cx, vec![Event::SecurityResult(security_report(true))]);
    let line = model(cx).read_with(cx, |m, _| {
        crate::views::security::summary_line(m.security.report.as_ref().unwrap(), NOW)
    });
    assert_eq!(
        line,
        "This is your first check. Next time Tidy will show what changed \u{b7} 1 not checked"
    );
    assert!(!has(cx, "sec-mark:Firewall") && !has(cx, "sec-login-mark:user:com.example.sync"));
}

#[::gpui::test]
fn amber_buttons_open_the_right_settings_page_only_from_a_click(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_security(cx);
    push(&h, cx, vec![Event::SecurityResult(security_report(false))]);
    assert!(
        h.settings_opened.borrow().is_empty(),
        "nothing opens by itself"
    );
    click(cx, "sec-action:Firewall");
    click(cx, "sec-action:LoginItems");
    assert_eq!(
        *h.settings_opened.borrow(),
        [SettingsLink::Network, SettingsLink::LoginItems]
    );
}

#[::gpui::test]
fn the_updates_row_is_gray_with_a_check_now_button_that_asks_for_updates(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_security(cx);
    push(&h, cx, vec![Event::SecurityResult(security_report(false))]);
    assert!(has(cx, "sec-updates"));
    click(cx, "sec-updates");
    let cmds = h.drain();
    assert!(
        matches!(
            &cmds[..],
            [Command::RunSecurity {
                include_updates: true
            }]
        ),
        "{cmds:?}"
    );
    // Once the updates were checked, the button is gone.
    let mut done = security_report(false);
    done.updates_checked = true;
    done.checks[5] = check(CheckId::MacosUpdates, Status::Green, "macOS is up to date.");
    push(&h, cx, vec![Event::SecurityResult(done)]);
    assert!(!has(cx, "sec-updates"));
}

#[::gpui::test]
fn login_items_show_the_signer_and_a_user_agent_can_go_to_the_trash_after_a_confirm(
    cx: &mut TestAppContext,
) {
    let (mut h, view, cx) = on_security(cx);
    push(&h, cx, vec![Event::SecurityResult(security_report(false))]);
    assert!(has(cx, "sec-login:user:com.example.sync"));
    assert!(has(cx, "sec-login:system:com.example.unknown"));
    let rows = model(cx).read_with(cx, |m, _| {
        crate::views::security::security_rows(&m.security)
    });
    let signer_of = |label: &str| -> String {
        rows.iter()
            .find_map(|r| match r {
                crate::views::security::SecRow::Login(l) if l.label == label => {
                    Some(l.meta[0].0.clone())
                }
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(signer_of("com.example.sync"), "Example Inc");
    assert_eq!(signer_of("com.example.unknown"), "Not signed");

    // A system daemon has no Move to Trash; a user agent has, on hover.
    let can_trash = |label: &str| {
        rows.iter()
            .find_map(|r| match r {
                crate::views::security::SecRow::Login(l) if l.label == label => Some(l.can_trash),
                _ => None,
            })
            .unwrap()
    };
    assert!(can_trash("com.example.sync") && !can_trash("com.example.unknown"));
    hover(cx, "sec-login:user:com.example.sync");
    click(cx, "sec-trash:user:com.example.sync");
    assert_eq!(
        dialog_of(&view, cx).expect("a dialog is open").kind,
        DialogKind::TrashLoginItem {
            key: "user:com.example.sync".into()
        }
    );
    assert!(h.drain().is_empty(), "nothing moves before the confirm");
    click(cx, "dialog-confirm");
    let plist = PathBuf::from("/fake/Library/LaunchAgents/com.example.sync.plist");
    let cmds = h.drain();
    match &cmds[..] {
        [Command::TrashPaths { paths }] => assert_eq!(paths, std::slice::from_ref(&plist)),
        other => panic!("{other:?}"),
    }
    // The core reports the move: the row goes, and the Large files screen is left alone.
    let entry = HistoryEntry {
        id: "x".into(),
        at: NOW,
        action: Action::Trash,
        source: Source::Large,
        trasher: TrasherKind::Dir,
        items: vec![HistoryItem {
            path: plist.clone(),
            trash_path: None,
            bytes: 400,
            rule: "large".into(),
            result: ItemResult::Trashed,
            reason: None,
            trash_id: None,
        }],
        trashed_bytes: 400,
        message: None,
    };
    push(
        &h,
        cx,
        vec![
            Event::ItemTrashed {
                id: plist.to_string_lossy().into_owned(),
                trash_path: None,
            },
            Event::CleanDone(entry),
        ],
    );
    advance(cx, 300);
    assert!(!has(cx, "sec-login:user:com.example.sync"));
    assert!(has(cx, "notice"));
    let (large_notice, large_trashing) =
        model(cx).read_with(cx, |m, _| (m.large.notice.clone(), m.large.trashing));
    assert_eq!((large_notice, large_trashing), (None, None));
}

#[::gpui::test]
fn a_refused_move_keeps_the_login_item_with_the_reason(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_security(cx);
    push(&h, cx, vec![Event::SecurityResult(security_report(false))]);
    hover(cx, "sec-login:user:com.example.sync");
    click(cx, "sec-trash:user:com.example.sync");
    click(cx, "dialog-confirm");
    h.drain();
    let plist = PathBuf::from("/fake/Library/LaunchAgents/com.example.sync.plist");
    let entry = HistoryEntry {
        id: "x".into(),
        at: NOW,
        action: Action::Trash,
        source: Source::Large,
        trasher: TrasherKind::Dir,
        items: vec![HistoryItem {
            path: plist.clone(),
            trash_path: None,
            bytes: 0,
            rule: "large".into(),
            result: ItemResult::Skipped,
            reason: Some("Not allowed".into()),
            trash_id: None,
        }],
        trashed_bytes: 0,
        message: None,
    };
    push(
        &h,
        cx,
        vec![
            Event::ItemSkipped {
                id: plist.to_string_lossy().into_owned(),
                reason: "Not allowed".into(),
            },
            Event::CleanDone(entry),
        ],
    );
    assert!(has(cx, "sec-login:user:com.example.sync"));
    let rows = model(cx).read_with(cx, |m, _| {
        crate::views::security::security_rows(&m.security)
    });
    let skipped = rows.iter().find_map(|r| match r {
        crate::views::security::SecRow::Login(l) if l.label == "com.example.sync" => {
            l.skipped.clone()
        }
        _ => None,
    });
    assert_eq!(skipped.as_deref(), Some("Not allowed"));
}

#[::gpui::test]
fn signatures_read_checking_while_a_run_is_going(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_security(cx);
    let mut report = security_report(false);
    report.login_items[0].signer = Signer::Unknown;
    report.login_items[0].notarized = None;
    push(&h, cx, vec![Event::SecurityResult(report)]);
    let meta = |cx: &mut VisualTestContext| {
        model(cx).read_with(cx, |m, _| {
            crate::views::security::security_rows(&m.security)
                .into_iter()
                .find_map(|r| match r {
                    crate::views::security::SecRow::Login(l) if l.label == "com.example.sync" => {
                        Some(l.meta[0].0.clone())
                    }
                    _ => None,
                })
                .unwrap()
        })
    };
    assert_eq!(meta(cx), "Unknown");
    click(cx, "rescan-button");
    push(&h, cx, vec![Event::SecurityProgress { done: 6, total: 12 }]);
    assert_eq!(meta(cx), "Checking signature");
    push(&h, cx, vec![Event::SecurityResult(security_report(false))]);
    assert_eq!(meta(cx), "Example Inc");
}

#[::gpui::test]
fn a_security_failure_ends_the_run_with_a_notice(cx: &mut TestAppContext) {
    let (h, _view, cx) = on_security(cx);
    click(cx, "empty-action");
    assert!(model(cx).read_with(cx, |m, _| m.security.running.is_some()));
    push(
        &h,
        cx,
        vec![Event::Failed {
            what: "security".into(),
            message: "The checks could not run.".into(),
        }],
    );
    assert!(model(cx).read_with(cx, |m, _| m.security.running.is_none()));
    assert!(has(cx, "notice"));
}

// ---------- Check up ----------

fn finished_checkup() -> Checkup {
    Checkup {
        at: NOW - 30,
        include_updates: false,
        cleanup: Some(CheckupCleanup {
            safe_bytes: 3_500 * MB,
            safe_items: 2,
            review_bytes: 2 * GB,
        }),
        security: Some(CheckupSecurity {
            id: "20261023-140211".into(),
            amber: 2,
            gray: 1,
            first_run: false,
        }),
        memory: Some(CheckupMemory {
            pressure: Pressure::Normal,
            swap_used: 0,
            compressed_bytes: 3 * GB,
        }),
    }
}

fn on_checkup(
    cx: &mut TestAppContext,
    settings: Settings,
) -> (Harness, Entity<TidyView>, &mut VisualTestContext) {
    let mut h = setup_with(cx, settings);
    let (view, cx) = open_main(cx);
    keys(cx, "cmd-1");
    h.drain();
    (h, view, cx)
}

#[::gpui::test]
fn run_check_up_sends_one_command_with_the_updates_choice(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    assert!(has(cx, "run-checkup") && has(cx, "include-updates"));
    click(cx, "run-checkup");
    let cmds = h.drain();
    // The first command is the check up; the click also remembers a Cleanup scan happened.
    assert!(
        matches!(
            cmds[0],
            Command::RunCheckup {
                include_updates: false
            }
        ),
        "{cmds:?}"
    );
    assert!(model(cx).read_with(cx, |m, _| m.checkup.running));
    // The button is disabled while it runs: a second click sends nothing.
    click(cx, "run-checkup");
    assert_eq!(
        count_cmds(&h.drain(), |c| matches!(c, Command::RunCheckup { .. })),
        0
    );
}

#[::gpui::test]
fn include_macos_updates_is_saved_and_passed_on(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    click(cx, "include-updates");
    let cmds = h.drain();
    match &cmds[..] {
        [Command::UpdateSettings(s)] => assert!(s.include_updates),
        other => panic!("{other:?}"),
    }
    click(cx, "run-checkup");
    let cmds = h.drain();
    assert!(
        cmds.iter().any(|c| matches!(
            c,
            Command::RunCheckup {
                include_updates: true
            }
        )),
        "{cmds:?}"
    );
}

#[::gpui::test]
fn the_three_cards_fill_in_and_link_to_their_screens(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    click(cx, "run-checkup");
    h.drain();
    // While it runs, the cards say they are checking.
    let c = model(cx).read_with(cx, |m, _| crate::views::checkup::cards(m));
    assert_eq!(c.cleanup.figure, "Checking");
    assert_eq!(c.security.figure, "Checking");
    assert_eq!(c.memory.figure, "Checking");

    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    let c = model(cx).read_with(cx, |m, _| crate::views::checkup::cards(m));
    assert_eq!(c.cleanup.figure, "3.5 GB");
    assert_eq!(c.cleanup.suffix.as_deref(), Some("ready to clean"));
    assert_eq!(c.cleanup.line, "2 GB more to review");
    assert_eq!(c.security.figure, "Checking");

    push(
        &h,
        cx,
        vec![
            Event::SecurityResult(security_report(false)),
            Event::MemorySnapshot(mem_snapshot(Pressure::Warning)),
        ],
    );
    push(
        &h,
        cx,
        vec![Event::CheckupDone {
            checkup: Checkup {
                memory: Some(CheckupMemory {
                    pressure: Pressure::Warning,
                    swap_used: 0,
                    compressed_bytes: 0,
                }),
                ..finished_checkup()
            },
            reminder: date(30),
        }],
    );
    let c = model(cx).read_with(cx, |m, _| crate::views::checkup::cards(m));
    assert_eq!(c.security.figure, "2 things to look at");
    assert_eq!(c.security.line, "1 not checked");
    assert_eq!(c.memory.figure, "Warning");
    assert_eq!(c.memory.line, Pressure::Warning.verdict());
    assert!(!model(cx).read_with(cx, |m, _| m.checkup.running));
    assert!(has(cx, "card-cleanup") && has(cx, "card-security") && has(cx, "card-memory"));
    assert!(has(cx, "last-checkup"));
    assert_eq!(
        crate::views::checkup::last_line(
            model(cx)
                .read_with(cx, |m, _| m.checkup.last.clone())
                .as_ref(),
            NOW
        ),
        "Last check up just now"
    );

    // Each card opens its screen.
    click(cx, "card-security");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Security);
    keys(cx, "cmd-1");
    click(cx, "card-memory");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Memory);
    keys(cx, "cmd-1");
    click(cx, "card-cleanup");
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Cleanup);
    h.drain();
}

#[::gpui::test]
fn the_clean_button_on_the_card_does_what_cleanup_does(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    // Nothing to clean before a plan exists.
    assert!(!has(cx, "checkup-clean"));
    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    assert!(has(cx, "checkup-clean"));
    click(cx, "checkup-clean");
    let cmds = h.drain();
    // Safe items only: no dialog, one Clean for the ticked ids.
    match &cmds[..] {
        [Command::Clean { item_ids }] => assert_eq!(item_ids, &["r:a", "r:log"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Checkup);
}

#[::gpui::test]
fn the_clean_button_asks_first_when_a_review_item_is_ticked(cx: &mut TestAppContext) {
    let (mut h, view, cx) = on_checkup(cx, Settings::default());
    push(&h, cx, vec![Event::CleanupPlan(plan())]);
    model(cx).update(cx, |m, cx| m.toggle_item("r:b", cx));
    click(cx, "checkup-clean");
    assert!(h.drain().is_empty());
    assert_eq!(
        dialog_of(&view, cx).expect("a dialog is open").kind,
        DialogKind::CleanReview
    );
}

#[::gpui::test]
fn the_last_check_up_loads_when_the_screen_opens(cx: &mut TestAppContext) {
    let mut h = setup(cx);
    let (_view, cx) = open_main(cx);
    keys(cx, "cmd-1");
    let mut answered = false;
    for c in h.drain() {
        if let Command::LastCheckup { reply } = c {
            let _ = reply.send(Some(finished_checkup()));
            answered = true;
        }
    }
    assert!(answered);
    cx.run_until_parked();
    advance(cx, 100);
    let last = model(cx).read_with(cx, |m, _| m.checkup.last.clone());
    assert_eq!(last.map(|c| c.at), Some(NOW - 30));
    let c = model(cx).read_with(cx, |m, _| crate::views::checkup::cards(m));
    // No live plan yet: the card shows the saved figure and says to run again.
    assert_eq!(c.cleanup.figure, "3.5 GB");
    assert_eq!(c.memory.figure, "Normal");
    assert!(!has(cx, "checkup-clean"));
}

#[::gpui::test]
fn a_checkup_failure_ends_the_run_with_a_notice(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    click(cx, "run-checkup");
    h.drain();
    push(
        &h,
        cx,
        vec![Event::Failed {
            what: "checkup".into(),
            message: "The check up could not finish.".into(),
        }],
    );
    assert!(!model(cx).read_with(cx, |m, _| m.checkup.running));
    assert!(has(cx, "notice"));
}

#[::gpui::test]
fn security_waits_for_a_running_check_up_instead_of_dropping_the_request(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    click(cx, "run-checkup");
    h.drain();
    // The check up is still scanning; Security shows it as busy and sends nothing.
    keys(cx, "cmd-5");
    assert!(has(cx, "busy") && !has(cx, "empty-state"));
    keys(cx, "cmd-r");
    model(cx).update(cx, |m, cx| m.run_security(true, cx));
    let cmds = h.drain();
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, Command::RunSecurity { .. })),
        "{cmds:?}"
    );
}

// ---------- the weekly reminder ----------

#[::gpui::test]
fn permission_is_not_asked_at_launch_or_when_visiting_check_up(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    // The app starts, the reminder is off, the screen is open.
    model(cx).update(cx, |m, cx| m.start_reminder(cx));
    cx.run_until_parked();
    assert_eq!(h.notifier.requests.get(), 0);
    assert_eq!(
        h.notifier.status_calls.get(),
        0,
        "the notification center is not even looked at"
    );
    assert!(h.notifier.scheduled.borrow().is_empty());
    keys(cx, "cmd-2");
    keys(cx, "cmd-1");
    assert_eq!(h.notifier.requests.get(), 0);
    h.drain();
}

#[::gpui::test]
fn the_auto_update_switch_drives_sparkle_and_saves_nothing(cx: &mut TestAppContext) {
    use crate::updates::{FakeUpdates, install};

    let fake = Rc::new(FakeUpdates::new(true, true));
    let mut h = setup_with(cx, Settings::default());
    cx.update(|cx| install(fake.clone(), cx));
    let (_view, cx) = open_main(cx);
    keys(cx, "cmd-1");
    h.drain();

    // The row sits under the weekly reminder.
    let reminder = bounds(cx, "reminder").expect("reminder row");
    let row = bounds(cx, "auto-update").expect("auto update row");
    assert!(reminder.bottom() <= row.top());
    assert!(has(cx, "auto-update-toggle") && !has(cx, "auto-update-off"));

    // Off: Sparkle stops downloading by itself but keeps its daily check. On again: both.
    click(cx, "auto-update-toggle");
    assert!(!fake.automatic_downloads.get());
    assert!(fake.automatic_checks.get());
    fake.automatic_checks.set(false);
    click(cx, "auto-update-toggle");
    assert!(fake.automatic_downloads.get() && fake.automatic_checks.get());
    // Sparkle keeps the choice in its own defaults: nothing goes to the core.
    assert!(h.drain().is_empty());
    assert_eq!(h.notifier.requests.get(), 0);

    // The menu item asks the updater to check.
    cx.dispatch_action(crate::app::CheckForUpdates);
    cx.run_until_parked();
    assert_eq!(fake.checks.get(), 1);
}

#[::gpui::test]
fn without_an_updater_check_up_says_off_and_check_for_updates_is_unavailable(
    cx: &mut TestAppContext,
) {
    let (_h, _view, cx) = on_checkup(cx, Settings::default());
    // Unit tests never start Sparkle, so there is no updater and no handler for the menu item.
    cx.update(|_, cx| crate::updates::start(cx));
    assert!(cx.update(|_, cx| crate::updates::control(cx).is_none()));
    assert!(!cx.update(|_, cx| cx.is_action_available(&crate::app::CheckForUpdates)));
    cx.run_until_parked();
    assert!(has(cx, "auto-update-off"));
    assert!(!has(cx, "auto-update-toggle"));
}

#[::gpui::test]
fn switching_the_reminder_on_asks_once_then_schedules_the_single_notification(
    cx: &mut TestAppContext,
) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    assert!(has(cx, "reminder-toggle"));
    click(cx, "reminder-toggle");
    assert_eq!(h.notifier.requests.get(), 1, "the toggle is what asks");
    cx.run_until_parked();
    advance(cx, 100);
    let cmds = h.drain();
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Command::UpdateSettings(s) if s.reminder_enabled)),
        "{cmds:?}"
    );
    // Granted: the app asks the core when it is due and schedules that date.
    assert_eq!(
        model(cx).read_with(cx, |m, _| m.checkup.permission),
        Some(Permission::Granted)
    );
    assert!(answer_next_reminder(cmds, date(30)));
    cx.run_until_parked();
    advance(cx, 100);
    assert_eq!(*h.notifier.scheduled.borrow(), [date(30)]);
    let note = model(cx).read_with(cx, |m, _| {
        crate::views::checkup::reminder_note(
            m.settings.reminder_enabled,
            m.checkup.permission,
            m.checkup.next_reminder.as_ref(),
        )
    });
    assert!(note.contains("Oct 30 at 10:00 AM"), "{note}");

    // A new check up moves the single trigger: the pending one is replaced, not added to.
    push(
        &h,
        cx,
        vec![Event::CheckupDone {
            checkup: finished_checkup(),
            reminder: date(31),
        }],
    );
    assert_eq!(*h.notifier.scheduled.borrow(), [date(30), date(31)]);
    assert_eq!(h.notifier.requests.get(), 1, "never asked again");

    // Switching it off removes the pending notification.
    click(cx, "reminder-toggle");
    assert_eq!(h.notifier.cancels.get(), 1);
    let cmds = h.drain();
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Command::UpdateSettings(s) if !s.reminder_enabled)),
        "{cmds:?}"
    );
    assert_eq!(h.notifier.requests.get(), 1);
}

#[::gpui::test]
fn at_launch_with_the_reminder_on_it_only_looks_up_and_reschedules(cx: &mut TestAppContext) {
    let settings = Settings {
        reminder_enabled: true,
        ..Settings::default()
    };
    let mut h = setup_with(cx, settings);
    h.notifier.permission.set(Permission::Granted);
    let (_view, cx) = open_main(cx);
    model(cx).update(cx, |m, cx| m.start_reminder(cx));
    cx.run_until_parked();
    advance(cx, 100);
    assert_eq!(h.notifier.status_calls.get(), 1);
    assert_eq!(h.notifier.requests.get(), 0, "launch never prompts");
    assert!(answer_next_reminder(h.drain(), date(30)));
    cx.run_until_parked();
    advance(cx, 100);
    assert_eq!(*h.notifier.scheduled.borrow(), [date(30)]);
}

#[::gpui::test]
fn at_launch_with_permission_not_yet_given_nothing_is_asked_or_scheduled(cx: &mut TestAppContext) {
    let settings = Settings {
        reminder_enabled: true,
        ..Settings::default()
    };
    let mut h = setup_with(cx, settings);
    h.notifier.permission.set(Permission::NotDetermined);
    let (_view, cx) = open_main(cx);
    model(cx).update(cx, |m, cx| m.start_reminder(cx));
    cx.run_until_parked();
    advance(cx, 100);
    assert_eq!(h.notifier.requests.get(), 0);
    assert!(h.notifier.scheduled.borrow().is_empty());
    assert!(!answer_next_reminder(h.drain(), date(30)));
}

#[::gpui::test]
fn a_denied_permission_shows_the_notifications_settings_button(cx: &mut TestAppContext) {
    let (mut h, _view, cx) = on_checkup(cx, Settings::default());
    h.notifier.on_request.set(Permission::Denied);
    assert!(!has(cx, "notifications-settings"));
    click(cx, "reminder-toggle");
    cx.run_until_parked();
    advance(cx, 100);
    assert!(has(cx, "notifications-settings"));
    let note = model(cx).read_with(cx, |m, _| {
        crate::views::checkup::reminder_note(
            m.settings.reminder_enabled,
            m.checkup.permission,
            m.checkup.next_reminder.as_ref(),
        )
    });
    assert_eq!(note, "Notifications are off for Tidy");
    // Nothing was scheduled, and the button opens the page only when clicked.
    assert!(h.notifier.scheduled.borrow().is_empty());
    assert!(h.settings_opened.borrow().is_empty());
    click(cx, "notifications-settings");
    assert_eq!(*h.settings_opened.borrow(), [SettingsLink::Notifications]);
    // Fixing it in System Settings: the next look-up schedules the reminder.
    h.notifier.permission.set(Permission::Granted);
    h.drain();
    keys(cx, "cmd-2");
    keys(cx, "cmd-1");
    cx.run_until_parked();
    advance(cx, 100);
    assert!(answer_next_reminder(h.drain(), date(30)));
    cx.run_until_parked();
    advance(cx, 100);
    assert_eq!(*h.notifier.scheduled.borrow(), [date(30)]);
    assert!(!has(cx, "notifications-settings"));
    assert_eq!(h.notifier.requests.get(), 1);
}

#[::gpui::test]
fn clicking_the_reminder_opens_check_up(cx: &mut TestAppContext) {
    let _h = setup(cx);
    let (_view, cx) = open_main(cx);
    keys(cx, "cmd-3");
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let weak = model(cx).downgrade();
    cx.update(|_, cx| crate::views::checkup::watch_reminder_clicks(rx, weak, cx));
    tx.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(model(cx).read_with(cx, |m, _| m.screen), Screen::Checkup);
}

// ---------- Large files: long paths ----------

#[::gpui::test]
fn long_paths_in_large_files_are_fitted_to_the_row(cx: &mut TestAppContext) {
    let (_h, _view, cx) = open_with_scan(cx);
    let long = "Library/Application Support/Some Long Vendor Name/Models/checkpoints/an-extremely-long-model-file-name-v2.safetensors";
    let mut scan = large_scan();
    scan.items
        .insert(0, large_item(long, 1_500, 10, true, LargeKind::File));
    let h2 = &_h;
    push(h2, cx, vec![Event::LargeResult(scan)]);
    assert!(has(cx, &format!("large-row:{long}")));
    // The line the row draws for this path fits the column the window gives it.
    let (viewport, char_w) = cx.update(|window, _| {
        (
            f32::from(window.viewport_size().width),
            crate::views::large::mono_char_width(window),
        )
    });
    let text = crate::views::large::path_text(
        &PathBuf::from("/fake").join(long),
        Path::new("/fake"),
        viewport,
        char_w,
    );
    let room = crate::views::large::text_col_width(viewport);
    assert!(text.contains('\u{2026}'), "{text}");
    assert!(text.starts_with("~/"), "{text}");
    assert!(
        text.chars().count() as f32 * char_w <= room,
        "{text} in {room}"
    );
}

// ---------- the app never signals or schedules anything real ----------

#[::gpui::test]
fn the_test_harness_has_only_fakes_for_quitting_and_notifying(cx: &mut TestAppContext) {
    let h = setup(cx);
    let (_view, cx) = open_main(cx);
    keys(cx, "cmd-1");
    keys(cx, "cmd-4");
    keys(cx, "cmd-5");
    advance(cx, 60_000);
    assert!(h.terminator.polite.borrow().is_empty() && h.terminator.forced.borrow().is_empty());
    assert_eq!(h.notifier.requests.get(), 0);
    assert!(h.notifier.scheduled.borrow().is_empty());
    assert!(h.settings_opened.borrow().is_empty());
}

// ---------- Overlay motion (C36 to C39) ----------

#[::gpui::test]
fn a_sheet_takes_input_from_its_first_frame_and_leaves_through_a_fading_copy(
    cx: &mut TestAppContext,
) {
    let (mut h, view, cx) = open_with_plan(cx);
    view.update(cx, |v, cx| {
        v.open_dialog(
            crate::views::dialogs::DialogSpec::empty_trash(TrashSummary {
                items: 2,
                bytes: GB,
            }),
            cx,
        )
    });
    cx.run_until_parked();
    // The enter is under way, and the confirm button already works.
    assert!(view.read_with(cx, |v, _| v.overlays.entered[0].is_some()));
    click(cx, "dialog-confirm");
    let sent = h.drain();
    assert!(
        sent.iter().any(|c| matches!(c, Command::EmptyTidyTrash)),
        "{sent:?}"
    );
    // The closed dialog is drawn once more while it fades, and takes no action.
    assert!(view.read_with(cx, |v, _| v.dialog.is_none()));
    assert!(has(cx, "dialog"), "the leaving copy is drawn");
    click(cx, "dialog-confirm");
    assert!(h.drain().is_empty(), "the leaving copy does nothing");
    advance(cx, crate::theme::motion::NOTE_OUT.as_millis() as u64 + 40);
    assert!(!has(cx, "dialog"));
    assert!(view.read_with(cx, |v, _| v.overlays.dialog_leaving.is_none()));

    // History: Escape closes it at once in the model; the copy fades and goes.
    click(cx, "history-button");
    h.drain();
    assert!(has(cx, "history"));
    keys(cx, "escape");
    assert!(!view.read_with(cx, |v, _| v.history_open));
    assert!(has(cx, "history"));
    advance(cx, crate::theme::motion::NOTE_OUT.as_millis() as u64 + 40);
    assert!(!has(cx, "history"));
}

#[::gpui::test]
fn sheets_close_at_once_under_reduce_motion(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    cx.update(|_, cx| cx.set_reduce_motion(true));
    click(cx, "history-button");
    h.drain();
    assert!(has(cx, "history"));
    keys(cx, "escape");
    assert!(!has(cx, "history"), "no leaving copy");
    assert!(view.read_with(cx, |v, _| v.overlays.leaving.iter().all(Option::is_none)));
}

#[::gpui::test]
fn the_details_popover_settles_in_and_fades_out_without_taking_clicks(cx: &mut TestAppContext) {
    let (_h, view, cx) = open_with_plan(cx);
    hover(cx, "clean-row:r:a");
    click(cx, "clean-details:r:a");
    assert!(view.read_with(cx, |v, _| v.overlays.details_entered.is_some()));
    // Another row's details replace it at once, with no exit for the old one.
    view.update(cx, |v, cx| {
        v.open_details("r:b".into(), point(px(400.), px(200.)), cx)
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.overlays.details_leaving.is_none()));
    keys(cx, "escape");
    assert!(view.read_with(cx, |v, _| v.details.is_none()
        && v.overlays.details_leaving.is_some()));
    advance(cx, crate::theme::motion::NOTE_OUT.as_millis() as u64 + 40);
    assert!(view.read_with(cx, |v, _| v.overlays.details_leaving.is_none()));
    assert!(!has(cx, "details"));
}

#[::gpui::test]
fn system_data_fades_in_its_first_report_only(cx: &mut TestAppContext) {
    use tidy_core::sysdata::{SysKind, SysRow, SysSize, SystemDataReport};
    let mut h = setup(cx);
    let (view, cx) = open_main(cx);
    keys(cx, "cmd-1");
    h.drain();
    let report = || SystemDataReport {
        at: NOW,
        rows: vec![SysRow {
            kind: SysKind::Caches,
            title: "Caches".into(),
            note: "n".into(),
            size: SysSize::Measured(3 * GB),
            report_only: false,
            commands: vec![],
        }],
    };
    let reply_of = |h: &mut Harness| match h.drain().into_iter().next() {
        Some(Command::MeasureSystemData { reply }) => reply,
        other => panic!("{other:?}"),
    };
    click(cx, "sysdata-open");
    assert!(view.read_with(cx, |v, _| v.overlays.report_arrived.is_none()));
    reply_of(&mut h).send(report()).unwrap();
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.overlays.report_arrived.is_some()));
    assert!(has(cx, "sysdata-row:caches"));
    // Reopened with the report from before: it shows still, also while it measures again.
    keys(cx, "escape");
    advance(cx, 300);
    click(cx, "sysdata-open");
    assert!(view.read_with(cx, |v, _| v.overlays.report_arrived.is_none()));
    reply_of(&mut h).send(report()).unwrap();
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.overlays.report_arrived.is_none()));
}

#[::gpui::test]
fn an_opened_history_entry_fades_its_items_in_and_closes_at_once(cx: &mut TestAppContext) {
    let (mut h, view, cx) = open_with_plan(cx);
    click(cx, "history-button");
    let reply = match h.drain().into_iter().next() {
        Some(Command::LoadHistory { reply }) => reply,
        other => panic!("{other:?}"),
    };
    let e = entry(
        Source::Cleanup,
        Action::Clean,
        vec![history_item("/fake/a", ItemResult::Trashed, None)],
        GB,
    );
    let id = e.id.clone();
    reply.send(vec![e]).unwrap();
    cx.run_until_parked();
    click(cx, "history-entry-0");
    assert!(
        has(cx, "history-item-0-0"),
        "the items are there from the first frame"
    );
    assert_eq!(
        view.read_with(cx, |v, _| v
            .overlays
            .history_opened
            .as_ref()
            .map(|o| o.0.clone())),
        Some(id)
    );
    click(cx, "history-entry-0");
    assert!(!has(cx, "history-item-0-0"), "closing is instant");
}
