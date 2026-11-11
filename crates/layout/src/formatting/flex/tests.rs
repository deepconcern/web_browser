//! Flex layout tests: real HTML styled by `wb-style`, laid out with the
//! fake text engine (8px per character, 16px lines at the default size).

use std::collections::HashMap;

use wb_css::parse_stylesheet;
use wb_dom::{Document, NodeId};
use wb_style::{MediaContext, SheetInput, StyleOptions, style_document};

use super::{Distribute, MAX_ITEMS_PER_LINE};
use crate::context::layout_tree_with;
use crate::text::FakeText;
use crate::*;

struct Laid {
    doc: Document,
    tree: FragmentTree,
}

#[derive(Default)]
struct Host {
    images: HashMap<NodeId, Size>,
}

impl LayoutHost for Host {
    fn image_size(&self, node: NodeId) -> Option<Size> {
        self.images.get(&node).copied()
    }
    fn scroll_offset(&self, _node: NodeId) -> Point {
        Point::ZERO
    }
}

fn lay_with(html: &str, css: &str, host: &dyn Fn(&Document) -> Host) -> Laid {
    let doc = wb_html::parse_document(html, &Default::default()).document;
    let css = format!("body {{ margin: 0 }} p {{ margin: 0 }} {css}");
    let sheets = vec![SheetInput::author(parse_stylesheet(&css))];
    let media = MediaContext {
        viewport_width: 800.0,
        viewport_height: 600.0,
        ..MediaContext::default()
    };
    let styles = style_document(&doc, &sheets, &media, &StyleOptions::default());
    let boxes = build_box_tree(&doc, &styles);
    let h = host(&doc);
    let tree = layout_tree_with(
        &boxes,
        &FakeText,
        &h,
        &LayoutOptions {
            viewport: Size::new(800.0, 600.0),
        },
    );
    Laid { doc, tree }
}

fn lay(html: &str, css: &str) -> Laid {
    lay_with(html, css, &|_| Host::default())
}

impl Laid {
    fn rect(&self, id: &str) -> Rect {
        let n = self.doc.element_by_id(id).expect("element");
        let f = *self.tree.fragments_for_node(n).first().expect("fragment");
        self.tree.get(f).expect("fragment").border_box
    }
    /// (x, y, width, height) of `#id`, rounded to 0.01px.
    fn xywh(&self, id: &str) -> (f32, f32, f32, f32) {
        let r = self.rect(id);
        let q = |v: f32| (v * 100.0).round() / 100.0;
        (q(r.x), q(r.y), q(r.width), q(r.height))
    }
}

#[test]
fn row_items_sit_side_by_side_and_stretch() {
    let l = lay(
        r#"<div id=c><div id=a>aa</div><div id=b>b<br>b<br>b</div><div id=d style="height:10px"></div></div>"#,
        "#c { display: flex } #a, #b, #d { width: 100px }",
    );
    assert_eq!(l.xywh("c"), (0.0, 0.0, 800.0, 48.0));
    assert_eq!(l.xywh("a"), (0.0, 0.0, 100.0, 48.0));
    assert_eq!(l.xywh("b"), (100.0, 0.0, 100.0, 48.0));
    // A definite height is not stretched.
    assert_eq!(l.xywh("d"), (200.0, 0.0, 100.0, 10.0));
}

#[test]
fn flex_grow_distributes_free_space() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div><div id=d></div></div>"#,
        "#c { display: flex; width: 400px } #a { flex: 1 } #b { flex: 2 } #d { width: 100px }",
    );
    assert_eq!(l.xywh("a").2, 100.0);
    assert_eq!(l.xywh("b"), (100.0, 0.0, 200.0, 0.0));
    assert_eq!(l.xywh("d").0, 300.0);
}

#[test]
fn flex_grow_below_one_leaves_space() {
    // §9.7 step 4b: factors summing to less than 1 take that fraction.
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div></div>"#,
        "#c { display: flex; width: 400px } #a { flex: 0.25 0 0 } #b { flex: 0.25 0 0 }",
    );
    assert_eq!(l.xywh("a").2, 100.0);
    assert_eq!(l.xywh("b"), (100.0, 0.0, 100.0, 0.0));
}

#[test]
fn flex_shrink_is_weighted_by_base_size() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div></div>"#,
        "#c { display: flex; width: 300px } #a { width: 200px } #b { width: 400px }",
    );
    assert_eq!(l.xywh("a").2, 100.0);
    assert_eq!(l.xywh("b"), (100.0, 0.0, 200.0, 0.0));
}

#[test]
fn min_and_max_violations_freeze_items() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div><div id=d></div></div>"#,
        "#c { display: flex; width: 300px } div div { flex: 1 } #a { min-width: 200px } #d { max-width: 20px }",
    );
    assert_eq!(l.xywh("a").2, 200.0);
    assert_eq!(l.xywh("b").2, 80.0);
    assert_eq!(l.xywh("d"), (280.0, 0.0, 20.0, 0.0));
}

#[test]
fn automatic_minimum_size_keeps_min_content() {
    // §4.5: items do not shrink below their min-content width (80px for
    // a 10-character word) unless `min-width` or `overflow` says so.
    let l = lay(
        r#"<div id=c><div id=a>xxxxxxxxxx</div><div id=b>xxxxxxxxxx</div><div id=d>xxxxxxxxxx</div></div>"#,
        "#c { display: flex; width: 100px } #a, #b, #d { width: 200px } #b { min-width: 0 } #d { overflow: hidden }",
    );
    assert_eq!(l.xywh("a").2, 80.0);
    // The other two share what is left (100 - 80 = 20; with min-width 0
    // they shrink from 200px each by equal weights).
    let b = l.xywh("b").2;
    let d = l.xywh("d").2;
    assert!(b <= 10.0 && approx(b, d), "{b} {d}");
}

#[test]
fn padding_border_and_margins_are_outer() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div></div>"#,
        "#c { display: flex; width: 300px; padding: 5px; border: 1px solid } #a { flex: 1; padding: 0 10px; margin: 0 5px; border: 2px solid } #b { width: 50px }",
    );
    // Inner 300; b 50; a outer = 250 = content + 20 + 4 + 10.
    assert_eq!(l.xywh("a"), (11.0, 6.0, 240.0, 4.0));
    assert_eq!(l.xywh("b").0, 6.0 + 250.0);
}

#[test]
fn justify_content_values() {
    let css = |j: &str| {
        format!(
            "#c {{ display: flex; width: 300px; justify-content: {j} }} #c div {{ width: 50px }}"
        )
    };
    let html = r#"<div id=c><div id=a></div><div id=b></div></div>"#;
    let xs = |j: &str| {
        let l = lay(html, &css(j));
        (l.xywh("a").0, l.xywh("b").0)
    };
    assert_eq!(xs("flex-start"), (0.0, 50.0));
    assert_eq!(xs("flex-end"), (200.0, 250.0));
    assert_eq!(xs("center"), (100.0, 150.0));
    assert_eq!(xs("space-between"), (0.0, 250.0));
    assert_eq!(xs("space-around"), (50.0, 200.0));
    let (a, b) = xs("space-evenly");
    assert!(approx(a, 200.0 / 3.0) && approx(b, 400.0 / 3.0 + 50.0));
    assert_eq!(xs("end"), (200.0, 250.0));
    assert_eq!(xs("right"), (200.0, 250.0));
}

#[test]
fn negative_free_space_fallbacks() {
    let html = r#"<div id=c><div id=a></div><div id=b></div></div>"#;
    let l = lay(
        html,
        "#c { display: flex; width: 50px; justify-content: space-between } #c div { width: 50px; flex-shrink: 0 }",
    );
    assert_eq!(l.xywh("a").0, 0.0);
    let l = lay(
        html,
        "#c { display: flex; width: 50px; justify-content: space-around } #c div { width: 50px; flex-shrink: 0 }",
    );
    assert_eq!(l.xywh("a").0, -25.0);
}

#[test]
fn row_reverse_and_order() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div><div id=d></div></div>"#,
        "#c { display: flex; flex-direction: row-reverse; width: 300px } #c div { width: 50px } #a { order: 2 }",
    );
    // Order-modified: b, d, a; laid out from the right.
    assert_eq!(l.xywh("b").0, 250.0);
    assert_eq!(l.xywh("d").0, 200.0);
    assert_eq!(l.xywh("a").0, 150.0);
    // `justify-content: flex-end` packs toward the left in row-reverse,
    // `start` packs at the physical start (left).
    let l = lay(
        r#"<div id=c><div id=a></div></div>"#,
        "#c { display: flex; flex-direction: row-reverse; width: 300px; justify-content: flex-end } #a { width: 50px }",
    );
    assert_eq!(l.xywh("a").0, 0.0);
    let l = lay(
        r#"<div id=c><div id=a></div></div>"#,
        "#c { display: flex; flex-direction: row-reverse; width: 300px; justify-content: start } #a { width: 50px }",
    );
    assert_eq!(l.xywh("a").0, 0.0);
}

#[test]
fn order_changes_paint_order() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div></div>"#,
        "#c { display: flex } #a { order: 1 }",
    );
    let c = l.doc.element_by_id("c").expect("c");
    let cf = l.tree.fragments_for_node(c)[0];
    let kids: Vec<_> = l
        .tree
        .children(cf)
        .iter()
        .filter_map(|f| l.tree.get(*f).and_then(|f| f.node))
        .collect();
    assert_eq!(
        kids,
        vec![
            l.doc.element_by_id("b").expect("b"),
            l.doc.element_by_id("a").expect("a")
        ]
    );
}

#[test]
fn auto_margins_absorb_free_space() {
    // A Wikipedia-like header: logo at the left, tools pushed right.
    let l = lay(
        r#"<header id=c><div id=logo></div><div id=search></div><div id=tools style="margin-left:auto"></div></header>"#,
        "#c { display: flex; width: 800px; height: 50px } #logo { width: 100px; height: 50px } #search { width: 300px; height: 30px; margin: auto 0 } #tools { width: 100px; height: 20px }",
    );
    assert_eq!(l.xywh("logo"), (0.0, 0.0, 100.0, 50.0));
    // Cross auto margins centre vertically.
    assert_eq!(l.xywh("search"), (100.0, 10.0, 300.0, 30.0));
    assert_eq!(l.xywh("tools").0, 700.0);
    // Both main margins auto: centred.
    let l = lay(
        r#"<div id=c><div id=a></div></div>"#,
        "#c { display: flex; width: 300px; height: 100px } #a { width: 100px; height: 20px; margin: auto }",
    );
    assert_eq!(l.xywh("a"), (100.0, 40.0, 100.0, 20.0));
}

#[test]
fn align_items_and_align_self() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div><div id=d></div><div id=e></div></div>"#,
        "#c { display: flex; height: 100px; align-items: center } #c div { width: 10px; height: 20px } #b { align-self: flex-end } #d { align-self: flex-start } #c #e { align-self: stretch; height: auto }",
    );
    assert_eq!(l.xywh("a").1, 40.0);
    assert_eq!(l.xywh("b").1, 80.0);
    assert_eq!(l.xywh("d").1, 0.0);
    assert_eq!(l.xywh("e"), (30.0, 0.0, 10.0, 100.0));
}

#[test]
fn stretch_respects_max_height_and_margins() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div></div>"#,
        "#c { display: flex; height: 100px } #a { width: 10px; max-height: 30px } #b { width: 10px; margin: 10px 0 }",
    );
    assert_eq!(l.xywh("a").3, 30.0);
    assert_eq!(l.xywh("b"), (10.0, 10.0, 10.0, 80.0));
}

#[test]
fn baseline_alignment() {
    let l = lay(
        r#"<div id=c><div id=a>a</div><div id=b>b</div></div>"#,
        "#c { display: flex; align-items: baseline } #a { padding-top: 20px } #b { font-size: 32px }",
    );
    // a's baseline: 20 + 12.8; b's: 25.6. b moves down by 7.2.
    let a = l.xywh("a");
    let b = l.xywh("b");
    assert_eq!(a.1, 0.0);
    assert!(approx(b.1, 20.0 + 12.8 - 25.6), "{b:?}");
    // The line is tall enough for both: max ascent 32.8 + max descent.
    let c = l.xywh("c");
    assert!(approx(c.3, 32.8 + 6.4), "{c:?}");
}

#[test]
fn wrapping_lines_and_align_content() {
    let html = r#"<div id=c><div id=a></div><div id=b></div><div id=d></div></div>"#;
    let base =
        "#c { display: flex; flex-wrap: wrap; width: 250px } #c div { width: 100px; height: 20px }";
    let l = lay(html, base);
    assert_eq!(l.xywh("a"), (0.0, 0.0, 100.0, 20.0));
    assert_eq!(l.xywh("b"), (100.0, 0.0, 100.0, 20.0));
    assert_eq!(l.xywh("d"), (0.0, 20.0, 100.0, 20.0));
    assert_eq!(l.xywh("c").3, 40.0);
    // Definite height: lines stretch (align-content: normal).
    let l = lay(
        html,
        &format!("{base} #c {{ height: 100px }} #c div {{ height: auto }}"),
    );
    assert_eq!(l.xywh("a").3, 50.0);
    assert_eq!(l.xywh("d").1, 50.0);
    let l = lay(
        html,
        &format!("{base} #c {{ height: 100px; align-content: space-between }}"),
    );
    assert_eq!(l.xywh("d").1, 80.0);
    let l = lay(
        html,
        &format!("{base} #c {{ height: 100px; align-content: center }}"),
    );
    assert_eq!(l.xywh("a").1, 30.0);
    // wrap-reverse: the first line is at the bottom.
    let l = lay(html, &format!("{base} #c {{ flex-wrap: wrap-reverse }}"));
    assert_eq!(l.xywh("a").1, 20.0);
    assert_eq!(l.xywh("d").1, 0.0);
}

#[test]
fn gaps() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div><div id=d></div></div>"#,
        "#c { display: flex; flex-wrap: wrap; width: 230px; column-gap: 10px; row-gap: 5px } #c div { width: 100px; height: 20px }",
    );
    assert_eq!(l.xywh("b").0, 110.0);
    assert_eq!(l.xywh("d"), (0.0, 25.0, 100.0, 20.0));
    assert_eq!(l.xywh("c").3, 45.0);
    // Gaps take space before flexing.
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div></div>"#,
        "#c { display: flex; width: 210px; gap: 10px } #c div { flex: 1 }",
    );
    assert_eq!(l.xywh("a").2, 100.0);
    assert_eq!(l.xywh("b").0, 110.0);
}

#[test]
fn column_direction() {
    let l = lay(
        r#"<div id=c><div id=a>a</div><div id=b>b<br>b</div><div id=d style="width:50px">d</div></div>"#,
        "#c { display: flex; flex-direction: column; width: 200px }",
    );
    assert_eq!(l.xywh("a"), (0.0, 0.0, 200.0, 16.0));
    assert_eq!(l.xywh("b"), (0.0, 16.0, 200.0, 32.0));
    assert_eq!(l.xywh("d"), (0.0, 48.0, 50.0, 16.0));
    assert_eq!(l.xywh("c").3, 64.0);
    // With a definite height, items grow and justify.
    let l = lay(
        r#"<div id=c><div id=a>a</div><div id=b>b</div></div>"#,
        "#c { display: flex; flex-direction: column; height: 100px } #a { flex-grow: 1 }",
    );
    assert_eq!(l.xywh("a").3, 84.0);
    assert_eq!(l.xywh("b").1, 84.0);
    let l = lay(
        r#"<div id=c><div id=a>a</div><div id=b>b</div></div>"#,
        "#c { display: flex; flex-direction: column-reverse; height: 100px }",
    );
    assert_eq!(l.xywh("a").1, 84.0);
    assert_eq!(l.xywh("b").1, 68.0);
    // align-items: center shrinks items to fit their content.
    let l = lay(
        r#"<div id=c><div id=a>abcd</div></div>"#,
        "#c { display: flex; flex-direction: column; width: 200px; align-items: center }",
    );
    assert_eq!(l.xywh("a"), (84.0, 0.0, 32.0, 16.0));
}

#[test]
fn column_min_height_auto_prevents_shrinking() {
    let l = lay(
        r#"<div id=c><div id=a>a<br>a<br>a</div><div id=b style="min-height:0">b<br>b<br>b</div></div>"#,
        "#c { display: flex; flex-direction: column; height: 60px }",
    );
    assert_eq!(l.xywh("a").3, 48.0);
    assert_eq!(l.xywh("b").3, 12.0);
}

#[test]
fn column_max_height_wraps() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div><div id=d></div></div>"#,
        "#c { display: flex; flex-direction: column; flex-wrap: wrap; max-height: 50px; width: 300px; align-content: flex-start } #c div { height: 20px; width: 30px }",
    );
    assert_eq!(l.xywh("b").1, 20.0);
    assert_eq!(l.xywh("d"), (30.0, 0.0, 30.0, 20.0));
    assert_eq!(l.xywh("c").3, 40.0);
}

#[test]
fn text_runs_become_anonymous_items() {
    let l = lay(
        r#"<div id=c>hello <span id=s>x</span> world</div>"#,
        "#c { display: flex }",
    );
    // Items: "hello " (40px, trailing space collapses away), <span>, "world".
    assert_eq!(l.xywh("s").0, 40.0);
    assert_eq!(l.xywh("c").3, 16.0);
    // White space between elements generates no item.
    let l = lay(
        "<div id=c>\n  <div id=a>a</div>\n  <div id=b>b</div>\n</div>",
        "#c { display: flex }",
    );
    assert_eq!(l.xywh("b").0, 8.0);
}

#[test]
fn inline_flex_shrinks_to_fit() {
    let l = lay(
        r#"<div><span id=c><span id=a>aaaa</span><span id=b>bb</span></span>x</div>"#,
        "#c { display: inline-flex; column-gap: 4px }",
    );
    assert_eq!(l.xywh("c").2, 32.0 + 4.0 + 16.0);
    assert_eq!(l.xywh("b").0, 36.0);
}

#[test]
fn nested_flex_shrink_to_fit_in_row() {
    // A flex container as a flex item is sized by its content (§9.9).
    let l = lay(
        r#"<div id=c><div id=n><div>aa</div><div>bbb</div></div><div id=f></div></div>"#,
        "#c { display: flex; width: 400px } #n { display: flex } #f { flex: 1 }",
    );
    assert_eq!(l.xywh("n").2, 40.0);
    assert_eq!(l.xywh("f"), (40.0, 0.0, 360.0, 16.0));
}

#[test]
fn absolutely_positioned_children_use_sole_item_static_position() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=p>pp</div><div id=b></div></div>"#,
        "#c { display: flex; width: 300px; justify-content: center } #a, #b { width: 50px } #p { position: absolute }",
    );
    // Not a flex item: a and b are centred together.
    assert_eq!(l.xywh("a").0, 100.0);
    assert_eq!(l.xywh("b").0, 150.0);
    // §4.1: laid out by the positioning module (shrink-to-fit, 16px) at
    // the static position of the sole flex item, centred.
    assert_eq!(l.xywh("p"), (142.0, 0.0, 16.0, 16.0));
}

#[test]
fn absolutely_positioned_flex_children_follow_alignment_and_insets() {
    let l = lay(
        r#"<div id=c><div id=a>a</div><div id=e>e</div><div id=i>i</div><div id=n></div></div>"#,
        "#c { position: relative; display: flex; flex-direction: column; width: 200px; height: 100px; \
         padding: 10px; justify-content: flex-end; align-items: center } \
         #a, #e, #i { position: absolute } #e { align-self: flex-start } \
         #i { top: 5px; right: 5px } #n { width: 10px; height: 10px }",
    );
    // Column: main axis vertical (flex-end), cross axis horizontal
    // (centre). Coordinates are relative to the container, whose padding
    // box is the containing block.
    let c = l.xywh("c");
    assert_eq!(
        l.xywh("a"),
        (c.0 + 10.0 + 96.0, c.1 + 10.0 + 84.0, 8.0, 16.0)
    );
    assert_eq!(l.xywh("e"), (c.0 + 10.0, c.1 + 94.0, 8.0, 16.0));
    // Insets win over the static position.
    assert_eq!(l.xywh("i"), (c.0 + 220.0 - 5.0 - 8.0, c.1 + 5.0, 8.0, 16.0));
    // Only #n is a flex item, at the end of the main axis.
    assert_eq!(l.xywh("n").1, c.1 + 10.0 + 90.0);
}

#[test]
fn images_keep_ratio_and_stretch() {
    let host = |d: &Document| {
        let mut h = Host::default();
        for id in ["i", "j"] {
            if let Some(n) = d.element_by_id(id) {
                h.images.insert(n, Size::new(100.0, 50.0));
            }
        }
        h
    };
    let l = lay_with(
        r#"<div id=c><img id=i><img id=j></div>"#,
        "#c { display: flex; width: 150px; align-items: flex-start } #j { flex: 0 0 auto }",
        &host,
    );
    // i shrinks to its min-content size (its natural width), so both
    // overflow with their natural sizes.
    assert_eq!(l.xywh("i"), (0.0, 0.0, 100.0, 50.0));
    let l = lay_with(
        r#"<div id=c><img id=i></div>"#,
        "#c { display: flex; height: 80px }",
        &host,
    );
    // Stretched in the cross axis (default align-items).
    assert_eq!(l.xywh("i"), (0.0, 0.0, 100.0, 80.0));
    let l = lay_with(
        r#"<div id=c><img id=i></div>"#,
        "#c { display: flex; width: 400px; align-items: center } #i { flex: 0 0 200px }",
        &host,
    );
    assert_eq!(l.xywh("i").2, 200.0);
    assert_eq!(l.xywh("i").3, 100.0);
}

#[test]
fn percentages_resolve_against_the_container() {
    let l = lay(
        r#"<div id=c><div id=a><div id=inner></div></div></div>"#,
        "#c { display: flex; width: 400px; height: 100px } #a { flex: 0 0 25% } #inner { height: 50% }",
    );
    assert_eq!(l.xywh("a").2, 100.0);
    // The stretched item has a definite height (§9.8).
    assert_eq!(l.xywh("inner").3, 50.0);
}

#[test]
fn wikipedia_like_page_structure() {
    let html = r#"
<div class="page">
  <header id=header class="header">
    <div class="start"><button id=menu>M</button><a id=logo href="/">Wiki</a></div>
    <div id=search class="search"><input id=box></div>
    <nav id=tools class="tools"><a>Create account</a><a>Log in</a></nav>
  </header>
  <div class="container">
    <nav id=side class="side">Main page Contents</nav>
    <main id=main class="main"><h1 id=title>Title</h1><p>Body text goes here.</p></main>
  </div>
</div>"#;
    let css = "
.page { display: flex; flex-direction: column; min-height: 300px }
.header { display: flex; align-items: center; height: 50px; padding: 0 10px }
.start { display: flex; align-items: center }
.search { flex-grow: 1; margin: 0 10px }
.tools { display: flex; gap: 8px; margin-left: auto }
.container { display: flex; flex: 1 }
.side { flex: 0 0 120px }
.main { flex: 1; min-width: 0 }
h1 { margin: 0 }";
    let l = lay(html, css);
    let header = l.xywh("header");
    assert_eq!(header, (0.0, 0.0, 800.0, 50.0));
    let tools = l.xywh("tools");
    // The tools sit at the right edge: 14 + 6 characters plus the gap.
    assert!(approx(tools.0 + tools.2, 790.0), "{tools:?}");
    assert!(approx(tools.2, 20.0 * 8.0 + 8.0), "{tools:?}");
    let search = l.xywh("search");
    assert!(approx(search.0 + search.2 + 10.0, tools.0), "{search:?}");
    // Side and main share the rest of the column (min-height: 300px).
    let side = l.xywh("side");
    let main = l.xywh("main");
    assert_eq!(side.2, 120.0);
    assert_eq!(main.0, 120.0);
    assert_eq!(main.2, 680.0);
    assert_eq!(side.1, 50.0);
    assert_eq!(side.3, 250.0);
    assert_eq!(main.3, 250.0);
}

#[test]
fn pathological_values_stay_finite() {
    let l = lay(
        r#"<div id=c><div id=a></div><div id=b></div></div>"#,
        "#c { display: flex; width: 100px } #a { flex: 1e38 1e38 1e38px } #b { flex: 1e-38 0 0 }",
    );
    for id in ["a", "b", "c"] {
        let (x, y, w, h) = l.xywh(id);
        assert!(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite());
    }
    let l = lay(
        r#"<div id=c><div id=a></div></div>"#,
        "#c { display: flex; width: 100px; gap: 1e30px; padding: 1e30px } #a { margin: -1e30px }",
    );
    let (x, _, w, _) = l.xywh("a");
    assert!(x.is_finite() && w.is_finite());
}

#[test]
fn many_items_break_lines_at_the_cap() {
    let n = MAX_ITEMS_PER_LINE + 10;
    let mut html = String::from("<div id=c>");
    for _ in 0..n {
        html.push_str("<i></i>");
    }
    html.push_str("<i id=last></i></div>");
    let l = lay(&html, "#c { display: flex } i { width: 1px; height: 1px }");
    // The items past the cap go on a second line.
    assert_eq!(l.xywh("last").1, 1.0);
}

#[test]
fn deeply_nested_flex_is_polynomial() {
    let depth = 60;
    let mut html = String::new();
    for _ in 0..depth {
        html.push_str("<div class=f><div class=a>x</div>");
    }
    for _ in 0..depth {
        html.push_str("</div>");
    }
    let start = std::time::Instant::now();
    let l = lay(
        &html,
        ".f { display: flex; flex-direction: column } .f > .f { flex: 1 } .f:nth-child(odd) { flex-direction: row }",
    );
    assert!(l.tree.root().is_some());
    assert!(start.elapsed().as_secs() < 20);
}

#[test]
fn distribute_offsets() {
    assert_eq!(Distribute::SpaceBetween.offsets(100.0, 1), (0.0, 0.0));
    assert_eq!(Distribute::SpaceAround.offsets(100.0, 2), (25.0, 50.0));
    assert_eq!(Distribute::SpaceEvenly.offsets(-30.0, 2), (-15.0, 0.0));
    assert_eq!(Distribute::Center.offsets(f32::NAN, 2), (0.0, 0.0));
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.02
}

#[test]
fn nested_stretched_rows_are_polynomial() {
    // Every level is measured, then stretched: without the item layout
    // cache this is exponential in the nesting depth.
    let depth = 150;
    let mut html = String::new();
    for _ in 0..depth {
        html.push_str("<div class=f><div>x<br>x</div>");
    }
    for _ in 0..depth {
        html.push_str("</div>");
    }
    let start = std::time::Instant::now();
    let l = lay(
        &html,
        ".f { display: flex; padding-top: 1px } .f > .f { flex: 1 }",
    );
    assert!(l.tree.root().is_some());
    assert!(start.elapsed().as_secs() < 20);
}
