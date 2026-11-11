//! Geometry and color types used by display lists. All lengths are CSS px.

/// A point in CSS px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Horizontal position, growing to the right.
    pub x: f32,
    /// Vertical position, growing downwards.
    pub y: f32,
}

impl Point {
    /// Makes a point.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    pub(crate) fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// An axis-aligned rectangle in CSS px: its top-left corner and its size.
///
/// A rectangle covers the half-open area `x <= px < x + width`,
/// `y <= py < y + height`, so rectangles that share an edge never both
/// contain a point on it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width. Negative widths are treated as zero.
    pub width: f32,
    /// Height. Negative heights are treated as zero.
    pub height: f32,
}

impl Rect {
    /// Makes a rectangle from its top-left corner and size.
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The right edge (`x + width`, with negative widths treated as zero).
    #[must_use]
    pub fn right(&self) -> f32 {
        self.x + self.width.max(0.0)
    }

    /// The bottom edge (`y + height`, with negative heights treated as zero).
    #[must_use]
    pub fn bottom(&self) -> f32 {
        self.y + self.height.max(0.0)
    }

    /// Whether the rectangle covers no area (or has a non-finite field).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !(self.width > 0.0 && self.height > 0.0) || !self.is_finite()
    }

    /// Whether `p` is inside the half-open area of the rectangle.
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        !self.is_empty()
            && p.x >= self.x
            && p.x < self.right()
            && p.y >= self.y
            && p.y < self.bottom()
    }

    /// The rectangle moved by (`dx`, `dy`).
    #[must_use]
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// The rectangle grown by `d` on every side (shrunk if `d` is negative).
    #[must_use]
    pub fn inflate(&self, d: f32) -> Rect {
        Rect::new(
            self.x - d,
            self.y - d,
            self.width + 2.0 * d,
            self.height + 2.0 * d,
        )
    }

    pub(crate) fn is_finite(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
    }
}

/// A non-premultiplied sRGB color with 8-bit channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha: 0 is fully transparent, 255 fully opaque.
    pub a: u8,
}

impl Color {
    /// Fully transparent black.
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    /// Opaque black.
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    /// Opaque white.
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    /// Makes an opaque color.
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }

    /// Makes a color with alpha.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }
}

/// The two radii of one elliptical corner (CSS Backgrounds 3 §5.1), in
/// CSS px. If either is zero the corner is square (§5.2).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Radius {
    /// Horizontal radius.
    pub x: f32,
    /// Vertical radius.
    pub y: f32,
}

impl Radius {
    /// Makes a radius.
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Radius {
        Radius { x, y }
    }

    /// A circular radius.
    #[must_use]
    pub const fn circular(r: f32) -> Radius {
        Radius { x: r, y: r }
    }

    /// Whether this corner is square: one of the radii is zero, negative or
    /// not a number.
    #[must_use]
    pub fn is_square(&self) -> bool {
        !(self.x > 0.0 && self.y > 0.0)
    }

    fn sanitized(self) -> Radius {
        if self.is_square() || !self.x.is_finite() || !self.y.is_finite() {
            Radius::default()
        } else {
            self
        }
    }

    fn scaled(self, f: f32) -> Radius {
        Radius::new(self.x * f, self.y * f)
    }
}

/// The radii of the four corners of a box (`border-*-radius`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CornerRadii {
    /// `border-top-left-radius`.
    pub top_left: Radius,
    /// `border-top-right-radius`.
    pub top_right: Radius,
    /// `border-bottom-right-radius`.
    pub bottom_right: Radius,
    /// `border-bottom-left-radius`.
    pub bottom_left: Radius,
}

impl CornerRadii {
    /// The same radius on every corner.
    #[must_use]
    pub const fn uniform(r: Radius) -> CornerRadii {
        CornerRadii {
            top_left: r,
            top_right: r,
            bottom_right: r,
            bottom_left: r,
        }
    }

    /// Whether every corner is square.
    #[must_use]
    pub fn is_square(&self) -> bool {
        self.top_left.is_square()
            && self.top_right.is_square()
            && self.bottom_right.is_square()
            && self.bottom_left.is_square()
    }

    /// The radii made usable for a box of `width` x `height`:
    /// square corners (§5.2) become zero, and if the radii on any side add
    /// up to more than that side's length, all radii are scaled down by the
    /// same factor so that none overlap (CSS Backgrounds 3 §5.5,
    /// "Overlapping Curves").
    #[must_use]
    pub fn normalized(&self, width: f32, height: f32) -> CornerRadii {
        let tl = self.top_left.sanitized();
        let tr = self.top_right.sanitized();
        let br = self.bottom_right.sanitized();
        let bl = self.bottom_left.sanitized();
        let w = width.max(0.0);
        let h = height.max(0.0);
        // f = min(L_i / S_i) over the four sides, where S_i > 0.
        let mut f = 1.0f32;
        for (len, sum) in [
            (w, tl.x + tr.x),
            (w, bl.x + br.x),
            (h, tl.y + bl.y),
            (h, tr.y + br.y),
        ] {
            if sum > 0.0 {
                f = f.min(len / sum);
            }
        }
        let f = if f.is_finite() { f.max(0.0) } else { 0.0 };
        let fix = |r: Radius| r.scaled(f).sanitized();
        CornerRadii {
            top_left: fix(tl),
            top_right: fix(tr),
            bottom_right: fix(br),
            bottom_left: fix(bl),
        }
    }

    pub(crate) fn is_finite(&self) -> bool {
        [
            self.top_left,
            self.top_right,
            self.bottom_right,
            self.bottom_left,
        ]
        .iter()
        .all(|r| r.x.is_finite() && r.y.is_finite())
    }
}

/// A rectangle with elliptical corners.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RoundedRect {
    /// The bounding rectangle.
    pub rect: Rect,
    /// The corner radii. [`RoundedRect::new`] normalizes them; the
    /// rasterizer and hit testing normalize them again, so a hand-built
    /// value is safe too.
    pub radii: CornerRadii,
}

impl RoundedRect {
    /// Makes a rounded rectangle, normalizing the radii (see
    /// [`CornerRadii::normalized`]).
    #[must_use]
    pub fn new(rect: Rect, radii: CornerRadii) -> RoundedRect {
        RoundedRect {
            rect,
            radii: radii.normalized(rect.width, rect.height),
        }
    }

    /// A rounded rectangle with square corners.
    #[must_use]
    pub fn from_rect(rect: Rect) -> RoundedRect {
        RoundedRect {
            rect,
            radii: CornerRadii::default(),
        }
    }

    /// Whether `p` is inside the shape: inside the half-open rectangle and,
    /// within a rounded corner's box, inside that corner's ellipse.
    #[must_use]
    pub fn contains(&self, p: Point) -> bool {
        if !self.rect.contains(p) {
            return false;
        }
        let r = &self.rect;
        let radii = self.radii.normalized(r.width, r.height);
        let corners = [
            (
                radii.top_left,
                r.x + radii.top_left.x,
                r.y + radii.top_left.y,
                true,
                true,
            ),
            (
                radii.top_right,
                r.right() - radii.top_right.x,
                r.y + radii.top_right.y,
                false,
                true,
            ),
            (
                radii.bottom_right,
                r.right() - radii.bottom_right.x,
                r.bottom() - radii.bottom_right.y,
                false,
                false,
            ),
            (
                radii.bottom_left,
                r.x + radii.bottom_left.x,
                r.bottom() - radii.bottom_left.y,
                true,
                false,
            ),
        ];
        for (radius, cx, cy, left, top) in corners {
            if radius.is_square() {
                continue;
            }
            let in_x = if left { p.x < cx } else { p.x > cx };
            let in_y = if top { p.y < cy } else { p.y > cy };
            if in_x && in_y {
                let dx = (p.x - cx) / radius.x;
                let dy = (p.y - cy) / radius.y;
                if dx * dx + dy * dy > 1.0 {
                    return false;
                }
            }
        }
        true
    }

    pub(crate) fn is_finite(&self) -> bool {
        self.rect.is_finite() && self.radii.is_finite()
    }
}

impl From<Rect> for RoundedRect {
    fn from(rect: Rect) -> RoundedRect {
        RoundedRect::from_rect(rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains(Point::new(0.0, 0.0)));
        assert!(r.contains(Point::new(9.99, 9.99)));
        assert!(!r.contains(Point::new(10.0, 5.0)));
        assert!(!Rect::new(0.0, 0.0, -1.0, 5.0).contains(Point::new(0.0, 0.0)));
    }

    #[test]
    fn overlapping_radii_are_scaled() {
        // 100 wide, radii 80 + 80 on the top: f = 100 / 160.
        let radii = CornerRadii::uniform(Radius::circular(80.0)).normalized(100.0, 400.0);
        assert!((radii.top_left.x - 50.0).abs() < 1e-4);
        assert!((radii.top_left.y - 50.0).abs() < 1e-4);
    }

    #[test]
    fn zero_component_makes_square_corner() {
        let radii = CornerRadii {
            top_left: Radius::new(10.0, 0.0),
            ..CornerRadii::default()
        }
        .normalized(100.0, 100.0);
        assert!(radii.is_square());
        let nan = CornerRadii::uniform(Radius::circular(f32::NAN)).normalized(10.0, 10.0);
        assert!(nan.is_square());
    }

    #[test]
    fn rounded_contains_respects_corners() {
        let rr = RoundedRect::new(
            Rect::new(0.0, 0.0, 20.0, 20.0),
            CornerRadii::uniform(Radius::circular(10.0)),
        );
        assert!(!rr.contains(Point::new(0.5, 0.5)));
        assert!(!rr.contains(Point::new(19.5, 19.5)));
        assert!(rr.contains(Point::new(10.0, 10.0)));
        assert!(rr.contains(Point::new(10.0, 0.5)));
        assert!(rr.contains(Point::new(3.0, 3.0)));
    }
}
