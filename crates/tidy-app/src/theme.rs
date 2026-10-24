//! Design tokens: Haul v2 (docs/mockup/v2/DESIGN.md in the Haul repo) plus the four tokens and
//! the Figure type role that Tidy adds (design section 11), in light and dark sets.
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
    /// Fill of the Empty Trash confirm button (white text). Equals `error_badge`.
    pub danger_button: Hsla,
    /// The opaque color secondary buttons sit on (`window_bg` unless set with [`Theme::on`]).
    /// gpui fills a spread shadow under its element, so a translucent `button_bg` would also
    /// pick up the edge color. Buttons paint [`Theme::button_fill`] instead.
    pub surface: Hsla,
    /// Device pixels per point of the window the tokens are for (2 on Retina, 1 on a
    /// standard display). Hairline edges depend on it, see [`Theme::edge`].
    pub scale: f32,
}

impl Theme {
    pub fn light() -> Theme {
        let separator_strong = black(0.16);
        let error_badge = hex(0xe5302a);
        Theme {
            dark: false,
            window_bg: hex(0xffffff),
            bar_bg: hex(0xf7f7f8),
            raised: hex(0xffffff),
            fill: black(0.05),
            fill_2: hex(0xf0f0f2),
            text: hex(0x1d1d1f),
            text_2: hex(0x6e6e73),
            text_3: hex(0x8a8a8f),
            separator: black(0.08),
            separator_strong,
            accent: hex(0x007aff),
            accent_ring: hexa(0x007aff, 0.32),
            track: black(0.08),
            success: hex(0x248a3d),
            success_badge: hex(0x30b350),
            success_tint: hexa(0x30b350, 0.12),
            warning: hex(0xb25000),
            warning_badge: hex(0xf08a00),
            warning_bg: hexa(0xf08a00, 0.10),
            error: hex(0xd70015),
            error_badge,
            error_bg: hexa(0xe5302a, 0.08),
            gray_badge: hex(0x8e8e93),
            button_bg: hex(0xffffff),
            pop_bg: hex(0xffffff),
            group_bg: hex(0xffffff),
            row_hover: black(0.04),
            row_pressed: black(0.075),
            sidebar_bg: hex(0xf4f4f6),
            sidebar_selected: black(0.07),
            tag_border: separator_strong,
            danger_button: error_badge,
            surface: hex(0xffffff),
            scale: 2.,
        }
    }

    pub fn dark() -> Theme {
        let separator_strong = white(0.18);
        let error_badge = hex(0xff453a);
        Theme {
            dark: true,
            window_bg: hex(0x1e1e20),
            bar_bg: hex(0x232326),
            raised: hex(0x2a2a2d),
            fill: white(0.075),
            fill_2: hex(0x2e2e31),
            text: hex(0xf2f2f7),
            text_2: hex(0xa1a1a6),
            text_3: hex(0x8e8e93),
            separator: white(0.08),
            separator_strong,
            accent: hex(0x0a84ff),
            accent_ring: hexa(0x0a84ff, 0.42),
            track: white(0.12),
            success: hex(0x30d158),
            success_badge: hex(0x30d158),
            success_tint: hexa(0x30d158, 0.13),
            warning: hex(0xff9f0a),
            warning_badge: hex(0xff9f0a),
            warning_bg: hexa(0xff9f0a, 0.12),
            error: hex(0xff6961),
            error_badge,
            error_bg: hexa(0xff453a, 0.12),
            gray_badge: hex(0x636366),
            button_bg: white(0.13),
            pop_bg: hex(0x2c2c2f),
            group_bg: hex(0x28282b),
            row_hover: white(0.05),
            row_pressed: white(0.09),
            sidebar_bg: hex(0x232326),
            sidebar_selected: white(0.09),
            tag_border: separator_strong,
            danger_button: error_badge,
            surface: hex(0x1e1e20),
            scale: 2.,
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
        }
    }

    pub fn of(window: &Window) -> Theme {
        Theme::for_appearance(window.appearance()).at_scale(window.scale_factor())
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

    /// Secondary button edge: `0 0 0 .5px black 20%` + `0 1px 1.5px black 8%` in
    /// light, `0 0 0 .5px white 6%` in dark.
    pub fn button_shadow(&self) -> Vec<BoxShadow> {
        if self.dark {
            vec![self.edge(white(0.06))]
        } else {
            vec![
                self.edge(black(0.20)),
                BoxShadow {
                    color: black(0.08),
                    offset: point(px(0.), px(1.)),
                    blur_radius: px(1.5),
                    spread_radius: px(0.),
                    inset: false,
                },
            ]
        }
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

    /// The scrim behind a dialog.
    pub fn scrim(&self) -> Hsla {
        black(if self.dark { 0.45 } else { 0.25 })
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

/// Sizes from the design (section 11), in points.
pub mod metrics {
    use super::*;

    pub const TOOLBAR_H: f32 = 52.;
    pub const SUMMARY_H: f32 = 64.;
    pub const STATUS_H: f32 = 30.;
    pub const LIST_INSET: f32 = 8.;
    /// Cleanup group headers and item rows share one height, because `uniform_list` needs a
    /// fixed row height.
    pub const CLEANUP_ROW_H: f32 = 44.;
    pub const LARGE_ROW_H: f32 = 52.;
    pub const BREADCRUMB_H: f32 = 28.;
    pub const SIDEBAR_W: f32 = 200.;
    pub const SIDEBAR_ROW_H: f32 = 28.;
    pub const ACTION: f32 = 26.;
    /// The trailing area of a row that holds the hover actions.
    pub const TRAIL_W: f32 = 60.;
    pub const BAR_H: f32 = 4.;
    pub const MAIN_W: f32 = 900.;
    pub const MAIN_H: f32 = 620.;
    pub const MAIN_MIN_W: f32 = 760.;
    pub const MAIN_MIN_H: f32 = 480.;
    pub const DIALOG_W: f32 = 420.;
    pub const HISTORY_W: f32 = 560.;
    pub const HISTORY_H: f32 = 480.;

    pub fn p(v: f32) -> Pixels {
        px(v)
    }
}

/// The Figure type role: 22 / 28, weight 600, tabular, `text`. The summary number.
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
        assert!((l.separator.a - 0.08).abs() < 1e-6);
        assert!((d.track.a - 0.12).abs() < 1e-6);
        assert!((l.accent_ring.a - 0.32).abs() < 1e-6);
        assert!((d.accent_ring.a - 0.42).abs() < 1e-6);
    }

    #[test]
    fn new_tokens_follow_the_spec() {
        let (l, d) = (Theme::light(), Theme::dark());
        assert_eq!(l.sidebar_bg, hex(0xf4f4f6));
        assert_eq!(d.sidebar_bg, hex(0x232326));
        assert!((l.sidebar_selected.a - 0.07).abs() < 1e-6 && l.sidebar_selected.l == 0.);
        assert!((d.sidebar_selected.a - 0.09).abs() < 1e-6 && d.sidebar_selected.l == 1.);
        assert_eq!(l.tag_border, l.separator_strong);
        assert_eq!(d.tag_border, d.separator_strong);
        assert_eq!(l.danger_button, l.error_badge);
        assert_eq!(d.danger_button, d.error_badge);
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
        assert_eq!(l.button_shadow().len(), 2);
        assert_eq!(d.button_shadow().len(), 1);
        assert_eq!(l.pop_shadow().len(), 2);
        assert!(l.pop_shadow().iter().all(|s| !s.inset));
    }

    #[test]
    fn hairlines_are_half_strength_at_1x() {
        let l2 = Theme::light();
        assert_eq!(l2.scale, 2.);
        let e2 = &l2.button_shadow()[0];
        assert_eq!(e2.spread_radius, px(0.5));
        assert!((e2.color.a - 0.20).abs() < 1e-6);
        let l1 = Theme::light().at_scale(1.);
        let e1 = &l1.button_shadow()[0];
        assert_eq!(e1.spread_radius, px(1.));
        assert!((e1.color.a - 0.10).abs() < 1e-6);
        assert_eq!(l1.button_shadow()[1], l2.button_shadow()[1]);
        let d1 = Theme::dark().at_scale(1.).button_shadow();
        assert!((d1[0].color.a - 0.03).abs() < 1e-6);
    }

    #[test]
    fn button_fill_is_flattened_on_the_surface() {
        let d = Theme::dark();
        let fill = d.button_fill();
        assert!((fill.a - 1.).abs() < 1e-4);
        let rgb = fill.to_rgb();
        assert!((rgb.r * 255. - 59.).abs() < 1.5, "{rgb:?}");
        assert_eq!(Theme::light().button_fill(), Theme::light().button_bg);
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
        for d in [REMOVE, NOTE_IN, NOTE_OUT, SPIN] {
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
