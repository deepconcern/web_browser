//! Device-space geometry: snapped rectangles, rounded rectangles flattened
//! to convex polygons, and polygon clipping.
//!
//! Every shape paint draws is a convex polygon, or a ring made of two
//! nested convex polygons (filled even-odd). Convex polygons can be clipped
//! exactly against half-planes (Sutherland–Hodgman), which is how rect clips
//! and border side regions are applied without allocating masks.

use std::f32::consts::FRAC_PI_2;

/// A polygon vertex in device px.
pub(crate) type Pt = [f32; 2];

/// A convex polygon.
pub(crate) type Poly = Vec<Pt>;

/// An integer rectangle in device px, `x0 <= x < x1`, `y0 <= y < y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub(crate) const EMPTY: IRect = IRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    pub(crate) fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub(crate) fn width(&self) -> u32 {
        u32::try_from(i64::from(self.x1) - i64::from(self.x0)).unwrap_or(0)
    }

    pub(crate) fn height(&self) -> u32 {
        u32::try_from(i64::from(self.y1) - i64::from(self.y0)).unwrap_or(0)
    }

    pub(crate) fn intersect(&self, o: &IRect) -> IRect {
        let r = IRect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        if r.is_empty() { IRect::EMPTY } else { r }
    }

    /// The smallest integer rectangle containing `r` (outward rounding).
    pub(crate) fn enclosing(r: &DRect) -> IRect {
        let out = IRect {
            x0: sat(r.l.floor()),
            y0: sat(r.t.floor()),
            x1: sat(r.r.ceil()),
            y1: sat(r.b.ceil()),
        };
        if out.is_empty() { IRect::EMPTY } else { out }
    }

    pub(crate) fn to_drect(self) -> DRect {
        DRect {
            l: self.x0 as f32,
            t: self.y0 as f32,
            r: self.x1 as f32,
            b: self.y1 as f32,
        }
    }
}

/// Whether `v > 0` (false for NaN).
pub(crate) fn positive(v: f32) -> bool {
    v > 0.0
}

/// Saturating float to i32 (NaN becomes 0).
pub(crate) fn sat(v: f32) -> i32 {
    // `as` saturates and maps NaN to 0.
    v as i32
}

/// Rounds half up, so snapping is the same wherever a shape is on the
/// device grid (f32::round rounds half away from zero instead).
pub(crate) fn snap(v: f32) -> f32 {
    (v + 0.5).floor()
}

/// Snaps a border width in device px (CSS Values 4, "snap as a border
/// width"): between 0 and 1 becomes 1, otherwise floor.
pub(crate) fn snap_border_width(w: f32) -> f32 {
    if !positive(w) || !w.is_finite() {
        0.0
    } else if w < 1.0 {
        1.0
    } else {
        // A small tolerance so 2.9999 from float error stays 3.
        (w + 1e-3).floor()
    }
}

/// A rectangle in device px by its edges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DRect {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl DRect {
    pub(crate) fn is_empty(&self) -> bool {
        !(self.r > self.l && self.b > self.t)
    }

    pub(crate) fn width(&self) -> f32 {
        (self.r - self.l).max(0.0)
    }

    pub(crate) fn height(&self) -> f32 {
        (self.b - self.t).max(0.0)
    }

    pub(crate) fn intersect(&self, o: &DRect) -> DRect {
        DRect {
            l: self.l.max(o.l),
            t: self.t.max(o.t),
            r: self.r.min(o.r),
            b: self.b.min(o.b),
        }
    }

    pub(crate) fn poly(&self) -> Poly {
        vec![
            [self.l, self.t],
            [self.r, self.t],
            [self.r, self.b],
            [self.l, self.b],
        ]
    }
}

/// Corner radii in device px: (x, y) for top-left, top-right,
/// bottom-right, bottom-left.
pub(crate) type DRadii = [[f32; 2]; 4];

/// A rounded rectangle in device px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DRRect {
    pub rect: DRect,
    pub radii: DRadii,
}

impl DRRect {
    pub(crate) fn is_rect(&self) -> bool {
        self.radii.iter().all(|r| !(r[0] > 0.0 && r[1] > 0.0))
    }

    /// The rounded rectangle inset by per-side amounts (top, right,
    /// bottom, left), with each corner radius reduced by the adjacent
    /// insets and clamped at zero (CSS Backgrounds 3 §5.2, inner border
    /// radius). If the insets cross, the result collapses to a line at the
    /// point that splits the box in proportion to the insets.
    pub(crate) fn inset(&self, t: f32, r: f32, b: f32, l: f32) -> DRRect {
        let mut x0 = self.rect.l + l;
        let mut x1 = self.rect.r - r;
        if x0 > x1 {
            let m = if l + r > 0.0 {
                self.rect.l + self.rect.width() * l / (l + r)
            } else {
                self.rect.l
            };
            x0 = m;
            x1 = m;
        }
        let mut y0 = self.rect.t + t;
        let mut y1 = self.rect.b - b;
        if y0 > y1 {
            let m = if t + b > 0.0 {
                self.rect.t + self.rect.height() * t / (t + b)
            } else {
                self.rect.t
            };
            y0 = m;
            y1 = m;
        }
        let [tl, tr, br, bl] = self.radii;
        let shrink = |c: [f32; 2], dx: f32, dy: f32| {
            let v = [(c[0] - dx).max(0.0), (c[1] - dy).max(0.0)];
            if v[0] > 0.0 && v[1] > 0.0 {
                v
            } else {
                [0.0, 0.0]
            }
        };
        DRRect {
            rect: DRect {
                l: x0,
                t: y0,
                r: x1,
                b: y1,
            },
            radii: [
                shrink(tl, l, t),
                shrink(tr, r, t),
                shrink(br, r, b),
                shrink(bl, l, b),
            ],
        }
    }

    /// The outline as a convex polygon, clockwise on screen, starting at
    /// the top-left corner. Curves are flattened finely enough that the
    /// chord error stays under a tenth of a pixel.
    pub(crate) fn poly(&self) -> Poly {
        if self.is_rect() {
            return self.rect.poly();
        }
        let DRect { l, t, r, b } = self.rect;
        let [tl, tr, br, bl] = self.radii;
        let mut out = Vec::new();
        // (radius, center, start angle) for each corner, angles measured
        // with y pointing down.
        let corners = [
            (tl, [l + tl[0], t + tl[1]], 2.0 * FRAC_PI_2, [l, t]),
            (tr, [r - tr[0], t + tr[1]], 3.0 * FRAC_PI_2, [r, t]),
            (br, [r - br[0], b - br[1]], 0.0, [r, b]),
            (bl, [l + bl[0], b - bl[1]], FRAC_PI_2, [l, b]),
        ];
        for (radius, c, start, square) in corners {
            if !(radius[0] > 0.0 && radius[1] > 0.0) {
                out.push(square);
                continue;
            }
            let n = arc_segments(radius[0].max(radius[1]));
            for i in 0..=n {
                let a = start + FRAC_PI_2 * (i as f32) / (n as f32);
                out.push([c[0] + radius[0] * a.cos(), c[1] + radius[1] * a.sin()]);
            }
        }
        out
    }
}

/// Segments for a quarter ellipse of the given largest radius (device px):
/// chord error `r * (1 - cos(θ/2))` at most 0.1 px, capped.
fn arc_segments(r: f32) -> u32 {
    if !positive(r) {
        return 1;
    }
    let ratio = (1.0 - 0.1 / r).clamp(-1.0, 1.0);
    let step = 2.0 * ratio.acos();
    if !positive(step) {
        return 128;
    }
    let n = (FRAC_PI_2 / step).ceil();
    if n.is_finite() {
        (n as u32).clamp(2, 128)
    } else {
        128
    }
}

/// Keeps the part of a convex polygon where `f(p) >= 0` for the linear
/// function `f(p) = a * x + b * y + c` (one Sutherland–Hodgman step).
pub(crate) fn clip_halfplane(poly: &[Pt], a: f32, b: f32, c: f32) -> Poly {
    let f = |p: &Pt| a * p[0] + b * p[1] + c;
    let mut out = Vec::with_capacity(poly.len() + 2);
    let Some(mut prev) = poly.last() else {
        return out;
    };
    let mut fp = f(prev);
    for cur in poly {
        let fc = f(cur);
        if (fc >= 0.0) != (fp >= 0.0) {
            let t = fp / (fp - fc);
            out.push([
                prev[0] + (cur[0] - prev[0]) * t,
                prev[1] + (cur[1] - prev[1]) * t,
            ]);
        }
        if fc >= 0.0 {
            out.push(*cur);
        }
        prev = cur;
        fp = fc;
    }
    out
}

/// Clips a convex polygon to a rectangle.
pub(crate) fn clip_to_rect(poly: &[Pt], r: &DRect) -> Poly {
    let p = clip_halfplane(poly, 1.0, 0.0, -r.l);
    let p = clip_halfplane(&p, -1.0, 0.0, r.r);
    let p = clip_halfplane(&p, 0.0, 1.0, -r.t);
    clip_halfplane(&p, 0.0, -1.0, r.b)
}

/// The half-plane through `p0` and `p1` that contains `inside`, as
/// coefficients for [`clip_halfplane`]. `None` if `p0 == p1`.
pub(crate) fn halfplane_through(p0: Pt, p1: Pt, inside: Pt) -> Option<(f32, f32, f32)> {
    // f(p) = cross(p1 - p0, p - p0) = dx * (y - y0) - dy * (x - x0).
    let dx = p1[0] - p0[0];
    let dy = p1[1] - p0[1];
    if dx == 0.0 && dy == 0.0 {
        return None;
    }
    let (a, b, c) = (-dy, dx, dy * p0[0] - dx * p0[1]);
    let s = a * inside[0] + b * inside[1] + c;
    Some(if s < 0.0 { (-a, -b, -c) } else { (a, b, c) })
}

/// Whether a polygon encloses any area.
pub(crate) fn has_area(poly: &[Pt]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut area2 = 0.0f32;
    let mut prev = poly.last();
    for p in poly {
        if let Some(q) = prev {
            area2 += q[0] * p[1] - p[0] * q[1];
        }
        prev = Some(p);
    }
    area2.abs() > 1e-4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping() {
        assert_eq!(snap(0.5), 1.0);
        assert_eq!(snap(-0.5), 0.0);
        assert_eq!(snap(1.49), 1.0);
        assert_eq!(snap_border_width(0.25), 1.0);
        assert_eq!(snap_border_width(1.75), 1.0);
        assert_eq!(snap_border_width(2.9999), 3.0);
        assert_eq!(snap_border_width(0.0), 0.0);
        assert_eq!(snap_border_width(f32::NAN), 0.0);
    }

    #[test]
    fn halfplane_clip_cuts_square() {
        let sq = DRect {
            l: 0.0,
            t: 0.0,
            r: 10.0,
            b: 10.0,
        }
        .poly();
        // Keep x <= 4.
        let p = clip_halfplane(&sq, -1.0, 0.0, 4.0);
        assert!(p.iter().all(|v| v[0] <= 4.0));
        assert!(has_area(&p));
        let none = clip_halfplane(&sq, -1.0, 0.0, -1.0);
        assert!(!has_area(&none));
    }

    #[test]
    fn rounded_poly_stays_in_bounds() {
        let rr = DRRect {
            rect: DRect {
                l: 0.0,
                t: 0.0,
                r: 40.0,
                b: 20.0,
            },
            radii: [[10.0, 5.0]; 4],
        };
        let p = rr.poly();
        assert!(p.len() > 8);
        for v in &p {
            assert!((0.0..=40.0).contains(&v[0]) && (0.0..=20.0).contains(&v[1]));
        }
    }

    #[test]
    fn inset_collapses_when_crossing() {
        let rr = DRRect {
            rect: DRect {
                l: 0.0,
                t: 0.0,
                r: 10.0,
                b: 10.0,
            },
            radii: [[6.0, 6.0]; 4],
        };
        let i = rr.inset(2.0, 8.0, 2.0, 8.0);
        assert_eq!(i.rect.l, i.rect.r);
        assert_eq!(i.rect.l, 5.0);
        assert_eq!(i.radii[0], [0.0, 0.0]);
        let j = rr.inset(1.0, 1.0, 1.0, 1.0);
        assert_eq!(j.radii[0], [5.0, 5.0]);
    }
}
