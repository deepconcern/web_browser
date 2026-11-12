//! Box dimensions: margins, borders and padding (CSS 2.2 §8), widths
//! (§10.2–§10.4), heights (§10.5–§10.7), `box-sizing` (CSS Sizing 3 §4.4)
//! and intrinsic size contributions (CSS Sizing 3 §5).

use wb_style::{BoxSizing, ComputedStyle, LengthPercentageOrAuto, MaxSize, Size as CssSize};

use super::{IntrinsicSizes, WidthMode, inline, intrinsic_sizes, replaced};
use crate::box_tree::{BoxId, BoxKind};
use crate::context::LayoutCtx;
use crate::geom::{Edges, MAX_COORD, sane, sane_non_negative};

/// Margins with `auto` kept as `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Margins {
    pub top: Option<f32>,
    pub right: Option<f32>,
    pub bottom: Option<f32>,
    pub left: Option<f32>,
}

impl Margins {
    /// The margins with `auto` as 0.
    pub(crate) fn or_zero(&self) -> Edges {
        Edges::new(
            self.top.unwrap_or(0.0),
            self.right.unwrap_or(0.0),
            self.bottom.unwrap_or(0.0),
            self.left.unwrap_or(0.0),
        )
    }
}

/// A box's resolved margins, border widths and padding.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BoxEdges {
    pub margin: Margins,
    pub border: Edges,
    pub padding: Edges,
}

impl BoxEdges {
    /// Border plus padding.
    pub(crate) fn border_padding(&self) -> Edges {
        self.border.add(&self.padding)
    }
}

/// Resolves margins, borders and padding. Percentages of every side refer
/// to the containing block's width (CSS 2.2 §8.3, §8.4).
pub(crate) fn box_edges(style: &ComputedStyle, cb_width: f32) -> BoxEdges {
    let m = |v: &LengthPercentageOrAuto| v.resolve(cb_width).map(sane);
    let p = |v: &wb_style::LengthPercentage| sane_non_negative(v.resolve(cb_width));
    let b = |s: &wb_style::BorderSide| {
        if s.is_visible() {
            sane_non_negative(s.width)
        } else {
            0.0
        }
    };
    BoxEdges {
        margin: Margins {
            top: m(&style.margin.top),
            right: m(&style.margin.right),
            bottom: m(&style.margin.bottom),
            left: m(&style.margin.left),
        },
        border: Edges::new(
            b(&style.border.top),
            b(&style.border.right),
            b(&style.border.bottom),
            b(&style.border.left),
        ),
        padding: Edges::new(
            p(&style.padding.top),
            p(&style.padding.right),
            p(&style.padding.bottom),
            p(&style.padding.left),
        ),
    }
}

/// A `width`-like value, in content-box px when it is a length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SizeSpec {
    Auto,
    Px(f32),
    MinContent,
    MaxContent,
    FitContent(Option<f32>),
}

/// Converts a specified size to the content box (`box-sizing`).
fn to_content(style: &ComputedStyle, v: f32, bp: f32) -> f32 {
    let v = sane_non_negative(v);
    match style.box_sizing {
        BoxSizing::ContentBox => v,
        BoxSizing::BorderBox => (v - bp).max(0.0),
    }
}

/// The specified `width` for a containing block width (`None` when the
/// percentage basis is unknown, e.g. for intrinsic sizing).
pub(crate) fn width_spec(style: &ComputedStyle, cb_width: Option<f32>, bp: f32) -> SizeSpec {
    size_spec(style, &style.width, cb_width, bp)
}

fn size_spec(style: &ComputedStyle, s: &CssSize, basis: Option<f32>, bp: f32) -> SizeSpec {
    match s {
        CssSize::Auto => SizeSpec::Auto,
        CssSize::LengthPercentage(lp) => match (lp.has_percentage(), basis) {
            (true, None) => SizeSpec::Auto,
            (_, b) => SizeSpec::Px(to_content(style, lp.resolve(b.unwrap_or(0.0)), bp)),
        },
        CssSize::MinContent => SizeSpec::MinContent,
        CssSize::MaxContent => SizeSpec::MaxContent,
        CssSize::FitContent(limit) => {
            SizeSpec::FitContent(
                limit
                    .as_ref()
                    .and_then(|lp| match (lp.has_percentage(), basis) {
                        (true, None) => None,
                        (_, b) => Some(to_content(style, lp.resolve(b.unwrap_or(0.0)), bp)),
                    }),
            )
        }
    }
}

/// The specified content-box height, if definite (CSS 2.2 §10.5:
/// percentages need a definite containing block height).
pub(crate) fn height_px(style: &ComputedStyle, cb_height: Option<f32>, bp: f32) -> Option<f32> {
    match size_spec(style, &style.height, cb_height, bp) {
        SizeSpec::Px(v) => Some(v),
        // M1 follow-up: intrinsic keywords for heights behave as `auto`.
        _ => None,
    }
}

/// The used `min-width` / `min-height` in content-box px (`auto` is 0 for
/// block layout, CSS Sizing 3 §4.5).
pub(crate) fn min_size(style: &ComputedStyle, s: &CssSize, basis: Option<f32>, bp: f32) -> f32 {
    match size_spec(style, s, basis, bp) {
        SizeSpec::Px(v) => v,
        // M1 follow-up: intrinsic keywords in min sizes.
        _ => 0.0,
    }
}

/// The used `max-width` / `max-height` in content-box px (`none` is
/// unbounded, [`MAX_COORD`]).
pub(crate) fn max_size(style: &ComputedStyle, s: &MaxSize, basis: Option<f32>, bp: f32) -> f32 {
    match s {
        MaxSize::LengthPercentage(lp) => match (lp.has_percentage(), basis) {
            (true, None) => MAX_COORD,
            (_, b) => to_content(style, lp.resolve(b.unwrap_or(0.0)), bp),
        },
        // M1 follow-up: intrinsic keywords in max sizes.
        _ => MAX_COORD,
    }
}

/// Clamps `v` to `[min, max]`, with `min` winning (CSS 2.2 §10.4).
pub(crate) fn clamp_min_max(v: f32, min: f32, max: f32) -> f32 {
    v.min(max).max(min)
}

/// The used width and horizontal margins of a non-replaced block-level box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct HorizontalLayout {
    pub content_width: f32,
    pub margin_left: f32,
    pub margin_right: f32,
}

/// Solves the horizontal equation for a block-level box (CSS 2.2
/// §10.3.3; §10.3.9 shrink-to-fit for inline-blocks), with `min-width`
/// and `max-width` (§10.4).
pub(crate) fn solve_width(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    style: &ComputedStyle,
    cb_width: f32,
    edges: &BoxEdges,
    mode: WidthMode,
    depth: usize,
) -> HorizontalLayout {
    let bp = edges.border_padding().horizontal();
    let ml = edges.margin.left;
    let mr = edges.margin.right;
    let available = sane_non_negative(cb_width - ml.unwrap_or(0.0) - mr.unwrap_or(0.0) - bp);
    let intrinsic = |ctx: &mut LayoutCtx<'_>| {
        let s = intrinsic_sizes(ctx, id, depth);
        // Contributions include margins, border and padding; strip them.
        let outer = ml.unwrap_or(0.0) + mr.unwrap_or(0.0) + bp;
        ((s.min - outer).max(0.0), (s.max - outer).max(0.0))
    };
    let spec = width_spec(style, Some(cb_width), bp);
    let fit = |min: f32, max: f32, avail: f32| min.max(avail).min(max);
    let tentative = match spec {
        SizeSpec::Px(w) => Some(w),
        SizeSpec::Auto => match mode {
            WidthMode::Stretch => None,
            WidthMode::ShrinkToFit => {
                let (min, max) = intrinsic(ctx);
                Some(fit(min, max, available))
            }
        },
        SizeSpec::MinContent => Some(intrinsic(ctx).0),
        SizeSpec::MaxContent => Some(intrinsic(ctx).1),
        SizeSpec::FitContent(limit) => {
            let (min, max) = intrinsic(ctx);
            Some(fit(min, max, limit.map_or(available, |l| l.min(available))))
        }
    };
    let min = min_size(style, &style.min_width, Some(cb_width), bp);
    let max = max_size(style, &style.max_width, Some(cb_width), bp);
    let width = match tentative {
        Some(w) => clamp_min_max(w, min, max),
        None => {
            let w = available;
            if w > max || w < min {
                clamp_min_max(w, min, max)
            } else {
                // Auto width: auto margins are 0 and the width fills.
                return HorizontalLayout {
                    content_width: w,
                    margin_left: ml.unwrap_or(0.0),
                    margin_right: mr.unwrap_or(0.0),
                };
            }
        }
    };
    let width = sane_non_negative(width);
    if mode == WidthMode::ShrinkToFit {
        return HorizontalLayout {
            content_width: width,
            margin_left: ml.unwrap_or(0.0),
            margin_right: mr.unwrap_or(0.0),
        };
    }
    let (margin_left, margin_right) = solve_margins(cb_width, width + bp, ml, mr);
    HorizontalLayout {
        content_width: width,
        margin_left,
        margin_right,
    }
}

/// CSS 2.2 §10.3.3 with a non-`auto` width: auto margins share the free
/// space (treated as 0 when the box overflows); an over-constrained
/// equation adjusts `margin-right` (left-to-right).
pub(crate) fn solve_margins(
    cb_width: f32,
    border_box_width: f32,
    ml: Option<f32>,
    mr: Option<f32>,
) -> (f32, f32) {
    let free = cb_width - border_box_width - ml.unwrap_or(0.0) - mr.unwrap_or(0.0);
    let (l, r) = match (ml, mr) {
        (None, None) => {
            if free < 0.0 {
                (0.0, free)
            } else {
                (free / 2.0, free / 2.0)
            }
        }
        (None, Some(r)) => (free.max(0.0), if free < 0.0 { r + free } else { r }),
        (Some(l), None) => (l, free),
        (Some(l), Some(_)) => (l, cb_width - border_box_width - l),
    };
    (sane(l), sane(r))
}

/// The used content height of a box from its content height and style
/// (CSS 2.2 §10.6.3, §10.7).
pub(crate) fn used_height(
    style: &ComputedStyle,
    content_height: f32,
    cb_height: Option<f32>,
    bp: f32,
) -> f32 {
    let h = height_px(style, cb_height, bp).unwrap_or(content_height);
    let min = min_size(style, &style.min_height, cb_height, bp);
    let max = max_size(style, &style.max_height, cb_height, bp);
    sane_non_negative(clamp_min_max(h, min, max))
}

/// Computes a box's min-/max-content contributions (CSS Sizing 3 §5.1):
/// outer sizes with margins, border and padding. Percentages behave as
/// `auto` (cyclic) and percentage margins/padding as 0.
pub(crate) fn compute_intrinsic(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    depth: usize,
) -> IntrinsicSizes {
    let Some(b) = ctx.get(id) else {
        return IntrinsicSizes::default();
    };
    let style = &b.style;
    let edges = box_edges(style, 0.0);
    let bp = edges.border_padding().horizontal();
    let outer = bp + edges.margin.left.unwrap_or(0.0) + edges.margin.right.unwrap_or(0.0);
    let min_w = min_size(style, &style.min_width, None, bp);
    let max_w = max_size(style, &style.max_width, None, bp);
    let content = match &b.kind {
        BoxKind::Replaced(_) => {
            let w = replaced::intrinsic_width(ctx, id);
            IntrinsicSizes { min: w, max: w }
        }
        BoxKind::Container {
            inline_children, ..
        } => match width_spec(style, None, bp) {
            SizeSpec::Px(w) => IntrinsicSizes { min: w, max: w },
            _ => {
                let c = if *inline_children {
                    inline::intrinsic_content(ctx, id, depth)
                } else {
                    let mut acc = IntrinsicSizes::default();
                    for c in &b.children {
                        let s = intrinsic_sizes(ctx, *c, depth + 1);
                        acc.min = acc.min.max(s.min);
                        acc.max = acc.max.max(s.max);
                    }
                    acc
                };
                let c = IntrinsicSizes {
                    min: c.min,
                    max: c.max.max(c.min),
                };
                match width_spec(style, None, bp) {
                    SizeSpec::MinContent => IntrinsicSizes {
                        min: c.min,
                        max: c.min,
                    },
                    SizeSpec::MaxContent => IntrinsicSizes {
                        min: c.max,
                        max: c.max,
                    },
                    _ => c,
                }
            }
        },
        _ => IntrinsicSizes::default(),
    };
    let clamp = |v: f32| clamp_min_max(v, min_w, max_w);
    IntrinsicSizes {
        min: sane_non_negative(clamp(content.min) + outer),
        max: sane_non_negative(clamp(content.max) + outer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margins_equation() {
        // Both auto: centred.
        assert_eq!(solve_margins(100.0, 60.0, None, None), (20.0, 20.0));
        // Overflowing: auto margins are 0 and the right one absorbs.
        assert_eq!(solve_margins(100.0, 120.0, None, None), (0.0, -20.0));
        // Left auto.
        assert_eq!(solve_margins(100.0, 60.0, None, Some(10.0)), (30.0, 10.0));
        // Over-constrained: margin-right is recomputed.
        assert_eq!(
            solve_margins(100.0, 60.0, Some(10.0), Some(10.0)),
            (10.0, 30.0)
        );
    }

    #[test]
    fn min_wins_over_max() {
        assert_eq!(clamp_min_max(50.0, 80.0, 60.0), 80.0);
        assert_eq!(clamp_min_max(50.0, 10.0, 40.0), 40.0);
    }
}
