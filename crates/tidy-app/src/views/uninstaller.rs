//! The Uninstaller screen (design section 19): the apps in the application folders, and for
//! the app the user opens, every file it left in the Library with its size, ticked or not,
//! before anything moves. Uninstall always asks first with the receipt (C55), and everything
//! goes to the Trash through the core, which looks for the files again right before the move.
//!
//! The list is read when the screen opens (reading `/Applications` asks macOS for nothing).
//! An app's leftovers are looked for only when the user opens it (C48). While the open app or
//! one of its helpers runs, Uninstall is off and the process list is read again every 3
//! seconds (`CheckRunning`), so the button comes back once the app has quit.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::api::Command;
use tidy_core::model::{HistoryEntry, ItemResult, RunningSet, UseSource};
use tidy_core::uninstall::{AppEntry, AppList, Leftover, LeftoverKind, UninstallPlan};

use crate::app::{
    Notice, OpenKind, Progress, STALE_SCAN_SECS, TidyModel, Tri, now_unix, open_path, send,
    terminator,
};
use crate::format;
use crate::motion;
use crate::theme::motion::NOTE_OUT;
use crate::theme::{DisplayType, MONO_FONT, Theme, figure, metrics, motion as m, tnum};
use crate::views::dialogs::DialogSpec;
use crate::views::large::mono_char_width;
use crate::views::main_window::TidyView;
use crate::views::{
    KeyStop, Segment, checkbox, glyph_tile, icon_button, row_action, segmented, tag, text_button,
};

/// The line under the list: what is not listed.
pub const LEFT_OUT_NOTE: &str = "Tidy doesn't list Apple's apps or itself";
/// The empty state's title and body, shown when listing failed or found nothing.
pub const EMPTY_TITLE: &str = "No apps to list";
pub const EMPTY_BODY: &str =
    "Apps in your Applications folders show up here. Tidy doesn't list Apple's apps or itself.";
/// The body while the first list is read.
pub const LISTING_TITLE: &str = "Listing your apps";
pub const LISTING_BODY: &str = "Tidy reads the Applications folders and measures each app.";
/// The notice when an uninstall moved nothing.
pub const NOTHING_MOVED: &str = "Tidy moved nothing. Each row says why.";
/// The notice when the open app is no longer listed.
pub const APP_GONE: &str = "That app isn't in the Applications folders anymore.";
/// The receipt note when a launch agent is ticked (C53).
pub const LAUNCH_AGENT_NOTE: &str = "Launch agents stop running after you log out or restart.";

/// How many app icons are read per frame while the list fills in.
const ICONS_PER_STEP: usize = 8;
const ICON_STEP: Duration = Duration::from_millis(16);

/// The width of everything in a leftover row except the text column: padding (20), checkbox
/// (16), tile (28), size column (90), trailing action (60) and the gaps (40).
const ROW_FIXED_W: f32 = 254.;

/// How the app list is sorted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppSort {
    #[default]
    Name,
    Size,
    /// What was not used first: apps with no open on record (their file date, oldest first),
    /// then the apps opened longest ago.
    LastOpened,
}

#[derive(Default)]
pub struct UninstallState {
    pub list: Option<AppList>,
    pub listing: bool,
    pub sort: AppSort,
    /// The app whose files the screen shows, if one is open.
    pub open: Option<PathBuf>,
    /// The open app's files, once found.
    pub plan: Option<UninstallPlan>,
    /// Tidy is looking for the open app's files.
    pub looking: bool,
    /// Ticked rows, by id (the path).
    pub ticked: BTreeSet<String>,
    /// "Quit Slack first" while the app or a helper runs.
    pub blocked: Option<String>,
    pub skipped: HashMap<String, String>,
    pub leaving: HashSet<String>,
    pub gone: HashSet<String>,
    /// Apps moved to the Trash, by path, left out of the list until the next listing.
    pub removed: HashSet<PathBuf>,
    pub removing: Option<Progress>,
    pub notice: Option<Notice>,
    pub notice_leaving: Option<Notice>,
    pub icons: HashMap<PathBuf, Arc<gpui_kit::Image>>,
    icons_tried: HashSet<PathBuf>,
    /// Counts the looks, so a late answer for an app that was closed is dropped.
    look: u64,
    /// When the open app's files arrived: the rows fade in over `CARD_IN` right after.
    arrived: Option<std::time::Instant>,
}

impl UninstallState {
    /// The listed apps in the chosen order, without the ones just moved to the Trash.
    pub fn apps(&self) -> Vec<&AppEntry> {
        let Some(list) = &self.list else {
            return Vec::new();
        };
        let mut apps: Vec<&AppEntry> = list
            .apps
            .iter()
            .filter(|a| !self.removed.contains(&a.path))
            .collect();
        match self.sort {
            AppSort::Name => {}
            AppSort::Size => {
                apps.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
            }
            AppSort::LastOpened => apps.sort_by_key(|a| {
                (
                    a.used_source == UseSource::Spotlight,
                    a.last_used.unwrap_or(0),
                    a.name.to_lowercase(),
                )
            }),
        }
        apps
    }

    /// The open app's rows that are still there.
    pub fn rows(&self) -> Vec<&Leftover> {
        self.plan
            .iter()
            .flat_map(|p| p.items.iter())
            .filter(|i| !self.gone.contains(&i.id))
            .collect()
    }

    /// The ticked rows in list order.
    pub fn ticked_rows(&self) -> Vec<&Leftover> {
        self.rows()
            .into_iter()
            .filter(|i| i.selectable && self.ticked.contains(&i.id))
            .collect()
    }

    pub fn ticked_bytes(&self) -> u64 {
        self.ticked_rows()
            .iter()
            .map(|i| i.bytes.unwrap_or(0))
            .sum()
    }

    /// Whether an event about `id` belongs to the running uninstall.
    pub fn is_removing(&self, id: &str) -> bool {
        self.removing.is_some() && self.plan.as_ref().is_some_and(|p| p.item(id).is_some())
    }

    pub fn on_trashed(&mut self, id: &str) {
        self.ticked.remove(id);
        self.skipped.remove(id);
        self.leaving.insert(id.to_string());
    }

    pub fn on_skipped(&mut self, id: String, reason: String) {
        self.leaving.remove(&id);
        self.ticked.remove(&id);
        self.skipped.insert(id, reason);
    }

    /// A new process list: the open app may have quit, or opened.
    pub fn on_running(&mut self, set: &RunningSet) {
        if let Some(plan) = &self.plan {
            self.blocked = plan.app.owner().blocked_by(set);
        }
    }

    /// The open app is running, so the process list is read again every 3 seconds.
    pub fn watches_running(&self) -> bool {
        self.open.is_some() && self.blocked.is_some() && self.removing.is_none()
    }

    pub fn can_uninstall(&self) -> bool {
        self.plan.is_some()
            && !self.looking
            && self.blocked.is_none()
            && self.removing.is_none()
            && !self.ticked_rows().is_empty()
    }

    /// The toolbar button: "Uninstall…", or "Moving 3 of 10" while it runs.
    pub fn button_label(&self) -> String {
        match self.removing {
            Some((done, total)) => format!("Moving {done} of {total}"),
            None => "Uninstall\u{2026}".to_string(),
        }
    }

    /// The status bar of the open app: "10 of 11 selected, 1.4 GB".
    pub fn detail_status(&self) -> String {
        let selectable = self.rows().iter().filter(|i| i.selectable).count();
        if self.plan.is_none() {
            return String::new();
        }
        format!(
            "{} of {} selected, {}",
            self.ticked_rows().len(),
            selectable,
            format::size(self.ticked_bytes())
        )
    }

    /// The status bar of the list: "84 apps, 31.2 GB · Apple's apps and Tidy aren't listed".
    pub fn list_status(&self) -> String {
        if self.list.is_none() {
            return String::new();
        }
        let apps = self.apps();
        let bytes: u64 = apps.iter().map(|a| a.bytes).sum();
        let count = match apps.len() {
            1 => "1 app".to_string(),
            n => format!("{} apps", format::thousands(n as u64)),
        };
        format!("{count}, {} \u{b7} {LEFT_OUT_NOTE}", format::size(bytes))
    }
}

/// The meta line of an app row: "Version 4.41 · Slack Technologies · Opened 21 days ago".
pub fn app_meta(app: &AppEntry, now: u64) -> String {
    let mut parts = Vec::new();
    if let Some(v) = app.version.as_deref().filter(|v| !v.is_empty()) {
        parts.push(format!("Version {v}"));
    }
    if let Some(team) = &app.team_name {
        parts.push(team.clone());
    }
    if app.last_used.is_some() {
        parts.push(format::use_phrase(app.last_used, app.used_source, now));
    }
    parts.join(" \u{b7} ")
}

/// The name shown on a row: the app's file name for the app itself, the kind otherwise.
pub fn row_label(item: &Leftover) -> String {
    match item.kind {
        LeftoverKind::App => item
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "App".to_string()),
        kind => kind.label().to_string(),
    }
}

/// The tag after a row's label, if any.
pub fn row_tag(item: &Leftover) -> Option<&'static str> {
    match item.kind {
        LeftoverKind::System => Some("Report only"),
        LeftoverKind::App if item.note.is_some() => Some("Needs admin"),
        _ if item.selectable && item.note.is_some() => Some("Review"),
        _ if item.by_name => Some("Name match"),
        _ => None,
    }
}

pub fn kind_icon(kind: LeftoverKind) -> IconName {
    match kind {
        LeftoverKind::App => IconName::AppWindow,
        LeftoverKind::AppSupport => IconName::FolderCog,
        LeftoverKind::Caches => IconName::Archive,
        LeftoverKind::Preferences => IconName::Settings,
        LeftoverKind::Container => IconName::Container,
        LeftoverKind::GroupContainer => IconName::Layers,
        LeftoverKind::AppScripts => IconName::ScrollText,
        LeftoverKind::SavedState => IconName::PanelsTopLeft,
        LeftoverKind::Logs => IconName::FileText,
        LeftoverKind::WebData | LeftoverKind::WebKit => IconName::Globe,
        LeftoverKind::LaunchAgent => IconName::Rocket,
        LeftoverKind::System => IconName::FileCog,
    }
}

impl DialogSpec {
    /// The receipt for Uninstall (C55): a line for the app, then a line per kind of leftover.
    pub fn uninstall(name: &str, ticked: &[&Leftover]) -> DialogSpec {
        let mut lines: Vec<(String, String)> = Vec::new();
        let mut kinds: Vec<LeftoverKind> = Vec::new();
        for item in ticked {
            if !kinds.contains(&item.kind) {
                kinds.push(item.kind);
            }
        }
        for kind in kinds {
            let of_kind: Vec<&&Leftover> = ticked.iter().filter(|i| i.kind == kind).collect();
            let bytes: u64 = of_kind.iter().map(|i| i.bytes.unwrap_or(0)).sum();
            let label = match (kind, of_kind.as_slice()) {
                (LeftoverKind::App, [one]) => row_label(one),
                (_, [_]) => kind.label().to_string(),
                (_, many) => format!("{}, {} items", kind.label(), many.len()),
            };
            lines.push((label, format::size(bytes)));
        }
        let total: u64 = ticked.iter().map(|i| i.bytes.unwrap_or(0)).sum();
        let mut notes = Vec::new();
        if ticked
            .iter()
            .any(|i| i.kind == LeftoverKind::App && i.note.is_some())
        {
            notes.push(format!("Finder will ask for your password to move {name}."));
        }
        if ticked.iter().any(|i| i.kind == LeftoverKind::LaunchAgent) {
            notes.push(LAUNCH_AGENT_NOTE.to_string());
        }
        DialogSpec {
            kind: crate::views::dialogs::DialogKind::Uninstall,
            title: format!("Uninstall {name}?"),
            body: Some("You can put them back from the Trash until you empty it.".to_string()),
            rows: ticked
                .iter()
                .map(|i| (row_label(i), format::size(i.bytes.unwrap_or(0))))
                .collect(),
            notes,
            confirm: "Move to Trash".to_string(),
            destructive: false,
            receipt: Some(crate::views::dialogs::Receipt {
                count_line: format::items(ticked.len()),
                lines,
                total: format::size(total),
            }),
        }
    }
}

// ---------- Model ----------

impl TidyModel {
    /// The screen opened: list the apps when there is no list or it is older than 10 minutes.
    pub fn enter_uninstaller(&mut self, cx: &mut Context<Self>) {
        let stale = match &self.uninstall.list {
            None => true,
            Some(l) => now_unix(cx).saturating_sub(l.at) > STALE_SCAN_SECS,
        };
        if stale && self.uninstall.open.is_none() {
            self.list_apps(cx);
        }
    }

    /// Cmd-R and Rescan: look again at the open app, or list the apps again.
    pub fn rescan_uninstaller(&mut self, cx: &mut Context<Self>) {
        match self.uninstall.open.clone() {
            Some(app) => self.open_app(app, cx),
            None => self.list_apps(cx),
        }
    }

    pub fn list_apps(&mut self, cx: &mut Context<Self>) {
        if self.uninstall.listing {
            return;
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        if !send(cx, Command::ListApps { reply }) {
            return;
        }
        self.uninstall.listing = true;
        cx.spawn(async move |this, cx| {
            let list = rx.await.ok();
            let _ = this.update(cx, |m, cx| {
                m.uninstall.listing = false;
                if let Some(list) = list {
                    m.set_app_list(list, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn set_app_list(&mut self, list: AppList, cx: &mut Context<Self>) {
        self.uninstall.removed.clear();
        self.uninstall.list = Some(list);
        self.load_app_icons(cx);
    }

    /// Reads the app icons a few per frame, so a long list never stalls the window.
    fn load_app_icons(&mut self, cx: &mut Context<Self>) {
        let Some(t) = terminator(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            loop {
                let more = this
                    .update(cx, |m, cx| {
                        let u = &mut m.uninstall;
                        let mut next: Vec<PathBuf> = u
                            .list
                            .iter()
                            .flat_map(|l| l.apps.iter().map(|a| a.path.clone()))
                            .filter(|p| !u.icons_tried.contains(p))
                            .take(ICONS_PER_STEP)
                            .collect();
                        if let Some(open) = &u.open
                            && !u.icons_tried.contains(open)
                        {
                            next.insert(0, open.clone());
                        }
                        if next.is_empty() {
                            return false;
                        }
                        for p in next {
                            u.icons_tried.insert(p.clone());
                            if let Some(icon) = t.icon(&p) {
                                u.icons.insert(p, icon);
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
                cx.background_executor().timer(ICON_STEP).await;
            }
        })
        .detach();
    }

    /// Opens an app: shows its row at once and looks for its leftovers.
    pub fn open_app(&mut self, app: PathBuf, cx: &mut Context<Self>) {
        if self.uninstall.removing.is_some() {
            return;
        }
        let u = &mut self.uninstall;
        u.look += 1;
        let look = u.look;
        u.open = Some(app.clone());
        u.plan = None;
        u.blocked = None;
        u.ticked.clear();
        u.skipped.clear();
        u.leaving.clear();
        u.gone.clear();
        u.notice = None;
        u.notice_leaving = None;
        let (reply, rx) = tokio::sync::oneshot::channel();
        if send(cx, Command::FindLeftovers { app, reply }) {
            self.uninstall.looking = true;
            cx.spawn(async move |this, cx| {
                let plan = rx.await.ok().flatten();
                let _ = this.update(cx, |m, cx| {
                    if m.uninstall.look == look {
                        m.set_uninstall_plan(plan);
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// The open app's files arrived. None means the app is no longer one Tidy lists.
    pub fn set_uninstall_plan(&mut self, plan: Option<UninstallPlan>) {
        let u = &mut self.uninstall;
        u.looking = false;
        u.arrived = Some(std::time::Instant::now());
        match plan {
            Some(plan) => {
                u.ticked = plan
                    .items
                    .iter()
                    .filter(|i| i.ticked)
                    .map(|i| i.id.clone())
                    .collect();
                u.blocked = plan.blocked.clone();
                u.plan = Some(plan);
            }
            None => {
                u.open = None;
                u.plan = None;
                u.notice = Some(Notice::Error(APP_GONE.to_string()));
            }
        }
    }

    /// Back to the list (the breadcrumb and Cmd-Up). Not while an uninstall runs.
    pub fn close_app(&mut self, cx: &mut Context<Self>) {
        let u = &mut self.uninstall;
        if u.open.is_none() || u.removing.is_some() {
            return;
        }
        u.look += 1;
        u.open = None;
        u.plan = None;
        u.looking = false;
        u.blocked = None;
        cx.notify();
    }

    pub fn set_app_sort(&mut self, sort: AppSort, cx: &mut Context<Self>) {
        if self.uninstall.sort != sort {
            self.uninstall.sort = sort;
            cx.notify();
        }
    }

    pub fn toggle_leftover(&mut self, id: &str, cx: &mut Context<Self>) {
        let u = &mut self.uninstall;
        if u.removing.is_some()
            || !u
                .plan
                .as_ref()
                .and_then(|p| p.item(id))
                .is_some_and(|i| i.selectable)
        {
            return;
        }
        if !u.ticked.remove(id) {
            u.ticked.insert(id.to_string());
        }
        u.skipped.remove(id);
        cx.notify();
    }

    /// The receipt's Move to Trash: sends `Uninstall` for the ticked rows.
    pub fn uninstall_ticked(&mut self, cx: &mut Context<Self>) -> bool {
        let u = &self.uninstall;
        if !u.can_uninstall() {
            return false;
        }
        let Some(app) = u.plan.as_ref().map(|p| p.app.path.clone()) else {
            return false;
        };
        let ids: Vec<String> = u.ticked_rows().iter().map(|i| i.id.clone()).collect();
        let total = ids.len();
        if !send(cx, Command::Uninstall { app, ids }) {
            return false;
        }
        self.uninstall.removing = Some((0, total));
        self.uninstall.notice = None;
        self.begin_activity();
        cx.notify();
        true
    }

    /// The uninstall ended. When the app went to the Trash and nothing was skipped, the screen
    /// goes back to the list without it; otherwise the rows that stayed say why.
    pub(crate) fn on_uninstall_done(&mut self, entry: &HistoryEntry) {
        let u = &mut self.uninstall;
        u.removing = None;
        let app = u.plan.as_ref().map(|p| p.app.path.clone());
        let app_moved = entry
            .items
            .iter()
            .any(|i| Some(&i.path) == app.as_ref() && i.result == ItemResult::Trashed);
        let all_moved = entry.items.iter().all(|i| i.result == ItemResult::Trashed);
        if app_moved && let Some(app) = &app {
            u.removed.insert(app.clone());
        }
        u.notice = Some(if entry.trashed_bytes > 0 || entry.trashed().count() > 0 {
            Notice::Moved {
                bytes: entry.trashed_bytes,
            }
        } else {
            Notice::Error(NOTHING_MOVED.to_string())
        });
        if app_moved && all_moved {
            u.look += 1;
            u.open = None;
            u.plan = None;
            u.blocked = None;
            u.leaving.clear();
        }
    }

    /// Rows that reached the Trash fade for 140 ms, then leave. At once under Reduce Motion.
    pub(crate) fn fade_out_uninstall(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let finish = |m: &mut TidyModel, ids: &[String]| {
            for id in ids {
                if m.uninstall.leaving.remove(id) {
                    m.uninstall.gone.insert(id.clone());
                }
            }
        };
        let Some(delay) = motion::remove_delay(cx.reduce_motion()) else {
            finish(self, &ids);
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |m, cx| {
                finish(m, &ids);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn dismiss_uninstall_notice(&mut self, cx: &mut Context<Self>) {
        let Some(notice) = self.uninstall.notice.take() else {
            return;
        };
        if !cx.reduce_motion() {
            self.uninstall.notice_leaving = Some(notice);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(NOTE_OUT).await;
                let _ = this.update(cx, |m, cx| {
                    m.uninstall.notice_leaving = None;
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }
}

// ---------- View ----------

impl TidyView {
    /// The scroll handle and row height of the list the Uninstaller shows now.
    pub(crate) fn uninstall_list(&self, cx: &App) -> (UniformListScrollHandle, f32) {
        if self.model.read(cx).uninstall.open.is_some() {
            (self.leftovers_scroll.clone(), metrics::CLEANUP_ROW_H)
        } else {
            (self.apps_scroll.clone(), metrics::LARGE_ROW_H)
        }
    }

    /// Uninstall…: the receipt for the ticked rows.
    pub fn request_uninstall(&mut self, cx: &mut Context<Self>) {
        let spec = {
            let u = &self.model.read(cx).uninstall;
            if !u.can_uninstall() {
                return;
            }
            let name = u
                .plan
                .as_ref()
                .map(|p| p.app.name.clone())
                .unwrap_or_default();
            DialogSpec::uninstall(&name, &u.ticked_rows())
        };
        self.open_dialog(spec, cx);
    }

    pub(crate) fn render_uninstaller(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.model.read(cx).uninstall.open.is_some() {
            self.render_app_detail(t, cx)
        } else {
            self.render_app_list(t, cx)
        }
    }

    fn uninstall_notice(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let u = &self.model.read(cx).uninstall;
        let shown = u
            .notice
            .clone()
            .map(|n| (n, false))
            .or(u.notice_leaving.clone().map(|n| (n, true)));
        shown.map(|(n, leaving)| {
            let model = self.model.clone();
            self.notice_band(
                t,
                &n,
                leaving,
                true,
                move |_, cx| model.update(cx, |m, cx| m.dismiss_uninstall_notice(cx)),
                cx,
            )
        })
    }

    fn render_app_list(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.clone();
        let now = now_unix(cx);
        let (apps, listing, has_list, sort, icons, status, listed_at) = {
            let u = &self.model.read(cx).uninstall;
            (
                Rc::new(u.apps().into_iter().cloned().collect::<Vec<AppEntry>>()),
                u.listing,
                u.list.is_some(),
                u.sort,
                u.icons.clone(),
                u.list_status(),
                u.list.as_ref().map(|l| l.at),
            )
        };
        let scrolled = self.apps_scroll.0.borrow().base_handle.offset().y < px(0.);

        let controls = if listing {
            self.busy_controls(t, "Listing apps".to_string(), cx)
                .into_any_element()
        } else if has_list {
            let (by_name, by_size, by_opened, rescan) =
                (model.clone(), model.clone(), model.clone(), model.clone());
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(segmented(
                    "app-sort",
                    vec![
                        Segment {
                            label: "Name",
                            selected: sort == AppSort::Name,
                            selector: "sort-name".to_string(),
                            on_click: Box::new(move |_, cx| {
                                by_name.update(cx, |m, cx| m.set_app_sort(AppSort::Name, cx));
                            }),
                        },
                        Segment {
                            label: "Size",
                            selected: sort == AppSort::Size,
                            selector: "sort-size".to_string(),
                            on_click: Box::new(move |_, cx| {
                                by_size.update(cx, |m, cx| m.set_app_sort(AppSort::Size, cx));
                            }),
                        },
                        Segment {
                            label: "Last opened",
                            selected: sort == AppSort::LastOpened,
                            selector: "sort-opened".to_string(),
                            on_click: Box::new(move |_, cx| {
                                by_opened
                                    .update(cx, |m, cx| m.set_app_sort(AppSort::LastOpened, cx));
                            }),
                        },
                    ],
                    t,
                ))
                .child(icon_button(
                    "apps-rescan",
                    "apps-rescan".to_string(),
                    IconName::RefreshCw,
                    t,
                    move |_, cx| rescan.update(cx, |m, cx| m.list_apps(cx)),
                ))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let toolbar = self.toolbar(t, "Uninstaller", scrolled, controls, cx);
        let notice = self.uninstall_notice(t, cx);

        let body: AnyElement = if !has_list || apps.is_empty() {
            let list_model = model.clone();
            if listing {
                quiet_state(t, LISTING_TITLE, LISTING_BODY).into_any_element()
            } else {
                self.empty_state(
                    t,
                    IconName::Package,
                    EMPTY_TITLE,
                    EMPTY_BODY.to_string(),
                    "List Apps",
                    move |_, cx| list_model.update(cx, |m, cx| m.list_apps(cx)),
                )
                .into_any_element()
            }
        } else {
            let count = apps.len();
            let row_model = model.clone();
            let list = uniform_list("app-rows", count, move |range, window, _cx| {
                let t = Theme::of(window);
                range
                    .filter_map(|ix| {
                        apps.get(ix)
                            .map(|a| render_app_row(a, ix, &t, &icons, now, &row_model))
                    })
                    .collect::<Vec<AnyElement>>()
            })
            .track_scroll(&self.apps_scroll)
            .flex_1()
            .px(px(metrics::LIST_INSET))
            .pt(px(2.))
            .pb(px(8.))
            .when(listing, |l| l.opacity(0.5));
            self.list_frame(list).into_any_element()
        };

        let right = listed_at.map(|at| {
            div()
                .debug_selector(|| "listed-at".to_string())
                .child(format!("Listed {}", format::ago(now, at)))
                .into_any_element()
        });
        let status = self.status_bar(t, status, right);
        div()
            .debug_selector(|| "uninstaller".to_string())
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

    fn render_app_detail(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.clone();
        let view = cx.entity().downgrade();
        let now = now_unix(cx);
        let snap = {
            let u = &self.model.read(cx).uninstall;
            let open = u.open.clone().unwrap_or_default();
            let entry = u.plan.as_ref().map(|p| p.app.clone()).or_else(|| {
                u.list
                    .as_ref()
                    .and_then(|l| l.apps.iter().find(|a| a.path == open).cloned())
            });
            DetailView {
                rows: Rc::new(u.rows().into_iter().cloned().collect()),
                ticked: u.ticked.clone(),
                skipped: u.skipped.clone(),
                leaving: u.leaving.clone(),
                icon: u.icons.get(&open).cloned(),
                name: entry.as_ref().map(|e| e.name.clone()).unwrap_or_else(|| {
                    open.file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default()
                }),
                meta: entry.as_ref().map(|e| app_meta(e, now)).unwrap_or_default(),
                looking: u.looking,
                arrived: u.arrived.map(|at| (at, u.look as usize)),
                blocked: u.blocked.clone(),
                removing: u.removing.is_some(),
                can_uninstall: u.can_uninstall(),
                button: u.button_label(),
                status: u.detail_status(),
                bytes: u.ticked_bytes(),
                root: u.plan.as_ref().map(|p| p.root.clone()).unwrap_or_default(),
            }
        };
        let scrolled = self.leftovers_scroll.0.borrow().base_handle.offset().y < px(0.);

        let button_view = view.clone();
        let controls = text_button(
            "uninstall",
            "uninstall-button".to_string(),
            snap.button.clone(),
            true,
            snap.can_uninstall,
            t,
            move |_, cx| {
                let _ = button_view.update(cx, |v, cx| v.request_uninstall(cx));
            },
        );
        let toolbar = self.toolbar(t, "Uninstaller", false, controls, cx);
        let notice = self.uninstall_notice(t, cx);
        let crumbs = self.app_breadcrumb(t, &snap.name, !snap.removing);
        let header = app_header(t, &snap);

        let body: AnyElement = if snap.looking {
            quiet_state(
                t,
                "Looking for its files",
                &format!("Tidy is looking for the files {} left behind.", snap.name),
            )
            .into_any_element()
        } else {
            let rows = snap.rows.clone();
            let (ticked, skipped, leaving, root) = (
                snap.ticked.clone(),
                snap.skipped.clone(),
                snap.leaving.clone(),
                snap.root.clone(),
            );
            let (hovered, icon) = (self.hovered.clone(), snap.icon.clone());
            let row_model = model.clone();
            let hover_view = view.clone();
            let count = rows.len();
            let list = uniform_list("leftover-rows", count, move |range, window, cx| {
                let t = Theme::of(window);
                let env = RowEnv {
                    viewport_w: f32::from(window.viewport_size().width),
                    char_w: mono_char_width(window),
                    model: row_model.clone(),
                    view: hover_view.clone(),
                    hovered: hovered.clone(),
                    reduce_motion: cx.reduce_motion(),
                    icon: icon.clone(),
                    root: root.clone(),
                };
                range
                    .filter_map(|ix| {
                        rows.get(ix).map(|item| {
                            let state = RowState {
                                ticked: ticked.contains(&item.id),
                                skipped: skipped.get(&item.id).cloned(),
                                leaving: leaving.contains(&item.id),
                            };
                            render_leftover_row(item, ix, &t, &env, state)
                        })
                    })
                    .collect::<Vec<AnyElement>>()
            })
            .track_scroll(&self.leftovers_scroll)
            .flex_1()
            .px(px(metrics::LIST_INSET))
            .pt(px(2.))
            .pb(px(8.));
            // The rows come in over CARD_IN when the look finishes, so they don't pop in
            // under "Looking for its files" (motion gate: occasional, preventing a jarring
            // change). Still under Reduce Motion and on a window that opens on them.
            let phase = crate::views::overlay::phase_for(snap.arrived, cx.reduce_motion());
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(crate::views::scroll_edge(t, scrolled))
                .child(match phase {
                    crate::views::overlay::Phase::Enter(generation) => self
                        .list_frame(list)
                        .with_animation(
                            ("leftovers-in", generation),
                            Animation::new(m::CARD_IN).with_easing(m::curve(m::enter())),
                            |el, d| el.opacity(d),
                        )
                        .into_any_element(),
                    _ => self.list_frame(list).into_any_element(),
                })
                .into_any_element()
        };
        let status = self.status_bar(t, snap.status.clone(), None);
        div()
            .debug_selector(|| "uninstaller-app".to_string())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .children(notice)
            .child(crumbs)
            .child(header)
            .child(body)
            .child(status)
            .into_any_element()
    }

    /// "All apps > Slack": the first segment goes back to the list.
    fn app_breadcrumb(&self, t: &Theme, name: &str, can_go_back: bool) -> Div {
        let hover = t.row_hover;
        let model = self.model.clone();
        let back = div()
            .id("crumb-apps")
            .debug_selector(|| "crumb-apps".to_string())
            .flex_none()
            .px(px(4.))
            .rounded(px(4.))
            .text_size(px(12.))
            .display(FontWeight::SEMIBOLD)
            .whitespace_nowrap()
            .text_color(t.text_2)
            .child("All apps")
            .when(can_go_back, |d| {
                d.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(move |_, _, cx| model.update(cx, |m, cx| m.close_app(cx)))
                    .key_stop(t)
            });
        div()
            .debug_selector(|| "breadcrumb".to_string())
            .flex_none()
            .h(px(metrics::BREADCRUMB_H))
            .pl(px(16.))
            .pr(px(20.))
            .gap(px(4.))
            .flex()
            .items_center()
            .overflow_hidden()
            .child(back)
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(12.))
                    .text_color(t.text_3),
            )
            .child(
                div()
                    .px(px(4.))
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(12.))
                    .display(FontWeight::SEMIBOLD)
                    .whitespace_nowrap()
                    .text_color(t.text)
                    .child(name.to_string()),
            )
    }
}

/// What the detail render needs, copied out of the model.
struct DetailView {
    rows: Rc<Vec<Leftover>>,
    ticked: BTreeSet<String>,
    skipped: HashMap<String, String>,
    leaving: HashSet<String>,
    icon: Option<Arc<gpui_kit::Image>>,
    name: String,
    meta: String,
    looking: bool,
    /// When the rows arrived, and the look that keys their fade.
    arrived: Option<(std::time::Instant, usize)>,
    blocked: Option<String>,
    removing: bool,
    can_uninstall: bool,
    button: String,
    status: String,
    bytes: u64,
    root: PathBuf,
}

/// A centered title and line with no button, for work in progress.
fn quiet_state(t: &Theme, title: &str, body: &str) -> Div {
    div()
        .debug_selector(|| "quiet-state".to_string())
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(15.))
                .display(FontWeight::BOLD)
                .text_color(t.text)
                .child(title.to_string()),
        )
        .child(
            div()
                .max_w(px(360.))
                .px(px(16.))
                .text_center()
                .text_size(px(13.))
                .line_height(px(18.))
                .text_color(t.text_2)
                .child(body.to_string()),
        )
}

fn app_icon(icon: Option<&Arc<gpui_kit::Image>>, size: f32, t: &Theme) -> AnyElement {
    match icon {
        Some(image) => img(image.clone())
            .flex_none()
            .size(px(size))
            .into_any_element(),
        None => {
            glyph_tile(IconName::AppWindow, size, size * 0.22, size * 0.55, t).into_any_element()
        }
    }
}

/// The open app: its icon and name, the version line (or why Uninstall is off), and the
/// ticked total at the right.
fn app_header(t: &Theme, snap: &DetailView) -> Div {
    let (line, warn) = match (&snap.blocked, snap.looking) {
        (Some(b), _) => (blocked_line(b, &snap.name), true),
        (None, _) => (snap.meta.clone(), false),
    };
    div()
        .debug_selector(|| "app-header".to_string())
        .flex_none()
        .h(px(metrics::SUMMARY_H))
        .px(px(20.))
        .flex()
        .items_center()
        .gap(px(14.))
        .child(app_icon(snap.icon.as_ref(), 40., t))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .debug_selector(|| "app-name".to_string())
                        .text_size(px(20.))
                        .line_height(px(26.))
                        .display(FontWeight::BOLD)
                        .text_color(t.text)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(snap.name.clone()),
                )
                .child(
                    div()
                        .debug_selector(|| "app-meta".to_string())
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .font_features(tnum())
                        .text_color(if warn { t.warning } else { t.text_2 })
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(line),
                ),
        )
        .when(!snap.looking, |d| {
            d.child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_end()
                    .child(
                        div()
                            .debug_selector(|| "app-figure".to_string())
                            .text_size(px(figure::SIZE))
                            .line_height(px(figure::LINE))
                            .display(FontWeight::EXTRA_BOLD)
                            .font_features(tnum())
                            .text_color(t.text)
                            .whitespace_nowrap()
                            .child(format::size(snap.bytes)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.text_2)
                            .whitespace_nowrap()
                            .child("selected"),
                    ),
            )
        })
}

/// "Quit Slack to uninstall it. Tidy checks again every few seconds." for "Quit Slack first";
/// other reasons (the process list could not be read) as they are.
pub fn blocked_line(blocked: &str, name: &str) -> String {
    if blocked == format!("Quit {name} first") {
        format!("Quit {name} to uninstall it. Tidy checks again every few seconds.")
    } else {
        format!("{blocked}.")
    }
}

fn render_app_row(
    a: &AppEntry,
    ix: usize,
    t: &Theme,
    icons: &HashMap<PathBuf, Arc<gpui_kit::Image>>,
    now: u64,
    model: &Entity<TidyModel>,
) -> AnyElement {
    let (open_model, path) = (model.clone(), a.path.clone());
    let selector = format!("app-row:{}", a.bundle_id);
    let (hover, pressed) = (t.row_hover, t.row_pressed);
    div()
        .id(("app-row", ix))
        .role(Role::Button)
        .debug_selector(move || selector)
        .w_full()
        .h(px(metrics::LARGE_ROW_H))
        .px(px(10.))
        .gap(px(12.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(pressed))
        .on_click(move |_, _, cx| {
            open_model.update(cx, |m, cx| m.open_app(path.clone(), cx));
        })
        .key_stop(t)
        .child(app_icon(icons.get(&a.path), 32., t))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(13.))
                                .line_height(px(17.))
                                .display(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(a.name.clone()),
                        )
                        .when(!a.movable, |d| d.child(tag("Needs admin", t))),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .font_features(tnum())
                        .text_color(t.text_2)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(app_meta(a, now)),
                ),
        )
        .child(
            div()
                .flex_none()
                .w(px(80.))
                .text_right()
                .text_size(px(13.))
                .display(FontWeight::SEMIBOLD)
                .font_features(tnum())
                .text_color(t.text)
                .whitespace_nowrap()
                .child(format::size(a.bytes)),
        )
        .child(
            div()
                .flex_none()
                .w(px(metrics::ACTION))
                .flex()
                .justify_center()
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(12.))
                        .text_color(t.text_3),
                ),
        )
        .into_any_element()
}

struct RowEnv {
    viewport_w: f32,
    char_w: f32,
    model: Entity<TidyModel>,
    view: WeakEntity<TidyView>,
    hovered: Option<String>,
    reduce_motion: bool,
    icon: Option<Arc<gpui_kit::Image>>,
    root: PathBuf,
}

struct RowState {
    ticked: bool,
    skipped: Option<String>,
    leaving: bool,
}

/// The path line of a leftover row, from `~`, shortened in the middle to fit.
pub fn leftover_path(path: &Path, root: &Path, viewport_w: f32, char_w: f32) -> String {
    let width =
        (viewport_w - metrics::SIDEBAR_W - 1. - 2. * metrics::LIST_INSET - ROW_FIXED_W).max(0.);
    let cols = crate::views::large::path_columns(width, char_w);
    format::middle_truncate(&format::display_path(path, root), cols)
}

fn render_leftover_row(
    item: &Leftover,
    ix: usize,
    t: &Theme,
    env: &RowEnv,
    state: RowState,
) -> AnyElement {
    let id = item.id.clone();
    let hovered = env.hovered.as_deref() == Some(id.as_str());
    let (row_model, row_id) = (env.model.clone(), id.clone());
    let (check_model, check_id) = (env.model.clone(), id.clone());
    let (hover_view, hover_key) = (env.view.clone(), id.clone());
    let reveal = item.path.clone();
    let selectable = item.selectable;
    let selector_id = id.clone();

    let meta: AnyElement = match (&state.skipped, &item.note) {
        (Some(reason), _) => meta_text(reason.clone(), t.warning).into_any_element(),
        (None, Some(note)) => meta_text(note.clone(), t.text_2).into_any_element(),
        (None, None) => meta_text(
            leftover_path(&item.path, &env.root, env.viewport_w, env.char_w),
            t.text_2,
        )
        .font_family(MONO_FONT)
        .into_any_element(),
    };
    let lead: AnyElement = if selectable {
        checkbox(
            ("leftover-check", ix),
            format!("leftover-check:{id}"),
            if state.ticked { Tri::On } else { Tri::Off },
            true,
            t,
            move |_, cx| check_model.update(cx, |m, cx| m.toggle_leftover(&check_id, cx)),
        )
        .into_any_element()
    } else {
        div()
            .flex_none()
            .size(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(IconName::Lock).size(px(12.)).text_color(t.text_3))
            .into_any_element()
    };
    let tile = if item.kind == LeftoverKind::App {
        app_icon(env.icon.as_ref(), 28., t)
    } else {
        glyph_tile(kind_icon(item.kind), 28., 6., 16., t).into_any_element()
    };
    let size = match item.bytes {
        Some(b) => format::size(b),
        None => "Size unknown".to_string(),
    };
    let el = div()
        .id(("leftover-row", ix))
        .debug_selector(move || format!("leftover-row:{selector_id}"))
        .w_full()
        .h(px(metrics::CLEANUP_ROW_H))
        .px(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .when(hovered, |d| d.bg(t.row_hover))
        .on_hover(move |h, _, cx| {
            let (key, h) = (hover_key.clone(), *h);
            let _ = hover_view.update(cx, |v, cx| v.set_hovered(&key, h, cx));
        })
        .when(selectable, |d| {
            d.on_click(move |_, _, cx| {
                row_model.update(cx, |m, cx| m.toggle_leftover(&row_id, cx));
            })
        })
        .child(lead)
        .child(tile)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(13.))
                                .line_height(px(17.))
                                .display(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(row_label(item)),
                        )
                        .children(row_tag(item).map(|l| tag(l, t))),
                )
                .child(meta),
        )
        .child(
            div()
                .flex_none()
                .w(px(90.))
                .text_right()
                .text_size(px(13.))
                .font_features(tnum())
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(size),
        )
        .child(
            div()
                .flex_none()
                .w(px(metrics::TRAIL_W))
                .flex()
                .justify_end()
                .child(row_action(
                    hovered,
                    format!("leftover-reveal:{id}"),
                    |selector| {
                        icon_button(
                            ("leftover-reveal", ix),
                            selector,
                            IconName::FolderSearch,
                            t,
                            move |_, cx| open_path(OpenKind::Reveal, &reveal, cx),
                        )
                    },
                )),
        );
    if state.leaving && !env.reduce_motion {
        el.with_animation(
            ("leftover-out", ix),
            Animation::new(m::REMOVE).with_easing(m::curve(m::exit())),
            |el, t| el.opacity(1. - t),
        )
        .into_any_element()
    } else {
        el.into_any_element()
    }
}

fn meta_text(text: String, color: Hsla) -> Div {
    div()
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(color)
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn app(name: &str, bytes: u64) -> AppEntry {
        AppEntry {
            path: PathBuf::from(format!("/Applications/{name}.app")),
            name: name.to_string(),
            bundle_id: format!("com.example.{}", name.to_lowercase()),
            version: Some("4.41".to_string()),
            executable: Some(name.to_string()),
            team_id: None,
            team_name: Some("Example Inc.".to_string()),
            bytes,
            last_used: Some(10_000_000 - 21 * 86_400),
            used_source: UseSource::Spotlight,
            helpers: Vec::new(),
            movable: true,
        }
    }

    fn leftover(path: &str, kind: LeftoverKind, bytes: u64) -> Leftover {
        Leftover {
            id: path.to_string(),
            path: PathBuf::from(path),
            kind,
            bytes: Some(bytes),
            ticked: true,
            selectable: true,
            by_name: false,
            note: None,
        }
    }

    #[test]
    fn the_meta_line_joins_version_team_and_date() {
        assert_eq!(
            app_meta(&app("Slack", 1), 10_000_000),
            "Version 4.41 \u{b7} Example Inc. \u{b7} Opened 21 days ago"
        );
        let mut bare = app("Box", 1);
        bare.version = None;
        bare.team_name = None;
        bare.last_used = None;
        assert_eq!(app_meta(&bare, 1_000_000), "");
    }

    #[test]
    fn sorting_by_size_puts_the_biggest_first() {
        let mut u = UninstallState {
            list: Some(AppList {
                apps: vec![app("Alpha", 10), app("Beta", 30), app("Gamma", 20)],
                left_out: 0,
                at: 0,
            }),
            ..UninstallState::default()
        };
        let names =
            |u: &UninstallState| u.apps().iter().map(|a| a.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&u), ["Alpha", "Beta", "Gamma"]);
        u.sort = AppSort::Size;
        assert_eq!(names(&u), ["Beta", "Gamma", "Alpha"]);
        u.removed.insert(PathBuf::from("/Applications/Beta.app"));
        assert_eq!(names(&u), ["Gamma", "Alpha"]);
    }

    #[test]
    fn sorting_by_last_opened_puts_what_was_not_used_first() {
        let used = |name: &str, days_ago: u64, source: UseSource| {
            let mut a = app(name, 1);
            a.last_used = Some(100_000_000 - days_ago * 86_400);
            a.used_source = source;
            a
        };
        let mut none = app("Nodate", 1);
        none.last_used = None;
        none.used_source = UseSource::FileTimes;
        let u = UninstallState {
            list: Some(AppList {
                apps: vec![
                    used("Daily", 0, UseSource::Spotlight),
                    used("Old", 200, UseSource::Spotlight),
                    used("Updated", 2, UseSource::FileTimes),
                    used("Stale", 400, UseSource::FileTimes),
                    none,
                    used("Month", 30, UseSource::Spotlight),
                ],
                left_out: 0,
                at: 0,
            }),
            sort: AppSort::LastOpened,
            ..UninstallState::default()
        };
        let names: Vec<String> = u.apps().iter().map(|a| a.name.clone()).collect();
        // No open on record first (no date at all, then the oldest file date), then the
        // apps opened longest ago.
        assert_eq!(
            names,
            ["Nodate", "Stale", "Updated", "Old", "Month", "Daily"]
        );
    }

    #[test]
    fn the_receipt_has_the_app_then_a_line_per_kind() {
        let a = leftover(
            "/Applications/Slack.app",
            LeftoverKind::App,
            300 * 1_000_000,
        );
        let c1 = leftover("/h/Library/Caches/a", LeftoverKind::Caches, 1_000_000);
        let c2 = leftover("/h/Library/Caches/b", LeftoverKind::Caches, 2_000_000);
        let agent = leftover(
            "/h/Library/LaunchAgents/a.plist",
            LeftoverKind::LaunchAgent,
            4096,
        );
        let spec = DialogSpec::uninstall("Slack", &[&a, &c1, &c2, &agent]);
        assert_eq!(spec.title, "Uninstall Slack?");
        let r = spec.receipt.unwrap();
        assert_eq!(r.count_line, "4 items");
        let labels: Vec<&str> = r.lines.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Slack.app", "Caches, 2 items", "Launch agent"]);
        assert_eq!(spec.notes, [LAUNCH_AGENT_NOTE]);
        assert_eq!(spec.confirm, "Move to Trash");
    }

    #[test]
    fn tags_say_why_a_row_is_different() {
        let mut shared = leftover("/x", LeftoverKind::GroupContainer, 1);
        shared.ticked = false;
        shared.note = Some("Shared with Word".to_string());
        assert_eq!(row_tag(&shared), Some("Review"));
        let mut system = leftover("/Library/LaunchDaemons/x.plist", LeftoverKind::System, 1);
        system.selectable = false;
        system.note = Some("Needs an administrator".to_string());
        assert_eq!(row_tag(&system), Some("Report only"));
        let mut stuck = leftover("/Applications/X.app", LeftoverKind::App, 1);
        stuck.note = Some(tidy_core::uninstall::APP_FINDER_NOTE.to_string());
        assert_eq!(row_tag(&stuck), Some("Needs admin"));
        let finder = DialogSpec::uninstall("X", &[&stuck]);
        assert_eq!(
            finder.notes,
            ["Finder will ask for your password to move X."]
        );
        let mut named = leftover("/x", LeftoverKind::Caches, 1);
        named.by_name = true;
        assert_eq!(row_tag(&named), Some("Name match"));
        assert_eq!(row_tag(&leftover("/x", LeftoverKind::Caches, 1)), None);
        assert_eq!(
            row_label(&leftover("/Applications/X.app", LeftoverKind::App, 1)),
            "X.app"
        );
    }

    #[test]
    fn the_blocked_line_says_what_to_do() {
        assert_eq!(
            blocked_line("Quit Slack first", "Slack"),
            "Quit Slack to uninstall it. Tidy checks again every few seconds."
        );
        assert_eq!(
            blocked_line("Tidy couldn't check whether Slack is open", "Slack"),
            "Tidy couldn't check whether Slack is open."
        );
    }
}
