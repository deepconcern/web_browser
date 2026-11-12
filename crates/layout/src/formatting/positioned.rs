//! Positioning schemes (CSS 2.2 §9.3, §9.4.3, §10.3.7, §10.6.4; CSS
//! Position 3).
//!
//! Relative positioning is implemented: [`apply_relative`] offsets a laid
//! out fragment (and so its descendants) without affecting other boxes.
//!
//! M1 follow-up: absolute and fixed positioning. [`is_absolutely_positioned`]
//! is the hook; until it returns `true`, such boxes are laid out as if
//! `position: static` (the style system has blockified them). The
//! module will collect them per containing block and lay them out after
//! the containing block's size is known, from their static position.

use wb_style::{ComputedStyle, Position};

use super::ContainingBlock;
use crate::box_tree::LayoutBox;
use crate::context::LayoutCtx;
use crate::fragment::FragmentId;
use crate::geom::{Point, sane};

/// Whether `b` is laid out out of flow by the positioning module.
pub(crate) fn is_absolutely_positioned(b: &LayoutBox) -> bool {
    // M1 follow-up: `b.style.is_absolutely_positioned()` once absolute and
    // fixed positioning are implemented.
    let _ = b;
    false
}

/// The offset of a relatively positioned box (CSS 2.2 §9.4.3): `left`
/// wins over `right` and `top` over `bottom` when both are set (the
/// direction is left-to-right in M1). Percentages of `top`/`bottom` refer
/// to the containing block's height and compute to `auto` when it is not
/// definite (§9.3.2). `sticky` is treated as `relative`.
// M1 follow-up: real sticky positioning.
pub(crate) fn relative_offset(style: &ComputedStyle, cb: &ContainingBlock) -> Point {
    if !matches!(style.position, Position::Relative | Position::Sticky) {
        return Point::ZERO;
    }
    let inset = &style.inset;
    let left = inset.left.resolve(cb.width);
    let right = inset.right.resolve(cb.width);
    let vertical = |v: &wb_style::LengthPercentageOrAuto| match v {
        wb_style::LengthPercentageOrAuto::Auto => None,
        wb_style::LengthPercentageOrAuto::LengthPercentage(lp) => {
            if lp.has_percentage() {
                cb.height.map(|h| lp.resolve(h))
            } else {
                Some(lp.resolve(0.0))
            }
        }
    };
    let top = vertical(&inset.top);
    let bottom = vertical(&inset.bottom);
    let dx = match (left, right) {
        (Some(l), _) => l,
        (None, Some(r)) => -r,
        (None, None) => 0.0,
    };
    let dy = match (top, bottom) {
        (Some(t), _) => t,
        (None, Some(b)) => -b,
        (None, None) => 0.0,
    };
    Point::new(sane(dx), sane(dy))
}

/// Records the relative-positioning offset of `style` on fragment `f`.
pub(crate) fn apply_relative(
    ctx: &mut LayoutCtx<'_>,
    f: FragmentId,
    style: &ComputedStyle,
    cb: &ContainingBlock,
) {
    let off = relative_offset(style, cb);
    if off != Point::ZERO
        && let Some(fr) = ctx.frag_mut(f)
    {
        fr.relative_offset = off;
    }
}
