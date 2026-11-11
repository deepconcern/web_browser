//! The collapsing border model (CSS 2.2 §17.6.2): border conflict
//! resolution (§17.6.2.1) on every edge of the table grid.
//!
//! [`resolve`] returns one winning border per grid edge segment: per
//! (row line, column) for horizontal segments and per (row, column line)
//! for vertical ones. Linear in the grid area.

use wb_style::{BorderSide, LineStyle, Rgba};

/// A resolved border: what is drawn on one grid edge segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Resolved {
    /// Full width of the collapsed border (0 when nothing is drawn).
    pub width: f32,
    /// Its style (`none` or `hidden` when nothing is drawn).
    pub style: LineStyle,
    /// Its color.
    pub color: Rgba,
}

impl Resolved {
    /// No border.
    pub(crate) const NONE: Resolved = Resolved {
        width: 0.0,
        style: LineStyle::None,
        color: Rgba::TRANSPARENT,
    };

    /// As a computed border side of `width` (half the collapsed width,
    /// for the box on one side of the grid line).
    pub(crate) fn side(&self, width: f32) -> BorderSide {
        BorderSide {
            width,
            style: if self.width > 0.0 {
                self.style
            } else {
                LineStyle::None
            },
            color: self.color,
        }
    }
}

/// Which kind of box a candidate border comes from: higher wins ties
/// (§17.6.2.1 rule 4: cell, row, row group, column, column group, table).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Origin {
    Table = 0,
    ColumnGroup = 1,
    Column = 2,
    RowGroup = 3,
    Row = 4,
    Cell = 5,
}

/// §17.6.2.1 rule 3: style ranking for equal widths.
fn style_rank(s: LineStyle) -> u8 {
    match s {
        LineStyle::Double => 8,
        LineStyle::Solid => 7,
        LineStyle::Dashed => 6,
        LineStyle::Dotted => 5,
        LineStyle::Ridge => 4,
        LineStyle::Outset => 3,
        LineStyle::Groove => 2,
        LineStyle::Inset => 1,
        LineStyle::None | LineStyle::Hidden => 0,
    }
}

/// Picks the winner among candidates given in tie-break order (the
/// earlier one is further left or further up, and wins full ties).
pub(crate) fn resolve_edge(cands: &[(BorderSide, Origin)]) -> Resolved {
    // Rule 1: `hidden` suppresses every border on the edge.
    if cands.iter().any(|(s, _)| s.style == LineStyle::Hidden) {
        return Resolved {
            style: LineStyle::Hidden,
            ..Resolved::NONE
        };
    }
    let mut best: Option<(Resolved, Origin)> = None;
    for (s, origin) in cands {
        // Rule 2: `none` has the lowest priority (never wins).
        if s.style == LineStyle::None || !s.width.is_finite() || s.width <= 0.0 {
            continue;
        }
        let r = Resolved {
            width: s.width,
            style: s.style,
            color: s.color,
        };
        let wins = match &best {
            None => true,
            Some((b, bo)) => {
                r.width > b.width
                    || (r.width == b.width
                        && (style_rank(r.style) > style_rank(b.style)
                            || (style_rank(r.style) == style_rank(b.style) && origin > bo)))
            }
        };
        if wins {
            best = Some((r, *origin));
        }
    }
    best.map_or(Resolved::NONE, |(r, _)| r)
}

/// The resolved borders of a grid of `rows` × `cols` slots.
#[derive(Clone, Debug, Default)]
pub(crate) struct Grid {
    pub cols: usize,
    /// Horizontal segments: `(rows + 1) * cols`, row line major.
    pub h: Vec<Resolved>,
    /// Vertical segments: `rows * (cols + 1)`, row major.
    pub v: Vec<Resolved>,
}

impl Grid {
    /// The horizontal segment on row line `line` (0..=rows) at column `c`.
    pub(crate) fn h(&self, line: usize, c: usize) -> Resolved {
        self.h
            .get(line * self.cols + c)
            .copied()
            .unwrap_or(Resolved::NONE)
    }

    /// The vertical segment in row `r` on column line `line` (0..=cols).
    pub(crate) fn v(&self, r: usize, line: usize) -> Resolved {
        self.v
            .get(r * (self.cols + 1) + line)
            .copied()
            .unwrap_or(Resolved::NONE)
    }

    /// The widest segment among `segs` (the first on ties).
    pub(crate) fn widest(segs: impl Iterator<Item = Resolved>) -> Resolved {
        let mut best = Resolved::NONE;
        for s in segs {
            if s.width > best.width {
                best = s;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(width: f32, style: LineStyle, red: u8) -> BorderSide {
        BorderSide {
            width,
            style,
            color: Rgba::rgb(red, 0, 0),
        }
    }

    #[test]
    fn conflict_resolution() {
        use LineStyle::*;
        // Wider wins.
        let r = resolve_edge(&[
            (side(1.0, Solid, 1), Origin::Cell),
            (side(3.0, Dotted, 2), Origin::Table),
        ]);
        assert_eq!((r.width, r.style), (3.0, Dotted));
        // Hidden wins over everything.
        let r = resolve_edge(&[
            (side(5.0, Solid, 1), Origin::Cell),
            (side(0.0, Hidden, 2), Origin::Table),
        ]);
        assert_eq!(r.style, Hidden);
        assert_eq!(r.width, 0.0);
        // Equal widths: style rank.
        let r = resolve_edge(&[
            (side(2.0, Dashed, 1), Origin::Cell),
            (side(2.0, Double, 2), Origin::Row),
        ]);
        assert_eq!(r.style, Double);
        // Same style: the cell beats the row.
        let r = resolve_edge(&[
            (side(2.0, Solid, 1), Origin::Row),
            (side(2.0, Solid, 2), Origin::Cell),
        ]);
        assert_eq!(r.color, Rgba::rgb(2, 0, 0));
        // Full tie: the first (left/top) wins.
        let r = resolve_edge(&[
            (side(2.0, Solid, 1), Origin::Cell),
            (side(2.0, Solid, 2), Origin::Cell),
        ]);
        assert_eq!(r.color, Rgba::rgb(1, 0, 0));
        // All none.
        assert_eq!(
            resolve_edge(&[(side(0.0, None, 1), Origin::Cell)]),
            Resolved::NONE
        );
    }
}
