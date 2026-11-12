//! Flex layout (CSS Flexible Box Layout Module Level 1).
//!
//! [`layout_flex_container`] implements the flex layout algorithm (§9):
//! flex items in order-modified document order (§5.4), single- and
//! multi-line containers (§6, §9.3), flexible lengths (§9.7) with the
//! automatic minimum size of flex items (§4.5), main-axis alignment with
//! `auto` margins and `justify-content` (§8.1, §8.2), cross-axis
//! alignment with `align-items`/`align-self`, including `stretch` and
//! baseline alignment (§8.3, §8.5, §9.4), `align-content` (§8.4) and gaps
//! (CSS Box Alignment 3 §8). [`intrinsic_content`] gives the container's
//! intrinsic inline sizes (§9.9).
//!
//! Flex items are laid out with their used sizes imposed through
//! [`FlexState`]: block layout and the width solver read
//! [`FlexState::forced`] in place of the item's own `width`/`height`.
//! Item layouts are cached per (box, containing block, imposed size) so
//! that the measure-then-stretch passes stay polynomial for nested flex
//! containers.
//!
//! The box tree wraps each run of text directly inside a flex container
//! in an anonymous block, the anonymous flex item of §4.
//!
//! Absolutely positioned children do not take part in flex layout (§4.1).
//! Until the positioning module lays them out, they are placed as static
//! placeholders at their static position (as if they were the sole flex
//! item, §4.1), sized shrink-to-fit.
//!
//! Directions are physical for a horizontal, left-to-right writing mode:
//! `row` runs left to right and `column` top to bottom.

use std::collections::HashMap;
use std::sync::Arc;

use wb_style::{
    AlignKeyword, BoxSizing, ComputedStyle, FlexBasis, FlexDirection, FlexWrap, Gap, Overflow,
    Size as CssSize,
};

use super::sizing::{
    Margins, box_edges, clamp_min_max, height_px, max_size, min_size, solve_width,
};
use super::{
    CollapsedMargin, ContainingBlock, IntrinsicSizes, LayoutResult, WidthMode, inline,
    intrinsic_sizes, positioned, replaced,
};
use crate::box_tree::{BoxId, BoxKind, Formatting, LayoutBox};
use crate::context::LayoutCtx;
use crate::fragment::{BoxFragmentKind, FragmentId, FragmentKind};
use crate::geom::{Edges, MAX_COORD, Point, Rect, Size, sane, sane_non_negative};

/// The most flex items one flex line holds. A line that would hold more
/// is broken after this many items, also in a single-line container: the
/// flexible length loop (§9.7) is quadratic in the items of a line.
pub(crate) const MAX_ITEMS_PER_LINE: usize = 1024;

/// The most item layouts kept in the item layout cache.
const MAX_CACHE_ENTRIES: usize = 1 << 17;

/// Item layouts whose fragment subtree is larger than this are not
/// cached (restoring a cached layout touches the whole subtree).
const MAX_CACHED_SUBTREE: usize = 1 << 14;

/// The largest flex factor used (larger values are clamped; NaN is 0).
const MAX_FLEX_FACTOR: f64 = 1.0e9;

/// Slack for floating-point comparisons when breaking lines.
const EPSILON: f32 = 0.01;

/// Content-box sizes a flex container imposes on one of its items.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ForcedSize {
    /// The used content width, replacing the item's `width`.
    pub width: Option<f32>,
    /// The used content height, replacing the item's `height`.
    pub height: Option<f32>,
}

/// The cache key of one item layout: the box, the containing block width
/// and height, and the imposed width and height (as `f32` bits).
type CacheKey = (BoxId, u32, Option<u32>, Option<u32>, Option<u32>);

#[derive(Clone, Debug)]
struct CachedLayout {
    result: LayoutResult,
    /// Every descendant fragment with its position relative to its parent,
    /// as they were when the layout finished.
    positions: Vec<(FragmentId, Point)>,
}

/// Per-layout state of flex layout, kept in [`LayoutCtx`].
#[derive(Debug, Default)]
pub(crate) struct FlexState {
    forced: HashMap<BoxId, ForcedSize>,
    cache: HashMap<CacheKey, CachedLayout>,
}

impl FlexState {
    /// The sizes the parent flex container imposes on box `id` while it
    /// is being laid out (none for other boxes).
    pub(crate) fn forced(&self, id: BoxId) -> ForcedSize {
        if self.forced.is_empty() {
            return ForcedSize::default();
        }
        self.forced.get(&id).copied().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// Converts a specified size to the content box (`box-sizing`).
fn to_content(style: &ComputedStyle, v: f32, bp: f32) -> f32 {
    let v = sane_non_negative(v);
    match style.box_sizing {
        BoxSizing::ContentBox => v,
        BoxSizing::BorderBox => (v - bp).max(0.0),
    }
}

/// A definite `width`/`height`/`flex-basis` length in content-box px, or
/// `None` for `auto`, intrinsic keywords, and percentages against an
/// indefinite basis.
fn definite(style: &ComputedStyle, s: &CssSize, basis: Option<f32>, bp: f32) -> Option<f32> {
    match s {
        CssSize::LengthPercentage(lp) => match (lp.has_percentage(), basis) {
            (true, None) => None,
            (_, b) => Some(to_content(style, lp.resolve(b.unwrap_or(0.0)), bp)),
        },
        _ => None,
    }
}

/// A used gap (CSS Box Alignment 3 §8.3): `normal` is 0 in flex layout
/// and percentages against an indefinite basis are 0.
fn gap(g: &Gap, basis: Option<f32>) -> f32 {
    match g {
        Gap::Normal => 0.0,
        Gap::LengthPercentage(lp) => match (lp.has_percentage(), basis) {
            (true, None) => 0.0,
            (_, b) => sane_non_negative(lp.resolve(b.unwrap_or(0.0))),
        },
    }
}

/// A sanitized flex factor.
fn factor(v: f32) -> f64 {
    let v = f64::from(v);
    if v.is_finite() {
        v.clamp(0.0, MAX_FLEX_FACTOR)
    } else if v == f64::INFINITY {
        MAX_FLEX_FACTOR
    } else {
        0.0
    }
}

/// Whether the box is a scroll container (CSS Overflow 3 §3), which
/// gives it an automatic minimum size of 0 (§4.5).
fn is_scroll_container(style: &ComputedStyle) -> bool {
    let scrolls = |o: Overflow| !matches!(o, Overflow::Visible | Overflow::Clip);
    scrolls(style.overflow_x) || scrolls(style.overflow_y)
}

/// A distribution of free space along an axis: where the first item
/// starts, and the extra space between items.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Distribute {
    Start,
    End,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

impl Distribute {
    /// The leading offset and the space added between neighbours, for
    /// `free` space and `n` items (CSS Box Alignment 3 §5.1, with the
    /// default fallbacks of §5.1.1 when `free` is negative).
    fn offsets(self, free: f32, n: usize) -> (f32, f32) {
        let free = sane(free);
        let nf = n.max(1) as f32;
        match self {
            Distribute::Start => (0.0, 0.0),
            Distribute::End => (free, 0.0),
            Distribute::Center => (free / 2.0, 0.0),
            Distribute::SpaceBetween => {
                if free <= 0.0 || n <= 1 {
                    (0.0, 0.0)
                } else {
                    (0.0, free / (nf - 1.0))
                }
            }
            Distribute::SpaceAround => {
                if free <= 0.0 {
                    (free / 2.0, 0.0)
                } else {
                    (free / nf / 2.0, free / nf)
                }
            }
            Distribute::SpaceEvenly => {
                if free <= 0.0 {
                    (free / 2.0, 0.0)
                } else {
                    (free / (nf + 1.0), free / (nf + 1.0))
                }
            }
        }
    }
}

/// The flex container's axes.
#[derive(Clone, Copy, Debug)]
struct Axes {
    /// The main axis is horizontal (`row`, `row-reverse`).
    row: bool,
    /// Main-start is on the right/bottom (`*-reverse`).
    main_reverse: bool,
    /// Cross-start is on the bottom/right (`wrap-reverse`).
    cross_reverse: bool,
}

impl Axes {
    /// Maps a positional alignment keyword to a flex-relative one for the
    /// main axis (`justify-content`). `start`/`end`/`left`/`right` are
    /// physical; `left`/`right` behave as `start` on a vertical axis
    /// (CSS Box Alignment 3 §4.2).
    fn justify(&self, k: AlignKeyword) -> Distribute {
        use AlignKeyword as K;
        let physical_start = |at_start: bool| {
            if at_start != self.main_reverse {
                Distribute::Start
            } else {
                Distribute::End
            }
        };
        match k {
            K::FlexEnd => Distribute::End,
            K::Center => Distribute::Center,
            K::SpaceBetween => Distribute::SpaceBetween,
            K::SpaceAround => Distribute::SpaceAround,
            K::SpaceEvenly => Distribute::SpaceEvenly,
            K::Start | K::SelfStart => physical_start(true),
            K::End | K::SelfEnd => physical_start(false),
            K::Left => physical_start(true),
            K::Right => physical_start(!self.row),
            _ => Distribute::Start,
        }
    }

    /// Maps `align-content` to a distribution along the cross axis
    /// (`stretch` is handled by the caller and falls back to `start`).
    fn align_content(&self, k: AlignKeyword) -> Distribute {
        use AlignKeyword as K;
        let physical_start = |at_start: bool| {
            if at_start != self.cross_reverse {
                Distribute::Start
            } else {
                Distribute::End
            }
        };
        match k {
            K::FlexEnd => Distribute::End,
            K::Center => Distribute::Center,
            K::SpaceBetween => Distribute::SpaceBetween,
            K::SpaceAround => Distribute::SpaceAround,
            K::SpaceEvenly => Distribute::SpaceEvenly,
            K::Start | K::SelfStart | K::Left => physical_start(true),
            K::End | K::SelfEnd => physical_start(false),
            K::Right => physical_start(self.row),
            _ => Distribute::Start,
        }
    }
}

/// The used `align-self` of an item (§8.3).
#[derive(Clone, Copy, Debug, PartialEq)]
enum CrossAlign {
    Stretch,
    /// Toward cross-start (flex-relative).
    Start,
    End,
    Center,
    Baseline,
}

fn cross_align(axes: &Axes, container: &ComputedStyle, item: &ComputedStyle) -> CrossAlign {
    use AlignKeyword as K;
    let k = match item.align_self {
        K::Auto => container.align_items,
        k => k,
    };
    let physical = |at_start: bool| {
        if at_start != axes.cross_reverse {
            CrossAlign::Start
        } else {
            CrossAlign::End
        }
    };
    match k {
        K::Normal | K::Stretch | K::Auto => CrossAlign::Stretch,
        K::Center => CrossAlign::Center,
        K::FlexEnd => CrossAlign::End,
        // Baseline alignment needs a horizontal main axis (the items'
        // baselines are horizontal); otherwise it falls back to
        // `flex-start` (§8.3).
        K::Baseline if axes.row => CrossAlign::Baseline,
        // M1 follow-up: last-baseline alignment; aligned as `flex-end`,
        // its fallback alignment (CSS Box Alignment 3 §9.1).
        K::LastBaseline => CrossAlign::End,
        K::Start | K::SelfStart | K::Left => physical(true),
        K::End | K::SelfEnd => physical(false),
        K::Right => physical(axes.row),
        _ => CrossAlign::Start,
    }
}

// ---------------------------------------------------------------------------
// Item layout with imposed sizes, cached
// ---------------------------------------------------------------------------

fn bits(v: f32) -> u32 {
    sane(v).to_bits()
}

/// Lays out flex item `id` in containing block `cb` with the given
/// imposed content sizes (shrink-to-fit where none is imposed). The
/// fragment's border box is at the origin.
fn layout_item(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    cb: &ContainingBlock,
    depth: usize,
    forced: ForcedSize,
) -> LayoutResult {
    let forced = ForcedSize {
        width: forced.width.map(sane_non_negative),
        height: forced.height.map(sane_non_negative),
    };
    let key = (
        id,
        bits(cb.width),
        cb.height.map(bits),
        forced.width.map(bits),
        forced.height.map(bits),
    );
    if let Some(hit) = ctx.flex.cache.get(&key).cloned() {
        // Restore the positions this layout had: a later layout of an
        // ancestor may have moved shared descendants.
        for (f, p) in hit.positions {
            ctx.set_position(f, p);
        }
        return hit.result;
    }
    let previous = ctx.flex.forced.insert(id, forced);
    let mut r = super::layout_atomic_inline(ctx, id, cb, depth);
    match previous {
        Some(p) => {
            ctx.flex.forced.insert(id, p);
        }
        None => {
            ctx.flex.forced.remove(&id);
        }
    }
    if let Some(b) = ctx.get(id)
        && matches!(b.kind, BoxKind::Replaced(_))
        && (forced.width.is_some() || forced.height.is_some())
    {
        resize_replaced(ctx, b, cb, forced, &mut r);
    }
    if let Some(f) = r.fragment
        && ctx.flex.cache.len() < MAX_CACHE_ENTRIES
        && let Some(positions) = subtree_positions(ctx, f)
    {
        ctx.flex.cache.insert(
            key,
            CachedLayout {
                result: r.clone(),
                positions,
            },
        );
    }
    r
}

/// The positions of every descendant of `f` (not `f` itself), or `None`
/// when the subtree is too large to cache.
fn subtree_positions(ctx: &LayoutCtx<'_>, f: FragmentId) -> Option<Vec<(FragmentId, Point)>> {
    let mut out = Vec::new();
    let mut stack: Vec<FragmentId> = ctx.frag(f)?.children.clone();
    while let Some(c) = stack.pop() {
        if out.len() >= MAX_CACHED_SUBTREE {
            return None;
        }
        let Some(fr) = ctx.frag(c) else { continue };
        out.push((c, fr.border_box.origin()));
        stack.extend(fr.children.iter().copied());
    }
    Some(out)
}

/// Applies imposed sizes to a replaced item, keeping its aspect ratio for
/// the dimension that is not imposed (CSS 2.2 §10.3.2, §10.6.2).
fn resize_replaced(
    ctx: &mut LayoutCtx<'_>,
    b: &LayoutBox,
    cb: &ContainingBlock,
    forced: ForcedSize,
    r: &mut LayoutResult,
) {
    let style = b.style.clone();
    let edges = box_edges(&style, cb.width);
    let bp = edges.border_padding();
    let natural = replaced::natural_size(ctx, b);
    let used = replaced::used_size(&style, natural, cb, true, bp);
    let ratio = natural.ratio.filter(|r| r.is_finite() && *r > 0.0);
    let min_w = min_size(&style, &style.min_width, Some(cb.width), bp.horizontal());
    let max_w = max_size(&style, &style.max_width, Some(cb.width), bp.horizontal()).max(min_w);
    let min_h = min_size(&style, &style.min_height, cb.height, bp.vertical());
    let max_h = max_size(&style, &style.max_height, cb.height, bp.vertical()).max(min_h);
    let height_set = height_px(&style, cb.height, bp.vertical()).is_some();
    let width_set = definite(&style, &style.width, Some(cb.width), bp.horizontal()).is_some();
    let (w, h) = match (forced.width, forced.height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => {
            let h = match ratio {
                Some(r) if !height_set => clamp_min_max(w / r, min_h, max_h),
                _ => used.height,
            };
            (w, h)
        }
        (None, Some(h)) => {
            let w = match ratio {
                Some(r) if !width_set => clamp_min_max(h * r, min_w, max_w),
                _ => used.width,
            };
            (w, h)
        }
        (None, None) => return,
    };
    let size = Size::new(
        sane_non_negative(w) + bp.horizontal(),
        sane_non_negative(h) + bp.vertical(),
    );
    if let Some(f) = r.fragment
        && let Some(fr) = ctx.frag_mut(f)
    {
        let o = fr.border_box.origin();
        fr.border_box = Rect::new(o.x, o.y, size.width, size.height);
        fr.padding_box = fr.border_box.deflate(&edges.border);
        fr.content_box = fr.padding_box.deflate(&edges.padding);
        fr.scrollable_overflow = fr.border_box;
    }
    r.size = size;
}

// ---------------------------------------------------------------------------
// The algorithm
// ---------------------------------------------------------------------------

/// One flex item while the container is laid out. Sizes are content-box
/// sizes unless named "outer".
struct Item {
    id: BoxId,
    style: Arc<ComputedStyle>,
    /// Border plus padding along the main and cross axes.
    bp_main: f32,
    bp_cross: f32,
    /// Margins along the main axis (left/top, right/bottom); `None` is
    /// `auto`.
    m_main: (Option<f32>, Option<f32>),
    /// Margins along the cross axis (top/left, bottom/right).
    m_cross: (Option<f32>, Option<f32>),
    grow: f64,
    shrink: f64,
    base: f32,
    hypo: f32,
    min_main: f32,
    max_main: f32,
    target: f32,
    frozen: bool,
    /// The imposed width an item of a column container is laid out at
    /// before stretching (`None`: fit-content).
    cross_measure: Option<f32>,
    cross_auto: bool,
    min_cross: f32,
    max_cross: f32,
    hypo_cross: f32,
    cross: f32,
    align: CrossAlign,
    result: LayoutResult,
    /// The baseline from the item's border-box top (row containers).
    baseline: f32,
}

impl Item {
    fn margins_main(&self) -> f32 {
        self.m_main.0.unwrap_or(0.0) + self.m_main.1.unwrap_or(0.0)
    }
    fn margins_cross(&self) -> f32 {
        self.m_cross.0.unwrap_or(0.0) + self.m_cross.1.unwrap_or(0.0)
    }
    fn outer_base(&self) -> f32 {
        self.base + self.bp_main + self.margins_main()
    }
    fn outer_hypo(&self) -> f32 {
        self.hypo + self.bp_main + self.margins_main()
    }
    fn outer_target(&self) -> f32 {
        self.target + self.bp_main + self.margins_main()
    }
    fn outer_cross(&self) -> f32 {
        self.cross + self.bp_cross + self.margins_cross()
    }
    fn outer_hypo_cross(&self) -> f32 {
        self.hypo_cross + self.bp_cross + self.margins_cross()
    }
    fn has_auto_cross_margin(&self) -> bool {
        self.m_cross.0.is_none() || self.m_cross.1.is_none()
    }
    /// Whether the item takes part in baseline alignment (§8.3).
    fn baseline_aligned(&self) -> bool {
        self.align == CrossAlign::Baseline && !self.has_auto_cross_margin()
    }
}

/// One flex line: a range of items, its cross size and position.
#[derive(Clone, Debug)]
struct Line {
    start: usize,
    end: usize,
    cross: f32,
    /// The largest distance from an item's outer cross-start edge to its
    /// baseline, among baseline-aligned items.
    max_ascent: f32,
    /// Offset of the line's cross-start edge from the content box's
    /// flex-relative cross-start edge.
    pos: f32,
}

/// What the container needs to know about itself while placing items.
struct Container<'s> {
    style: &'s ComputedStyle,
    axes: Axes,
    wrap: bool,
    item_cb: ContainingBlock,
    content_width: f32,
    main_gap: f32,
    /// The definite inner cross size (single-line containers stretch
    /// items to it before line sizing, §9.4 step 8).
    cross_definite: Option<f32>,
}

/// Lays out a flex container (§9).
pub(crate) fn layout_flex_container(
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
    // §9.2 step 2: the available space. The container's own width comes
    // from block layout (or shrink-to-fit with the §9.9 intrinsic sizes).
    let hl = solve_width(ctx, id, &style, cb.width, &edges, mode, depth);
    let content_width = hl.content_width;
    let min_h = min_size(&style, &style.min_height, cb.height, bp.vertical());
    let max_h = max_size(&style, &style.max_height, cb.height, bp.vertical()).max(min_h);
    let definite_height = ctx.flex.forced(id).height.or_else(|| {
        height_px(&style, cb.height, bp.vertical()).map(|h| clamp_min_max(h, min_h, max_h))
    });
    let axes = Axes {
        row: matches!(
            style.flex_direction,
            FlexDirection::Row | FlexDirection::RowReverse
        ),
        main_reverse: matches!(
            style.flex_direction,
            FlexDirection::RowReverse | FlexDirection::ColumnReverse
        ),
        cross_reverse: style.flex_wrap == FlexWrap::WrapReverse,
    };
    let wrap = style.flex_wrap != FlexWrap::Nowrap;
    let column_gap = gap(&style.column_gap, Some(content_width));
    let row_gap = gap(&style.row_gap, definite_height);
    let (main_gap, cross_gap) = if axes.row {
        (column_gap, row_gap)
    } else {
        (row_gap, column_gap)
    };
    let container = Container {
        style: &style,
        axes,
        wrap,
        item_cb: ContainingBlock {
            width: content_width,
            height: definite_height,
        },
        content_width,
        main_gap,
        cross_definite: if axes.row {
            definite_height
        } else {
            Some(content_width)
        },
    };
    let inner_main_definite = if axes.row {
        Some(content_width)
    } else {
        definite_height
    };

    // §9.1 / §4.1: the flex items, in order-modified document order
    // (§5.4); absolutely positioned children are not flex items.
    let mut order: Vec<(i32, usize, BoxId)> = Vec::new();
    let mut placeholders: Vec<BoxId> = Vec::new();
    for (i, &c) in b.children.iter().enumerate() {
        let Some(cbox) = ctx.get(c) else { continue };
        if positioned::is_absolutely_positioned(cbox) {
            // Laid out by the positioning module.
            continue;
        }
        if cbox.style.is_absolutely_positioned() {
            placeholders.push(c);
            continue;
        }
        if !matches!(cbox.kind, BoxKind::Container { .. } | BoxKind::Replaced(_)) {
            continue;
        }
        order.push((cbox.style.order, i, c));
    }
    order.sort_by_key(|&(o, i, _)| (o, i));

    // §9.2 step 3: flex base sizes and hypothetical main sizes.
    let mut items: Vec<Item> = Vec::with_capacity(order.len());
    for &(_, _, c) in &order {
        if ctx.exhausted() {
            ctx.truncated = true;
            break;
        }
        if let Some(item) = new_item(ctx, &container, c, inner_main_definite, depth) {
            items.push(item);
        }
    }

    // §9.3: collect items into flex lines.
    let avail_main = match inner_main_definite {
        Some(m) => m,
        None if max_h < MAX_COORD => max_h,
        None => f32::INFINITY,
    };
    let mut lines = collect_lines(&items, wrap, avail_main, main_gap);

    // §9.2 step 4 (column containers with an indefinite height): the main
    // size is the largest line's sum of outer hypothetical main sizes.
    let inner_main = match inner_main_definite {
        Some(m) => m,
        None => {
            let longest = lines
                .iter()
                .map(|l| {
                    let line = items.get(l.start..l.end).unwrap_or(&[]);
                    line_sum(line, main_gap, Item::outer_hypo)
                })
                .fold(0.0f32, f32::max);
            clamp_min_max(longest, min_h, max_h)
        }
    };

    // §9.7: resolve flexible lengths, line by line.
    for l in &lines {
        if let Some(line_items) = items.get_mut(l.start..l.end) {
            resolve_flexible_lengths(line_items, inner_main, main_gap);
        }
    }

    // §9.4 steps 7–8: hypothetical cross sizes and line cross sizes.
    for item in &mut items {
        hypothetical_cross(ctx, &container, item, depth);
    }
    for l in &mut lines {
        let line_items = items.get(l.start..l.end).unwrap_or(&[]);
        size_line(l, line_items, &container, axes.row, min_h, max_h);
    }

    // The container's inner cross size (§9.4 step 15 for row containers;
    // column containers have a definite width).
    let lines_total = lines.iter().map(|l| l.cross).sum::<f32>()
        + cross_gap * lines.len().saturating_sub(1) as f32;
    let inner_cross = match container.cross_definite {
        Some(c) => c,
        None => clamp_min_max(sane(lines_total), min_h, max_h),
    };

    // §9.4 step 9 / §8.4: `align-content` (multi-line containers only).
    let free_cross = inner_cross - lines_total;
    let mut distribution = axes.align_content(style.align_content);
    if wrap
        && matches!(
            style.align_content,
            AlignKeyword::Normal | AlignKeyword::Stretch
        )
    {
        if free_cross > 0.0 && !lines.is_empty() {
            let extra = free_cross / lines.len() as f32;
            for l in &mut lines {
                l.cross += extra;
            }
        }
        distribution = if axes.cross_reverse {
            Distribute::End
        } else {
            Distribute::Start
        };
    }
    let lines_total = lines.iter().map(|l| l.cross).sum::<f32>()
        + cross_gap * lines.len().saturating_sub(1) as f32;
    let (lead, between) = if wrap {
        distribution.offsets(inner_cross - lines_total, lines.len())
    } else {
        (0.0, 0.0)
    };
    let mut pos = lead;
    for l in &mut lines {
        l.pos = sane(pos);
        pos += l.cross + cross_gap + between;
    }

    // §9.4 step 11: stretch items; then lay out every item at its used
    // size.
    for l in &lines {
        for item in items.get_mut(l.start..l.end).unwrap_or(&mut []) {
            final_cross(ctx, &container, item, l.cross, depth);
        }
    }

    // §9.5 and §9.6: main- and cross-axis alignment, then placement.
    let inner_height = if axes.row { inner_cross } else { inner_main };
    let content_height = clamp_min_max(definite_height.unwrap_or(inner_height), min_h, max_h);
    let inner_main_final = if axes.row {
        content_width
    } else {
        content_height
    };
    let inner_cross_final = if axes.row {
        content_height
    } else {
        content_width
    };
    let offset = Point::new(bp.left, bp.top);
    let mut children: Vec<FragmentId> = Vec::with_capacity(items.len() + placeholders.len());
    let mut first_baseline = None;
    let mut last_baseline = None;
    for (li, l) in lines.iter().enumerate() {
        let Some(line_items) = items.get_mut(l.start..l.end) else {
            continue;
        };
        let placed = place_line(
            ctx,
            &container,
            line_items,
            l,
            inner_main_final,
            inner_cross_final,
            offset,
        );
        children.extend(placed.fragments);
        if li == 0 {
            first_baseline = placed.first_baseline;
        }
        if placed.last_baseline.is_some() {
            last_baseline = placed.last_baseline;
        }
    }

    // §4.1: absolutely positioned children, at their static position.
    for c in placeholders {
        if let Some(f) = place_placeholder(
            ctx,
            &container,
            c,
            inner_main_final,
            inner_cross_final,
            offset,
            depth,
        ) {
            children.push(f);
        }
    }

    let size = Size::new(
        content_width + bp.horizontal(),
        content_height + bp.vertical(),
    );
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
        b.parent.is_none(),
    );
    let mt = edges.margin.top.unwrap_or(0.0);
    let mb = edges.margin.bottom.unwrap_or(0.0);
    LayoutResult {
        fragment,
        size,
        margin: Edges::new(mt, hl.margin_right, mb, hl.margin_left),
        collapsed_top: CollapsedMargin::new(mt),
        collapsed_bottom: CollapsedMargin::new(mb),
        collapses_through: false,
        first_baseline: first_baseline.map(sane),
        last_baseline: last_baseline.map(sane),
    }
}

/// Builds an item and determines its flex base size, hypothetical main
/// size and min/max main sizes (§9.2 step 3, §4.5, §7.1).
fn new_item(
    ctx: &mut LayoutCtx<'_>,
    container: &Container<'_>,
    id: BoxId,
    inner_main_definite: Option<f32>,
    depth: usize,
) -> Option<Item> {
    let b = ctx.get(id)?;
    let style = b.style.clone();
    let row = container.axes.row;
    let cw = container.content_width;
    // Percentage margins and padding refer to the containing block's
    // inline size (CSS Box Model 3 §5.3, §6.2).
    let edges = box_edges(&style, cw);
    let bpe = edges.border_padding();
    let (bp_main, bp_cross) = if row {
        (bpe.horizontal(), bpe.vertical())
    } else {
        (bpe.vertical(), bpe.horizontal())
    };
    let Margins {
        top,
        right,
        bottom,
        left,
    } = edges.margin;
    let (m_main, m_cross) = if row {
        ((left, right), (top, bottom))
    } else {
        ((top, bottom), (left, right))
    };
    // Row containers: the definite height; column containers: the width.
    let cross_basis = container.cross_definite;
    let (main_prop, cross_prop) = if row {
        (&style.width, &style.height)
    } else {
        (&style.height, &style.width)
    };
    let (min_main_prop, min_cross_prop) = if row {
        (&style.min_width, &style.min_height)
    } else {
        (&style.min_height, &style.min_width)
    };
    let (max_main_prop, max_cross_prop) = if row {
        (&style.max_width, &style.max_height)
    } else {
        (&style.max_height, &style.max_width)
    };
    let main_size = definite(&style, main_prop, inner_main_definite, bp_main);
    let cross_size = definite(&style, cross_prop, cross_basis, bp_cross);
    let max_main = max_size(&style, max_main_prop, inner_main_definite, bp_main);
    let min_cross = min_size(&style, min_cross_prop, cross_basis, bp_cross);
    let max_cross = max_size(&style, max_cross_prop, cross_basis, bp_cross).max(min_cross);
    let align = cross_align(&container.axes, container.style, &style);

    // §7.2.3: `flex-basis: auto` uses the main size property; `content`
    // and an indefinite size mean content-based sizing (§9.2 step 3 E).
    let basis = match &style.flex_basis {
        FlexBasis::Content => None,
        FlexBasis::Size(CssSize::Auto) => main_size,
        FlexBasis::Size(s) => definite(&style, s, inner_main_definite, bp_main),
    };

    let m_cross_auto = m_cross.0.is_none() || m_cross.1.is_none();
    // The width a column item is measured at: its definite width, the
    // line's width when it stretches in a single-line container (§9.8
    // item 1), else fit-content (§9.2 step 3 E).
    let cross_measure = if row {
        None
    } else if let Some(w) = cross_size {
        Some(clamp_min_max(w, min_cross, max_cross))
    } else if align == CrossAlign::Stretch && !container.wrap && !m_cross_auto {
        let outer = m_cross.0.unwrap_or(0.0) + m_cross.1.unwrap_or(0.0) + bp_cross;
        Some(clamp_min_max((cw - outer).max(0.0), min_cross, max_cross))
    } else {
        None
    };

    let auto_min = matches!(min_main_prop, CssSize::Auto) && !is_scroll_container(&style);
    let content = if row {
        let s = content_intrinsic(ctx, id, depth + 1);
        Some((s.min, s.max))
    } else if basis.is_none() || auto_min {
        // The content height at the item's measuring width.
        let r = layout_item(
            ctx,
            id,
            &container.item_cb,
            depth + 1,
            ForcedSize {
                width: cross_measure,
                height: None,
            },
        );
        let h = (r.size.height - bp_main).max(0.0);
        Some((h, h))
    } else {
        None
    };
    let base = match (basis, content) {
        (Some(v), _) => v,
        (None, Some((_, max))) => max,
        (None, None) => 0.0,
    };
    // §4.5: the content-based minimum size.
    let min_main = if auto_min {
        let content_suggestion = content.map_or(0.0, |(min, _)| min).min(max_main);
        match main_size {
            Some(s) => s.min(max_main).min(content_suggestion),
            None => content_suggestion,
        }
    } else {
        min_size(&style, min_main_prop, inner_main_definite, bp_main)
    };
    let max_main = max_main.max(min_main);
    let base = sane_non_negative(base);
    let hypo = clamp_min_max(base, min_main, max_main);
    Some(Item {
        id,
        grow: factor(style.flex_grow),
        shrink: factor(style.flex_shrink),
        style,
        bp_main,
        bp_cross,
        m_main,
        m_cross,
        base,
        hypo,
        min_main,
        max_main,
        target: hypo,
        frozen: false,
        cross_measure,
        cross_auto: cross_size.is_none(),
        min_cross,
        max_cross,
        hypo_cross: 0.0,
        cross: 0.0,
        align,
        result: LayoutResult::default(),
        baseline: 0.0,
    })
}

/// The content-box min-content and max-content widths of a box,
/// ignoring its own `width`, `min-width` and `max-width` (for content
/// size suggestions and content-based flex base sizes).
fn content_intrinsic(ctx: &mut LayoutCtx<'_>, id: BoxId, depth: usize) -> IntrinsicSizes {
    let Some(b) = ctx.get(id) else {
        return IntrinsicSizes::default();
    };
    if depth > crate::box_tree::MAX_BOX_DEPTH * 2 {
        return IntrinsicSizes::default();
    }
    match &b.kind {
        BoxKind::Replaced(_) => {
            let w = replaced::intrinsic_width(ctx, id);
            IntrinsicSizes { min: w, max: w }
        }
        BoxKind::Container {
            inline_children: true,
            ..
        } => inline::intrinsic_content(ctx, id, depth),
        BoxKind::Container {
            formatting: Formatting::Flex,
            ..
        } => intrinsic_content(ctx, id, depth),
        BoxKind::Container { .. } => {
            let mut acc = IntrinsicSizes::default();
            for &c in &b.children {
                let s = intrinsic_sizes(ctx, c, depth + 1);
                acc.min = acc.min.max(s.min);
                acc.max = acc.max.max(s.max);
            }
            acc.max = acc.max.max(acc.min);
            acc
        }
        _ => IntrinsicSizes::default(),
    }
}

/// The sum of `size` over `items` plus the gaps between them.
fn line_sum(items: &[Item], gap: f32, size: fn(&Item) -> f32) -> f32 {
    let n = items.len();
    sane(items.iter().map(size).sum::<f32>() + gap * n.saturating_sub(1) as f32)
}

/// §9.3: collects items into flex lines. A line holds at least one item
/// and at most [`MAX_ITEMS_PER_LINE`].
fn collect_lines(items: &[Item], wrap: bool, avail_main: f32, gap: f32) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut sum = 0.0f32;
    for (i, item) in items.iter().enumerate() {
        let o = item.outer_hypo();
        let full = i - start >= MAX_ITEMS_PER_LINE;
        let overflows = wrap && sum + gap + o > avail_main + EPSILON;
        if i > start && (full || overflows) {
            lines.push(Line {
                start,
                end: i,
                cross: 0.0,
                max_ascent: 0.0,
                pos: 0.0,
            });
            start = i;
            sum = o;
        } else if i > start {
            sum += gap + o;
        } else {
            sum = o;
        }
    }
    if start < items.len() || lines.is_empty() {
        lines.push(Line {
            start,
            end: items.len(),
            cross: 0.0,
            max_ascent: 0.0,
            pos: 0.0,
        });
    }
    lines
}

/// §9.7: resolves the flexible lengths of one line's items into their
/// target main sizes. Terminates: every iteration freezes at least one
/// item, and the loop is also capped at the item count.
fn resolve_flexible_lengths(items: &mut [Item], inner_main: f32, gap: f32) {
    let n = items.len();
    if n == 0 {
        return;
    }
    let gaps = f64::from(gap) * (n - 1) as f64;
    let inner = f64::from(sane(inner_main));
    // Step 1: grow or shrink.
    let hypo_sum: f64 = items.iter().map(|i| f64::from(i.outer_hypo())).sum();
    let growing = hypo_sum + gaps < inner;
    // Step 2: size inflexible items.
    for it in items.iter_mut() {
        it.target = it.hypo;
        let f = if growing { it.grow } else { it.shrink };
        it.frozen = f == 0.0 || (growing && it.base > it.hypo) || (!growing && it.base < it.hypo);
    }
    let free = |items: &[Item]| -> f64 {
        let used: f64 = items
            .iter()
            .map(|i| {
                f64::from(if i.frozen {
                    i.outer_target()
                } else {
                    i.outer_base()
                })
            })
            .sum();
        inner - gaps - used
    };
    // Step 3: initial free space.
    let initial_free = free(items);
    // Step 4: loop.
    for _ in 0..=n {
        if items.iter().all(|i| i.frozen) {
            break;
        }
        // b. remaining free space.
        let mut remaining = free(items);
        let sum_factors: f64 = items
            .iter()
            .filter(|i| !i.frozen)
            .map(|i| if growing { i.grow } else { i.shrink })
            .sum();
        if sum_factors < 1.0 {
            let v = initial_free * sum_factors;
            if v.abs() < remaining.abs() {
                remaining = v;
            }
        }
        if !remaining.is_finite() {
            remaining = 0.0;
        }
        // c. distribute.
        if remaining != 0.0 {
            if growing {
                if sum_factors > 0.0 {
                    for it in items.iter_mut().filter(|i| !i.frozen) {
                        let share = remaining * it.grow / sum_factors;
                        it.target = sane((f64::from(it.base) + share) as f32);
                    }
                }
            } else {
                let sum_scaled: f64 = items
                    .iter()
                    .filter(|i| !i.frozen)
                    .map(|i| i.shrink * f64::from(i.base))
                    .sum();
                for it in items.iter_mut().filter(|i| !i.frozen) {
                    it.target = if sum_scaled > 0.0 {
                        let ratio = it.shrink * f64::from(it.base) / sum_scaled;
                        sane((f64::from(it.base) - remaining.abs() * ratio) as f32)
                    } else {
                        it.base
                    };
                }
            }
        } else {
            for it in items.iter_mut().filter(|i| !i.frozen) {
                it.target = it.base;
            }
        }
        // d. fix min/max violations.
        let mut total = 0.0f64;
        let mut violation = vec![0i8; n];
        for (it, v) in items.iter_mut().zip(violation.iter_mut()) {
            if it.frozen {
                continue;
            }
            let clamped = clamp_min_max(it.target, it.min_main, it.max_main).max(0.0);
            let d = f64::from(clamped) - f64::from(it.target);
            if d > 0.0 {
                *v = 1;
            } else if d < 0.0 {
                *v = -1;
            }
            total += d;
            it.target = clamped;
        }
        // e. freeze over-flexed items.
        for (it, &v) in items.iter_mut().zip(violation.iter()) {
            if it.frozen {
                continue;
            }
            if total == 0.0 || (total > 0.0 && v > 0) || (total < 0.0 && v < 0) {
                it.frozen = true;
            }
        }
    }
    for it in items.iter_mut() {
        it.target = sane_non_negative(it.target);
    }
}

/// §9.4 step 7: lays out an item at its used main size to find its
/// hypothetical cross size (and its baseline).
fn hypothetical_cross(ctx: &mut LayoutCtx<'_>, c: &Container<'_>, item: &mut Item, depth: usize) {
    let forced = if c.axes.row {
        ForcedSize {
            width: Some(item.target),
            height: None,
        }
    } else {
        ForcedSize {
            width: item.cross_measure,
            height: Some(item.target),
        }
    };
    let r = layout_item(ctx, item.id, &c.item_cb, depth + 1, forced);
    let size = if c.axes.row {
        r.size.height
    } else {
        r.size.width
    };
    item.hypo_cross = (size - item.bp_cross).max(0.0);
    item.cross = item.hypo_cross;
    item.baseline = r.first_baseline.unwrap_or(r.size.height);
    item.result = r;
}

/// §9.4 step 8: the cross size of a line.
fn size_line(l: &mut Line, items: &[Item], c: &Container<'_>, row: bool, min_h: f32, max_h: f32) {
    let mut max_ascent = 0.0f32;
    let mut max_descent = 0.0f32;
    let mut largest = 0.0f32;
    for it in items {
        if it.baseline_aligned() {
            let ascent = it.m_cross.0.unwrap_or(0.0) + it.baseline;
            max_ascent = max_ascent.max(ascent);
            max_descent = max_descent.max(it.outer_hypo_cross() - ascent);
        } else {
            largest = largest.max(it.outer_hypo_cross());
        }
    }
    l.max_ascent = max_ascent;
    if !c.wrap
        && let Some(cross) = c.cross_definite
    {
        l.cross = cross;
        return;
    }
    let mut cross = largest.max(max_ascent + max_descent).max(0.0);
    if !c.wrap && row {
        // Single-line: clamp to the container's min/max cross size.
        cross = clamp_min_max(cross, min_h, max_h);
    }
    l.cross = sane(cross);
}

/// §9.4 step 11: determines the used cross size (stretching) and lays the
/// item out at its final size.
fn final_cross(
    ctx: &mut LayoutCtx<'_>,
    c: &Container<'_>,
    item: &mut Item,
    line_cross: f32,
    depth: usize,
) {
    let stretch =
        item.align == CrossAlign::Stretch && item.cross_auto && !item.has_auto_cross_margin();
    if !stretch {
        return;
    }
    let cross = clamp_min_max(
        (line_cross - item.margins_cross() - item.bp_cross).max(0.0),
        item.min_cross,
        item.max_cross,
    );
    item.cross = sane_non_negative(cross);
    let forced = if c.axes.row {
        ForcedSize {
            width: Some(item.target),
            height: Some(item.cross),
        }
    } else {
        ForcedSize {
            width: Some(item.cross),
            height: Some(item.target),
        }
    };
    let r = layout_item(ctx, item.id, &c.item_cb, depth + 1, forced);
    item.baseline = r.first_baseline.unwrap_or(r.size.height);
    item.result = r;
}

/// The fragments and baselines of one placed line.
struct PlacedLine {
    fragments: Vec<FragmentId>,
    first_baseline: Option<f32>,
    last_baseline: Option<f32>,
}

/// §9.5 and §9.6: aligns one line's items on both axes and positions
/// their fragments (relative to the container's border box).
fn place_line(
    ctx: &mut LayoutCtx<'_>,
    c: &Container<'_>,
    items: &mut [Item],
    l: &Line,
    inner_main: f32,
    inner_cross: f32,
    offset: Point,
) -> PlacedLine {
    let axes = c.axes;
    let n = items.len();
    let used = line_sum(items, c.main_gap, Item::outer_target);
    let mut free = inner_main - used;
    // §9.5 step 12: `auto` margins absorb positive free space.
    let auto_count = items
        .iter()
        .map(|i| usize::from(i.m_main.0.is_none()) + usize::from(i.m_main.1.is_none()))
        .sum::<usize>();
    let auto_share = if free > 0.0 && auto_count > 0 {
        let s = free / auto_count as f32;
        free = 0.0;
        s
    } else {
        0.0
    };
    // §9.5 step 13: `justify-content`.
    let (lead, between) = axes.justify(c.style.justify_content).offsets(free, n);
    // The line's physical cross-start offset within the content box.
    let line_phys = if axes.cross_reverse {
        inner_cross - l.pos - l.cross
    } else {
        l.pos
    };
    let mut out = PlacedLine {
        fragments: Vec::with_capacity(n),
        first_baseline: None,
        last_baseline: None,
    };
    let mut p = lead;
    let mut first_participant: Option<f32> = None;
    for item in items.iter_mut() {
        let main_start_margin = item.m_main.0.unwrap_or(auto_share);
        let main_end_margin = item.m_main.1.unwrap_or(auto_share);
        let size_main = item.target + item.bp_main;
        let outer_main = size_main + main_start_margin + main_end_margin;
        let margin_box_main = if axes.main_reverse {
            inner_main - p - outer_main
        } else {
            p
        };
        let main_border = margin_box_main + main_start_margin;
        p += outer_main + c.main_gap + between;

        // §9.6: cross-axis alignment.
        let outer_cross = item.outer_cross();
        let cross_free = l.cross - outer_cross;
        let (cs_margin, cross_offset) = if item.has_auto_cross_margin() {
            // §9.6 step 14: auto margins absorb the free space.
            let both = item.m_cross.0.is_none() && item.m_cross.1.is_none();
            let start = match item.m_cross.0 {
                Some(m) => m,
                None if cross_free > 0.0 => {
                    if both {
                        cross_free / 2.0
                    } else {
                        cross_free
                    }
                }
                None => 0.0,
            };
            (start, 0.0)
        } else {
            let m0 = item.m_cross.0.unwrap_or(0.0);
            let off = match item.align {
                CrossAlign::Stretch | CrossAlign::Start => {
                    if axes.cross_reverse {
                        cross_free
                    } else {
                        0.0
                    }
                }
                CrossAlign::End => {
                    if axes.cross_reverse {
                        0.0
                    } else {
                        cross_free
                    }
                }
                CrossAlign::Center => cross_free / 2.0,
                CrossAlign::Baseline => l.max_ascent - (m0 + item.baseline),
            };
            (m0, off)
        };
        let cross_border = line_phys + cross_offset + cs_margin;
        let (x, y) = if axes.row {
            (main_border, cross_border)
        } else {
            (cross_border, main_border)
        };
        let Some(f) = item.result.fragment else {
            continue;
        };
        let pos = Point::new(sane(offset.x + x), sane(offset.y + y));
        ctx.set_position(f, pos);
        positioned::apply_relative(ctx, f, &item.style, &c.item_cb);
        out.fragments.push(f);
        // §8.5: the container's baselines.
        let first = item.result.first_baseline.map(|b| pos.y + b);
        if item.baseline_aligned() && first_participant.is_none() {
            first_participant = first;
        }
        if out.first_baseline.is_none() {
            out.first_baseline = first;
        }
        if let Some(b) = item.result.last_baseline {
            out.last_baseline = Some(pos.y + b);
        }
    }
    if first_participant.is_some() {
        out.first_baseline = first_participant;
    }
    out
}

/// §4.1: lays out an absolutely positioned child as a static placeholder
/// at its static position, the position it would have as the sole flex
/// item of the container.
// M1 follow-up: the positioning module takes these over once
// `positioned::is_absolutely_positioned` returns `true`.
fn place_placeholder(
    ctx: &mut LayoutCtx<'_>,
    c: &Container<'_>,
    id: BoxId,
    inner_main: f32,
    inner_cross: f32,
    offset: Point,
    depth: usize,
) -> Option<FragmentId> {
    let style = ctx.get(id)?.style.clone();
    let r = super::layout_atomic_inline(ctx, id, &c.item_cb, depth + 1);
    let f = r.fragment?;
    let outer = r.margin_box();
    let (outer_main, outer_cross) = if c.axes.row {
        (outer.width, outer.height)
    } else {
        (outer.height, outer.width)
    };
    let (lead, _) = c
        .axes
        .justify(c.style.justify_content)
        .offsets(inner_main - outer_main, 1);
    let main = if c.axes.main_reverse {
        inner_main - lead - outer_main
    } else {
        lead
    };
    let cross_free = inner_cross - outer_cross;
    let cross = match cross_align(&c.axes, c.style, &style) {
        CrossAlign::Center => cross_free / 2.0,
        CrossAlign::End => cross_free,
        _ if c.axes.cross_reverse => cross_free,
        _ => 0.0,
    };
    let (x, y) = if c.axes.row {
        (main, cross)
    } else {
        (cross, main)
    };
    let pos = Point::new(
        sane(offset.x + x + r.margin.left),
        sane(offset.y + y + r.margin.top),
    );
    ctx.set_position(f, pos);
    Some(f)
}

/// The intrinsic content widths of flex container `id` (§9.9.1): for a
/// row container the sum of its items' contributions (the largest one
/// for the min-content size of a multi-line container), for a column
/// container the largest contribution. Item contributions are limited by
/// the flex base size of items that cannot grow or shrink.
pub(crate) fn intrinsic_content(
    ctx: &mut LayoutCtx<'_>,
    id: BoxId,
    depth: usize,
) -> IntrinsicSizes {
    let Some(b) = ctx.get(id) else {
        return IntrinsicSizes::default();
    };
    if depth > crate::box_tree::MAX_BOX_DEPTH * 2 {
        return IntrinsicSizes::default();
    }
    let style = &b.style;
    let row = matches!(
        style.flex_direction,
        FlexDirection::Row | FlexDirection::RowReverse
    );
    let wrap = style.flex_wrap != FlexWrap::Nowrap;
    let main_gap = gap(&style.column_gap, None);
    let mut acc = IntrinsicSizes::default();
    let mut n = 0usize;
    for &c in &b.children {
        let Some(cbox) = ctx.get(c) else { continue };
        if cbox.style.is_absolutely_positioned()
            || !matches!(cbox.kind, BoxKind::Container { .. } | BoxKind::Replaced(_))
        {
            continue;
        }
        let mut s = intrinsic_sizes(ctx, c, depth + 1);
        if row {
            let cs = &cbox.style;
            let e = box_edges(cs, 0.0);
            let bp = e.border_padding().horizontal();
            let outer = bp + e.margin.left.unwrap_or(0.0) + e.margin.right.unwrap_or(0.0);
            let basis = match &cs.flex_basis {
                FlexBasis::Content => None,
                FlexBasis::Size(CssSize::Auto) => definite(cs, &cs.width, None, bp),
                FlexBasis::Size(sz) => definite(cs, sz, None, bp),
            };
            if let Some(basis) = basis {
                let min_w = min_size(cs, &cs.min_width, None, bp);
                let max_w = max_size(cs, &cs.max_width, None, bp);
                let basis = clamp_min_max(basis, min_w, max_w) + outer;
                if factor(cs.flex_grow) == 0.0 {
                    s.max = s.max.min(basis);
                    s.min = s.min.min(basis);
                }
                if factor(cs.flex_shrink) == 0.0 {
                    s.max = s.max.max(basis);
                    s.min = s.min.max(basis);
                }
            }
            acc.max += s.max;
            if wrap {
                acc.min = acc.min.max(s.min);
            } else {
                acc.min += s.min;
            }
        } else {
            acc.max = acc.max.max(s.max);
            acc.min = acc.min.max(s.min);
        }
        n += 1;
    }
    if row && n > 1 {
        let gaps = main_gap * (n - 1) as f32;
        acc.max += gaps;
        if !wrap {
            acc.min += gaps;
        }
    }
    IntrinsicSizes {
        min: sane_non_negative(acc.min),
        max: sane_non_negative(acc.max.max(acc.min)),
    }
}

#[cfg(test)]
mod tests;
