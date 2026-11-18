//! The trust surfaces (design section 17): the details popover of a Cleanup item, the "System
//! Data on this Mac" sheet, and the "What Tidy never does" card on Check up.
//!
//! None of them moves anything. The details come from the rule table through
//! `tidy_core::explain`; the sheet asks the core to measure (read-only) and shows commands for
//! the user to copy, which Tidy never runs itself (design section 4, C30).

use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::api::Command;
use tidy_core::explain::{Guard, GuardState, explain_with};
use tidy_core::model::{Class, Item};
use tidy_core::sysdata::{self, CopyCommand, SysKind, SystemDataReport};

use crate::app::{TidyModel, now_unix, send};
use crate::format;
use crate::theme::{DisplayType, MONO_FONT, Theme, figure, motion as m, tnum};
use crate::views::main_window::TidyView;
use crate::views::overlay::{self, Phase, Sheet};
use crate::views::{KeyStop, glyph_tile, hairline, icon_button, tag, text_button};
use gpui_kit::base::FocusTrapElement as _;

// ---------- item details (17.1) ----------

/// The popover's width.
pub const DETAILS_W: f32 = 420.;
/// The label column of the popover's field list.
const DETAILS_LABEL_W: f32 = 80.;
/// "3 of 4 checks passed": the words that go with the dots of a Cleanup row and with the marks of
/// the details popover, so a check never reads by color or shape alone.
pub fn guard_count(passed: usize, total: usize) -> String {
    format!("{passed} of {total} checks passed")
}

/// The longest path the popover shows before it truncates the middle.
const DETAILS_PATH_MAX: usize = 60;

/// The open details popover: for which item, and where it was asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailsAt {
    pub id: String,
    pub at: Point<Pixels>,
}

/// One labelled field of the popover ("Checks", "Rule").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailField {
    pub label: &'static str,
    pub lines: Vec<String>,
    /// Paths and rule ids are drawn in mono.
    pub mono: bool,
}

/// Everything the popover draws for one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailView {
    pub name: String,
    pub tag: Option<&'static str>,
    pub size: String,
    pub verdict: String,
    pub class: Class,
    /// The checks that guard the move, each with whether it passes now. Empty for a Report
    /// only item.
    pub guards: Vec<Guard>,
    pub fields: Vec<DetailField>,
    /// The command a Report only row suggests, with a Copy button.
    pub command: Option<String>,
}

/// The popover's content for `item`, with paths shown under `root` as `~`.
pub fn detail_view(item: &Item, root: &Path, state: GuardState) -> DetailView {
    let e = explain_with(item, state);
    let mut fields = Vec::new();
    if !e.rebuild.is_empty() {
        fields.push(DetailField {
            label: "If removed",
            lines: vec![e.rebuild.clone()],
            mono: false,
        });
    }
    fields.push(DetailField {
        label: "Checks",
        lines: e.checks.clone(),
        mono: false,
    });
    let guards = e.guards.clone();
    let mut rule = vec![e.rule_id.clone()];
    rule.extend(e.patterns.iter().cloned());
    fields.push(DetailField {
        label: "Rule",
        lines: rule,
        mono: true,
    });
    fields.push(DetailField {
        label: "Path",
        lines: vec![format::middle_truncate(
            &format::display_path(&item.path, root),
            DETAILS_PATH_MAX,
        )],
        mono: true,
    });
    DetailView {
        name: item.name.clone(),
        tag: item.class.tag(),
        size: format::size(item.bytes_alloc),
        verdict: e.verdict,
        class: item.class,
        guards,
        fields,
        command: e.command,
    }
}

// ---------- System Data (17.2) ----------

/// The System Data sheet's state. It measures once each time it opens.
#[derive(Default)]
pub struct SystemDataState {
    pub open: bool,
    pub measuring: bool,
    pub report: Option<SystemDataReport>,
}

pub const SYSDATA_TITLE: &str = "System Data on this Mac";
pub const SYSDATA_ROW_TITLE: &str = "What is System Data on this Mac?";
pub const SYSDATA_ROW_NOTE: &str = "See what Tidy can measure without Full Disk Access.";
pub const SYSDATA_LINK: &str = "What's in System Data?";
pub const SYSDATA_W: f32 = 600.;
pub const SYSDATA_H: f32 = 520.;
/// The size column of the sheet's rows: wide enough for "Needs Full Disk Access".
const SYSDATA_SIZE_W: f32 = 140.;

impl TidyModel {
    /// Opens the System Data sheet and measures. Only a click gets here, never a launch or a
    /// check up.
    pub fn open_system_data(&mut self, cx: &mut Context<Self>) {
        self.system_data.open = true;
        if !self.system_data.measuring {
            let (reply, rx) = tokio::sync::oneshot::channel();
            if send(cx, Command::MeasureSystemData { reply }) {
                self.system_data.measuring = true;
                cx.spawn(async move |this, cx| {
                    let report = rx.await.ok();
                    let _ = this.update(cx, |m, cx| {
                        m.system_data.measuring = false;
                        if let Some(r) = report {
                            m.system_data.report = Some(r);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    pub fn close_system_data(&mut self, cx: &mut Context<Self>) {
        if self.system_data.open {
            self.system_data.open = false;
            cx.notify();
        }
    }
}

/// One row of the sheet, ready to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SysRowView {
    pub kind: Option<SysKind>,
    pub icon: IconName,
    pub title: String,
    pub note: String,
    /// "4.2 GB", or what stands in for a size ("Not measured").
    pub size: String,
    /// Whether `size` is a number (drawn tabular, 13) or a phrase (12).
    pub measured: bool,
    pub report_only: bool,
    pub commands: Vec<CopyCommand>,
}

pub fn sys_icon(kind: SysKind) -> IconName {
    match kind {
        SysKind::Caches => IconName::Folder,
        SysKind::Logs => IconName::FileText,
        SysKind::Xcode => IconName::SquareTerminal,
        SysKind::Simulators => IconName::Smartphone,
        SysKind::VirtualMachines => IconName::Layers,
        SysKind::IosBackups => IconName::TabletSmartphone,
        SysKind::Docker => IconName::Container,
        SysKind::Snapshots => IconName::Clock,
    }
}

/// The rows of a report, then the "Everything else" row, which has no size.
pub fn sysdata_rows(r: &SystemDataReport) -> Vec<SysRowView> {
    let mut rows: Vec<SysRowView> = r
        .rows
        .iter()
        .map(|row| SysRowView {
            kind: Some(row.kind),
            icon: sys_icon(row.kind),
            title: row.title.clone(),
            note: row.note.clone(),
            size: match row.size.label() {
                Some(label) => label.to_string(),
                None => format::size(row.size.bytes()),
            },
            measured: row.size.label().is_none(),
            report_only: row.report_only,
            commands: row.commands.clone(),
        })
        .collect();
    rows.push(SysRowView {
        kind: None,
        icon: IconName::Lock,
        title: sysdata::REST_TITLE.to_string(),
        note: sysdata::REST_NOTE.to_string(),
        size: String::new(),
        measured: false,
        report_only: false,
        commands: Vec::new(),
    });
    rows
}

/// The line under the figure: "Measured just now", or "Measuring" while a new one runs.
pub fn sysdata_line(r: Option<&SystemDataReport>, measuring: bool, now: u64) -> String {
    match (r, measuring) {
        (_, true) => "Measuring\u{2026}".to_string(),
        (Some(r), false) => format!("Measured {}", format::ago(now, r.at)),
        (None, false) => "Tidy couldn't measure this time.".to_string(),
    }
}

// ---------- What Tidy never does (17.3) ----------

pub const NEVER_TITLE: &str = "What Tidy never does";
/// The six items: what Tidy never does, and one plain sentence each.
pub const NEVER: [(&str, &str); 6] = [
    (
        "Ask for Full Disk Access",
        "It works with what macOS gives every app.",
    ),
    (
        "Run in the background",
        "Nothing starts at login or runs after you quit.",
    ),
    (
        "Ask for your password itself",
        "When something needs it, macOS or Finder asks. Tidy never sees it.",
    ),
    (
        "Delete outside the Trash",
        "Items go to the Trash. You can put them back until you empty it.",
    ),
    (
        "Empty the Trash on its own",
        "Emptying it is a separate step you confirm.",
    ),
    (
        "Use scare tactics",
        "No red badges, countdowns or alarm words.",
    ),
];

// ---------- drawing ----------

/// A command in mono on `fill` with a Copy button: "Copied" right after a click.
fn command_box(
    key: String,
    command: &str,
    copied: bool,
    t: &Theme,
    on_copy: impl Fn(&mut Window, &mut App) + 'static,
) -> Div {
    let surface = t.fill;
    div()
        .debug_selector({
            let key = key.clone();
            move || format!("command:{key}")
        })
        .min_w_0()
        .h(px(30.))
        .pl(px(10.))
        .pr(px(3.))
        .rounded(px(6.))
        .bg(surface)
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .font_family(MONO_FONT)
                .text_size(px(12.))
                .text_color(t.text)
                .child(command.to_string()),
        )
        .child(text_button(
            SharedString::from(format!("copy-{key}")),
            format!("copy:{key}"),
            if copied { "Copied" } else { "Copy" },
            false,
            true,
            &t.on(t.pop_bg.blend(surface)),
            on_copy,
        ))
}

impl TidyView {
    /// Opens the details popover of a Cleanup item at `at`.
    pub fn open_details(&mut self, id: String, at: Point<Pixels>, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        self.menu = None;
        self.copied = None;
        self.details = Some(DetailsAt { id, at });
        cx.notify();
    }

    /// Writes `command` to the clipboard and marks its button "Copied". Nothing runs it.
    pub fn copy_command(&mut self, key: String, command: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(command));
        self.copied = Some(key);
        cx.notify();
    }

    /// The details popover, drawn over the window next to the click.
    pub(crate) fn render_details(
        &mut self,
        t: &Theme,
        at: &DetailsAt,
        phase: Phase,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = {
            let c = &self.model.read(cx).cleanup;
            c.plan.as_ref().and_then(|p| {
                p.item(&at.id)
                    .map(|item| detail_view(item, &p.root, c.guard_state(&item.id)))
            })
        };
        let Some(d) = detail else {
            return div().into_any_element();
        };
        let leaving = matches!(phase, Phase::Leave(_));
        let view = cx.entity().downgrade();
        let close = view.clone();
        let on_pop = t.on(t.pop_bg);
        let copied = self.copied.as_deref() == Some("details");

        let head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(15.))
                    .line_height(px(20.))
                    .display(FontWeight::BOLD)
                    .text_color(t.text)
                    .child(d.name.clone()),
            )
            .children(d.tag.map(|label| tag(label, t)))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(px(15.))
                    .display(FontWeight::BOLD)
                    .font_features(tnum())
                    .text_color(t.text)
                    .whitespace_nowrap()
                    .child(d.size.clone()),
            );

        let guards = d.guards.clone();
        let passed = guards.iter().filter(|g| g.passed).count();
        let total = guards.len();
        let fields = d.fields.iter().map(|f| {
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(
                    div()
                        .flex_none()
                        .w(px(DETAILS_LABEL_W))
                        .flex()
                        .flex_col()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(t.text_2)
                        .child(f.label)
                        .when(f.label == "Checks" && !guards.is_empty(), |d| {
                            // The count in words, so the marks never carry it alone.
                            d.child(
                                div()
                                    .debug_selector(|| "details-checks-count".to_string())
                                    .text_color(if passed == total { t.text_2 } else { t.warning })
                                    .font_features(tnum())
                                    .child(guard_count(passed, total)),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .children(f.lines.iter().enumerate().map(|(i, line)| {
                            let el = div()
                                .text_size(px(12.))
                                .line_height(px(17.))
                                .child(line.clone());
                            if f.label == "Checks" {
                                // Each guard on its own line behind a mark: a check when it
                                // passes, a cross in amber when it does not. The count under
                                // "Checks" says it in words, so a mark never carries it alone.
                                let ok = guards.get(i).is_none_or(|g| g.passed);
                                return div()
                                    .flex()
                                    .items_start()
                                    .gap(px(6.))
                                    .child(
                                        div().flex_none().pt(px(3.)).child(
                                            Icon::new(if ok {
                                                IconName::Check
                                            } else {
                                                IconName::X
                                            })
                                            .size(px(11.))
                                            .text_color(if ok { t.success } else { t.warning }),
                                        ),
                                    )
                                    .child(el.flex_1().min_w_0().text_color(if ok {
                                        t.text
                                    } else {
                                        t.warning
                                    }));
                            }
                            if f.mono {
                                // The rule id in `text`, its patterns and the path in `text_2`.
                                el.font_family(MONO_FONT).text_color(
                                    if i == 0 && f.label == "Rule" {
                                        t.text
                                    } else {
                                        t.text_2
                                    },
                                )
                            } else {
                                el.text_color(t.text)
                            }
                        })),
                )
        });

        let command = d.command.clone().map(|cmd| {
            let view = view.clone();
            let to_copy = cmd.clone();
            command_box(
                "details".to_string(),
                &cmd,
                copied,
                &on_pop,
                move |_, cx| {
                    let c = to_copy.clone();
                    let _ = view.update(cx, |v, cx| v.copy_command("details".to_string(), c, cx));
                },
            )
        });

        let card = div()
            .id("details")
            .debug_selector(|| "details".to_string())
            .w(px(DETAILS_W))
            .p(px(14.))
            .rounded(px(10.))
            .bg(t.pop_bg)
            .shadow(t.pop_shadow())
            .flex()
            .flex_col()
            .gap(px(10.))
            .when(!leaving, |d| {
                d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            })
            .child(head)
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(
                        div().flex_none().pt(px(1.)).child(
                            Icon::new(match d.class {
                                Class::Safe => IconName::CircleCheck,
                                Class::Review => IconName::Info,
                                Class::ReportOnly => IconName::Lock,
                            })
                            .size(px(16.))
                            .text_color(match d.class {
                                Class::Safe => t.success,
                                _ => t.text_2,
                            }),
                        ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "details-verdict".to_string())
                            .flex_1()
                            .min_w_0()
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .display(FontWeight::BOLD)
                            .text_color(t.text)
                            .child(d.verdict.clone()),
                    ),
            )
            .child(hairline(t))
            .child(div().flex().flex_col().gap(px(8.)).children(fields))
            .children(command);

        // The leaving copy only fades: it has no scrim and takes no clicks.
        let scrim = (!leaving).then(|| {
            div()
                .id("details-scrim")
                .absolute()
                .inset_0()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    let _ = close.update(cx, |v, cx| {
                        v.details = None;
                        cx.notify();
                    });
                })
        });
        div()
            .absolute()
            .inset_0()
            .children(scrim)
            .child(deferred(
                anchored()
                    .anchor(Anchor::TopRight)
                    .position(at.at)
                    .offset(point(px(0.), px(14.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(if leaving {
                        overlay::present_popover(card, phase)
                    } else {
                        let card = card.focus_trap("details-trap", &self.traps.details);
                        overlay::present_popover(card, phase)
                    }),
            ))
            .into_any_element()
    }

    /// The System Data sheet: History's chrome, the measured rows, the report-only rows with
    /// their commands, and the row about everything else.
    pub(crate) fn render_system_data(
        &mut self,
        t: &Theme,
        phase: Phase,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The first report of this opening fades in over "Measuring"; a re-measure over a
        // report does not (its figures stay readable, only dimmed while measuring).
        let first_report = overlay::phase_for(self.overlays.report_arrived, cx.reduce_motion());
        // A leaving copy only fades: it takes no clicks.
        let leaving = matches!(phase, Phase::Leave(_));
        let now = now_unix(cx);
        let model = self.model.clone();
        let close_model = model.clone();
        let scrim_model = model.clone();
        let view = cx.entity().downgrade();
        let (report, measuring) = {
            let s = &self.model.read(cx).system_data;
            (s.report.clone(), s.measuring)
        };
        let on_card = t.on(t.pop_bg);

        let figure_row = div()
            .debug_selector(|| "sysdata-figure".to_string())
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(figure::SIZE))
                            .line_height(px(figure::LINE))
                            .display(FontWeight::BOLD)
                            .font_features(tnum())
                            .text_color(t.text)
                            .whitespace_nowrap()
                            .child(match &report {
                                Some(r) => format::size(r.measured_bytes()),
                                None => "Measuring".to_string(),
                            }),
                    )
                    .when(report.is_some(), |d| {
                        d.child(
                            div()
                                .text_size(px(15.))
                                .text_color(t.text_2)
                                .whitespace_nowrap()
                                .child("that Tidy can measure"),
                        )
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "sysdata-line".to_string())
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .font_features(tnum())
                    .text_color(t.text_2)
                    .child(sysdata_line(report.as_ref(), measuring, now)),
            );

        let rows = report.as_ref().map(sysdata_rows).unwrap_or_default();
        let copied = self.copied.clone();
        let row_els = rows.iter().enumerate().map(|(n, row)| {
            let key_base = row
                .kind
                .map_or("rest".to_string(), |k| format!("{k:?}").to_lowercase());
            // A fixed size column keeps every row's text and command boxes the same width.
            let size = div().flex_none().w(px(SYSDATA_SIZE_W)).flex().justify_end();
            let size = size.children((!row.size.is_empty()).then(|| {
                let el = div().text_right().whitespace_nowrap().text_color(t.text_2);
                if row.measured {
                    el.text_size(px(14.))
                        .display(FontWeight::BOLD)
                        .text_color(t.text)
                        .font_features(tnum())
                        .child(row.size.clone())
                } else {
                    el.text_size(px(12.)).child(row.size.clone())
                }
            }));
            let commands = row.commands.iter().enumerate().map(|(i, c)| {
                let key = format!("{key_base}-{i}");
                let is_copied = copied.as_deref() == Some(key.as_str());
                let view = view.clone();
                let (k2, cmd) = (key.clone(), c.command.clone());
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(t.text_2)
                            .child(c.label.clone()),
                    )
                    .child(command_box(
                        key,
                        &c.command,
                        is_copied,
                        &on_card,
                        move |_, cx| {
                            let (k, c) = (k2.clone(), cmd.clone());
                            let _ = view.update(cx, |v, cx| v.copy_command(k, c, cx));
                        },
                    ))
            });
            div()
                .debug_selector({
                    let key = key_base.clone();
                    move || format!("sysdata-row:{key}")
                })
                .relative()
                .py(px(12.))
                .flex()
                .items_start()
                .gap(px(12.))
                .when(n > 0, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .child(hairline(t)),
                    )
                })
                .child(glyph_tile(row.icon, 28., 6., 16., t))
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
                                        .line_height(px(18.))
                                        .display(FontWeight::SEMIBOLD)
                                        .text_color(t.text)
                                        .whitespace_nowrap()
                                        .child(row.title.clone()),
                                )
                                .when(row.report_only, |d| d.child(tag("Report only", t))),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .text_color(t.text_2)
                                .child(row.note.clone()),
                        )
                        .when(!row.commands.is_empty(), |d| {
                            d.child(
                                div()
                                    .mt(px(6.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(8.))
                                    .children(commands),
                            )
                        }),
                )
                .child(size)
        });

        let body: AnyElement = if report.is_none() {
            let spinner = Icon::new(IconName::LoaderCircle)
                .size(px(16.))
                .text_color(t.text_2);
            let spinner = if cx.reduce_motion() || !measuring {
                spinner.into_any_element()
            } else {
                spinner
                    .with_animation(
                        "sysdata-spinner",
                        Animation::new(m::SPIN).repeat(),
                        |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
                    )
                    .into_any_element()
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(24.))
                .text_size(px(13.))
                .text_color(t.text_2)
                .when(measuring, |d| d.child(spinner))
                .child(sysdata_line(None, measuring, now))
                .into_any_element()
        } else {
            div()
                .debug_selector(|| "sysdata-rows".to_string())
                .flex()
                .flex_col()
                .when(measuring, |d| d.opacity(0.5))
                .children(row_els)
                .into_any_element()
        };

        let content = div()
            .id("sysdata-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.sysdata_scroll)
            .px(px(16.))
            .pt(px(14.))
            .pb(px(16.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(px(18.))
                    .text_color(t.text)
                    .child(sysdata::LEAD),
            )
            .child(overlay::fade_in(
                "sysdata-figure-in",
                figure_row,
                first_report,
                m::CARD_IN,
            ))
            .child(overlay::fade_in(
                "sysdata-rows-in",
                body,
                first_report,
                m::CARD_IN,
            ))
            .when(report.is_some(), |d| {
                d.child(overlay::fade_in(
                    "sysdata-footer-in",
                    div()
                        .debug_selector(|| "sysdata-footer".to_string())
                        .pt(px(10.))
                        .relative()
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .child(hairline(t)),
                        )
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(t.text_2)
                        .child(sysdata::FOOTER),
                    first_report,
                    m::CARD_IN,
                ))
            });

        let card = div()
            .id("sysdata-card")
            .debug_selector(|| "system-data".to_string())
            .w(px(SYSDATA_W))
            .h(px(SYSDATA_H))
            .max_w_full()
            .max_h_full()
            .rounded(px(12.))
            .bg(t.pop_bg)
            .shadow(t.pop_shadow())
            .overflow_hidden()
            .flex()
            .flex_col()
            .when(!leaving, |d| {
                d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            })
            .child(
                div()
                    .flex_none()
                    .h(px(44.))
                    .pl(px(16.))
                    .pr(px(10.))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(17.))
                            .display(FontWeight::BOLD)
                            .text_color(t.text)
                            .child(SYSDATA_TITLE),
                    )
                    .child(icon_button(
                        "sysdata-close",
                        "sysdata-close".to_string(),
                        IconName::X,
                        &t.on(t.pop_bg),
                        move |_, cx| {
                            close_model.update(cx, |m, cx| m.close_system_data(cx));
                        },
                    )),
            )
            .child(hairline(t))
            .child(content);

        let scrim = div()
            .id("sysdata-scrim")
            .absolute()
            .inset_0()
            .bg(t.scrim())
            .flex()
            .items_center()
            .justify_center()
            .when(!leaving, |d| {
                d.on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    scrim_model.update(cx, |m, cx| m.close_system_data(cx));
                })
            });
        // Tab and Shift-Tab stay inside the sheet while it is open.
        if leaving {
            overlay::present(Sheet::SystemData, scrim, card, phase)
        } else {
            let card = card.focus_trap("sysdata-trap", &self.traps.system_data);
            overlay::present(Sheet::SystemData, scrim, card, phase)
        }
    }

    /// The Check up row that opens the sheet: a settings row with a chevron.
    pub(crate) fn system_data_row(&self, t: &Theme) -> Stateful<Div> {
        let model = self.model.clone();
        let hover = t.card_hover();
        div()
            .id("sysdata-open")
            .debug_selector(|| "sysdata-open".to_string())
            .mx(px(20.))
            .mt(px(12.))
            .px(px(14.))
            .py(px(12.))
            .rounded(px(10.))
            .bg(t.card_bg())
            .flex()
            .items_center()
            .gap(px(12.))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_click(move |_, _, cx| {
                model.update(cx, |m, cx| m.open_system_data(cx));
            })
            .key_stop(t)
            .child(
                Icon::new(IconName::HardDrive)
                    .size(px(16.))
                    .text_color(t.text_2),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(17.))
                            .display(FontWeight::SEMIBOLD)
                            .text_color(t.text)
                            .child(SYSDATA_ROW_TITLE),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(16.))
                            .text_color(t.text_2)
                            .child(SYSDATA_ROW_NOTE),
                    ),
            )
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(12.))
                    .text_color(t.text_3),
            )
    }

    /// "What Tidy never does": a heading and one card with the six items in two columns.
    pub(crate) fn never_card(&self, t: &Theme) -> Div {
        let item = |(title, note): (&str, &str)| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_start()
                .gap(px(10.))
                .child(
                    div()
                        .flex_none()
                        .pt(px(2.))
                        .child(Icon::new(IconName::Ban).size(px(14.)).text_color(t.text_3)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(17.))
                                .display(FontWeight::SEMIBOLD)
                                .text_color(t.text)
                                .child(title.to_string()),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .text_color(t.text_2)
                                .child(note.to_string()),
                        ),
                )
        };
        let rows = NEVER.chunks(2).map(|pair| {
            div()
                .flex()
                .items_start()
                .gap(px(16.))
                .children(pair.iter().copied().map(item))
        });
        div()
            .debug_selector(|| "never".to_string())
            .mx(px(20.))
            .mt(px(20.))
            .mb(px(20.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(px(17.))
                    .display(FontWeight::BOLD)
                    .text_color(t.text)
                    .child(NEVER_TITLE),
            )
            .child(
                div()
                    .p(px(14.))
                    .rounded(px(10.))
                    .bg(t.card_bg())
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::path::PathBuf;
    use tidy_core::model::{Class, Group};
    use tidy_core::sysdata::{SysRow, SysSize};

    fn item(id: &str, class: Class, reason: &str) -> Item {
        Item {
            id: id.to_string(),
            group: Group::Caches,
            class,
            ticked: class == Class::Safe,
            blocked: None,
            name: "Spotify".into(),
            reason: reason.into(),
            path: PathBuf::from("/Users/demo").join(id.split_once(':').unwrap().1),
            bytes_alloc: 1_400_000_000,
            bytes_logical: 1_400_000_000,
            newest_mtime: None,
            members: vec![],
        }
    }

    #[test]
    fn a_safe_row_shows_its_reason_rule_pattern_and_what_rebuilds_it() {
        let d = detail_view(
            &item(
                "cache.app:Library/Caches/com.spotify.client",
                Class::Safe,
                "Cache, rebuilt by the app",
            ),
            Path::new("/Users/demo"),
            GuardState::default(),
        );
        assert_eq!(d.name, "Spotify");
        assert_eq!(d.tag, None);
        assert_eq!(d.size, "1.4 GB");
        assert_eq!(d.verdict, "Safe to clean. Cache, rebuilt by the app.");
        let labels: Vec<&str> = d.fields.iter().map(|f| f.label).collect();
        assert_eq!(labels, ["If removed", "Checks", "Rule", "Path"]);
        assert_eq!(d.fields[2].lines[0], "cache.app");
        assert!(
            d.fields[2]
                .lines
                .contains(&"Library/Caches/<app>".to_string())
        );
        assert_eq!(d.fields[3].lines, ["~/Library/Caches/com.spotify.client"]);
        assert!(d.fields[2].mono && d.fields[3].mono && !d.fields[1].mono);
        assert_eq!(d.command, None);
    }

    #[test]
    fn a_report_only_row_offers_its_command_to_copy() {
        let d = detail_view(
            &item(
                "dev.pnpm-store:Library/pnpm/store",
                Class::ReportOnly,
                "Reclaim unused packages with: pnpm store prune",
            ),
            Path::new("/Users/demo"),
            GuardState::default(),
        );
        assert_eq!(d.tag, Some("Report only"));
        assert!(d.verdict.starts_with("Tidy won't remove this."));
        assert_eq!(d.command.as_deref(), Some("pnpm store prune"));
        let checks = d.fields.iter().find(|f| f.label == "Checks").unwrap();
        assert_eq!(checks.lines, ["Tidy never moves it"]);
    }

    #[test]
    fn long_paths_are_truncated_in_the_middle() {
        let d = detail_view(
            &item(
                "proj.node_modules:Documents/projects/a-very-long-project-name/packages/web-client/node_modules",
                Class::Safe,
                "Build output",
            ),
            Path::new("/Users/demo"),
            GuardState::default(),
        );
        let path = &d.fields.last().unwrap().lines[0];
        assert!(path.chars().count() <= DETAILS_PATH_MAX, "{path}");
        assert!(path.starts_with("~/") && path.ends_with("node_modules"));
    }

    fn report() -> SystemDataReport {
        let row = |kind: SysKind, size: SysSize, cmds: Vec<CopyCommand>| SysRow {
            kind,
            title: kind.title().to_string(),
            note: "note".into(),
            size,
            report_only: kind.report_only(),
            commands: cmds,
        };
        SystemDataReport {
            at: 1_000,
            rows: vec![
                row(SysKind::Caches, SysSize::Measured(3_000_000_000), vec![]),
                row(SysKind::IosBackups, SysSize::NeedsFullDiskAccess, vec![]),
                row(
                    SysKind::Docker,
                    SysSize::NotMeasured,
                    vec![CopyCommand {
                        label: "See".into(),
                        command: "docker system df".into(),
                    }],
                ),
                row(SysKind::Snapshots, SysSize::NotReported, vec![]),
            ],
        }
    }

    #[test]
    fn sheet_rows_show_sizes_or_honest_phrases_and_end_with_the_rest() {
        let rows = sysdata_rows(&report());
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].size, "3 GB");
        assert!(rows[0].measured);
        assert_eq!(rows[1].size, "Needs Full Disk Access");
        assert!(!rows[1].measured);
        assert_eq!(rows[2].size, "Not measured");
        assert!(rows[2].report_only);
        assert_eq!(rows[2].commands[0].command, "docker system df");
        assert_eq!(rows[3].size, "Size not reported");
        let rest = rows.last().unwrap();
        assert_eq!(rest.kind, None);
        assert_eq!(rest.title, sysdata::REST_TITLE);
        assert!(rest.size.is_empty() && !rest.report_only);
        assert_eq!(report().measured_bytes(), 3_000_000_000);
    }

    #[test]
    fn the_sheet_line_says_when_it_measured() {
        let r = report();
        assert_eq!(sysdata_line(Some(&r), false, 1_010), "Measured just now");
        assert_eq!(sysdata_line(Some(&r), true, 1_010), "Measuring\u{2026}");
        assert_eq!(sysdata_line(None, true, 0), "Measuring\u{2026}");
        assert!(sysdata_line(None, false, 0).starts_with("Tidy couldn't"));
    }

    #[test]
    fn the_never_list_names_the_six_promises_in_plain_words() {
        let titles: Vec<&str> = NEVER.iter().map(|(t, _)| *t).collect();
        assert_eq!(
            titles,
            [
                "Ask for Full Disk Access",
                "Run in the background",
                "Ask for your password itself",
                "Delete outside the Trash",
                "Empty the Trash on its own",
                "Use scare tactics"
            ]
        );
        let mut all: Vec<&str> = vec![
            NEVER_TITLE,
            SYSDATA_TITLE,
            SYSDATA_ROW_TITLE,
            SYSDATA_ROW_NOTE,
            SYSDATA_LINK,
        ];
        for (t, n) in NEVER {
            all.push(t);
            all.push(n);
        }
        for s in all {
            assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'), "{s}");
            assert!(!s.contains('!'), "{s}");
        }
    }
}
