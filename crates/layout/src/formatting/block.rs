//! Block formatting contexts (CSS 2.2 §9.4.1) and block container boxes:
//! widths and heights (§10.3.3, §10.6.3), margin collapsing (§8.3.1).

use super::sizing::{BoxEdges, box_edges, height_px, min_size, solve_width, used_height};
use super::{
    CollapsedMargin, ContainingBlock, LayoutResult, WidthMode, float, inline, layout_block_level,
    positioned,
};
use crate::box_tree::{BoxId, BoxKind, LayoutBox};
use crate::context::LayoutCtx;
use crate::fragment::{BoxFragmentKind, FragmentId, FragmentKind};
use crate::geom::{Edges, Point, Size, sane};

/// The laid out contents of a block container, in content-box
/// coordinates.
#[derive(Debug, Default)]
pub(crate) struct ContentLayout {
    /// Child fragments (block boxes or line boxes), already positioned.
    pub children: Vec<FragmentId>,
    /// The content height (CSS 2.2 §10.6.3).
    pub height: f32,
    /// Margins of leading children collapsed into the container's top
    /// margin (only when the container's top edge is open).
    pub top: CollapsedMargin,
    /// Margins of trailing children carried into the container's bottom
    /// margin (only when the container's bottom edge is open).
    pub bottom: CollapsedMargin,
    /// No child separated the container's top and bottom edges: no line
    /// boxes and every child collapsed through.
    pub empty: bool,
    /// Baselines from the content-box top.
    pub first_baseline: Option<f32>,
    pub last_baseline: Option<f32>,
}

/// Lays out a block container box (`display: block`, `flow-root`,
/// `inline-block`, and the fallbacks for table, flex and grid boxes).
pub(crate) fn layout_block_box(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    mode: WidthMode,
) -> LayoutResult {
    let Some(b) = ctx.get(id) else {
        return LayoutResult::default();
    };
    let style = b.style.clone();
    let edges = box_edges(&style, cb.width);
    let bp = edges.border_padding();
    let hl = solve_width(ctx, id, &style, cb.width, &edges, mode, depth);
    let content_width = hl.content_width;
    // A height imposed by the parent formatting context (flex items).
    let forced_height = ctx.flex.forced(id).height;
    let specified_height = height_px(&style, cb.height, bp.vertical());
    let child_cb = ContainingBlock {
        width: content_width,
        height: forced_height
            .or_else(|| specified_height.map(|h| used_height(&style, h, cb.height, bp.vertical()))),
    };
    let is_root = b.parent.is_none();
    let bfc = is_root || mode == WidthMode::ShrinkToFit || b.establishes_bfc();
    let top_open = !bfc && edges.border.top == 0.0 && edges.padding.top == 0.0;
    let min_h = min_size(&style, &style.min_height, cb.height, bp.vertical());
    let bottom_open = !bfc
        && edges.border.bottom == 0.0
        && edges.padding.bottom == 0.0
        && matches!(style.height, wb_style::Size::Auto)
        && min_h <= 0.0;

    let content = layout_contents(ctx, b, id, &child_cb, depth, top_open, bottom_open);
    let content_height = forced_height
        .unwrap_or_else(|| used_height(&style, content.height, cb.height, bp.vertical()));
    let collapses_through = content.empty && top_open && bottom_open && content_height <= 0.0;

    // Place the contents inside the padding and border.
    let offset = Point::new(bp.left, bp.top);
    for c in &content.children {
        ctx.translate(*c, offset);
    }
    let mut children = content.children;
    let size = Size::new(
        content_width + bp.horizontal(),
        content_height + bp.vertical(),
    );
    let first_baseline = content.first_baseline.map(|v| v + bp.top);
    if let Some(m) = b.marker
        && let Some(f) =
            inline::layout_outside_marker(ctx, m, bp.left, first_baseline, bp.top, depth)
    {
        children.push(f);
    }
    let fragment = ctx.box_fragment(
        b,
        Some(id),
        FragmentKind::Box(match b.level {
            crate::box_tree::Level::Block => BoxFragmentKind::Block,
            crate::box_tree::Level::Inline => BoxFragmentKind::AtomicInline,
        }),
        size,
        edges.border,
        edges.padding,
        children,
        is_root,
    );
    let mt = edges.margin.top.unwrap_or(0.0);
    let mb = edges.margin.bottom.unwrap_or(0.0);
    let collapsed_top = if top_open {
        CollapsedMargin::new(mt).adjoin(content.top)
    } else {
        CollapsedMargin::new(mt)
    };
    let collapsed_bottom = if bottom_open && !collapses_through {
        CollapsedMargin::new(mb).adjoin(content.bottom)
    } else {
        CollapsedMargin::new(mb)
    };
    LayoutResult {
        fragment,
        size,
        margin: Edges::new(mt, hl.margin_right, mb, hl.margin_left),
        collapsed_top,
        collapsed_bottom,
        collapses_through,
        first_baseline,
        last_baseline: content.last_baseline.map(|v| v + bp.top),
    }
}

/// Lays out the contents of a block container: an inline formatting
/// context when its children are inline-level, else its block-level
/// children.
pub(crate) fn layout_contents(
    ctx: &mut LayoutCtx<'_>,
    b: &LayoutBox,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    top_open: bool,
    bottom_open: bool,
) -> ContentLayout {
    match b.kind {
        BoxKind::Container {
            inline_children: true,
            ..
        } => {
            let ifc = inline::layout_inline_formatting_context(ctx, id, cb, depth);
            ContentLayout {
                children: ifc.lines,
                height: ifc.height,
                top: CollapsedMargin::default(),
                bottom: CollapsedMargin::default(),
                empty: !ifc.has_content,
                first_baseline: ifc.first_baseline,
                last_baseline: ifc.last_baseline,
            }
        }
        _ => layout_block_children(ctx, &b.children, cb, depth, top_open, bottom_open),
    }
}

/// Stacks block-level children vertically, collapsing adjoining margins
/// (CSS 2.2 §8.3.1). Linear in the number of children.
pub(crate) fn layout_block_children(
    ctx: &mut LayoutCtx<'_>,
    children: &[BoxId],
    cb: &ContainingBlock,
    depth: usize,
    top_open: bool,
    bottom_open: bool,
) -> ContentLayout {
    let mut out = ContentLayout {
        empty: true,
        ..ContentLayout::default()
    };
    let mut y = 0.0f32;
    let mut pending = CollapsedMargin::default();
    // Still at the container's top edge: child margins collapse into the
    // container's own top margin.
    let mut at_top = top_open;
    for &child in children {
        if ctx.exhausted() {
            ctx.truncated = true;
            break;
        }
        let Some(cbox) = ctx.get(child) else { continue };
        // Hook: floats and absolutely positioned boxes leave the flow here.
        // M1 follow-up: hand them to the float / positioning modules (both
        // predicates return `false` until then).
        if float::is_float(cbox) || positioned::is_absolutely_positioned(cbox) {
            continue;
        }
        let r = layout_block_level(ctx, child, cb, depth + 1);
        let Some(f) = r.fragment else { continue };
        positioned::apply_relative(ctx, f, &cbox.style, cb);
        if r.collapses_through {
            let combined = r.collapsed_top.adjoin(r.collapsed_bottom);
            let pos = if at_top {
                out.top = out.top.adjoin(combined);
                y
            } else {
                // §8.3.1: the box's top border edge is where it would be
                // if it had a non-zero bottom border.
                let p = y + pending.adjoin(r.collapsed_top).resolve();
                pending = pending.adjoin(combined);
                p
            };
            ctx.set_position(f, Point::new(r.margin.left, sane(pos)));
            out.children.push(f);
            continue;
        }
        out.empty = false;
        let child_y = if at_top {
            out.top = out.top.adjoin(r.collapsed_top);
            at_top = false;
            y
        } else {
            y + pending.adjoin(r.collapsed_top).resolve()
        };
        let child_y = sane(child_y);
        ctx.set_position(f, Point::new(r.margin.left, child_y));
        out.children.push(f);
        if out.first_baseline.is_none() {
            out.first_baseline = r.first_baseline.map(|v| v + child_y);
        }
        if let Some(v) = r.last_baseline {
            out.last_baseline = Some(v + child_y);
        }
        y = sane(child_y + r.size.height);
        pending = r.collapsed_bottom;
    }
    if at_top {
        // Every child collapsed through into the top margin.
        out.height = 0.0;
        return out;
    }
    if bottom_open {
        out.bottom = pending;
        out.height = y.max(0.0);
    } else {
        out.height = sane(y + pending.resolve()).max(0.0);
    }
    out
}

/// Exposed for other formatting contexts: the edges of a box for a
/// containing block width.
#[allow(dead_code)]
pub(crate) fn edges_for(b: &LayoutBox, cb: &ContainingBlock) -> BoxEdges {
    box_edges(&b.style, cb.width)
}
