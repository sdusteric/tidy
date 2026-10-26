//! The Large files screen (design sections 5.2 and 11): Files and Folders modes, the Biggest and
//! Not opened lately filters, the threshold menu, the breadcrumb drill-down, selection and the
//! Move to Trash confirm. Every move asks first, even for one item.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use tidy_core::model::{LARGE_THRESHOLDS, LargeKind};

use crate::app::{
    LargeEntry, LargeFilter, LargeMode, LargeRow, Notice, OpenKind, TidyModel, Tri, now_unix,
    open_path,
};
use crate::format::{self, Glyph};
use crate::theme::{DisplayType, MONO_FONT, Theme, metrics, motion as m, tnum};
use crate::views::dialogs::DialogSpec;
use crate::views::main_window::TidyView;
use crate::views::{
    KeyStop, RowFocus, Segment, checkbox, glyph_tile, icon_button, plain_button, row_action,
    scroll_edge, segmented, tag,
};
use gpui_kit::base::FocusTrapElement as _;

pub const EMPTY_TITLE: &str = "See what takes up space";

/// The body of the first-visit empty state.
pub fn empty_body(min: u64) -> String {
    format!(
        "Tidy lists files and folders over {} in your home folder. macOS may ask once each for Documents, Desktop and Downloads.",
        format::size(min)
    )
}

/// The icon of a Large files row.
pub fn glyph_icon(glyph: Glyph) -> IconName {
    match glyph {
        Glyph::Film => IconName::Film,
        Glyph::Archive => IconName::Archive,
        Glyph::DiskImage => IconName::Disc,
        Glyph::App => IconName::AppWindow,
        Glyph::Folder => IconName::Folder,
        Glyph::Document => IconName::FileText,
    }
}

/// What the render needs from the model, copied out so the borrow ends.
pub(crate) struct LargeView {
    pub(crate) rows: Rc<Vec<LargeRow>>,
    pub(crate) has_scan: bool,
    pub(crate) scanning: Option<u64>,
    pub(crate) mode: LargeMode,
    pub(crate) filter: LargeFilter,
    pub(crate) min: u64,
    pub(crate) crumbs: Vec<(String, String)>,
    pub(crate) selected: BTreeSet<String>,
    pub(crate) selected_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) item_count: usize,
    pub(crate) scanned_at: Option<u64>,
    pub(crate) notice: Option<Notice>,
    pub(crate) notice_leaving: Option<Notice>,
    pub(crate) root: PathBuf,
    pub(crate) now: u64,
    pub(crate) not_opened_days: u32,
    pub(crate) skipped: std::collections::HashMap<String, String>,
    pub(crate) leaving: std::collections::HashSet<String>,
}

impl LargeView {
    /// The left side of the status bar: "3 selected, 12.4 GB" or "48 items, up to 214 GB".
    pub(crate) fn status_left(&self) -> String {
        if !self.has_scan {
            String::new()
        } else if !self.selected.is_empty() {
            format::selected_line(self.selected.len(), self.selected_bytes)
        } else {
            format!(
                "{}, up to {}",
                format::items(self.item_count),
                format::size(self.total_bytes)
            )
        }
    }

    /// The label of the threshold menu button: "Over 500 MB".
    pub(crate) fn threshold_label(&self) -> String {
        format::over(self.min)
    }
}

/// The width of everything in a Large files row except the text column, in points: the row
/// padding (20), checkbox (16), tile (32), date column (150), size column (80), trailing
/// actions (60) and the gaps between those and the text column (50). Tags sit on the name
/// line, so the path line under it has the whole text column.
pub const ROW_FIXED_W: f32 = 408.;
/// The fewest characters a path line is cut to.
pub const MIN_PATH_COLS: usize = 12;
/// The advance of a character in the 12 pt mono font when the text system can't tell.
pub const FALLBACK_CHAR_W: f32 = 7.2;

/// The width of the name and path column of a row, for a window `viewport_w` points wide.
pub fn text_col_width(viewport_w: f32) -> f32 {
    (viewport_w - metrics::SIDEBAR_W - 1. - 2. * metrics::LIST_INSET - ROW_FIXED_W).max(0.)
}

/// How many mono characters fit in `width` points when one takes `char_w`. One column is kept
/// back so rounding never lets the last character touch the edge.
pub fn path_columns(width: f32, char_w: f32) -> usize {
    let cols = if char_w > 0. {
        (width / char_w).floor() as usize
    } else {
        0
    };
    cols.saturating_sub(1).max(MIN_PATH_COLS)
}

/// The path line of a row: `~/Movies/2025/…/trip.mov`, shortened in the middle so it fits
/// the text column of a window `viewport_w` points wide. A name too long for even that keeps
/// both ends with an ellipsis between. The element also ends in an ellipsis when the font is wider
/// than measured, so a path is never cut mid-character.
pub fn path_text(
    path: &std::path::Path,
    root: &std::path::Path,
    viewport_w: f32,
    char_w: f32,
) -> String {
    let cols = path_columns(text_col_width(viewport_w), char_w);
    format::middle_truncate(&format::display_path(path, root), cols)
}

/// The advance of one character of the 12 pt mono font, measured with the window's text
/// system.
pub(crate) fn mono_char_width(window: &Window) -> f32 {
    let system = window.text_system();
    let font = Font {
        family: MONO_FONT.into(),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let id = system.resolve_font(&font);
    system
        .advance(id, px(12.), 'm')
        .map(|s| f32::from(s.width))
        .ok()
        .filter(|w| *w > 0.)
        .unwrap_or(FALLBACK_CHAR_W)
}

/// What a row needs besides its own data.
struct RowEnv {
    /// The window's width and the width of one mono character, to fit path lines.
    viewport_w: f32,
    char_w: f32,
    model: Entity<TidyModel>,
    view: WeakEntity<TidyView>,
    hovered: Option<String>,
    /// The key of the row the keyboard is on (see `RowFocus`).
    keyboard_row: Option<String>,
    reduce_motion: bool,
    mode: LargeMode,
    selected: BTreeSet<String>,
    root: PathBuf,
    now: u64,
    skipped: std::collections::HashMap<String, String>,
    leaving: std::collections::HashSet<String>,
}

fn render_row(row: &LargeRow, ix: usize, t: &Theme, env: &RowEnv, cx: &mut App) -> AnyElement {
    match row {
        LargeRow::Item(e) => {
            let key = format!("large:{}", e.rel);
            let inside = env.keyboard_row.as_deref() == Some(key.as_str());
            render_item(e, ix, t, env, RowFocus::handle(&key, cx), inside)
        }
        LargeRow::Rest { count, bytes } => div()
            .debug_selector(|| "large-rest".to_string())
            .w_full()
            .h(px(metrics::LARGE_ROW_H))
            .pl(px(10. + 16. + 10.))
            .pr(px(10.))
            .gap(px(10.))
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .text_color(t.text_2)
                    .whitespace_nowrap()
                    .child(format!(
                        "{} smaller {}, {}",
                        format::thousands(*count),
                        if *count == 1 { "item" } else { "items" },
                        format::size(*bytes)
                    )),
            )
            .into_any_element(),
    }
}

fn render_item(
    e: &LargeEntry,
    ix: usize,
    t: &Theme,
    env: &RowEnv,
    row_focus: FocusHandle,
    keyboard_inside: bool,
) -> AnyElement {
    let rel = e.rel.clone();
    // The pointer or the keyboard is on the row: it shows its actions.
    let hovered = keyboard_inside || env.hovered.as_deref() == Some(rel.as_str());
    let selected = env.selected.contains(&rel);
    let hover_bg = t.row_hover;
    let folders = env.mode == LargeMode::Folders;
    let drill = e.drill;
    let skipped = env.skipped.get(&rel).cloned();

    let (row_model, row_rel) = (env.model.clone(), rel.clone());
    let (check_model, check_rel) = (env.model.clone(), rel.clone());
    let (hover_view, hover_key) = (env.view.clone(), rel.clone());
    let (open_model, open_rel) = (env.model.clone(), rel.clone());
    let (trash_view, trash_rel) = (env.view.clone(), rel.clone());
    let reveal_path = e.path.clone();
    let selector_rel = rel.clone();

    let path_line = path_text(&e.path, &env.root, env.viewport_w, env.char_w);
    let second: AnyElement = if let Some(reason) = skipped {
        div()
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(t.warning)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .child(reason)
            .into_any_element()
    } else if let Some(share) = e.share {
        div()
            .mt(px(5.))
            .w(px(160.))
            .h(px(metrics::BAR_H))
            .rounded(px(2.))
            .bg(t.track)
            .child(
                div()
                    .h_full()
                    .w(relative(share.max(0.01)))
                    .rounded(px(2.))
                    .bg(t.accent),
            )
            .into_any_element()
    } else {
        div()
            .font_family(MONO_FONT)
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(t.text_2)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .child(path_line)
            .into_any_element()
    };

    let date = if folders {
        if e.kind == LargeKind::Folder {
            format!("{} files", format::thousands(e.files))
        } else {
            String::new()
        }
    } else {
        format::use_phrase(e.last_used, e.source, env.now)
    };

    let actions = div()
        .flex()
        .items_center()
        .child(row_action(
            hovered,
            format!("large-reveal:{rel}"),
            |selector| {
                icon_button(
                    ("large-reveal", ix),
                    selector,
                    IconName::FolderSearch,
                    t,
                    move |_, cx| open_path(OpenKind::Reveal, &reveal_path, cx),
                )
            },
        ))
        .map(|d| {
            if drill {
                d.child(icon_button(
                    ("large-open", ix),
                    format!("large-open:{rel}"),
                    IconName::ChevronRight,
                    t,
                    move |_, cx| {
                        open_model.update(cx, |m, cx| m.open_folder(&open_rel, cx));
                    },
                ))
            } else {
                d.child(row_action(
                    hovered,
                    format!("large-trash:{rel}"),
                    |selector| {
                        icon_button(
                            ("large-trash", ix),
                            selector,
                            IconName::Trash,
                            t,
                            move |_, cx| {
                                let rel = trash_rel.clone();
                                let _ = trash_view.update(cx, |v, cx| v.trash_single(&rel, cx));
                            },
                        )
                    },
                ))
            }
        });

    let el = div()
        .id(("large-item", ix))
        .track_focus(&row_focus)
        .debug_selector(move || format!("large-row:{selector_rel}"))
        .w_full()
        .h(px(metrics::LARGE_ROW_H))
        .px(px(10.))
        .gap(px(10.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .when(hovered || selected, |d| d.bg(hover_bg))
        .on_hover(move |h, _, cx| {
            let key = hover_key.clone();
            let h = *h;
            let _ = hover_view.update(cx, |v, cx| v.set_hovered(&key, h, cx));
        })
        .on_click(move |ev, _, cx| {
            if ev.click_count() >= 2 {
                if drill {
                    row_model.update(cx, |m, cx| m.open_folder(&row_rel, cx));
                }
            } else {
                row_model.update(cx, |m, cx| m.toggle_select(&row_rel, cx));
            }
        })
        .child(checkbox(
            ("large-check", ix),
            format!("large-check:{rel}"),
            if selected { Tri::On } else { Tri::Off },
            true,
            t,
            move |_, cx| {
                check_model.update(cx, |m, cx| m.toggle_select(&check_rel, cx));
            },
        ))
        .child(glyph_tile(
            glyph_icon(format::glyph_for(e.kind, &e.name)),
            32.,
            7.,
            18.,
            t,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    // Tags sit right after the name, as Haul's quality tag does.
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
                                .child(e.name.clone()),
                        )
                        .when(e.app_data, |d| d.child(tag("App data", t)))
                        .when(e.kind == LargeKind::Package, |d| d.child(tag("Package", t))),
                )
                .child(second),
        )
        .child(
            div()
                .flex_none()
                .w(px(150.))
                .text_right()
                .text_size(px(12.))
                .font_features(tnum())
                .text_color(t.text_2)
                .whitespace_nowrap()
                .child(date),
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
                .child(format::size(e.bytes)),
        )
        .child(
            div()
                .flex_none()
                .w(px(metrics::TRAIL_W))
                .flex()
                .justify_end()
                .child(actions),
        );
    if env.leaving.contains(&rel) && !env.reduce_motion {
        el.with_animation(
            ("large-out", ix),
            Animation::new(m::REMOVE).with_easing(m::curve(m::exit())),
            |el, t| el.opacity(1. - t),
        )
        .into_any_element()
    } else {
        el.into_any_element()
    }
}

impl TidyView {
    /// Move to Trash for the ticked rows (button and Cmd-Backspace). Asks first.
    pub fn trash_selection(&mut self, cx: &mut Context<Self>) {
        let entries = {
            let m = self.model.read(cx);
            if m.screen != crate::app::Screen::Large {
                return;
            }
            let rows = m.large.rows(now_unix(cx), m.settings.not_opened_days);
            m.large.selected_entries(&rows)
        };
        if entries.is_empty() {
            return;
        }
        self.open_dialog(DialogSpec::trash(&entries), cx);
    }

    /// The hover trash button: selects that single row and opens the same dialog.
    pub fn trash_single(&mut self, rel: &str, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            m.large.selected.clear();
            m.large.selected.insert(rel.to_string());
            cx.notify();
        });
        self.trash_selection(cx);
    }

    /// The threshold menu, drawn over the window at the click.
    pub(crate) fn render_threshold_menu(
        &mut self,
        t: &Theme,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.model.read(cx).large.min;
        let view = cx.entity().downgrade();
        let close = view.clone();
        let hover = t.row_hover;
        let model = self.model.clone();
        let on_menu = t.on(t.pop_bg);
        let card = div()
            .debug_selector(|| "threshold-menu".to_string())
            .min_w(px(150.))
            .p(px(4.))
            .rounded(px(8.))
            .bg(t.pop_bg)
            .shadow(t.pop_shadow())
            .flex()
            .flex_col()
            .children(LARGE_THRESHOLDS.iter().enumerate().map(|(i, min)| {
                let min = *min;
                let model = model.clone();
                let view = view.clone();
                div()
                    .id(("threshold-option", i))
                    .debug_selector(move || format!("threshold-option-{i}"))
                    .h(px(28.))
                    .px(px(10.))
                    .gap(px(8.))
                    .rounded(px(5.))
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .text_color(t.text)
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .child(div().w(px(14.)).flex_none().when(min == current, |d| {
                        d.child(Icon::new(IconName::Check).size(px(14.)).text_color(t.text))
                    }))
                    .child(format::size(min))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _, cx| {
                        model.update(cx, |m, cx| m.set_threshold(min, cx));
                        let _ = view.update(cx, |v, cx| {
                            v.threshold_menu = None;
                            cx.notify();
                        });
                    })
                    .key_stop(&on_menu)
            }))
            .focus_trap("threshold-menu-trap", &self.traps.menu);
        div()
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("threshold-scrim")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let _ = close.update(cx, |v, cx| {
                            v.threshold_menu = None;
                            cx.notify();
                        });
                    }),
            )
            .child(deferred(
                anchored()
                    .position(at)
                    .snap_to_window_with_margin(px(8.))
                    .child(card),
            ))
            .into_any_element()
    }

    /// The state the Large files screen draws, copied out of the model.
    pub(crate) fn large_snapshot(&self, cx: &App, now: u64) -> LargeView {
        let m = self.model.read(cx);
        let l = &m.large;
        let rows = l.rows(now, m.settings.not_opened_days);
        let items: Vec<&LargeEntry> = rows
            .iter()
            .filter_map(|r| match r {
                LargeRow::Item(e) => Some(e),
                _ => None,
            })
            .collect();
        let selected_entries = l.selected_entries(&rows);
        LargeView {
            has_scan: l.scan.is_some(),
            scanning: l.scanning,
            mode: l.mode,
            filter: l.filter,
            min: l.min,
            crumbs: l.breadcrumb(),
            selected: selected_entries.iter().map(|e| e.rel.clone()).collect(),
            selected_bytes: selected_entries.iter().map(|e| e.bytes).sum(),
            total_bytes: items.iter().map(|e| e.bytes).sum(),
            item_count: items.len(),
            scanned_at: l.scan.as_ref().map(|s| s.scanned_at),
            notice: l.notice.clone(),
            notice_leaving: l.notice_leaving.clone(),
            root: l.scan.as_ref().map(|s| s.root.clone()).unwrap_or_default(),
            now,
            not_opened_days: m.settings.not_opened_days,
            skipped: l.skipped.clone(),
            leaving: l.leaving.clone(),
            rows: Rc::new(rows),
        }
    }

    /// The whole Large files screen.
    pub(crate) fn render_large(&mut self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let now = now_unix(cx);
        let snap = self.large_snapshot(cx, now);
        let scrolled = self.large_scroll.0.borrow().base_handle.offset().y < px(0.);
        let model = self.model.clone();
        let view = cx.entity().downgrade();
        let trashing = self.model.read(cx).large.trashing.is_some();

        let controls = {
            let stop_model = model.clone();
            let rescan_model = model.clone();
            let threshold_view = view.clone();
            let mode_a = model.clone();
            let mode_b = model.clone();
            let filter_a = model.clone();
            let filter_b = model.clone();
            let scanning = snap.scanning;
            if let Some(files) = scanning {
                self.scanning_controls(
                    t,
                    files,
                    move |_, cx| {
                        stop_model.update(cx, |m, cx| m.stop_scan(cx));
                    },
                    cx,
                )
                .into_any_element()
            } else if snap.has_scan {
                let threshold_label = snap.threshold_label();
                let (button_fill, shadow) = (t.button_fill(), t.button_shadow());
                let (text, text_2) = (t.text, t.text_2);
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(snap.mode == LargeMode::Files, |d| {
                        d.child(segmented(
                            "large-filter",
                            vec![
                                Segment {
                                    label: "Biggest",
                                    selected: snap.filter == LargeFilter::Biggest,
                                    selector: "filter-biggest".to_string(),
                                    on_click: Box::new(move |_, cx| {
                                        filter_a.update(cx, |m, cx| {
                                            m.set_filter(LargeFilter::Biggest, cx)
                                        });
                                    }),
                                },
                                Segment {
                                    label: "Not opened lately",
                                    selected: snap.filter == LargeFilter::NotOpened,
                                    selector: "filter-not-opened".to_string(),
                                    on_click: Box::new(move |_, cx| {
                                        filter_b.update(cx, |m, cx| {
                                            m.set_filter(LargeFilter::NotOpened, cx)
                                        });
                                    }),
                                },
                            ],
                            t,
                        ))
                    })
                    // Files | Folders sits right of the filter, so hiding the filter in Folders
                    // mode does not move the control that was just clicked.
                    .child(segmented(
                        "large-mode",
                        vec![
                            Segment {
                                label: "Files",
                                selected: snap.mode == LargeMode::Files,
                                selector: "mode-files".to_string(),
                                on_click: Box::new(move |_, cx| {
                                    mode_a.update(cx, |m, cx| m.set_mode(LargeMode::Files, cx));
                                }),
                            },
                            Segment {
                                label: "Folders",
                                selected: snap.mode == LargeMode::Folders,
                                selector: "mode-folders".to_string(),
                                on_click: Box::new(move |_, cx| {
                                    mode_b.update(cx, |m, cx| m.set_mode(LargeMode::Folders, cx));
                                }),
                            },
                        ],
                        t,
                    ))
                    .child(
                        div()
                            .id("threshold")
                            .debug_selector(|| "threshold".to_string())
                            .flex_none()
                            .h(px(24.))
                            .pl(px(10.))
                            .pr(px(6.))
                            .gap(px(4.))
                            .rounded(px(6.))
                            .bg(button_fill)
                            .shadow(shadow)
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .display(FontWeight::SEMIBOLD)
                            .font_features(tnum())
                            .text_color(text)
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |ev, window, cx| {
                                let at = crate::views::click_anchor(ev, window);
                                let _ = threshold_view.update(cx, |v, cx| {
                                    v.threshold_menu = Some(at);
                                    cx.notify();
                                });
                            })
                            .key_stop_with(t, t.button_shadow())
                            .child(threshold_label)
                            .child(
                                Icon::new(IconName::ChevronsUpDown)
                                    .size(px(12.))
                                    .text_color(text_2),
                            ),
                    )
                    .child(icon_button(
                        "large-rescan",
                        "large-rescan".to_string(),
                        IconName::RefreshCw,
                        t,
                        move |_, cx| {
                            rescan_model.update(cx, |m, cx| m.scan_large(cx));
                        },
                    ))
                    .into_any_element()
            } else {
                div().into_any_element()
            }
        };
        // In Folders mode the breadcrumb stays put under the toolbar, so the scrolled edge is
        // under the breadcrumb.
        let crumbs_shown = snap.has_scan && snap.mode == LargeMode::Folders;
        let toolbar = self.toolbar(t, "Large files", scrolled && !crumbs_shown, controls, cx);

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
                    model.update(cx, |m, cx| m.dismiss_large_notice(cx));
                },
                cx,
            )
        });

        let breadcrumb = crumbs_shown.then(|| {
            div()
                .flex_none()
                .flex()
                .flex_col()
                .child(self.breadcrumb(t, &snap.crumbs, cx))
                .child(scroll_edge(t, scrolled))
        });

        let items_empty = snap.item_count == 0 && snap.rows.is_empty();
        let body: AnyElement = if !snap.has_scan {
            let scan_model = model.clone();
            self.empty_state(
                t,
                IconName::HardDrive,
                EMPTY_TITLE,
                empty_body(snap.min),
                "Scan Home Folder",
                move |_, cx| {
                    scan_model.update(cx, |m, cx| m.scan_large(cx));
                },
            )
            .into_any_element()
        } else if items_empty {
            let body = match (snap.mode, snap.filter) {
                (LargeMode::Files, LargeFilter::NotOpened) => format!(
                    "Nothing over {} has gone unopened for {} days.",
                    format::size(snap.min),
                    snap.not_opened_days
                ),
                _ => format!(
                    "Nothing here is over {}. Lower the size to see more.",
                    format::size(snap.min)
                ),
            };
            div()
                .debug_selector(|| "large-none".to_string())
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
                        .child("Nothing to show"),
                )
                .child(div().text_size(px(13.)).text_color(t.text_2).child(body))
                .into_any_element()
        } else {
            let rows = snap.rows.clone();
            let env_model = model.clone();
            let env_view = view.clone();
            let hovered = self.hovered.clone();
            let keyboard_row = self.keyboard_row.clone();
            let dim = snap.scanning.is_some();
            let (mode, selected, root, now) = (
                snap.mode,
                snap.selected.clone(),
                snap.root.clone(),
                snap.now,
            );
            let (skipped, leaving) = (snap.skipped.clone(), snap.leaving.clone());
            let count = rows.len();
            let list = uniform_list("large-rows", count, move |range, window, cx| {
                let t = Theme::of(window);
                let env = RowEnv {
                    viewport_w: f32::from(window.viewport_size().width),
                    char_w: mono_char_width(window),
                    model: env_model.clone(),
                    view: env_view.clone(),
                    hovered: hovered.clone(),
                    keyboard_row: keyboard_row.clone(),
                    reduce_motion: cx.reduce_motion(),
                    mode,
                    selected: selected.clone(),
                    root: root.clone(),
                    now,
                    skipped: skipped.clone(),
                    leaving: leaving.clone(),
                };
                range
                    .filter_map(|ix| rows.get(ix).map(|row| render_row(row, ix, &t, &env, cx)))
                    .collect::<Vec<AnyElement>>()
            })
            .track_scroll(&self.large_scroll)
            .flex_1()
            .px(px(metrics::LIST_INSET))
            .pt(px(2.))
            .pb(px(8.))
            .when(dim, |l| l.opacity(0.5));
            self.list_frame(list).into_any_element()
        };

        let left = snap.status_left();
        let right = snap.has_scan.then(|| {
            let view = view.clone();
            let can_trash = !snap.selected.is_empty() && !trashing;
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .children(snap.scanned_at.map(|at| {
                    div()
                        .debug_selector(|| "scanned-at".to_string())
                        .child(format::scanned(snap.now, at))
                }))
                .child(plain_button(
                    "move-to-trash",
                    "move-to-trash".to_string(),
                    match self.model.read(cx).large.trashing {
                        Some((done, total)) => format!("Moving {done} of {total}"),
                        None => "Move to Trash\u{2026}".to_string(),
                    },
                    can_trash,
                    &t.on(t.bar_bg),
                    move |_, cx| {
                        let _ = view.update(cx, |v, cx| v.trash_selection(cx));
                    },
                ))
                .into_any_element()
        });
        let status = self.status_bar(t, left, right);

        div()
            .debug_selector(|| "large".to_string())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .children(notice)
            .children(breadcrumb)
            .child(body)
            .child(status)
            .into_any_element()
    }

    /// The breadcrumb row of Folders mode: "Home > Library > Application Support". Earlier
    /// segments are buttons, the last one is plain text.
    fn breadcrumb(&self, t: &Theme, crumbs: &[(String, String)], _cx: &mut Context<Self>) -> Div {
        let last = crumbs.len().saturating_sub(1);
        let hover = t.row_hover;
        let model = self.model.clone();
        div()
            .debug_selector(|| "breadcrumb".to_string())
            .flex_none()
            .h(px(metrics::BREADCRUMB_H))
            // Segments have 4 pt of padding, so their text lines up with the title at 20.
            .pl(px(16.))
            .pr(px(20.))
            .gap(px(4.))
            .flex()
            .items_center()
            .overflow_hidden()
            .children(crumbs.iter().enumerate().flat_map(|(i, (label, rel))| {
                let mut parts: Vec<AnyElement> = Vec::new();
                if i > 0 {
                    parts.push(
                        Icon::new(IconName::ChevronRight)
                            .size(px(12.))
                            .text_color(t.text_3)
                            .into_any_element(),
                    );
                }
                let rel = rel.clone();
                let model = model.clone();
                let seg = div()
                    .id(("crumb", i))
                    .debug_selector(move || format!("crumb-{i}"))
                    .flex_none()
                    .px(px(4.))
                    .rounded(px(4.))
                    .text_size(px(12.))
                    .display(FontWeight::SEMIBOLD)
                    .whitespace_nowrap()
                    .child(label.clone());
                parts.push(if i == last {
                    seg.text_color(t.text).into_any_element()
                } else {
                    seg.text_color(t.text_2)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(move |_, _, cx| {
                            model.update(cx, |m, cx| m.open_folder(&rel, cx));
                        })
                        .key_stop(t)
                        .into_any_element()
                });
                parts
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::LargeState;
    use core::prelude::v1::test;

    const LONG: &str =
        "/Users/demo/Library/Application Support/LTX Studio/models/ltx-video-13b.safetensors";

    #[test]
    fn long_paths_are_shortened_to_fit_the_text_column() {
        let root = std::path::Path::new("/Users/demo");
        for viewport in [760., 900., 1400.] {
            let text = path_text(std::path::Path::new(LONG), root, viewport, FALLBACK_CHAR_W);
            let width = text.chars().count() as f32 * FALLBACK_CHAR_W;
            let room = text_col_width(viewport);
            // Never wider than the column, unless the column is smaller than the minimum.
            assert!(
                width <= room || text.chars().count() <= MIN_PATH_COLS,
                "{viewport}: {text} is {width} wide in {room}"
            );
            let full = format::display_path(std::path::Path::new(LONG), root);
            if text.chars().count() < full.chars().count() {
                assert!(text.contains('\u{2026}'), "{text}");
            }
        }
        // At the default window the file name survives and the middle is replaced.
        let at_900 = path_text(std::path::Path::new(LONG), root, 900., FALLBACK_CHAR_W);
        assert!(at_900.starts_with("~/"), "{at_900}");
        assert!(at_900.ends_with("ltx-video-13b.safetensors"), "{at_900}");
        assert!(at_900.contains("/\u{2026}/"), "{at_900}");
    }

    #[test]
    fn a_short_path_is_left_alone_and_a_wider_window_keeps_more() {
        let root = std::path::Path::new("/Users/demo");
        let short = std::path::Path::new("/Users/demo/Movies/trip.mov");
        assert_eq!(
            path_text(short, root, 900., FALLBACK_CHAR_W),
            "~/Movies/trip.mov"
        );
        let narrow = path_text(std::path::Path::new(LONG), root, 760., FALLBACK_CHAR_W);
        let wide = path_text(std::path::Path::new(LONG), root, 1400., FALLBACK_CHAR_W);
        assert!(narrow.chars().count() <= wide.chars().count());
        assert!(
            !wide.contains('\u{2026}'),
            "a 1400 pt window fits the whole path: {wide}"
        );
    }

    #[test]
    fn column_math_has_a_floor() {
        assert_eq!(text_col_width(100.), 0.);
        assert_eq!(path_columns(0., 7.2), MIN_PATH_COLS);
        assert_eq!(path_columns(100., 0.), MIN_PATH_COLS);
        assert_eq!(path_columns(7.2 * 40., 7.2), 39);
    }

    use tidy_core::model::{FolderNode, LargeItem, LargeScan, MB, UseSource};

    const NOW: u64 = 1_792_764_131;
    const DAY: u64 = 86_400;

    fn item(rel: &str, mb: u64, days_ago: u64, app_data: bool, kind: LargeKind) -> LargeItem {
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

    fn node(
        name: &str,
        rel: &str,
        mb: u64,
        kind: LargeKind,
        children: Vec<FolderNode>,
    ) -> FolderNode {
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

    fn state() -> LargeState {
        let mut scan = LargeScan::empty("/fake", NOW, 50 * MB);
        scan.items = vec![
            item("Movies/trip.mov", 900, 5, false, LargeKind::File),
            item(
                "Library/Application Support/Models/big.bin",
                700,
                400,
                true,
                LargeKind::File,
            ),
            item("Apps/Big.app", 300, 200, false, LargeKind::Package),
            item("Downloads/old.zip", 120, 3, false, LargeKind::File),
        ];
        let mut library = node(
            "Library",
            "Library",
            700,
            LargeKind::Folder,
            vec![node(
                "Application Support",
                "Library/Application Support",
                700,
                LargeKind::Folder,
                vec![],
            )],
        );
        library.rest_count = 3;
        library.rest_alloc = 20 * MB;
        scan.tree = node(
            "Home",
            "",
            2_000,
            LargeKind::Folder,
            vec![
                node("Movies", "Movies", 900, LargeKind::Folder, vec![]),
                library,
                node("Apps", "Apps", 300, LargeKind::Folder, vec![]),
                node("Tiny", "Tiny", 60, LargeKind::Folder, vec![]),
            ],
        );
        scan.tree.rest_count = 5;
        scan.tree.rest_alloc = 30 * MB;
        let mut l = LargeState::new(500 * MB);
        l.scan = Some(scan);
        l
    }

    fn rels(rows: &[LargeRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                LargeRow::Item(e) => e.rel.clone(),
                LargeRow::Rest { count, .. } => format!("rest:{count}"),
            })
            .collect()
    }

    #[test]
    fn the_threshold_refilters_the_saved_scan() {
        let mut l = state();
        assert_eq!(
            rels(&l.rows(NOW, 90)),
            [
                "Movies/trip.mov",
                "Library/Application Support/Models/big.bin"
            ]
        );
        l.min = 100 * MB;
        assert_eq!(l.rows(NOW, 90).len(), 4);
        l.min = 1_000 * MB;
        assert!(l.rows(NOW, 90).is_empty());
        assert_eq!(l.item_count(), Some(0));
    }

    #[test]
    fn not_opened_keeps_old_items_only() {
        let mut l = state();
        l.min = 100 * MB;
        l.filter = LargeFilter::NotOpened;
        assert_eq!(
            rels(&l.rows(NOW, 90)),
            ["Library/Application Support/Models/big.bin", "Apps/Big.app"]
        );
        // A longer limit drops the 200 day old app.
        assert_eq!(
            rels(&l.rows(NOW, 300)),
            ["Library/Application Support/Models/big.bin"]
        );
    }

    #[test]
    fn trashed_items_leave_the_rows() {
        let mut l = state();
        l.gone.insert("Movies/trip.mov".into());
        assert_eq!(
            rels(&l.rows(NOW, 90)),
            ["Library/Application Support/Models/big.bin"]
        );
        assert_eq!(l.item_count(), Some(1));
    }

    #[test]
    fn folders_mode_lists_children_with_shares_and_a_folded_row() {
        let mut l = state();
        l.mode = LargeMode::Folders;
        l.min = 100 * MB;
        let rows = l.rows(NOW, 90);
        // Tiny (60 MB) is below the threshold and folds into the last row with the rest.
        assert_eq!(rels(&rows), ["Movies", "Library", "Apps", "rest:6"]);
        match &rows[0] {
            LargeRow::Item(e) => {
                assert!((e.share.unwrap() - 0.45).abs() < 1e-6);
                assert!(!e.drill, "a folder with nothing kept inside is a leaf");
            }
            _ => unreachable!(),
        }
        match &rows[1] {
            LargeRow::Item(e) => {
                assert!(e.drill && e.app_data);
            }
            _ => unreachable!(),
        }
        match &rows[3] {
            LargeRow::Rest { count, bytes } => assert_eq!((*count, *bytes), (6, 90 * MB)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn breadcrumb_and_drill_down_paths() {
        let mut l = state();
        assert_eq!(l.breadcrumb(), vec![("Home".to_string(), String::new())]);
        l.mode = LargeMode::Folders;
        l.folder = "Library/Application Support".into();
        assert_eq!(
            l.breadcrumb(),
            vec![
                ("Home".to_string(), String::new()),
                ("Library".to_string(), "Library".to_string()),
                (
                    "Application Support".to_string(),
                    "Library/Application Support".to_string()
                ),
            ]
        );
        l.min = 100 * MB;
        let rows = l.rows(NOW, 90);
        // The open folder has no kept children and no folded ones.
        assert!(rows.is_empty());
    }

    #[test]
    fn selected_entries_follow_list_order() {
        let mut l = state();
        l.min = 100 * MB;
        l.selected.insert("Downloads/old.zip".into());
        l.selected.insert("Movies/trip.mov".into());
        let rows = l.rows(NOW, 90);
        let sel = l.selected_entries(&rows);
        assert_eq!(
            sel.iter().map(|e| e.rel.as_str()).collect::<Vec<_>>(),
            ["Movies/trip.mov", "Downloads/old.zip"]
        );
    }

    #[test]
    fn empty_body_names_the_threshold() {
        assert!(empty_body(500 * MB).contains("over 500 MB"));
        assert!(empty_body(1_000 * MB).contains("over 1 GB"));
        assert!(!empty_body(1).contains('\u{2014}'));
    }
}
