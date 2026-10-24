//! Windows and the pieces they share: buttons, the checkbox, the segmented control and tags.

pub mod cleanup;
pub mod dialogs;
pub mod history;
pub mod large;
pub mod main_window;
pub mod sidebar;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::app::Tri;
use crate::theme::{Theme, metrics, tnum};

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

/// A 24 px high text button (radius 6, 12 px weight 500). Primary uses the accent color;
/// secondary uses `button_bg` with the edge shadow of the current appearance. A disabled button
/// is dimmed and ignores clicks.
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
        .h(px(24.))
        .px(px(10.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .font_features(tnum())
        .whitespace_nowrap()
        .child(label.into())
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
    let base = if primary {
        base.bg(t.accent).text_color(gpui_kit::white())
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

/// A plain text button (no background until hovered): 12 px weight 500 in `text_2`, padding
/// 3 6, radius 5 ("Empty Trash...").
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

/// A 16 px checkbox with three states. Off is an outline, On and Mixed are filled with the
/// accent color and a white check or dash. A disabled one is drawn at 40% opacity.
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
        Tri::Off => base
            .bg(t.window_bg)
            .border_1()
            .border_color(t.separator_strong),
        Tri::On => base.bg(t.accent).child(
            Icon::new(IconName::Check)
                .size(px(12.))
                .text_color(gpui_kit::white()),
        ),
        Tri::Mixed => base.bg(t.accent).child(
            Icon::new(IconName::Minus)
                .size(px(12.))
                .text_color(gpui_kit::white()),
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

/// A two-button group, 24 px high: the selected segment is a raised button, the others are
/// plain text on a `fill` track.
pub fn segmented(id: &'static str, segments: Vec<Segment>, t: &Theme) -> Div {
    let (selected_fill, shadow, text, text_2) =
        (t.button_fill(), t.button_shadow(), t.text, t.text_2);
    div()
        .flex_none()
        .h(px(24.))
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(7.))
        .bg(t.fill)
        .flex()
        .items_center()
        .children(segments.into_iter().enumerate().map(move |(i, seg)| {
            let on_click = seg.on_click;
            div()
                .id((id, i))
                .debug_selector(|| seg.selector)
                .h(px(20.))
                .px(px(9.))
                .rounded(px(5.))
                .flex()
                .items_center()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .when(seg.selected, |d| {
                    d.bg(selected_fill).text_color(text).shadow(shadow.clone())
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
        .font_weight(FontWeight::SEMIBOLD)
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
