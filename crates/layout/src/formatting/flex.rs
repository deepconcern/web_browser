//! Flex layout (CSS Flexbox 1).
//!
//! M1 follow-up: the flex module (single- and multi-line, `flex-grow`/
//! `shrink`/`basis`, alignment) replaces this function. Until then flex
//! containers are laid out as block containers.

use super::{ContainingBlock, LayoutResult, WidthMode, block};
use crate::box_tree::BoxId;
use crate::context::LayoutCtx;

/// Lays out a flex container.
pub(crate) fn layout_flex_container(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    mode: WidthMode,
) -> LayoutResult {
    // M1 follow-up: real flex layout. Fall back to block layout.
    block::layout_block_box(ctx, id, cb, depth, mode)
}
