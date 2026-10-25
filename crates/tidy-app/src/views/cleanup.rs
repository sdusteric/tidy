//! The Cleanup screen (design sections 5.1 and 11): an empty state before the first scan, then
//! five groups of items in a `uniform_list` where group headers and item rows share one 44 pt
//! height. The screen only sends commands; the core moves the files.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::model::{Class, Group};

use crate::app::{CleanupState, OpenKind, TidyModel, Tri, now_unix, open_path};
use crate::format;
use crate::theme::{Theme, figure, metrics, motion as m, tnum};
use crate::views::dialogs::DialogSpec;
use crate::views::main_window::TidyView;
use crate::views::{
    checkbox, glyph_tile, icon_button, plain_button, scroll_edge, tag, text_button,
};

pub const EMPTY_FIRST_TITLE: &str = "Find space you can get back";
pub const EMPTY_FIRST_BODY: &str = "Tidy looks at caches, old logs, old build output and package caches. Nothing moves until you choose, and everything goes to the Trash.";
pub const EMPTY_DONE_TITLE: &str = "Nothing to clean right now";
pub const EMPTY_DONE_BODY: &str =
    "Caches of apps that are open aren't counted. Quit them and scan again.";

/// An entry of the "..." menu on a Cleanup row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuEntry {
    /// Opens the item's details popover (design 17.1).
    ShowDetails,
    /// Adds the path to the ignore list in settings.
    NeverShow,
    CopyPath,
}

impl MenuEntry {
    pub fn label(self) -> &'static str {
        match self {
            MenuEntry::ShowDetails => "Show Details",
            MenuEntry::NeverShow => "Never show this again",
            MenuEntry::CopyPath => "Copy Path",
        }
    }

    /// The entries, in menu order.
    pub fn all() -> Vec<MenuEntry> {
        vec![
            MenuEntry::ShowDetails,
            MenuEntry::NeverShow,
            MenuEntry::CopyPath,
        ]
    }

    fn selector(self) -> &'static str {
        match self {
            MenuEntry::ShowDetails => "menu-show-details",
            MenuEntry::NeverShow => "menu-never-show",
            MenuEntry::CopyPath => "menu-copy-path",
        }
    }
}

/// The open "..." menu: for which item, with which entries, where it was clicked.
#[derive(Clone, Debug)]
pub struct RowMenu {
    pub id: String,
    pub path: PathBuf,
    pub entries: Vec<MenuEntry>,
    pub at: Point<Pixels>,
}

/// One row of the list. Built from the state on every render.
#[derive(Clone, Debug, PartialEq)]
pub enum CleanRow {
    Header {
        group: Group,
        expanded: bool,
        /// None when no item of the group can be ticked.
        tri: Option<Tri>,
        count: usize,
        bytes: u64,
        first: bool,
    },
    Item(ItemRow),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemRow {
    pub id: String,
    pub group: Group,
    pub name: String,
    /// The meta line, and whether it is drawn in the warning color.
    pub meta: String,
    pub warn: bool,
    pub bytes: u64,
    pub class: Class,
    pub ticked: bool,
    pub enabled: bool,
    pub leaving: bool,
    pub path: PathBuf,
}

impl ItemRow {
    pub fn tag(&self) -> Option<&'static str> {
        self.class.tag()
    }
}

/// The glyph of a group's tiles.
pub fn group_icon(group: Group) -> IconName {
    match group {
        Group::Caches => IconName::Folder,
        Group::Logs => IconName::FileText,
        Group::Developer => IconName::SquareTerminal,
        Group::AiModels => IconName::Cpu,
        Group::Installers => IconName::Disc,
        Group::Trash => IconName::Trash,
    }
}

/// The rows to show: for each group with items left, a header, then its items unless the group
/// is collapsed.
pub fn cleanup_rows(c: &CleanupState) -> Vec<CleanRow> {
    let mut rows = Vec::new();
    for (n, group) in c.groups().into_iter().enumerate() {
        let items = c.visible_items(group);
        let expanded = !c.collapsed.contains(&group);
        rows.push(CleanRow::Header {
            group,
            expanded,
            tri: c.group_tri(group),
            count: items.len(),
            bytes: c.group_bytes(group),
            first: n == 0,
        });
        if !expanded {
            continue;
        }
        for item in items {
            let (meta, warn) = if let Some(reason) = c.skipped.get(&item.id) {
                (reason.clone(), true)
            } else if let Some(reason) = c.blocked.get(&item.id) {
                (reason.clone(), true)
            } else if item.class == Class::ReportOnly {
                let text = if item.reason.is_empty() {
                    "Tidy won't remove this".to_string()
                } else {
                    format!("Tidy won't remove this. {}", item.reason)
                };
                (text, false)
            } else {
                (item.reason.clone(), false)
            };
            rows.push(CleanRow::Item(ItemRow {
                id: item.id.clone(),
                group,
                name: item.name.clone(),
                meta,
                warn,
                bytes: item.bytes_alloc,
                class: item.class,
                ticked: c.is_ticked(item),
                enabled: c.enabled(item),
                leaving: c.leaving.contains(&item.id),
                path: item.path.clone(),
            }));
        }
    }
    rows
}

/// What a row needs besides its own data.
struct RowEnv {
    model: Entity<TidyModel>,
    view: WeakEntity<TidyView>,
    /// The key of the hovered row (or the one a demo forces).
    hovered: Option<String>,
    reduce_motion: bool,
}

fn render_row(row: &CleanRow, ix: usize, t: &Theme, env: &RowEnv) -> AnyElement {
    match row {
        CleanRow::Header {
            group,
            expanded,
            tri,
            count,
            bytes,
            first,
        } => render_header(*group, *expanded, *tri, *count, *bytes, *first, ix, t, env),
        CleanRow::Item(item) => render_item(item, ix, t, env),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_header(
    group: Group,
    expanded: bool,
    tri: Option<Tri>,
    count: usize,
    bytes: u64,
    first: bool,
    ix: usize,
    t: &Theme,
    env: &RowEnv,
) -> AnyElement {
    let hover = t.row_hover;
    let toggle_model = env.model.clone();
    let check_model = env.model.clone();
    div()
        .id(("clean-head", ix))
        .debug_selector(move || format!("group-{group:?}"))
        .relative()
        .w_full()
        .h(px(metrics::CLEANUP_ROW_H))
        .px(px(10.))
        .gap(px(8.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .when(!first, |d| {
            d.child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(10.))
                    .right(px(10.))
                    .h(px(1.))
                    .bg(t.separator),
            )
        })
        .on_click(move |_, _, cx| {
            toggle_model.update(cx, |m, cx| m.toggle_collapsed(group, cx));
        })
        .child(
            Icon::new(if expanded {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .size(px(12.))
            .text_color(t.text_3),
        )
        .child(checkbox(
            ("group-check", ix),
            format!("group-check-{group:?}"),
            tri.unwrap_or(Tri::Off),
            tri.is_some(),
            t,
            move |_, cx| {
                check_model.update(cx, |m, cx| m.toggle_group(group, cx));
            },
        ))
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text)
                .whitespace_nowrap()
                .child(group.label()),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(format::items(count)),
        )
        .child(div().flex_1())
        .child(
            div()
                .debug_selector(move || format!("group-size-{group:?}"))
                .flex_none()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .font_features(tnum())
                .text_color(t.text)
                .child(format::size(bytes)),
        )
        // The hover-action area of the item rows, so group totals line up with item sizes.
        .child(div().flex_none().w(px(metrics::CLEAN_TRAIL_W)))
        .into_any_element()
}

fn render_item(item: &ItemRow, ix: usize, t: &Theme, env: &RowEnv) -> AnyElement {
    let id = item.id.clone();
    let hovered = env.hovered.as_deref() == Some(id.as_str());
    let hover_bg = t.row_hover;
    let row_model = env.model.clone();
    let check_model = env.model.clone();
    let hover_view = env.view.clone();
    let reveal_path = item.path.clone();
    let menu_view = env.view.clone();
    let menu_path = item.path.clone();
    let details_view = env.view.clone();
    let details_id = id.clone();
    let details_selector = format!("clean-details:{id}");
    let (row_id, check_id, menu_row_id, hover_key) =
        (id.clone(), id.clone(), id.clone(), id.clone());
    let selector_id = id.clone();
    let reveal_selector = format!("clean-reveal:{id}");
    let more_selector = format!("clean-more:{id}");

    let lead: AnyElement = if item.class.selectable() {
        checkbox(
            ("clean-check", ix),
            format!("clean-check:{id}"),
            if item.ticked { Tri::On } else { Tri::Off },
            item.enabled,
            t,
            move |_, cx| {
                check_model.update(cx, |m, cx| m.toggle_item(&check_id, cx));
            },
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

    let actions = hovered.then(|| {
        div()
            .flex()
            .items_center()
            .child(icon_button(
                ("clean-details", ix),
                details_selector,
                IconName::Info,
                t,
                move |window, cx| {
                    let at = window.mouse_position();
                    let id = details_id.clone();
                    let _ = details_view.update(cx, |v, cx| v.open_details(id, at, cx));
                },
            ))
            .child(icon_button(
                ("clean-reveal", ix),
                reveal_selector,
                IconName::FolderSearch,
                t,
                move |_, cx| open_path(OpenKind::Reveal, &reveal_path, cx),
            ))
            .child(icon_button(
                ("clean-more", ix),
                more_selector,
                IconName::Ellipsis,
                t,
                move |window, cx| {
                    let at = window.mouse_position();
                    let (id, path) = (menu_row_id.clone(), menu_path.clone());
                    let _ = menu_view.update(cx, |v, cx| v.open_row_menu(id, path, at, cx));
                },
            ))
    });

    let el = div()
        .id(("clean-item", ix))
        .debug_selector(move || format!("clean-row:{selector_id}"))
        .w_full()
        .h(px(metrics::CLEANUP_ROW_H))
        // 10 + chevron 12 + gap 8: the checkbox sits under the group's checkbox.
        .pl(px(30.))
        .pr(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .when(hovered, |d| d.bg(hover_bg))
        .on_hover(move |h, _, cx| {
            let key = hover_key.clone();
            let h = *h;
            let _ = hover_view.update(cx, |v, cx| v.set_hovered(&key, h, cx));
        })
        .on_click(move |_, _, cx| {
            row_model.update(cx, |m, cx| m.toggle_item(&row_id, cx));
        })
        .child(lead)
        .child(glyph_tile(group_icon(item.group), 28., 6., 16., t))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    // The tag sits right after the name, as Haul's quality tag does.
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(13.))
                                .line_height(px(17.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.text)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(item.name.clone()),
                        )
                        .children(item.tag().map(|label| tag(label, t))),
                )
                .child(
                    div()
                        .debug_selector({
                            let key = format!("clean-meta:{id}");
                            move || key
                        })
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(if item.warn { t.warning } else { t.text_2 })
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(item.meta.clone()),
                ),
        )
        .child(
            div()
                .flex_none()
                .w(px(76.))
                .text_right()
                .text_size(px(13.))
                .font_features(tnum())
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(format::size(item.bytes)),
        )
        .child(
            div()
                .flex_none()
                .w(px(metrics::CLEAN_TRAIL_W))
                .flex()
                .justify_end()
                .children(actions),
        );
    if item.leaving && !env.reduce_motion {
        // The row fades over 140 ms as its item reaches the Trash; the model removes it then.
        el.with_animation(
            ("clean-out", ix),
            Animation::new(m::REMOVE).with_easing(m::curve(m::exit())),
            |el, t| el.opacity(1. - t),
        )
        .into_any_element()
    } else {
        el.into_any_element()
    }
}

impl TidyView {
    /// Opens the "..." menu of a Cleanup row at `at`.
    pub fn open_row_menu(
        &mut self,
        id: String,
        path: PathBuf,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        self.menu = Some(RowMenu {
            id,
            path,
            entries: MenuEntry::all(),
            at,
        });
        cx.notify();
    }

    fn run_menu_entry(&mut self, entry: MenuEntry, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        match entry {
            MenuEntry::ShowDetails => {
                self.open_details(menu.id, menu.at, cx);
            }
            MenuEntry::NeverShow => {
                self.model.update(cx, |m, cx| m.ignore_item(&menu.id, cx));
            }
            MenuEntry::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    menu.path.to_string_lossy().into_owned(),
                ));
            }
        }
        cx.notify();
    }

    /// The primary button: clean the ticked items. With a Review item ticked it asks first.
    pub fn request_clean(&mut self, cx: &mut Context<Self>) {
        let spec = {
            let c = &self.model.read(cx).cleanup;
            let ticked = c.ticked_items();
            if ticked.is_empty() || c.cleaning.is_some() {
                return;
            }
            if ticked.iter().any(|i| i.class == Class::Review) {
                Some(DialogSpec::review(&ticked))
            } else {
                None
            }
        };
        match spec {
            Some(spec) => self.open_dialog(spec, cx),
            None => {
                self.model.update(cx, |m, cx| m.clean_ticked(cx));
            }
        }
    }

    /// The "..." menu, drawn over the window at the click.
    pub(crate) fn render_row_menu(
        &mut self,
        t: &Theme,
        menu: &RowMenu,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let close = view.clone();
        let hover = t.row_hover;
        let card = div()
            .debug_selector(|| "row-menu".to_string())
            .min_w(px(180.))
            .p(px(4.))
            .rounded(px(8.))
            .bg(t.pop_bg)
            .shadow(t.pop_shadow())
            .flex()
            .flex_col()
            .children(menu.entries.iter().enumerate().map(|(i, entry)| {
                let entry = *entry;
                let view = view.clone();
                div()
                    .id(("menu-item", i))
                    .debug_selector(move || entry.selector().to_string())
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
                    .child(entry.label())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        let _ = view.update(cx, |v, cx| v.run_menu_entry(entry, cx));
                    })
            }));
        div()
            .absolute()
            .inset_0()
            .child(div().id("menu-scrim").absolute().inset_0().on_mouse_down(
                MouseButton::Left,
                move |_, _, cx| {
                    let _ = close.update(cx, |v, cx| {
                        v.menu = None;
                        cx.notify();
                    });
                },
            ))
            .child(deferred(
                anchored()
                    .position(menu.at)
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            ))
            .into_any_element()
    }

    /// The state the Cleanup screen draws, copied out of the model.
    pub(crate) fn cleanup_snapshot(&self, cx: &App) -> CleanupView {
        let c = &self.model.read(cx).cleanup;
        CleanupView {
            rows: Rc::new(cleanup_rows(c)),
            has_plan: c.plan.is_some(),
            scanning: c.scanning,
            cleaning: c.cleaning,
            ticked_count: c.ticked_items().len(),
            ticked_bytes: c.ticked_bytes(),
            any_review: c.any_review_ticked(),
            review_bytes: c.review_bytes(),
            scanned_at: c.plan.as_ref().map(|p| p.scanned_at),
            notice: c.notice.clone(),
            notice_leaving: c.notice_leaving.clone(),
            trash: c.trash,
            empty_result: c.is_empty_result(),
        }
    }

    /// The whole Cleanup screen: toolbar, notice, summary, list or empty state, status bar.
    pub(crate) fn render_cleanup(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = now_unix(cx);
        let snap = self.cleanup_snapshot(cx);
        let scrolled = self.cleanup_scroll.0.borrow().base_handle.offset().y < px(0.);
        let model = self.model.clone();
        let view = cx.entity().downgrade();

        let controls = {
            let history_view = view.clone();
            let rescan_model = model.clone();
            let clean_view = view.clone();
            let stop_model = model.clone();
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(match snap.scanning {
                    Some(files) => self
                        .scanning_controls(
                            t,
                            files,
                            move |_, cx| {
                                stop_model.update(cx, |m, cx| m.stop_scan(cx));
                            },
                            cx,
                        )
                        .into_any_element(),
                    None => div().into_any_element(),
                })
                .child(icon_button(
                    "history",
                    "history-button".to_string(),
                    IconName::Clock,
                    t,
                    move |_, cx| {
                        let _ = history_view.update(cx, |v, cx| v.open_history(cx));
                    },
                ))
                .when(snap.has_plan && snap.scanning.is_none(), |d| {
                    d.child(icon_button(
                        "rescan",
                        "rescan-button".to_string(),
                        IconName::RefreshCw,
                        t,
                        move |_, cx| {
                            rescan_model.update(cx, |m, cx| m.scan_cleanup(cx));
                        },
                    ))
                })
                .when(snap.has_plan && !snap.empty_result, |d| {
                    d.child(text_button(
                        "clean",
                        "clean-button".to_string(),
                        snap.button_label(),
                        true,
                        snap.can_clean(),
                        t,
                        move |_, cx| {
                            let _ = clean_view.update(cx, |v, cx| v.request_clean(cx));
                        },
                    ))
                })
        };
        // The summary stays put above the list, so the edge that shows when the list is
        // scrolled is under the summary, not under the toolbar.
        let toolbar = self.toolbar(t, "Cleanup", false, controls, cx);

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
                true,
                move |_, cx| {
                    model.update(cx, |m, cx| m.dismiss_cleanup_notice(cx));
                },
                cx,
            )
        });

        let body: AnyElement = if !snap.has_plan || snap.empty_result {
            let first = !snap.has_plan;
            let scan_model = model.clone();
            self.empty_state(
                t,
                IconName::BrushCleaning,
                if first {
                    EMPTY_FIRST_TITLE
                } else {
                    EMPTY_DONE_TITLE
                },
                if first {
                    EMPTY_FIRST_BODY
                } else {
                    EMPTY_DONE_BODY
                }
                .to_string(),
                if first { "Scan" } else { "Scan Again" },
                move |_, cx| {
                    scan_model.update(cx, |m, cx| m.scan_cleanup(cx));
                },
            )
            .into_any_element()
        } else {
            let rows = snap.rows.clone();
            let env_model = model.clone();
            let env_view = view.clone();
            let hovered = self.hovered.clone();
            let dim = snap.scanning.is_some();
            let count = rows.len();
            let list = uniform_list("cleanup-rows", count, move |range, window, cx| {
                let t = Theme::of(window);
                let env = RowEnv {
                    model: env_model.clone(),
                    view: env_view.clone(),
                    hovered: hovered.clone(),
                    reduce_motion: cx.reduce_motion(),
                };
                range
                    .filter_map(|ix| rows.get(ix).map(|row| render_row(row, ix, &t, &env)))
                    .collect::<Vec<AnyElement>>()
            })
            .track_scroll(&self.cleanup_scroll)
            .flex_1()
            .px(px(metrics::LIST_INSET))
            .pt(px(2.))
            .pb(px(8.))
            .when(dim, |l| l.opacity(0.5));
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.summary_strip(t, &snap, now))
                .child(scroll_edge(t, scrolled))
                .child(list)
                .into_any_element()
        };

        let left = snap.status_left();
        let right = snap.trash_line().map(|line| {
            let view = view.clone();
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .debug_selector(|| "trash-total".to_string())
                        .child(line),
                )
                .child(plain_button(
                    "empty-trash",
                    "empty-trash".to_string(),
                    "Empty Trash\u{2026}",
                    true,
                    t,
                    move |_, cx| {
                        let _ = view.update(cx, |v, cx| v.request_empty_trash(cx));
                    },
                ))
                .into_any_element()
        });
        let status = self.status_bar(t, left, right);

        div()
            .debug_selector(|| "cleanup".to_string())
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

    /// 64 pt: the Figure ("6.3 GB ready to clean") and one line under it.
    fn summary_strip(&self, t: &Theme, snap: &CleanupView, now: u64) -> Div {
        div()
            .debug_selector(|| "summary".to_string())
            .flex_none()
            .h(px(metrics::SUMMARY_H))
            .px(px(20.))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .child(
                        div()
                            .debug_selector(|| "figure".to_string())
                            .text_size(px(figure::SIZE))
                            .line_height(px(figure::LINE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .font_features(tnum())
                            .text_color(t.text)
                            .whitespace_nowrap()
                            .child(snap.figure()),
                    )
                    .when(snap.ticked_bytes > 0, |d| {
                        d.child(
                            div()
                                .text_size(px(15.))
                                .text_color(t.text_2)
                                .whitespace_nowrap()
                                .child("ready to clean"),
                        )
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "summary-line".to_string())
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .font_features(tnum())
                    .text_color(t.text_2)
                    .whitespace_nowrap()
                    .child(snap.summary_line(now)),
            )
    }
}

/// What the render needs from the model, copied out so the borrow ends.
pub(crate) struct CleanupView {
    pub(crate) rows: Rc<Vec<CleanRow>>,
    pub(crate) has_plan: bool,
    pub(crate) scanning: Option<u64>,
    pub(crate) cleaning: Option<(usize, usize)>,
    pub(crate) ticked_count: usize,
    pub(crate) ticked_bytes: u64,
    pub(crate) any_review: bool,
    pub(crate) review_bytes: u64,
    pub(crate) scanned_at: Option<u64>,
    pub(crate) notice: Option<crate::app::Notice>,
    pub(crate) notice_leaving: Option<crate::app::Notice>,
    pub(crate) trash: tidy_core::model::TrashSummary,
    pub(crate) empty_result: bool,
}

impl CleanupView {
    /// "Clean 6.3 GB", "Clean selected" or "Cleaning 4 of 12".
    pub(crate) fn button_label(&self) -> String {
        format::clean_label(self.ticked_bytes, self.any_review, self.cleaning)
    }

    /// Whether the primary button can be clicked.
    pub(crate) fn can_clean(&self) -> bool {
        self.ticked_count > 0 && self.cleaning.is_none()
    }

    /// The left side of the status bar: "12 selected, 6.3 GB".
    pub(crate) fn status_left(&self) -> String {
        if !self.has_plan || self.empty_result {
            String::new()
        } else if self.ticked_count == 0 {
            // The summary already says "Nothing selected"; say what is left instead.
            format!("{}, none selected", format::items(self.item_count()))
        } else {
            format::selected_line(self.ticked_count, self.ticked_bytes)
        }
    }

    /// The right side of the status bar, when something of Tidy's is in the Trash.
    pub(crate) fn trash_line(&self) -> Option<String> {
        (self.trash.items > 0)
            .then(|| format!("In the Trash from Tidy: {}", format::size(self.trash.bytes)))
    }

    /// The Figure: "6.3 GB", or "Nothing selected" when nothing is ticked (after a clean, or
    /// when every box was unticked), rather than "0 KB ready to clean".
    pub(crate) fn figure(&self) -> String {
        if self.ticked_bytes == 0 {
            "Nothing selected".to_string()
        } else {
            format::size(self.ticked_bytes)
        }
    }

    /// How many items the list holds, collapsed groups included.
    pub(crate) fn item_count(&self) -> usize {
        self.rows
            .iter()
            .map(|r| match r {
                CleanRow::Header { count, .. } => *count,
                CleanRow::Item(_) => 0,
            })
            .sum()
    }

    /// The line under the Figure: "34 GB more to review · Scanned 2 min ago".
    pub(crate) fn summary_line(&self, now: u64) -> String {
        let mut line = Vec::new();
        if self.review_bytes > 0 {
            line.push(format!(
                "{} more to review",
                format::size(self.review_bytes)
            ));
        }
        if let Some(at) = self.scanned_at {
            line.push(format::scanned(now, at));
        }
        line.join(" \u{b7} ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use tidy_core::model::{Item, Plan};

    fn item(id: &str, group: Group, class: Class, bytes: u64, blocked: Option<&str>) -> Item {
        Item {
            id: format!("r:{id}"),
            group,
            class,
            ticked: class == Class::Safe && blocked.is_none(),
            blocked: blocked.map(str::to_string),
            name: id.to_string(),
            reason: format!("reason of {id}"),
            path: PathBuf::from("/fake").join(id),
            bytes_alloc: bytes,
            bytes_logical: bytes,
            newest_mtime: None,
            members: vec![],
        }
    }

    fn state() -> CleanupState {
        let mut plan = Plan::empty("/fake", 100);
        plan.items = vec![
            item("a", Group::Caches, Class::Safe, 3_000, None),
            item("b", Group::Caches, Class::Review, 2_000, None),
            item(
                "chrome",
                Group::Caches,
                Class::Safe,
                1_000,
                Some("Quit Google Chrome first"),
            ),
            item("log", Group::Logs, Class::Safe, 500, None),
            item("vm", Group::AiModels, Class::ReportOnly, 9_000, None),
        ];
        let mut c = CleanupState::default();
        let mut m = TidyModel::new(Default::default());
        m.load_demo(crate::app::Screen::Cleanup, Some(plan), None, None);
        std::mem::swap(&mut c, &mut m.cleanup);
        c
    }

    #[test]
    fn rows_follow_groups_and_collapse_ai_models() {
        let c = state();
        let rows = cleanup_rows(&c);
        // Caches (3 items), Logs (1), AI models header only (starts collapsed).
        let kinds: Vec<String> = rows
            .iter()
            .map(|r| match r {
                CleanRow::Header { group, .. } => format!("H:{group:?}"),
                CleanRow::Item(i) => format!("I:{}", i.id),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "H:Caches",
                "I:r:a",
                "I:r:b",
                "I:r:chrome",
                "H:Logs",
                "I:r:log",
                "H:AiModels"
            ]
        );
        match &rows[0] {
            CleanRow::Header {
                first,
                count,
                bytes,
                tri,
                expanded,
                ..
            } => {
                assert!(*first && *expanded);
                assert_eq!((*count, *bytes), (3, 6_000));
                assert_eq!(*tri, Some(Tri::Mixed));
            }
            _ => unreachable!(),
        }
        match &rows[4] {
            CleanRow::Header { first, .. } => assert!(!*first),
            _ => unreachable!(),
        }
    }

    #[test]
    fn meta_lines_and_flags() {
        let c = state();
        let rows = cleanup_rows(&c);
        let find = |id: &str| -> ItemRow {
            rows.iter()
                .find_map(|r| match r {
                    CleanRow::Item(i) if i.id == id => Some(i.clone()),
                    _ => None,
                })
                .unwrap()
        };
        let a = find("r:a");
        assert!(a.ticked && a.enabled && !a.warn && a.meta == "reason of a");
        assert_eq!(a.tag(), None);
        let b = find("r:b");
        assert!(!b.ticked && b.enabled);
        assert_eq!(b.tag(), Some("Review"));
        let chrome = find("r:chrome");
        assert!(!chrome.ticked && !chrome.enabled && chrome.warn);
        assert_eq!(chrome.meta, "Quit Google Chrome first");
    }

    #[test]
    fn report_only_rows_say_so_and_have_no_checkbox() {
        let mut c = state();
        c.collapsed.clear();
        let rows = cleanup_rows(&c);
        let vm = rows
            .iter()
            .find_map(|r| match r {
                CleanRow::Item(i) if i.id == "r:vm" => Some(i.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(vm.meta, "Tidy won't remove this. reason of vm");
        assert_eq!(vm.tag(), Some("Report only"));
        assert!(!vm.class.selectable());
        // A group of report-only items cannot be ticked.
        assert_eq!(c.group_tri(Group::AiModels), None);
    }

    #[test]
    fn skipped_rows_keep_their_reason_in_the_warning_color() {
        let mut c = state();
        c.skipped
            .insert("r:a".into(), "Google Chrome opened, skipped".into());
        let rows = cleanup_rows(&c);
        let a = rows
            .iter()
            .find_map(|r| match r {
                CleanRow::Item(i) if i.id == "r:a" => Some(i.clone()),
                _ => None,
            })
            .unwrap();
        assert!(a.warn);
        assert_eq!(a.meta, "Google Chrome opened, skipped");
    }

    #[test]
    fn gone_rows_and_empty_groups_disappear() {
        let mut c = state();
        c.gone.insert("r:log".into());
        let rows = cleanup_rows(&c);
        assert!(!rows.iter().any(|r| matches!(
            r,
            CleanRow::Header {
                group: Group::Logs,
                ..
            }
        )));
        assert_eq!(c.groups(), vec![Group::Caches, Group::AiModels]);
    }

    #[test]
    fn totals_count_only_enabled_ticked_items() {
        let c = state();
        assert_eq!(c.ticked_bytes(), 3_500);
        assert!(!c.any_review_ticked());
        assert_eq!(c.safe_bytes(), Some(3_500));
        assert_eq!(c.review_bytes(), 2_000);
    }

    #[test]
    fn menu_entries_in_order() {
        let labels: Vec<_> = MenuEntry::all().into_iter().map(MenuEntry::label).collect();
        assert_eq!(
            labels,
            ["Show Details", "Never show this again", "Copy Path"]
        );
    }

    #[test]
    fn copy_has_no_em_dashes() {
        for s in [
            EMPTY_FIRST_TITLE,
            EMPTY_FIRST_BODY,
            EMPTY_DONE_TITLE,
            EMPTY_DONE_BODY,
        ] {
            assert!(!s.contains('\u{2014}'));
        }
    }
}
