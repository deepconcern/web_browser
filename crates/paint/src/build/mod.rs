//! The display-list builder: turns a [`wb_layout::FragmentTree`] into a
//! [`DisplayList`] in CSS 2.2 Appendix E painting order.
//!
//! # Painting order
//!
//! Every stacking context (the root, positioned boxes with an integer
//! `z-index`, `position: fixed`/`sticky`, `opacity` below 1, as reported
//! by [`wb_layout::Stacking::creates_stacking_context`]) paints, in order
//! (CSS 2.2 Appendix E §E.2, CSS Position 3 §5):
//!
//! 1. its own background and borders (for the root: the canvas, see
//!    below);
//! 2. child stacking contexts with a negative `z-index`, most negative
//!    first, then in tree order;
//! 3. the backgrounds and borders of its in-flow, non-positioned,
//!    block-level descendants, in tree order;
//! 4. its non-positioned floats, each painted atomically as if it created
//!    a stacking context;
//! 5. its in-flow inline content: inline box backgrounds and borders,
//!    text (underline and overline, then the glyphs, then line-through,
//!    CSS Text Decoration 3 §3), replaced content, form controls, list
//!    markers, and inline-blocks painted atomically;
//! 6. positioned descendants with `z-index: auto` and stacking contexts
//!    with `z-index: 0` (or none, like `opacity` < 1), in tree order;
//!    a positioned `auto` box is painted as if it created a stacking
//!    context, but its positioned descendants and real stacking contexts
//!    belong to the enclosing one;
//! 7. child stacking contexts with a positive `z-index`.
//!
//! The builder relies only on [`wb_layout::Stacking`] (`positioned`,
//! `z_index`, `creates_stacking_context`, `opacity`, `float`) and on the
//! fragment kinds, so floats, tables, flex items and absolutely positioned
//! boxes take part without the builder knowing how they were laid out.
//!
//! # Clips, scrolling and opacity
//!
//! A box whose `overflow` is not `visible` clips its descendants to its
//! padding box, rounded by the inner border radii when both axes clip
//! (CSS Overflow 3 §3, CSS Backgrounds 3 §5.3); a single clipped axis is
//! clipped as a band. A clip applies to a descendant only when the
//! descendant's containing block chain goes through the clipping box: an
//! absolutely positioned box escapes clips between itself and its nearest
//! positioned ancestor, and a fixed one escapes every clip. Since boxes
//! are painted out of tree order, each painted item is wrapped in exactly
//! the clips that apply to it (open groups are reused across consecutive
//! items).
//!
//! Layout has already moved the contents of scroll containers by their
//! scroll offsets. The builder undoes that and wraps the contents in
//! [`DisplayItem::PushScroll`] with the (rounded) negated offset instead,
//! so the list stays in unscrolled page coordinates as its contract says.
//!
//! Stacking contexts with `opacity` below 1 are wrapped in an opacity
//! group.
//!
//! # Canvas background
//!
//! The root element's background, or when it is transparent with no
//! images and the root is `<html>`, the background of its first `<body>`
//! child, paints the whole canvas (CSS Backgrounds 3 §2.11.2): the
//! document's scrollable overflow area, which includes the viewport.
//! Background images keep the root element's box as their positioning
//! area. The element the background came from does not paint it again.
//!
//! # Visibility
//!
//! Boxes with `visibility` other than `visible` paint nothing and get no
//! hit regions (CSS Display 3 §4; such boxes are not hit-test targets per
//! CSS UI 4 `pointer-events`); their visible descendants still paint.
//!
//! # Hit tags
//!
//! Every item is tagged with [`fragment_tag`] of the fragment it paints:
//! the tag's value is the fragment's index in
//! [`wb_layout::FragmentTree::fragments`]. Text runs carry their text
//! fragment, so [`crate::Hit::text_offset`] is a byte offset in that
//! fragment's rendered text ([`wb_layout::TextFragment::text`]). Boxes
//! with a DOM node and nothing opaque covering their border box get an
//! invisible [`DisplayItem::HitRegion`], so clicks on empty parts of a
//! block or an inline element (a link) still find it. [`resolve_hit`] maps
//! a hit back to the fragment, its DOM node (the nearest ancestor's node
//! for anonymous fragments) and, for text, the offset in the source text
//! node. The shell finds links by walking up the DOM from that node.

mod content;
mod decor;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use wb_dom::{Document, NodeId};
use wb_image::Image;
use wb_layout::{Fragment, FragmentId, FragmentKind, FragmentTree};
use wb_text::FontContext;

use crate::geom::{Rect, RoundedRect};
use crate::list::{DisplayItem, DisplayList, HitTag, ListLimits};

/// The deepest nesting of fragments the builder descends into. Layout
/// bounds the box tree depth ([`wb_layout::MAX_BOX_DEPTH`]); line boxes
/// and inline pieces add at most a few levels per box.
const MAX_PAINT_DEPTH: usize = wb_layout::MAX_BOX_DEPTH * 4;

/// Images the page shows, supplied by the embedder. Both methods default
/// to "not loaded", which paints nothing for the image.
pub trait PaintResources {
    /// The decoded image of the replaced element `node` (`<img>`,
    /// `<input type=image>`).
    fn node_image(&self, node: NodeId) -> Option<Arc<Image>> {
        let _ = node;
        None
    }

    /// The decoded image at the absolute `url` (`background-image`).
    fn url_image(&self, url: &str) -> Option<Arc<Image>> {
        let _ = url;
        None
    }
}

/// [`PaintResources`] with no images loaded.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoResources;

impl PaintResources for NoResources {}

/// Options for [`build_display_list`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildOptions {
    /// The caps of the list being built.
    pub limits: ListLimits,
}

/// The tag the builder gives items painted for fragment `id`.
#[must_use]
pub fn fragment_tag(id: FragmentId) -> HitTag {
    HitTag(id.index() as u64)
}

/// What a [`crate::Hit`] on a built list points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HitTarget {
    /// The fragment that painted the item.
    pub fragment: FragmentId,
    /// Its DOM node, or the nearest ancestor fragment's node when the
    /// fragment is anonymous.
    pub node: Option<NodeId>,
    /// For text: the byte offset in the source text node (or the
    /// pseudo-element's generated text) of the caret position nearest the
    /// point.
    pub text_offset: Option<usize>,
}

/// Maps a hit on a list built from `tree` back to the fragment and DOM
/// node. Returns `None` for tags that name no fragment of `tree`.
#[must_use]
pub fn resolve_hit(tree: &FragmentTree, hit: &crate::Hit) -> Option<HitTarget> {
    let index = usize::try_from(hit.tag.0).ok()?;
    let frag = tree.fragments().get(index)?;
    let fragment = fragment_id(tree, index)?;
    let mut node = frag.node;
    let mut cur = frag.parent;
    let mut steps = 0;
    while node.is_none() && steps <= MAX_PAINT_DEPTH {
        let Some(p) = cur.and_then(|c| tree.get(c)) else {
            break;
        };
        node = p.node;
        cur = p.parent;
        steps += 1;
    }
    let text_offset = match (&frag.kind, hit.text_offset) {
        (FragmentKind::Text(t), Some(off)) => Some(t.source_offset(off)),
        _ => None,
    };
    Some(HitTarget {
        fragment,
        node,
        text_offset,
    })
}

/// The [`FragmentId`] of the fragment at `index`.
fn fragment_id(tree: &FragmentTree, index: usize) -> Option<FragmentId> {
    let frag = tree.fragments().get(index)?;
    match frag.parent {
        Some(p) => tree
            .children(p)
            .iter()
            .copied()
            .find(|c| c.index() == index),
        None => tree.root().filter(|r| r.index() == index),
    }
}

/// Builds the display list for `tree`, laid out from `doc`.
///
/// `fonts` is the font context layout shaped the text with (for
/// decoration metrics and form-control labels); `resources` supplies
/// decoded images. See the [module documentation](crate::build) for the
/// painting order, clipping and hit tags.
#[must_use]
pub fn build_display_list(
    doc: &Document,
    tree: &FragmentTree,
    fonts: &FontContext,
    resources: &dyn PaintResources,
    options: &BuildOptions,
) -> DisplayList {
    let mut b = Builder::new(doc, tree, fonts, resources, options);
    b.run();
    b.list.close_all();
    b.list
}

/// Per-fragment data computed before painting.
#[derive(Clone, Copy, Debug, Default)]
struct Info {
    /// The nearest fragment whose clip applies to this one.
    clip_link: Option<usize>,
    /// The nearest positioned ancestor.
    positioned_ancestor: Option<usize>,
    /// What to add to the fragment's rectangles to undo the scroll offsets
    /// layout applied to it.
    unshift: (f32, f32),
}

/// A clip (and maybe scroll) group the builder has open in the list.
#[derive(Clone, Copy, Debug)]
struct OpenClip {
    frag: usize,
    scrolled: bool,
}

/// Child stacking contexts and positioned descendants of one stacking
/// context, by layer.
#[derive(Default)]
struct Layers {
    negative: Vec<(i32, usize)>,
    zero: Vec<usize>,
    positive: Vec<(i32, usize)>,
}

pub(crate) struct Builder<'a> {
    doc: &'a Document,
    tree: &'a FragmentTree,
    frags: &'a [Fragment],
    fonts: &'a FontContext,
    res: &'a dyn PaintResources,
    list: DisplayList,
    info: Vec<Info>,
    open: Vec<OpenClip>,
    /// For each open opacity group, how many clips were open when it
    /// started; clips below that can't be closed inside the group.
    bases: Vec<usize>,
    /// The `clip_link` the open clips were last set up for.
    current: Option<Option<usize>>,
    /// Fragments whose background was propagated to the canvas.
    no_background: Vec<usize>,
}

impl<'a> Builder<'a> {
    fn new(
        doc: &'a Document,
        tree: &'a FragmentTree,
        fonts: &'a FontContext,
        res: &'a dyn PaintResources,
        options: &BuildOptions,
    ) -> Builder<'a> {
        let frags = tree.fragments();
        let mut b = Builder {
            doc,
            tree,
            frags,
            fonts,
            res,
            list: DisplayList::with_limits(options.limits),
            info: vec![Info::default(); frags.len()],
            open: Vec::new(),
            bases: Vec::new(),
            current: None,
            no_background: Vec::new(),
        };
        b.compute_info();
        b
    }

    fn frag(&self, i: usize) -> Option<&'a Fragment> {
        self.frags.get(i)
    }

    fn info(&self, i: usize) -> Info {
        self.info.get(i).copied().unwrap_or_default()
    }

    /// Fills [`Info`] in one pre-order pass (the arena is pre-order, so
    /// parents come first).
    fn compute_info(&mut self) {
        for i in 0..self.frags.len() {
            let Some(f) = self.frags.get(i) else { continue };
            let parent = f.parent.map(FragmentId::index);
            let pinfo = parent.map(|p| (self.info(p), self.frags.get(p)));
            let mut info = Info::default();
            if let Some((pi, Some(pf))) = pinfo {
                info.unshift = (
                    pi.unshift.0 + pf.scroll_offset.x,
                    pi.unshift.1 + pf.scroll_offset.y,
                );
                info.positioned_ancestor = if pf.stacking.positioned {
                    parent
                } else {
                    pi.positioned_ancestor
                };
            }
            // The box whose containing block chain the clips follow
            // (CSS Overflow 3 §3: a clip applies to descendants whose
            // containing block is the box or one of its descendants).
            let cb = match f.style.position {
                wb_style::Position::Fixed => None,
                wb_style::Position::Absolute => info.positioned_ancestor,
                _ => parent,
            };
            info.clip_link = cb.and_then(|c| {
                if self.frags.get(c).is_some_and(|cf| cf.clip.is_some()) {
                    Some(c)
                } else {
                    self.info(c).clip_link
                }
            });
            if let Some(slot) = self.info.get_mut(i) {
                *slot = info;
            }
        }
    }

    /// The painting entry point: canvas, then the root stacking context.
    fn run(&mut self) {
        let Some(root) = self.tree.root().map(FragmentId::index) else {
            return;
        };
        self.paint_canvas(root);
        self.paint_context(root, true, 0);
        self.set_clips(None);
    }

    // ----- Clip groups --------------------------------------------------

    /// Makes the open clip groups those that apply to a fragment whose
    /// `clip_link` is `link`.
    fn set_clips(&mut self, link: Option<usize>) {
        if self.current == Some(link) {
            return;
        }
        self.current = Some(link);
        let mut chain = Vec::new();
        let mut cur = link;
        while let Some(c) = cur {
            if chain.len() > MAX_PAINT_DEPTH {
                break;
            }
            chain.push(c);
            cur = self.info(c).clip_link;
        }
        chain.reverse();
        let base = self.bases.last().copied().unwrap_or(0);
        let common = self
            .open
            .iter()
            .zip(&chain)
            .take_while(|(o, c)| o.frag == **c)
            .count();
        if common >= base {
            self.pop_clips_to(common);
            for c in chain.get(common..).unwrap_or(&[]).to_vec() {
                self.push_clip(c);
            }
        } else {
            // A box escaping a clip that an enclosing opacity group was
            // opened under: keep the group's clips.
            // M1 follow-up: paint such boxes outside the group's clips.
            self.pop_clips_to(base);
            let kept: std::collections::HashSet<usize> = self.open.iter().map(|o| o.frag).collect();
            for c in chain {
                if !kept.contains(&c) {
                    self.push_clip(c);
                }
            }
        }
    }

    fn pop_clips_to(&mut self, len: usize) {
        while self.open.len() > len {
            let Some(o) = self.open.pop() else { break };
            if o.scrolled {
                self.list.push(DisplayItem::PopScroll);
            }
            self.list.push(DisplayItem::PopClip);
        }
    }

    fn push_clip(&mut self, c: usize) {
        let Some(f) = self.frag(c) else { return };
        let Some(clip) = f.clip else { return };
        let info = self.info(c);
        let rect = self.rect(&clip.rect, info.unshift);
        // A clip on one axis only is a band, unbounded on the other.
        const FAR: f32 = 2.0 * wb_layout::MAX_COORD;
        let (x, w) = if clip.x {
            (rect.x, rect.width)
        } else {
            (-FAR, 2.0 * FAR)
        };
        let (y, h) = if clip.y {
            (rect.y, rect.height)
        } else {
            (-FAR, 2.0 * FAR)
        };
        let band = Rect::new(x, y, w, h);
        let shape = if clip.x && clip.y {
            let geo = decor::BoxGeometry::of(self, c);
            RoundedRect::new(band, geo.inner_radii(decor::EdgeBox::Padding))
        } else {
            RoundedRect::from_rect(band)
        };
        self.list.push(DisplayItem::PushClip(shape));
        let mut scrolled = false;
        if clip.scroll_container {
            let round = |v: f32| -> i32 {
                let r = (-v).round();
                if r.is_finite() {
                    r.clamp(-1.0e9, 1.0e9) as i32
                } else {
                    0
                }
            };
            let (dx, dy) = (round(f.scroll_offset.x), round(f.scroll_offset.y));
            if dx != 0 || dy != 0 {
                self.list.push(DisplayItem::PushScroll { dx, dy });
                scrolled = true;
            }
        }
        self.open.push(OpenClip { frag: c, scrolled });
    }

    /// Opens the clips for fragment `i` before painting it.
    pub(crate) fn enter(&mut self, i: usize) {
        let link = self.info(i).clip_link;
        self.set_clips(link);
    }

    /// A layout rectangle, moved back to unscrolled page coordinates.
    pub(crate) fn rect(&self, r: &wb_layout::Rect, unshift: (f32, f32)) -> Rect {
        Rect::new(r.x + unshift.0, r.y + unshift.1, r.width, r.height)
    }

    pub(crate) fn tag(&self, i: usize) -> Option<HitTag> {
        Some(HitTag(i as u64))
    }

    // ----- Painting order ----------------------------------------------

    fn is_visible(f: &Fragment) -> bool {
        matches!(f.style.visibility, wb_style::Visibility::Visible)
    }

    /// Paints fragment `i` as a stacking context (`real`) or atomically as
    /// if it were one (floats, inline-blocks, positioned `z-index: auto`).
    fn paint_context(&mut self, i: usize, real: bool, depth: usize) {
        let Some(f) = self.frag(i) else { return };
        if depth > MAX_PAINT_DEPTH {
            return;
        }
        let opacity = real && f.stacking.opacity < 1.0;
        if opacity {
            self.enter(i);
            self.list.push(DisplayItem::PushOpacity(f.stacking.opacity));
            self.bases.push(self.open.len());
        }
        let mut layers = Layers::default();
        if real {
            self.collect(i, &mut layers, depth + 1);
            // Stable sorts keep tree order among equal z-indexes.
            layers.negative.sort_by_key(|(z, _)| *z);
            layers.positive.sort_by_key(|(z, _)| *z);
        }
        // Step 1.
        self.paint_decorations(i);
        // Step 2.
        for (_, c) in &layers.negative {
            self.paint_context(*c, true, depth + 1);
        }
        // Steps 3 to 5.
        self.paint_blocks(i, depth + 1);
        self.paint_floats(i, depth + 1);
        if matches!(f.kind, FragmentKind::Replaced(_)) {
            self.paint_replaced(i);
        }
        if matches!(f.kind, FragmentKind::Text(_)) {
            self.paint_text(i);
        }
        self.paint_inline(i, depth + 1);
        // Step 6.
        for c in &layers.zero {
            let sc = self
                .frag(*c)
                .is_some_and(|cf| cf.stacking.creates_stacking_context);
            self.paint_context(*c, sc, depth + 1);
        }
        // Step 7.
        for (_, c) in &layers.positive {
            self.paint_context(*c, true, depth + 1);
        }
        if opacity {
            let base = self.bases.pop().unwrap_or(0);
            self.pop_clips_to(base);
            self.current = None;
            self.list.push(DisplayItem::PopOpacity);
        }
    }

    /// Collects the stacking contexts and positioned descendants that
    /// belong to the stacking context `i`.
    fn collect(&self, i: usize, layers: &mut Layers, depth: usize) {
        if depth > MAX_PAINT_DEPTH {
            return;
        }
        for c in self.children(i) {
            let Some(f) = self.frag(c) else { continue };
            let s = f.stacking;
            if s.creates_stacking_context {
                // A stacking context without an integer z-index (opacity,
                // fixed or sticky with `auto`) paints at layer 0.
                match s.z_index.unwrap_or(0) {
                    z if z < 0 => layers.negative.push((z, c)),
                    0 => layers.zero.push(c),
                    z => layers.positive.push((z, c)),
                }
                continue;
            }
            if s.positioned {
                layers.zero.push(c);
            }
            self.collect(c, layers, depth + 1);
        }
    }

    fn children(&self, i: usize) -> Vec<usize> {
        self.frag(i)
            .map(|f| f.children.iter().map(|c| c.index()).collect())
            .unwrap_or_default()
    }

    /// Whether `f` is painted in a layer of its own rather than as part of
    /// its parent's content.
    fn own_layer(f: &Fragment) -> bool {
        let s = f.stacking;
        s.creates_stacking_context || s.positioned || s.float
    }

    /// Whether replaced fragment `i` is inline-level (in a line).
    fn in_line(&self, i: usize) -> bool {
        let parent = self.frag(i).and_then(|f| f.parent);
        parent.and_then(|p| self.tree.get(p)).is_some_and(|p| {
            matches!(
                p.kind,
                FragmentKind::Line(_)
                    | FragmentKind::Marker
                    | FragmentKind::Box(wb_layout::BoxFragmentKind::Inline)
            )
        })
    }

    /// Step 3: backgrounds and borders of in-flow block-level descendants.
    fn paint_blocks(&mut self, i: usize, depth: usize) {
        if depth > MAX_PAINT_DEPTH {
            return;
        }
        for c in self.children(i) {
            let Some(f) = self.frag(c) else { continue };
            if Self::own_layer(f) {
                continue;
            }
            match f.kind {
                FragmentKind::Box(wb_layout::BoxFragmentKind::Block) => {
                    self.paint_decorations(c);
                    self.paint_blocks(c, depth + 1);
                }
                FragmentKind::Replaced(_) if !self.in_line(c) => self.paint_decorations(c),
                _ => {}
            }
        }
    }

    /// Step 4: floats, each painted atomically.
    fn paint_floats(&mut self, i: usize, depth: usize) {
        if depth > MAX_PAINT_DEPTH {
            return;
        }
        for c in self.children(i) {
            let Some(f) = self.frag(c) else { continue };
            let s = f.stacking;
            if s.creates_stacking_context || s.positioned {
                continue;
            }
            if s.float {
                self.paint_context(c, false, depth + 1);
                continue;
            }
            match f.kind {
                // Inline-blocks paint their floats themselves.
                FragmentKind::Box(wb_layout::BoxFragmentKind::AtomicInline)
                | FragmentKind::Replaced(_)
                | FragmentKind::Text(_) => {}
                _ => self.paint_floats(c, depth + 1),
            }
        }
    }

    /// Step 5: inline content.
    fn paint_inline(&mut self, i: usize, depth: usize) {
        if depth > MAX_PAINT_DEPTH {
            return;
        }
        for c in self.children(i) {
            let Some(f) = self.frag(c) else { continue };
            if Self::own_layer(f) {
                continue;
            }
            match &f.kind {
                FragmentKind::Box(wb_layout::BoxFragmentKind::Block) | FragmentKind::Line(_) => {
                    self.paint_inline(c, depth + 1);
                }
                FragmentKind::Box(wb_layout::BoxFragmentKind::Inline) | FragmentKind::Marker => {
                    self.paint_decorations(c);
                    self.paint_inline(c, depth + 1);
                }
                FragmentKind::Box(wb_layout::BoxFragmentKind::AtomicInline) => {
                    self.paint_context(c, false, depth + 1);
                }
                FragmentKind::Text(_) => self.paint_text(c),
                FragmentKind::Replaced(_) => {
                    if self.in_line(c) {
                        self.paint_decorations(c);
                    }
                    self.paint_replaced(c);
                }
            }
        }
    }

    // ----- Canvas -------------------------------------------------------

    /// The canvas background (CSS Backgrounds 3 §2.11.2).
    fn paint_canvas(&mut self, root: usize) {
        let Some(rf) = self.frag(root) else { return };
        let mut source = root;
        let transparent = |f: &Fragment| {
            f.style.background_color.alpha == 0
                && f.style
                    .background_layers
                    .iter()
                    .all(|l| matches!(l.image, wb_style::Image::None))
        };
        if transparent(rf)
            && let Some(root_node) = rf.node
            && self.doc.is_html_element(root_node, "html")
            && let Some(body) = self
                .doc
                .element_children(root_node)
                .find(|c| self.doc.is_html_element(*c, "body"))
            && let Some(bf) = rf
                .children
                .iter()
                .map(|c| c.index())
                .find(|c| self.frag(*c).is_some_and(|f| f.node == Some(body)))
        {
            source = bf;
        }
        self.no_background.push(root);
        self.no_background.push(source);
        let canvas = self.tree.document_overflow().union(&self.tree.viewport());
        let canvas = Rect::new(canvas.x, canvas.y, canvas.width, canvas.height);
        self.enter(root);
        self.paint_canvas_background(root, source, canvas);
    }
}
