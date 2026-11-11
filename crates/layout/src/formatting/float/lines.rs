//! Line boxes next to floats (CSS 2.2 §9.5: "line boxes ... are shortened
//! to make room for the margin box of the float").
//!
//! [`LineSource`] hands the inline formatting context one line at a time:
//! where the line box starts, how wide it is, and which part of the
//! paragraph it holds. Without floats beside the paragraph it simply
//! breaks the whole paragraph at the container width, as before. With
//! floats it asks the BFC's exclusions for the free band at the line's
//! height and breaks one line into it; a line whose first unit does not
//! fit moves down to the next float edge. Each unit is broken once, so
//! the whole paragraph stays linear in its length (plus the band lookups,
//! logarithmic in the number of floats).

use super::exclusions::EPSILON;
use super::ifc_origin;
use crate::box_tree::BoxId;
use crate::context::LayoutCtx;
use crate::formatting::inline::Prepared;
use crate::formatting::inline::lines::{LineSpan, Unit, break_lines, break_lines_from, span_width};
use crate::fragment::MAX_FRAGMENTS;
use crate::geom::Point;

/// How many times one line moves down past float edges looking for room
/// before it is laid out where it is (overflowing).
const MAX_LINE_SHIFTS: usize = 64;

/// The next line of an inline formatting context.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NextLine {
    /// The part of the paragraph on the line.
    pub span: LineSpan,
    /// The line box's left edge in the container's content box.
    pub x: f32,
    /// The line box's width.
    pub width: f32,
    /// The line box's top in the container's content box (at least the
    /// `y` asked for).
    pub y: f32,
}

#[derive(Debug)]
enum Mode {
    /// No floats beside the paragraph: lines precomputed at full width.
    Plain { spans: Vec<LineSpan>, next: usize },
    /// Floats beside the paragraph: lines broken one at a time.
    Floats {
        /// The container's content-box origin in its BFC.
        origin: Point,
        /// The strut height used to look up the free band.
        strut: f32,
        /// The next unit to place.
        k: usize,
        /// The rest of a unit split by an emergency break.
        tail: Option<Unit>,
        count: usize,
        done: bool,
    },
}

/// Produces the lines of one inline formatting context.
#[derive(Debug)]
pub(crate) struct LineSource {
    mode: Mode,
    avail: f32,
    indent: f32,
}

impl LineSource {
    /// Starts the lines of container `id`, whose content box is `avail`
    /// wide, with `indent` before the first line. Places floats waiting
    /// for this content.
    pub(crate) fn new(
        ctx: &mut LayoutCtx<'_>,
        id: BoxId,
        prep: &Prepared,
        fixed: &[f32],
        units: &[Unit],
        avail: f32,
        indent: f32,
    ) -> LineSource {
        let origin = ifc_origin(ctx, id).filter(|o| {
            ctx.floats
                .bfcs
                .last()
                .is_some_and(|b| b.excl.any_below(o.y))
        });
        let mode = match (origin, ctx.get(id)) {
            (Some(origin), Some(b)) => {
                let style = b.style.clone();
                let strut = ctx.metrics(&style).line_height(&style).max(0.0);
                Mode::Floats {
                    origin,
                    strut,
                    k: 0,
                    tail: None,
                    count: 0,
                    done: units.is_empty(),
                }
            }
            _ => Mode::Plain {
                spans: break_lines(prep, fixed, units, avail, indent, MAX_FRAGMENTS),
                next: 0,
            },
        };
        LineSource {
            mode,
            avail,
            indent,
        }
    }

    /// Whether every line has been handed out.
    pub(crate) fn is_done(&self) -> bool {
        match &self.mode {
            Mode::Plain { spans, next } => *next >= spans.len(),
            Mode::Floats { done, .. } => *done,
        }
    }

    /// The next line, at or below `y` in the container's content box.
    pub(crate) fn next(
        &mut self,
        ctx: &LayoutCtx<'_>,
        prep: &Prepared,
        fixed: &[f32],
        units: &[Unit],
        y: f32,
    ) -> Option<NextLine> {
        let avail = self.avail;
        let indent = self.indent;
        match &mut self.mode {
            Mode::Plain { spans, next } => {
                let span = *spans.get(*next)?;
                *next += 1;
                Some(NextLine {
                    span,
                    x: 0.0,
                    width: avail,
                    y,
                })
            }
            Mode::Floats {
                origin,
                strut,
                k,
                tail,
                count,
                done,
            } => {
                if *done || *count >= MAX_FRAGMENTS {
                    *done = true;
                    return None;
                }
                let excl = &ctx.floats.bfcs.last()?.excl;
                let mut yy = y;
                let mut shifts = 0usize;
                let rest = units.get(*k..).unwrap_or(&[]);
                let first_indent = if *count == 0 { indent } else { 0.0 };
                let (span, band) = loop {
                    let band = excl.band(origin.y + yy, *strut, origin.x, origin.x + avail);
                    let w = band.width();
                    let lines = break_lines_from(prep, fixed, rest, *tail, w, first_indent, 1);
                    let Some(span) = lines.first().copied() else {
                        *done = true;
                        return None;
                    };
                    // CSS 2.2 §9.5: a shortened line too small for any
                    // content moves down until something fits or no float
                    // is beside it.
                    if band.constrained
                        && span.width > w + EPSILON
                        && shifts < MAX_LINE_SHIFTS
                        && let Some(n) = excl.next_change(origin.y + yy)
                    {
                        yy = n - origin.y;
                        shifts += 1;
                        continue;
                    }
                    break (span, band);
                };
                // Advance past the units on the line.
                let idx = *k + rest.partition_point(|u| u.start < span.end);
                let last = if idx > *k {
                    idx.checked_sub(1).and_then(|i| units.get(i)).copied()
                } else {
                    *tail
                };
                *tail = match last {
                    Some(u) if u.end > span.end => {
                        let split_item = span.item_end.saturating_sub(1);
                        Some(Unit {
                            start: span.end,
                            item_start: split_item,
                            width: span_width(
                                prep,
                                fixed,
                                span.end,
                                u.end,
                                split_item..u.item_end,
                                0.0,
                            ),
                            ..u
                        })
                    }
                    _ => None,
                };
                *k = idx;
                *count += 1;
                *done = *k >= units.len() && tail.is_none();
                Some(NextLine {
                    span,
                    x: band.lo - origin.x,
                    width: band.width(),
                    y: yy,
                })
            }
        }
    }
}
