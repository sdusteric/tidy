//! The Security screen (design sections 5.4 and 11): a list of checks with a status dot, a
//! title and one plain sentence each, "New" and "Changed" marks against the last check, amber
//! buttons that open the right System Settings page, and under the Login items and Chrome
//! extensions checks the items themselves.
//!
//! The screen reads settings and never asks for anything: a check that could not run is gray
//! with the reason and no button that asks for access. A run is started only by a click. The
//! only action that touches a file is "Move to Trash" on a user launch agent, and it asks first.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::api::Command;
use tidy_core::model::{
    CheckId, CheckResult, FIRST_CHECK_SENTENCE, LaunchKind, LoginItem, Mark, Scope, SecurityReport,
    Signer, Source, Status,
};

use crate::app::{Notice, OpenKind, TidyModel, now_unix, open_path, open_settings, send};
use crate::format;
use crate::theme::{DisplayType, MONO_FONT, Theme, figure, metrics, tnum};
use crate::views::dialogs::DialogSpec;
use crate::views::main_window::TidyView;
use crate::views::{RowFocus, glyph_tile, hairline, icon_button, row_action, text_button};

pub const EMPTY_TITLE: &str = "Check this Mac's settings";
pub const EMPTY_BODY: &str = "Tidy reads FileVault, the firewall, Gatekeeper, System Integrity Protection, XProtect, login items and Chrome extensions. It only reads settings and never asks for access.";
pub const RUNNING_BODY: &str = "Checking your settings. This takes a few seconds.";
pub const CHECKING_SIGNATURE: &str = "Checking signature";
/// The note under a macOS updates row that has not been checked.
pub const UPDATES_BUTTON: &str = "Check Now";
/// The sentence of a macOS updates row that shows the Check Now button. The core's sentence
/// (also printed by tidyctl) explains why it waits; on screen the button is the way to start.
pub const UPDATES_ROW_SENTENCE: &str =
    "Not checked. Tidy asks Apple for updates only when you click Check Now.";

/// A login item moved to the Trash from this screen, until the core reports the outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trashing {
    pub key: String,
    pub path: PathBuf,
    pub outcome: Option<TrashOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrashOutcome {
    Trashed,
    Skipped(String),
}

#[derive(Default)]
pub struct SecurityState {
    /// The latest report: loaded from disk when the screen opens, replaced by each run.
    pub report: Option<SecurityReport>,
    /// The saved report was asked for.
    pub loaded: bool,
    /// A run is going: `(done, total)` steps, `(0, 0)` before the first progress event.
    pub running: Option<(usize, usize)>,
    /// Login items moved to the Trash since the report, by key.
    pub removed: HashSet<String>,
    /// Why a Move to Trash did not happen, by login item key.
    pub skipped: HashMap<String, String>,
    pub trashing: Option<Trashing>,
    pub notice: Option<Notice>,
    pub notice_leaving: Option<Notice>,
}

impl SecurityState {
    /// Whether `path` is the login item a Move to Trash is in flight for.
    pub fn is_trashing(&self, path: &Path) -> bool {
        self.trashing.as_ref().is_some_and(|t| t.path == path)
    }

    pub fn on_trashed(&mut self) {
        if let Some(t) = self.trashing.as_mut() {
            t.outcome = Some(TrashOutcome::Trashed);
        }
    }

    pub fn on_skipped(&mut self, reason: String) {
        if let Some(t) = self.trashing.as_mut() {
            t.outcome = Some(TrashOutcome::Skipped(reason));
        }
    }
}

impl TidyModel {
    /// Reads the last saved report once, so the screen shows something before a new run. It
    /// only reads a file in Tidy's own data folder.
    pub fn load_last_security(&mut self, cx: &mut Context<Self>) {
        if self.security.loaded {
            return;
        }
        let (reply, rx) = tokio::sync::oneshot::channel();
        if !send(cx, Command::LastSecurity { reply }) {
            return;
        }
        self.security.loaded = true;
        cx.spawn(async move |this, cx| {
            let report = rx.await.ok().flatten();
            let _ = this.update(cx, |m, cx| {
                // A run that finished meanwhile is newer than the saved file.
                if m.security.report.is_none() {
                    m.security.report = report;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs the checks. `include_updates` also runs `softwareupdate --list`; the Security
    /// screen sets it only from the "Check now" button on the macOS updates row.
    pub fn run_security(&mut self, include_updates: bool, cx: &mut Context<Self>) {
        // A check up runs the checks itself, and the core ignores a second request meanwhile.
        if self.security.running.is_some() || self.checkup.running {
            return;
        }
        if send(cx, Command::RunSecurity { include_updates }) {
            self.security.running = Some((0, 0));
            self.security.notice = None;
            self.security.notice_leaving = None;
            self.begin_activity();
            cx.notify();
        }
    }

    pub(crate) fn on_security_result(&mut self, report: SecurityReport) {
        self.checkup.parts.security = true;
        let s = &mut self.security;
        s.running = None;
        s.loaded = true;
        s.removed.clear();
        s.skipped.clear();
        s.report = Some(report);
    }

    /// "Move to Trash" for a user launch agent, after the confirm. The core checks the path
    /// again right before the move.
    pub fn trash_login_item(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(item) = self
            .security
            .report
            .as_ref()
            .and_then(|r| r.login_items.iter().find(|i| i.key() == key))
        else {
            return;
        };
        let Some(path) = item.plist.clone().filter(|_| item.can_trash()) else {
            return;
        };
        if self.security.trashing.is_some() {
            return;
        }
        if send(
            cx,
            Command::TrashPaths {
                paths: vec![path.clone()],
            },
        ) {
            self.security.trashing = Some(Trashing {
                key: key.to_string(),
                path,
                outcome: None,
            });
            self.security.skipped.remove(key);
            self.security.notice = None;
            cx.notify();
        }
    }

    /// The end of a Move to Trash started here. True when `entry` is that one, so the Large
    /// files screen leaves it alone.
    pub(crate) fn security_trash_done(&mut self, entry: &tidy_core::model::HistoryEntry) -> bool {
        let ours = self.security.trashing.as_ref().is_some_and(|t| {
            entry.source == Source::Large && entry.items.iter().any(|i| i.path == t.path)
        });
        if !ours {
            return false;
        }
        let Some(t) = self.security.trashing.take() else {
            return false;
        };
        match t.outcome {
            Some(TrashOutcome::Trashed) => {
                self.security.removed.insert(t.key);
                self.security.notice = Some(Notice::Info(
                    "Moved the launch agent to the Trash. It stops running the next time you log in."
                        .to_string(),
                ));
            }
            Some(TrashOutcome::Skipped(reason)) => {
                self.security.skipped.insert(t.key, reason);
            }
            None => {
                let reason = entry
                    .items
                    .iter()
                    .find(|i| i.path == t.path)
                    .and_then(|i| i.reason.clone())
                    .unwrap_or_else(|| "It was not moved.".to_string());
                self.security.skipped.insert(t.key, reason);
            }
        }
        true
    }

    /// Dismisses the notice band.
    pub fn dismiss_security_notice(&mut self, cx: &mut Context<Self>) {
        self.security.notice = None;
        self.security.notice_leaving = None;
        cx.notify();
    }
}

// ---------- rows ----------

/// One launch agent or daemon, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginRow {
    pub key: String,
    pub label: String,
    /// The program path, or the plist when the job names no program.
    pub path: String,
    /// (text, drawn in the warning color) segments: signer, notarization, when it runs.
    pub meta: Vec<(String, bool)>,
    pub mark: Option<Mark>,
    pub can_trash: bool,
    pub plist: Option<PathBuf>,
    pub program: Option<PathBuf>,
    pub skipped: Option<String>,
}

/// One Chrome extension, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionRow {
    pub key: String,
    pub name: String,
    pub meta: String,
    pub mark: Option<Mark>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SecRow {
    Check {
        check: CheckResult,
        /// A "Check now" button on the macOS updates row.
        updates_button: bool,
        first: bool,
    },
    Login(LoginRow),
    Extension(ExtensionRow),
}

fn login_row(item: &LoginItem, running: bool, state: &SecurityState) -> LoginRow {
    let program = item.program.clone();
    let path = program.as_ref().or(item.plist.as_ref()).map_or_else(
        || "No program listed".to_string(),
        |p| p.display().to_string(),
    );
    let mut meta = Vec::new();
    let checking = running && item.signer == Signer::Unknown && item.notarized.is_none();
    if checking {
        meta.push((CHECKING_SIGNATURE.to_string(), false));
    } else {
        meta.push((
            item.signer.label().to_string(),
            item.signer == Signer::Unsigned,
        ));
        match item.notarized {
            // Notarized: Apple scanned the program before it was shipped.
            Some(true) => meta.push(("Checked by Apple".to_string(), false)),
            Some(false) => meta.push(("Not checked by Apple".to_string(), true)),
            None => {}
        }
    }
    meta.push((
        if item.runs_at_load {
            "Starts automatically"
        } else {
            "Runs when needed"
        }
        .to_string(),
        false,
    ));
    if item.kind == LaunchKind::Daemon {
        meta.push(("System service".to_string(), false));
    } else if item.scope == Scope::System {
        meta.push(("All users".to_string(), false));
    }
    LoginRow {
        key: item.key(),
        label: item.label.clone(),
        path,
        meta,
        mark: item.mark,
        can_trash: item.can_trash(),
        plist: item.plist.clone(),
        program,
        skipped: state.skipped.get(&item.key()).cloned(),
    }
}

/// The rows to draw: every check in order, with the login items under their check and the
/// Chrome extensions under theirs.
pub fn security_rows(state: &SecurityState) -> Vec<SecRow> {
    let Some(report) = &state.report else {
        return Vec::new();
    };
    let running = state.running.is_some();
    let mut rows = Vec::new();
    for (n, check) in report.checks.iter().enumerate() {
        rows.push(SecRow::Check {
            check: check.clone(),
            updates_button: check.id == CheckId::MacosUpdates && !report.updates_checked,
            first: n == 0,
        });
        match check.id {
            CheckId::LoginItems => {
                rows.extend(
                    report
                        .login_items
                        .iter()
                        .filter(|i| !state.removed.contains(&i.key()))
                        .map(|i| SecRow::Login(login_row(i, running, state))),
                );
            }
            CheckId::ChromeExtensions => {
                rows.extend(report.extensions.iter().map(|e| {
                    let mut meta = Vec::new();
                    if let Some(v) = &e.version {
                        meta.push(format!("Version {v}"));
                    }
                    meta.push(if e.profile == "Default" {
                        "Default profile".to_string()
                    } else {
                        e.profile.clone()
                    });
                    if !e.enabled {
                        meta.push("Turned off".to_string());
                    }
                    SecRow::Extension(ExtensionRow {
                        key: e.key(),
                        name: e.name.clone(),
                        meta: meta.join(" \u{b7} "),
                        mark: e.mark,
                    })
                }));
            }
            _ => {}
        }
    }
    rows
}

/// The line under the headline: which check the marks compare with, or that this is the first
/// check, and how many checks could not run. When this check ran is in the status bar.
pub fn summary_line(report: &SecurityReport, now: u64) -> String {
    let mut parts = Vec::new();
    if report.first_run {
        parts.push(FIRST_CHECK_SENTENCE.to_string());
    } else if let Some(at) = report.compared_at {
        parts.push(format!(
            "New and changed since your check {}",
            format::ago(now, at)
        ));
    } else if report.compared_to.is_some() {
        parts.push("New and changed since your last check".to_string());
    }
    let gray = report.gray();
    if gray > 0 {
        // "Not checked" covers both a check that could not run and macOS updates, which wait
        // for a click.
        parts.push(format!("{gray} not checked"));
    }
    if parts.len() > 1 {
        // Joined with " · ", a sentence's own full stop goes.
        for p in &mut parts {
            if let Some(s) = p.strip_suffix('.') {
                *p = s.to_string();
            }
        }
    }
    parts.join(" \u{b7} ")
}

/// "8 checks, 2 to look at, checked 2 min ago" for the status bar.
pub fn status_line(report: &SecurityReport, now: u64) -> String {
    let n = report.checks.len();
    let amber = report.amber();
    format!(
        "{n} {}, {}, checked {}",
        if n == 1 { "check" } else { "checks" },
        if amber == 0 {
            "none to look at".to_string()
        } else {
            format!("{amber} to look at")
        },
        format::ago(now, report.at)
    )
}

// ---------- drawing ----------

/// The status dot: 10 px, in a box as wide as a checkbox so titles line up.
fn dot(status: Status, t: &Theme) -> Div {
    div()
        .flex_none()
        .w(px(16.))
        .h(px(17.))
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(10.)).rounded_full().bg(match status {
            Status::Green => t.success_badge,
            Status::Amber => t.warning_badge,
            Status::Gray => t.gray_badge,
        }))
}

/// "New" or "Changed": the tag shape in the accent color.
fn mark_badge(mark: Mark, t: &Theme) -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.accent)
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(12.))
        .display(FontWeight::BOLD)
        .text_color(t.accent)
        .whitespace_nowrap()
        .child(mark.label())
}

fn check_row(
    check: &CheckResult,
    updates_button: bool,
    first: bool,
    ix: usize,
    t: &Theme,
    model: &Entity<TidyModel>,
) -> AnyElement {
    let id = check.id;
    let action = check.action.map(|link| {
        text_button(
            ("sec-action", ix),
            format!("sec-action:{id:?}"),
            link.label(),
            false,
            true,
            t,
            move |_, cx| {
                cx.stop_propagation();
                open_settings(link, cx);
            },
        )
    });
    let updates = updates_button.then(|| {
        let model = model.clone();
        text_button(
            ("sec-updates", ix),
            "sec-updates".to_string(),
            UPDATES_BUTTON,
            false,
            true,
            t,
            move |_, cx| {
                cx.stop_propagation();
                model.update(cx, |m, cx| m.run_security(true, cx));
            },
        )
    });
    div()
        .id(("sec-check", ix))
        .debug_selector(move || format!("sec-check:{id:?}"))
        .relative()
        .w_full()
        .min_h(px(52.))
        .px(px(10.))
        .py(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .flex()
        .items_start()
        .when(!first, |d| {
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(10.))
                    .right(px(10.))
                    .child(hairline(t)),
            )
        })
        .child(dot(check.status, t))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(17.))
                                .display(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .whitespace_nowrap()
                                .child(check.title.clone()),
                        )
                        .children(check.mark.map(|m| {
                            mark_badge(m, t).debug_selector(move || format!("sec-mark:{id:?}"))
                        })),
                )
                .child(
                    div()
                        .debug_selector(move || format!("sec-sentence:{id:?}"))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(t.text_2)
                        .child(if updates_button {
                            UPDATES_ROW_SENTENCE.to_string()
                        } else {
                            check.sentence.clone()
                        }),
                ),
        )
        .children(action)
        .children(updates)
        .into_any_element()
}

fn login_row_view(
    row: &LoginRow,
    ix: usize,
    hovered: bool,
    row_focus: FocusHandle,
    t: &Theme,
    view: &WeakEntity<TidyView>,
) -> AnyElement {
    let key = row.key.clone();
    let hover_bg = t.row_hover;
    let hover_view = view.clone();
    let hover_key = key.clone();
    let reveal_path = row.plist.clone().or(row.program.clone());
    let actions = Some({
        let reveal = reveal_path.map(|path| {
            row_action(hovered, format!("sec-reveal:{key}"), |selector| {
                icon_button(
                    ("sec-login-reveal", ix),
                    selector,
                    IconName::FolderSearch,
                    t,
                    move |_, cx| open_path(OpenKind::Reveal, &path, cx),
                )
            })
        });
        let trash = row.can_trash.then(|| {
            let view = view.clone();
            let trash_key = key.clone();
            let label = row.label.clone();
            row_action(hovered, format!("sec-trash:{key}"), |selector| {
                icon_button(
                    ("sec-login-trash", ix),
                    selector,
                    IconName::Trash,
                    t,
                    move |_, cx| {
                        let (key, label) = (trash_key.clone(), label.clone());
                        let _ = view.update(cx, |v, cx| v.request_trash_login_item(key, label, cx));
                    },
                )
            })
        });
        div().flex().items_center().children(reveal).children(trash)
    });
    let selector_key = key.clone();
    div()
        .id(("sec-login", ix))
        .track_focus(&row_focus)
        .debug_selector(move || format!("sec-login:{selector_key}"))
        .w_full()
        .h(px(60.))
        .pl(px(34.))
        .pr(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .when(hovered, |d| d.bg(hover_bg))
        .on_hover(move |h, _, cx| {
            let k = hover_key.clone();
            let h = *h;
            let _ = hover_view.update(cx, |v, cx| v.set_hovered(&k, h, cx));
        })
        .child(glyph_tile(IconName::SquareTerminal, 28., 6., 16., t))
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
                                .child(row.label.clone()),
                        )
                        .children(row.mark.map(|m| {
                            let key = row.key.clone();
                            mark_badge(m, t).debug_selector(move || format!("sec-login-mark:{key}"))
                        })),
                )
                .child(
                    div()
                        .font_family(MONO_FONT)
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(t.text_2)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(format::middle_truncate(&row.path, 64)),
                )
                .child(match &row.skipped {
                    Some(reason) => div()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(t.warning)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(reason.clone()),
                    None => div()
                        .debug_selector({
                            let key = row.key.clone();
                            move || format!("sec-login-meta:{key}")
                        })
                        .flex()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .whitespace_nowrap()
                        .children(row.meta.iter().enumerate().map(|(i, (text, warn))| {
                            div()
                                .flex()
                                .gap(px(6.))
                                .text_color(if *warn { t.warning } else { t.text_2 })
                                .when(i > 0, |d| {
                                    d.child(div().text_color(t.text_3).child("\u{b7}"))
                                })
                                .child(text.clone())
                        })),
                }),
        )
        .child(
            div()
                .flex_none()
                .w(px(metrics::TRAIL_W))
                .flex()
                .justify_end()
                .children(actions),
        )
        .into_any_element()
}

fn extension_row_view(row: &ExtensionRow, ix: usize, t: &Theme) -> AnyElement {
    let key = row.key.clone();
    div()
        .id(("sec-ext", ix))
        .debug_selector(move || format!("sec-ext:{key}"))
        .w_full()
        .h(px(44.))
        .pl(px(34.))
        .pr(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .child(glyph_tile(IconName::Puzzle, 28., 6., 16., t))
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
                                .child(row.name.clone()),
                        )
                        .children(row.mark.map(|m| mark_badge(m, t))),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(t.text_2)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.meta.clone()),
                ),
        )
        .into_any_element()
}

/// What the Security screen draws, copied out of the model.
pub(crate) struct SecurityView {
    pub(crate) report: Option<SecurityReport>,
    pub(crate) rows: Rc<Vec<SecRow>>,
    pub(crate) running: Option<(usize, usize)>,
    pub(crate) notice: Option<Notice>,
    pub(crate) notice_leaving: Option<Notice>,
}

impl TidyView {
    /// Asks before a user launch agent goes to the Trash.
    pub fn request_trash_login_item(&mut self, key: String, label: String, cx: &mut Context<Self>) {
        self.open_dialog(DialogSpec::trash_login_item(&key, &label), cx);
    }

    pub(crate) fn security_snapshot(&self, cx: &App) -> SecurityView {
        let m = self.model.read(cx);
        let s = &m.security;
        SecurityView {
            report: s.report.clone(),
            rows: Rc::new(security_rows(s)),
            // Before a check up reaches its Security part, the screen already shows it as busy.
            running: s.running.or(m.checkup.running.then_some((0, 0))),
            notice: s.notice.clone(),
            notice_leaving: s.notice_leaving.clone(),
        }
    }

    /// The whole Security screen: toolbar, summary, checks, status bar.
    pub(crate) fn render_security(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = now_unix(cx);
        let snap = self.security_snapshot(cx);
        let scrolled = self.security_scroll.offset().y < px(0.);
        let model = self.model.clone();
        let view = cx.entity().downgrade();

        let controls = match snap.running {
            Some((done, total)) => self
                .busy_controls(t, format::checking(done, total), cx)
                .into_any_element(),
            None if snap.report.is_some() => {
                let rescan = model.clone();
                icon_button(
                    "sec-rescan",
                    "rescan-button".to_string(),
                    IconName::RefreshCw,
                    t,
                    move |_, cx| {
                        rescan.update(cx, |m, cx| m.run_security(false, cx));
                    },
                )
                .into_any_element()
            }
            None => div().into_any_element(),
        };
        // The summary scrolls with the checks, so the toolbar's edge is the one that shows.
        let toolbar = self.toolbar(t, "Security", scrolled, controls, cx);

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
                    model.update(cx, |m, cx| m.dismiss_security_notice(cx));
                },
                cx,
            )
        });

        let body: AnyElement = match &snap.report {
            None if snap.running.is_some() => div()
                .debug_selector(|| "security-running".to_string())
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .text_color(t.text_2)
                .child(RUNNING_BODY)
                .into_any_element(),
            None => {
                let run = model.clone();
                self.empty_state(
                    t,
                    IconName::ShieldCheck,
                    EMPTY_TITLE,
                    EMPTY_BODY.to_string(),
                    "Run Check",
                    move |_, cx| {
                        run.update(cx, |m, cx| m.run_security(false, cx));
                    },
                )
                .into_any_element()
            }
            Some(report) => {
                let rows = snap.rows.clone();
                let hovered = self.hovered.clone();
                let keyboard_row = self.keyboard_row.clone();
                let dim = snap.running.is_some();
                let list = div()
                    .id("security-list")
                    .debug_selector(|| "security-list".to_string())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.security_scroll)
                    .px(px(metrics::LIST_INSET))
                    .pt(px(2.))
                    .pb(px(8.))
                    .when(dim, |l| l.opacity(0.5))
                    .child(summary_strip(t, report, now))
                    .children(rows.iter().enumerate().map(|(ix, row)| match row {
                        SecRow::Check {
                            check,
                            updates_button,
                            first,
                        } => check_row(check, *updates_button, *first, ix, t, &model),
                        SecRow::Login(l) => {
                            let focus_key = format!("sec:{}", l.key);
                            let inside = keyboard_row.as_deref() == Some(focus_key.as_str());
                            let hovered = inside || hovered.as_deref() == Some(l.key.as_str());
                            let handle = RowFocus::handle(&focus_key, cx);
                            login_row_view(l, ix, hovered, handle, t, &view)
                        }
                        SecRow::Extension(e) => extension_row_view(e, ix, t),
                    }));
                list.into_any_element()
            }
        };

        let left = snap
            .report
            .as_ref()
            .map(|r| status_line(r, now))
            .unwrap_or_default();
        let status = self.status_bar(t, left, None);

        div()
            .debug_selector(|| "security".to_string())
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

/// 64 pt: the headline as the Figure and one line under it. It is the first child of the
/// list (inset 8), so 12 more puts its text at 20 like the other screens.
fn summary_strip(t: &Theme, report: &SecurityReport, now: u64) -> Div {
    div()
        .debug_selector(|| "summary".to_string())
        .flex_none()
        .h(px(metrics::SUMMARY_H))
        .px(px(12.))
        .flex()
        .flex_col()
        .justify_center()
        .child(
            div()
                .debug_selector(|| "figure".to_string())
                .text_size(px(figure::SIZE))
                .line_height(px(figure::LINE))
                .display(FontWeight::BOLD)
                .font_features(tnum())
                .text_color(t.text)
                .whitespace_nowrap()
                .child(report.headline()),
        )
        .child(
            div()
                .debug_selector(|| "summary-line".to_string())
                .text_size(px(12.))
                .line_height(px(16.))
                .text_color(t.text_2)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(summary_line(report, now)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use tidy_core::model::{ChromeExtension, SecurityReport};

    fn report() -> SecurityReport {
        let mut r = SecurityReport::empty(1_792_764_000);
        r.checks = CheckId::ALL
            .iter()
            .map(|id| CheckResult::new(*id, Status::Green, "ok"))
            .collect();
        r.login_items = vec![
            LoginItem {
                label: "com.example.helper".into(),
                plist: Some("/Users/demo/Library/LaunchAgents/com.example.helper.plist".into()),
                program: Some("/Applications/Example.app/Contents/MacOS/helper".into()),
                runs_at_load: true,
                scope: Scope::User,
                kind: LaunchKind::Agent,
                signer: Signer::Team("Example Inc".into()),
                notarized: Some(true),
                mark: Some(Mark::New),
            },
            LoginItem {
                label: "com.example.root".into(),
                plist: Some("/Library/LaunchDaemons/com.example.root.plist".into()),
                program: None,
                runs_at_load: false,
                scope: Scope::System,
                kind: LaunchKind::Daemon,
                signer: Signer::Unsigned,
                notarized: Some(false),
                mark: None,
            },
        ];
        r.extensions = vec![ChromeExtension {
            id: "a".repeat(32),
            name: "Reader".into(),
            version: Some("1.2".into()),
            profile: "Default".into(),
            enabled: false,
            mark: Some(Mark::New),
        }];
        r
    }

    fn state() -> SecurityState {
        SecurityState {
            report: Some(report()),
            ..SecurityState::default()
        }
    }

    #[test]
    fn items_sit_under_their_check_in_order() {
        let rows = security_rows(&state());
        let kinds: Vec<String> = rows
            .iter()
            .map(|r| match r {
                SecRow::Check { check, .. } => format!("C:{:?}", check.id),
                SecRow::Login(l) => format!("L:{}", l.label),
                SecRow::Extension(e) => format!("E:{}", e.name),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "C:FileVault",
                "C:Firewall",
                "C:Gatekeeper",
                "C:Sip",
                "C:XProtect",
                "C:MacosUpdates",
                "C:LoginItems",
                "L:com.example.helper",
                "L:com.example.root",
                "C:ChromeExtensions",
                "E:Reader"
            ]
        );
        assert!(matches!(rows[0], SecRow::Check { first: true, .. }));
        assert!(matches!(rows[1], SecRow::Check { first: false, .. }));
    }

    #[test]
    fn login_rows_show_signer_notarization_and_when_they_run() {
        let rows = security_rows(&state());
        let login = |label: &str| -> LoginRow {
            rows.iter()
                .find_map(|r| match r {
                    SecRow::Login(l) if l.label == label => Some(l.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let helper = login("com.example.helper");
        assert_eq!(
            helper.meta,
            [
                ("Example Inc".to_string(), false),
                ("Checked by Apple".to_string(), false),
                ("Starts automatically".to_string(), false)
            ]
        );
        assert_eq!(helper.mark, Some(Mark::New));
        assert!(helper.can_trash);
        assert_eq!(
            helper.path,
            "/Applications/Example.app/Contents/MacOS/helper"
        );
        let root = login("com.example.root");
        assert_eq!(root.meta[0], ("Not signed".to_string(), true));
        assert_eq!(root.meta[1], ("Not checked by Apple".to_string(), true));
        assert!(root.meta.iter().any(|(t, _)| t == "System service"));
        // No program listed: the plist is shown, and a daemon can't be trashed here.
        assert_eq!(root.path, "/Library/LaunchDaemons/com.example.root.plist");
        assert!(!root.can_trash);
    }

    #[test]
    fn a_running_check_shows_signatures_as_checking() {
        let mut s = state();
        if let Some(r) = s.report.as_mut() {
            r.login_items[0].signer = Signer::Unknown;
            r.login_items[0].notarized = None;
        }
        s.running = Some((3, 12));
        let rows = security_rows(&s);
        let helper = rows
            .iter()
            .find_map(|r| match r {
                SecRow::Login(l) if l.label == "com.example.helper" => Some(l.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(helper.meta[0], (CHECKING_SIGNATURE.to_string(), false));
        s.running = None;
        let rows = security_rows(&s);
        let helper = rows
            .iter()
            .find_map(|r| match r {
                SecRow::Login(l) if l.label == "com.example.helper" => Some(l.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(helper.meta[0], ("Unknown".to_string(), false));
    }

    #[test]
    fn trashed_items_leave_the_list() {
        let mut s = state();
        s.removed.insert("user:com.example.helper".into());
        let n = security_rows(&s)
            .iter()
            .filter(|r| matches!(r, SecRow::Login(_)))
            .count();
        assert_eq!(n, 1);
    }

    #[test]
    fn the_updates_row_has_a_button_until_it_was_checked() {
        let mut s = state();
        let has_button = |s: &SecurityState| {
            security_rows(s).iter().any(|r| {
                matches!(
                    r,
                    SecRow::Check {
                        check,
                        updates_button: true,
                        ..
                    } if check.id == CheckId::MacosUpdates
                )
            })
        };
        assert!(has_button(&s));
        s.report.as_mut().unwrap().updates_checked = true;
        assert!(!has_button(&s));
    }

    #[test]
    fn the_summary_line_says_first_check_or_compares() {
        let mut r = report();
        let now = r.at;
        assert_eq!(summary_line(&r, now), "");
        r.first_run = true;
        assert_eq!(summary_line(&r, now), FIRST_CHECK_SENTENCE);
        r.first_run = false;
        r.compared_to = Some("20261021-090000".into());
        assert_eq!(
            summary_line(&r, now),
            "New and changed since your last check"
        );
        r.compared_at = Some(now - 2 * 86_400);
        r.checks[5] = CheckResult::new(CheckId::MacosUpdates, Status::Gray, "Checked on demand.");
        assert_eq!(
            summary_line(&r, now),
            "New and changed since your check 2 days ago \u{b7} 1 not checked"
        );
    }

    #[test]
    fn the_status_line_counts_amber_checks_and_says_when() {
        let mut r = report();
        let now = r.at + 120;
        assert_eq!(
            status_line(&r, now),
            "8 checks, none to look at, checked 2 min ago"
        );
        r.checks[1] = CheckResult::new(CheckId::Firewall, Status::Amber, "off");
        assert_eq!(
            status_line(&r, now),
            "8 checks, 1 to look at, checked 2 min ago"
        );
        assert_eq!(r.headline(), "1 thing to look at");
    }

    #[test]
    fn copy_has_no_scare_words_or_em_dashes() {
        for s in [EMPTY_TITLE, EMPTY_BODY, RUNNING_BODY, CHECKING_SIGNATURE] {
            let lower = s.to_lowercase();
            for bad in [
                "\u{2014}",
                &["thre", "at"].concat(),
                &["infe", "cted"].concat(),
                &["dan", "ger"].concat(),
                &["risk", " score"].concat(),
            ] {
                assert!(!lower.contains(bad), "{s}");
            }
        }
    }
}
