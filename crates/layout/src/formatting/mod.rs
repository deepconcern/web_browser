//! Formatting contexts and the per-box layout dispatch.
//!
//! [`layout_block_level`] and [`layout_atomic_inline`] are the entry
//! points every formatting context uses to lay out a child box: they pick
//! the algorithm from the box's kind and [`Formatting`]. Each algorithm
//! returns a [`LayoutResult`] whose fragment has its border box at the
//! origin; the parent positions it with [`LayoutCtx::set_position`].
//!
//! Formatting modules:
//!
//! - `block` and `inline`: block and inline formatting contexts.
//! - `table`: [`Formatting::Table`] / [`Formatting::TableInternal`]
//!   (currently block layout).
//! - `flex`: [`Formatting::Flex`], flex layout.
//! - `float`: floats and `clear` in block formatting contexts; line boxes
//!   ask it for the room beside floats.
//! - `positioned`: relative, absolute and fixed positioning. Block and
//!   flex layout hand absolutely positioned children to it; positioned
//!   containers lay them out once their size is known.

pub(crate) mod block;
pub(crate) mod flex;
pub(crate) mod float;
pub(crate) mod inline;
pub(crate) mod marker;
pub(crate) mod positioned;
pub(crate) mod replaced;
pub(crate) mod sizing;
pub(crate) mod table;

use crate::box_tree::{BoxId, BoxKind, Formatting};
use crate::context::LayoutCtx;
use crate::fragment::FragmentId;
use crate::geom::{Edges, Point, Size, sane};

/// The containing block a box is laid out in (CSS 2.2 §10.1): the content
/// box size of the parent block container.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContainingBlock {
    /// The width, always definite.
    pub width: f32,
    /// The height, when definite (percentage heights resolve against it).
    pub height: Option<f32>,
}

/// Adjoining vertical margins being collapsed (CSS 2.2 §8.3.1): the
/// largest positive and the most negative margin seen. The collapsed
/// margin is their sum.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CollapsedMargin {
    positive: f32,
    negative: f32,
}

impl CollapsedMargin {
    /// A single margin.
    pub(crate) fn new(m: f32) -> Self {
        let m = sane(m);
        CollapsedMargin {
            positive: m.max(0.0),
            negative: m.min(0.0),
        }
    }

    /// Collapses `other` into `self`.
    pub(crate) fn adjoin(self, other: CollapsedMargin) -> Self {
        CollapsedMargin {
            positive: self.positive.max(other.positive),
            negative: self.negative.min(other.negative),
        }
    }

    /// The resulting margin.
    pub(crate) fn resolve(self) -> f32 {
        self.positive + self.negative
    }
}

/// The outcome of laying out one box.
#[derive(Clone, Debug, Default)]
pub(crate) struct LayoutResult {
    /// The box's fragment, with its border box at the origin. `None` when
    /// the box produced nothing (or the fragment budget ran out).
    pub fragment: Option<FragmentId>,
    /// The border-box size.
    pub size: Size,
    /// The used margins. For block-level boxes in a block formatting
    /// context the vertical margins take part in collapsing through
    /// [`LayoutResult::collapsed_top`] and [`LayoutResult::collapsed_bottom`].
    pub margin: Edges,
    /// The margin at the box's top edge, collapsed with the margins of
    /// descendants adjoining it.
    pub collapsed_top: CollapsedMargin,
    /// The margin at the box's bottom edge, collapsed likewise.
    pub collapsed_bottom: CollapsedMargin,
    /// The box's top and bottom margins are adjoining (an empty box): its
    /// margins collapse through it (CSS 2.2 §8.3.1).
    pub collapses_through: bool,
    /// The y of the first line box's baseline from the border-box top.
    pub first_baseline: Option<f32>,
    /// The y of the last line box's baseline from the border-box top (the
    /// baseline of an `inline-block`, CSS 2.2 §10.8.1).
    pub last_baseline: Option<f32>,
}

impl LayoutResult {
    /// The margin-box size.
    pub(crate) fn margin_box(&self) -> Size {
        Size::new(
            self.size.width + self.margin.horizontal(),
            self.size.height + self.margin.vertical(),
        )
    }
}

/// Min-content and max-content contributions of a box (CSS Sizing 3 §5):
/// outer sizes, margins included.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct IntrinsicSizes {
    pub min: f32,
    pub max: f32,
}

/// How a block-level box's width is found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WidthMode {
    /// Fill the containing block (CSS 2.2 §10.3.3).
    Stretch,
    /// Shrink-to-fit (CSS 2.2 §10.3.5, §10.3.9): inline-blocks, floats,
    /// absolutely positioned boxes.
    ShrinkToFit,
}

/// Lays out the root box in the initial containing block and positions it
/// at its margins (CSS 2.2 §10.1; the root's margins never collapse).
pub(crate) fn layout_root(
    ctx: &mut LayoutCtx<'_>,
    root: BoxId,
    cb: &ContainingBlock,
) -> LayoutResult {
    let res = layout_block_level(ctx, root, cb, 0);
    if let Some(f) = res.fragment {
        ctx.set_position(f, Point::new(res.margin.left, res.margin.top));
        if let Some(b) = ctx.get(root) {
            positioned::apply_relative(ctx, f, &b.style, cb);
        }
        if let Some(fr) = ctx.frag_mut(f) {
            fr.stacking.creates_stacking_context = true;
        }
    }
    res
}

/// Lays out an in-flow block-level box for a containing block.
pub(crate) fn layout_block_level(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
) -> LayoutResult {
    layout_box(ctx, id, cb, depth, WidthMode::Stretch)
}

/// Lays out an atomic inline-level box (`inline-block`, inline replaced
/// elements, `inline-table`, `inline-flex`): shrink-to-fit width.
pub(crate) fn layout_atomic_inline(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
) -> LayoutResult {
    layout_box(ctx, id, cb, depth, WidthMode::ShrinkToFit)
}

fn layout_box(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    mode: WidthMode,
) -> LayoutResult {
    let Some(b) = ctx.get(id) else {
        return LayoutResult::default();
    };
    if ctx.exhausted() || depth > crate::box_tree::MAX_BOX_DEPTH * 2 {
        ctx.truncated = true;
        return LayoutResult::default();
    }
    match &b.kind {
        BoxKind::Replaced(_) => replaced::layout_replaced(ctx, id, cb),
        BoxKind::Container { formatting, .. } => match formatting {
            Formatting::Table | Formatting::TableInternal(_) => {
                table::layout_table_box(ctx, id, cb, depth, mode)
            }
            Formatting::Flex => flex::layout_flex_container(ctx, id, cb, depth, mode),
            // Grid falls back to block layout in M1 (issue #3).
            Formatting::Flow | Formatting::FlowRoot | Formatting::Grid => {
                block::layout_block_box(ctx, id, cb, depth, mode)
            }
        },
        // Text, line breaks and word-break opportunities are inline-level
        // and laid out by their inline formatting context.
        BoxKind::Text { .. } | BoxKind::LineBreak | BoxKind::WordBreakOpportunity => {
            LayoutResult::default()
        }
    }
}

/// The min-/max-content contributions of box `id` (cached).
pub(crate) fn intrinsic_sizes(ctx: &mut LayoutCtx<'_>, id: BoxId, depth: usize) -> IntrinsicSizes {
    if let Some(s) = ctx.intrinsic_cache.get(&id) {
        return *s;
    }
    let s = if depth > crate::box_tree::MAX_BOX_DEPTH * 2 {
        IntrinsicSizes::default()
    } else {
        sizing::compute_intrinsic(ctx, id, depth)
    };
    ctx.intrinsic_cache.insert(id, s);
    s
}
