//! Geometry in CSS pixels: points, sizes, rectangles and box edges.
//!
//! The y axis points down. Every value layout produces is finite and
//! clamped to [`MAX_COORD`], so consumers never see NaN or infinities.

/// The largest magnitude of any coordinate or length layout produces, in
/// CSS px. Page-controlled values beyond it (huge widths, margins, font
/// sizes) are clamped, which keeps `f32` arithmetic exact enough and
/// prevents overflow to infinity.
pub const MAX_COORD: f32 = 10_000_000.0;

/// Clamps a page-controlled length to `-MAX_COORD..=MAX_COORD`; NaN
/// becomes 0.
pub(crate) fn sane(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-MAX_COORD, MAX_COORD)
    }
}

/// Like [`sane`] but also clamps negative values to 0.
pub(crate) fn sane_non_negative(v: f32) -> f32 {
    sane(v).max(0.0)
}

/// A point (or a vector) in CSS px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// The horizontal coordinate, growing to the right.
    pub x: f32,
    /// The vertical coordinate, growing downwards.
    pub y: f32,
}

impl Point {
    /// The origin.
    pub const ZERO: Point = Point { x: 0.0, y: 0.0 };

    /// A point at `(x, y)`.
    pub fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }

    /// The point moved by `other`.
    pub fn offset(self, other: Point) -> Point {
        Point::new(sane(self.x + other.x), sane(self.y + other.y))
    }
}

/// A size in CSS px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    /// The width.
    pub width: f32,
    /// The height.
    pub height: f32,
}

impl Size {
    /// A size of `width` by `height`.
    pub fn new(width: f32, height: f32) -> Size {
        Size { width, height }
    }
}

/// An axis-aligned rectangle in CSS px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// The left edge.
    pub x: f32,
    /// The top edge.
    pub y: f32,
    /// The width (never negative in layout output).
    pub width: f32,
    /// The height (never negative in layout output).
    pub height: f32,
}

impl Rect {
    /// A rectangle with its top-left corner at `(x, y)`.
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The top-left corner.
    pub fn origin(&self) -> Point {
        Point::new(self.x, self.y)
    }

    /// The size.
    pub fn size(&self) -> Size {
        Size::new(self.width, self.height)
    }

    /// The right edge.
    pub fn max_x(&self) -> f32 {
        self.x + self.width
    }

    /// The bottom edge.
    pub fn max_y(&self) -> f32 {
        self.y + self.height
    }

    /// Whether `p` lies inside (left/top edges inclusive, right/bottom
    /// exclusive).
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.max_x() && p.y < self.max_y()
    }

    /// Whether the rectangle has no area.
    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// The rectangle moved by `d`.
    pub fn translate(&self, d: Point) -> Rect {
        Rect::new(
            sane(self.x + d.x),
            sane(self.y + d.y),
            self.width,
            self.height,
        )
    }

    /// The smallest rectangle containing both. An empty rectangle still
    /// contributes its position (scrollable overflow counts empty boxes'
    /// positions too, CSS Overflow 3 §2.2 is about border boxes).
    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let max_x = self.max_x().max(other.max_x());
        let max_y = self.max_y().max(other.max_y());
        Rect::new(
            x,
            y,
            sane_non_negative(max_x - x),
            sane_non_negative(max_y - y),
        )
    }

    /// The overlap of two rectangles (zero-sized when they do not overlap).
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let max_x = self.max_x().min(other.max_x());
        let max_y = self.max_y().min(other.max_y());
        Rect::new(x, y, (max_x - x).max(0.0), (max_y - y).max(0.0))
    }

    /// The rectangle shrunk by `e` on each side (never below zero size).
    pub fn deflate(&self, e: &Edges) -> Rect {
        Rect::new(
            self.x + e.left,
            self.y + e.top,
            (self.width - e.left - e.right).max(0.0),
            (self.height - e.top - e.bottom).max(0.0),
        )
    }

    /// The rectangle grown by `e` on each side.
    pub fn inflate(&self, e: &Edges) -> Rect {
        Rect::new(
            self.x - e.left,
            self.y - e.top,
            (self.width + e.left + e.right).max(0.0),
            (self.height + e.top + e.bottom).max(0.0),
        )
    }
}

/// Widths of the four sides of a box edge (margin, border or padding), in
/// CSS px.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    /// The top side.
    pub top: f32,
    /// The right side.
    pub right: f32,
    /// The bottom side.
    pub bottom: f32,
    /// The left side.
    pub left: f32,
}

impl Edges {
    /// All sides zero.
    pub const ZERO: Edges = Edges {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    /// Edges with the given sides.
    pub fn new(top: f32, right: f32, bottom: f32, left: f32) -> Edges {
        Edges {
            top,
            right,
            bottom,
            left,
        }
    }

    /// `left + right`.
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    /// `top + bottom`.
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }

    /// The side-wise sum.
    pub fn add(&self, o: &Edges) -> Edges {
        Edges::new(
            self.top + o.top,
            self.right + o.right,
            self.bottom + o.bottom,
            self.left + o.left,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sane_values() {
        assert_eq!(sane(f32::NAN), 0.0);
        assert_eq!(sane(f32::INFINITY), MAX_COORD);
        assert_eq!(sane(f32::NEG_INFINITY), -MAX_COORD);
        assert_eq!(sane_non_negative(-5.0), 0.0);
        assert_eq!(sane(12.5), 12.5);
    }

    #[test]
    fn rect_ops() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.union(&b), Rect::new(0.0, 0.0, 15.0, 15.0));
        assert_eq!(a.intersect(&b), Rect::new(5.0, 5.0, 5.0, 5.0));
        assert!(a.contains(Point::new(0.0, 9.9)));
        assert!(!a.contains(Point::new(10.0, 5.0)));
        let e = Edges::new(1.0, 2.0, 3.0, 4.0);
        assert_eq!(a.deflate(&e), Rect::new(4.0, 1.0, 4.0, 6.0));
        assert_eq!(a.deflate(&e).inflate(&e), a);
        assert_eq!(e.horizontal(), 6.0);
        assert_eq!(e.vertical(), 4.0);
    }
}
