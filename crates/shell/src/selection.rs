//! Text selection: a range between two caret positions (a text node and a
//! byte offset in it), its rendered text for copying and its highlight
//! rectangles, computed from the fragment tree.

use std::collections::HashMap;

use wb_dom::{Document, NodeId};
use wb_layout::{FragmentId, FragmentKind, FragmentTree};

/// A caret position: a text node and a byte offset in its data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    /// The text node.
    pub node: NodeId,
    /// The byte offset in the node's data.
    pub offset: usize,
}

/// A selection from `anchor` (where the drag started) to `focus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started.
    pub anchor: Caret,
    /// Where it ends (follows the pointer).
    pub focus: Caret,
}

/// A highlight rectangle in document CSS px: x, y, width, height.
pub type HighlightRect = (f32, f32, f32, f32);

/// Each node's position in tree order, for comparing carets.
#[derive(Debug)]
pub struct TreeOrder(HashMap<NodeId, usize>);

impl TreeOrder {
    /// Numbers the nodes of `doc` in tree order.
    #[must_use]
    pub fn new(doc: &Document) -> TreeOrder {
        TreeOrder(
            doc.descendants(doc.root())
                .enumerate()
                .map(|(i, n)| (n, i))
                .collect(),
        )
    }

    fn key(&self, c: Caret) -> (usize, usize) {
        (self.0.get(&c.node).copied().unwrap_or(usize::MAX), c.offset)
    }
}

impl Selection {
    /// Whether the selection is empty (both ends at the same place).
    #[must_use]
    pub fn is_collapsed(&self) -> bool {
        self.anchor == self.focus
    }

    /// The selection's ends in tree order, as sort keys.
    fn bounds(&self, order: &TreeOrder) -> ((usize, usize), (usize, usize)) {
        let (a, f) = (order.key(self.anchor), order.key(self.focus));
        if a <= f { (a, f) } else { (f, a) }
    }

    /// Selects every text node of `doc`, or `None` when it has no text.
    #[must_use]
    pub fn all(doc: &Document) -> Option<Selection> {
        let mut texts = doc
            .descendants(doc.root())
            .filter(|&n| doc.text(n).is_some_and(|t| !t.is_empty()));
        let first = texts.next()?;
        let last = texts.last().unwrap_or(first);
        let len = doc.text(last).map_or(0, str::len);
        Some(Selection {
            anchor: Caret {
                node: first,
                offset: 0,
            },
            focus: Caret {
                node: last,
                offset: len,
            },
        })
    }
}

/// One selected piece of a text fragment.
struct Piece<'t> {
    line: Option<FragmentId>,
    block: Option<FragmentId>,
    text: &'t str,
    rects: Vec<HighlightRect>,
}

/// The selected pieces of every text fragment, in fragment order.
fn pieces<'t>(tree: &'t FragmentTree, order: &TreeOrder, sel: &Selection) -> Vec<Piece<'t>> {
    let (start, end) = sel.bounds(order);
    let mut out = Vec::new();
    if start == end {
        return out;
    }
    for frag in tree.fragments() {
        let FragmentKind::Text(t) = &frag.kind else {
            continue;
        };
        let Some(node) = frag.node else { continue };
        let Some(&k) = order.0.get(&node) else {
            continue;
        };
        if k < start.0 || k > end.0 {
            continue;
        }
        let inside = |rendered: usize| {
            let key = (k, t.source_offset(rendered));
            key >= start && key < end
        };
        // The selected rendered byte range of this fragment.
        let Some(shown) = t.text.get(t.range.clone()) else {
            continue;
        };
        let mut lo = None;
        let mut hi = t.range.start;
        for (i, c) in shown.char_indices() {
            let r = t.range.start + i;
            if inside(r) {
                lo.get_or_insert(r);
                hi = r + c.len_utf8();
            }
        }
        let Some(lo) = lo else { continue };
        let Some(text) = t.text.get(lo..hi) else {
            continue;
        };
        let b = frag.border_box;
        let rects = t
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
            .filter(|g| g.cluster >= lo && g.cluster < hi)
            .map(|g| (g.origin.x, b.y, g.advance, b.height))
            .collect::<Vec<_>>();
        let rects = if rects.is_empty() && lo == t.range.start && hi == t.range.end {
            vec![(b.x, b.y, b.width, b.height)]
        } else {
            rects
        };
        let line = enclosing_line(tree, frag.parent);
        let block = line.and_then(|l| tree.get(l)).and_then(|l| l.parent);
        out.push(Piece {
            line,
            block,
            text,
            rects,
        });
    }
    out
}

/// The nearest line box at or above `id` (text inside inline boxes sits
/// a few levels below its line).
fn enclosing_line(tree: &FragmentTree, mut id: Option<FragmentId>) -> Option<FragmentId> {
    for _ in 0..wb_layout::MAX_BOX_DEPTH {
        let f = tree.get(id?)?;
        if matches!(f.kind, FragmentKind::Line(_)) {
            return id;
        }
        id = f.parent;
    }
    None
}

/// The selected text as rendered (white space collapsed), with a space
/// between lines of one block and a line break between blocks.
#[must_use]
pub fn selected_text(tree: &FragmentTree, order: &TreeOrder, sel: &Selection) -> String {
    let mut out = String::new();
    let mut prev: Option<(Option<FragmentId>, Option<FragmentId>)> = None;
    for p in pieces(tree, order, sel) {
        if let Some((line, block)) = prev
            && line != p.line
        {
            let sep = if block == p.block { ' ' } else { '\n' };
            if !out.ends_with([' ', '\n']) || sep == '\n' {
                if sep == '\n' {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                }
                out.push(sep);
            }
        }
        out.push_str(p.text);
        prev = Some((p.line, p.block));
    }
    out.trim_end().to_string()
}

/// Rectangles (document CSS px) covering the selected glyphs, merged per
/// fragment.
#[must_use]
pub fn highlight_rects(
    tree: &FragmentTree,
    order: &TreeOrder,
    sel: &Selection,
) -> Vec<HighlightRect> {
    let mut out = Vec::new();
    for p in pieces(tree, order, sel) {
        let mut it = p.rects.into_iter();
        let Some(mut cur) = it.next() else { continue };
        for r in it {
            if (r.0 - (cur.0 + cur.2)).abs() < 0.5 && r.1 == cur.1 {
                cur.2 = r.0 + r.2 - cur.0;
            } else {
                out.push(cur);
                cur = r;
            }
        }
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wb_url::Url;

    use super::*;
    use crate::engine::{Engine, LocalFetcher, Point, RenderOptions};

    #[test]
    fn select_and_copy() {
        let fonts = Engine::system_fonts();
        let engine = Engine::new(Arc::new(LocalFetcher::new()), fonts.clone());
        let mut page = engine
            .load_html(
                Url::parse("https://a.test/").unwrap(),
                "<body style='margin:0;font-size:20px'><p id=a>Hello   brave\nnew world</p>\
                 <p id=b>Second <b>para</b></p>",
            )
            .unwrap();
        page.render(&RenderOptions::new(400.0, 200.0)).unwrap();
        let doc = page.document();
        let order = TreeOrder::new(doc);
        let tree = page.fragment_tree().unwrap();
        let all = Selection::all(doc).unwrap();
        assert_eq!(
            selected_text(tree, &order, &all),
            "Hello brave new world\nSecond para"
        );
        let t1 = doc.first_child(doc.element_by_id("a").unwrap()).unwrap();
        let b = doc.element_by_id("b").unwrap();
        let t2 = doc.first_child(b).unwrap();
        // From "brave" (offset 8) to before "cond" (offset 2), backwards.
        let sel = Selection {
            anchor: Caret {
                node: t2,
                offset: 2,
            },
            focus: Caret {
                node: t1,
                offset: 8,
            },
        };
        assert_eq!(selected_text(tree, &order, &sel), "brave new world\nSe");
        let collapsed = Selection {
            anchor: sel.focus,
            focus: sel.focus,
        };
        assert!(collapsed.is_collapsed());
        assert_eq!(selected_text(tree, &order, &collapsed), "");
        if !fonts.is_empty() {
            let rects = highlight_rects(tree, &order, &sel);
            assert!(rects.len() >= 2, "{rects:?}");
            // Hit testing a point in "world" yields a caret in t1.
            let hit = page
                .hit_test(Point::new(rects[0].0 + 2.0, rects[0].1 + 5.0))
                .unwrap();
            assert_eq!(hit.node, Some(t1));
            assert!(hit.text_offset.is_some());
        }
    }
}
