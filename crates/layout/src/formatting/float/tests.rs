//! Float layout tests: real HTML styled by `wb-style`, laid out with the
//! fake text engine (every character 8px wide at the default 16px size,
//! `line-height: normal` = 16px). The helpers are shared with the
//! positioning tests.

use wb_css::parse_stylesheet;
use wb_dom::{Document, NodeId};
use wb_style::{MediaContext, SheetInput, StyleOptions, style_document};

use crate::context::layout_tree_with;
use crate::text::FakeText;
use crate::*;

pub(crate) struct Laid {
    pub doc: Document,
    pub tree: FragmentTree,
}

pub(crate) fn lay_at(html: &str, css: &str, width: f32) -> Laid {
    let doc = wb_html::parse_document(html, &Default::default()).document;
    let sheets = vec![SheetInput::author(parse_stylesheet(css))];
    let media = MediaContext {
        viewport_width: width,
        viewport_height: 600.0,
        ..MediaContext::default()
    };
    let styles = style_document(&doc, &sheets, &media, &StyleOptions::default());
    let boxes = build_box_tree(&doc, &styles);
    let tree = layout_tree_with(
        &boxes,
        &FakeText,
        &NoHost,
        &LayoutOptions {
            viewport: Size::new(width, 600.0),
        },
    );
    Laid { doc, tree }
}

pub(crate) fn lay(html: &str, css: &str) -> Laid {
    lay_at(html, css, 800.0)
}

pub(crate) const RESET: &str = "body { margin: 0 } p { margin: 0 } ";

impl Laid {
    pub fn node(&self, id: &str) -> NodeId {
        self.doc.element_by_id(id).expect("element")
    }
    pub fn id_of(&self, id: &str) -> FragmentId {
        *self
            .tree
            .fragments_for_node(self.node(id))
            .first()
            .expect("fragment")
    }
    pub fn frag(&self, id: &str) -> &Fragment {
        self.tree.get(self.id_of(id)).expect("fragment")
    }
    pub fn rect(&self, id: &str) -> Rect {
        self.frag(id).border_box
    }
    /// The line boxes of `#id`: (x, y, width) each.
    pub fn lines(&self, id: &str) -> Vec<Rect> {
        self.tree
            .children(self.id_of(id))
            .iter()
            .filter_map(|c| self.tree.get(*c))
            .filter(|c| matches!(c.kind, FragmentKind::Line(_)))
            .map(|c| c.border_box)
            .collect()
    }
    /// The rendered text of each line box of `#id`.
    pub fn line_texts(&self, id: &str) -> Vec<String> {
        self.tree
            .children(self.id_of(id))
            .iter()
            .filter(|c| {
                matches!(
                    self.tree.get(**c).map(|x| &x.kind),
                    Some(FragmentKind::Line(_))
                )
            })
            .map(|c| {
                let mut out = String::new();
                let mut stack = vec![*c];
                while let Some(id) = stack.pop() {
                    let Some(fr) = self.tree.get(id) else {
                        continue;
                    };
                    if let FragmentKind::Text(t) = &fr.kind {
                        out.push_str(&t.text[t.range.clone()]);
                    }
                    stack.extend(fr.children.iter().rev().copied());
                }
                out
            })
            .collect()
    }
}

pub(crate) fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(x, y, w, h)
}

#[track_caller]
pub(crate) fn assert_rect(got: Rect, want: Rect) {
    let close = |a: f32, b: f32| (a - b).abs() < 0.01;
    assert!(
        close(got.x, want.x)
            && close(got.y, want.y)
            && close(got.width, want.width)
            && close(got.height, want.height),
        "got {got:?}, want {want:?}"
    );
}

#[test]
fn left_float_shortens_lines_beside_it() {
    let l = lay(
        r#"<div id=c><div id=f></div><p id=p>aaaa bbbb cccc dddd eeee ffff</p></div>"#,
        &format!("{RESET} #c {{ width: 200px }} #f {{ float: left; width: 50px; height: 40px }}"),
    );
    assert_rect(l.rect("f"), rect(0.0, 0.0, 50.0, 40.0));
    let lines = l.lines("p");
    // Lines overlapping the float [0, 40) start at 50 and are 150 wide
    // (3 words of 40px fit: "aaaa bbbb cccc" is 112px, 4 would be 152).
    assert_rect(lines[0], rect(50.0, 0.0, 150.0, 16.0));
    assert_rect(lines[1], rect(50.0, 16.0, 150.0, 16.0));
    assert_eq!(l.line_texts("p")[0].trim_end(), "aaaa bbbb cccc");
    assert_eq!(l.line_texts("p")[1].trim_end(), "dddd eeee ffff");
    // The paragraph itself ignores the float: it starts at the left edge.
    assert_rect(l.rect("p"), rect(0.0, 0.0, 200.0, 32.0));
    // The container is not a BFC root: the float overflows it.
    assert_eq!(l.rect("c").height, 32.0);
}

#[test]
fn right_float_infobox_with_wrapping_text() {
    // A Wikipedia-like infobox: a right-floated table, then paragraphs.
    let l = lay(
        r#"<div id=c><table id=info><tr><td>Infobox</td></tr></table>
           <p id=p1>aaaa bbbb cccc dddd eeee</p><p id=p2>ffff gggg</p></div>"#,
        &format!(
            "{RESET} #c {{ width: 300px }} \
             #info {{ float: right; width: 120px; height: 40px; margin: 0 0 8px 8px; border-spacing: 0 }}"
        ),
    );
    // Right edge of the container; the left margin keeps text 8px away.
    assert_rect(l.rect("info"), rect(180.0, 0.0, 120.0, 40.0));
    let lines = l.lines("p1");
    // Free band: [0, 172) = 172px: "aaaa bbbb cccc dddd" is 152px.
    assert_rect(lines[0], rect(0.0, 0.0, 172.0, 16.0));
    assert_eq!(l.line_texts("p1")[0].trim_end(), "aaaa bbbb cccc dddd");
    // p2 starts at y=32, still beside the float's margin box (to 48).
    assert_rect(l.lines("p2")[0], rect(0.0, 32.0, 172.0, 16.0));
}

#[test]
fn floats_stack_side_by_side_and_wrap_below() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div><div id=d></div><div id=r></div></div>"#,
        &format!(
            "{RESET} #c {{ width: 200px }} #a, #b, #d {{ float: left; width: 80px }} \
             #a {{ height: 30px }} #b {{ height: 20px }} #d {{ height: 10px }} \
             #r {{ float: right; width: 30px; height: 10px }}"
        ),
    );
    assert_rect(l.rect("a"), rect(0.0, 0.0, 80.0, 30.0));
    assert_rect(l.rect("b"), rect(80.0, 0.0, 80.0, 20.0));
    // No room for a third 80px float next to the first two: it goes below
    // the shorter one, next to `a`.
    assert_rect(l.rect("d"), rect(80.0, 20.0, 80.0, 10.0));
    // A right float may not be higher than an earlier float's top
    // (§9.5.1 rule 6): it sits at y=20 too.
    assert_rect(l.rect("r"), rect(170.0, 20.0, 30.0, 10.0));
}

#[test]
fn clear_moves_below_floats_and_clearfix_extends_parent() {
    let l = lay(
        r#"<div id=c><div id=f></div><p id=p>x</p><div id=cl></div></div><p id=after>y</p>"#,
        &format!(
            "{RESET} #c {{ width: 200px }} #f {{ float: left; width: 50px; height: 60px }} \
             #cl {{ clear: both }}"
        ),
    );
    // The empty clearing block sits at the float's bottom and the
    // container grows to contain it (the classic clearfix).
    assert_eq!(l.rect("cl").y, 60.0);
    assert_eq!(l.rect("c").height, 60.0);
    assert_eq!(l.rect("after").y, 60.0);
    assert_rect(l.lines("after")[0], rect(0.0, 60.0, 800.0, 16.0));
}

#[test]
fn clear_left_ignores_right_floats() {
    let l = lay(
        r#"<div id=c><div id=l></div><div id=r></div><div id=x>x</div></div>"#,
        &format!(
            "{RESET} #c {{ width: 200px }} #l {{ float: left; width: 20px; height: 20px }} \
             #r {{ float: right; width: 20px; height: 50px }} #x {{ clear: left; margin-top: 5px }}"
        ),
    );
    // The floats sit at the container's top: the cleared box's margin does
    // not collapse through to them.
    assert_eq!(l.rect("l").y, 0.0);
    assert_eq!(l.rect("x").y, 20.0);
    // Still beside the right float.
    assert_rect(l.lines("x")[0], rect(0.0, 20.0, 180.0, 16.0));
}

#[test]
fn bfc_root_avoids_floats_and_contains_its_own() {
    let l = lay(
        r#"<div id=c><div id=f></div><div id=o><div id=inner></div></div></div>"#,
        &format!(
            "{RESET} #c {{ width: 200px }} #f {{ float: left; width: 50px; height: 100px }} \
             #o {{ overflow: hidden; margin-left: 10px }} \
             #inner {{ float: right; width: 20px; height: 30px }}"
        ),
    );
    // The BFC root's border box starts at the float's edge (its 10px
    // margin overlaps the float) and fills the rest.
    assert_rect(l.rect("o"), rect(50.0, 0.0, 150.0, 30.0));
    // Its auto height includes its own float (§10.6.7).
    assert_rect(l.rect("inner"), rect(180.0, 0.0, 20.0, 30.0));
}

#[test]
fn wide_bfc_root_moves_below_floats() {
    let l = lay(
        r#"<div id=c><div id=f></div><div id=o></div></div>"#,
        &format!(
            "{RESET} #c {{ width: 200px }} #f {{ float: left; width: 150px; height: 40px }} \
             #o {{ overflow: hidden; width: 100px; height: 10px }}"
        ),
    );
    assert_rect(l.rect("o"), rect(0.0, 40.0, 100.0, 10.0));
}

#[test]
fn float_lines_up_with_the_next_block_after_margin_collapsing() {
    let l = lay(
        r#"<div id=a>a</div><div id=f></div><div id=b>b</div>"#,
        &format!(
            "{RESET} #a {{ margin-bottom: 20px }} #b {{ margin-top: 10px }} \
             #f {{ float: left; width: 30px; height: 30px }}"
        ),
    );
    assert_eq!(l.rect("b").y, 36.0);
    assert_rect(l.rect("f"), rect(0.0, 36.0, 30.0, 30.0));
    assert_rect(l.lines("b")[0], rect(30.0, 36.0, 770.0, 16.0));
}

#[test]
fn float_at_top_of_collapsing_parent_moves_with_it() {
    let l = lay(
        r#"<div id=p><div id=f></div><p id=t>t</p></div>"#,
        &format!(
            "{RESET} #t {{ margin-top: 20px }} #f {{ float: left; width: 30px; height: 30px }}"
        ),
    );
    // The paragraph's margin collapses through #p; the float follows the
    // collapsed content top.
    assert_eq!(l.rect("p").y, 20.0);
    assert_rect(l.rect("f"), rect(0.0, 20.0, 30.0, 30.0));
    assert_rect(l.lines("t")[0], rect(30.0, 20.0, 770.0, 16.0));
}

#[test]
fn line_too_narrow_moves_below_the_float() {
    let l = lay(
        r#"<div id=c><div id=f></div><p id=p>abcdefghij</p></div>"#,
        &format!("{RESET} #c {{ width: 100px }} #f {{ float: left; width: 50px; height: 20px }}"),
    );
    // The 80px word does not fit in the 50px beside the float.
    assert_rect(l.lines("p")[0], rect(0.0, 20.0, 100.0, 16.0));
    assert_eq!(l.rect("p").height, 36.0);
}

#[test]
fn float_shrinks_to_fit_and_floats_respect_margins() {
    let l = lay(
        r#"<div id=c><span id=f>abc de</span><p id=p>x</p></div>"#,
        &format!(
            "{RESET} #c {{ width: 300px }} #f {{ float: right; margin: 5px 7px; padding: 2px }}"
        ),
    );
    // A floated span is blockified; its width is its max-content (48px).
    assert_rect(l.rect("f"), rect(300.0 - 7.0 - 52.0, 5.0, 52.0, 20.0));
    assert!(l.frag("f").stacking.float);
    assert_rect(l.lines("p")[0], rect(0.0, 0.0, 300.0 - 66.0, 16.0));
}

#[test]
fn wikipedia_thumbnail_floats_right_between_paragraphs() {
    let l = lay(
        r#"<div id=content><h2 id=h>Heading</h2>
           <p id=p1>aaaa bbbb</p>
           <figure id=thumb><div id=img></div><figcaption>Cap</figcaption></figure>
           <p id=p2>cccc dddd eeee ffff gggg hhhh iiii jjjj</p>
           <p id=p3>after</p></div>"#,
        &format!(
            "{RESET} h2 {{ margin: 0 0 8px; font-size: 16px }} #content {{ width: 240px }} \
             #thumb {{ float: right; clear: right; margin: 0 0 8px 12px }} \
             #img {{ width: 100px; height: 60px }} figcaption {{ height: 16px }}"
        ),
    );
    // The thumbnail floats right below p1, lined up with p2.
    let p2 = l.rect("p2");
    assert_rect(l.rect("thumb"), rect(140.0, p2.y, 100.0, 76.0));
    // p2's lines are 128px wide beside the thumbnail (240 - 112).
    let lines = l.lines("p2");
    assert!(lines.iter().take(4).all(|r| r.width == 128.0));
    assert_eq!(l.line_texts("p2")[0].trim_end(), "cccc dddd eeee");
    // Below the float (76 + 8 margin) lines take the full width.
    let last = l.lines("p3")[0];
    assert!(last.y >= p2.y + 84.0 || last.width == 128.0);
}

#[test]
fn root_height_and_document_include_floats() {
    let l = lay(
        r#"<body id=b><div id=f></div></body>"#,
        &format!("{RESET} #f {{ float: left; width: 10px; height: 900px }}"),
    );
    // body is not a BFC root: its height ignores the float, but the root
    // (a BFC root) and the scrollable overflow include it.
    assert_eq!(l.rect("b").height, 0.0);
    assert!(l.tree.document_overflow().height >= 900.0);
    let root = l.tree.root().and_then(|r| l.tree.get(r)).expect("root");
    assert_eq!(root.border_box.height, 900.0);
}

#[test]
fn thousands_of_floats_lay_out_quickly() {
    let mut html = String::from("<div id=c>");
    for i in 0..3000 {
        html.push_str(if i % 3 == 0 {
            "<i class=l></i>"
        } else if i % 3 == 1 {
            "<i class=r></i>"
        } else {
            "<p>word word word</p>"
        });
    }
    html.push_str("</div>");
    let start = std::time::Instant::now();
    let l = lay(
        &html,
        &format!(
            "{RESET} #c {{ width: 400px }} .l {{ float: left; width: 30px; height: 25px }} \
             .r {{ float: right; width: 10px; height: 70px }}"
        ),
    );
    assert!(l.tree.len() > 3000);
    assert!(start.elapsed().as_secs() < 20, "took {:?}", start.elapsed());
}
