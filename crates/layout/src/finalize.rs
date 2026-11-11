//! Turns the fragment arena (positions relative to each parent) into the
//! public [`FragmentTree`] with absolute positions: applies relative
//! positioning offsets, computes clips and scrollable overflow, and
//! applies scroll offsets.

use crate::LayoutHost;
use crate::context::translate_fragment;
use crate::fragment::{Clip, Fragment, FragmentId, FragmentTree};
use crate::geom::{Edges, Point, Rect, Size};

pub(crate) fn finalize(
    frags: Vec<Fragment>,
    root: Option<FragmentId>,
    viewport: Size,
    truncated: bool,
    host: &dyn LayoutHost,
) -> FragmentTree {
    let viewport_rect = Rect::new(0.0, 0.0, viewport.width, viewport.height);
    let Some(root) = root else {
        return FragmentTree {
            fragments: Vec::new(),
            root: None,
            viewport: viewport_rect,
            document_overflow: viewport_rect,
            truncated,
        };
    };
    // Compact into pre-order, so a parent precedes its descendants and
    // unreachable fragments are dropped.
    let mut slots: Vec<Option<Fragment>> = frags.into_iter().map(Some).collect();
    let mut out: Vec<Fragment> = Vec::with_capacity(slots.len());
    // (old id, new parent)
    let mut stack: Vec<(FragmentId, Option<FragmentId>)> = vec![(root, None)];
    while let Some((old, parent)) = stack.pop() {
        let Some(mut f) = slots.get_mut(old.index()).and_then(Option::take) else {
            continue;
        };
        let Some(new_id) = FragmentId::from_index(out.len()) else {
            break;
        };
        f.parent = parent;
        let kids = std::mem::take(&mut f.children);
        for k in kids.iter().rev() {
            stack.push((*k, Some(new_id)));
        }
        out.push(f);
        if let Some(p) = parent.and_then(|p| out.get_mut(p.index())) {
            p.children.push(new_id);
        }
    }

    // Pass 1 (pre-order): absolute positions, including relative offsets.
    for i in 0..out.len() {
        let parent_origin = out
            .get(i)
            .and_then(|f| f.parent)
            .and_then(|p| out.get(p.index()))
            .map_or(Point::ZERO, |p| p.border_box.origin());
        if let Some(f) = out.get_mut(i) {
            let d = parent_origin.offset(f.relative_offset);
            translate_fragment(f, d);
            f.scrollable_overflow = f.border_box;
            f.clip = clip_for(f);
        }
    }

    // Pass 2 (post-order): scrollable overflow (CSS Overflow 3 §2.2): a
    // box's own padding box (border box for the purpose of its parent)
    // plus its descendants' overflow, unless it clips them.
    for i in (0..out.len()).rev() {
        let Some(f) = out.get(i) else { continue };
        let mut kids: Option<Rect> = None;
        for c in &f.children {
            if let Some(cf) = out.get(c.index()) {
                let r = contribution(cf);
                kids = Some(kids.map_or(r, |k| k.union(&r)));
            }
        }
        let own = match (f.clip.is_some(), kids) {
            // A scroll container's content area includes its end padding
            // (CSS Overflow 3 §2.2).
            (true, Some(k)) => {
                let end_pad = Edges::new(
                    0.0,
                    f.padding_box.max_x() - f.content_box.max_x(),
                    f.padding_box.max_y() - f.content_box.max_y(),
                    0.0,
                );
                f.padding_box.union(&k.inflate(&end_pad))
            }
            (true, None) => f.padding_box,
            (false, Some(k)) => f.border_box.union(&k),
            (false, None) => f.border_box,
        };
        if let Some(f) = out.get_mut(i) {
            f.scrollable_overflow = own;
        }
    }

    // Pass 3 (pre-order): scroll offsets of scroll containers move their
    // descendants.
    let mut shifts: Vec<Point> = vec![Point::ZERO; out.len()];
    for i in 0..out.len() {
        let parent = out.get(i).and_then(|f| f.parent);
        let inherited = parent.map_or(Point::ZERO, |p| {
            let base = shifts.get(p.index()).copied().unwrap_or(Point::ZERO);
            let own = out
                .get(p.index())
                .map_or(Point::ZERO, |pf| pf.scroll_offset);
            Point::new(base.x - own.x, base.y - own.y)
        });
        if let Some(s) = shifts.get_mut(i) {
            *s = inherited;
        }
        let Some(f) = out.get_mut(i) else { continue };
        translate_fragment(f, inherited);
        if let Some(clip) = f.clip
            && clip.scroll_container
            && let Some(node) = f.node
        {
            let want = host.scroll_offset(node);
            let max_x = (f.scrollable_overflow.max_x() - clip.rect.max_x()).max(0.0);
            let max_y = (f.scrollable_overflow.max_y() - clip.rect.max_y()).max(0.0);
            let clamp = |v: f32, max: f32| {
                if v.is_finite() {
                    v.clamp(0.0, max)
                } else {
                    0.0
                }
            };
            f.scroll_offset = Point::new(
                if clip.x { clamp(want.x, max_x) } else { 0.0 },
                if clip.y { clamp(want.y, max_y) } else { 0.0 },
            );
        }
    }

    let document_overflow = out
        .first()
        .map_or(viewport_rect, |r| viewport_rect.union(&contribution(r)));
    FragmentTree {
        fragments: out,
        root: Some(FragmentId::from_index(0).unwrap_or(root)),
        viewport: viewport_rect,
        document_overflow,
        truncated,
    }
}

/// What a fragment adds to its parent's scrollable overflow.
fn contribution(f: &Fragment) -> Rect {
    if let Some(c) = f.clip {
        // Clipped content does not escape, on the clipped axes.
        let o = f.scrollable_overflow;
        let b = f.border_box;
        let (x, w) = if c.x { (b.x, b.width) } else { (o.x, o.width) };
        let (y, h) = if c.y {
            (b.y, b.height)
        } else {
            (o.y, o.height)
        };
        Rect::new(x, y, w, h).union(&b)
    } else {
        f.scrollable_overflow
    }
}

/// The clip a box applies to its descendants (CSS Overflow 3 §3).
fn clip_for(f: &Fragment) -> Option<Clip> {
    use crate::fragment::{BoxFragmentKind, FragmentKind};
    use wb_style::Overflow;
    // `overflow` applies to block containers and atomic inlines, not to
    // inline boxes.
    if !matches!(
        f.kind,
        FragmentKind::Box(BoxFragmentKind::Block | BoxFragmentKind::AtomicInline)
    ) {
        return None;
    }
    let s = &f.style;
    let x = !matches!(s.overflow_x, Overflow::Visible);
    let y = !matches!(s.overflow_y, Overflow::Visible);
    if !x && !y {
        return None;
    }
    let scrolls = |o: Overflow| matches!(o, Overflow::Hidden | Overflow::Scroll | Overflow::Auto);
    Some(Clip {
        rect: f.padding_box,
        x,
        y,
        scroll_container: scrolls(s.overflow_x) || scrolls(s.overflow_y),
    })
}
