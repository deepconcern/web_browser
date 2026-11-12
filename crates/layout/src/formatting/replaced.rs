//! Replaced elements (CSS 2.2 §10.3.2, §10.3.4, §10.6.2, §10.4's
//! constraint table; CSS Images 3 §5 default sizing) and form controls
//! laid out as replaced boxes with UA default sizes (HTML §15.5).

use super::sizing::{
    SizeSpec, box_edges, clamp_min_max, height_px, max_size, min_size, solve_margins, width_spec,
};
use super::{ContainingBlock, LayoutResult};
use crate::box_tree::{BoxId, BoxKind, FormControlKind, LayoutBox, Level, ReplacedKind};
use crate::context::LayoutCtx;
use crate::fragment::{FragmentKind, ReplacedFragment};
use crate::geom::{Edges, Size, sane_non_negative};

/// The default object size (CSS Images 3 §5.2, HTML §15.4.1).
const DEFAULT_WIDTH: f32 = 300.0;
const DEFAULT_HEIGHT: f32 = 150.0;

/// Natural dimensions: width, height, and aspect ratio (width / height).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Natural {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub ratio: Option<f32>,
}

/// The natural size of a replaced box.
pub(crate) fn natural_size(ctx: &mut LayoutCtx<'_>, b: &LayoutBox) -> Natural {
    let BoxKind::Replaced(kind) = &b.kind else {
        return Natural::default();
    };
    match kind {
        ReplacedKind::Image => {
            let size = b.node.and_then(|n| ctx.host.image_size(n));
            match size {
                Some(s) => {
                    let w = sane_non_negative(s.width);
                    let h = sane_non_negative(s.height);
                    Natural {
                        width: Some(w),
                        height: Some(h),
                        ratio: (w > 0.0 && h > 0.0).then(|| w / h),
                    }
                }
                // Not loaded (or broken): no natural size, laid out as
                // 0×0 unless sized by CSS.
                // M1 follow-up: render `alt` text for broken images.
                None => Natural {
                    width: Some(0.0),
                    height: Some(0.0),
                    ratio: None,
                },
            }
        }
        // M1 follow-up: `<canvas>`/`<video>` natural sizes from their
        // attributes and media.
        ReplacedKind::Embedded => Natural {
            width: Some(DEFAULT_WIDTH),
            height: Some(DEFAULT_HEIGHT),
            ratio: None,
        },
        ReplacedKind::FormControl(fc) => {
            let style = b.style.clone();
            let m = ctx.metrics(&style);
            let lh = m.line_height(&style);
            let ch = advance_of(ctx, "0", &style).max(1.0);
            let text_w = |ctx: &mut LayoutCtx<'_>| advance_of(ctx, &fc.text, &style);
            let cols = fc.cols.max(1) as f32;
            let rows = fc.rows.max(1) as f32;
            let (w, h) = match fc.kind {
                FormControlKind::TextField => (cols * ch, lh),
                FormControlKind::Button => (text_w(ctx), lh),
                FormControlKind::Checkbox | FormControlKind::Radio => (13.0, 13.0),
                FormControlKind::Range => (129.0, 16.0),
                FormControlKind::Color => (44.0, 23.0),
                FormControlKind::Select => (text_w(ctx).max(fc.cols as f32 * ch) + 20.0, lh),
                FormControlKind::ListBox => (fc.cols as f32 * ch + 20.0, rows * lh),
                FormControlKind::TextArea => (cols * ch, rows * lh),
                FormControlKind::Progress => (160.0, 16.0),
                FormControlKind::Meter => (80.0, 16.0),
            };
            Natural {
                width: Some(sane_non_negative(w)),
                height: Some(sane_non_negative(h)),
                ratio: None,
            }
        }
    }
}

/// The advance width of `text` in `style`.
fn advance_of(
    ctx: &mut LayoutCtx<'_>,
    text: &str,
    style: &std::sync::Arc<wb_style::ComputedStyle>,
) -> f32 {
    let t: String = text.chars().take(1024).collect();
    let shaped = ctx.text.shape(&t, style);
    sane_non_negative(shaped.clusters.iter().map(|(_, a)| a).sum())
}

/// The used content size of a replaced box (CSS 2.2 §10.3.2, §10.6.2 and
/// the §10.4 table).
pub(crate) fn used_size(
    style: &wb_style::ComputedStyle,
    natural: Natural,
    cb: &ContainingBlock,
    cb_width_known: bool,
    bp: Edges,
) -> Size {
    let basis = cb_width_known.then_some(cb.width);
    let sw = match width_spec(style, basis, bp.horizontal()) {
        SizeSpec::Px(w) => Some(w),
        // M1 follow-up: intrinsic size keywords on replaced elements.
        _ => None,
    };
    let sh = height_px(style, cb.height, bp.vertical());
    let ratio = natural.ratio.filter(|r| r.is_finite() && *r > 0.0);
    let min_w = min_size(style, &style.min_width, basis, bp.horizontal());
    let max_w = max_size(style, &style.max_width, basis, bp.horizontal()).max(min_w);
    let min_h = min_size(style, &style.min_height, cb.height, bp.vertical());
    let max_h = max_size(style, &style.max_height, cb.height, bp.vertical()).max(min_h);
    let (w, h) = match (sw, sh) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (
            w,
            ratio
                .map(|r| w / r)
                .or(natural.height)
                .unwrap_or(DEFAULT_HEIGHT),
        ),
        (None, Some(h)) => (
            ratio
                .map(|r| h * r)
                .or(natural.width)
                .unwrap_or(DEFAULT_WIDTH),
            h,
        ),
        (None, None) => {
            let (w, h) = match (natural.width, natural.height, ratio) {
                (Some(w), Some(h), _) => (w, h),
                (Some(w), None, Some(r)) => (w, w / r),
                (None, Some(h), Some(r)) => (h * r, h),
                (None, None, Some(r)) if cb_width_known => (cb.width, cb.width / r),
                (w, h, _) => (w.unwrap_or(DEFAULT_WIDTH), h.unwrap_or(DEFAULT_HEIGHT)),
            };
            if ratio.is_some() {
                let (w, h) = constrain_with_ratio(w, h, min_w, max_w, min_h, max_h);
                return Size::new(sane_non_negative(w), sane_non_negative(h));
            }
            (w, h)
        }
    };
    let w = clamp_min_max(w, min_w, max_w);
    let h = clamp_min_max(h, min_h, max_h);
    Size::new(sane_non_negative(w), sane_non_negative(h))
}

/// The CSS 2.2 §10.4 table for replaced elements with `auto` width and
/// height and an intrinsic ratio.
fn constrain_with_ratio(
    w: f32,
    h: f32,
    min_w: f32,
    max_w: f32,
    min_h: f32,
    max_h: f32,
) -> (f32, f32) {
    if w <= 0.0 || h <= 0.0 {
        return (
            clamp_min_max(w, min_w, max_w),
            clamp_min_max(h, min_h, max_h),
        );
    }
    let over_w = w > max_w;
    let under_w = w < min_w;
    let over_h = h > max_h;
    let under_h = h < min_h;
    match (over_w, under_w, over_h, under_h) {
        (false, false, false, false) => (w, h),
        (true, _, false, false) => (max_w, (max_w * h / w).max(min_h)),
        (_, true, false, false) => (min_w, (min_w * h / w).min(max_h)),
        (false, false, true, _) => ((max_h * w / h).max(min_w), max_h),
        (false, false, _, true) => ((min_h * w / h).min(max_w), min_h),
        (true, _, true, _) => {
            if max_w / w <= max_h / h {
                (max_w, (max_w * h / w).max(min_h))
            } else {
                ((max_h * w / h).max(min_w), max_h)
            }
        }
        (_, true, _, true) => {
            if min_w / w <= min_h / h {
                ((min_h * w / h).min(max_w), min_h)
            } else {
                (min_w, (min_w * h / w).min(max_h))
            }
        }
        (_, true, true, _) => (min_w, max_h),
        (true, _, _, true) => (max_w, min_h),
    }
}

/// The content width a replaced box contributes to intrinsic sizing.
pub(crate) fn intrinsic_width(ctx: &mut LayoutCtx<'_>, id: BoxId) -> f32 {
    let Some(b) = ctx.get(id) else { return 0.0 };
    let natural = natural_size(ctx, b);
    let edges = box_edges(&b.style, 0.0);
    let cb = ContainingBlock {
        width: 0.0,
        height: None,
    };
    used_size(&b.style, natural, &cb, false, edges.border_padding()).width
}

/// Lays out a replaced box (block-level or atomic inline).
pub(crate) fn layout_replaced(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
) -> LayoutResult {
    let Some(b) = ctx.get(id) else {
        return LayoutResult::default();
    };
    let style = b.style.clone();
    let edges = box_edges(&style, cb.width);
    let bp = edges.border_padding();
    let natural = natural_size(ctx, b);
    let content = used_size(&style, natural, cb, true, bp);
    let size = Size::new(
        content.width + bp.horizontal(),
        content.height + bp.vertical(),
    );
    let (ml, mr) = match b.level {
        // §10.3.4: block-level replaced boxes use the §10.3.3 margin rules.
        Level::Block => solve_margins(cb.width, size.width, edges.margin.left, edges.margin.right),
        Level::Inline => (
            edges.margin.left.unwrap_or(0.0),
            edges.margin.right.unwrap_or(0.0),
        ),
    };
    let mt = edges.margin.top.unwrap_or(0.0);
    let mb = edges.margin.bottom.unwrap_or(0.0);
    let BoxKind::Replaced(kind) = &b.kind else {
        return LayoutResult::default();
    };
    // Text-bearing controls align with the text around them by their
    // text's baseline; other replaced boxes use their bottom margin edge
    // (CSS 2.2 §10.8.1).
    let baseline = match kind {
        ReplacedKind::FormControl(fc)
            if matches!(
                fc.kind,
                FormControlKind::TextField
                    | FormControlKind::Button
                    | FormControlKind::Select
                    | FormControlKind::ListBox
                    | FormControlKind::TextArea
            ) =>
        {
            let m = ctx.metrics(&style);
            let lh = m.line_height(&style);
            let half_leading = (lh - (m.ascent + m.descent)) / 2.0;
            Some(bp.top + half_leading + m.ascent)
        }
        _ => None,
    };
    let fragment = ctx.box_fragment(
        b,
        Some(id),
        FragmentKind::Replaced(ReplacedFragment { kind: kind.clone() }),
        size,
        edges.border,
        edges.padding,
        Vec::new(),
        b.parent.is_none(),
    );
    LayoutResult {
        fragment,
        size,
        margin: Edges::new(mt, mr, mb, ml),
        collapsed_top: super::CollapsedMargin::new(mt),
        collapsed_bottom: super::CollapsedMargin::new(mb),
        collapses_through: false,
        first_baseline: baseline,
        last_baseline: baseline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_table() {
        // 400x200 image, max-width 100: scaled down preserving ratio.
        assert_eq!(
            constrain_with_ratio(400.0, 200.0, 0.0, 100.0, 0.0, f32::MAX),
            (100.0, 50.0)
        );
        // Both too large: the tighter constraint wins.
        assert_eq!(
            constrain_with_ratio(400.0, 200.0, 0.0, 200.0, 0.0, 50.0),
            (100.0, 50.0)
        );
        // Too small.
        assert_eq!(
            constrain_with_ratio(10.0, 20.0, 40.0, f32::MAX, 0.0, f32::MAX),
            (40.0, 80.0)
        );
    }
}
