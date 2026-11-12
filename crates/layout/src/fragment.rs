//! The fragment tree: the output of layout (CSS Display 3 §1, "box
//! fragments").
//!
//! Every fragment has absolute positions in CSS px (relative to the
//! initial containing block's origin, i.e. the top-left of the document
//! before the viewport scrolls), the DOM node it came from for hit
//! testing, its computed style, and the stacking information paint needs
//! to order it (CSS 2.2 Appendix E).
//!
//! The tree mirrors the box tree, with line boxes inserted between a block
//! container and its inline content, and inline boxes broken into one
//! fragment per line. Children are in tree (document) order; paint sorts
//! them into painting order with [`Stacking`].

use std::ops::Range;
use std::sync::Arc;

use wb_dom::NodeId;
use wb_style::{ComputedStyle, PseudoElement};
use wb_text::{FontId, GlyphId, Synthesis};

use crate::box_tree::{BoxId, ReplacedKind};
use crate::geom::{Point, Rect};

/// The most fragments one layout produces. Content past the limit is not
/// laid out and [`FragmentTree::truncated`] is set.
pub const MAX_FRAGMENTS: usize = 1 << 19;

/// Identifies a fragment in its [`FragmentTree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FragmentId(u32);

impl FragmentId {
    /// The fragment's index in [`FragmentTree::fragments`].
    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) fn from_index(i: usize) -> Option<FragmentId> {
        u32::try_from(i).ok().map(FragmentId)
    }
}

/// How a box fragment takes part in its parent's layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BoxFragmentKind {
    /// A block-level box (or the root box).
    Block,
    /// One line's piece of a non-atomic inline box.
    Inline,
    /// An atomic inline-level box (`inline-block`, ...). Replaced elements
    /// use [`FragmentKind::Replaced`] instead.
    AtomicInline,
}

/// One glyph, positioned absolutely.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    /// The glyph in the run's font.
    pub id: GlyphId,
    /// The pen position: the glyph's origin on the baseline, with the
    /// shaper's offsets applied.
    pub origin: Point,
    /// The glyph's advance (after letter- and word-spacing and
    /// justification).
    pub advance: f32,
    /// The byte offset, in [`TextFragment::text`], of the cluster the glyph
    /// belongs to.
    pub cluster: usize,
}

/// A run of glyphs from one font face.
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRun {
    /// The face.
    pub font: FontId,
    /// The font size in CSS px.
    pub size: f32,
    /// Faux bold/oblique to apply when drawing.
    pub synthesis: Synthesis,
    /// The glyphs, left to right.
    pub glyphs: Vec<PositionedGlyph>,
}

/// A piece of text on one line.
#[derive(Clone, Debug, PartialEq)]
pub struct TextFragment {
    /// The text as rendered: after white-space processing and
    /// `text-transform`. Glyph clusters index into it.
    pub text: Arc<str>,
    /// The part of [`TextFragment::text`] this fragment shows.
    pub range: Range<usize>,
    /// The byte range in the source text (the DOM text node's data, or the
    /// pseudo-element's generated text) that this fragment shows.
    pub source_range: Range<usize>,
    /// Maps rendered offsets to source offsets: sorted `(rendered, source)`
    /// anchors; offsets between two anchors map linearly from the earlier
    /// one. See [`TextFragment::source_offset`].
    pub offset_map: Arc<[(usize, usize)]>,
    /// The absolute y of the alphabetic baseline.
    pub baseline: f32,
    /// The shaped glyphs. Empty when no installed font covers the text.
    pub runs: Vec<GlyphRun>,
}

impl TextFragment {
    /// Maps a byte offset in [`TextFragment::text`] to a byte offset in the
    /// source text node (for hit testing and selection).
    pub fn source_offset(&self, rendered: usize) -> usize {
        let i = self.offset_map.partition_point(|(r, _)| *r <= rendered);
        match i.checked_sub(1).and_then(|i| self.offset_map.get(i)) {
            Some((r, s)) => s.saturating_add(rendered - r),
            None => rendered,
        }
    }
}

/// A replaced element's content.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplacedFragment {
    /// What to draw in the content box.
    pub kind: ReplacedKind,
}

/// A line box (CSS 2.2 §9.4.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineFragment {
    /// The absolute y of the line's baseline (the root inline box's
    /// alphabetic baseline).
    pub baseline: f32,
}

/// What a fragment is.
#[derive(Clone, Debug, PartialEq)]
pub enum FragmentKind {
    /// A box: block, inline piece or atomic inline. Its children are
    /// blocks, line boxes, or (for inline pieces) inline content.
    Box(BoxFragmentKind),
    /// A line box. Its children are the inline content of one line.
    Line(LineFragment),
    /// Text on one line.
    Text(TextFragment),
    /// A replaced element (image, form control, embedded content).
    Replaced(ReplacedFragment),
    /// A list item's marker (CSS Lists 3 §2): an inline-level box whose
    /// children are its text (or a replaced image for `list-style-image`).
    Marker,
}

/// What paint needs to place a fragment in painting order (CSS 2.2
/// Appendix E, CSS Position 3 §5).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stacking {
    /// `position` is not `static`.
    pub positioned: bool,
    /// `z-index` as an integer, or `None` for `auto` (and for boxes where
    /// it does not apply).
    pub z_index: Option<i32>,
    /// The box creates a stacking context (positioned with an integer
    /// `z-index`, `position: fixed`/`sticky`, `opacity` below 1, or the
    /// root).
    pub creates_stacking_context: bool,
    /// `opacity`, 0..=1.
    pub opacity: f32,
    /// The box floats (paints in the float layer).
    pub float: bool,
}

impl Stacking {
    /// Stacking for a fragment that inherits everything from its parent
    /// (text, line boxes, anonymous boxes).
    pub const NONE: Stacking = Stacking {
        positioned: false,
        z_index: None,
        creates_stacking_context: false,
        opacity: 1.0,
        float: false,
    };

    pub(crate) fn from_style(style: &ComputedStyle, is_root: bool) -> Stacking {
        let positioned = !matches!(style.position, wb_style::Position::Static);
        let z_index = match style.z_index {
            wb_style::ZIndex::Integer(z) if positioned => Some(z),
            _ => None,
        };
        let opacity = if style.opacity.is_finite() {
            style.opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        Stacking {
            positioned,
            z_index,
            creates_stacking_context: is_root || style.creates_stacking_context(),
            opacity,
            float: style.is_floating(),
        }
    }
}

/// How a fragment clips its descendants (CSS Overflow 3 §3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    /// The overflow clip edge: the padding box, absolute.
    pub rect: Rect,
    /// Descendants are clipped horizontally (`overflow-x` is not
    /// `visible`).
    pub x: bool,
    /// Descendants are clipped vertically (`overflow-y` is not `visible`).
    pub y: bool,
    /// The box is a scroll container (`hidden`, `scroll` or `auto` on
    /// either axis, not just `clip`).
    pub scroll_container: bool,
}

/// One fragment.
#[derive(Clone, Debug)]
pub struct Fragment {
    /// What the fragment is.
    pub kind: FragmentKind,
    /// The DOM node, for hit testing: the element, the text node, or for a
    /// pseudo-element the originating element. `None` for anonymous boxes
    /// and line boxes.
    pub node: Option<NodeId>,
    /// The pseudo-element this fragment belongs to.
    pub pseudo: Option<PseudoElement>,
    /// The box this fragment was generated from (`None` for line boxes).
    pub box_id: Option<BoxId>,
    /// The computed style (for line boxes and text, the containing box's
    /// style).
    pub style: Arc<ComputedStyle>,
    /// The border box, absolute. For text it is the text's content area
    /// (ascent to descent, start to end advance).
    pub border_box: Rect,
    /// The padding box, absolute.
    pub padding_box: Rect,
    /// The content box, absolute.
    pub content_box: Rect,
    /// The offset `position: relative` applied (CSS 2.2 §9.4.3); already
    /// included in the rectangles above.
    pub relative_offset: Point,
    /// Stacking and painting-order information.
    pub stacking: Stacking,
    /// How the fragment clips its descendants, if it does.
    pub clip: Option<Clip>,
    /// The scroll offset applied to descendants of a scroll container
    /// (from [`crate::LayoutHost::scroll_offset`], clamped to the
    /// scrollable range). Zero otherwise.
    pub scroll_offset: Point,
    /// The scrollable overflow area (CSS Overflow 3 §2.2) of the box's
    /// contents and its own padding box, absolute, before scrolling.
    pub scrollable_overflow: Rect,
    /// The parent fragment.
    pub parent: Option<FragmentId>,
    /// The children, in tree order.
    pub children: Vec<FragmentId>,
}

/// The result of hit testing (see [`FragmentTree::hit_test`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The innermost fragment under the point.
    pub fragment: FragmentId,
    /// Its DOM node (the nearest ancestor fragment's node when the
    /// fragment itself is anonymous).
    pub node: Option<NodeId>,
    /// For text: the byte offset in the source text node of the caret
    /// position nearest the point.
    pub text_offset: Option<usize>,
}

/// The output of layout.
#[derive(Clone, Debug, Default)]
pub struct FragmentTree {
    pub(crate) fragments: Vec<Fragment>,
    pub(crate) root: Option<FragmentId>,
    pub(crate) viewport: Rect,
    pub(crate) document_overflow: Rect,
    pub(crate) truncated: bool,
}

impl FragmentTree {
    /// The root fragment (the root element's box), if anything rendered.
    pub fn root(&self) -> Option<FragmentId> {
        self.root
    }

    /// The fragment with id `id`.
    pub fn get(&self, id: FragmentId) -> Option<&Fragment> {
        self.fragments.get(id.index())
    }

    /// All fragments, indexed by [`FragmentId::index`], in tree order
    /// (pre-order): the root is first and every parent comes before its
    /// descendants.
    pub fn fragments(&self) -> &[Fragment] {
        &self.fragments
    }

    /// The number of fragments.
    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    /// Whether the tree is empty.
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// The initial containing block: the viewport rectangle at the origin.
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// The document's scrollable overflow area (the viewport's scroll
    /// range): the union of the viewport and everything laid out.
    pub fn document_overflow(&self) -> Rect {
        self.document_overflow
    }

    /// Whether content was dropped because of a resource limit
    /// ([`MAX_FRAGMENTS`], [`crate::MAX_BOX_DEPTH`], [`crate::MAX_BOXES`]).
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The children of `id` (empty for unknown ids).
    pub fn children(&self, id: FragmentId) -> &[FragmentId] {
        self.get(id).map(|f| f.children.as_slice()).unwrap_or(&[])
    }

    /// The fragments generated by `node`, in tree order.
    pub fn fragments_for_node(&self, node: NodeId) -> Vec<FragmentId> {
        self.fragments
            .iter()
            .enumerate()
            .filter(|(_, f)| f.node == Some(node))
            .filter_map(|(i, _)| FragmentId::from_index(i))
            .collect()
    }

    /// The topmost fragment at `point` (absolute, document coordinates),
    /// respecting overflow clips, scroll offsets and a basic painting
    /// order: positioned descendants above in-flow content, ordered by
    /// `z-index`, later siblings above earlier ones. Fragments with
    /// `visibility: hidden` are skipped (their visible descendants are
    /// still found).
    // M1 follow-up: full CSS 2.2 Appendix E ordering across nested
    // stacking contexts, and `pointer-events`.
    pub fn hit_test(&self, point: Point) -> Option<Hit> {
        let root = self.root?;
        // Points on the canvas outside every box hit the root element
        // (its background covers the canvas).
        let id = self
            .hit_in(root, point, 0)
            .or_else(|| self.document_overflow.contains(point).then_some(root))?;
        let frag = self.get(id)?;
        let mut node = frag.node;
        let mut cur = frag.parent;
        while node.is_none() {
            let Some(p) = cur.and_then(|c| self.get(c)) else {
                break;
            };
            node = p.node;
            cur = p.parent;
        }
        let text_offset = match &frag.kind {
            FragmentKind::Text(t) => Some(text_offset_at(t, frag, point.x)),
            _ => None,
        };
        Some(Hit {
            fragment: id,
            node,
            text_offset,
        })
    }

    fn hit_in(&self, id: FragmentId, point: Point, depth: usize) -> Option<FragmentId> {
        let f = self.get(id)?;
        if depth > crate::box_tree::MAX_BOX_DEPTH * 3 {
            return None;
        }
        // Descendants are clipped by this box's clip.
        let clipped_out = f.clip.is_some_and(|c| {
            (c.x && (point.x < c.rect.x || point.x >= c.rect.max_x()))
                || (c.y && (point.y < c.rect.y || point.y >= c.rect.max_y()))
        });
        if !clipped_out {
            // Children in reverse painting order.
            let mut kids: Vec<(usize, FragmentId)> =
                f.children.iter().copied().enumerate().collect();
            kids.sort_by_key(|(i, c)| (self.layer_of(*c), *i));
            for (_, c) in kids.iter().rev() {
                if let Some(hit) = self.hit_in(*c, point, depth + 1) {
                    return Some(hit);
                }
            }
        }
        let visible = matches!(f.style.visibility, wb_style::Visibility::Visible);
        let hittable = !matches!(f.kind, FragmentKind::Line(_));
        (visible && hittable && f.border_box.contains(point)).then_some(id)
    }

    /// A coarse painting layer: negative z-index, in-flow, floats,
    /// positioned `auto`/0, positive z-index (CSS 2.2 Appendix E steps).
    fn layer_of(&self, id: FragmentId) -> (i8, i64) {
        let Some(f) = self.get(id) else {
            return (0, 0);
        };
        let s = f.stacking;
        match (s.positioned, s.z_index) {
            (true, Some(z)) if z < 0 => (-1, i64::from(z)),
            (true, Some(z)) if z > 0 => (3, i64::from(z)),
            (true, _) => (2, 0),
            (false, _) if s.float => (1, 0),
            _ => (0, 0),
        }
    }
}

/// The source offset of the caret position nearest `x` in a text fragment.
fn text_offset_at(t: &TextFragment, frag: &Fragment, x: f32) -> usize {
    if t.runs.is_empty() {
        // No glyphs (no font covers the text): assume equal advances.
        let slice = t.text.get(t.range.clone()).unwrap_or("");
        let n = slice.chars().count();
        let w = frag.border_box.width;
        if n == 0 || w <= 0.0 {
            return t.source_offset(t.range.start);
        }
        let k = (((x - frag.border_box.x) / w) * n as f32)
            .round()
            .clamp(0.0, n as f32) as usize;
        let off = slice.char_indices().nth(k).map_or(slice.len(), |(i, _)| i);
        return t.source_offset(t.range.start + off);
    }
    let mut best = t.range.start;
    let mut best_d = (x - frag.border_box.x).abs();
    for g in t.runs.iter().flat_map(|r| r.glyphs.iter()) {
        // Caret before the cluster.
        let d = (x - g.origin.x).abs();
        if d < best_d {
            best_d = d;
            best = g.cluster;
        }
        // Caret after the cluster: the next char boundary.
        let after_x = g.origin.x + g.advance;
        let d = (x - after_x).abs();
        if d < best_d {
            best_d = d;
            best = next_boundary(&t.text, g.cluster).min(t.range.end);
        }
    }
    if (x - frag.border_box.max_x()).abs() < best_d {
        best = t.range.end;
    }
    t.source_offset(best)
}

fn next_boundary(text: &str, offset: usize) -> usize {
    text.get(offset..)
        .and_then(|s| s.chars().next())
        .map_or(offset, |c| offset + c.len_utf8())
}
