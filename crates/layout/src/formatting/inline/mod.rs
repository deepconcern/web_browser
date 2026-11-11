//! Inline formatting contexts (CSS 2.2 §9.4.2, §10.8; CSS Inline 3; CSS
//! Text 3).
//!
//! A block container whose children are inline-level lays them out in
//! line boxes:
//!
//! 1. [`prepare`] flattens the inline content into one paragraph of items
//!    (text, inline box starts and ends, atomic inlines, forced breaks),
//!    processes white space, shapes text and finds break opportunities.
//!    The result is cached per container.
//! 2. [`lines`] packs the paragraph into lines.
//! 3. This module builds each line box: horizontal positions with
//!    `text-align`/`text-indent`, inline box fragments per line (with their
//!    start/end margins, borders and padding only on the first/last piece),
//!    and vertical alignment (`vertical-align`, half-leading).

pub(crate) mod lines;
pub(crate) mod prepare;

use std::rc::Rc;
use std::sync::Arc;

use wb_style::{ComputedStyle, TextAlign, VerticalAlign, VerticalAlignKeyword};

use self::lines::{LineSpan, build_units, span_width, tab_advance, trailing_hang};
use self::prepare::ItemKind;
pub(crate) use self::prepare::Prepared;
use super::sizing::box_edges;
use super::{
    ContainingBlock, IntrinsicSizes, LayoutResult, float, intrinsic_sizes, layout_atomic_inline,
    positioned,
};
use crate::box_tree::{BoxId, LayoutBox};
use crate::context::LayoutCtx;
use crate::fragment::{
    BoxFragmentKind, Fragment, FragmentId, FragmentKind, GlyphRun, LineFragment, PositionedGlyph,
    Stacking, TextFragment,
};
use crate::geom::{Edges, MAX_COORD, Point, Rect, Size, sane, sane_non_negative};
use crate::text::font_size;

/// The result of laying out an inline formatting context.
#[derive(Debug, Default)]
pub(crate) struct InlineLayout {
    /// Line box fragments, positioned in the container's content box.
    pub lines: Vec<FragmentId>,
    /// The total height of the line boxes.
    pub height: f32,
    /// At least one line box has content (CSS 2.2 §9.4.2: lines without
    /// content are zero-height and ignored for margin collapsing).
    pub has_content: bool,
    pub first_baseline: Option<f32>,
    pub last_baseline: Option<f32>,
    /// The widest line's content advance.
    pub max_line_width: f32,
}

fn prepared(ctx: &mut LayoutCtx<'_>, id: BoxId) -> Rc<Prepared> {
    if let Some(p) = ctx.inline_cache.get(&id) {
        return p.clone();
    }
    let p = Rc::new(prepare::prepare(ctx, id));
    ctx.inline_cache.insert(id, p.clone());
    p
}

/// The start (left) and end (right) inline edges of an inline box:
/// margin + border + padding, honouring split pieces.
fn inline_edges(b: &LayoutBox, cb_width: f32) -> (Edges, Edges, Edges) {
    let e = box_edges(&b.style, cb_width);
    let margin = e.margin.or_zero();
    (margin, e.border, e.padding)
}

/// Widths of open/close edges for every item (`cb_width` resolves
/// percentages; 0 for intrinsic sizing).
fn edge_widths(ctx: &LayoutCtx<'_>, prep: &Prepared, cb_width: f32, fixed: &mut [f32]) {
    for (i, it) in prep.items.iter().enumerate() {
        let w = match it.kind {
            ItemKind::Open | ItemKind::Close => {
                let Some(b) = ctx.get(it.box_id) else {
                    continue;
                };
                let (m, bo, p) = inline_edges(b, cb_width);
                if matches!(it.kind, ItemKind::Open) {
                    if b.split.start {
                        m.left + bo.left + p.left
                    } else {
                        0.0
                    }
                } else if b.split.end {
                    m.right + bo.right + p.right
                } else {
                    0.0
                }
            }
            _ => continue,
        };
        if let Some(slot) = fixed.get_mut(i) {
            *slot = sane(w);
        }
    }
}

/// The min-/max-content widths of the inline content of container `id`
/// (CSS Sizing 3 §5: the longest unbreakable unit, and the longest line
/// with only forced breaks).
pub(crate) fn intrinsic_content(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    depth: usize,
) -> IntrinsicSizes {
    let prep = prepared(ctx, id);
    let mut fixed_min = vec![0.0f32; prep.items.len()];
    let mut fixed_max = vec![0.0f32; prep.items.len()];
    edge_widths(ctx, &prep, 0.0, &mut fixed_min);
    edge_widths(ctx, &prep, 0.0, &mut fixed_max);
    for (i, it) in prep.items.iter().enumerate() {
        if matches!(it.kind, ItemKind::Atomic) {
            let s = intrinsic_sizes(ctx, it.box_id, depth + 1);
            if let Some(v) = fixed_min.get_mut(i) {
                *v = s.min;
            }
            if let Some(v) = fixed_max.get_mut(i) {
                *v = s.max;
            }
        }
    }
    let indent = ctx
        .get(id)
        .map_or(0.0, |b| sane(b.style.text_indent.resolve(0.0)));
    let units_min = build_units(&prep, &fixed_min);
    let mut min = 0.0f32;
    for u in &units_min {
        let w = if u.has_tab {
            span_width(
                &prep,
                &fixed_min,
                u.start,
                u.end,
                u.item_start..u.item_end,
                0.0,
            )
        } else {
            u.width
        };
        min = min.max(w - u.hang);
    }
    let units_max = build_units(&prep, &fixed_max);
    let mut max = 0.0f32;
    let mut x = indent;
    let mut hang = 0.0;
    for u in &units_max {
        let w = if u.has_tab {
            span_width(
                &prep,
                &fixed_max,
                u.start,
                u.end,
                u.item_start..u.item_end,
                x,
            )
        } else {
            u.width
        };
        x += w;
        hang = u.hang;
        if u.mandatory {
            max = max.max(x - hang);
            x = 0.0;
            hang = 0.0;
        }
    }
    max = max.max(x - hang);
    IntrinsicSizes {
        min: sane_non_negative(min),
        max: sane_non_negative(max),
    }
}

/// One box taking part in a line's vertical alignment.
#[derive(Clone, Copy, Debug)]
struct VNode {
    parent: usize,
    /// Distances above and below the baseline of the box's content area
    /// (for atomics: of its margin box).
    ascent: f32,
    descent: f32,
    half_leading: f32,
    align: VAlign,
    font_size: f32,
    x_height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum VAlign {
    /// Baseline shift (positive = down) relative to the parent's baseline.
    Shift(f32),
    Top,
    Bottom,
}

/// A fragment created on the current line whose vertical position waits
/// for the line's alignment.
#[derive(Clone, Copy, Debug)]
enum Pending {
    /// An inline box piece: content area around its node's baseline.
    InlineBox {
        frag: FragmentId,
        node: usize,
        top_extra: f32,
    },
    /// Text on the baseline of `node`.
    Text { frag: FragmentId, node: usize },
    /// An atomic inline whose margin-box top is at `baseline - ascent`.
    Atomic {
        frag: FragmentId,
        node: usize,
        margin_top: f32,
    },
}

/// An inline box open on the line being built.
#[derive(Debug)]
struct OpenBox {
    box_id: BoxId,
    node: usize,
    /// Border-box start x.
    start: f32,
    has_start: bool,
    children: Vec<FragmentId>,
}

/// Lays out the inline formatting context of block container `id` whose
/// content box is `cb.width` wide.
pub(crate) fn layout_inline_formatting_context(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
) -> InlineLayout {
    let Some(container) = ctx.get(id) else {
        return InlineLayout::default();
    };
    let prep = prepared(ctx, id);
    let avail = sane_non_negative(cb.width);
    let mut fixed = vec![0.0f32; prep.items.len()];
    edge_widths(ctx, &prep, avail, &mut fixed);
    // Lay out atomic inlines first: their widths decide line breaks.
    let mut atomics: Vec<Option<LayoutResult>> = Vec::with_capacity(prep.items.len());
    for (i, it) in prep.items.iter().enumerate() {
        if matches!(it.kind, ItemKind::Atomic) && !ctx.exhausted() {
            let r = layout_atomic_inline(ctx, it.box_id, cb, depth + 1);
            if let Some(f) = r.fragment {
                positioned::apply_relative(ctx, f, &it.style, cb);
            }
            if let Some(slot) = fixed.get_mut(i) {
                *slot = r.margin_box().width;
            }
            atomics.push(Some(r));
        } else {
            atomics.push(None);
        }
    }
    let style = container.style.clone();
    let indent = sane(style.text_indent.resolve(avail));
    let units = build_units(&prep, &fixed);
    // Lines come one at a time: next to floats each gets its own width.
    let mut source = float::LineSource::new(ctx, id, &prep, &fixed, &units, avail, indent);

    let mut out = InlineLayout::default();
    let mut y = 0.0f32;
    // Inline boxes still open at the end of the previous line.
    let mut carry: Vec<BoxId> = Vec::new();
    let mut n = 0usize;
    while let Some(next) = source.next(ctx, &prep, &fixed, &units, y) {
        if ctx.exhausted() {
            ctx.truncated = true;
            break;
        }
        let span = &next.span;
        let is_last = source.is_done();
        y = next.y;
        let built = build_line(
            ctx,
            &prep,
            &fixed,
            &atomics,
            container,
            span,
            if n == 0 { indent } else { 0.0 },
            next.width,
            is_last,
            cb,
            &mut carry,
        );
        n += 1;
        let Some(line) = built else { continue };
        ctx.set_position(line.frag, Point::new(next.x, y));
        out.lines.push(line.frag);
        if line.content {
            out.has_content = true;
            if out.first_baseline.is_none() {
                out.first_baseline = Some(y + line.baseline);
            }
            out.last_baseline = Some(y + line.baseline);
        }
        out.max_line_width = out.max_line_width.max(span.width);
        y = sane(y + line.height);
    }
    out.height = y;
    out
}

struct BuiltLine {
    frag: FragmentId,
    height: f32,
    baseline: f32,
    content: bool,
}

/// The vertical metrics node for an inline box's style.
fn style_vnode(
    ctx: &mut LayoutCtx<'_>,
    style: &Arc<ComputedStyle>,
    parent: usize,
    nodes: &[VNode],
) -> VNode {
    let m = ctx.metrics(style);
    let lh = m.line_height(style);
    let half = (lh - (m.ascent + m.descent)) / 2.0;
    let fs = font_size(style);
    let align = vertical_align(style, nodes.get(parent), m.ascent, m.descent, half, lh);
    VNode {
        parent,
        ascent: m.ascent,
        descent: m.descent,
        half_leading: half,
        align,
        font_size: fs,
        x_height: m.x_height,
    }
}

/// The baseline shift for `vertical-align` (CSS 2.2 §10.8.1). `a`, `d`
/// and `half` describe the aligned box; `parent` the parent inline box.
fn vertical_align(
    style: &ComputedStyle,
    parent: Option<&VNode>,
    a: f32,
    d: f32,
    half: f32,
    own_line_height: f32,
) -> VAlign {
    let Some(p) = parent else {
        return VAlign::Shift(0.0);
    };
    let shift = match &style.vertical_align {
        VerticalAlign::Keyword(k) => match k {
            VerticalAlignKeyword::Baseline => 0.0,
            // UA-chosen offsets: about a fifth and a third of the parent's
            // font size.
            VerticalAlignKeyword::Sub => p.font_size * 0.2,
            VerticalAlignKeyword::Super => -p.font_size * 0.34,
            VerticalAlignKeyword::Middle => -p.x_height / 2.0 - (d - a) / 2.0,
            VerticalAlignKeyword::TextTop => a + half - p.ascent,
            VerticalAlignKeyword::TextBottom => p.descent - d - half,
            VerticalAlignKeyword::Top => return VAlign::Top,
            VerticalAlignKeyword::Bottom => return VAlign::Bottom,
        },
        VerticalAlign::LengthPercentage(lp) => -lp.resolve(own_line_height),
    };
    VAlign::Shift(sane(shift))
}

/// Builds one line box.
#[allow(clippy::too_many_arguments)]
fn build_line(
    ctx: &mut LayoutCtx<'_>,
    prep: &Prepared,
    fixed: &[f32],
    atomics: &[Option<LayoutResult>],
    container: &LayoutBox,
    span: &LineSpan,
    indent: f32,
    avail: f32,
    is_last: bool,
    cb: &ContainingBlock,
    carry: &mut Vec<BoxId>,
) -> Option<BuiltLine> {
    let root_style = container.style.clone();
    // Trim trailing hanging/collapsible spaces, and collapsible spaces at
    // the start of the line (CSS Text 3 §4.1.2).
    let (trim_end, _) = trailing_hang(prep, span.start, span.end, span.item_start, span.item_end);
    let lead = leading_trim(prep, span);

    // text-align (CSS Text 3 §6.1); `justify` (§6.4) stretches spaces on
    // every line but the last and those ending in a forced break.
    let free = avail - indent - span.width;
    let justify = matches!(root_style.text_align, TextAlign::Justify) && !span.forced && !is_last;
    let offset = match root_style.text_align {
        TextAlign::Right | TextAlign::End => free,
        TextAlign::Center => free / 2.0,
        _ => 0.0,
    };
    let mut extra_per_space = 0.0f32;
    if justify && free > 0.0 {
        let spaces = count_spaces(prep, span, lead, trim_end);
        if spaces > 0 {
            extra_per_space = free / spaces as f32;
        }
    }

    let mut nodes: Vec<VNode> = Vec::new();
    let root_node = style_vnode(ctx, &root_style, usize::MAX, &[]);
    nodes.push(VNode {
        align: VAlign::Shift(0.0),
        ..root_node
    });
    let mut pending: Vec<Pending> = Vec::new();
    let mut root_children: Vec<FragmentId> = Vec::new();
    let mut stack: Vec<OpenBox> = Vec::new();
    let mut x = sane(indent + offset);
    let mut created_pieces: Vec<FragmentId> = Vec::new();

    // Boxes continuing from the previous line.
    for bid in carry.drain(..) {
        let Some(b) = ctx.get(bid) else { continue };
        let parent = stack.last().map_or(0, |o| o.node);
        let v = style_vnode(ctx, &b.style, parent, &nodes);
        nodes.push(v);
        stack.push(OpenBox {
            box_id: bid,
            node: nodes.len() - 1,
            start: x,
            has_start: false,
            children: Vec::new(),
        });
    }

    for i in span.item_start..span.item_end {
        let Some(it) = prep.items.get(i) else { break };
        let parent_node = stack.last().map_or(0, |o| o.node);
        match &it.kind {
            ItemKind::Open => {
                let Some(b) = ctx.get(it.box_id) else {
                    continue;
                };
                let (m, _, _) = inline_edges(b, avail);
                if b.split.start {
                    x += m.left;
                }
                let start = x;
                let edge = fixed.get(i).copied().unwrap_or(0.0);
                x += if b.split.start { edge - m.left } else { 0.0 };
                let v = style_vnode(ctx, &b.style, parent_node, &nodes);
                nodes.push(v);
                stack.push(OpenBox {
                    box_id: it.box_id,
                    node: nodes.len() - 1,
                    start,
                    has_start: b.split.start,
                    children: Vec::new(),
                });
            }
            ItemKind::Close => {
                let Some(open) = stack.pop() else { continue };
                let Some(b) = ctx.get(open.box_id) else {
                    continue;
                };
                let (m, _, _) = inline_edges(b, avail);
                let edge = fixed.get(i).copied().unwrap_or(0.0);
                if b.split.end {
                    x += edge - m.right;
                }
                let end = x;
                if b.split.end {
                    x += m.right;
                }
                if let Some(f) = close_piece(
                    ctx,
                    b,
                    open,
                    end,
                    b.split.end,
                    avail,
                    cb,
                    &mut pending,
                    &mut created_pieces,
                ) {
                    push_child(&mut stack, &mut root_children, f);
                }
            }
            ItemKind::Text(t) => {
                let s = it.range.start.max(span.start).max(lead);
                let e = it.range.end.min(span.end).min(trim_end);
                if e <= s {
                    continue;
                }
                let local = (s - it.range.start)..(e - it.range.start);
                let (frag, w) = text_fragment(ctx, it, t, local, x, extra_per_space);
                x += w;
                if let Some(f) = frag {
                    pending.push(Pending::Text {
                        frag: f,
                        node: parent_node,
                    });
                    push_child(&mut stack, &mut root_children, f);
                }
            }
            ItemKind::Tab => {
                if it.range.start >= span.start && it.range.start < trim_end.max(span.start) {
                    x += tab_advance(x, prep.tab_space.get(i).copied().unwrap_or(0.0));
                }
            }
            ItemKind::Atomic => {
                let Some(Some(r)) = atomics.get(i) else {
                    continue;
                };
                let Some(f) = r.fragment else { continue };
                ctx.set_position(f, Point::new(x + r.margin.left, 0.0));
                x += r.margin_box().width;
                let margin_h = r.margin_box().height;
                let overflow_visible = it.style.overflow_x == wb_style::Overflow::Visible
                    && it.style.overflow_y == wb_style::Overflow::Visible;
                // CSS 2.2 §10.8.1: an inline-block's baseline is its last
                // line box's, unless it has none or its overflow is not
                // visible: then the bottom margin edge.
                let ascent = match r.last_baseline {
                    Some(bl) if overflow_visible => r.margin.top + bl,
                    _ => margin_h,
                };
                let descent = margin_h - ascent;
                let align =
                    vertical_align(&it.style, nodes.get(parent_node), ascent, descent, 0.0, {
                        ctx.metrics(&it.style).line_height(&it.style)
                    });
                nodes.push(VNode {
                    parent: parent_node,
                    ascent,
                    descent,
                    half_leading: 0.0,
                    align,
                    font_size: font_size(&it.style),
                    x_height: 0.0,
                });
                pending.push(Pending::Atomic {
                    frag: f,
                    node: nodes.len() - 1,
                    margin_top: r.margin.top,
                });
                push_child(&mut stack, &mut root_children, f);
            }
            ItemKind::ForcedBreak | ItemKind::WordBreak => {}
        }
    }
    // Close pieces of boxes that continue on the next line.
    let mut still_open: Vec<BoxId> = Vec::new();
    while let Some(open) = stack.pop() {
        still_open.push(open.box_id);
        let Some(b) = ctx.get(open.box_id) else {
            continue;
        };
        if let Some(f) = close_piece(
            ctx,
            b,
            open,
            x,
            false,
            avail,
            cb,
            &mut pending,
            &mut created_pieces,
        ) {
            push_child(&mut stack, &mut root_children, f);
        }
    }
    still_open.reverse();
    *carry = still_open;

    // Vertical alignment (CSS 2.2 §10.8).
    let (height, baselines) = align_line(&nodes, span.content);
    let root_baseline = baselines.first().copied().unwrap_or(0.0);
    for p in &pending {
        match *p {
            Pending::Text { frag, node } => {
                let v = nodes.get(node).copied().unwrap_or(root_node);
                let bl = baselines.get(node).copied().unwrap_or(root_baseline);
                ctx.translate(frag, Point::new(0.0, bl - v.ascent));
            }
            Pending::InlineBox {
                frag,
                node,
                top_extra,
            } => {
                let v = nodes.get(node).copied().unwrap_or(root_node);
                let bl = baselines.get(node).copied().unwrap_or(root_baseline);
                ctx.translate(frag, Point::new(0.0, bl - v.ascent - top_extra));
            }
            Pending::Atomic {
                frag,
                node,
                margin_top,
            } => {
                let v = nodes.get(node).copied().unwrap_or(root_node);
                let bl = baselines.get(node).copied().unwrap_or(root_baseline);
                let top = bl - v.ascent + margin_top;
                if let Some(f) = ctx.frag(frag) {
                    let d = Point::new(0.0, top - f.border_box.y);
                    ctx.translate(frag, d);
                }
            }
        }
    }
    // Inline box pieces were created innermost first; make each piece's
    // children relative to it.
    for piece in created_pieces {
        let Some(f) = ctx.frag(piece) else { continue };
        let origin = f.border_box.origin();
        let kids = f.children.clone();
        for k in kids {
            ctx.translate(k, Point::new(-origin.x, -origin.y));
        }
    }

    let line_rect_height = if span.content { height } else { 0.0 };
    let frag = ctx.push(Fragment {
        kind: FragmentKind::Line(LineFragment {
            baseline: root_baseline,
        }),
        node: None,
        pseudo: None,
        box_id: None,
        style: root_style,
        border_box: Rect::new(0.0, 0.0, avail, line_rect_height),
        padding_box: Rect::new(0.0, 0.0, avail, line_rect_height),
        content_box: Rect::new(0.0, 0.0, avail, line_rect_height),
        relative_offset: Point::ZERO,
        stacking: Stacking::NONE,
        clip: None,
        scroll_offset: Point::ZERO,
        scrollable_overflow: Rect::new(0.0, 0.0, avail, line_rect_height),
        parent: None,
        children: root_children,
    })?;
    Some(BuiltLine {
        frag,
        height: line_rect_height,
        baseline: root_baseline,
        content: span.content,
    })
}

fn push_child(stack: &mut [OpenBox], root: &mut Vec<FragmentId>, f: FragmentId) {
    match stack.last_mut() {
        Some(o) => o.children.push(f),
        None => root.push(f),
    }
}

/// Creates the fragment of one line's piece of an inline box.
#[allow(clippy::too_many_arguments)]
fn close_piece(
    ctx: &mut LayoutCtx<'_>,
    b: &LayoutBox,
    open: OpenBox,
    end: f32,
    has_end: bool,
    cb_width: f32,
    cb: &ContainingBlock,
    pending: &mut Vec<Pending>,
    created: &mut Vec<FragmentId>,
) -> Option<FragmentId> {
    let (_, bo, p) = inline_edges(b, cb_width);
    let border = Edges::new(
        bo.top,
        if has_end { bo.right } else { 0.0 },
        bo.bottom,
        if open.has_start { bo.left } else { 0.0 },
    );
    let padding = Edges::new(
        p.top,
        if has_end { p.right } else { 0.0 },
        p.bottom,
        if open.has_start { p.left } else { 0.0 },
    );
    let m = ctx.metrics(&b.style);
    // The content area spans the font's ascent and descent; vertical
    // padding and borders extend outside it without affecting the line
    // (CSS 2.2 §10.6.1).
    let height = m.ascent + m.descent + border.vertical() + padding.vertical();
    let width = (end - open.start).max(0.0);
    let f = ctx.box_fragment(
        b,
        Some(open.box_id),
        FragmentKind::Box(BoxFragmentKind::Inline),
        Size::new(width, height),
        border,
        padding,
        open.children,
        false,
    )?;
    ctx.translate(f, Point::new(open.start, 0.0));
    positioned::apply_relative(ctx, f, &b.style, cb);
    pending.push(Pending::InlineBox {
        frag: f,
        node: open.node,
        top_extra: border.top + padding.top,
    });
    created.push(f);
    Some(f)
}

/// Creates a text fragment for `local` (relative to the item's text) at
/// pen position `x`. Returns the fragment and its advance.
fn text_fragment(
    ctx: &mut LayoutCtx<'_>,
    it: &prepare::Item,
    t: &prepare::TextItem,
    local: std::ops::Range<usize>,
    x: f32,
    extra_per_space: f32,
) -> (Option<FragmentId>, f32) {
    // Cluster positions (relative to the fragment start).
    let mut cluster_x: Vec<(usize, f32)> = Vec::new();
    let mut pen = 0.0f32;
    for (off, end, adv) in t.clusters_in(local.start, local.end) {
        cluster_x.push((off, pen));
        let mut a = adv;
        if extra_per_space != 0.0 {
            let spaces = t.text.get(off..end).map_or(0, |s| {
                s.chars().filter(|c| matches!(c, ' ' | '\u{A0}')).count()
            });
            a += extra_per_space * spaces as f32;
        }
        pen += a;
    }
    let width = sane_non_negative(pen);
    let m = ctx.metrics(&it.style);
    // Glyphs: every glyph whose cluster starts in range, placed at its
    // cluster's position plus its offset within the cluster.
    let mut runs: Vec<GlyphRun> = Vec::new();
    for r in &t.runs {
        let mut glyphs = Vec::new();
        let mut within: Option<(usize, f32)> = None;
        for g in &r.glyphs {
            if g.cluster < local.start || g.cluster >= local.end {
                continue;
            }
            let base = match cluster_x.binary_search_by_key(&g.cluster, |(o, _)| *o) {
                Ok(i) => cluster_x.get(i).map_or(0.0, |(_, cx)| *cx),
                Err(_) => continue,
            };
            let inner = match within {
                Some((c, w)) if c == g.cluster => w,
                _ => 0.0,
            };
            glyphs.push(PositionedGlyph {
                id: g.id,
                origin: Point::new(x + base + inner + g.x_offset, m.ascent + g.y_offset),
                advance: g.advance,
                cluster: g.cluster,
            });
            within = Some((g.cluster, inner + g.advance));
        }
        if !glyphs.is_empty() {
            runs.push(GlyphRun {
                font: r.font,
                size: r.size,
                synthesis: r.synthesis,
                glyphs,
            });
        }
    }
    let height = m.ascent + m.descent;
    let rect = Rect::new(x, 0.0, width, height);
    let source_range = t.source_offset(local.start)..t.source_offset(local.end);
    let b = ctx.get(it.box_id);
    let frag = ctx.push(Fragment {
        kind: FragmentKind::Text(TextFragment {
            text: t.text.clone(),
            range: local,
            source_range,
            offset_map: t.offset_map.clone(),
            baseline: m.ascent,
            runs,
        }),
        node: b.and_then(|b| b.node),
        pseudo: b.and_then(|b| b.pseudo),
        box_id: Some(it.box_id),
        style: it.style.clone(),
        border_box: rect,
        padding_box: rect,
        content_box: rect,
        relative_offset: Point::ZERO,
        stacking: Stacking::NONE,
        clip: None,
        scroll_offset: Point::ZERO,
        scrollable_overflow: rect,
        parent: None,
        children: Vec::new(),
    });
    (frag, width)
}

/// Collapsible spaces at the start of a line (only after an emergency
/// break, since phase I already removed the others).
fn leading_trim(prep: &Prepared, span: &LineSpan) -> usize {
    let mut pos = span.start;
    for i in span.item_start..span.item_end {
        let Some(it) = prep.items.get(i) else { break };
        match &it.kind {
            ItemKind::Text(t) => {
                if !matches!(
                    it.white_space,
                    wb_style::WhiteSpace::Normal
                        | wb_style::WhiteSpace::Nowrap
                        | wb_style::WhiteSpace::PreLine
                ) {
                    return pos;
                }
                let s = span.start.max(it.range.start) - it.range.start;
                let e = span.end.min(it.range.end).saturating_sub(it.range.start);
                let slice = t.text.get(s..e).unwrap_or("");
                let trimmed = slice.trim_start_matches(' ');
                pos = it.range.start + s + (slice.len() - trimmed.len());
                if !trimmed.is_empty() {
                    return pos;
                }
            }
            ItemKind::Open | ItemKind::Close | ItemKind::WordBreak => {}
            _ => return pos,
        }
    }
    pos
}

/// Justification opportunities on the line (spaces between `lead` and
/// `trim_end`).
fn count_spaces(prep: &Prepared, span: &LineSpan, lead: usize, trim_end: usize) -> usize {
    let a = span.start.max(lead);
    let b = span.end.min(trim_end);
    prep.text.get(a..b.max(a)).map_or(0, |s| {
        s.chars().filter(|c| matches!(c, ' ' | '\u{A0}')).count()
    })
}

/// Computes the line height and each node's baseline y from the line
/// box top (CSS 2.2 §10.8.1). `top`/`bottom` aligned subtrees are placed
/// after the rest of the line's height is known.
fn align_line(nodes: &[VNode], has_content: bool) -> (f32, Vec<f32>) {
    let n = nodes.len();
    // Baselines relative to the aligned subtree's root, and that root.
    let mut rel = vec![0.0f32; n];
    let mut group: Vec<Option<usize>> = vec![None; n];
    for i in 1..n {
        let Some(v) = nodes.get(i) else { continue };
        match v.align {
            VAlign::Top | VAlign::Bottom => {
                if let Some(g) = group.get_mut(i) {
                    *g = Some(i);
                }
            }
            VAlign::Shift(s) => {
                let pr = rel.get(v.parent).copied().unwrap_or(0.0);
                let pg = group.get(v.parent).copied().flatten();
                if let Some(r) = rel.get_mut(i) {
                    *r = pr + s;
                }
                if let Some(g) = group.get_mut(i) {
                    *g = pg;
                }
            }
        }
    }
    let bounds = |i: usize| {
        let v = nodes.get(i)?;
        let b = rel.get(i).copied()?;
        Some((
            b - v.ascent - v.half_leading,
            b + v.descent + v.half_leading,
        ))
    };
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    // Per top/bottom group: (top, bottom) relative to the group root.
    let mut groups: Vec<(usize, f32, f32)> = Vec::new();
    for i in 0..n {
        let Some((t, b)) = bounds(i) else { continue };
        match group.get(i).copied().flatten() {
            None => {
                if i == 0 && !has_content {
                    continue;
                }
                top = top.min(t);
                bottom = bottom.max(b);
            }
            Some(g) => match groups.iter_mut().find(|(r, _, _)| *r == g) {
                Some(e) => {
                    e.1 = e.1.min(t);
                    e.2 = e.2.max(b);
                }
                None => groups.push((g, t, b)),
            },
        }
    }
    if !top.is_finite() || !bottom.is_finite() {
        top = 0.0;
        bottom = 0.0;
    }
    let root_h = (bottom - top).max(0.0);
    let mut ext_top = 0.0f32;
    let mut height = root_h;
    for (g, t, b) in &groups {
        let h = b - t;
        if nodes.get(*g).is_some_and(|v| v.align == VAlign::Bottom) {
            ext_top = ext_top.max(h - root_h);
        }
        height = height.max(h);
    }
    height = height.max(root_h + ext_top);
    let root_baseline = -top + ext_top;
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let r = rel.get(i).copied().unwrap_or(0.0);
        let v = match group.get(i).copied().flatten() {
            None => root_baseline + r,
            Some(g) => {
                let (gt, gb) = groups
                    .iter()
                    .find(|(x, _, _)| *x == g)
                    .map_or((0.0, 0.0), |(_, t, b)| (*t, *b));
                if nodes.get(g).is_some_and(|v| v.align == VAlign::Bottom) {
                    height - gb + r
                } else {
                    -gt + r
                }
            }
        };
        if let Some(o) = out.get_mut(i) {
            *o = sane(v);
        }
    }
    (sane_non_negative(height).min(MAX_COORD), out)
}

/// Lays out an outside list marker (CSS Lists 3 §2.2) for a list item
/// whose content box starts at `content_left`: the marker's end edge
/// touches the content box's start and its baseline matches the first
/// line's (`first_baseline`, from the border-box top), or it sits at the
/// content top when there is no line.
pub(crate) fn layout_outside_marker(
    ctx: &mut LayoutCtx<'_>,
    marker: BoxId,
    content_left: f32,
    first_baseline: Option<f32>,
    content_top: f32,
    depth: usize,
) -> Option<FragmentId> {
    let b = ctx.get(marker)?;
    let width = intrinsic_content(ctx, marker, depth + 1).max;
    let cb = ContainingBlock {
        width,
        height: None,
    };
    let ifc = layout_inline_formatting_context(ctx, marker, &cb, depth + 1);
    let y = match (first_baseline, ifc.first_baseline) {
        (Some(li), Some(own)) => li - own,
        _ => content_top,
    };
    let f = ctx.box_fragment(
        b,
        Some(marker),
        FragmentKind::Marker,
        Size::new(width, ifc.height),
        Edges::ZERO,
        Edges::ZERO,
        ifc.lines,
        false,
    )?;
    ctx.set_position(f, Point::new(content_left - width, y));
    Some(f)
}
