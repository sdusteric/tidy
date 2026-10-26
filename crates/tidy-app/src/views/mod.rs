//! Windows and the pieces they share: buttons, the checkbox, the segmented control and tags.

pub mod checkup;
pub mod cleanup;
pub mod dialogs;
pub mod history;
pub mod large;
pub mod main_window;
pub mod memory;
pub mod overlay;
pub mod security;
pub mod sidebar;
pub mod trust;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::app::Tri;
use crate::theme::{DisplayType, Theme, metrics, tnum};

/// A 26 x 26 icon button (radius 6, 16 px icon). The icon takes its color from this element,
/// so hover lifts it from `text_2` to `text`.
pub fn icon_button(
    id: impl Into<ElementId>,
    selector: String,
    icon: IconName,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (hover, pressed, lifted) = (t.row_hover, t.row_pressed, t.text);
    div()
        .id(id)
        .debug_selector(|| selector)
        .flex_none()
        .size(px(metrics::ACTION))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(t.text_2)
        .cursor_pointer()
        // The click must not start a window drag or a row click underneath.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .hover(move |s| s.bg(hover).text_color(lifted))
        .active(move |s| s.bg(pressed))
        .child(Icon::new(icon).size(px(16.)))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx)
        })
}

/// A 26 px high text button (radius 7, 12 px, display 700). Primary is an ink button (`accent`
/// fill, `on_accent` text); secondary uses `button_bg` with a 1 pt ring. A disabled button is
/// dimmed and ignores clicks.
pub fn text_button(
    id: impl Into<ElementId>,
    selector: String,
    label: impl Into<SharedString>,
    primary: bool,
    enabled: bool,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let pressed = t.row_pressed;
    let base = div()
        .id(id)
        .debug_selector(|| selector)
        .flex_none()
        .h(px(26.))
        .px(px(11.))
        .rounded(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .display(FontWeight::BOLD)
        .font_features(tnum())
        .whitespace_nowrap()
        .child(label.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let base = if primary {
        base.bg(t.accent).text_color(t.on_accent)
    } else {
        base.bg(t.button_fill())
            .text_color(t.text)
            .shadow(t.button_shadow())
    };
    if enabled {
        base.cursor_pointer()
            .when(primary, |b| b.active(|s| s.opacity(0.9)))
            .when(!primary, |b| b.active(move |s| s.bg(pressed)))
            .on_click(move |_, window, cx| on_click(window, cx))
    } else {
        base.opacity(0.4)
    }
}

/// The large primary button of a screen's one main action (Run Check up, the empty states'
/// Scan): 34 px high, radius 8, 14 px display 700, ink fill.
pub fn large_button(
    id: impl Into<ElementId>,
    selector: String,
    label: impl Into<SharedString>,
    enabled: bool,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .debug_selector(|| selector)
        .flex_none()
        .h(px(34.))
        .px(px(16.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .bg(t.accent)
        .text_color(t.on_accent)
        .text_size(px(14.))
        .display(FontWeight::BOLD)
        .whitespace_nowrap()
        .child(label.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if enabled {
        base.cursor_pointer()
            .active(|s| s.opacity(0.9))
            .on_click(move |_, window, cx| on_click(window, cx))
    } else {
        base.opacity(0.4)
    }
}

/// A plain text button (no background until hovered): 12 px weight 500 in `text_2`, padding
/// 3 6, radius 5 ("Empty Trash…").
pub fn plain_button(
    id: impl Into<ElementId>,
    selector: String,
    label: impl Into<SharedString>,
    enabled: bool,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (hover, pressed, lifted) = (t.row_hover, t.row_pressed, t.text);
    let base = div()
        .id(id)
        .debug_selector(|| selector)
        .flex_none()
        .px(px(6.))
        .py(px(3.))
        .rounded(px(5.))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.text_2)
        .whitespace_nowrap()
        .child(label.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    if enabled {
        base.cursor_pointer()
            .hover(move |s| s.bg(hover).text_color(lifted))
            .active(move |s| s.bg(pressed))
            .on_click(move |_, window, cx| on_click(window, cx))
    } else {
        base.opacity(0.4)
    }
}

/// A 16 px checkbox with three states. Off is a 1.5 pt `checkbox_border` (ink) outline, On and
/// Mixed are filled with ink and an `on_accent` check or dash. A disabled one is drawn at 40%
/// opacity, and a disabled Off box is filled with `fill` so it does not read as clickable.
pub fn checkbox(
    id: impl Into<ElementId>,
    selector: String,
    state: Tri,
    enabled: bool,
    t: &Theme,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .debug_selector(|| selector)
        .flex_none()
        .size(px(16.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let base = match state {
        Tri::Off if enabled => base
            .bg(t.window_bg)
            .border(px(1.5))
            .border_color(t.checkbox_border),
        Tri::Off => base
            .bg(t.window_bg.blend(t.fill))
            .border(px(1.5))
            .border_color(t.separator_strong),
        Tri::On => base.bg(t.accent).child(
            Icon::new(IconName::Check)
                .size(px(12.))
                .text_color(t.on_accent),
        ),
        Tri::Mixed => base.bg(t.accent).child(
            Icon::new(IconName::Minus)
                .size(px(12.))
                .text_color(t.on_accent),
        ),
    };
    if enabled {
        base.cursor_pointer().on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx)
        })
    } else {
        base.opacity(0.4)
    }
}

/// A click handler for a segment.
pub type ClickFn = Box<dyn Fn(&mut Window, &mut App)>;

/// One option of a [`segmented`] control.
pub struct Segment {
    pub label: &'static str,
    pub selected: bool,
    pub selector: String,
    pub on_click: ClickFn,
}

/// A two-button group, 26 px high: the selected segment is an ink pill, the others are plain
/// text on a `fill` track.
pub fn segmented(id: &'static str, segments: Vec<Segment>, t: &Theme) -> Div {
    let (selected_fill, selected_text, text_2) = (t.accent, t.on_accent, t.text_2);
    div()
        .flex_none()
        .h(px(26.))
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(8.))
        .bg(t.fill)
        .flex()
        .items_center()
        .children(segments.into_iter().enumerate().map(move |(i, seg)| {
            let on_click = seg.on_click;
            div()
                .id((id, i))
                .debug_selector(|| seg.selector)
                .h(px(22.))
                .px(px(10.))
                .rounded(px(6.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .display(FontWeight::BOLD)
                .whitespace_nowrap()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(seg.selected, |d| {
                    d.bg(selected_fill).text_color(selected_text)
                })
                .when(!seg.selected, |d| d.text_color(text_2))
                .child(seg.label)
                .on_click(move |_, window, cx| on_click(window, cx))
        }))
}

/// A tag such as "Review" or "App data": 11 px weight 600, outlined, radius 4, 16 px high.
pub fn tag(label: impl Into<SharedString>, t: &Theme) -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.tag_border)
        .flex()
        .items_center()
        .text_size(px(11.))
        .line_height(px(12.))
        .display(FontWeight::BOLD)
        .text_color(t.text_2)
        .whitespace_nowrap()
        .child(label.into())
}

/// A glyph tile: a rounded `fill_2` square with an icon.
pub fn glyph_tile(icon: IconName, size_pt: f32, radius: f32, icon_pt: f32, t: &Theme) -> Div {
    div()
        .flex_none()
        .size(px(size_pt))
        .rounded(px(radius))
        .bg(t.fill_2)
        .flex()
        .items_center()
        .justify_center()
        .text_color(t.text_2)
        .child(Icon::new(icon).size(px(icon_pt)))
}

/// A one-pixel hairline across the top or bottom of a relative parent.
pub fn hairline(t: &Theme) -> Div {
    div().h(px(1.)).w_full().bg(t.separator)
}

/// The edge between content that stays put (a summary) and a list that scrolls under it: a
/// hairline while the list is scrolled, the same height in transparent otherwise, so the list
/// does not move when it appears.
pub fn scroll_edge(t: &Theme, scrolled: bool) -> Div {
    div().flex_none().h(px(1.)).w_full().bg(if scrolled {
        t.separator
    } else {
        gpui_kit::transparent_black()
    })
}

/// How a row's check dots are colored. When every check passes the dots stay in the quiet `dot`
/// gray and the count in `text_2`, so a page of safe rows is calm. Only a row with a failed
/// check uses color: passed dots green, failed dots hollow, the count in amber. The count
/// ("k of N") is always drawn, so the dots never carry the meaning alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DotTone {
    Quiet,
    Failed,
}

pub fn dot_tone(passed: usize, total: usize) -> DotTone {
    if passed >= total {
        DotTone::Quiet
    } else {
        DotTone::Failed
    }
}

/// "5 of 5", "3 of 5": the text of the count beside the dots.
pub fn dots_count(passed: usize, total: usize) -> String {
    format!("{passed} of {total}")
}

/// The width of the dots and their count, wide enough for the row with the most checks.
pub const DOTS_W: f32 = 140.;

/// `total` dots for the guards of a row, then "k of N". Passed dots come first.
pub fn check_dots(passed: usize, total: usize, selector: String, t: &Theme) -> Div {
    let tone = dot_tone(passed, total);
    let dots = (0..total).map(move |i| {
        let pass = i < passed;
        let d = div().flex_none().size(px(9.)).rounded_full();
        match (tone, pass) {
            (DotTone::Quiet, _) => d.bg(t.dot),
            (DotTone::Failed, true) => d.bg(t.success),
            // A failed check is a ring, a different shape from a passed one.
            (DotTone::Failed, false) => d.border(px(1.5)).border_color(t.text_2),
        }
    });
    div()
        .debug_selector(|| selector)
        .flex_none()
        .w(px(DOTS_W))
        .flex()
        .items_center()
        .justify_end()
        .gap(px(10.))
        .child(div().flex().items_center().gap(px(5.)).children(dots))
        .child(
            div()
                .flex_none()
                .w(px(42.))
                .text_size(px(13.))
                .font_features(tnum())
                .text_color(match tone {
                    DotTone::Quiet => t.text_2,
                    DotTone::Failed => t.warning,
                })
                .whitespace_nowrap()
                .child(dots_count(passed, total)),
        )
}

#[cfg(test)]
mod dot_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn all_passed_is_quiet_and_a_failure_uses_color() {
        assert_eq!(dot_tone(5, 5), DotTone::Quiet);
        assert_eq!(dot_tone(3, 3), DotTone::Quiet);
        assert_eq!(dot_tone(4, 5), DotTone::Failed);
    }

    #[test]
    fn the_count_is_always_words() {
        assert_eq!(dots_count(5, 5), "5 of 5");
        assert_eq!(dots_count(4, 6), "4 of 6");
    }
}
