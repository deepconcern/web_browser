//! The display list: what to paint, in paint order.

use std::ops::Range;
use std::sync::Arc;

use wb_image::Image;
use wb_text::{Direction, FontId, GlyphId, Synthesis};

use crate::geom::{Color, CornerRadii, Point, Rect, RoundedRect};

/// An opaque value that the list's builder attaches to items so that
/// [`crate::hit_test`] can say what is under a point. Layout uses it to
/// name DOM nodes; paint never looks inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HitTag(pub u64);

/// A filled rectangle, optionally with rounded corners. Used for
/// `background-color`, text decorations, selection highlights and similar.
#[derive(Clone, Debug, PartialEq)]
pub struct FillItem {
    /// The area to fill.
    pub shape: RoundedRect,
    /// The fill color.
    pub color: Color,
    /// Hit-test tag; `None` makes the item invisible to hit testing.
    pub tag: Option<HitTag>,
}

/// `border-*-style` values (CSS Backgrounds 3 §4.2).
///
/// `groove`, `ridge`, `inset` and `outset` are painted as `solid` in M1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BorderStyle {
    /// No border; the side is not painted.
    #[default]
    None,
    /// Like `none`, for table border conflict resolution; not painted.
    Hidden,
    /// A series of round dots.
    Dotted,
    /// A series of dashes.
    Dashed,
    /// One solid line.
    Solid,
    /// Two parallel solid lines with a gap between them; the lines and the
    /// gap are each a third of the width.
    Double,
    /// Painted as `solid` in M1.
    Groove,
    /// Painted as `solid` in M1.
    Ridge,
    /// Painted as `solid` in M1.
    Inset,
    /// Painted as `solid` in M1.
    Outset,
}

/// One side of a border.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BorderSide {
    /// The used `border-*-width` in CSS px. The rasterizer snaps it to whole
    /// device pixels the way CSS Values 4 "snap as a border width" says:
    /// widths between 0 and 1 device pixel become 1, others are floored.
    pub width: f32,
    /// `border-*-style`.
    pub style: BorderStyle,
    /// `border-*-color`.
    pub color: Color,
}

impl BorderSide {
    /// Whether the side paints anything.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.width > 0.0
            && self.color.a > 0
            && !matches!(self.style, BorderStyle::None | BorderStyle::Hidden)
    }
}

/// A box's border, painted inside its border box.
#[derive(Clone, Debug, PartialEq)]
pub struct BorderItem {
    /// The border box.
    pub rect: Rect,
    /// `border-*-radius` (outer edge). The inner (padding edge) radii are
    /// derived from these and the widths (CSS Backgrounds 3 §5.2).
    pub radii: CornerRadii,
    /// `border-top-*`.
    pub top: BorderSide,
    /// `border-right-*`.
    pub right: BorderSide,
    /// `border-bottom-*`.
    pub bottom: BorderSide,
    /// `border-left-*`.
    pub left: BorderSide,
    /// Hit-test tag. A border hits only on the painted ring.
    pub tag: Option<HitTag>,
}

/// One glyph of a [`TextItem`], positioned by the caller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextGlyph {
    /// The glyph in the run's font.
    pub id: GlyphId,
    /// Horizontal position of the glyph origin relative to the run's
    /// origin, in CSS px.
    pub x: f32,
    /// Vertical position of the glyph origin relative to the baseline,
    /// positive down, in CSS px.
    pub y: f32,
    /// The glyph's advance in CSS px (used for hit testing).
    pub advance: f32,
    /// Byte offset of the start of the glyph's cluster, in the same text as
    /// [`TextItem::text_range`].
    pub cluster: usize,
}

/// A run of glyphs from one font, in one color.
#[derive(Clone, Debug, PartialEq)]
pub struct TextItem {
    /// The pen position at the start of the run, on the baseline.
    pub origin: Point,
    /// The font face.
    pub font: FontId,
    /// The font size in CSS px.
    pub size: f32,
    /// Faux bold and oblique, from shaping.
    pub synthesis: Synthesis,
    /// The text color.
    pub color: Color,
    /// The glyphs, in visual (left to right) order.
    pub glyphs: Vec<TextGlyph>,
    /// The run's box (its fragment: left edge, line top, advance width,
    /// line height). Used for hit testing; culling pads it by one em for
    /// glyph overhang.
    pub bounds: Rect,
    /// The byte range of the source text this run shows.
    pub text_range: Range<usize>,
    /// The run's direction, for mapping points to caret offsets.
    pub direction: Direction,
    /// Hit-test tag.
    pub tag: Option<HitTag>,
}

impl TextItem {
    /// Maps a horizontal position (CSS px, in the list's coordinates
    /// before any scroll offset) to the nearest caret offset in the source
    /// text, for text selection. Clicking the left half of a
    /// left-to-right cluster gives its start and the right half its end
    /// (mirrored for right-to-left). Points beyond the run snap to the
    /// nearest end.
    #[must_use]
    pub fn offset_at(&self, x: f32) -> usize {
        let x = x - self.origin.x;
        // Clusters in visual order: (cluster start, left, right).
        let mut clusters: Vec<(usize, f32, f32)> = Vec::new();
        for g in &self.glyphs {
            let (l, r) = (g.x.min(g.x + g.advance), g.x.max(g.x + g.advance));
            match clusters.last_mut() {
                Some((c, cl, cr)) if *c == g.cluster => {
                    *cl = cl.min(l);
                    *cr = cr.max(r);
                }
                _ => clusters.push((g.cluster, l, r)),
            }
        }
        let mut best: Option<(f32, usize)> = None;
        for (i, (_, l, r)) in clusters.iter().enumerate() {
            let d = if x < *l {
                l - x
            } else if x > *r {
                x - r
            } else {
                0.0
            };
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
        let Some((_, i)) = best else {
            return self.text_range.start;
        };
        let Some(&(cluster, l, r)) = clusters.get(i) else {
            return self.text_range.start;
        };
        let mut starts: Vec<usize> = clusters.iter().map(|c| c.0).collect();
        starts.sort_unstable();
        starts.dedup();
        // The cluster ends where the next cluster in logical order starts.
        let end = match starts.binary_search(&cluster) {
            Ok(k) => starts.get(k + 1).copied(),
            Err(_) => None,
        }
        .unwrap_or(self.text_range.end)
        .max(cluster);
        let left_half = x < (l + r) / 2.0;
        if left_half == (self.direction == Direction::Ltr) {
            cluster
        } else {
            end
        }
    }
}

/// How an image is scaled (`image-rendering`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ImageRendering {
    /// Smooth (bilinear) scaling; exact copy at 1:1.
    #[default]
    Auto,
    /// Nearest-neighbor scaling (`pixelated`, `crisp-edges`).
    Pixelated,
}

/// A replaced image (`<img>`), scaled to fill `dest`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageItem {
    /// Where the image goes (the content box, after `object-fit`).
    pub dest: Rect,
    /// The decoded image.
    pub image: Arc<Image>,
    /// Scaling filter.
    pub rendering: ImageRendering,
    /// Hit-test tag.
    pub tag: Option<HitTag>,
}

/// A `background-image` layer: one positioned tile, repeated along the
/// chosen axes, painted only inside `clip`.
///
/// The builder resolves `background-position`, `background-size` and
/// `background-origin` into `tile`, `background-clip` and the border
/// radii into `clip`, and `background-repeat: round` into the tile size.
/// `space` is not supported here; the builder can emit one no-repeat item
/// per tile instead (M1 follow-up).
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundImageItem {
    /// The background painting area: the clip box with its corner radii.
    pub clip: RoundedRect,
    /// One tile: where the image is placed and how big it is drawn.
    pub tile: Rect,
    /// Repeat horizontally (tiles to the left and right of `tile`).
    pub repeat_x: bool,
    /// Repeat vertically.
    pub repeat_y: bool,
    /// The decoded image.
    pub image: Arc<Image>,
    /// Scaling filter.
    pub rendering: ImageRendering,
    /// Hit-test tag.
    pub tag: Option<HitTag>,
}

/// One entry of a display list.
///
/// Drawing items paint over the items before them. The `Push*`/`Pop*`
/// pairs bracket groups and must nest properly; [`DisplayList::push`]
/// enforces this.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayItem {
    /// A filled (rounded) rectangle.
    Fill(FillItem),
    /// A border.
    Border(BorderItem),
    /// A run of glyphs.
    Text(TextItem),
    /// A replaced image.
    Image(ImageItem),
    /// A background image layer.
    BackgroundImage(BackgroundImageItem),
    /// An invisible area that only takes part in hit testing (for example
    /// an element with no background, or a form control's whole box).
    HitRegion {
        /// The hit area.
        shape: RoundedRect,
        /// The tag reported for it.
        tag: HitTag,
    },
    /// Starts clipping everything up to the matching [`DisplayItem::PopClip`]
    /// to this shape (intersected with any enclosing clip).
    PushClip(RoundedRect),
    /// Ends the innermost clip.
    PopClip,
    /// Starts a group painted to a separate layer and composited with this
    /// opacity (`0.0..=1.0`) at the matching [`DisplayItem::PopOpacity`].
    PushOpacity(f32),
    /// Ends the innermost opacity group.
    PopOpacity,
    /// Moves everything up to the matching [`DisplayItem::PopScroll`] by
    /// (`dx`, `dy`) whole CSS px: the negated scroll position of a
    /// scroll container. This is the only transform in M1.
    PushScroll {
        /// Horizontal offset.
        dx: i32,
        /// Vertical offset.
        dy: i32,
    },
    /// Ends the innermost scroll offset.
    PopScroll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupKind {
    Clip,
    Opacity,
    Scroll,
}

impl DisplayItem {
    fn opens(&self) -> Option<GroupKind> {
        match self {
            DisplayItem::PushClip(_) => Some(GroupKind::Clip),
            DisplayItem::PushOpacity(_) => Some(GroupKind::Opacity),
            DisplayItem::PushScroll { .. } => Some(GroupKind::Scroll),
            _ => None,
        }
    }

    fn closes(&self) -> Option<GroupKind> {
        match self {
            DisplayItem::PopClip => Some(GroupKind::Clip),
            DisplayItem::PopOpacity => Some(GroupKind::Opacity),
            DisplayItem::PopScroll => Some(GroupKind::Scroll),
            _ => None,
        }
    }

    /// The item's hit-test tag, if it has one.
    #[must_use]
    pub fn tag(&self) -> Option<HitTag> {
        match self {
            DisplayItem::Fill(i) => i.tag,
            DisplayItem::Border(i) => i.tag,
            DisplayItem::Text(i) => i.tag,
            DisplayItem::Image(i) => i.tag,
            DisplayItem::BackgroundImage(i) => i.tag,
            DisplayItem::HitRegion { tag, .. } => Some(*tag),
            _ => None,
        }
    }
}

/// Caps that keep a page from building a list that exhausts memory or
/// time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListLimits {
    /// Most items kept (pops of accepted groups are always kept on top of
    /// this, so a list never has unbalanced groups).
    pub max_items: usize,
    /// Deepest nesting of clip, opacity and scroll groups.
    pub max_depth: usize,
    /// Most glyphs over all text items.
    pub max_glyphs: usize,
}

impl Default for ListLimits {
    fn default() -> ListLimits {
        ListLimits {
            max_items: 500_000,
            max_depth: 256,
            max_glyphs: 2_000_000,
        }
    }
}

/// A display list: drawing items in paint order, with properly nested
/// clip, opacity and scroll groups.
///
/// # Contract for builders
///
/// - **Order is the painting order.** Paint does not sort: the builder
///   (layout's display-list step) emits items in CSS 2.1 Appendix E
///   stacking order, so later items are on top. Stacking contexts become
///   nothing more than the order of items, plus an opacity group when
///   `opacity < 1`.
/// - **Coordinates** are CSS px in the page's coordinate space; the
///   rasterizer applies the viewport and device scale factor. Scroll
///   containers wrap their contents in [`DisplayItem::PushScroll`] (and
///   normally a clip).
/// - **Limits.** Items past [`ListLimits`] are dropped; a group that
///   would nest too deep is dropped together with everything inside it,
///   so that content never escapes a clip. Items with non-finite numbers
///   are dropped. [`DisplayList::dropped`] counts drops.
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    items: Vec<DisplayItem>,
    open: Vec<GroupKind>,
    /// Depth of a dropped group whose contents are being skipped.
    suppressed: usize,
    glyphs: usize,
    dropped: usize,
    limits: ListLimits,
}

impl DisplayList {
    /// An empty list with the default limits.
    #[must_use]
    pub fn new() -> DisplayList {
        DisplayList::default()
    }

    /// An empty list with the given limits.
    #[must_use]
    pub fn with_limits(limits: ListLimits) -> DisplayList {
        DisplayList {
            limits,
            ..DisplayList::default()
        }
    }

    /// Appends an item. Returns whether it was kept (see the limits on
    /// [`DisplayList`]). A pop that does not match the innermost open
    /// group is dropped.
    pub fn push(&mut self, item: DisplayItem) -> bool {
        if self.suppressed > 0 {
            if item.opens().is_some() {
                self.suppressed += 1;
            } else if item.closes().is_some() {
                self.suppressed -= 1;
            }
            self.dropped += 1;
            return false;
        }
        if let Some(kind) = item.closes() {
            if self.open.last() == Some(&kind) {
                self.open.pop();
                self.items.push(item);
                return true;
            }
            self.dropped += 1;
            return false;
        }
        let opens = item.opens().is_some();
        let Some(item) = sanitize(item) else {
            // An invalid group push drops its whole group.
            if opens {
                self.suppressed = 1;
            }
            self.dropped += 1;
            return false;
        };
        if let Some(kind) = item.opens() {
            if self.open.len() >= self.limits.max_depth || self.items.len() >= self.limits.max_items
            {
                self.suppressed = 1;
                self.dropped += 1;
                return false;
            }
            self.open.push(kind);
            self.items.push(item);
            return true;
        }
        if self.items.len() >= self.limits.max_items {
            self.dropped += 1;
            return false;
        }
        if let DisplayItem::Text(t) = &item {
            let total = self.glyphs.saturating_add(t.glyphs.len());
            if total > self.limits.max_glyphs {
                self.dropped += 1;
                return false;
            }
            self.glyphs = total;
        }
        self.items.push(item);
        true
    }

    /// Pushes a [`DisplayItem::Fill`] of a plain rectangle.
    pub fn push_rect(&mut self, rect: Rect, color: Color, tag: Option<HitTag>) -> bool {
        self.push(DisplayItem::Fill(FillItem {
            shape: RoundedRect::from_rect(rect),
            color,
            tag,
        }))
    }

    /// Closes every open group. The rasterizer does this implicitly too.
    pub fn close_all(&mut self) {
        while let Some(kind) = self.open.pop() {
            self.items.push(match kind {
                GroupKind::Clip => DisplayItem::PopClip,
                GroupKind::Opacity => DisplayItem::PopOpacity,
                GroupKind::Scroll => DisplayItem::PopScroll,
            });
        }
        self.suppressed = 0;
    }

    /// The items, in paint order.
    #[must_use]
    pub fn items(&self) -> &[DisplayItem] {
        &self.items
    }

    /// The number of items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the list has no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// How many pushed items were dropped (limits, invalid numbers,
    /// unmatched pops, contents of dropped groups).
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The limits this list enforces.
    #[must_use]
    pub fn limits(&self) -> ListLimits {
        self.limits
    }
}

/// Checks an item's numbers; returns it with normalized radii and clamped
/// opacity, or `None` if it should be dropped.
fn sanitize(item: DisplayItem) -> Option<DisplayItem> {
    let rr = |s: RoundedRect| -> Option<RoundedRect> {
        s.is_finite().then(|| RoundedRect::new(s.rect, s.radii))
    };
    let side_ok = |s: &BorderSide| s.width.is_finite();
    Some(match item {
        DisplayItem::Fill(mut f) => {
            f.shape = rr(f.shape)?;
            if f.shape.rect.is_empty() {
                return None;
            }
            DisplayItem::Fill(f)
        }
        DisplayItem::Border(mut b) => {
            if !(b.rect.is_finite()
                && b.radii.is_finite()
                && side_ok(&b.top)
                && side_ok(&b.right)
                && side_ok(&b.bottom)
                && side_ok(&b.left))
                || b.rect.is_empty()
            {
                return None;
            }
            b.radii = b.radii.normalized(b.rect.width, b.rect.height);
            DisplayItem::Border(b)
        }
        DisplayItem::Text(t) => {
            let ok = t.origin.is_finite()
                && t.size.is_finite()
                && t.size > 0.0
                && t.bounds.is_finite()
                && t.glyphs
                    .iter()
                    .all(|g| g.x.is_finite() && g.y.is_finite() && g.advance.is_finite());
            if !ok || t.glyphs.is_empty() {
                return None;
            }
            DisplayItem::Text(t)
        }
        DisplayItem::Image(i) => {
            if i.dest.is_empty() {
                return None;
            }
            DisplayItem::Image(i)
        }
        DisplayItem::BackgroundImage(mut b) => {
            b.clip = rr(b.clip)?;
            if b.clip.rect.is_empty() || b.tile.is_empty() {
                return None;
            }
            DisplayItem::BackgroundImage(b)
        }
        DisplayItem::HitRegion { shape, tag } => DisplayItem::HitRegion {
            shape: rr(shape)?,
            tag,
        },
        DisplayItem::PushClip(s) => DisplayItem::PushClip(rr(s)?),
        DisplayItem::PushOpacity(a) => {
            if a.is_nan() {
                return None;
            }
            DisplayItem::PushOpacity(a.clamp(0.0, 1.0))
        }
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 10.0, 10.0)
    }

    #[test]
    fn unmatched_pop_is_dropped() {
        let mut list = DisplayList::new();
        assert!(!list.push(DisplayItem::PopClip));
        assert!(list.push(DisplayItem::PushOpacity(0.5)));
        assert!(!list.push(DisplayItem::PopClip));
        assert!(list.push(DisplayItem::PopOpacity));
        assert_eq!(list.len(), 2);
        assert_eq!(list.dropped(), 2);
    }

    #[test]
    fn too_deep_group_drops_contents() {
        let mut list = DisplayList::with_limits(ListLimits {
            max_depth: 2,
            ..ListLimits::default()
        });
        assert!(list.push(DisplayItem::PushClip(rect().into())));
        assert!(list.push(DisplayItem::PushClip(rect().into())));
        assert!(!list.push(DisplayItem::PushClip(rect().into())));
        assert!(!list.push_rect(rect(), Color::BLACK, None));
        assert!(!list.push(DisplayItem::PushScroll { dx: 1, dy: 1 }));
        assert!(!list.push(DisplayItem::PopScroll));
        assert!(!list.push(DisplayItem::PopClip));
        // Back at depth 2: items are kept again.
        assert!(list.push_rect(rect(), Color::BLACK, None));
        assert!(list.push(DisplayItem::PopClip));
        assert!(list.push(DisplayItem::PopClip));
        assert_eq!(list.len(), 5);
    }

    #[test]
    fn item_and_glyph_caps() {
        let mut list = DisplayList::with_limits(ListLimits {
            max_items: 2,
            max_depth: 4,
            max_glyphs: 3,
        });
        assert!(list.push(DisplayItem::PushOpacity(0.5)));
        assert!(list.push_rect(rect(), Color::BLACK, None));
        assert!(!list.push_rect(rect(), Color::BLACK, None));
        assert!(list.push(DisplayItem::PopOpacity));
        assert_eq!(list.len(), 3);

        let glyph = TextGlyph {
            id: GlyphId(1),
            x: 0.0,
            y: 0.0,
            advance: 1.0,
            cluster: 0,
        };
        let Some(font) = font_id() else { return };
        let mut list = DisplayList::with_limits(ListLimits {
            max_glyphs: 3,
            ..ListLimits::default()
        });
        let text = |n: usize| {
            DisplayItem::Text(TextItem {
                origin: Point::default(),
                font,
                size: 10.0,
                synthesis: Synthesis::default(),
                color: Color::BLACK,
                glyphs: vec![glyph; n],
                bounds: rect(),
                text_range: 0..1,
                direction: Direction::Ltr,
                tag: None,
            })
        };
        assert!(list.push(text(2)));
        assert!(!list.push(text(2)));
        assert!(list.push(text(1)));
    }

    #[test]
    fn non_finite_items_are_dropped() {
        let mut list = DisplayList::new();
        assert!(!list.push_rect(Rect::new(f32::NAN, 0.0, 1.0, 1.0), Color::BLACK, None));
        assert!(!list.push_rect(Rect::new(0.0, 0.0, f32::INFINITY, 1.0), Color::BLACK, None));
        assert!(!list.push(DisplayItem::PushOpacity(f32::NAN)));
        // The dropped group's contents are dropped too.
        assert!(!list.push_rect(rect(), Color::BLACK, None));
        assert!(!list.push(DisplayItem::PopOpacity));
        assert!(list.push_rect(rect(), Color::BLACK, None));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn close_all_balances() {
        let mut list = DisplayList::new();
        list.push(DisplayItem::PushClip(rect().into()));
        list.push(DisplayItem::PushScroll { dx: 0, dy: -5 });
        list.close_all();
        assert_eq!(
            list.items().get(2..),
            Some(&[DisplayItem::PopScroll, DisplayItem::PopClip][..])
        );
    }

    fn font_id() -> Option<FontId> {
        crate::test_support::font_id()
    }

    #[test]
    fn text_offset_ltr_and_rtl() {
        let g = |x: f32, cluster: usize| TextGlyph {
            id: GlyphId(1),
            x,
            y: 0.0,
            advance: 10.0,
            cluster,
        };
        let Some(font) = font_id() else { return };
        let mut t = TextItem {
            origin: Point::new(100.0, 50.0),
            font,
            size: 10.0,
            synthesis: Synthesis::default(),
            color: Color::BLACK,
            glyphs: vec![g(0.0, 0), g(10.0, 1), g(20.0, 2)],
            bounds: Rect::new(100.0, 40.0, 30.0, 12.0),
            text_range: 0..3,
            direction: Direction::Ltr,
            tag: None,
        };
        assert_eq!(t.offset_at(101.0), 0);
        assert_eq!(t.offset_at(106.0), 1);
        assert_eq!(t.offset_at(114.0), 1);
        assert_eq!(t.offset_at(129.0), 3);
        assert_eq!(t.offset_at(50.0), 0);
        assert_eq!(t.offset_at(500.0), 3);
        // Right-to-left: visual order has decreasing clusters.
        t.glyphs = vec![g(0.0, 2), g(10.0, 1), g(20.0, 0)];
        t.direction = Direction::Rtl;
        assert_eq!(t.offset_at(101.0), 3);
        assert_eq!(t.offset_at(129.0), 0);
        assert_eq!(t.offset_at(124.0), 1);
        // A ligature: two characters, one glyph.
        t.glyphs = vec![g(0.0, 0), g(10.0, 2)];
        t.direction = Direction::Ltr;
        assert_eq!(t.offset_at(108.0), 2);
    }
}
