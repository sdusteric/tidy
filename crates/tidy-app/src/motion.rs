//! Pure decisions behind the motions (design section 11): when a motion may play. No GPUI
//! state here, so every rule is unit-tested. Section 20 adds the size map's motions: tiles
//! sliding into a gap, the folder zoom, the staggered entrance and the disk bar.

use std::time::Duration;

use crate::theme::motion::{CARD_IN, REMOVE, STAGGER};
use crate::treemap::Frac;

/// How long a trashed row fades before it leaves the list. None means it leaves at once
/// (Reduce Motion).
pub fn remove_delay(reduce_motion: bool) -> Option<Duration> {
    (!reduce_motion).then_some(REMOVE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn rows_fade_for_140_ms_unless_motion_is_reduced() {
        assert_eq!(remove_delay(true), None);
        assert_eq!(remove_delay(false), Some(Duration::from_millis(140)));
    }
}

/// Which way the size map zooms when the open folder changes from `prev` to `next` (both
/// `/`-separated, empty for the home folder), and the tile it zooms through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// Into a child: the tile at this place in the old map grows to fill the map.
    In(Frac),
    /// Back up: the child we left shrinks into this place in the new map.
    Out(Frac),
}

/// `prev_tiles` and `next_tiles` give each tile's place by its `rel`. None for a change that is
/// not a step down or up (another scan, a jump across), or when the tile is not on the map.
pub fn folder_zoom(
    prev: &str,
    next: &str,
    prev_tiles: &dyn Fn(&str) -> Option<Frac>,
    next_tiles: &dyn Fn(&str) -> Option<Frac>,
) -> Option<Zoom> {
    let below = |top: &str, deep: &str| -> Option<String> {
        let rest = if top.is_empty() {
            deep
        } else {
            deep.strip_prefix(top)?.strip_prefix('/')?
        };
        let first = rest.split('/').next().filter(|s| !s.is_empty())?;
        Some(if top.is_empty() {
            first.to_string()
        } else {
            format!("{top}/{first}")
        })
    };
    if prev == next {
        return None;
    }
    if let Some(child) = below(prev, next) {
        return prev_tiles(&child).map(Zoom::In);
    }
    if let Some(child) = below(next, prev) {
        return next_tiles(&child).map(Zoom::Out);
    }
    None
}

/// Where the map's contents sit `t` (0 to 1, eased) into a zoom, as fractions of the map.
pub fn zoom_frame(zoom: Zoom, t: f32) -> Frac {
    match zoom {
        Zoom::In(tile) => Frac::lerp(tile, Frac::FULL, t),
        Zoom::Out(tile) => Frac::lerp(tile.zoomed_out_from(), Frac::FULL, t),
    }
}

/// How far one group's fade is when the whole staggered entrance is `t` (0 to 1) through: group
/// `index` waits `index` times STAGGER, then fades over CARD_IN.
pub fn staggered(t: f32, index: usize, groups: usize) -> f32 {
    let total = entrance_length(groups).as_secs_f32();
    let start = (STAGGER * index as u32).as_secs_f32();
    ((t * total - start) / CARD_IN.as_secs_f32()).clamp(0., 1.)
}

/// The whole staggered entrance for this many groups.
pub fn entrance_length(groups: usize) -> Duration {
    CARD_IN + STAGGER * groups.saturating_sub(1) as u32
}

/// Whether the map's tiles should slide to their new places: only when the same result lost
/// tiles (items reached the Trash) and the map kept its size. A new scan, a resize or a new
/// folder shows its layout at once.
pub fn reflows(same_result: bool, same_size: bool, before: &[&str], after: &[&str]) -> bool {
    same_result
        && same_size
        && after.len() < before.len()
        && after.iter().all(|k| before.contains(k))
}

#[cfg(test)]
mod map_tests {
    use super::*;
    use core::prelude::v1::test;

    const A: Frac = Frac {
        x: 0.5,
        y: 0.,
        w: 0.5,
        h: 0.5,
    };

    #[test]
    fn stepping_into_a_child_zooms_in_through_its_tile() {
        let prev = |rel: &str| (rel == "Movies").then_some(A);
        let none = |_: &str| None;
        assert_eq!(folder_zoom("", "Movies", &prev, &none), Some(Zoom::In(A)));
        // Two levels at once still zooms through the first one.
        assert_eq!(
            folder_zoom("", "Movies/2025", &prev, &none),
            Some(Zoom::In(A))
        );
    }

    #[test]
    fn stepping_back_up_zooms_out_into_the_childs_place() {
        let next = |rel: &str| (rel == "Movies").then_some(A);
        let none = |_: &str| None;
        assert_eq!(
            folder_zoom("Movies/2025", "", &none, &next),
            Some(Zoom::Out(A))
        );
        assert_eq!(folder_zoom("Movies", "", &none, &next), Some(Zoom::Out(A)));
    }

    #[test]
    fn other_changes_do_not_zoom() {
        let all = |_: &str| Some(A);
        assert_eq!(folder_zoom("Movies", "Movies", &all, &all), None);
        assert_eq!(folder_zoom("Movies", "Music", &all, &all), None);
        // A name that only starts like the folder is not inside it.
        assert_eq!(folder_zoom("Movies", "MoviesOld", &all, &all), None);
        let none = |_: &str| None;
        assert_eq!(folder_zoom("", "Movies", &none, &none), None);
    }

    #[test]
    fn a_zoom_ends_on_the_full_map() {
        assert_eq!(zoom_frame(Zoom::In(A), 0.), A);
        assert_eq!(zoom_frame(Zoom::In(A), 1.), Frac::FULL);
        assert_eq!(zoom_frame(Zoom::Out(A), 1.), Frac::FULL);
        assert_eq!(zoom_frame(Zoom::Out(A), 0.), A.zoomed_out_from());
    }

    #[test]
    fn groups_fade_in_forty_ms_apart() {
        assert_eq!(entrance_length(1), Duration::from_millis(200));
        assert_eq!(entrance_length(6), Duration::from_millis(400));
        // At the start nothing shows, at the end everything does.
        assert_eq!(staggered(0., 0, 6), 0.);
        assert_eq!(staggered(1., 5, 6), 1.);
        // 40 ms in, the first group is a fifth of the way and the second has not started.
        let t = 0.04 / 0.4;
        assert!((staggered(t, 0, 6) - 0.2).abs() < 1e-4);
        assert_eq!(staggered(t, 1, 6), 0.);
    }

    #[test]
    fn tiles_slide_only_when_items_left_the_same_result() {
        assert!(reflows(true, true, &["a", "b", "c"], &["a", "c"]));
        assert!(!reflows(false, true, &["a", "b"], &["a"]));
        assert!(!reflows(true, false, &["a", "b"], &["a"]));
        assert!(!reflows(true, true, &["a", "b"], &["a", "b"]));
        assert!(!reflows(true, true, &["a", "b"], &["c"]));
    }
}
