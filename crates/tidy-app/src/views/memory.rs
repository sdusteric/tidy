//! The Memory screen (design sections 5.3 and 11): a verdict from memory pressure, three
//! figures, a pressure history graph, and the top apps by memory with a polite Quit and a
//! Relaunch (C32 to C35).
//!
//! This is the only screen that polls: while it is the visible screen the view asks the core
//! for a snapshot every 2 seconds (see `TidyView::new`), and when it is not visible nothing
//! runs. The app never signals a process itself. Quit is `NSRunningApplication.terminate`
//! through the [`crate::macos::Terminator`] global, Force Quit only follows a confirm, and a
//! row that is Tidy, Finder, WindowServer, loginwindow or the System row has no Quit button.
//!
//! Relaunch is a polite Quit followed by opening the same app bundle again once it has exited.
//! It never force quits: an app that does not exit within [`QUIT_WAIT`] is left running and its
//! row says so. Only rows of real apps (an `.app` bundle) can be relaunched.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::api::Command;
use tidy_core::model::{AppMemory, MemoryKind, MemorySnapshot, Pressure};

use crate::app::{Notice, QUIT_WAIT, Screen, TidyModel, send, terminator};
use crate::format;
use crate::theme::{DisplayType, Theme, figure, metrics, tnum};
use crate::views::dialogs::DialogSpec;
use crate::views::main_window::TidyView;
use crate::views::{KeyStop, glyph_tile, icon_button, icon_button_at, scroll_edge, text_button};
use gpui_kit::base::FocusTrapElement as _;

/// Samples the pressure graph keeps: five minutes at one every 2 seconds.
pub const HISTORY_MAX: usize = 150;
/// Rows of "Top apps by memory".
pub const MAX_ROWS: usize = 15;

/// What the row says after Quit was pressed.
pub const QUIT_REQUESTED: &str = "Quit requested";
pub const STILL_OPEN: &str = "Still open. The app may be asking to save.";
pub const QUIT_REFUSED: &str = "macOS did not pass the request on.";
pub const SYSTEM_NOTE: &str = "Tidy can't read their memory, so they are listed as one row.";
/// What a relaunching row says while it waits for the app to quit.
pub const RELAUNCHING: &str = "Quitting, then opening again";
/// What a row says when its app did not quit in time, so Relaunch left it running.
pub const RELAUNCH_STILL_OPEN: &str = "Didn't quit in 10 seconds, so Tidy left it open.";
/// What a row says once the app was opened again.
pub const REOPENED: &str = "Opened again";
/// What a row says when the app quit but macOS did not open it again.
pub const REOPEN_FAILED: &str = "Quit, but macOS didn't open it again. Open it from the Dock.";

/// How often Relaunch looks whether the app has quit.
pub const RELAUNCH_POLL: std::time::Duration = std::time::Duration::from_millis(250);
/// How long "Opened again" stays on the row.
pub const REOPENED_NOTE: std::time::Duration = std::time::Duration::from_secs(4);

/// A browser shows the tip about its own memory setting above this footprint (4 GB).
pub const BROWSER_TIP_BYTES: u64 = 4_000_000_000;

/// The System row's line when it has a size: what it holds.
pub const SYSTEM_SIZED_NOTE: &str = "Processes that belong to macOS.";

/// The System row's line: the large processes inside it ("WindowServer, kernel_task and
/// more."). `sized` says whether the row has a size (the core read one with `top`); without
/// one the line also says Tidy can't read their memory, so the missing number makes sense.
pub fn system_note(members: &[String], sized: bool) -> String {
    let names = match members {
        [] => None,
        [one] => Some(one.clone()),
        [a, b] => Some(format!("{a} and {b}")),
        [a, b, ..] => Some(format!("{a}, {b} and more")),
    };
    match (names, sized) {
        (None, true) => SYSTEM_SIZED_NOTE.to_string(),
        (None, false) => SYSTEM_NOTE.to_string(),
        (Some(n), true) => format!("{n}."),
        (Some(n), false) if members.len() == 1 => format!("{n}. Tidy can't read its memory."),
        (Some(n), false) => format!("{n}. Tidy can't read their memory."),
    }
}

/// Where a Quit stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitState {
    /// `terminate` was sent; the row says "Quit requested".
    Requested,
    /// 10 seconds went by and the app is still running: the "..." menu offers Force Quit.
    StillOpen,
    /// There was no app to send the request to.
    Refused,
    /// Relaunch: `terminate` was sent; the app opens again once it has quit.
    Relaunching,
    /// Relaunch: the app did not quit within the wait, so it was left running.
    RelaunchStillOpen,
    /// Relaunch: the app quit and was opened again.
    Reopened,
    /// Relaunch: the app quit, but macOS did not open it again.
    ReopenFailed,
}

impl QuitState {
    /// A request is out and the row waits for the app.
    pub fn waiting(self) -> bool {
        matches!(self, QuitState::Requested | QuitState::Relaunching)
    }

    /// A state of Relaunch, which never offers Force Quit and never hides the row.
    pub fn relaunch(self) -> bool {
        matches!(
            self,
            QuitState::Relaunching
                | QuitState::RelaunchStillOpen
                | QuitState::Reopened
                | QuitState::ReopenFailed
        )
    }
}

/// A Quit in progress for one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quit {
    pub state: QuitState,
    /// The pid the request went to, so a different process on the same row is not mistaken
    /// for it.
    pub pid: i32,
}

#[derive(Default)]
pub struct MemoryState {
    /// The latest snapshot, from the Memory screen's polling or from a check up.
    pub snapshot: Option<MemorySnapshot>,
    /// One pressure sample per snapshot taken while the screen was visible. Cleared when the
    /// screen is opened, so the graph covers only the time it has been open.
    pub history: Vec<Pressure>,
    /// Quits in progress, by row id.
    pub quits: HashMap<String, Quit>,
    /// The app bundle each Relaunch opens again, by row id.
    pub relaunch_bundles: HashMap<String, PathBuf>,
    /// Rows whose app has exited; they stay hidden until a snapshot no longer lists them.
    pub exited: HashSet<String>,
    pub icons: HashMap<PathBuf, Arc<gpui_kit::Image>>,
    icons_tried: HashSet<PathBuf>,
    pub notice: Option<Notice>,
    pub notice_leaving: Option<Notice>,
}

impl MemoryState {
    /// The state of a row's Quit, if one is in progress.
    pub fn quit_state(&self, id: &str) -> Option<QuitState> {
        self.quits.get(id).map(|q| q.state)
    }
}

/// Whether the row may be relaunched: it may be quit, and it is an app with a bundle that can be
/// opened again (never a plain process, the System row, Finder or Tidy).
pub fn relaunch_allowed(row: &AppMemory) -> bool {
    quit_allowed(row) && row.kind == MemoryKind::App && row.app_path.is_some()
}

/// The Chromium browsers whose own setting frees memory from tabs that are not in use, by bundle
/// id, with the sentence the row shows when the browser uses more than 4 GB (C34).
pub fn browser_tip(row: &AppMemory) -> Option<&'static str> {
    if row.footprint <= BROWSER_TIP_BYTES || row.kind != MemoryKind::App {
        return None;
    }
    match row.bundle_id.as_deref()? {
        "com.google.Chrome" => Some("Memory Saver, in Settings > Performance, frees idle tabs."),
        "com.microsoft.edgemac" => {
            Some("Sleeping tabs, in Settings > System and performance, free memory.")
        }
        "com.brave.Browser" => Some("Memory Saver, in Settings > System, frees idle tabs."),
        "company.thebrowser.Browser" => {
            Some("Archiving idle tabs sooner, in Settings > Profiles, frees memory.")
        }
        _ => None,
    }
}

/// The "What would help" line when pressure is not Normal (C35): the two largest apps that can
/// be quit, named with their size. None when pressure is Normal.
pub fn help_line(snapshot: &MemorySnapshot, rows: &[MemoryRow]) -> Option<String> {
    if snapshot.pressure == Pressure::Normal {
        return None;
    }
    let names: Vec<String> = rows
        .iter()
        .filter(|r| r.quittable)
        .take(2)
        .map(|r| format!("{} ({})", r.name, format::size(r.bytes)))
        .collect();
    Some(match names.as_slice() {
        [] => "Quit apps you aren't using.".to_string(),
        [one] => format!("Quit or relaunch {one}."),
        [a, b] => format!("Quit or relaunch {a} or {b}."),
        _ => unreachable!("at most two names"),
    })
}

/// Whether the row may be asked to quit at all. Tidy itself (by pid and bundle id), Finder,
/// WindowServer, loginwindow and the System row never can, whatever the core said.
pub fn quit_allowed(row: &AppMemory) -> bool {
    row.quittable
        && row.kind != MemoryKind::System
        && row.pid > 1
        && row.pid != std::process::id() as i32
        && !AppMemory::never_quit(&row.name)
        && !row.bundle_id.as_deref().is_some_and(AppMemory::never_quit)
}

impl TidyModel {
    /// The Memory screen was opened: start a new graph and read memory now.
    pub fn enter_memory(&mut self, cx: &mut Context<Self>) {
        self.memory.history.clear();
        self.snapshot_memory(cx);
    }

    /// Asks the core for a snapshot. Called when the screen opens, by Cmd-R, and every 2
    /// seconds while the screen is visible.
    pub fn snapshot_memory(&mut self, cx: &mut Context<Self>) {
        send(cx, Command::SnapshotMemory);
    }

    pub(crate) fn on_memory_snapshot(&mut self, snapshot: MemorySnapshot, cx: &mut Context<Self>) {
        self.checkup.parts.memory = true;
        let mem = &mut self.memory;
        if self.screen == Screen::Memory {
            mem.history.push(snapshot.pressure);
            if mem.history.len() > HISTORY_MAX {
                mem.history.remove(0);
            }
        }
        let terminator = terminator(cx);
        // A Quit in progress ends when its app has exited or its row has gone.
        let ids: HashSet<&str> = snapshot.apps.iter().map(|a| a.id.as_str()).collect();
        let mut gone = Vec::new();
        let mut cleared = Vec::new();
        for (id, quit) in &mem.quits {
            let row_pid = snapshot.app(id).map(|a| a.pid);
            if quit.state.relaunch() {
                // Relaunch's own timer moves it on; a finished one goes once a new process
                // takes the row. The row is never hidden.
                if quit.state != QuitState::Relaunching && row_pid != Some(quit.pid) {
                    cleared.push(id.clone());
                }
                continue;
            }
            let exited = terminator
                .as_ref()
                .is_some_and(|t| t.is_terminated(quit.pid));
            if row_pid != Some(quit.pid) || exited {
                gone.push((id.clone(), row_pid == Some(quit.pid)));
            }
        }
        for (id, still_listed) in gone {
            mem.quits.remove(&id);
            if still_listed {
                mem.exited.insert(id);
            }
        }
        for id in cleared {
            // "Opened again" stays its own few seconds (its timer clears it).
            if mem.quit_state(&id) != Some(QuitState::Reopened) {
                mem.quits.remove(&id);
                mem.relaunch_bundles.remove(&id);
            }
        }
        mem.exited.retain(|id| ids.contains(id.as_str()));
        if let Some(t) = &terminator {
            for app in snapshot.apps.iter().take(MAX_ROWS * 2) {
                if let Some(path) = &app.app_path
                    && mem.icons_tried.insert(path.clone())
                    && let Some(icon) = t.icon(path)
                {
                    mem.icons.insert(path.clone(), icon);
                }
            }
        }
        mem.snapshot = Some(snapshot);
        cx.notify();
    }

    /// Quit on a row: a polite `terminate` for its main process. The row then says "Quit
    /// requested"; after 10 seconds without an exit it says "Still open".
    pub fn quit_app(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(row) = self
            .memory
            .snapshot
            .as_ref()
            .and_then(|s| s.app(id))
            .cloned()
        else {
            return;
        };
        if !quit_allowed(&row) || self.memory.quit_state(id).is_some_and(QuitState::waiting) {
            return;
        }
        let Some(t) = terminator(cx) else {
            return;
        };
        let state = if t.terminate(row.pid) {
            QuitState::Requested
        } else {
            QuitState::Refused
        };
        self.memory.quits.insert(
            id.to_string(),
            Quit {
                state,
                pid: row.pid,
            },
        );
        if state == QuitState::Requested {
            self.wait_for_quit(id.to_string(), row.pid, cx);
        }
        cx.notify();
    }

    /// Relaunch on a row: a polite `terminate`, then, once the app has quit, its bundle is opened
    /// again. Never a force quit: when the app is still running after [`QUIT_WAIT`] it is left
    /// alone and the row says so.
    pub fn relaunch_app(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(row) = self
            .memory
            .snapshot
            .as_ref()
            .and_then(|s| s.app(id))
            .cloned()
        else {
            return;
        };
        if !relaunch_allowed(&row) || self.memory.quit_state(id).is_some_and(QuitState::waiting) {
            return;
        }
        let Some(t) = terminator(cx) else {
            return;
        };
        // The bundle to open comes from the running app itself, before it quits.
        let bundle = t.bundle_path(row.pid);
        let state = match &bundle {
            Some(_) if t.terminate(row.pid) => QuitState::Relaunching,
            _ => QuitState::Refused,
        };
        self.memory.quits.insert(
            id.to_string(),
            Quit {
                state,
                pid: row.pid,
            },
        );
        if let (QuitState::Relaunching, Some(bundle)) = (state, bundle) {
            self.memory.relaunch_bundles.insert(id.to_string(), bundle);
            self.wait_for_relaunch(id.to_string(), row.pid, cx);
        }
        cx.notify();
    }

    /// Looks every [`RELAUNCH_POLL`] whether the app has quit, for at most [`QUIT_WAIT`].
    fn wait_for_relaunch(&mut self, id: String, pid: i32, cx: &mut Context<Self>) {
        let tries = (QUIT_WAIT.as_millis() / RELAUNCH_POLL.as_millis()).max(1) as u32;
        cx.spawn(async move |this, cx| {
            for n in 1..=tries {
                cx.background_executor().timer(RELAUNCH_POLL).await;
                let last = n == tries;
                let done = this
                    .update(cx, |m, cx| m.relaunch_tick(&id, pid, last, cx))
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// One look at a relaunching app. True when Relaunch is over.
    fn relaunch_tick(&mut self, id: &str, pid: i32, last: bool, cx: &mut Context<Self>) -> bool {
        let current = self.memory.quits.get(id).copied();
        if current
            != Some(Quit {
                state: QuitState::Relaunching,
                pid,
            })
        {
            return true;
        }
        let Some(t) = terminator(cx) else {
            return true;
        };
        let state = if t.is_terminated(pid) {
            let opened = self
                .memory
                .relaunch_bundles
                .get(id)
                .is_some_and(|b| t.open_app(b));
            if opened {
                QuitState::Reopened
            } else {
                QuitState::ReopenFailed
            }
        } else if last {
            QuitState::RelaunchStillOpen
        } else {
            return false;
        };
        self.memory
            .quits
            .insert(id.to_string(), Quit { state, pid });
        if state == QuitState::Reopened {
            let id = id.to_string();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(REOPENED_NOTE).await;
                let _ = this.update(cx, |m, cx| {
                    if m.memory.quit_state(&id) == Some(QuitState::Reopened) {
                        m.memory.quits.remove(&id);
                        m.memory.relaunch_bundles.remove(&id);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
        true
    }

    /// Force Quit, after the confirm. Only a row that is Still open can be force quit.
    pub fn force_quit(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(row) = self
            .memory
            .snapshot
            .as_ref()
            .and_then(|s| s.app(id))
            .cloned()
        else {
            return;
        };
        if !quit_allowed(&row) || self.memory.quit_state(id) != Some(QuitState::StillOpen) {
            return;
        }
        let Some(t) = terminator(cx) else {
            return;
        };
        let state = if t.force_terminate(row.pid) {
            QuitState::Requested
        } else {
            QuitState::Refused
        };
        self.memory.quits.insert(
            id.to_string(),
            Quit {
                state,
                pid: row.pid,
            },
        );
        if state == QuitState::Requested {
            self.wait_for_quit(id.to_string(), row.pid, cx);
        }
        cx.notify();
    }

    /// After [`QUIT_WAIT`], the row either goes (the app exited) or says "Still open".
    fn wait_for_quit(&mut self, id: String, pid: i32, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUIT_WAIT).await;
            let _ = this.update(cx, |m, cx| {
                m.quit_wait_over(&id, pid, cx);
            });
        })
        .detach();
    }

    fn quit_wait_over(&mut self, id: &str, pid: i32, cx: &mut Context<Self>) {
        let Some(quit) = self.memory.quits.get(id).copied() else {
            return;
        };
        if quit.pid != pid || quit.state != QuitState::Requested {
            return;
        }
        let exited = terminator(cx).is_none_or(|t| t.is_terminated(pid));
        if exited {
            self.memory.quits.remove(id);
            self.memory.exited.insert(id.to_string());
        } else {
            self.memory.quits.insert(
                id.to_string(),
                Quit {
                    state: QuitState::StillOpen,
                    pid,
                },
            );
        }
        cx.notify();
    }

    /// Dismisses the notice band.
    pub fn dismiss_memory_notice(&mut self, cx: &mut Context<Self>) {
        self.memory.notice = None;
        self.memory.notice_leaving = None;
        cx.notify();
    }
}

/// One row of "Top apps by memory". Built from the state on every render.
#[derive(Clone)]
pub struct MemoryRow {
    pub id: String,
    pub name: String,
    pub kind: MemoryKind,
    pub bytes: u64,
    pub helpers: u32,
    /// Whether the row has a Quit button.
    pub quittable: bool,
    /// Whether the row has a Relaunch button.
    pub relaunchable: bool,
    /// The browser's own memory setting, for a Chromium browser above 4 GB.
    pub tip: Option<&'static str>,
    pub quit: Option<QuitState>,
    pub icon: Option<Arc<gpui_kit::Image>>,
    /// The System row's process names ([`AppMemory::members`]).
    pub members: Vec<String>,
}

impl MemoryRow {
    /// The line under the name, and whether it is drawn in the warning color.
    pub fn meta(&self) -> (String, bool) {
        match self.quit {
            Some(QuitState::Requested) => (QUIT_REQUESTED.to_string(), false),
            Some(QuitState::StillOpen) => (STILL_OPEN.to_string(), true),
            Some(QuitState::Refused) => (QUIT_REFUSED.to_string(), true),
            Some(QuitState::Relaunching) => (RELAUNCHING.to_string(), false),
            Some(QuitState::RelaunchStillOpen) => (RELAUNCH_STILL_OPEN.to_string(), true),
            Some(QuitState::Reopened) => (REOPENED.to_string(), false),
            Some(QuitState::ReopenFailed) => (REOPEN_FAILED.to_string(), true),
            None if self.tip.is_some() => (self.tip.unwrap_or_default().to_string(), false),
            None => match self.kind {
                MemoryKind::System => (system_note(&self.members, self.bytes > 0), false),
                _ if self.helpers == 0 => (String::new(), false),
                _ if self.helpers == 1 => ("1 helper".to_string(), false),
                _ => (format!("{} helpers", self.helpers), false),
            },
        }
    }

    /// Whether the meta line is the browser tip (drawn with a lightbulb).
    pub fn shows_tip(&self) -> bool {
        self.quit.is_none() && self.tip.is_some()
    }

    /// Whether the Quit button shows: a quittable row that is not already being quit.
    pub fn shows_quit(&self) -> bool {
        self.quittable && !self.quit.is_some_and(QuitState::waiting)
    }

    /// Whether the Relaunch button shows: next to Quit, except while a request is out or while
    /// a Quit waits on an app that may be asking to save.
    pub fn shows_relaunch(&self) -> bool {
        self.relaunchable
            && !self
                .quit
                .is_some_and(|q| q.waiting() || q == QuitState::StillOpen)
    }

    /// Whether the "..." menu (Force Quit) shows.
    pub fn shows_more(&self) -> bool {
        self.quittable && self.quit == Some(QuitState::StillOpen)
    }
}

/// The rows to show: the snapshot's rows by footprint, less the ones whose app has exited.
pub fn memory_rows(m: &MemoryState) -> Vec<MemoryRow> {
    let Some(snapshot) = &m.snapshot else {
        return Vec::new();
    };
    snapshot
        .apps
        .iter()
        .filter(|a| !m.exited.contains(&a.id))
        .take(MAX_ROWS)
        .map(|a| MemoryRow {
            id: a.id.clone(),
            name: a.name.clone(),
            kind: a.kind,
            bytes: a.footprint,
            helpers: a.helpers,
            quittable: quit_allowed(a),
            relaunchable: relaunch_allowed(a),
            tip: browser_tip(a),
            quit: m.quit_state(&a.id),
            icon: a.app_path.as_ref().and_then(|p| m.icons.get(p).cloned()),
            members: a.members.clone(),
        })
        .collect()
}

/// The verdict split in two: "Memory is fine." and the rest.
pub fn verdict_parts(p: Pressure) -> (&'static str, &'static str) {
    let v = p.verdict();
    match v.split_once(". ") {
        Some((first, rest)) => (&v[..first.len() + 1], rest),
        None => (v, ""),
    }
}

/// The one-line explanations under the three figures.
pub const PRESSURE_NOTE: &str = "How hard macOS works to find free memory.";
// They say what each figure is, not whether it is fine, so they never contradict a Warning or
// Critical figure above them. The verdict line says whether it is fine.
pub const SWAP_NOTE: &str = "Memory macOS moved to disk to make room.";
pub const COMPRESSED_NOTE: &str = "Memory macOS compressed to fit more in.";

/// The graph's caption: "Pressure since you opened this screen" and how long that is ("2 min"),
/// or, once the graph is full and drops its oldest bars, "Pressure in the last 5 min".
pub fn graph_caption(samples: usize) -> (String, String) {
    let span = graph_span(samples);
    if samples >= HISTORY_MAX {
        (format!("Pressure in the last {span}"), String::new())
    } else {
        ("Pressure since you opened this screen".to_string(), span)
    }
}

/// How long `samples` readings cover: "40 s", "2 min".
fn graph_span(samples: usize) -> String {
    let secs = samples as u64 * crate::app::MEMORY_PERIOD.as_secs();
    if secs < 60 {
        format!("{secs} s")
    } else {
        format!("{} min", (secs + 30) / 60)
    }
}

/// The height of a pressure bar as a share of the graph.
pub fn bar_share(p: Pressure) -> f32 {
    match p {
        Pressure::Normal => 0.28,
        Pressure::Warning => 0.62,
        Pressure::Critical => 1.0,
    }
}

/// The "..." menu of a row: for which row, and where it was clicked.
#[derive(Clone, Debug)]
pub struct MemoryMenu {
    pub id: String,
    pub name: String,
    pub at: Point<Pixels>,
}

/// What the Memory screen draws, copied out of the model.
pub(crate) struct MemoryView {
    pub(crate) snapshot: Option<MemorySnapshot>,
    pub(crate) history: Vec<Pressure>,
    pub(crate) rows: Rc<Vec<MemoryRow>>,
    pub(crate) notice: Option<Notice>,
    pub(crate) notice_leaving: Option<Notice>,
}

impl TidyView {
    /// Opens the "..." menu of a Memory row.
    pub fn open_memory_menu(
        &mut self,
        id: String,
        name: String,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        self.menu = None;
        self.memory_menu = Some(MemoryMenu { id, name, at });
        cx.notify();
    }

    /// "Force Quit…" in the menu: asks first, because unsaved changes are lost.
    fn request_force_quit(&mut self, cx: &mut Context<Self>) {
        let Some(menu) = self.memory_menu.take() else {
            return;
        };
        self.open_dialog(DialogSpec::force_quit(&menu.id, &menu.name), cx);
    }

    pub(crate) fn render_memory_menu(
        &mut self,
        t: &Theme,
        menu: &MemoryMenu,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let close = view.clone();
        let hover = t.row_hover;
        let on_menu = t.on(t.pop_bg);
        let card = div()
            .debug_selector(|| "memory-menu".to_string())
            .min_w(px(160.))
            .p(px(4.))
            .rounded(px(8.))
            .bg(t.pop_bg)
            .shadow(t.pop_shadow())
            .flex()
            .flex_col()
            .child(
                div()
                    .id("memory-menu-force")
                    .debug_selector(|| "memory-force-quit".to_string())
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(px(5.))
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .text_color(t.text)
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child("Force Quit\u{2026}")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        let _ = view.update(cx, |v, cx| v.request_force_quit(cx));
                    })
                    .key_stop(&on_menu),
            )
            .focus_trap("memory-menu-trap", &self.traps.menu);
        div()
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("memory-menu-scrim")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let _ = close.update(cx, |v, cx| {
                            v.memory_menu = None;
                            cx.notify();
                        });
                    }),
            )
            .child(deferred(
                anchored()
                    .position(menu.at)
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            ))
            .into_any_element()
    }

    pub(crate) fn memory_snapshot_view(&self, cx: &App) -> MemoryView {
        let m = &self.model.read(cx).memory;
        MemoryView {
            snapshot: m.snapshot.clone(),
            history: m.history.clone(),
            rows: Rc::new(memory_rows(m)),
            notice: m.notice.clone(),
            notice_leaving: m.notice_leaving.clone(),
        }
    }

    /// The whole Memory screen: toolbar, verdict, figures, graph, list, status bar.
    pub(crate) fn render_memory(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.memory_snapshot_view(cx);
        let scrolled = self.memory_scroll.0.borrow().base_handle.offset().y < px(0.);
        let model = self.model.clone();
        let view = cx.entity().downgrade();

        let controls = {
            let refresh = model.clone();
            icon_button(
                "memory-refresh",
                "memory-refresh".to_string(),
                IconName::RefreshCw,
                t,
                move |_, cx| {
                    refresh.update(cx, |m, cx| m.snapshot_memory(cx));
                },
            )
        };
        // The figures stay put above the list; the scrolled edge is under the list header.
        let toolbar = self.toolbar(t, "Memory", false, controls, cx);

        let shown = snap
            .notice
            .as_ref()
            .map(|n| (n, false))
            .or(snap.notice_leaving.as_ref().map(|n| (n, true)));
        let notice = shown.map(|(n, leaving)| {
            let model = model.clone();
            self.notice_band(
                t,
                n,
                leaving,
                false,
                move |_, cx| {
                    model.update(cx, |m, cx| m.dismiss_memory_notice(cx));
                },
                cx,
            )
        });

        let body: AnyElement = match &snap.snapshot {
            None => div()
                .debug_selector(|| "memory-loading".to_string())
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .text_color(t.text_2)
                .child("Reading memory")
                .into_any_element(),
            Some(s) => {
                let rows = snap.rows.clone();
                let env_model = model.clone();
                let env_view = view.clone();
                let count = rows.len();
                let list = uniform_list("memory-rows", count, move |range, window, _cx| {
                    let t = Theme::of(window);
                    range
                        .filter_map(|ix| {
                            rows.get(ix)
                                .map(|row| render_row(row, ix, &t, &env_model, &env_view))
                        })
                        .collect::<Vec<AnyElement>>()
                })
                .track_scroll(&self.memory_scroll)
                .flex_1()
                .px(px(metrics::LIST_INSET))
                .pt(px(2.))
                .pb(px(8.));
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(verdict_strip(t, s))
                    .child(figures(t, s))
                    .child(graph(t, &snap.history))
                    .children(help_line(s, &snap.rows).map(|line| help(t, line)))
                    .child(list_header(t))
                    .child(scroll_edge(t, scrolled))
                    .child(self.list_frame(list))
                    .into_any_element()
            }
        };

        let left = match &snap.snapshot {
            Some(s) if s.total_bytes > 0 => format!(
                "{} of memory, updated every 2 seconds",
                format::size(s.total_bytes)
            ),
            _ => String::new(),
        };
        let status = self.status_bar(t, left, None);

        div()
            .debug_selector(|| "memory".to_string())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .children(notice)
            .child(body)
            .child(status)
            .into_any_element()
    }
}

/// 64 pt: the first sentence of the verdict as the Figure, the rest under it.
fn verdict_strip(t: &Theme, s: &MemorySnapshot) -> Div {
    let (first, rest) = verdict_parts(s.pressure);
    div()
        .debug_selector(|| "memory-verdict".to_string())
        .flex_none()
        .h(px(metrics::SUMMARY_H))
        .px(px(20.))
        .flex()
        .flex_col()
        .justify_center()
        .child(
            div()
                .debug_selector(|| "memory-verdict-first".to_string())
                .text_size(px(figure::SIZE))
                .line_height(px(figure::LINE))
                .display(FontWeight::BOLD)
                // In `text`, as on the other screens: the Pressure figure and the graph carry
                // the color.
                .text_color(t.text)
                .whitespace_nowrap()
                .child(first),
        )
        .child(
            div()
                .debug_selector(|| "memory-verdict-rest".to_string())
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(rest),
        )
}

/// The three figures: Pressure, Swap used, Compressed.
fn figures(t: &Theme, s: &MemorySnapshot) -> Div {
    let pressure_color = match s.pressure {
        Pressure::Normal => t.text,
        Pressure::Warning => t.warning,
        Pressure::Critical => t.error,
    };
    div()
        .flex_none()
        .px(px(20.))
        .pt(px(4.))
        .pb(px(12.))
        .flex()
        .gap(px(12.))
        .child(figure_card(
            t,
            "memory-figure-pressure",
            "Pressure",
            s.pressure.label().to_string(),
            pressure_color,
            PRESSURE_NOTE,
        ))
        .child(figure_card(
            t,
            "memory-figure-swap",
            "Swap used",
            format::size(s.swap.used),
            t.text,
            SWAP_NOTE,
        ))
        .child(figure_card(
            t,
            "memory-figure-compressed",
            "Compressed",
            format::size(s.compressed_bytes),
            t.text,
            COMPRESSED_NOTE,
        ))
}

fn figure_card(
    t: &Theme,
    selector: &'static str,
    label: &'static str,
    value: String,
    color: Hsla,
    note: &'static str,
) -> Div {
    div()
        .debug_selector(move || selector.to_string())
        .flex_1()
        .min_w_0()
        .px(px(12.))
        .py(px(10.))
        .rounded(px(10.))
        .bg(t.card_bg())
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .display(FontWeight::SEMIBOLD)
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(label),
        )
        .child(
            div()
                .debug_selector(move || format!("{selector}-value"))
                .text_size(px(figure::SIZE))
                .line_height(px(figure::LINE))
                .display(FontWeight::BOLD)
                .font_features(tnum())
                .text_color(color)
                .whitespace_nowrap()
                .child(value),
        )
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(t.text_2)
                .child(note),
        )
}

/// The pressure history: one bar per sample, taller and warmer as pressure rises.
fn graph(t: &Theme, history: &[Pressure]) -> Div {
    const H: f32 = 36.;
    let (caption, span) = graph_caption(history.len());
    div()
        .debug_selector(|| "memory-graph".to_string())
        .flex_none()
        .mx(px(20.))
        .mb(px(8.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(10.))
        .bg(t.card_bg())
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .justify_between()
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(t.text_2)
                .child(div().display(FontWeight::SEMIBOLD).child(caption))
                .child(div().font_features(tnum()).child(span)),
        )
        .child(
            div()
                .h(px(H))
                .w_full()
                .flex()
                .items_end()
                .gap(px(2.))
                .children(history.iter().map(|p| {
                    div()
                        .flex_1()
                        .max_w(px(10.))
                        .h(relative(bar_share(*p)))
                        .rounded(px(1.5))
                        .bg(match p {
                            Pressure::Normal => t.success_badge,
                            Pressure::Warning => t.warning_badge,
                            Pressure::Critical => t.error_badge,
                        })
                })),
        )
}

/// The trailing column of a row: the "..." menu, Relaunch and Quit.
const ACTIONS_W: f32 = 168.;

/// "What would help", under the graph while pressure is not Normal: the largest apps to quit
/// or relaunch, in the screen's own words. Plain text, no card: the verdict and the Pressure
/// figure already carry the color.
fn help(t: &Theme, line: String) -> Div {
    div()
        .debug_selector(|| "memory-help".to_string())
        .flex_none()
        .px(px(20.))
        .pt(px(2.))
        .pb(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(12.))
        .line_height(px(16.))
        .child(
            Icon::new(IconName::Lightbulb)
                .size(px(13.))
                .text_color(t.text_2)
                .flex_none(),
        )
        .child(
            div()
                .flex_none()
                .display(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .whitespace_nowrap()
                .child("What would help"),
        )
        .child(
            div()
                .debug_selector(|| "memory-help-text".to_string())
                .flex_1()
                .min_w_0()
                .text_color(t.text_2)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .font_features(tnum())
                .child(line),
        )
}

fn list_header(t: &Theme) -> Div {
    div()
        .flex_none()
        .h(px(28.))
        .px(px(20.))
        .flex()
        .items_center()
        .justify_between()
        .text_size(px(12.))
        .text_color(t.text_2)
        .child(
            div()
                .text_size(px(13.))
                .display(FontWeight::BOLD)
                .text_color(t.text)
                .child("Top apps by memory"),
        )
        // Lines the label's right edge up with the sizes: the row's trailing column, its
        // padding and the list inset.
        .child(div().pr(px(ACTIONS_W + 6.)).child("Memory in use"))
}

fn render_row(
    row: &MemoryRow,
    ix: usize,
    t: &Theme,
    model: &Entity<TidyModel>,
    view: &WeakEntity<TidyView>,
) -> AnyElement {
    let id = row.id.clone();
    let (meta, warn) = row.meta();
    let tile: AnyElement = match &row.icon {
        Some(image) => img(image.clone())
            .flex_none()
            .size(px(28.))
            .into_any_element(),
        None => glyph_tile(
            match row.kind {
                MemoryKind::App => IconName::AppWindow,
                MemoryKind::Process => IconName::SquareTerminal,
                MemoryKind::System => IconName::Cpu,
            },
            28.,
            6.,
            16.,
            t,
        )
        .into_any_element(),
    };
    let quit = row.shows_quit().then(|| {
        let model = model.clone();
        let quit_id = id.clone();
        text_button(
            ("memory-quit", ix),
            format!("memory-quit:{id}"),
            "Quit",
            false,
            true,
            t,
            move |_, cx| {
                cx.stop_propagation();
                model.update(cx, |m, cx| m.quit_app(&quit_id, cx));
            },
        )
    });
    let relaunch = row.shows_relaunch().then(|| {
        let model = model.clone();
        let relaunch_id = id.clone();
        text_button(
            ("memory-relaunch", ix),
            format!("memory-relaunch:{id}"),
            "Relaunch",
            false,
            true,
            t,
            move |_, cx| {
                cx.stop_propagation();
                model.update(cx, |m, cx| m.relaunch_app(&relaunch_id, cx));
            },
        )
    });
    let more = row.shows_more().then(|| {
        let view = view.clone();
        let (more_id, more_name) = (id.clone(), row.name.clone());
        icon_button_at(
            ("memory-more", ix),
            format!("memory-more:{id}"),
            IconName::Ellipsis,
            t,
            move |at, _, cx| {
                let (id, name) = (more_id.clone(), more_name.clone());
                let _ = view.update(cx, |v, cx| v.open_memory_menu(id, name, at, cx));
            },
        )
    });
    let selector_id = id.clone();
    let meta_id = id.clone();
    div()
        .id(("memory-row", ix))
        .debug_selector(move || format!("memory-row:{selector_id}"))
        .w_full()
        .h(px(metrics::CLEANUP_ROW_H))
        .px(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .child(tile)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(13.))
                        .line_height(px(17.))
                        .display(FontWeight::SEMIBOLD)
                        .text_color(t.text)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.name.clone()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .min_w_0()
                        .when(row.shows_tip(), |d| {
                            d.child(
                                Icon::new(IconName::Lightbulb)
                                    .size(px(12.))
                                    .text_color(t.text_2)
                                    .flex_none(),
                            )
                        })
                        .child(
                            div()
                                .debug_selector(move || format!("memory-meta:{meta_id}"))
                                .min_w_0()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .text_color(if warn { t.warning } else { t.text_2 })
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(meta),
                        ),
                ),
        )
        .child(
            div()
                .flex_none()
                .w(px(76.))
                .text_right()
                .text_size(px(13.))
                .display(FontWeight::SEMIBOLD)
                .font_features(tnum())
                .text_color(t.text)
                .whitespace_nowrap()
                // A System row without a size (`top` gave none) shows no figure rather than
                // "0 KB".
                .child(if row.kind == MemoryKind::System && row.bytes == 0 {
                    String::new()
                } else {
                    format::size(row.bytes)
                }),
        )
        .child(
            div()
                .flex_none()
                .w(px(ACTIONS_W))
                .flex()
                .items_center()
                .justify_end()
                .gap(px(6.))
                .children(more)
                .children(relaunch)
                .children(quit),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn app(id: &str, name: &str, pid: i32, bytes: u64, helpers: u32) -> AppMemory {
        AppMemory {
            id: id.to_string(),
            name: name.to_string(),
            kind: MemoryKind::App,
            pid,
            pids: vec![pid],
            bundle_id: None,
            app_path: None,
            footprint: bytes,
            helpers,
            quittable: true,
            members: Vec::new(),
        }
    }

    fn state(apps: Vec<AppMemory>) -> MemoryState {
        MemoryState {
            snapshot: Some(MemorySnapshot {
                apps,
                ..MemorySnapshot::default()
            }),
            ..MemoryState::default()
        }
    }

    #[test]
    fn the_verdict_splits_into_a_headline_and_a_line() {
        assert_eq!(
            verdict_parts(Pressure::Normal),
            (
                "Memory is fine.",
                "macOS is using spare memory for caching, which is normal."
            )
        );
        assert_eq!(
            verdict_parts(Pressure::Warning),
            ("Memory is tight.", "Quitting a large app below will help.")
        );
        assert_eq!(verdict_parts(Pressure::Critical).0, "Memory is very tight.");
    }

    #[test]
    fn rows_say_helpers_then_the_quit_state() {
        let m = state(vec![
            app("app:/A.app", "A", 10, 4_200_000_000, 3),
            app("app:/B.app", "B", 11, 100, 1),
            app("app:/C.app", "C", 12, 50, 0),
        ]);
        let rows = memory_rows(&m);
        assert_eq!(rows[0].meta(), ("3 helpers".to_string(), false));
        assert_eq!(rows[1].meta(), ("1 helper".to_string(), false));
        assert_eq!(rows[2].meta(), (String::new(), false));
        assert!(rows[0].shows_quit() && !rows[0].shows_more());

        let mut r = rows[0].clone();
        r.quit = Some(QuitState::Requested);
        assert_eq!(r.meta(), ("Quit requested".to_string(), false));
        assert!(!r.shows_quit() && !r.shows_more());
        r.quit = Some(QuitState::StillOpen);
        assert_eq!(r.meta(), (STILL_OPEN.to_string(), true));
        assert!(r.shows_quit() && r.shows_more());
    }

    #[test]
    fn protected_rows_have_no_quit_button() {
        let mut finder = app("app:/Finder.app", "Finder", 300, 1, 0);
        let mut tidy = app("app:/Tidy.app", "Tidy", 301, 1, 0);
        tidy.bundle_id = Some("dev.sdusteric.tidy".into());
        let ws = AppMemory {
            kind: MemoryKind::Process,
            ..app("pid:99", "WindowServer", 99, 1, 0)
        };
        let login = AppMemory {
            kind: MemoryKind::Process,
            ..app("pid:98", "loginwindow", 98, 1, 0)
        };
        let system = AppMemory {
            kind: MemoryKind::System,
            pid: 0,
            pids: vec![],
            ..app("system", "System processes", 0, 9, 0)
        };
        let mine = app("app:/Me.app", "Me", std::process::id() as i32, 1, 0);
        // The core already clears `quittable`, but the app does not rely on that.
        finder.quittable = true;
        for row in [&finder, &tidy, &ws, &login, &system, &mine] {
            assert!(!quit_allowed(row), "{}", row.name);
        }
        let fine = app("app:/Fine.app", "Fine", 500, 1, 0);
        assert!(quit_allowed(&fine));
        let mut by_core = fine.clone();
        by_core.quittable = false;
        assert!(!quit_allowed(&by_core));
    }

    #[test]
    fn exited_apps_leave_the_list() {
        let mut m = state(vec![
            app("app:/A.app", "A", 10, 3, 0),
            app("app:/B.app", "B", 11, 2, 0),
        ]);
        m.exited.insert("app:/A.app".into());
        let ids: Vec<String> = memory_rows(&m).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["app:/B.app"]);
    }

    #[test]
    fn the_list_is_capped_and_pressure_bars_rise() {
        let apps = (0..40)
            .map(|i| app(&format!("pid:{i}"), "x", 100 + i, 1, 0))
            .collect();
        assert_eq!(memory_rows(&state(apps)).len(), MAX_ROWS);
        assert!(bar_share(Pressure::Normal) < bar_share(Pressure::Warning));
        assert!(bar_share(Pressure::Warning) < bar_share(Pressure::Critical));
    }

    #[test]
    fn relaunch_states_have_their_own_lines_and_buttons() {
        let m = state(vec![app("app:/A.app", "A", 10, 1, 2)]);
        let mut r = memory_rows(&m)[0].clone();
        // `app` rows have no bundle path, so they cannot be relaunched.
        assert!(!r.relaunchable && !r.shows_relaunch());
        r.relaunchable = true;
        assert!(r.shows_relaunch() && r.shows_quit());
        for (q, line, warn, relaunch, quit) in [
            (QuitState::Relaunching, RELAUNCHING, false, false, false),
            (
                QuitState::RelaunchStillOpen,
                RELAUNCH_STILL_OPEN,
                true,
                true,
                true,
            ),
            (QuitState::Reopened, REOPENED, false, true, true),
            (QuitState::ReopenFailed, REOPEN_FAILED, true, true, true),
            (QuitState::Requested, QUIT_REQUESTED, false, false, false),
            (QuitState::StillOpen, STILL_OPEN, true, false, true),
        ] {
            r.quit = Some(q);
            assert_eq!(r.meta(), (line.to_string(), warn), "{q:?}");
            assert_eq!(r.shows_relaunch(), relaunch, "{q:?} relaunch");
            assert_eq!(r.shows_quit(), quit, "{q:?} quit");
            // Relaunch never leads to Force Quit.
            assert_eq!(r.shows_more(), q == QuitState::StillOpen, "{q:?} more");
        }
    }

    #[test]
    fn only_real_apps_that_may_be_quit_can_be_relaunched() {
        let mut a = app("app:/A.app", "A", 500, 1, 0);
        assert!(!relaunch_allowed(&a), "no bundle path");
        a.app_path = Some(PathBuf::from("/Applications/A.app"));
        assert!(relaunch_allowed(&a));
        let mut process = a.clone();
        process.kind = MemoryKind::Process;
        assert!(!relaunch_allowed(&process));
        let mut finder = a.clone();
        finder.name = "Finder".into();
        assert!(!relaunch_allowed(&finder));
        let mut me = a.clone();
        me.pid = std::process::id() as i32;
        assert!(!relaunch_allowed(&me));
        let mut tidy = a.clone();
        tidy.bundle_id = Some("dev.sdusteric.tidy".into());
        assert!(!relaunch_allowed(&tidy));
    }

    #[test]
    fn chromium_browsers_above_4_gb_get_their_own_setting_as_a_tip() {
        let browser = |bundle: &str, bytes: u64| AppMemory {
            bundle_id: Some(bundle.to_string()),
            ..app("app:/B.app", "B", 10, bytes, 9)
        };
        for (id, words) in [
            (
                "com.google.Chrome",
                "Memory Saver, in Settings > Performance",
            ),
            (
                "com.microsoft.edgemac",
                "in Settings > System and performance",
            ),
            ("com.brave.Browser", "Memory Saver, in Settings > System"),
            ("company.thebrowser.Browser", "in Settings > Profiles"),
        ] {
            let tip = browser_tip(&browser(id, 4_200_000_000)).expect(id);
            assert!(tip.contains(words), "{tip}");
            assert!(tip.len() <= 66, "{tip} is {} long", tip.len());
            // At or under 4 GB, nothing.
            assert_eq!(browser_tip(&browser(id, 4_000_000_000)), None);
        }
        assert_eq!(
            browser_tip(&browser("com.apple.Safari", 9_000_000_000)),
            None
        );
        assert_eq!(
            browser_tip(&browser("org.mozilla.firefox", 9_000_000_000)),
            None
        );
        // The tip replaces the helper count, and a Quit state replaces the tip.
        let m = state(vec![browser("com.google.Chrome", 5_600_000_000)]);
        let mut row = memory_rows(&m)[0].clone();
        assert!(row.shows_tip());
        assert_eq!(
            row.meta().0,
            browser_tip(&m.snapshot.as_ref().unwrap().apps[0]).unwrap()
        );
        row.quit = Some(QuitState::Requested);
        assert!(!row.shows_tip());
        assert_eq!(row.meta().0, QUIT_REQUESTED);
    }

    #[test]
    fn what_would_help_names_the_two_largest_apps_only_under_pressure() {
        let mut m = state(vec![
            app("app:/C.app", "Google Chrome", 10, 5_600_000_000, 1),
            app("app:/X.app", "Xcode", 11, 4_300_000_000, 1),
            app("app:/S.app", "Slack", 12, 1_400_000_000, 1),
        ]);
        let snap = |m: &MemoryState| m.snapshot.clone().unwrap();
        assert_eq!(help_line(&snap(&m), &memory_rows(&m)), None);
        m.snapshot.as_mut().unwrap().pressure = Pressure::Warning;
        assert_eq!(
            help_line(&snap(&m), &memory_rows(&m)).unwrap(),
            "Quit or relaunch Google Chrome (5.6 GB) or Xcode (4.3 GB)."
        );
        // Rows that cannot be quit are passed over.
        m.snapshot.as_mut().unwrap().apps[0].quittable = false;
        m.snapshot.as_mut().unwrap().pressure = Pressure::Critical;
        assert!(
            help_line(&snap(&m), &memory_rows(&m))
                .unwrap()
                .starts_with("Quit or relaunch Xcode (4.3 GB) or Slack (1.4 GB).")
        );
        let none = state(vec![]);
        let mut s = snap(&none);
        s.pressure = Pressure::Warning;
        assert_eq!(help_line(&s, &[]).unwrap(), "Quit apps you aren't using.");
    }

    #[test]
    fn copy_has_no_em_dashes_or_scare_words() {
        for s in [
            RELAUNCHING,
            RELAUNCH_STILL_OPEN,
            REOPENED,
            REOPEN_FAILED,
            PRESSURE_NOTE,
            SWAP_NOTE,
            COMPRESSED_NOTE,
            STILL_OPEN,
            QUIT_REFUSED,
            SYSTEM_NOTE,
            SYSTEM_SIZED_NOTE,
            QUIT_REQUESTED,
        ] {
            assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'), "{s}");
        }
    }

    #[test]
    fn the_graph_caption_says_how_long_it_covers() {
        assert_eq!(
            graph_caption(20),
            (
                "Pressure since you opened this screen".to_string(),
                "40 s".to_string()
            )
        );
        assert_eq!(graph_caption(60).1, "2 min");
        assert_eq!(
            graph_caption(HISTORY_MAX),
            ("Pressure in the last 5 min".to_string(), String::new())
        );
    }

    /// The System row's line names the large processes inside, WindowServer first as the core
    /// orders them. When the row has no size (`top` gave none) the line says why.
    #[test]
    fn the_system_row_names_the_processes_it_holds() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(system_note(&[], false), SYSTEM_NOTE);
        assert_eq!(system_note(&[], true), SYSTEM_SIZED_NOTE);
        assert_eq!(
            system_note(&names(&["WindowServer"]), false),
            "WindowServer. Tidy can't read its memory."
        );
        assert_eq!(
            system_note(&names(&["WindowServer", "kernel_task"]), false),
            "WindowServer and kernel_task. Tidy can't read their memory."
        );
        assert_eq!(
            system_note(&names(&["WindowServer", "kernel_task", "launchd"]), false),
            "WindowServer, kernel_task and more. Tidy can't read their memory."
        );
        assert_eq!(
            system_note(&names(&["WindowServer", "kernel_task", "launchd"]), true),
            "WindowServer, kernel_task and more."
        );
        assert_eq!(
            system_note(&names(&["WindowServer"]), true),
            "WindowServer."
        );
        let system = AppMemory {
            kind: MemoryKind::System,
            pid: 0,
            pids: vec![622, 1],
            members: names(&["WindowServer", "launchd"]),
            ..app("system", "System processes", 0, 0, 2)
        };
        let rows = memory_rows(&state(vec![system]));
        assert_eq!(
            rows[0].meta(),
            (
                "WindowServer and launchd. Tidy can't read their memory.".to_string(),
                false
            )
        );
        // With the size `top` gave, the row shows it and the line only names what is inside.
        let sized = AppMemory {
            kind: MemoryKind::System,
            pid: 0,
            pids: vec![622, 1],
            members: names(&["WindowServer", "launchd"]),
            ..app("system", "System processes", 0, 1_500_000_000, 2)
        };
        let rows = memory_rows(&state(vec![sized]));
        assert_eq!(rows[0].bytes, 1_500_000_000);
        assert!(!rows[0].quittable);
        assert_eq!(
            rows[0].meta(),
            ("WindowServer and launchd.".to_string(), false)
        );
    }
}
