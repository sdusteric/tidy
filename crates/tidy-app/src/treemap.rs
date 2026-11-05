//! The size map's layout (design section 20): a squarified treemap, as fractions of the map so
//! a tile keeps its place when the window is resized. Pure math, no GPUI state.

/// A rectangle as fractions of the map: 0 to 1 on each axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frac {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Frac {
    pub const FULL: Frac = Frac {
        x: 0.,
        y: 0.,
        w: 1.,
        h: 1.,
    };

    pub fn lerp(a: Frac, b: Frac, t: f32) -> Frac {
        let mix = |p: f32, q: f32| p + (q - p) * t;
        Frac {
            x: mix(a.x, b.x),
            y: mix(a.y, b.y),
            w: mix(a.w, b.w),
            h: mix(a.h, b.h),
        }
    }

    /// Where the parent map sits, as fractions of the map, when this child tile fills the map:
    /// the start of a zoom out.
    pub fn zoomed_out_from(self) -> Frac {
        let (w, h) = (self.w.max(f32::EPSILON), self.h.max(f32::EPSILON));
        Frac {
            x: -self.x / w,
            y: -self.y / h,
            w: 1. / w,
            h: 1. / h,
        }
    }

    pub fn area(self) -> f32 {
        self.w * self.h
    }
}

/// Lays `sizes` out in a `w` by `h` box with the squarified algorithm (Bruls, Huizing and van
/// Wijk), largest first in rows along the short side. The result is in input order, as
/// fractions of the box. A size of zero or less gets an empty rectangle.
pub fn squarify(sizes: &[f64], w: f32, h: f32) -> Vec<Frac> {
    let mut out = vec![
        Frac {
            x: 0.,
            y: 0.,
            w: 0.,
            h: 0.
        };
        sizes.len()
    ];
    let total: f64 = sizes.iter().filter(|s| **s > 0.).sum();
    if total <= 0. || w <= 0. || h <= 0. {
        return out;
    }
    let scale = f64::from(w) * f64::from(h) / total;
    let mut order: Vec<(usize, f64)> = sizes
        .iter()
        .enumerate()
        .filter(|(_, s)| **s > 0.)
        .map(|(i, s)| (i, s * scale))
        .collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

    // The free rectangle, in points.
    let (mut fx, mut fy, mut fw, mut fh) = (0f64, 0f64, f64::from(w), f64::from(h));
    let mut row: Vec<(usize, f64)> = Vec::new();
    let worst = |row: &[(usize, f64)], side: f64| -> f64 {
        let sum: f64 = row.iter().map(|r| r.1).sum();
        let max = row.iter().map(|r| r.1).fold(f64::MIN, f64::max);
        let min = row.iter().map(|r| r.1).fold(f64::MAX, f64::min);
        (side * side * max / (sum * sum)).max(sum * sum / (side * side * min))
    };
    let mut place =
        |row: &mut Vec<(usize, f64)>, fx: &mut f64, fy: &mut f64, fw: &mut f64, fh: &mut f64| {
            let sum: f64 = row.iter().map(|r| r.1).sum();
            if *fw >= *fh {
                // A column on the left of the free rectangle.
                let cw = if *fh > 0. { sum / *fh } else { 0. };
                let mut cy = *fy;
                for (i, a) in row.drain(..) {
                    let ch = if cw > 0. { a / cw } else { 0. };
                    out[i] = frac(*fx, cy, cw, ch, w, h);
                    cy += ch;
                }
                *fx += cw;
                *fw -= cw;
            } else {
                // A row along the top of the free rectangle.
                let ch = if *fw > 0. { sum / *fw } else { 0. };
                let mut cx = *fx;
                for (i, a) in row.drain(..) {
                    let cw = if ch > 0. { a / ch } else { 0. };
                    out[i] = frac(cx, *fy, cw, ch, w, h);
                    cx += cw;
                }
                *fy += ch;
                *fh -= ch;
            }
        };
    for item in order {
        let side = fw.min(fh);
        if row.is_empty() || {
            let mut next = row.clone();
            next.push(item);
            worst(&next, side) <= worst(&row, side)
        } {
            row.push(item);
        } else {
            place(&mut row, &mut fx, &mut fy, &mut fw, &mut fh);
            row.push(item);
        }
    }
    if !row.is_empty() {
        place(&mut row, &mut fx, &mut fy, &mut fw, &mut fh);
    }
    out
}

fn frac(x: f64, y: f64, w: f64, h: f64, bw: f32, bh: f32) -> Frac {
    let (bw, bh) = (f64::from(bw), f64::from(bh));
    Frac {
        x: (x / bw) as f32,
        y: (y / bh) as f32,
        w: (w / bw) as f32,
        h: (h / bh) as f32,
    }
}

/// Two levels: each group is laid out in the map, then its items inside the group's
/// rectangle shrunk by `inset` points on every side, so groups read as blocks. The result is
/// per group, per item, as fractions of the whole map.
pub fn nested(groups: &[Vec<f64>], w: f32, h: f32, inset: f32) -> Vec<Vec<Frac>> {
    let totals: Vec<f64> = groups
        .iter()
        .map(|g| g.iter().filter(|s| **s > 0.).sum())
        .collect();
    let outer = squarify(&totals, w, h);
    groups
        .iter()
        .zip(outer)
        .map(|(items, g)| {
            // The group's rectangle in points, shrunk by the inset where it has room.
            let (gx, gy, gw, gh) = (g.x * w, g.y * h, g.w * w, g.h * h);
            let ix = inset.min(gw / 4.);
            let iy = inset.min(gh / 4.);
            let (gx, gy, gw, gh) = (gx + ix, gy + iy, gw - 2. * ix, gh - 2. * iy);
            squarify(items, gw, gh)
                .into_iter()
                .map(|f| Frac {
                    x: (gx + f.x * gw) / w,
                    y: (gy + f.y * gh) / h,
                    w: f.w * gw / w,
                    h: f.h * gh / h,
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn overlaps(a: Frac, b: Frac) -> bool {
        let e = 1e-4;
        a.x + a.w > b.x + e && b.x + b.w > a.x + e && a.y + a.h > b.y + e && b.y + b.h > a.y + e
    }

    #[test]
    fn areas_follow_sizes_and_fill_the_map() {
        let sizes = [8.2, 4.7, 2.0, 1.8, 1.1, 0.75];
        let tiles = squarify(&sizes, 660., 128.);
        let total: f64 = sizes.iter().sum();
        let mut sum = 0.;
        for (s, f) in sizes.iter().zip(&tiles) {
            assert!(close(f.area(), (*s / total) as f32), "{s}: {f:?}");
            assert!(f.x >= -1e-4 && f.y >= -1e-4);
            assert!(f.x + f.w <= 1. + 1e-4 && f.y + f.h <= 1. + 1e-4);
            sum += f.area();
        }
        assert!(close(sum, 1.));
    }

    #[test]
    fn tiles_never_overlap() {
        let sizes = [21.6, 12.4, 8.9, 3.3, 2.3, 2.1, 1.8];
        let tiles = squarify(&sizes, 660., 220.);
        for i in 0..tiles.len() {
            for j in i + 1..tiles.len() {
                assert!(!overlaps(tiles[i], tiles[j]), "{i} and {j}");
            }
        }
    }

    #[test]
    fn results_keep_the_input_order() {
        let tiles = squarify(&[1., 9.], 100., 100.);
        assert!(tiles[1].area() > tiles[0].area());
    }

    #[test]
    fn tiles_stay_close_to_square() {
        // The point of squarifying: no sliver much longer than wide when sizes are similar.
        let tiles = squarify(&[6., 6., 4., 3., 2., 2., 1.], 600., 400.);
        for f in tiles {
            let (w, h) = (f.w * 600., f.h * 400.);
            assert!(w.max(h) / w.min(h) < 3., "{f:?}");
        }
    }

    #[test]
    fn empty_and_zero_sizes_get_empty_tiles() {
        assert!(squarify(&[], 100., 100.).is_empty());
        let tiles = squarify(&[0., 5., -1.], 100., 100.);
        assert_eq!(tiles[0].area(), 0.);
        assert_eq!(tiles[2].area(), 0.);
        assert!(close(tiles[1].area(), 1.));
        assert!(
            squarify(&[0., 0.], 100., 100.)
                .iter()
                .all(|f| f.area() == 0.)
        );
        assert!(squarify(&[1.], 0., 100.).iter().all(|f| f.area() == 0.));
    }

    #[test]
    fn nested_items_stay_inside_their_group_with_the_inset() {
        let groups = vec![vec![8.2, 4.7], vec![2.0, 1.8, 1.1], vec![0.2, 0.04]];
        let (w, h) = (660., 128.);
        let tiles = nested(&groups, w, h, 2.);
        let outer = squarify(&[12.9, 4.9, 0.24], w, h);
        for (g, items) in outer.iter().zip(&tiles) {
            for f in items {
                assert!(f.x * w >= g.x * w + 0.5 - 1e-3, "{f:?} in {g:?}");
                assert!((f.x + f.w) * w <= (g.x + g.w) * w + 1e-3);
                assert!(f.y * h >= g.y * h - 1e-3);
                assert!((f.y + f.h) * h <= (g.y + g.h) * h + 1e-3);
            }
        }
        // No two items of any group overlap.
        let all: Vec<Frac> = tiles.into_iter().flatten().collect();
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                assert!(!overlaps(all[i], all[j]));
            }
        }
    }

    #[test]
    fn zooming_out_puts_the_child_back_over_the_whole_map() {
        let child = Frac {
            x: 0.5,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        let parent = child.zoomed_out_from();
        // The child's rectangle inside the parent, mapped through the parent's position,
        // lands on the full map.
        assert!(close(parent.x + child.x * parent.w, 0.));
        assert!(close(parent.y + child.y * parent.h, 0.));
        assert!(close(child.w * parent.w, 1.));
        assert!(close(child.h * parent.h, 1.));
        assert_eq!(Frac::lerp(parent, Frac::FULL, 1.), Frac::FULL);
    }
}
