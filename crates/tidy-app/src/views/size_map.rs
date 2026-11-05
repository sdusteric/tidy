//! The size map (design section 20): what a screen lists, drawn as tiles whose area is their
//! size. Cleanup shows its items grouped; Large files shows the open folder in Folders mode.
//! The map repeats the list, it never replaces it: every tile is a row below, and a click on a
//! tile does what the row's checkbox (or chevron) does.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::motion::{self as rules, Zoom};
use crate::theme::{DisplayType, Theme, motion as m, tnum};
use crate::treemap::Frac;

/// Points between two tiles, and the extra inset that sets groups apart.
pub const TILE_GAP: f32 = 2.;
pub const GROUP_INSET: f32 = 2.;
/// The Cleanup map's height. Short enough that seven rows still show at the default size.
pub const CLEANUP_MAP_H: f32 = 120.;
/// The Folders map's height.
pub const FOLDER_MAP_H: f32 = 220.;
/// Points of space below a map, before the list.
pub const MAP_BOTTOM: f32 = 12.;

/// How a tile is drawn. Ink means "will move", as the ticked checkbox does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileLook {
    /// Ticked: moves to the Trash with the next Clean or Move to Trash.
    Ticked,
    /// Can be ticked, is not.
    Open,
    /// Can't be ticked until its app quits.
    Blocked,
    /// A folder that opens on click (Folders mode).
    Folder,
    /// The folded "smaller items": no action.
    Rest,
}

impl TileLook {
    pub fn clickable(self) -> bool {
        matches!(self, TileLook::Ticked | TileLook::Open | TileLook::Folder)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapTile {
    /// The row's key: the Cleanup item id or the Large files `rel`.
    pub key: String,
    pub name: String,
    /// The size, or what stops it ("Quit first").
    pub size: String,
    pub look: TileLook,
    pub frac: Frac,
    /// Which group the tile belongs to, for the staggered entrance.
    pub group: usize,
    /// Fading out because its item reached the Trash.
    pub leaving: bool,
    /// "Review" under the name when there is room.
    pub tag: Option<&'static str>,
}

/// Whether a tile this many points across shows its name, and its size too.
pub fn labels(w: f32, h: f32) -> (bool, bool) {
    let name = w >= 52. && h >= 24.;
    (name, name && h >= 44.)
}

/// Each tile's place before items left, by key: where a reflow slides tiles from.
type Places = Rc<HashMap<String, Frac>>;

/// What the map remembers between frames to start its motions.
#[derive(Default)]
pub(crate) struct MapState {
    /// The result the map showed: the scan time. A change is a new scan.
    result: Option<String>,
    /// How the result is viewed (mode, threshold). A change shows the new layout at once.
    view: Option<String>,
    folder: Option<String>,
    size: (f32, f32),
    last: HashMap<String, Frac>,
    pub(crate) reflow: Option<(Instant, usize, Places)>,
    pub(crate) entrance: Option<(Instant, usize, usize)>,
    pub(crate) zoom: Option<(Instant, usize, Zoom)>,
}

/// One frame's worth of what the map is showing, for `MapState::observe`.
pub(crate) struct Seen<'a> {
    pub result: &'a str,
    pub view: &'a str,
    pub folder: &'a str,
    pub size: (f32, f32),
    pub tiles: &'a [MapTile],
    pub groups: usize,
}

impl MapState {
    /// Compares this frame with the last and starts the motion it calls for. `motion` is false
    /// under Reduce Motion; `generation` keys each new animation.
    pub(crate) fn observe(&mut self, seen: Seen, motion: bool, generation: &mut usize) {
        let now = Instant::now();
        let layout: HashMap<String, Frac> =
            seen.tiles.iter().map(|t| (t.key.clone(), t.frac)).collect();
        let fresh = |slot: &mut usize| {
            *slot += 1;
            *slot
        };
        if let (Some(result), Some(view), Some(folder)) = (&self.result, &self.view, &self.folder)
            && motion
        {
            if result != seen.result {
                self.entrance = Some((now, fresh(generation), seen.groups));
                self.reflow = None;
                self.zoom = None;
            } else if view != seen.view {
                // A new mode or threshold: no motion.
            } else if folder != seen.folder {
                let last = &self.last;
                let zoom = rules::folder_zoom(
                    folder,
                    seen.folder,
                    &|rel| last.get(rel).copied(),
                    &|rel| layout.get(rel).copied(),
                );
                if let Some(zoom) = zoom {
                    self.zoom = Some((now, fresh(generation), zoom));
                    self.reflow = None;
                }
            } else {
                let before: Vec<&str> = self.last.keys().map(String::as_str).collect();
                let after: Vec<&str> = layout.keys().map(String::as_str).collect();
                if rules::reflows(true, self.size == seen.size, &before, &after) {
                    self.reflow = Some((now, fresh(generation), Rc::new(self.last.clone())));
                }
            }
        }
        if !motion {
            self.reflow = None;
            self.entrance = None;
            self.zoom = None;
        }
        self.result = Some(seen.result.to_string());
        self.view = Some(seen.view.to_string());
        self.folder = Some(seen.folder.to_string());
        self.size = seen.size;
        self.last = layout;
    }

    /// The motions still playing at `now`; ended ones are dropped.
    pub(crate) fn live(&mut self) -> Live {
        let playing = |at: Instant, d: std::time::Duration| at.elapsed() < d;
        if self
            .reflow
            .as_ref()
            .is_some_and(|r| !playing(r.0, m::CARD_IN))
        {
            self.reflow = None;
        }
        if self
            .entrance
            .is_some_and(|e| !playing(e.0, rules::entrance_length(e.2)))
        {
            self.entrance = None;
        }
        if self.zoom.is_some_and(|z| !playing(z.0, m::ZOOM)) {
            self.zoom = None;
        }
        Live {
            reflow: self.reflow.as_ref().map(|r| (r.1, r.2.clone())),
            entrance: self.entrance.map(|e| (e.1, e.2)),
            zoom: self.zoom.map(|z| (z.1, z.2)),
        }
    }
}

/// The motions to draw this frame, each with its generation.
#[derive(Clone, Default)]
pub(crate) struct Live {
    pub reflow: Option<(usize, Places)>,
    pub entrance: Option<(usize, usize)>,
    pub zoom: Option<(usize, Zoom)>,
}

type KeyFn = Rc<dyn Fn(&str, &mut Window, &mut App)>;
type HoverFn = Rc<dyn Fn(&str, bool, &mut Window, &mut App)>;

pub(crate) struct MapProps {
    pub id: &'static str,
    pub tiles: Vec<MapTile>,
    /// The map's size in points.
    pub w: f32,
    pub h: f32,
    pub hovered: Option<String>,
    pub live: Live,
    pub reduce_motion: bool,
    pub on_click: KeyFn,
    pub on_hover: HoverFn,
}

/// Draws the map. Tiles are placed in fractions of the map, so the zoom only has to move the
/// frame that holds them.
pub(crate) fn render_map(p: MapProps, t: &Theme) -> AnyElement {
    let MapProps {
        id,
        tiles,
        w,
        h,
        hovered,
        live,
        reduce_motion,
        on_click,
        on_hover,
    } = p;
    let children: Vec<AnyElement> = tiles
        .iter()
        .filter(|tile| tile.frac.area() > 0.)
        .map(|tile| {
            let is_hovered = hovered.as_deref() == Some(tile.key.as_str());
            let face = tile_face(tile, w, h, is_hovered, reduce_motion, t);
            let key = tile.key.clone();
            let click = on_click.clone();
            let hover = on_hover.clone();
            let clickable = tile.look.clickable();
            let wrap = div()
                .id(SharedString::from(format!("{id}-tile-{}", tile.key)))
                .debug_selector({
                    let sel = format!("{id}-tile:{}", tile.key);
                    move || sel
                })
                .absolute()
                .p(px(TILE_GAP / 2.))
                .on_hover({
                    let key = key.clone();
                    move |h, window, cx| hover(&key, *h, window, cx)
                })
                .when(clickable, |d| {
                    d.cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| click(&key, window, cx))
                })
                .child(face);
            place(wrap, tile, &live, id)
        })
        .collect();

    let frame = div().absolute().size_full().children(children);
    let frame: AnyElement = match live.zoom {
        Some((generation, zoom)) => {
            let start = rules::zoom_frame(zoom, 0.);
            set_frac(div().absolute(), start)
                .child(frame)
                .with_animation(
                    (SharedString::from(format!("{id}-zoom")), generation),
                    Animation::new(m::ZOOM).with_easing(m::curve(m::enter())),
                    move |el, d| set_frac(el, rules::zoom_frame(zoom, d)),
                )
                .into_any_element()
        }
        None => frame.into_any_element(),
    };
    div()
        .debug_selector(move || id.to_string())
        .relative()
        .flex_none()
        .w(px(w))
        .h(px(h))
        .overflow_hidden()
        .child(frame)
        .into_any_element()
}

fn set_frac<E: Styled>(el: E, f: Frac) -> E {
    el.left(relative(f.x))
        .top(relative(f.y))
        .w(relative(f.w))
        .h(relative(f.h))
}

/// Puts a tile at its place, sliding it there when the map is reflowing and fading it in
/// when the map is entering.
fn place(wrap: Stateful<Div>, tile: &MapTile, live: &Live, id: &'static str) -> AnyElement {
    let to = tile.frac;
    if let Some((generation, from)) = &live.reflow
        && let Some(from) = from.get(&tile.key).copied()
        && from != to
    {
        return set_frac(wrap, from)
            .with_animation(
                (
                    SharedString::from(format!("{id}-slide-{}", tile.key)),
                    *generation,
                ),
                Animation::new(m::CARD_IN).with_easing(m::curve(m::enter())),
                move |el, d| set_frac(el, Frac::lerp(from, to, d)),
            )
            .into_any_element();
    }
    if let Some((generation, groups)) = live.entrance {
        let group = tile.group;
        return set_frac(wrap, to)
            .opacity(0.)
            .with_animation(
                (
                    SharedString::from(format!("{id}-in-{}", tile.key)),
                    generation,
                ),
                Animation::new(rules::entrance_length(groups)),
                move |el, d| el.opacity(m::enter().sample(rules::staggered(d, group, groups))),
            )
            .into_any_element();
    }
    set_frac(wrap, to).into_any_element()
}

/// The tile itself: fill, outline, name and size.
fn tile_face(
    tile: &MapTile,
    map_w: f32,
    map_h: f32,
    hovered: bool,
    reduce_motion: bool,
    t: &Theme,
) -> AnyElement {
    let (w, h) = (
        tile.frac.w * map_w - TILE_GAP,
        tile.frac.h * map_h - TILE_GAP,
    );
    let (show_name, show_size) = labels(w, h);
    let small = w < 24. || h < 24.;
    let (bg, ink, ink_2) = match tile.look {
        TileLook::Ticked => (Some(t.accent), t.on_accent, t.on_accent.opacity(0.72)),
        TileLook::Open | TileLook::Folder | TileLook::Blocked => (Some(t.fill), t.text, t.text_2),
        TileLook::Rest => (None, t.text_2, t.text_2),
    };
    let size_color = if tile.look == TileLook::Blocked {
        t.warning
    } else {
        ink_2
    };
    let face = div()
        .size_full()
        .relative()
        .overflow_hidden()
        .rounded(px(if small { 3. } else { 6. }))
        .when_some(bg, |d, bg| d.bg(bg))
        .map(|d| match (tile.look, hovered) {
            // Hover draws a 2 pt ring in the tile's own ink, so it shows on ink tiles too.
            (TileLook::Ticked, true) => d.border_2().border_color(t.on_accent.opacity(0.7)),
            (_, true) => d.border_2().border_color(t.text),
            (TileLook::Blocked, false) => d.border(px(1.5)).border_color(t.warning),
            (TileLook::Rest, false) => d.border_1().border_color(t.separator),
            _ => d,
        })
        .px(px(8.))
        .py(px(6.))
        .flex()
        .flex_col()
        .justify_between()
        .when(show_name, |d| {
            d.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .line_height(px(15.))
                                    .display(FontWeight::SEMIBOLD)
                                    .text_color(ink)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(tile.name.clone()),
                            )
                            .when(tile.look == TileLook::Folder && w >= 72., |d| {
                                d.child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(px(12.))
                                        .text_color(t.text_2),
                                )
                            }),
                    )
                    .when_some(tile.tag.filter(|_| h >= 60.), |d, tag| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .line_height(px(14.))
                                .text_color(ink_2)
                                .child(tag),
                        )
                    }),
            )
        })
        .when(show_size, |d| {
            d.child(
                div()
                    .text_size(px(12.))
                    .line_height(px(15.))
                    .display(FontWeight::SEMIBOLD)
                    .font_features(tnum())
                    .text_color(size_color)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(tile.size.clone()),
            )
        });
    if tile.leaving && !reduce_motion {
        // As the row does: the tile fades over 140 ms while its item reaches the Trash.
        face.with_animation(
            SharedString::from(format!("tile-out-{}", tile.key)),
            Animation::new(m::REMOVE).with_easing(m::curve(m::exit())),
            |el, d| el.opacity(1. - d),
        )
        .into_any_element()
    } else {
        face.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn tile(key: &str, frac: Frac) -> MapTile {
        MapTile {
            key: key.to_string(),
            name: key.to_string(),
            size: "1 GB".to_string(),
            look: TileLook::Open,
            frac,
            group: 0,
            leaving: false,
            tag: None,
        }
    }

    const L: Frac = Frac {
        x: 0.,
        y: 0.,
        w: 0.5,
        h: 1.,
    };
    const R: Frac = Frac {
        x: 0.5,
        y: 0.,
        w: 0.5,
        h: 1.,
    };

    fn seen<'a>(result: &'a str, folder: &'a str, tiles: &'a [MapTile]) -> Seen<'a> {
        Seen {
            result,
            view: "v",
            folder,
            size: (600., 120.),
            tiles,
            groups: 2,
        }
    }

    #[test]
    fn small_tiles_drop_their_labels() {
        assert_eq!(labels(120., 60.), (true, true));
        assert_eq!(labels(120., 30.), (true, false));
        assert_eq!(labels(40., 60.), (false, false));
    }

    #[test]
    fn only_tiles_with_an_action_take_clicks() {
        assert!(TileLook::Ticked.clickable());
        assert!(TileLook::Open.clickable());
        assert!(TileLook::Folder.clickable());
        assert!(!TileLook::Blocked.clickable());
        assert!(!TileLook::Rest.clickable());
    }

    #[test]
    fn the_first_frame_shows_still() {
        let mut s = MapState::default();
        let mut generation = 0;
        s.observe(seen("1", "", &[tile("a", L)]), true, &mut generation);
        let live = s.live();
        assert!(live.reflow.is_none() && live.entrance.is_none() && live.zoom.is_none());
    }

    #[test]
    fn a_new_scan_staggers_the_tiles_in() {
        let mut s = MapState::default();
        let mut generation = 0;
        s.observe(seen("none", "", &[]), true, &mut generation);
        s.observe(seen("1", "", &[tile("a", L)]), true, &mut generation);
        assert_eq!(s.live().entrance.map(|e| e.1), Some(2));
    }

    #[test]
    fn a_tile_leaving_slides_the_rest_into_the_gap() {
        let mut s = MapState::default();
        let mut generation = 0;
        s.observe(
            seen("1", "", &[tile("a", L), tile("b", R)]),
            true,
            &mut generation,
        );
        s.observe(
            seen("1", "", &[tile("b", Frac::FULL)]),
            true,
            &mut generation,
        );
        let (_, from) = s.live().reflow.expect("reflow");
        assert_eq!(from.get("b"), Some(&R));
    }

    #[test]
    fn opening_a_folder_tile_zooms_in_through_it() {
        let mut s = MapState::default();
        let mut generation = 0;
        s.observe(
            seen("1", "", &[tile("Movies", L), tile("Music", R)]),
            true,
            &mut generation,
        );
        s.observe(
            seen("1", "Movies", &[tile("Movies/2025", Frac::FULL)]),
            true,
            &mut generation,
        );
        assert_eq!(s.live().zoom.map(|z| z.1), Some(Zoom::In(L)));
    }

    #[test]
    fn reduce_motion_starts_nothing() {
        let mut s = MapState::default();
        let mut generation = 0;
        s.observe(
            seen("1", "", &[tile("a", L), tile("b", R)]),
            false,
            &mut generation,
        );
        s.observe(
            seen("1", "", &[tile("b", Frac::FULL)]),
            false,
            &mut generation,
        );
        s.observe(
            seen("2", "", &[tile("c", Frac::FULL)]),
            false,
            &mut generation,
        );
        let live = s.live();
        assert!(live.reflow.is_none() && live.entrance.is_none() && live.zoom.is_none());
    }
}
