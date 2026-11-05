//! The sidebar (design section 11): 200 pt wide, the six screens (Check up, Cleanup, Large
//! files, Memory, Security, Uninstaller), and the free-space figure at the bottom.

use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::app::{Screen, TidyModel};
use crate::format;
use crate::theme::{DisplayType, Theme, metrics, motion as m, tnum};
use crate::views::main_window::TidyView;
use crate::views::trust::SYSDATA_LINK;
use crate::views::{KeyStop, plain_button};

/// The selector name of a screen's sidebar row (for tests).
pub fn selector_for(screen: Screen) -> &'static str {
    match screen {
        Screen::Checkup => "sidebar-checkup",
        Screen::Cleanup => "sidebar-cleanup",
        Screen::Large => "sidebar-large",
        Screen::Memory => "sidebar-memory",
        Screen::Security => "sidebar-security",
        Screen::Uninstaller => "sidebar-uninstaller",
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
        Screen::Uninstaller => "sidebar-uninstaller-figure",
    }
}

/// The trailing figure of a sidebar row: the safe total after a Cleanup scan, the item count
/// on Large files. None before the first result, when the safe total is zero (a "0 KB" next
/// to the row says nothing), and on the other screens.
pub fn figure_for(screen: Screen, m: &TidyModel) -> Option<String> {
    match screen {
        Screen::Cleanup => m.cleanup.safe_bytes().filter(|b| *b > 0).map(format::size),
        Screen::Large => m.large.item_count().map(|n| format::thousands(n as u64)),
        Screen::Checkup | Screen::Memory | Screen::Security | Screen::Uninstaller => None,
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
        let disk_fill = disk
            .as_ref()
            .map(|disk| self.disk_fill(t, disk.used_fraction(), cx));
        let link_model = self.model.clone();

        let drag = self.drag_area(
            div()
                .id("sidebar-drag")
                .flex_none()
                .h(px(metrics::TOOLBAR_H)),
            cx,
        );

        let t = *t;
        let on_sidebar = t.on(t.sidebar_bg);
        let items = rows.into_iter().map(|(screen, figure)| {
            let selected = screen == current;
            let model = self.model.clone();
            let (hover, selected_bg) = (t.row_hover, t.sidebar_selected);
            div()
                .id(("sidebar-row", screen as usize))
                .role(gpui_kit::accesskit::Role::Tab)
                .aria_selected(selected)
                .debug_selector(move || selector_for(screen).to_string())
                .h(px(metrics::SIDEBAR_ROW_H))
                .px(px(12.))
                .gap(px(8.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .cursor_pointer()
                .when(selected, |d| d.bg(selected_bg))
                .when(!selected, |d| d.hover(move |s| s.bg(hover)))
                .on_click(move |_, _, cx| {
                    model.update(cx, |m, cx| m.select_screen(screen, cx));
                })
                .key_stop(&on_sidebar)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(14.))
                        .display(FontWeight::SEMIBOLD)
                        .text_color(if selected { t.on_accent } else { t.text_2 })
                        .whitespace_nowrap()
                        .child(screen.title()),
                )
                .when_some(figure, |d, figure| {
                    d.child(
                        div()
                            .debug_selector(move || figure_selector_for(screen).to_string())
                            .flex_none()
                            .text_size(px(12.))
                            .display(FontWeight::SEMIBOLD)
                            .font_features(tnum())
                            .text_color(if selected { t.on_accent } else { t.text_2 })
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
                                .text_size(px(14.))
                                .display(FontWeight::BOLD)
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
                                .children(disk_fill),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_features(tnum())
                                .text_color(t.text_2)
                                .whitespace_nowrap()
                                .child(format::disk_line(disk.available, disk.total)),
                        )
                        // The button's own 6 pt padding sits outside the column, so its
                        // text lines up with the disk line above it.
                        .child(div().ml(px(-6.)).flex().child(plain_button(
                            "sysdata-link",
                            "sysdata-link".to_string(),
                            SYSDATA_LINK,
                            true,
                            &on_sidebar,
                            move |_, cx| {
                                link_model.update(cx, |m, cx| m.open_system_data(cx));
                            },
                        ))),
                )
            })
    }
}

impl TidyView {
    /// The used part of the disk bar. After a clean or a trash it moves to the new share over
    /// 320 ms (design section 20); a move that is still playing retargets from where it is.
    /// The first frame, a demo and Reduce Motion show the share at once.
    fn disk_fill(&mut self, t: &Theme, used: f32, cx: &mut Context<Self>) -> AnyElement {
        let playing = |at: std::time::Instant| at.elapsed() < m::DISK;
        if self.disk_move.is_some_and(|(at, _, _)| !playing(at)) {
            self.disk_move = None;
        }
        if let Some(shown) = self.disk_shown
            && (shown - used).abs() > 1e-4
            && !cx.reduce_motion()
            && crate::demo::state(cx).is_none()
        {
            // Start from where the bar is now, mid-move or not.
            let from = match self.disk_move {
                Some((at, _, from)) => {
                    let d = m::enter().sample(at.elapsed().as_secs_f32() / m::DISK.as_secs_f32());
                    from + (shown - from) * d
                }
                None => shown,
            };
            self.card_generation += 1;
            self.disk_move = Some((std::time::Instant::now(), self.card_generation, from));
        }
        self.disk_shown = Some(used);
        let fill = div().h_full().rounded(px(2.)).bg(t.accent);
        match self.disk_move {
            Some((_, generation, from)) => fill
                .w(relative(from))
                .with_animation(
                    ("disk-bar", generation),
                    Animation::new(m::DISK).with_easing(m::curve(m::enter())),
                    move |el, d| el.w(relative(from + (used - from) * d)),
                )
                .into_any_element(),
            None => fill.w(relative(used)).into_any_element(),
        }
    }
}
