//! Shared layout state: the box tree, the text engine, the host, the
//! fragment arena under construction, and per-layout caches.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use wb_style::ComputedStyle;
use wb_text::FontContext;

use crate::box_tree::{BoxId, BoxTree, LayoutBox};
use crate::formatting::inline::Prepared;
use crate::formatting::{ContainingBlock, IntrinsicSizes};
use crate::fragment::{Fragment, FragmentId, FragmentKind, FragmentTree, MAX_FRAGMENTS, Stacking};
use crate::geom::{Edges, Point, Rect, Size, sane_non_negative};
use crate::text::{Metrics, TextEngine};
use crate::{LayoutHost, LayoutOptions};

/// Everything one layout pass reads and writes.
pub(crate) struct LayoutCtx<'a> {
    pub(crate) tree: &'a BoxTree,
    pub(crate) text: &'a dyn TextEngine,
    pub(crate) host: &'a dyn LayoutHost,
    /// The initial containing block size (for fixed positioning and
    /// viewport-relative layout).
    pub(crate) viewport: Size,
    frags: Vec<Fragment>,
    pub(crate) truncated: bool,
    /// Collected, white-space-processed and shaped inline content per
    /// block container (layout and intrinsic sizing share it).
    pub(crate) inline_cache: HashMap<BoxId, Rc<Prepared>>,
    /// Min-/max-content contributions per box.
    pub(crate) intrinsic_cache: HashMap<BoxId, IntrinsicSizes>,
    /// Font metrics per style object.
    metrics_cache: HashMap<usize, Metrics>,
    /// Floats of the block formatting contexts being laid out.
    pub(crate) floats: crate::formatting::float::FloatState,
    /// Containing blocks collecting absolutely positioned boxes.
    pub(crate) abs: crate::formatting::positioned::AbsState,
    /// Flex layout state: sizes imposed on flex items and the item layout
    /// cache (CSS Flexbox 1 §9).
    pub(crate) flex: crate::formatting::flex::FlexState,
}

impl<'a> LayoutCtx<'a> {
    pub(crate) fn new(
        tree: &'a BoxTree,
        text: &'a dyn TextEngine,
        host: &'a dyn LayoutHost,
        viewport: Size,
    ) -> Self {
        LayoutCtx {
            tree,
            text,
            host,
            viewport,
            frags: Vec::new(),
            truncated: tree.truncated(),
            inline_cache: HashMap::new(),
            intrinsic_cache: HashMap::new(),
            metrics_cache: HashMap::new(),
            floats: Default::default(),
            abs: Default::default(),
            flex: Default::default(),
        }
    }

    /// The box `id`.
    pub(crate) fn get(&self, id: BoxId) -> Option<&'a LayoutBox> {
        self.tree.get(id)
    }

    /// Whether the fragment budget is used up; layout stops adding content.
    pub(crate) fn exhausted(&self) -> bool {
        self.frags.len() >= MAX_FRAGMENTS
    }

    /// Adds a fragment. Its rectangles are relative to its parent
    /// fragment's border-box origin until [`crate::finalize`] runs.
    pub(crate) fn push(&mut self, f: Fragment) -> Option<FragmentId> {
        if self.exhausted() {
            self.truncated = true;
            return None;
        }
        let id = FragmentId::from_index(self.frags.len())?;
        self.frags.push(f);
        Some(id)
    }

    pub(crate) fn frag(&self, id: FragmentId) -> Option<&Fragment> {
        self.frags.get(id.index())
    }

    pub(crate) fn frag_mut(&mut self, id: FragmentId) -> Option<&mut Fragment> {
        self.frags.get_mut(id.index())
    }

    /// Moves fragment `id` so its border box's top-left corner is at `p`
    /// (relative to its parent).
    pub(crate) fn set_position(&mut self, id: FragmentId, p: Point) {
        if let Some(f) = self.frag_mut(id) {
            let d = Point::new(p.x - f.border_box.x, p.y - f.border_box.y);
            translate_fragment(f, d);
        }
    }

    /// Moves fragment `id` by `d`.
    pub(crate) fn translate(&mut self, id: FragmentId, d: Point) {
        if let Some(f) = self.frag_mut(id) {
            translate_fragment(f, d);
        }
    }

    /// The primary font's metrics for `style` (cached per style object).
    pub(crate) fn metrics(&mut self, style: &Arc<ComputedStyle>) -> Metrics {
        let key = Arc::as_ptr(style) as usize;
        if let Some(m) = self.metrics_cache.get(&key) {
            return *m;
        }
        let m = self.text.metrics(style);
        self.metrics_cache.insert(key, m);
        m
    }

    /// Creates a box fragment at the origin with the given border-box
    /// size, border and padding widths, and children.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn box_fragment(
        &mut self,
        b: &LayoutBox,
        box_id: Option<BoxId>,
        kind: FragmentKind,
        size: Size,
        border: Edges,
        padding: Edges,
        children: Vec<FragmentId>,
        is_root: bool,
    ) -> Option<FragmentId> {
        let border_box = Rect::new(
            0.0,
            0.0,
            sane_non_negative(size.width),
            sane_non_negative(size.height),
        );
        let padding_box = border_box.deflate(&border);
        let content_box = padding_box.deflate(&padding);
        let stacking = if b.anonymous {
            Stacking::NONE
        } else {
            Stacking::from_style(&b.style, is_root)
        };
        self.push(Fragment {
            kind,
            node: if b.anonymous { None } else { b.node },
            pseudo: b.pseudo,
            box_id,
            style: b.style.clone(),
            border_box,
            padding_box,
            content_box,
            relative_offset: Point::ZERO,
            stacking,
            clip: None,
            scroll_offset: Point::ZERO,
            scrollable_overflow: border_box,
            parent: None,
            children,
        })
    }

    /// Takes the finished arena.
    pub(crate) fn into_fragments(self) -> (Vec<Fragment>, bool) {
        (self.frags, self.truncated)
    }
}

/// Moves a fragment's own rectangles (and text/line positions) by `d`.
pub(crate) fn translate_fragment(f: &mut Fragment, d: Point) {
    if d == Point::ZERO {
        return;
    }
    f.border_box = f.border_box.translate(d);
    f.padding_box = f.padding_box.translate(d);
    f.content_box = f.content_box.translate(d);
    f.scrollable_overflow = f.scrollable_overflow.translate(d);
    if let Some(c) = &mut f.clip {
        c.rect = c.rect.translate(d);
    }
    match &mut f.kind {
        FragmentKind::Text(t) => {
            t.baseline += d.y;
            for r in &mut t.runs {
                for g in &mut r.glyphs {
                    g.origin = g.origin.offset(d);
                }
            }
        }
        FragmentKind::Line(l) => l.baseline += d.y,
        _ => {}
    }
}

/// Lays out `tree` with the system font context.
pub(crate) fn layout_tree(
    tree: &BoxTree,
    fonts: &FontContext,
    host: &dyn LayoutHost,
    options: &LayoutOptions,
) -> FragmentTree {
    layout_tree_with(tree, fonts, host, options)
}

/// Lays out `tree` with any text engine.
pub(crate) fn layout_tree_with(
    tree: &BoxTree,
    text: &dyn TextEngine,
    host: &dyn LayoutHost,
    options: &LayoutOptions,
) -> FragmentTree {
    let viewport = Size::new(
        sane_non_negative(options.viewport.width),
        sane_non_negative(options.viewport.height),
    );
    let mut ctx = LayoutCtx::new(tree, text, host, viewport);
    let root = tree.root().and_then(|r| {
        let cb = ContainingBlock {
            width: viewport.width,
            height: Some(viewport.height),
        };
        let res = crate::formatting::layout_root(&mut ctx, r, &cb);
        res.fragment
    });
    let (frags, truncated) = ctx.into_fragments();
    crate::finalize::finalize(frags, root, viewport, truncated, host)
}
