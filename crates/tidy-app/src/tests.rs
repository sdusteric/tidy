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
    Action, Class, FolderNode, Group, HistoryEntry, HistoryItem, Item, ItemResult, LargeItem,
    LargeKind, LargeScan, MB, Plan, RunningSet, Source, TrashSummary, TrasherKind, UseSource,
};
use tidy_core::settings::Settings;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::app::{
    Core, EMPTY_TRASH_REFUSED, LargeFilter, LargeMode, Model, Notice, OpenKind, Opener,
    PUMP_PERIOD, RUNNING_PERIOD, STALE_SCAN_SECS, Screen, Tokio, graceful_shutdown, spawn_pump,
};
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
        let m = cx.new(|_| crate::app::TidyModel::new(settings));
        cx.set_global(Model(m.clone()));
        spawn_pump(m.downgrade(), event_rx, PUMP_PERIOD, cx);
    });
    Harness {
        commands,
        events,
        opened,
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
    assert_eq!(snap.status_left(), "Nothing selected");
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
    assert_eq!(snap.button_label(), "Clean 0 KB");

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
        Some(vec![MenuEntry::NeverShow, MenuEntry::CopyPath])
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
