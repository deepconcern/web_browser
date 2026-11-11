//! Border geometry (CSS Backgrounds 3 §4–5) in device px.
//!
//! A border is the ring between the border edge (outer rounded rectangle)
//! and the padding edge (inner rounded rectangle, radii reduced by the
//! widths, §5.2). When all sides look the same the ring is filled at once.
//! Otherwise each side is its own region: the ring cut by the lines from
//! each outer corner to the matching inner (padding box) corner, so that
//! the color changes at an angle proportional to the two widths (§5.4,
//! "Corner Shaping"), and by a line that splits the box between opposite
//! sides in proportion to their widths (so sides never overlap even when
//! the corner lines do not meet).
//!
//! `dashed` and `dotted` sides paint their corner pieces solid and the
//! straight part between the corners as dashes (twice the width long, gaps
//! one width) or round dots (one width across, centers two widths apart),
//! stretched so the straight part starts and ends with a dash or dot.
//! `double` (at least 3 device px wide) is two rings a third of the width
//! each. `groove`, `ridge`, `inset` and `outset` paint as `solid` in M1.

use crate::geom::Color;
use crate::list::{BorderSide, BorderStyle};
use crate::shape::{
    DRRect, DRect, Poly, Pt, clip_halfplane, halfplane_through, has_area, positive, snap,
    snap_border_width,
};

/// Polygons to fill together with the even-odd rule, in one color.
#[derive(Debug)]
pub(crate) struct Fill {
    pub polys: Vec<Poly>,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Solid,
    Double,
    Dashed,
    Dotted,
}

/// A side after snapping: device width, color, painted style.
#[derive(Clone, Copy, Debug)]
struct Side {
    w: f32,
    color: Color,
    kind: Kind,
}

fn resolve(side: &BorderSide, scale: f32) -> Side {
    let w = if matches!(side.style, BorderStyle::None | BorderStyle::Hidden) {
        0.0
    } else {
        snap_border_width(side.width * scale)
    };
    let kind = match side.style {
        BorderStyle::Double if w >= 3.0 => Kind::Double,
        BorderStyle::Dashed => Kind::Dashed,
        BorderStyle::Dotted => Kind::Dotted,
        _ => Kind::Solid,
    };
    Side {
        w,
        color: side.color,
        kind,
    }
}

/// Builds the fills for a border whose border box is `outer` (snapped,
/// device px, radii scaled). `sides` are top, right, bottom, left in CSS
/// px. `clip` bounds the dashes and dots that are generated.
pub(crate) fn border_fills(
    outer: &DRRect,
    sides: [&BorderSide; 4],
    scale: f32,
    clip: &DRect,
) -> Vec<Fill> {
    let s = sides.map(|b| resolve(b, scale));
    let [t, r, b, l] = s;
    let inner = outer.inset(t.w, r.w, b.w, l.w);
    let mut fills = Vec::new();

    // Fast path: every side with a width shares color and (solid/double)
    // style.
    let mut visible = s.iter().filter(|x| x.w > 0.0);
    if let Some(first) = visible.next() {
        let uniform = matches!(first.kind, Kind::Solid | Kind::Double)
            && visible.all(|x| x.color == first.color && x.kind == first.kind);
        if uniform {
            if first.color.a > 0 {
                let polys = match first.kind {
                    Kind::Double => {
                        let (m1, m2) = double_mids(outer, &s);
                        let mut v = ring(outer, &m1, &[]);
                        v.extend(ring(&m2, &inner, &[]));
                        v
                    }
                    _ => ring(outer, &inner, &[]),
                };
                fills.push(Fill {
                    polys,
                    color: first.color,
                });
            }
            return fills;
        }
    } else {
        return fills;
    }

    let o = outer.rect;
    let i = inner.rect;
    // Outer and inner corners: top-left, top-right, bottom-right,
    // bottom-left.
    let oc: [Pt; 4] = [[o.l, o.t], [o.r, o.t], [o.r, o.b], [o.l, o.b]];
    let ic: [Pt; 4] = [[i.l, i.t], [i.r, i.t], [i.r, i.b], [i.l, i.b]];
    let (w, h) = (o.width(), o.height());
    let split = |a: f32, bb: f32, len: f32| {
        if a + bb > 0.0 {
            len * a / (a + bb)
        } else {
            len
        }
    };
    let rad = outer.radii;

    for (idx, side) in s.iter().enumerate() {
        if side.w <= 0.0 || side.color.a == 0 {
            continue;
        }
        // Corner indices at the start and end of this side (clockwise),
        // a point inside the side, and the split half-plane.
        let (c0, c1, inside, split_plane) = match idx {
            0 => (
                0,
                1,
                [(o.l + o.r) / 2.0, o.t + side.w / 2.0],
                (0.0, -1.0, o.t + split(t.w, b.w, h)),
            ),
            1 => (
                1,
                2,
                [o.r - side.w / 2.0, (o.t + o.b) / 2.0],
                (1.0, 0.0, -(o.r - split(r.w, l.w, w))),
            ),
            2 => (
                2,
                3,
                [(o.l + o.r) / 2.0, o.b - side.w / 2.0],
                (0.0, 1.0, -(o.b - split(b.w, t.w, h))),
            ),
            _ => (
                3,
                0,
                [o.l + side.w / 2.0, (o.t + o.b) / 2.0],
                (-1.0, 0.0, o.l + split(l.w, r.w, w)),
            ),
        };
        let mut planes = vec![split_plane];
        for c in [c0, c1] {
            if let (Some(a), Some(bp)) = (oc.get(c), ic.get(c))
                && let Some(p) = halfplane_through(*a, *bp, inside)
            {
                planes.push(p);
            }
        }
        let solid_ring = |planes: &[(f32, f32, f32)]| match side.kind {
            Kind::Double => {
                let (m1, m2) = double_mids(outer, &s);
                let mut v = ring(outer, &m1, planes);
                v.extend(ring(&m2, &inner, planes));
                v
            }
            _ => ring(outer, &inner, planes),
        };
        match side.kind {
            Kind::Solid | Kind::Double => fills.push(Fill {
                polys: solid_ring(&planes),
                color: side.color,
            }),
            Kind::Dashed | Kind::Dotted => {
                // The straight part along the side's axis, between the
                // corner curves and the adjacent borders.
                let (s0, s1, band0, band1, horizontal) = match idx {
                    0 => (
                        o.l + rad[0][0].max(l.w),
                        o.r - rad[1][0].max(r.w),
                        o.t,
                        o.t + side.w,
                        true,
                    ),
                    1 => (
                        o.t + rad[1][1].max(t.w),
                        o.b - rad[2][1].max(b.w),
                        o.r - side.w,
                        o.r,
                        false,
                    ),
                    2 => (
                        o.l + rad[3][0].max(l.w),
                        o.r - rad[2][0].max(r.w),
                        o.b - side.w,
                        o.b,
                        true,
                    ),
                    _ => (
                        o.t + rad[0][1].max(t.w),
                        o.b - rad[3][1].max(b.w),
                        o.l,
                        o.l + side.w,
                        false,
                    ),
                };
                if s1 <= s0 {
                    fills.push(Fill {
                        polys: solid_ring(&planes),
                        color: side.color,
                    });
                    continue;
                }
                // Solid corner pieces outside [s0, s1].
                let (lo, hi) = if horizontal {
                    ((-1.0, 0.0, s0), (1.0, 0.0, -s1))
                } else {
                    ((0.0, -1.0, s0), (0.0, 1.0, -s1))
                };
                let mut polys = Vec::new();
                for extra in [lo, hi] {
                    let mut p = planes.clone();
                    p.push(extra);
                    polys.extend(ring(outer, &inner, &p));
                }
                fills.push(Fill {
                    polys,
                    color: side.color,
                });
                let (clip0, clip1) = if horizontal {
                    (clip.l, clip.r)
                } else {
                    (clip.t, clip.b)
                };
                let mk = |a0: f32, a1: f32, round: bool| {
                    let rect = if horizontal {
                        DRect {
                            l: a0,
                            t: band0,
                            r: a1,
                            b: band1,
                        }
                    } else {
                        DRect {
                            l: band0,
                            t: a0,
                            r: band1,
                            b: a1,
                        }
                    };
                    if round {
                        let rr = side.w / 2.0;
                        DRRect {
                            rect,
                            radii: [[rr, rr]; 4],
                        }
                        .poly()
                    } else {
                        rect.poly()
                    }
                };
                let pieces = match side.kind {
                    Kind::Dashed => dashes(s0, s1, 2.0 * side.w, side.w, clip0, clip1)
                        .into_iter()
                        .map(|(a0, a1)| mk(a0, a1, false))
                        .collect::<Vec<_>>(),
                    _ => dots(s0, s1, side.w, clip0, clip1)
                        .into_iter()
                        .map(|(a0, a1)| mk(a0, a1, true))
                        .collect(),
                };
                // Each dash is its own fill so even-odd never cancels
                // touching pieces.
                for p in pieces {
                    fills.push(Fill {
                        polys: vec![p],
                        color: side.color,
                    });
                }
            }
        }
    }
    fills
}

/// The two middle edges of a `double` border: a third of each width in
/// from the outside, and a third in from the inside.
fn double_mids(outer: &DRRect, s: &[Side; 4]) -> (DRRect, DRRect) {
    let third = |x: &Side| {
        if x.kind == Kind::Double {
            snap(x.w / 3.0).max(1.0)
        } else {
            x.w / 2.0
        }
    };
    let [t, r, b, l] = s;
    let m1 = outer.inset(third(t), third(r), third(b), third(l));
    let m2 = outer.inset(
        t.w - third(t),
        r.w - third(r),
        b.w - third(b),
        l.w - third(l),
    );
    (m1, m2)
}

/// The ring between two nested rounded rectangles, cut by half-planes.
fn ring(outer: &DRRect, inner: &DRRect, planes: &[(f32, f32, f32)]) -> Vec<Poly> {
    let cut = |mut p: Poly| {
        for (a, b, c) in planes {
            p = clip_halfplane(&p, *a, *b, *c);
        }
        p
    };
    let o = cut(outer.poly());
    if !has_area(&o) {
        return Vec::new();
    }
    let i = cut(inner.poly());
    if has_area(&i) { vec![o, i] } else { vec![o] }
}

/// Dash spans along `[s0, s1]` that reach `[clip0, clip1]`.
fn dashes(s0: f32, s1: f32, dash: f32, gap: f32, clip0: f32, clip1: f32) -> Vec<(f32, f32)> {
    let len = s1 - s0;
    if !positive(dash) || !positive(len) {
        return Vec::new();
    }
    let n = ((len + gap) / (dash + gap)).round().max(1.0);
    let k = len / (n * dash + (n - 1.0) * gap);
    let (d, g) = (dash * k, gap * k);
    spans(s0, n, d + g, d, clip0, clip1)
}

/// Dot spans (diameter `w`) along `[s0, s1]` that reach `[clip0, clip1]`.
fn dots(s0: f32, s1: f32, w: f32, clip0: f32, clip1: f32) -> Vec<(f32, f32)> {
    let len = s1 - s0;
    if !positive(w) || !positive(len) {
        return Vec::new();
    }
    let n = (len / (2.0 * w)).round().max(1.0);
    let pitch = len / n;
    // Dot i is centered at s0 + (i + 0.5) * pitch.
    spans(s0 + (pitch - w) / 2.0, n, pitch, w, clip0, clip1)
}

/// `n` spans of length `len` starting at `start + i * pitch`, limited to
/// the ones that overlap `[clip0, clip1]` so that a huge border near a
/// small viewport generates few pieces.
fn spans(start: f32, n: f32, pitch: f32, len: f32, clip0: f32, clip1: f32) -> Vec<(f32, f32)> {
    if !positive(pitch) || !n.is_finite() {
        return Vec::new();
    }
    let first = ((clip0 - start - len) / pitch).ceil().max(0.0);
    let last = ((clip1 - start) / pitch).floor().min(n - 1.0);
    let mut out = Vec::new();
    let mut i = first;
    while i <= last {
        let a = start + i * pitch;
        out.push((a, a + len));
        i += 1.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashes_start_and_end_with_dash() {
        let d = dashes(0.0, 100.0, 4.0, 2.0, -1e9, 1e9);
        assert_eq!(d.first().map(|x| x.0), Some(0.0));
        let last = d.last().map(|x| x.1).unwrap_or_default();
        assert!((last - 100.0).abs() < 1e-3);
    }

    #[test]
    fn spans_are_culled_to_clip() {
        // A million-pixel side, clip 100 px wide: only a handful of dashes.
        let d = dashes(0.0, 1.0e6, 2.0, 1.0, 500.0, 600.0);
        assert!(d.len() <= 36, "{}", d.len());
        assert!(d.iter().all(|(a, b)| *b >= 500.0 && *a <= 600.0));
    }

    #[test]
    fn dots_are_centered() {
        let d = dots(0.0, 40.0, 5.0, -1e9, 1e9);
        assert_eq!(d.len(), 4);
        assert_eq!(d.first().copied(), Some((2.5, 7.5)));
    }
}
