//! The float exclusion map of one block formatting context: where floats
//! push line boxes and block formatting context roots aside (CSS 2.2
//! §9.5, §9.5.1).
//!
//! Left floats and right floats are kept as two step functions of `y`
//! (sorted bands): the rightmost left-float edge and the leftmost
//! right-float edge at each height. CSS 2.2 §9.5.1 rules 5 and 6 make
//! float tops non-decreasing in document order, and rules 2 and 3 put a
//! new float beside every earlier float on its side that it overlaps, so
//! adding a float flattens the bands it covers into one: the number of
//! bands stays below twice the number of floats, and each insertion only
//! touches the bands under the new float. Queries binary-search the band
//! list.

use wb_style::Clear;

use crate::geom::Point;

/// The most floats one block formatting context records. Floats past the
/// limit are still placed but no longer push other content aside, which
/// keeps queries bounded on hostile pages.
pub(crate) const MAX_FLOATS: usize = 4096;

/// Slack for floating-point fit comparisons.
pub(crate) const EPSILON: f32 = 0.01;

/// A step function over `y`.
#[derive(Debug, Clone)]
struct Steps {
    /// Breakpoints `(y, value)`, sorted by `y` and unique: `value` holds
    /// from `y` to the next breakpoint (the last one holds forever).
    /// Before the first breakpoint the value is `none`.
    segs: Vec<(f32, f32)>,
    /// The value where no float is present.
    none: f32,
    /// Picks the more constraining of two values (`max` for left edges,
    /// `min` for right edges).
    better: fn(f32, f32) -> f32,
}

impl Steps {
    fn new(none: f32, better: fn(f32, f32) -> f32) -> Steps {
        Steps {
            segs: Vec::new(),
            none,
            better,
        }
    }

    /// The value at `y`.
    fn at(&self, y: f32) -> f32 {
        self.segs
            .partition_point(|s| s.0 <= y)
            .checked_sub(1)
            .and_then(|i| self.segs.get(i))
            .map_or(self.none, |s| s.1)
    }

    /// The most constraining value over `[t, b)` (just `at(t)` when the
    /// range is empty).
    fn extreme(&self, t: f32, b: f32) -> f32 {
        let mut v = self.at(t);
        let start = self.segs.partition_point(|s| s.0 <= t);
        for s in self.segs.iter().skip(start) {
            if s.0 >= b {
                break;
            }
            v = (self.better)(v, s.1);
        }
        v
    }

    /// The first breakpoint strictly below `y`.
    fn next_after(&self, y: f32) -> Option<f32> {
        let i = self.segs.partition_point(|s| s.0 <= y);
        self.segs.get(i).map(|s| s.0)
    }

    /// Inserts a breakpoint at `y` that keeps the current value.
    fn split(&mut self, y: f32) {
        let i = self.segs.partition_point(|s| s.0 < y);
        if self.segs.get(i).is_some_and(|s| s.0 == y) {
            return;
        }
        let v = self.at(y);
        self.segs.insert(i, (y, v));
    }

    /// Makes the value on `[t, b)` at least as constraining as `v`.
    fn apply(&mut self, t: f32, b: f32, v: f32) {
        if b <= t || !t.is_finite() || !b.is_finite() {
            return;
        }
        self.split(t);
        self.split(b);
        let lo = self.segs.partition_point(|s| s.0 < t);
        let hi = self.segs.partition_point(|s| s.0 < b);
        let better = self.better;
        for s in self.segs.iter_mut().skip(lo).take(hi - lo) {
            s.1 = better(s.1, v);
        }
        // Merge equal neighbours around the touched range.
        let a = lo.saturating_sub(1);
        let e = (hi + 1).min(self.segs.len());
        let mut prev = a
            .checked_sub(1)
            .and_then(|i| self.segs.get(i))
            .map_or(self.none, |s| s.1);
        let mut kept: Vec<(f32, f32)> = Vec::with_capacity(e.saturating_sub(a));
        for s in self.segs.get(a..e).unwrap_or(&[]) {
            if s.1 != prev {
                kept.push(*s);
                prev = s.1;
            }
        }
        self.segs.splice(a..e, kept);
    }
}

/// The side a float goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
}

/// Which floats a box clears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClearSide {
    Left,
    Right,
    Both,
}

impl ClearSide {
    /// The computed `clear` value, for a left-to-right inline direction.
    pub(crate) fn from_style(c: Clear) -> Option<ClearSide> {
        match c {
            Clear::None => None,
            Clear::Left | Clear::InlineStart => Some(ClearSide::Left),
            Clear::Right | Clear::InlineEnd => Some(ClearSide::Right),
            Clear::Both => Some(ClearSide::Both),
        }
    }
}

/// A horizontal band free of floats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Band {
    /// The left edge (at least the containing block's left edge).
    pub lo: f32,
    /// The right edge (at least `lo`).
    pub hi: f32,
    /// Some float narrows the band.
    pub constrained: bool,
}

impl Band {
    pub(crate) fn width(&self) -> f32 {
        self.hi - self.lo
    }
}

/// The floats of one block formatting context, in its content-box
/// coordinates.
#[derive(Debug, Clone)]
pub(crate) struct Exclusions {
    left: Steps,
    right: Steps,
    count: usize,
    /// The top of the last float placed (CSS 2.2 §9.5.1 rules 5 and 6).
    last_top: f32,
    /// The lowest margin-box bottom of left and right floats.
    bottom_left: f32,
    bottom_right: f32,
}

impl Default for Exclusions {
    fn default() -> Self {
        Exclusions {
            left: Steps::new(f32::NEG_INFINITY, f32::max),
            right: Steps::new(f32::INFINITY, f32::min),
            count: 0,
            last_top: f32::NEG_INFINITY,
            bottom_left: f32::NEG_INFINITY,
            bottom_right: f32::NEG_INFINITY,
        }
    }
}

impl Exclusions {
    /// Whether any float reaches below `y`.
    pub(crate) fn any_below(&self, y: f32) -> bool {
        self.bottom_left.max(self.bottom_right) > y
    }

    /// The lowest float bottom (`None` without floats).
    pub(crate) fn bottom(&self) -> Option<f32> {
        let b = self.bottom_left.max(self.bottom_right);
        b.is_finite().then_some(b)
    }

    /// Where a box clearing `side` may start (CSS 2.2 §9.5.2): below the
    /// relevant floats; negative infinity when there are none.
    pub(crate) fn clear_y(&self, side: ClearSide) -> f32 {
        match side {
            ClearSide::Left => self.bottom_left,
            ClearSide::Right => self.bottom_right,
            ClearSide::Both => self.bottom_left.max(self.bottom_right),
        }
    }

    /// The space between floats over `[y, y + h)` inside the containing
    /// block range `[lo, hi]`.
    pub(crate) fn band(&self, y: f32, h: f32, lo: f32, hi: f32) -> Band {
        let b = y + h.max(0.0);
        let l = self.left.extreme(y, b);
        let r = self.right.extreme(y, b);
        let constrained = l > lo || r < hi;
        let lo2 = l.max(lo);
        let hi2 = r.min(hi).max(lo2);
        Band {
            lo: lo2,
            hi: hi2,
            constrained,
        }
    }

    /// The next height below `y` where the float edges change.
    pub(crate) fn next_change(&self, y: f32) -> Option<f32> {
        match (self.left.next_after(y), self.right.next_after(y)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Places a float whose margin box is `w` × `h` in a containing block
    /// spanning `[lo, hi]` horizontally, no higher than `y0` (CSS 2.2
    /// §9.5.1), and records it. Returns the margin box's top-left corner.
    pub(crate) fn place(&mut self, side: Side, w: f32, h: f32, y0: f32, lo: f32, hi: f32) -> Point {
        let w = w.max(0.0);
        let h = h.max(0.0);
        // Rules 4–6: not above the containing block, earlier floats or
        // the current line (the caller's `y0`).
        let mut y = y0.max(self.last_top);
        let mut steps = 0usize;
        let band = loop {
            let band = self.band(y, h, lo, hi);
            // Rule 7: as high as possible where it fits; a float wider than
            // its containing block goes where no float is in the way.
            if !band.constrained || band.width() + EPSILON >= w {
                break band;
            }
            steps += 1;
            match self.next_change(y) {
                Some(n) if steps <= 2 * MAX_FLOATS + 4 => y = n,
                _ => break band,
            }
        };
        // Rule 8/9: as far left (right) as possible.
        let x = match side {
            Side::Left => band.lo,
            Side::Right => band.hi - w,
        };
        self.last_top = y;
        if w > 0.0 && h > 0.0 && self.count < MAX_FLOATS {
            self.count += 1;
            match side {
                Side::Left => {
                    self.left.apply(y, y + h, x + w);
                    self.bottom_left = self.bottom_left.max(y + h);
                }
                Side::Right => {
                    self.right.apply(y, y + h, x);
                    self.bottom_right = self.bottom_right.max(y + h);
                }
            }
        }
        Point::new(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_floats_stack_and_wrap() {
        let mut e = Exclusions::default();
        assert_eq!(
            e.place(Side::Left, 30.0, 20.0, 0.0, 0.0, 100.0),
            Point::new(0.0, 0.0)
        );
        assert_eq!(
            e.place(Side::Left, 30.0, 10.0, 0.0, 0.0, 100.0),
            Point::new(30.0, 0.0)
        );
        // No room beside the first two: drops below the shorter one.
        assert_eq!(
            e.place(Side::Left, 50.0, 10.0, 0.0, 0.0, 100.0),
            Point::new(30.0, 10.0)
        );
        let b = e.band(0.0, 5.0, 0.0, 100.0);
        assert_eq!((b.lo, b.hi, b.constrained), (60.0, 100.0, true));
        let b = e.band(25.0, 5.0, 0.0, 100.0);
        assert_eq!((b.lo, b.hi, b.constrained), (0.0, 100.0, false));
        assert_eq!(e.clear_y(ClearSide::Left), 20.0);
        assert_eq!(e.clear_y(ClearSide::Right), f32::NEG_INFINITY);
    }

    #[test]
    fn right_and_left_share_the_line() {
        let mut e = Exclusions::default();
        e.place(Side::Right, 40.0, 30.0, 0.0, 0.0, 100.0);
        assert_eq!(
            e.place(Side::Left, 60.0, 10.0, 0.0, 0.0, 100.0),
            Point::new(0.0, 0.0)
        );
        // 70 does not fit next to the 40px right float until it ends.
        assert_eq!(
            e.place(Side::Left, 70.0, 10.0, 0.0, 0.0, 100.0),
            Point::new(0.0, 30.0)
        );
        assert_eq!(e.next_change(0.0), Some(10.0));
    }

    #[test]
    fn tops_never_go_up_and_wide_floats_go_below() {
        let mut e = Exclusions::default();
        e.place(Side::Left, 50.0, 100.0, 50.0, 0.0, 100.0);
        // Rule 6: not above the earlier float's top even if asked to.
        let p = e.place(Side::Right, 10.0, 10.0, 0.0, 0.0, 100.0);
        assert_eq!(p, Point::new(90.0, 50.0));
        // Wider than the space anywhere next to floats: below them all.
        let p = e.place(Side::Left, 200.0, 10.0, 0.0, 0.0, 100.0);
        assert_eq!(p, Point::new(0.0, 150.0));
    }

    #[test]
    fn many_floats_stay_cheap() {
        let mut e = Exclusions::default();
        for i in 0..20_000 {
            let side = if i % 2 == 0 { Side::Left } else { Side::Right };
            e.place(side, 1.0, 1000.0 - (i % 500) as f32, 0.0, 0.0, 100.0);
        }
        assert!(e.count <= MAX_FLOATS);
        assert!(e.left.segs.len() <= 2 * MAX_FLOATS + 2);
    }
}
