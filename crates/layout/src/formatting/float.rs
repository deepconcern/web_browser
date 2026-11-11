//! Floats and `clear` (CSS 2.2 §9.5).
//!
//! # Model
//!
//! Every block formatting context (BFC) being laid out has a [`Bfc`] on
//! the stack in [`FloatState`]: its float [`Exclusions`] in the BFC
//! root's content-box coordinates, and one [`Frame`] per block container
//! of that BFC currently being laid out, giving the container's content
//! box position in the BFC.
//!
//! Block layout positions a child only after laying it out (its top
//! margin may collapse with its descendants', CSS 2.2 §8.3.1), so a
//! frame's vertical position can be *pending*: "the base plus these
//! still-collapsing margins". Before laying out an in-flow child, block
//! layout leaves a [`Hint`] (from [`prepare_child`]) that tells the child
//! where its border box will start; the child's frame resolves as soon as
//! something fixes the position: a top border or padding, the first line
//! box, a BFC root or a cleared box. At that point every pending frame of
//! the BFC resolves to the same height, since they all share their
//! content top with the first content.
//!
//! A float met in block flow is laid out at once (shrink-to-fit, CSS 2.2
//! §10.3.5) but placed lazily: it waits in the BFC's queue until the next
//! in-flow content fixes the current height (so a float between two
//! blocks lines up with the second block's border box after margin
//! collapsing), then [`Exclusions::place`] applies the §9.5.1 rules and
//! the fragment is moved there.
//!
//! Line boxes are shortened next to floats through [`LineSource`], which
//! the inline formatting context asks for one line at a time; a line that
//! cannot fit even its first unit moves down below the next float edge
//! (§9.5). BFC roots, tables and block-level replaced elements avoid
//! floats ([`prepare_child`] narrows their containing block and
//! [`place_child`] moves them down if they still overlap), and the
//! auto height of a BFC root includes its floats (§10.6.7).
//!
//! # M1 limits
//!
//! - Floats inside a run of inline content split it into anonymous blocks
//!   (the box tree treats them as block-level), so a float in the middle
//!   of a paragraph starts on a new line instead of at the current one.
//! - Line boxes are shortened for the strut height (the container's
//!   `line-height`); a taller line is not re-checked against floats that
//!   start below its first strut.
//! - A box with clearance uses its own top margin, not margins collapsed
//!   from its descendants, for the hypothetical position (§9.5.2).

mod exclusions;
mod lines;
#[cfg(test)]
pub(crate) mod tests;

use wb_style::Float;

pub(crate) use self::exclusions::{ClearSide, Exclusions, Side};
pub(crate) use self::lines::LineSource;
use super::sizing::{BoxEdges, box_edges};
use super::{CollapsedMargin, ContainingBlock, LayoutResult, layout_atomic_inline, positioned};
use crate::box_tree::{BoxId, BoxKind, LayoutBox};
use crate::context::LayoutCtx;
use crate::fragment::FragmentId;
use crate::geom::{Point, Size, sane};

/// How many times a BFC root is moved down looking for room beside
/// floats before it is placed below all of them.
const MAX_AVOID_STEPS: usize = 64;

/// Whether `b` is laid out as a float.
pub(crate) fn is_float(b: &LayoutBox) -> bool {
    b.parent.is_some() && b.style.is_floating() && !b.style.is_absolutely_positioned()
}

fn float_side(b: &LayoutBox) -> Side {
    match b.style.float {
        Float::Right | Float::InlineEnd => Side::Right,
        _ => Side::Left,
    }
}

/// The vertical position of a frame's content-box top in its BFC.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Origin {
    /// Known.
    At(f32),
    /// `base + margin.resolve()` once every margin that collapses into it
    /// is known.
    Pending { base: f32, margin: CollapsedMargin },
}

impl Origin {
    fn estimate(self) -> f32 {
        match self {
            Origin::At(y) => y,
            Origin::Pending { base, margin } => base + margin.resolve(),
        }
    }
}

/// A block container of the BFC being laid out.
#[derive(Clone, Copy, Debug)]
struct Frame {
    id: BoxId,
    /// Content-box left edge in BFC coordinates.
    x: f32,
    /// Content-box width.
    width: f32,
    origin: Origin,
}

/// Where the next in-flow child's border box starts: at
/// `base + (pending ⊕ its collapsed top margin)`, or at `fixed_top`
/// when it has clearance or was moved below floats.
#[derive(Clone, Copy, Debug)]
struct Hint {
    id: BoxId,
    base: f32,
    pending: CollapsedMargin,
    fixed_top: Option<f32>,
    /// The containing block's content-box left edge.
    x: f32,
}

/// A float laid out but not yet placed.
#[derive(Clone, Copy, Debug)]
struct Queued {
    frag: FragmentId,
    side: Side,
    clear: Option<ClearSide>,
    /// The margin-box size.
    size: Size,
    /// The top-left margins.
    margin: Point,
    /// The border-box position the fragment was given, in BFC
    /// coordinates, assuming its parent's origin estimate below.
    provisional: Point,
    /// The parent frame and its origin estimate when the float was met.
    parent: BoxId,
    parent_estimate: f32,
    /// The containing block's horizontal range.
    cb_lo: f32,
    cb_hi: f32,
}

/// One block formatting context being laid out.
#[derive(Debug, Default)]
struct Bfc {
    excl: Exclusions,
    frames: Vec<Frame>,
    queue: Vec<Queued>,
    hint: Option<Hint>,
}

impl Bfc {
    fn frame_for(&self, id: BoxId) -> Option<&Frame> {
        self.frames.last().filter(|f| f.id == id)
    }

    /// Resolves every pending frame to `y` (they all share their content
    /// top with the content that fixed it).
    fn resolve_pending(&mut self, y: f32) {
        for f in &mut self.frames {
            if matches!(f.origin, Origin::Pending { .. }) {
                f.origin = Origin::At(y);
            }
        }
    }
}

/// Float layout state: one entry per block formatting context being laid
/// out (innermost last).
#[derive(Debug, Default)]
pub(crate) struct FloatState {
    bfcs: Vec<Bfc>,
}

/// What [`enter_block`] pushed, for [`exit_block`].
#[derive(Clone, Copy, Debug)]
pub(crate) enum FloatScope {
    /// The box establishes a BFC.
    Root,
    /// The box takes part in its parent's BFC.
    Frame(BoxId),
    /// The box was laid out outside block flow (a flex item, ...): its
    /// floats are kept to itself, as in a BFC root.
    Isolated,
}

/// Places the queued floats. `y` is the height the current content
/// starts at (`None` at the end of the BFC: each float stays where it was
/// met).
fn flush(ctx: &mut LayoutCtx<'_>, y: Option<f32>) -> Vec<(FragmentId, BoxId)> {
    let Some(bfc) = ctx.floats.bfcs.last_mut() else {
        return Vec::new();
    };
    if bfc.queue.is_empty() {
        return Vec::new();
    }
    let queue = std::mem::take(&mut bfc.queue);
    let mut moves: Vec<(FragmentId, BoxId, Point)> = Vec::with_capacity(queue.len());
    for q in queue {
        let mut y0 = y.unwrap_or(q.provisional.y - q.margin.y);
        if let Some(c) = q.clear {
            y0 = y0.max(bfc.excl.clear_y(c));
        }
        let at = bfc
            .excl
            .place(q.side, q.size.width, q.size.height, y0, q.cb_lo, q.cb_hi);
        let actual = Point::new(at.x + q.margin.x, at.y + q.margin.y);
        // If the parent's origin has resolved since, the fragment (placed
        // relative to the parent) must follow the difference too.
        let origin_now = bfc
            .frames
            .iter()
            .rev()
            .find(|f| f.id == q.parent)
            .map_or(q.parent_estimate, |f| f.origin.estimate());
        let d = Point::new(
            sane(actual.x - q.provisional.x),
            sane(actual.y - q.provisional.y - (origin_now - q.parent_estimate)),
        );
        moves.push((q.frag, q.parent, d));
    }
    let mut placed = Vec::with_capacity(moves.len());
    for (f, parent, d) in moves {
        ctx.translate(f, d);
        placed.push((f, parent));
    }
    placed
}

/// Starts the layout of block container `id`'s contents (called by block
/// layout after the box's width is known). `new_bfc` says whether it
/// establishes a BFC; `top_open` whether its top margin collapses with
/// its first child's.
pub(crate) fn enter_block(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    new_bfc: bool,
    top_open: bool,
    edges: &BoxEdges,
    margin_left: f32,
    content_width: f32,
) -> FloatScope {
    let fresh = |ctx: &mut LayoutCtx<'_>| {
        ctx.floats.bfcs.push(Bfc {
            frames: vec![Frame {
                id,
                x: 0.0,
                width: content_width,
                origin: Origin::At(0.0),
            }],
            ..Bfc::default()
        });
    };
    let hint = ctx.floats.bfcs.last_mut().and_then(|b| {
        if b.hint.is_some_and(|h| h.id == id) {
            b.hint.take()
        } else {
            None
        }
    });
    if new_bfc {
        fresh(ctx);
        return FloatScope::Root;
    }
    let Some(h) = hint else {
        fresh(ctx);
        return FloatScope::Isolated;
    };
    let bp_top = edges.border.top + edges.padding.top;
    let x = h.x + margin_left + edges.border.left + edges.padding.left;
    let mt = CollapsedMargin::new(edges.margin.top.unwrap_or(0.0));
    let origin = if let Some(top) = h.fixed_top {
        Origin::At(top + bp_top)
    } else if top_open {
        Origin::Pending {
            base: h.base,
            margin: h.pending.adjoin(mt),
        }
    } else {
        // The border box's position is fixed: so is every pending frame's
        // content top, and the queued floats go here.
        let top = sane(h.base + h.pending.adjoin(mt).resolve());
        if let Some(b) = ctx.floats.bfcs.last_mut() {
            b.resolve_pending(top);
        }
        flush(ctx, Some(top));
        Origin::At(top + bp_top)
    };
    if let Some(b) = ctx.floats.bfcs.last_mut() {
        b.frames.push(Frame {
            id,
            x,
            width: content_width,
            origin,
        });
    }
    FloatScope::Frame(id)
}

/// Ends what [`enter_block`] started. Returns, for a BFC root, how far
/// below its content-box top its floats reach (CSS 2.2 §10.6.7), else 0.
pub(crate) fn exit_block(ctx: &mut LayoutCtx<'_>, scope: FloatScope) -> f32 {
    match scope {
        FloatScope::Frame(id) => {
            if let Some(b) = ctx.floats.bfcs.last_mut()
                && b.frames.last().is_some_and(|f| f.id == id)
            {
                b.frames.pop();
            }
            0.0
        }
        FloatScope::Root | FloatScope::Isolated => {
            flush(ctx, None);
            let bottom = ctx
                .floats
                .bfcs
                .pop()
                .and_then(|b| b.excl.bottom())
                .unwrap_or(0.0);
            // An isolated box (a flex item, ...) is laid out outside any
            // block flow, as an independent formatting context: its floats
            // count toward its height like a BFC root's.
            sane(bottom).max(0.0)
        }
    }
}

/// Lays out float `child` met in block flow at `y_local` (in its parent's
/// content box) and queues it for placement. Returns its fragment, which
/// the caller adds to the parent's children.
pub(crate) fn layout_float(
    ctx: &mut LayoutCtx<'_>,
    child: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    y_local: f32,
) -> Option<FragmentId> {
    let b = ctx.get(child)?;
    // CSS 2.2 §10.3.5 / §10.6.7: shrink-to-fit width, and a float is a
    // BFC root.
    let r = layout_atomic_inline(ctx, child, cb, depth);
    let f = r.fragment?;
    positioned::apply_relative(ctx, f, &b.style, cb);
    let local = Point::new(r.margin.left, sane(y_local + r.margin.top));
    ctx.set_position(f, local);
    let parent = b.parent?;
    let bfc = ctx.floats.bfcs.last_mut()?;
    let frame = *bfc.frame_for(parent)?;
    let estimate = frame.origin.estimate();
    bfc.queue.push(Queued {
        frag: f,
        side: float_side(b),
        clear: ClearSide::from_style(b.style.clear),
        size: r.margin_box(),
        margin: Point::new(r.margin.left, r.margin.top),
        provisional: Point::new(frame.x + local.x, estimate + local.y),
        parent,
        parent_estimate: estimate,
        cb_lo: frame.x,
        cb_hi: frame.x + frame.width,
    });
    Some(f)
}

/// How block layout lays out and positions an in-flow child (from
/// [`prepare_child`]).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChildPlan {
    /// A narrower containing block for a BFC root next to floats.
    pub cb: Option<ContainingBlock>,
    child: Option<BoxId>,
    avoid: Option<Avoid>,
}

/// The state of a child that clears floats or must avoid them.
#[derive(Clone, Copy, Debug)]
struct Avoid {
    parent: BoxId,
    top_y: f32,
    forced: bool,
    avoids: bool,
    dx: f32,
    margin_left: f32,
    margin_right: f32,
}

/// Where block layout puts the child (from [`place_child`]).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChildPlacement {
    /// The border-box top in the parent's content box when it is fixed by
    /// clearance or float avoidance (the child's margins then do not
    /// collapse with the parent's or the previous sibling's).
    pub top: Option<f32>,
    /// A horizontal shift for a BFC root beside floats.
    pub dx: f32,
}

/// Called by block layout before laying out in-flow child `child`: `y`,
/// `pending` and `top` are block layout's flow position, pending margins
/// and margins collapsed into the parent's top, `at_top` whether the
/// child is still at the parent's top edge.
pub(crate) fn prepare_child(
    ctx: &mut LayoutCtx<'_>,
    child: BoxId,
    cb: &ContainingBlock,
    y: f32,
    pending: CollapsedMargin,
    top: CollapsedMargin,
    at_top: bool,
) -> ChildPlan {
    let mut plan = ChildPlan {
        child: Some(child),
        ..ChildPlan::default()
    };
    let Some(b) = ctx.get(child) else {
        return plan;
    };
    let Some(parent) = b.parent else { return plan };
    let Some(bfc) = ctx.floats.bfcs.last_mut() else {
        return plan;
    };
    let Some(frame) = bfc.frame_for(parent).copied() else {
        return plan;
    };
    let (base, pend) = match frame.origin {
        Origin::Pending { base, margin } if at_top => (base, margin.adjoin(top)),
        Origin::Pending { base, margin } => (base + margin.adjoin(top).resolve() + y, pending),
        Origin::At(o) => (o + y, pending),
    };
    bfc.hint = Some(Hint {
        id: child,
        base,
        pending: pend,
        fixed_top: None,
        x: frame.x,
    });
    let clear = ClearSide::from_style(b.style.clear);
    let avoids = b.establishes_bfc() || matches!(b.kind, BoxKind::Replaced(_));
    if (clear.is_none() && !avoids) || (bfc.queue.is_empty() && bfc.excl.bottom().is_none()) {
        return plan;
    }
    let edges = box_edges(&b.style, cb.width);
    let mt = edges.margin.top.unwrap_or(0.0);
    let hyp = sane(base + pend.adjoin(CollapsedMargin::new(mt)).resolve());
    let chain_pending = at_top && matches!(frame.origin, Origin::Pending { .. });
    // Queued floats go where the child's margin box starts if it clears
    // (its margin then no longer collapses with what is above), else at
    // its border box. Pending frames resolve there for now.
    let before_margin = sane(base + pend.resolve());
    let flush_y = if clear.is_some() { before_margin } else { hyp };
    let pending_ids: Vec<BoxId> = if chain_pending {
        let ids = bfc
            .frames
            .iter()
            .filter(|f| matches!(f.origin, Origin::Pending { .. }))
            .map(|f| f.id)
            .collect();
        bfc.resolve_pending(flush_y);
        ids
    } else {
        Vec::new()
    };
    let placed = flush(ctx, Some(flush_y));
    let Some(bfc) = ctx.floats.bfcs.last_mut() else {
        return plan;
    };
    let mut avoid = Avoid {
        parent,
        top_y: hyp,
        forced: false,
        avoids,
        dx: 0.0,
        margin_left: edges.margin.left.unwrap_or(0.0),
        margin_right: edges.margin.right.unwrap_or(0.0),
    };
    // CSS 2.2 §9.5.2: clearance puts the border box below the floats.
    if let Some(c) = clear {
        let cy = bfc.excl.clear_y(c);
        if cy > hyp {
            avoid.top_y = sane(cy);
            avoid.forced = true;
        }
    }
    if avoid.forced
        && let Some(h) = &mut bfc.hint
    {
        h.fixed_top = Some(avoid.top_y);
    }
    // With clearance the chain's content top is above the child's
    // margin; without, it is the child's border box.
    let final_origin = if avoid.forced { before_margin } else { hyp };
    let mut shift = Vec::new();
    if chain_pending && final_origin != flush_y {
        for f in &mut bfc.frames {
            if pending_ids.contains(&f.id) {
                f.origin = Origin::At(final_origin);
            }
        }
        shift = placed
            .iter()
            .filter(|(_, p)| pending_ids.contains(p))
            .map(|(f, _)| (*f, Point::new(0.0, sane(flush_y - final_origin))))
            .collect();
    }
    // CSS 2.2 §9.5: a BFC root's border box does not overlap floats. Its
    // margins may: the border box starts at the float edge or at the
    // margin edge, whichever is further in.
    if avoids {
        let band = bfc
            .excl
            .band(avoid.top_y, 0.0, frame.x, frame.x + frame.width);
        if band.constrained {
            let l = frame.x.max(band.lo - avoid.margin_left);
            let r = (frame.x + frame.width)
                .min(band.hi + avoid.margin_right)
                .max(l);
            plan.cb = Some(ContainingBlock {
                width: r - l,
                height: cb.height,
            });
            avoid.dx = l - frame.x;
        }
    }
    plan.avoid = Some(avoid);
    for (f, d) in shift {
        ctx.translate(f, d);
    }
    plan
}

/// Called by block layout after laying out the child of `plan`: drops an
/// unused hint and, for a BFC root that still overlaps floats, moves it
/// down until it fits (CSS 2.2 §9.5).
pub(crate) fn place_child(
    ctx: &mut LayoutCtx<'_>,
    plan: &ChildPlan,
    r: &LayoutResult,
) -> ChildPlacement {
    let Some(bfc) = ctx.floats.bfcs.last_mut() else {
        return ChildPlacement::default();
    };
    if bfc.hint.is_some_and(|h| Some(h.id) == plan.child) {
        bfc.hint = None;
    }
    let Some(a) = plan.avoid else {
        return ChildPlacement::default();
    };
    let Some(frame) = bfc.frame_for(a.parent).copied() else {
        return ChildPlacement::default();
    };
    let (lo, hi) = (frame.x, frame.x + frame.width);
    let mut top_y = a.top_y;
    let mut forced = a.forced;
    let mut dx = a.dx;
    if a.avoids && r.fragment.is_some() {
        let w = r.size.width;
        let h = r.size.height;
        let left = lo + dx + r.margin.left;
        let band = bfc.excl.band(top_y, h, lo, hi);
        let fits_here =
            !band.constrained || (left + EPS_FIT >= band.lo && left + w <= band.hi + EPS_FIT);
        if !fits_here {
            let mut y = top_y;
            let mut found = None;
            for _ in 0..MAX_AVOID_STEPS {
                let Some(n) = bfc.excl.next_change(y) else {
                    break;
                };
                y = n;
                let band = bfc.excl.band(y, h, lo, hi);
                if !band.constrained {
                    found = Some((y, 0.0));
                    break;
                }
                let border_left = (lo + a.margin_left).max(band.lo);
                let border_right = (hi - a.margin_right).min(band.hi);
                if border_left + w <= border_right + EPS_FIT {
                    found = Some((y, border_left - lo - r.margin.left));
                    break;
                }
            }
            let (ny, ndx) = found.unwrap_or_else(|| (bfc.excl.bottom().unwrap_or(y).max(y), 0.0));
            top_y = sane(ny);
            dx = sane(ndx);
            forced = true;
        }
    }
    if !forced {
        return ChildPlacement { top: None, dx };
    }
    // The parent's origin was resolved by `prepare_child`.
    let o = frame.origin.estimate();
    ChildPlacement {
        top: Some(sane(top_y - o)),
        dx,
    }
}

const EPS_FIT: f32 = exclusions::EPSILON;

/// Called by block layout once a child that does not collapse through has
/// its position `child_y` in the parent's content box; `top` is the margin
/// collapsed into the parent's top so far. Resolves the parent's origin
/// and places queued floats at the child's border-box top.
pub(crate) fn child_placed(
    ctx: &mut LayoutCtx<'_>,
    child: BoxId,
    child_y: f32,
    top: CollapsedMargin,
) {
    let Some(parent) = ctx.get(child).and_then(|b| b.parent) else {
        return;
    };
    let Some(bfc) = ctx.floats.bfcs.last_mut() else {
        return;
    };
    let Some(frame) = bfc.frame_for(parent).copied() else {
        return;
    };
    let o = match frame.origin {
        Origin::At(o) => o,
        Origin::Pending { base, margin } => {
            let o = sane(base + margin.adjoin(top).resolve());
            bfc.resolve_pending(o);
            o
        }
    };
    flush(ctx, Some(sane(o + child_y)));
}

/// The content-box position of inline formatting context container `id`
/// in its BFC, resolving it (its first line box fixes it) and placing
/// queued floats there. `None` when the container has no frame.
fn ifc_origin(ctx: &mut LayoutCtx<'_>, id: BoxId) -> Option<Point> {
    let bfc = ctx.floats.bfcs.last_mut()?;
    let frame = *bfc.frame_for(id)?;
    let o = frame.origin.estimate();
    if matches!(frame.origin, Origin::Pending { .. }) {
        bfc.resolve_pending(o);
    }
    flush(ctx, Some(o));
    Some(Point::new(frame.x, o))
}
