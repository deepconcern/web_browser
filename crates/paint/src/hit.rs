//! Hit testing: which tagged items are under a point.

use crate::geom::{Point, Rect, RoundedRect};
use crate::list::{DisplayItem, DisplayList, HitTag};

/// One item under the point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The item's tag.
    pub tag: HitTag,
    /// The item's index in [`DisplayList::items`].
    pub item: usize,
    /// For text items, the caret offset in the source text nearest the
    /// point (see [`crate::TextItem::offset_at`]).
    pub text_offset: Option<usize>,
}

/// Every tagged item whose painted area contains `point` (CSS px, page
/// coordinates), topmost first: the reverse of paint order, which is the
/// stacking order the list was built in.
///
/// An item counts only where all of its enclosing clips (rounded ones
/// included) contain the point. Opacity does not matter: a fully
/// transparent element is still hit, as in CSS. Fills, images and hit
/// regions hit inside their (rounded) shape, background images inside
/// their clip, borders only on the ring between the border and padding
/// edges, and text inside the run's box.
///
/// It takes one pass over the list, so it costs O(items).
#[must_use]
pub fn hit_test(list: &DisplayList, point: Point) -> Vec<Hit> {
    if !point.is_finite() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    // For each open group: whether the point is inside every clip so far,
    // and the scroll offset.
    let mut inside = true;
    let mut scroll = (0i64, 0i64);
    let mut stack: Vec<(bool, (i64, i64))> = Vec::new();
    for (index, item) in list.items().iter().enumerate() {
        // The point in the item's own (unscrolled) coordinates.
        let p = Point::new(point.x - scroll.0 as f32, point.y - scroll.1 as f32);
        match item {
            DisplayItem::PushClip(shape) => {
                stack.push((inside, scroll));
                inside = inside && shape.contains(p);
                continue;
            }
            DisplayItem::PushOpacity(_) => {
                stack.push((inside, scroll));
                continue;
            }
            DisplayItem::PushScroll { dx, dy } => {
                stack.push((inside, scroll));
                scroll = (
                    scroll.0.saturating_add(i64::from(*dx)),
                    scroll.1.saturating_add(i64::from(*dy)),
                );
                continue;
            }
            DisplayItem::PopClip | DisplayItem::PopOpacity | DisplayItem::PopScroll => {
                if let Some((i, s)) = stack.pop() {
                    inside = i;
                    scroll = s;
                }
                continue;
            }
            _ => {}
        }
        if !inside {
            continue;
        }
        let Some(tag) = item.tag() else { continue };
        let mut text_offset = None;
        let hit = match item {
            DisplayItem::Fill(f) => f.shape.contains(p),
            DisplayItem::HitRegion { shape, .. } => shape.contains(p),
            DisplayItem::Image(i) => i.dest.contains(p),
            DisplayItem::BackgroundImage(b) => b.clip.contains(p),
            DisplayItem::Border(b) => {
                let outer = RoundedRect::new(b.rect, b.radii);
                outer.contains(p) && !inner_edge(b).contains(p)
            }
            DisplayItem::Text(t) => {
                let hit = t.bounds.contains(p);
                if hit {
                    text_offset = Some(t.offset_at(p.x));
                }
                hit
            }
            _ => false,
        };
        if hit {
            hits.push(Hit {
                tag,
                item: index,
                text_offset,
            });
        }
    }
    hits.reverse();
    hits
}

/// The topmost tagged item under `point`, if any.
#[must_use]
pub fn hit_test_topmost(list: &DisplayList, point: Point) -> Option<Hit> {
    hit_test(list, point).into_iter().next()
}

/// The padding edge of a border, in CSS px (CSS Backgrounds 3 §5.2).
fn inner_edge(b: &crate::list::BorderItem) -> RoundedRect {
    let w = |s: &crate::list::BorderSide| {
        if s.is_visible() {
            s.width.max(0.0)
        } else {
            0.0
        }
    };
    let (t, r, bo, l) = (w(&b.top), w(&b.right), w(&b.bottom), w(&b.left));
    let outer = RoundedRect::new(b.rect, b.radii);
    let rect = Rect::new(
        b.rect.x + l,
        b.rect.y + t,
        b.rect.width - l - r,
        b.rect.height - t - bo,
    );
    let shrink = |c: crate::geom::Radius, dx: f32, dy: f32| {
        crate::geom::Radius::new((c.x - dx).max(0.0), (c.y - dy).max(0.0))
    };
    let o = outer.radii;
    RoundedRect::new(
        rect,
        crate::geom::CornerRadii {
            top_left: shrink(o.top_left, l, t),
            top_right: shrink(o.top_right, r, t),
            bottom_right: shrink(o.bottom_right, r, bo),
            bottom_left: shrink(o.bottom_left, l, bo),
        },
    )
}
