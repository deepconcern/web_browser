//! Positioning schemes (CSS 2.2 §9.3, §9.4.3, §9.6, §10.3.7, §10.6.4;
//! CSS Position 3).
//!
//! - Relative positioning: [`apply_relative`] offsets a laid out fragment
//!   (and so its descendants) without affecting other boxes. `sticky` is
//!   laid out as `relative` in M1.
//! - Absolute positioning: block layout hands an absolutely positioned
//!   child to [`defer_absolute`] with its static position (where it would
//!   have been in flow) instead of laying it out. Every positioned block
//!   container, and the root for the initial containing block, collects
//!   the boxes it contains ([`enter_box`]); once its own size is known
//!   ([`exit_box`]) they are laid out against its padding box (CSS 2.2
//!   §10.1 item 4) by solving the horizontal and vertical constraint
//!   equations (§10.3.7, §10.6.4; §10.3.8 and §10.6.5 for replaced and
//!   other atomic boxes, which keep their own size). The fragment becomes
//!   a child of the containing block's fragment, so an `overflow` clip
//!   between the two does not clip it (CSS Overflow 3 §3) and it moves
//!   with its containing block's relative offset.
//! - Fixed positioning: the containing block is the viewport (the initial
//!   containing block, CSS 2.2 §10.1 item 3) and the fragment is a child
//!   of the root fragment. Layout places it relative to the document's
//!   origin with the viewport unscrolled: in M1 a fixed box scrolls with
//!   the document like an absolutely positioned one (paint may later pin
//!   it by checking `position: fixed` on the fragment's style).
//!
//! Stacking information (`z-index`, stacking contexts for positioned boxes
//! with an integer `z-index`, `fixed`/`sticky`, `opacity` below 1) is set
//! on every fragment by [`crate::fragment::Stacking::from_style`]; paint
//! orders by it.
//!
//! M1 limits: an inline box (a relatively positioned `<span>`) is not a
//! containing block for absolutely positioned descendants (the nearest
//! positioned block container, or the initial containing block, is used);
//! the fragment of an absolutely positioned box is the last child of its
//! containing block's fragment rather than in tree order; tables and flex
//! containers with `left` and `right` set keep their shrink-to-fit width.

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use wb_style::{ComputedStyle, Position};

use super::sizing::{
    BoxEdges, SizeSpec, box_edges, clamp_min_max, height_px, max_size, min_size, width_spec,
};
use super::{ContainingBlock, block, float, inline, intrinsic_sizes, layout_atomic_inline};
use crate::box_tree::{BoxId, BoxKind, Formatting, LayoutBox};
use crate::context::LayoutCtx;
use crate::fragment::{BoxFragmentKind, FragmentId, FragmentKind};
use crate::geom::{Edges, Point, Size, sane, sane_non_negative};

/// Whether `b` is laid out out of flow by the positioning module.
pub(crate) fn is_absolutely_positioned(b: &LayoutBox) -> bool {
    b.parent.is_some() && b.style.is_absolutely_positioned()
}

/// An absolutely positioned box waiting for its containing block.
#[derive(Clone, Copy, Debug)]
struct AbsEntry {
    id: BoxId,
    /// The block container the box would have been laid out in.
    parent: BoxId,
    /// The static position: the hypothetical margin-box top-left corner
    /// in `parent`'s content box (the start of `area` when aligned).
    local: Point,
    /// The static position's alignment in `area`, per axis (0 = start,
    /// 0.5 = centre, 1 = end), applied once the box's size is known: the
    /// static position of an absolutely positioned child of a flex
    /// container (CSS Flexbox 1 §4.1).
    align: Point,
    area: Size,
    fixed: bool,
    depth: usize,
}

/// A containing block collecting absolutely positioned descendants.
#[derive(Debug)]
struct AbsFrame {
    id: BoxId,
    is_root: bool,
    entries: Vec<AbsEntry>,
}

/// Positioning state: the containing blocks being laid out, outermost
/// (the root) first.
#[derive(Debug, Default)]
pub(crate) struct AbsState {
    frames: Vec<AbsFrame>,
    /// Boxes that collect their own absolutely positioned descendants
    /// (searches for static positions skip them).
    containers: HashSet<BoxId>,
}

/// What [`enter_box`] did, for [`exit_box`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct AbsScope {
    id: Option<BoxId>,
}

/// Called before laying out the contents of block container `id`: a
/// positioned box (or the root) becomes the containing block of the
/// absolutely positioned boxes inside it.
pub(crate) fn enter_box(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    style: &ComputedStyle,
    is_root: bool,
) -> AbsScope {
    if !is_root && matches!(style.position, Position::Static) {
        return AbsScope { id: None };
    }
    ctx.abs.frames.push(AbsFrame {
        id,
        is_root,
        entries: Vec::new(),
    });
    if !is_root {
        ctx.abs.containers.insert(id);
    }
    AbsScope { id: Some(id) }
}

/// Records absolutely positioned `child` met in block flow at `y_local`
/// in its parent's content box (its static position, CSS 2.2 §10.3.7,
/// §10.6.4).
pub(crate) fn defer_absolute(ctx: &mut LayoutCtx<'_>, child: BoxId, y_local: f32, depth: usize) {
    let Some(b) = ctx.get(child) else { return };
    let Some(parent) = b.parent else { return };
    let fixed = matches!(b.style.position, Position::Fixed);
    push_entry(
        ctx,
        AbsEntry {
            id: child,
            parent,
            local: Point::new(0.0, sane(y_local)),
            align: Point::ZERO,
            area: Size::new(0.0, 0.0),
            fixed,
            depth,
        },
    );
}

/// Records absolutely positioned `child` of a flex container (CSS Flexbox
/// 1 §4.1): its static position is that of the sole flex item, aligned
/// by `align` (per-axis fractions, see [`AbsEntry::align`]) in the
/// container's content box of size `area`.
pub(crate) fn defer_absolute_aligned(
    ctx: &mut LayoutCtx<'_>,
    child: BoxId,
    area: Size,
    align: Point,
    depth: usize,
) {
    let Some(b) = ctx.get(child) else { return };
    let Some(parent) = b.parent else { return };
    push_entry(
        ctx,
        AbsEntry {
            id: child,
            parent,
            local: Point::ZERO,
            align: Point::new(align.x.clamp(0.0, 1.0), align.y.clamp(0.0, 1.0)),
            area,
            fixed: matches!(b.style.position, Position::Fixed),
            depth,
        },
    );
}

fn push_entry(ctx: &mut LayoutCtx<'_>, entry: AbsEntry) {
    let fixed = entry.fixed;
    let frames = &mut ctx.abs.frames;
    let frame = if fixed {
        frames.first_mut().filter(|f| f.is_root)
    } else {
        frames.last_mut()
    };
    match frame {
        Some(f) => f.entries.push(entry),
        None => ctx.truncated = true,
    }
}

/// Called once containing block `scope` has its fragment `frag` (border
/// box `size`, `border` widths; `margin_origin` is the root's margin
/// offset, for the initial containing block): lays out the absolutely
/// positioned boxes it collected and adds them to its children.
pub(crate) fn exit_box(
    ctx: &mut LayoutCtx<'_>,
    scope: AbsScope,
    frag: Option<FragmentId>,
    size: Size,
    border: Edges,
    margin_origin: Point,
) {
    let Some(id) = scope.id else { return };
    let Some(pos) = ctx.abs.frames.iter().rposition(|f| f.id == id) else {
        return;
    };
    let is_root = ctx.abs.frames.get(pos).is_some_and(|f| f.is_root);
    let Some(frag) = frag else {
        ctx.abs.frames.truncate(pos);
        return;
    };
    let positioned = ctx
        .get(id)
        .is_some_and(|b| !matches!(b.style.position, Position::Static));
    let padding_origin = Point::new(border.left, border.top);
    let padding_size = Size::new(
        sane_non_negative(size.width - border.horizontal()),
        sane_non_negative(size.height - border.vertical()),
    );
    let icb_origin = Point::new(-margin_origin.x, -margin_origin.y);
    let viewport = ctx.viewport;
    let mut roots: Vec<(FragmentId, Point)> = vec![(frag, Point::ZERO)];
    loop {
        // The root keeps its frame while its boxes are laid out: fixed
        // boxes inside them come back to it as a new batch.
        let batch = match ctx.abs.frames.get_mut(pos) {
            Some(f) => std::mem::take(&mut f.entries),
            None => Vec::new(),
        };
        if !is_root {
            ctx.abs.frames.truncate(pos);
        }
        if batch.is_empty() {
            break;
        }
        // A box laid out more than once (e.g. measured, then laid out by a
        // flex container) was recorded each time: the last record wins.
        let mut last: HashMap<BoxId, usize> = HashMap::with_capacity(batch.len());
        for (i, e) in batch.iter().enumerate() {
            last.insert(e.id, i);
        }
        let batch: Vec<AbsEntry> = batch
            .into_iter()
            .enumerate()
            .filter(|(i, e)| last.get(&e.id) == Some(i))
            .map(|(_, e)| e)
            .collect();
        let needed: HashSet<BoxId> = batch.iter().map(|e| e.parent).collect();
        // Only the root's search enters nested containing blocks: fixed
        // boxes inside them belong to it.
        let offsets = find_offsets(ctx, &roots, &needed, if is_root { None } else { Some(id) });
        let mut laid = Vec::new();
        for e in batch {
            let (cb_origin, cb_size) = if e.fixed || (is_root && !positioned) {
                (icb_origin, viewport)
            } else {
                (padding_origin, padding_size)
            };
            let parent = offsets.get(&e.parent).copied().unwrap_or(cb_origin);
            let stat = Point::new(
                parent.x + e.local.x - cb_origin.x,
                parent.y + e.local.y - cb_origin.y,
            );
            let place = StaticPos {
                at: stat,
                align: e.align,
                area: e.area,
            };
            let Some((f, p)) = layout_absolute(ctx, e.id, cb_size, place, e.depth) else {
                continue;
            };
            let at = Point::new(sane(cb_origin.x + p.x), sane(cb_origin.y + p.y));
            ctx.set_position(f, at);
            if let Some(cf) = ctx.frag_mut(frag) {
                cf.children.push(f);
            }
            laid.push((f, at));
        }
        if !is_root {
            break;
        }
        roots = laid;
    }
    ctx.abs.frames.truncate(pos);
}

/// Finds, below the fragments `roots` (each with its border-box origin
/// relative to the containing block's), the content-box origin of the
/// fragment of every box in `needed`, relative to the containing block's
/// border box. With `skip_for`, the search does not enter other
/// containing blocks. Linear in the fragments visited.
fn find_offsets(
    ctx: &LayoutCtx<'_>,
    roots: &[(FragmentId, Point)],
    needed: &HashSet<BoxId>,
    skip_for: Option<BoxId>,
) -> HashMap<BoxId, Point> {
    let mut found: HashMap<BoxId, Point> = HashMap::new();
    let mut stack: Vec<(FragmentId, Point)> = roots.to_vec();
    while let Some((id, off)) = stack.pop() {
        if found.len() >= needed.len() {
            break;
        }
        let Some(f) = ctx.frag(id) else { continue };
        if let Some(b) = f.box_id
            && needed.contains(&b)
            && !found.contains_key(&b)
        {
            found.insert(
                b,
                Point::new(
                    off.x + f.content_box.x - f.border_box.x,
                    off.y + f.content_box.y - f.border_box.y,
                ),
            );
        }
        for c in f.children.iter().rev() {
            let Some(cf) = ctx.frag(*c) else { continue };
            if skip_for.is_some()
                && cf
                    .box_id
                    .is_some_and(|b| Some(b) != skip_for && ctx.abs.containers.contains(&b))
            {
                continue;
            }
            stack.push((
                *c,
                Point::new(
                    off.x + cf.border_box.x + cf.relative_offset.x,
                    off.y + cf.border_box.y + cf.relative_offset.y,
                ),
            ));
        }
    }
    found
}

/// One axis of the constraint equation of an absolutely positioned box
/// (CSS 2.2 §10.3.7 horizontally, §10.6.4 vertically): `start + m_start
/// + bp + size + m_end + end = cb`.
#[derive(Clone, Copy, Debug)]
struct Axis {
    start: Option<f32>,
    end: Option<f32>,
    /// The content size, if not `auto`.
    size: Option<f32>,
    m_start: Option<f32>,
    m_end: Option<f32>,
    bp: f32,
    cb: f32,
    /// The static position of the margin edge (the start of the
    /// alignment area when `stat_align` is not 0).
    stat: f32,
    /// Where the margin box goes in the static-position area of size
    /// `stat_area` (0 = start, 1 = end).
    stat_align: f32,
    stat_area: f32,
    /// Horizontal (left-to-right): negative equal auto margins become
    /// `margin-left: 0`.
    horizontal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AxisSolution {
    start: f32,
    size: f32,
    m_start: f32,
}

/// Solves one axis. `auto_size(available)` gives the size when it is
/// `auto` and not fixed by both insets: shrink-to-fit horizontally, the
/// content height vertically.
fn solve_axis(a: Axis, auto_size: &mut dyn FnMut(f32) -> f32) -> AxisSolution {
    let ms0 = a.m_start.unwrap_or(0.0);
    let me0 = a.m_end.unwrap_or(0.0);
    let free = |used: f32| a.cb - used - ms0 - me0 - a.bp;
    // The static position once the size is known (CSS Flexbox 1 §4.1).
    let stat = |size: f32| {
        if a.stat_align == 0.0 {
            a.stat
        } else {
            a.stat + a.stat_align * (a.stat_area - (ms0 + me0 + a.bp + size))
        }
    };
    let (start, size, m_start) = match (a.start, a.size, a.end) {
        // All three auto: the static position, then rule 3.
        (None, None, None) => {
            let size = auto_size(free(a.stat));
            (stat(size), size, ms0)
        }
        // None auto: auto margins take the rest; over-constrained ignores
        // the end inset.
        (Some(s), Some(w), Some(e)) => {
            let rest = a.cb - s - e - w - a.bp;
            let ms = match (a.m_start, a.m_end) {
                (None, None) => {
                    let half = rest / 2.0;
                    if a.horizontal && half < 0.0 {
                        0.0
                    } else {
                        half
                    }
                }
                (None, Some(me)) => rest - me,
                (Some(ms), _) => ms,
            };
            (s, w, ms)
        }
        // Rule 1: start and size auto.
        (None, None, Some(e)) => {
            let size = auto_size(free(e));
            (a.cb - e - ms0 - me0 - a.bp - size, size, ms0)
        }
        // Rule 2: start and end auto.
        (None, Some(w), None) => (stat(w), w, ms0),
        // Rule 3: size and end auto.
        (Some(s), None, None) => {
            let size = auto_size(free(s));
            (s, size, ms0)
        }
        // Rule 4: start auto.
        (None, Some(w), Some(e)) => (a.cb - e - ms0 - me0 - a.bp - w, w, ms0),
        // Rule 5: size auto.
        (Some(s), None, Some(e)) => (s, (a.cb - s - e - ms0 - me0 - a.bp).max(0.0), ms0),
        // Rule 6: end auto.
        (Some(s), Some(w), None) => (s, w, ms0),
    };
    AxisSolution {
        start: sane(start),
        size: sane_non_negative(size),
        m_start: sane(m_start),
    }
}

/// Solves `axis` with `size_spec`, then again with `max` or `min` if the
/// result breaks them (CSS 2.2 §10.4, §10.7).
fn solve_clamped(
    axis: Axis,
    min: f32,
    max: f32,
    auto_size: &mut dyn FnMut(f32) -> f32,
) -> AxisSolution {
    let s = solve_axis(axis, auto_size);
    let clamped = clamp_min_max(s.size, min, max);
    if clamped == s.size {
        return s;
    }
    solve_axis(
        Axis {
            size: Some(clamped),
            ..axis
        },
        auto_size,
    )
}

fn insets(style: &ComputedStyle, cb: Size) -> (Option<f32>, Option<f32>, Option<f32>, Option<f32>) {
    let i = &style.inset;
    (
        i.left.resolve(cb.width).map(sane),
        i.right.resolve(cb.width).map(sane),
        i.top.resolve(cb.height).map(sane),
        i.bottom.resolve(cb.height).map(sane),
    )
}

/// The static position of an absolutely positioned box, relative to its
/// containing block's padding box.
#[derive(Clone, Copy, Debug)]
struct StaticPos {
    at: Point,
    align: Point,
    area: Size,
}

fn axes(edges: &BoxEdges, style: &ComputedStyle, cb: Size, stat: StaticPos) -> (Axis, Axis) {
    let (left, right, top, bottom) = insets(style, cb);
    let bp = edges.border_padding();
    (
        Axis {
            start: left,
            end: right,
            size: None,
            m_start: edges.margin.left,
            m_end: edges.margin.right,
            bp: bp.horizontal(),
            cb: cb.width,
            stat: stat.at.x,
            stat_align: stat.align.x,
            stat_area: stat.area.width,
            horizontal: true,
        },
        Axis {
            start: top,
            end: bottom,
            size: None,
            m_start: edges.margin.top,
            m_end: edges.margin.bottom,
            bp: bp.vertical(),
            cb: cb.height,
            stat: stat.at.y,
            stat_align: stat.align.y,
            stat_area: stat.area.height,
            horizontal: false,
        },
    )
}

/// Lays out absolutely positioned box `id` in a containing block of size
/// `cb` (its padding box) with static position `stat` (relative to the
/// padding box). Returns the fragment and its border-box position
/// relative to the padding box.
fn layout_absolute(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: Size,
    stat: StaticPos,
    depth: usize,
) -> Option<(FragmentId, Point)> {
    let b = ctx.get(id)?;
    if ctx.exhausted() || depth > crate::box_tree::MAX_BOX_DEPTH * 2 {
        ctx.truncated = true;
        return None;
    }
    let style = b.style.clone();
    let edges = box_edges(&style, cb.width);
    let bp = edges.border_padding();
    let (hx, vx) = axes(&edges, &style, cb, stat);
    let block_like = matches!(
        b.kind,
        BoxKind::Container {
            formatting: Formatting::Flow | Formatting::FlowRoot | Formatting::Grid,
            ..
        }
    );
    if !block_like {
        // Replaced elements (§10.3.8, §10.6.5), tables and flex
        // containers: their own (shrink-to-fit) size, then the insets.
        let ccb = ContainingBlock {
            width: cb.width,
            height: Some(cb.height),
        };
        let r = layout_atomic_inline(ctx, id, &ccb, depth);
        let f = r.fragment?;
        let mut none = |_| 0.0;
        let h = solve_axis(
            Axis {
                size: Some((r.size.width - bp.horizontal()).max(0.0)),
                ..hx
            },
            &mut none,
        );
        let v = solve_axis(
            Axis {
                size: Some((r.size.height - bp.vertical()).max(0.0)),
                ..vx
            },
            &mut none,
        );
        return Some((f, Point::new(h.start + h.m_start, v.start + v.m_start)));
    }

    // Width (§10.3.7): shrink-to-fit is min(max(min-content, available),
    // max-content).
    let spec = width_spec(&style, Some(cb.width), bp.horizontal());
    let (min_c, max_c) = if matches!(spec, SizeSpec::Px(_)) {
        (0.0, 0.0)
    } else {
        let s = intrinsic_sizes(ctx, id, depth);
        let outer =
            bp.horizontal() + edges.margin.left.unwrap_or(0.0) + edges.margin.right.unwrap_or(0.0);
        ((s.min - outer).max(0.0), (s.max - outer).max(0.0))
    };
    let fit = |avail: f32| min_c.max(avail).min(max_c);
    let width = match spec {
        SizeSpec::Px(w) => Some(w),
        SizeSpec::Auto | SizeSpec::FitContent(None) => None,
        SizeSpec::MinContent => Some(min_c),
        SizeSpec::MaxContent => Some(max_c),
        SizeSpec::FitContent(Some(l)) => Some(fit(l)),
    };
    let min_w = min_size(&style, &style.min_width, Some(cb.width), bp.horizontal());
    let max_w = max_size(&style, &style.max_width, Some(cb.width), bp.horizontal());
    let mut shrink = |avail: f32| fit(avail.max(0.0));
    let h = solve_clamped(Axis { size: width, ..hx }, min_w, max_w, &mut shrink);
    let content_width = h.size;

    // Height (§10.6.4): definite when specified or fixed by both insets.
    let spec_h = height_px(&style, Some(cb.height), bp.vertical());
    let min_h = min_size(&style, &style.min_height, Some(cb.height), bp.vertical());
    let max_h = max_size(&style, &style.max_height, Some(cb.height), bp.vertical());
    let definite = spec_h
        .or_else(|| match (vx.start, vx.end) {
            (Some(t), Some(bo)) => Some(
                cb.height
                    - t
                    - bo
                    - edges.margin.top.unwrap_or(0.0)
                    - edges.margin.bottom.unwrap_or(0.0)
                    - bp.vertical(),
            ),
            _ => None,
        })
        .map(|v| sane_non_negative(clamp_min_max(v.max(0.0), min_h, max_h)));
    let child_cb = ContainingBlock {
        width: content_width,
        height: definite,
    };
    let fscope = float::enter_block(ctx, id, true, false, &edges, 0.0, content_width);
    let ascope = enter_box(ctx, id, &style, false);
    let content = block::layout_contents(ctx, b, id, &child_cb, depth, false, false);
    let float_bottom = float::exit_block(ctx, fscope);
    let auto_h = content.height.max(float_bottom);
    let mut content_h = |_| auto_h;
    let v = solve_clamped(Axis { size: spec_h, ..vx }, min_h, max_h, &mut content_h);

    let offset = Point::new(bp.left, bp.top);
    for c in &content.children {
        ctx.translate(*c, offset);
    }
    let mut children = content.children;
    if let Some(m) = b.marker
        && let Some(f) = inline::layout_outside_marker(
            ctx,
            m,
            bp.left,
            content.first_baseline.map(|v| v + bp.top),
            bp.top,
            depth,
        )
    {
        children.push(f);
    }
    let size = Size::new(content_width + bp.horizontal(), v.size + bp.vertical());
    let frag = ctx.box_fragment(
        b,
        Some(id),
        FragmentKind::Box(BoxFragmentKind::Block),
        size,
        edges.border,
        edges.padding,
        children,
        false,
    );
    exit_box(ctx, ascope, frag, size, edges.border, Point::ZERO);
    Some((frag?, Point::new(h.start + h.m_start, v.start + v.m_start)))
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
