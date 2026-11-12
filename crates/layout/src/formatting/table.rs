//! Table layout (CSS 2.2 §17).
//!
//! M1 follow-up: the table module (anonymous table objects §17.2.1, auto
//! and fixed table layout §17.5.2, `colspan`/`rowspan`, border models)
//! replaces this function. Until then table and table-internal boxes are
//! laid out as block containers.

use super::{ContainingBlock, LayoutResult, WidthMode, block};
use crate::box_tree::BoxId;
use crate::context::LayoutCtx;

/// Lays out a table (or table-internal) box.
pub(crate) fn layout_table_box(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    mode: WidthMode,
) -> LayoutResult {
    // M1 follow-up: real table layout. Fall back to block layout.
    block::layout_block_box(ctx, id, cb, depth, mode)
}
