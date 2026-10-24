//! The sidebar (design section 11): 200 pt wide, the five screens (Check up, Cleanup, Large
//! files, Memory, Security), and the free-space figure at the bottom.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::app::{Screen, TidyModel};
use crate::format;
use crate::theme::{Theme, metrics, tnum};
use crate::views::main_window::TidyView;

/// The sidebar row for a screen: its icon.
pub fn icon_for(screen: Screen) -> IconName {
    match screen {
        Screen::Checkup => IconName::Stethoscope,
        Screen::Cleanup => IconName::Sparkles,
        Screen::Large => IconName::HardDrive,
        Screen::Memory => IconName::MemoryStick,
        Screen::Security => IconName::ShieldCheck,
    }
}

/// The selector name of a screen's sidebar row (for tests).
pub fn selector_for(screen: Screen) -> &'static str {
    match screen {
        Screen::Checkup => "sidebar-checkup",
        Screen::Cleanup => "sidebar-cleanup",
        Screen::Large => "sidebar-large",
        Screen::Memory => "sidebar-memory",
        Screen::Security => "sidebar-security",
    }
}

/// The selector name of a sidebar row's trailing figure.
pub fn figure_selector_for(screen: Screen) -> &'static str {
    match screen {
        Screen::Checkup => "sidebar-checkup-figure",
        Screen::Cleanup => "sidebar-cleanup-figure",
        Screen::Large => "sidebar-large-figure",
        Screen::Memory => "sidebar-memory-figure",
        Screen::Security => "sidebar-security-figure",
    }
}

/// The trailing figure of a sidebar row: the safe total after a Cleanup scan, the item count
/// on Large files. None before the first result, and on the other screens.
pub fn figure_for(screen: Screen, m: &TidyModel) -> Option<String> {
    match screen {
        Screen::Cleanup => m.cleanup.safe_bytes().map(format::size),
        Screen::Large => m.large.item_count().map(|n| format::thousands(n as u64)),
        Screen::Checkup | Screen::Memory | Screen::Security => None,
    }
}

impl TidyView {
    pub(crate) fn render_sidebar(&mut self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let model = self.model.read(cx);
        let current = model.screen;
        let rows: Vec<(Screen, Option<String>)> = Screen::ALL
            .into_iter()
            .map(|s| (s, figure_for(s, model)))
            .collect();
        let disk = model.disk.clone();

        let drag = self.drag_area(
            div()
                .id("sidebar-drag")
                .flex_none()
                .h(px(metrics::TOOLBAR_H)),
            cx,
        );

        let t = *t;
        let items = rows.into_iter().map(|(screen, figure)| {
            let selected = screen == current;
            let model = self.model.clone();
            let (hover, selected_bg) = (t.row_hover, t.sidebar_selected);
            div()
                .id(("sidebar-row", screen as usize))
                .debug_selector(move || selector_for(screen).to_string())
                .h(px(metrics::SIDEBAR_ROW_H))
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .cursor_pointer()
                .when(selected, |d| d.bg(selected_bg))
                .when(!selected, |d| d.hover(move |s| s.bg(hover)))
                .on_click(move |_, _, cx| {
                    model.update(cx, |m, cx| m.select_screen(screen, cx));
                })
                .child(
                    Icon::new(icon_for(screen))
                        .size(px(16.))
                        .text_color(if selected { t.accent } else { t.text_2 }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.))
                        .text_color(t.text)
                        .whitespace_nowrap()
                        .child(screen.title()),
                )
                .when_some(figure, |d, figure| {
                    d.child(
                        div()
                            .debug_selector(move || figure_selector_for(screen).to_string())
                            .flex_none()
                            .text_size(px(12.))
                            .font_features(tnum())
                            .text_color(t.text_2)
                            .child(figure),
                    )
                })
        });

        div()
            .debug_selector(|| "sidebar".to_string())
            .flex_none()
            .w(px(metrics::SIDEBAR_W))
            .h_full()
            .flex()
            .flex_col()
            .bg(t.sidebar_bg)
            .border_r_1()
            .border_color(t.separator)
            .child(drag)
            .child(
                div()
                    .px(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .children(items),
            )
            .child(div().flex_1())
            .when_some(disk, |d, disk| {
                d.child(
                    div()
                        .debug_selector(|| "disk".to_string())
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.text)
                                .whitespace_nowrap()
                                .child(disk.name.clone()),
                        )
                        .child(
                            div()
                                .h(px(metrics::BAR_H))
                                .w_full()
                                .rounded(px(2.))
                                .bg(t.track)
                                .child(
                                    div()
                                        .h_full()
                                        .w(relative(disk.used_fraction()))
                                        .rounded(px(2.))
                                        .bg(t.accent),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_features(tnum())
                                .text_color(t.text_2)
                                .whitespace_nowrap()
                                .child(format::disk_line(disk.available, disk.total)),
                        ),
                )
            })
    }
}
