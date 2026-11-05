//! Design tokens: the Preflight look (design section 11.x, 2026-10-25), in light and dark sets.
//! Warm neutral grounds, ink buttons and an ink-filled selected row, one green and one amber,
//! Avenir Next for the display type. Every text and background pair is held to WCAG AA by the
//! `documented_contrast_ratios_hold` test.
//!
//! Views pick the set from `window.appearance()` on every render and observe appearance
//! changes, so switching macOS between light and dark re-renders.

use std::sync::{Arc, OnceLock};

use gpui_kit::{
    App, BoxShadow, FontFeatures, Hsla, Pixels, Window, WindowAppearance, point, px, rgb,
};

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

fn hexa(c: u32, alpha: f32) -> Hsla {
    let mut h: Hsla = rgb(c).into();
    h.a = alpha;
    h
}

fn black(alpha: f32) -> Hsla {
    Hsla {
        h: 0.,
        s: 0.,
        l: 0.,
        a: alpha,
    }
}

fn white(alpha: f32) -> Hsla {
    Hsla {
        h: 0.,
        s: 0.,
        l: 1.,
        a: alpha,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// Toolbar and list background (unified).
    pub window_bg: Hsla,
    /// Status bar.
    pub bar_bg: Hsla,
    /// Focused field, floating menus.
    pub raised: Hsla,
    /// Notice band, segmented control track.
    pub fill: Hsla,
    /// Glyph tiles.
    pub fill_2: Hsla,
    pub text: Hsla,
    pub text_2: Hsla,
    /// Icons and separator dots only, not for text that must be read.
    pub text_3: Hsla,
    pub separator: Hsla,
    /// Outlines: tags, unchecked checkboxes.
    pub separator_strong: Hsla,
    pub accent: Hsla,
    /// Focus ring outer 3 px: accent at 32% (light) or 42% (dark).
    pub accent_ring: Hsla,
    /// Text and icons on `accent` and on the selected sidebar row (ink buttons).
    pub on_accent: Hsla,
    /// The quiet gray of a check dot that is not a pass in color: every dot of a row whose
    /// checks all pass, and the empty dots of a row with a failed check.
    pub dot: Hsla,
    pub track: Hsla,
    pub success: Hsla,
    pub success_badge: Hsla,
    pub success_tint: Hsla,
    pub warning: Hsla,
    pub warning_badge: Hsla,
    pub warning_bg: Hsla,
    pub error: Hsla,
    pub error_badge: Hsla,
    pub error_bg: Hsla,
    pub gray_badge: Hsla,
    /// Secondary buttons.
    pub button_bg: Hsla,
    /// Menus, dialogs.
    pub pop_bg: Hsla,
    pub group_bg: Hsla,
    pub row_hover: Hsla,
    pub row_pressed: Hsla,
    /// Sidebar background (C24).
    pub sidebar_bg: Hsla,
    /// Selected sidebar row.
    pub sidebar_selected: Hsla,
    /// Outline of the Review, App data and Package tags. Equals `separator_strong`.
    pub tag_border: Hsla,
    /// Outline of an unticked checkbox: black 28% (light) or white 30% (dark), so an empty box
    /// reads as a control (about 3:1 on the window), not as a faint frame.
    pub checkbox_border: Hsla,
    /// Fill of the Empty Trash confirm button (white text). Equals `error_badge`.
    pub danger_button: Hsla,
    /// The opaque color secondary buttons sit on (`window_bg` unless set with [`Theme::on`]).
    /// gpui fills a spread shadow under its element, so a translucent `button_bg` would also
    /// pick up the edge color. Buttons paint [`Theme::button_fill`] instead.
    pub surface: Hsla,
    /// Device pixels per point of the window the tokens are for (2 on Retina, 1 on a
    /// standard display). Hairline edges depend on it, see [`Theme::edge`].
    pub scale: f32,
    /// Whether the last input was the keyboard. Only then do controls draw their focus ring,
    /// so a click never leaves a ring behind. See [`Theme::focus_shadow`] and [`ring_visible`].
    pub focus_ring: bool,
}

thread_local! {
    static RING_VISIBLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether focus rings are drawn: true after a key press, false after a mouse press. GPUI's own
/// `focus_visible` follows the last input too, but it also flips back on a mouse move, which
/// would hide the ring when the trackpad is brushed. Tidy flips on key and mouse presses only.
pub fn ring_visible() -> bool {
    RING_VISIBLE.with(|c| c.get())
}

/// Sets [`ring_visible`]. Returns true when the value changed.
pub fn set_ring_visible(on: bool) -> bool {
    RING_VISIBLE.with(|c| c.replace(on) != on)
}

impl Theme {
    pub fn light() -> Theme {
        let separator_strong = hex(0x8c8e87);
        let error_badge = hex(0xc62d1f);
        let ink = 0x0d0e0c;
        Theme {
            dark: false,
            window_bg: hex(0xfafaf8),
            bar_bg: hex(0xf0f0ec),
            raised: hex(0xfafaf8),
            fill: hex(0xefefeb),
            fill_2: hex(0xe6e6e0),
            text: hex(ink),
            text_2: hex(0x555852),
            text_3: hex(0x6b6d66),
            separator: hex(0xdadad4),
            separator_strong,
            accent: hex(ink),
            accent_ring: hexa(ink, 0.35),
            on_accent: hex(0xfafaf8),
            dot: hex(0xcfcfc8),
            track: hex(0xdadad4),
            success: hex(0x0b7550),
            success_badge: hex(0x0b7550),
            success_tint: hexa(0x0b7550, 0.12),
            warning: hex(0xa64b00),
            warning_badge: hex(0xa64b00),
            warning_bg: hexa(0xa64b00, 0.10),
            error: hex(0xb42318),
            error_badge,
            error_bg: hexa(0xc62d1f, 0.08),
            gray_badge: separator_strong,
            button_bg: hex(0xfafaf8),
            pop_bg: hex(0xfafaf8),
            group_bg: hex(0xfafaf8),
            row_hover: hexa(ink, 0.045),
            row_pressed: hexa(ink, 0.09),
            sidebar_bg: hex(0xf0f0ec),
            sidebar_selected: hex(ink),
            tag_border: separator_strong,
            checkbox_border: hex(ink),
            danger_button: error_badge,
            surface: hex(0xfafaf8),
            scale: 2.,
            focus_ring: false,
        }
    }

    pub fn dark() -> Theme {
        let separator_strong = hex(0x6a6c66);
        let error_badge = hex(0xff6b5e);
        let ink = 0xf4f4f0;
        Theme {
            dark: true,
            window_bg: hex(0x0b0c0b),
            bar_bg: hex(0x131412),
            raised: hex(0x131412),
            fill: hex(0x1a1b19),
            fill_2: hex(0x242622),
            text: hex(ink),
            text_2: hex(0xa2a49e),
            text_3: hex(0x8a8c86),
            separator: hex(0x2a2c28),
            separator_strong,
            accent: hex(ink),
            accent_ring: hexa(ink, 0.45),
            on_accent: hex(0x0b0c0b),
            dot: hex(0x3a3c37),
            track: hex(0x2a2c28),
            success: hex(0x46d39a),
            success_badge: hex(0x46d39a),
            success_tint: hexa(0x46d39a, 0.13),
            warning: hex(0xf2a03d),
            warning_badge: hex(0xf2a03d),
            warning_bg: hexa(0xf2a03d, 0.12),
            error: hex(0xff8b7e),
            error_badge,
            error_bg: hexa(0xff6b5e, 0.12),
            gray_badge: separator_strong,
            button_bg: hex(0x1a1b19),
            pop_bg: hex(0x131412),
            group_bg: hex(0x131412),
            row_hover: hexa(ink, 0.06),
            row_pressed: hexa(ink, 0.10),
            sidebar_bg: hex(0x131412),
            sidebar_selected: hex(ink),
            tag_border: separator_strong,
            checkbox_border: hex(ink),
            danger_button: hex(0xc62d1f),
            surface: hex(0x0b0c0b),
            scale: 2.,
            focus_ring: false,
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
        }
    }

    pub fn of(window: &Window) -> Theme {
        Theme {
            focus_ring: ring_visible(),
            ..Theme::for_appearance(window.appearance()).at_scale(window.scale_factor())
        }
    }

    /// The same tokens for a window at `scale` device pixels per point.
    pub fn at_scale(self, scale: f32) -> Theme {
        Theme { scale, ..self }
    }

    /// The same tokens for controls drawn on `surface` (an opaque color).
    pub fn on(self, surface: Hsla) -> Theme {
        Theme { surface, ..self }
    }

    /// `button_bg` flattened on `surface`: what the CSS mockup shows, since there the edge
    /// shadow does not paint under the button.
    pub fn button_fill(&self) -> Hsla {
        self.surface.blend(self.button_bg)
    }

    /// Secondary button edge: a 1 pt ring in `separator_strong`, flat, no drop.
    pub fn button_shadow(&self) -> Vec<BoxShadow> {
        vec![self.ring(self.separator_strong)]
    }

    /// Menus and dialogs: `0 0 0 .5px black 14%` + a soft drop (dark: white 12% edge, black 60%
    /// drop). gpui's blur reaches about twice as far as the CSS blur of the same value, so the
    /// blur is halved here.
    pub fn pop_shadow(&self) -> Vec<BoxShadow> {
        let (edge_color, drop, y, blur) = if self.dark {
            (white(0.12), black(0.60), 10., 14.)
        } else {
            (black(0.14), black(0.22), 8., 12.)
        };
        vec![
            self.edge(edge_color),
            BoxShadow {
                color: drop,
                offset: point(px(0.), px(y)),
                blur_radius: px(blur),
                spread_radius: px(-6.),
                inset: false,
            },
        ]
    }

    /// The switch knob (Haul's): `0 1px 2px black 30%` + `0 0 0 .5px black 6%`. gpui's blur
    /// reaches about twice as far as the CSS blur of the same value, so it is halved.
    pub fn knob_shadow(&self) -> Vec<BoxShadow> {
        vec![
            BoxShadow {
                color: black(0.30),
                offset: point(px(0.), px(1.)),
                blur_radius: px(1.),
                spread_radius: px(0.),
                inset: false,
            },
            self.edge(black(0.06)),
        ]
    }

    /// Cards and groups (Check up, Memory): `fill` flattened on the window, so a hover tint
    /// on top of it stays opaque.
    pub fn card_bg(&self) -> Hsla {
        self.window_bg.blend(self.fill)
    }

    /// A card under the pointer: `row_hover` over [`Theme::card_bg`].
    pub fn card_hover(&self) -> Hsla {
        self.card_bg().blend(self.row_hover)
    }

    /// The scrim behind a dialog.
    pub fn scrim(&self) -> Hsla {
        black(if self.dark { 0.45 } else { 0.25 })
    }

    /// A 1 pt ring with no blur (a 2 pt line at 1x would be too heavy, so it stays 1 pt).
    pub fn ring(&self, color: Hsla) -> BoxShadow {
        BoxShadow {
            color,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(1.),
            inset: false,
        }
    }

    /// The keyboard focus ring: a 2 pt ring in `text` (ink in light, near white in dark) set
    /// 2 pt off the control, with the gap painted in `surface` so the ring also reads on an ink
    /// button or a selected row. `base` is the control's own shadow (a secondary button's edge),
    /// kept inside the ring. Ink on any surface is above 3:1, see `documented_pairs`.
    pub fn focus_shadow(&self, base: Vec<BoxShadow>) -> Vec<BoxShadow> {
        let spread = |width: f32, color: Hsla| BoxShadow {
            color,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(width),
            inset: false,
        };
        let mut shadows = vec![
            spread(FOCUS_GAP + FOCUS_RING_W, self.text),
            spread(FOCUS_GAP, self.surface),
        ];
        shadows.extend(base);
        shadows
    }

    /// A 0.5 pt ring with no blur, the mockup's `0 0 0 .5px` hairline. At 2x it is one device
    /// pixel. At 1x gpui draws a 0.5 px spread as a whole pixel at full strength, about twice
    /// as dark as the browser's half-covered pixel, so there it is one pixel at half alpha.
    pub fn edge(&self, color: Hsla) -> BoxShadow {
        let (spread, color) = if self.scale < 1.5 {
            (px(1.), color.opacity(0.5))
        } else {
            (px(0.5), color)
        };
        BoxShadow {
            color,
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: spread,
            inset: false,
        }
    }
}

/// Width of the keyboard focus ring, in points.
pub const FOCUS_RING_W: f32 = 2.;
/// Gap between a control and its focus ring, in points.
pub const FOCUS_GAP: f32 = 2.;

/// Sizes from the design (section 11), in points.
pub mod metrics {
    use super::*;

    pub const TOOLBAR_H: f32 = 52.;
    pub const SUMMARY_H: f32 = 64.;
    /// The line under the Figure on Cleanup.
    pub const SUMMARY_LINE_H: f32 = 28.;
    /// The Cleanup header row: the Figure at the left, the Clean button at the right.
    pub const HERO_H: f32 = 84.;
    /// The Figure's size on Cleanup, in points.
    pub const FIGURE_PT: f32 = 56.;
    pub const STATUS_H: f32 = 30.;
    pub const LIST_INSET: f32 = 8.;
    /// Cleanup group headers and item rows share one height, because `uniform_list` needs a
    /// fixed row height.
    pub const CLEANUP_ROW_H: f32 = 44.;
    pub const LARGE_ROW_H: f32 = 52.;
    pub const BREADCRUMB_H: f32 = 28.;
    pub const SIDEBAR_W: f32 = 200.;
    pub const SIDEBAR_ROW_H: f32 = 34.;
    pub const ACTION: f32 = 26.;
    /// The trailing area of a row that holds the hover actions.
    pub const TRAIL_W: f32 = 60.;
    /// The hover actions of a Cleanup row: Details, Reveal and more (design 17.1).
    pub const CLEAN_TRAIL_W: f32 = 86.;
    pub const BAR_H: f32 = 4.;
    pub const MAIN_W: f32 = 900.;
    pub const MAIN_H: f32 = 620.;
    pub const MAIN_MIN_W: f32 = 760.;
    pub const MAIN_MIN_H: f32 = 480.;
    pub const DIALOG_W: f32 = 420.;
    /// The Move to Trash receipt.
    pub const RECEIPT_W: f32 = 460.;
    pub const HISTORY_W: f32 = 560.;
    pub const HISTORY_H: f32 = 480.;

    pub fn p(v: f32) -> Pixels {
        px(v)
    }
}

/// The Figure type role: 22 / 28, Avenir Next 800, tabular, `text`. The summary number of the
/// screens other than Cleanup, whose own Figure is [`metrics::FIGURE_PT`].
pub mod figure {
    pub const SIZE: f32 = 22.;
    pub const LINE: f32 = 28.;
}

/// Motion constants: Haul's durations and curves that Tidy reuses.
pub mod motion {
    use std::time::Duration;

    use gpui_kit::base::Easing;

    /// A row fades out as its item reaches the Trash (Haul's REMOVE).
    pub const REMOVE: Duration = Duration::from_millis(140);
    /// The notice band in.
    pub const NOTE_IN: Duration = Duration::from_millis(160);
    /// The notice band out.
    pub const NOTE_OUT: Duration = Duration::from_millis(200);
    /// One turn of the scanning spinner.
    pub const SPIN: Duration = Duration::from_millis(900);
    /// The switch knob sliding to its new side after a click.
    pub const SWITCH: Duration = Duration::from_millis(160);
    /// A Check up card's result fading in over "Checking" when its part reports.
    pub const CARD_IN: Duration = Duration::from_millis(200);
    /// Play SWITCH or CARD_IN only within this time of the change, so a window that opens on
    /// a settled state (or a demo shot of it) shows it still.
    pub const CHANGE_GATE: Duration = Duration::from_millis(400);
    /// The size map: a folder tile growing to fill the map, or shrinking back into its place.
    pub const ZOOM: Duration = Duration::from_millis(220);
    /// The size map: the gap between one group's tiles fading in and the next.
    pub const STAGGER: Duration = Duration::from_millis(40);
    /// The sidebar disk bar moving to its new used share after a clean or a trash.
    pub const DISK: Duration = Duration::from_millis(320);

    fn bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> Easing {
        Easing::cubic_bezier(x1, y1, x2, y2).expect("control points are in range")
    }

    /// cubic-bezier(.16, 1, .3, 1)
    pub fn enter() -> Easing {
        bezier(0.16, 1., 0.3, 1.)
    }

    /// cubic-bezier(.4, 0, 1, 1)
    pub fn exit() -> Easing {
        bezier(0.4, 0., 1., 1.)
    }

    /// An easing as the closure `Animation::with_easing` takes.
    pub fn curve(easing: Easing) -> impl Fn(f32) -> f32 + 'static {
        move |t| easing.sample(t)
    }
}

/// Tabular figures for numbers that change (sizes, counts, dates). Built once; cloning shares
/// the `Arc`.
pub fn tnum() -> FontFeatures {
    static TNUM: OnceLock<FontFeatures> = OnceLock::new();
    TNUM.get_or_init(|| FontFeatures(Arc::new(vec![("tnum".into(), 1)])))
        .clone()
}

/// Aligns the gpui-component theme with Tidy's tokens for the current appearance, so the
/// controls Tidy takes from gpui-component (Tooltip) match the rest of the window. Call it right
/// after `gpui_kit::component::theme::Theme::sync_system_appearance`, which reloads the stock
/// colors.
pub fn sync_component_tokens(cx: &mut App) {
    let dark = matches!(
        cx.window_appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    );
    apply_component_tokens(if dark { Theme::dark() } else { Theme::light() }, cx);
}

/// Like `sync_component_tokens` for a window's own appearance.
pub fn sync_component_tokens_for(window: &Window, cx: &mut App) {
    apply_component_tokens(Theme::of(window), cx);
}

fn apply_component_tokens(t: Theme, cx: &mut App) {
    gpui_kit::component::theme::Theme::update(cx, |c| {
        c.muted_foreground = t.text_2;
        c.caret = t.accent;
        c.selection = t.accent_ring;
        c.ring = t.accent;
        c.foreground = t.text;
        c.popover = t.pop_bg;
        c.popover_foreground = t.text;
        c.border = t.separator_strong;
    });
}

/// The display face (Preflight): the big figure at 800, headings, sizes and buttons at 700, the
/// sidebar at 600. Body text stays in the system font. Avenir Next ships with macOS.
pub const DISPLAY_FONT: &str = "Avenir Next";

/// `.display(weight)` sets the display face and a weight on any styled element.
pub trait DisplayType: gpui_kit::Styled + Sized {
    fn display(self, weight: gpui_kit::FontWeight) -> Self {
        self.font_family(DISPLAY_FONT).font_weight(weight)
    }
}

impl<T: gpui_kit::Styled> DisplayType for T {}

/// Monospaced family for paths: the system monospaced font (SF Mono). CoreText resolves this
/// family name by collection on every Mac; if a system ever lacked it, gpui falls back to its
/// default font stack.
pub const MONO_FONT: &str = ".AppleSystemUIFontMonospaced";

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn appearance_picks_set() {
        assert!(Theme::for_appearance(WindowAppearance::Dark).dark);
        assert!(Theme::for_appearance(WindowAppearance::VibrantDark).dark);
        assert!(!Theme::for_appearance(WindowAppearance::Light).dark);
        assert!(!Theme::for_appearance(WindowAppearance::VibrantLight).dark);
    }

    #[test]
    fn tokens_differ_between_sets() {
        let (l, d) = (Theme::light(), Theme::dark());
        assert_ne!(l.window_bg, d.window_bg);
        assert_ne!(l.text, d.text);
        assert_ne!(l.accent, d.accent);
        assert_ne!(l.sidebar_bg, d.sidebar_bg);
        assert!((l.accent_ring.a - 0.35).abs() < 1e-6);
        assert!((d.accent_ring.a - 0.45).abs() < 1e-6);
    }

    #[test]
    fn preflight_tokens_match_the_approved_direction() {
        let (l, d) = (Theme::light(), Theme::dark());
        assert_eq!(l.window_bg, hex(0xfafaf8));
        assert_eq!(l.sidebar_bg, hex(0xf0f0ec));
        assert_eq!(l.text, hex(0x0d0e0c));
        assert_eq!(l.text_2, hex(0x555852));
        assert_eq!(l.separator, hex(0xdadad4));
        assert_eq!(l.success, hex(0x0b7550));
        assert_eq!(l.warning, hex(0xa64b00));
        assert_eq!(l.fill, hex(0xefefeb));
        assert_eq!(l.dot, hex(0xcfcfc8));
        assert_eq!(d.window_bg, hex(0x0b0c0b));
        assert_eq!(d.sidebar_bg, hex(0x131412));
        assert_eq!(d.text, hex(0xf4f4f0));
        assert_eq!(d.text_2, hex(0xa2a49e));
        assert_eq!(d.separator, hex(0x2a2c28));
        assert_eq!(d.success, hex(0x46d39a));
        assert_eq!(d.warning, hex(0xf2a03d));
        assert_eq!(d.fill, hex(0x1a1b19));
        assert_eq!(d.dot, hex(0x3a3c37));
        // Ink buttons and an ink-filled selected row, in both sets.
        for t in [l, d] {
            assert_eq!(t.accent, t.text);
            assert_eq!(t.sidebar_selected, t.text);
            assert_eq!(t.tag_border, t.separator_strong);
            assert_eq!(t.card_bg(), t.fill);
        }
    }

    #[test]
    fn metrics_match_the_window_spec() {
        assert_eq!((metrics::MAIN_W, metrics::MAIN_H), (900., 620.));
        assert_eq!((metrics::MAIN_MIN_W, metrics::MAIN_MIN_H), (760., 480.));
        assert_eq!(metrics::SIDEBAR_W, 200.);
        assert_eq!(metrics::CLEANUP_ROW_H, 44.);
        assert_eq!(metrics::LARGE_ROW_H, 52.);
        assert_eq!((figure::SIZE, figure::LINE), (22., 28.));
    }

    #[test]
    fn shadows_follow_the_spec() {
        let (l, d) = (Theme::light(), Theme::dark());
        assert_eq!(l.button_shadow().len(), 1);
        assert_eq!(d.button_shadow().len(), 1);
        assert_eq!(l.pop_shadow().len(), 2);
        assert!(l.pop_shadow().iter().all(|s| !s.inset));
        assert_eq!(l.knob_shadow().len(), 2);
    }

    #[test]
    fn hairlines_are_half_strength_at_1x() {
        let l2 = Theme::light();
        assert_eq!(l2.scale, 2.);
        let e2 = l2.edge(black(0.20));
        assert_eq!(e2.spread_radius, px(0.5));
        assert!((e2.color.a - 0.20).abs() < 1e-6);
        let e1 = Theme::light().at_scale(1.).edge(black(0.20));
        assert_eq!(e1.spread_radius, px(1.));
        assert!((e1.color.a - 0.10).abs() < 1e-6);
    }

    #[test]
    fn button_fill_is_flattened_on_the_surface() {
        let d = Theme::dark();
        let fill = d.button_fill();
        assert!((fill.a - 1.).abs() < 1e-4);
        let rgb = fill.to_rgb();
        assert!((rgb.r * 255. - 26.).abs() < 1.5, "{rgb:?}");
        assert_eq!(Theme::light().button_fill(), Theme::light().button_bg);
    }

    /// WCAG relative luminance of an opaque color.
    fn luminance(c: Hsla) -> f32 {
        let rgb = c.to_rgb();
        let lin = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(rgb.r) + 0.7152 * lin(rgb.g) + 0.0722 * lin(rgb.b)
    }

    /// The WCAG contrast ratio of `fg` over `bg`; a translucent `fg` is flattened on `bg`.
    fn ratio(fg: Hsla, bg: Hsla) -> f32 {
        let (a, b) = (luminance(bg.blend(fg)), luminance(bg));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// (name, foreground, background, minimum ratio). 4.5 for text, 3 for large display text and
    /// for the outlines and marks that identify a control.
    fn documented_pairs(t: &Theme) -> Vec<(&'static str, Hsla, Hsla, f32)> {
        let band = |c: Hsla| t.window_bg.blend(c);
        let white = Hsla {
            h: 0.,
            s: 0.,
            l: 1.,
            a: 1.,
        };
        vec![
            ("text on window", t.text, t.window_bg, 4.5),
            ("text on sidebar", t.text, t.sidebar_bg, 4.5),
            ("text on band (fill)", t.text, t.fill, 4.5),
            ("text on dialog", t.text, t.pop_bg, 4.5),
            ("text on status bar", t.text, t.bar_bg, 4.5),
            ("text_2 on window", t.text_2, t.window_bg, 4.5),
            ("text_2 on sidebar", t.text_2, t.sidebar_bg, 4.5),
            ("text_2 on band (fill)", t.text_2, t.fill, 4.5),
            ("text_2 on dialog", t.text_2, t.pop_bg, 4.5),
            ("text_2 on status bar", t.text_2, t.bar_bg, 4.5),
            ("text_2 on hovered row", t.text_2, band(t.row_hover), 4.5),
            ("text_3 on window", t.text_3, t.window_bg, 4.5),
            ("text_3 on band (fill)", t.text_3, t.fill, 4.5),
            ("ok on window", t.success, t.window_bg, 4.5),
            ("ok on band (fill)", t.success, t.fill, 4.5),
            ("ok on sidebar", t.success, t.sidebar_bg, 4.5),
            ("ok on status bar", t.success, t.bar_bg, 4.5),
            ("amber on window", t.warning, t.window_bg, 4.5),
            ("amber on band (fill)", t.warning, t.fill, 4.5),
            ("amber on sidebar", t.warning, t.sidebar_bg, 4.5),
            ("amber on amber tint", t.warning, band(t.warning_bg), 4.5),
            ("error on window", t.error, t.window_bg, 4.5),
            ("error on band (fill)", t.error, t.fill, 4.5),
            ("error on error tint", t.error, band(t.error_bg), 4.5),
            ("on_accent on ink button", t.on_accent, t.accent, 4.5),
            (
                "on_accent on selected row",
                t.on_accent,
                t.sidebar_selected,
                4.5,
            ),
            ("white on Empty Trash button", white, t.danger_button, 4.5),
            ("figure (text) on window, large", t.text, t.window_bg, 3.0),
            (
                "checkbox outline on window",
                t.checkbox_border,
                t.window_bg,
                3.0,
            ),
            ("tag outline on window", t.tag_border, t.window_bg, 3.0),
            (
                "failed dot outline (text_2) on window",
                t.text_2,
                t.window_bg,
                3.0,
            ),
            ("ok dot on window", t.success, t.window_bg, 3.0),
            ("focus ring on window", t.text, t.window_bg, 3.0),
            ("focus ring on sidebar", t.text, t.sidebar_bg, 3.0),
            ("focus ring on dialog", t.text, t.pop_bg, 3.0),
            ("focus ring on band (fill)", t.text, t.fill, 3.0),
            ("focus ring on status bar", t.text, t.bar_bg, 3.0),
            ("focus ring on hovered row", t.text, band(t.row_hover), 3.0),
            ("focus ring on card", t.text, t.card_bg(), 3.0),
        ]
    }

    #[test]
    fn the_focus_ring_is_two_points_wide_and_two_points_off_the_control() {
        for t in [Theme::light(), Theme::dark()] {
            let ring = t.focus_shadow(t.button_shadow());
            // Ink ring first (painted underneath), then the gap in the surface color, then the
            // control's own edge.
            assert_eq!(ring.len(), 3);
            assert_eq!(ring[0].color, t.text);
            assert_eq!(ring[1].color, t.surface);
            let spread = |i: usize| f32::from(ring[i].spread_radius);
            assert_eq!(spread(1), FOCUS_GAP);
            assert_eq!(spread(0) - spread(1), FOCUS_RING_W);
            assert_eq!(FOCUS_RING_W, 2.);
            assert_eq!(FOCUS_GAP, 2.);
        }
    }

    #[test]
    fn documented_contrast_ratios_hold() {
        for t in [Theme::light(), Theme::dark()] {
            let mode = if t.dark { "dark" } else { "light" };
            for (name, fg, bg, min) in documented_pairs(&t) {
                let r = ratio(fg, bg);
                println!("CONTRAST {mode} | {name}: {r:.2} (min {min})");
                assert!(r >= min, "{mode}: {name} is {r:.2}, needs {min}");
            }
        }
    }

    #[test]
    fn the_quiet_dot_is_not_a_text_color() {
        // The all-passed state draws dots in `dot` and the count in `text_2`: the count, which
        // carries the meaning, is readable; the dots are decoration and sit near the ground.
        for t in [Theme::light(), Theme::dark()] {
            assert!(ratio(t.text_2, t.window_bg) >= 4.5);
            assert!(ratio(t.dot, t.window_bg) < 3.0);
            assert!(ratio(t.dot, t.window_bg) > 1.2);
        }
    }

    #[test]
    fn tnum_is_cached() {
        let (a, b) = (tnum(), tnum());
        assert!(Arc::ptr_eq(&a.0, &b.0));
        assert_eq!(a.0.as_slice(), &[("tnum".to_string(), 1)]);
    }

    #[test]
    fn motion_durations_are_positive_and_curves_end_at_one() {
        use motion::*;
        for d in [
            REMOVE,
            NOTE_IN,
            NOTE_OUT,
            SPIN,
            SWITCH,
            CARD_IN,
            CHANGE_GATE,
        ] {
            assert!(d > std::time::Duration::ZERO);
        }
        assert_eq!(REMOVE, std::time::Duration::from_millis(140));
        for easing in [enter(), exit()] {
            let mut last = -1.0;
            for i in 0..=100 {
                let v = easing.sample(i as f32 / 100.);
                assert!(v >= last - 1e-5, "{v} < {last}");
                last = v;
            }
            assert!((easing.sample(1.0) - 1.0).abs() < 1e-4);
        }
        assert!((curve(enter())(1.0) - 1.0).abs() < 1e-4);
    }
}
