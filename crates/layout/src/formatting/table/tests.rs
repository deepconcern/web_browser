//! Table layout tests: real HTML styled by `wb-style`, laid out with the
//! fake text engine (each character 8px wide at 16px, lines 16px tall).

use wb_css::parse_stylesheet;
use wb_dom::{Document, NodeId};
use wb_style::{LineStyle, MediaContext, Rgba, SheetInput, StyleOptions, style_document};

use crate::context::layout_tree_with;
use crate::text::FakeText;
use crate::*;

struct Laid {
    doc: Document,
    boxes: BoxTree,
    tree: FragmentTree,
}

fn lay_w(html: &str, css: &str, width: f32) -> Laid {
    let doc = wb_html::parse_document(html, &Default::default()).document;
    let mut sheets = vec![SheetInput::author(parse_stylesheet("body { margin: 0 }"))];
    if !css.is_empty() {
        sheets.push(SheetInput::author(parse_stylesheet(css)));
    }
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
    Laid { doc, boxes, tree }
}

fn lay(html: &str, css: &str) -> Laid {
    lay_w(html, css, 800.0)
}

impl Laid {
    fn node(&self, id: &str) -> NodeId {
        self.doc.element_by_id(id).expect("element")
    }
    fn frag(&self, id: &str) -> &Fragment {
        let f = *self
            .tree
            .fragments_for_node(self.node(id))
            .first()
            .unwrap_or_else(|| panic!("no fragment for #{id}"));
        self.tree.get(f).expect("fragment")
    }
    fn rect(&self, id: &str) -> Rect {
        self.frag(id).border_box
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[track_caller]
fn assert_rect(r: Rect, x: f32, y: f32, w: f32, h: f32) {
    assert!(
        close(r.x, x) && close(r.y, y) && close(r.width, w) && close(r.height, h),
        "got {r:?}, want ({x}, {y}, {w}, {h})"
    );
}

#[test]
fn simple_grid_with_spacing_and_padding() {
    // UA: border-spacing 2px, td padding 1px.
    let l = lay(
        "<table id=t><tr><td id=a>aa<td id=b>bbbb</tr><tr><td id=c>c<td id=d>d</tr></table>",
        "",
    );
    // Columns: 2*8+2 = 18 and 4*8+2 = 34; rows 16+2 = 18.
    assert_rect(
        l.rect("t"),
        0.0,
        0.0,
        2.0 + 18.0 + 2.0 + 34.0 + 2.0,
        2.0 + 18.0 + 2.0 + 18.0 + 2.0,
    );
    assert_rect(l.rect("a"), 2.0, 2.0, 18.0, 18.0);
    assert_rect(l.rect("b"), 22.0, 2.0, 34.0, 18.0);
    assert_rect(l.rect("c"), 2.0, 22.0, 18.0, 18.0);
    assert_rect(l.rect("d"), 22.0, 22.0, 34.0, 18.0);
    // Content box of a cell: inside the padding.
    let a = l.frag("a");
    assert!(close(a.content_box.x, 3.0) && close(a.content_box.y, 3.0));
}

#[test]
fn row_and_group_fragments() {
    let l = lay(
        "<table id=t cellspacing=0><tbody id=g><tr id=r><td id=a>x</td></tr></tbody></table>",
        "td { padding: 0 }",
    );
    assert_rect(l.rect("g"), 0.0, 0.0, 8.0, 16.0);
    assert_rect(l.rect("r"), 0.0, 0.0, 8.0, 16.0);
    let r = l.frag("r");
    let cell = l.tree.get(r.children[0]).expect("cell");
    assert_eq!(cell.node, Some(l.node("a")));
}

#[test]
fn anonymous_table_objects() {
    // A cell inside a block gets an anonymous row and table (§17.2.1).
    let l = lay(
        "<div id=d><span id=c style='display:table-cell'>xx</span><span id=e style='display:table-cell'>yyy</span></div>",
        "",
    );
    let c = l.rect("c");
    let e = l.rect("e");
    assert!(close(c.y, e.y), "cells share a row: {c:?} {e:?}");
    assert!(close(e.x, c.max_x()), "{c:?} {e:?}");
    assert!(close(c.width, 16.0) && close(e.width, 24.0));
    // The box tree has an anonymous table and row around them.
    let cell_box = l
        .boxes
        .boxes()
        .iter()
        .position(|b| b.node == Some(l.node("c")) && b.pseudo.is_none())
        .expect("cell box");
    let row = l.boxes.boxes()[cell_box].parent.expect("row");
    let table = l.boxes.get(row).and_then(|b| b.parent).expect("table");
    assert!(l.boxes.get(row).expect("row").anonymous);
    assert_eq!(
        l.boxes.get(table).and_then(|b| b.formatting()),
        Some(Formatting::Table)
    );
}

#[test]
fn text_directly_in_a_table_gets_a_cell() {
    let l = lay("<div id=t style='display:table'>hello</div>", "");
    assert_rect(l.rect("t"), 0.0, 0.0, 40.0, 16.0);
    // Inter-element white space in tables makes no cells.
    let l = lay(
        "<div id=t style='display:table'>\n  <div id=r style='display:table-row'>\n <div id=c style='display:table-cell'>ab</div>\n </div>\n</div>",
        "",
    );
    assert_rect(l.rect("t"), 0.0, 0.0, 16.0, 16.0);
    assert_rect(l.rect("c"), 0.0, 0.0, 16.0, 16.0);
}

#[test]
fn rowspan_and_colspan_slots() {
    let l = lay(
        "<table cellspacing=0><tr><td id=a rowspan=2>a<td id=b>b<td id=x>x</tr>\
         <tr><td id=c>c<td id=d>d</tr>\
         <tr><td id=e colspan=3>eeeeeeeeeeeeee</tr></table>",
        "td { padding: 0 }",
    );
    // c skips the slot a covers.
    assert!(close(l.rect("c").x, l.rect("b").x));
    assert!(close(l.rect("d").x, l.rect("x").x));
    // a spans two rows.
    assert!(close(l.rect("a").height, 32.0));
    // e spans the three columns: 14 chars = 112px shared by 3 columns.
    let e = l.rect("e");
    assert!(close(e.x, 0.0) && close(e.width, 112.0), "{e:?}");
    assert!(close(l.rect("x").max_x(), 112.0));
}

#[test]
fn rowspan_zero_and_clipping_stay_in_the_group() {
    let l = lay(
        "<table cellspacing=0><tbody><tr><td id=a rowspan=0>a<td>b</tr><tr><td id=c>c</tr></tbody>\
         <tbody><tr><td id=d rowspan=9>d<td id=e>e</tr></tbody></table>",
        "td { padding: 0 }",
    );
    assert!(close(l.rect("a").height, 32.0));
    assert!(close(l.rect("c").x, 8.0));
    // d is clipped to its one-row group: e sits next to it.
    assert!(close(l.rect("d").height, 16.0));
    assert!(close(l.rect("e").x, 8.0));
}

#[test]
fn span_attributes_are_clamped() {
    let l = lay(
        "<table id=t cellspacing=0><tr><td colspan=5000 id=a>a</tr><tr><td>b</tr></table>",
        "td { padding: 0 }",
    );
    let a = l
        .boxes
        .boxes()
        .iter()
        .position(|b| b.node == Some(l.node("a")));
    let a = BoxId::from_index(a.expect("box")).expect("id");
    assert_eq!(l.boxes.table_spans.get(&a).map(|s| s.cols), Some(1000));
    assert!(close(l.rect("t").width, 8.0));
}

#[test]
fn header_and_footer_groups_move() {
    let l = lay(
        "<table cellspacing=0><tfoot><tr><td id=f>f</tr></tfoot><tbody><tr><td id=b>b</tr></tbody>\
         <thead><tr><td id=h>h</tr></thead></table>",
        "td { padding: 0 }",
    );
    assert!(close(l.rect("h").y, 0.0));
    assert!(close(l.rect("b").y, 16.0));
    assert!(close(l.rect("f").y, 32.0));
}

#[test]
fn percentage_widths() {
    let l = lay(
        "<table id=t width=50% cellspacing=0><tr><td id=a width=25%>a<td id=b>b</tr></table>",
        "td { padding: 0 }",
    );
    assert_rect(l.rect("t"), 0.0, 0.0, 400.0, 16.0);
    assert!(close(l.rect("a").width, 100.0), "{:?}", l.rect("a"));
    assert!(close(l.rect("b").width, 300.0));
}

#[test]
fn auto_width_shrinks_and_wraps_to_the_container() {
    // Max-content 100 chars (800px) in a 200px container: the column takes
    // the available width and the text wraps.
    let text = "word ".repeat(20);
    let l = lay_w(
        &format!("<table id=t cellspacing=0><tr><td id=a>{text}</tr></table>"),
        "td { padding: 0 }",
        200.0,
    );
    assert!(close(l.rect("t").width, 200.0));
    assert!(l.rect("a").height > 16.0);
}

#[test]
fn fixed_cell_width_and_auto_distribution() {
    // b is fixed at 100; the excess goes to the auto column.
    let l = lay(
        "<table width=400 cellspacing=0><tr><td id=a>a<td id=b width=100>b</tr></table>",
        "td { padding: 0 }",
    );
    assert!(close(l.rect("b").width, 100.0), "{:?}", l.rect("b"));
    assert!(close(l.rect("a").width, 300.0));
}

#[test]
fn nowrap_cells_do_not_wrap() {
    let l = lay_w(
        "<table id=t cellspacing=0><tr><td id=a nowrap>aaa bbb ccc</td><td id=b>x y z</td></tr></table>",
        "td { padding: 0 }",
        60.0,
    );
    // a keeps its whole line (88px) even though the viewport is 60px.
    assert!(close(l.rect("a").width, 88.0), "{:?}", l.rect("a"));
    // One line in a; b wraps to three, which sets the row height.
    assert_eq!(l.frag("a").children.len(), 1);
    assert!(close(l.rect("a").height, 48.0));
    assert!(l.rect("t").width >= 88.0 + 8.0);
}

#[test]
fn col_elements_and_fixed_layout() {
    let l = lay(
        "<table id=t style='table-layout:fixed;width:300px' cellspacing=0>\
         <col width=50><col><col>\
         <tr><td id=a>aaaaaaaaaaaaaaaaaaaaaa<td id=b>b<td id=c>c</tr></table>",
        "td { padding: 0 }",
    );
    // The first column is 50px whatever its content; the rest share 250.
    assert!(close(l.rect("a").width, 50.0));
    assert!(close(l.rect("b").width, 125.0));
    assert!(close(l.rect("c").x, 175.0));
    assert!(close(l.rect("t").width, 300.0));
}

#[test]
fn col_width_in_auto_layout() {
    let l = lay(
        "<table cellspacing=0><colgroup><col id=k width=80></colgroup><tr><td id=a>a<td>b</tr></table>",
        "td { padding: 0 }",
    );
    assert!(close(l.rect("a").width, 80.0));
    // The column has a fragment spanning the rows.
    assert_rect(l.rect("k"), 0.0, 0.0, 80.0, 16.0);
}

#[test]
fn table_height_grows_rows() {
    let l = lay(
        "<table height=100 cellspacing=0><tr><td id=a>a</tr><tr><td id=b>b</tr></table>",
        "td { padding: 0 }",
    );
    assert!(close(l.rect("a").height, 50.0));
    assert!(close(l.rect("b").y, 50.0));
}

#[test]
fn row_height_hint_and_vertical_align() {
    let l = lay(
        "<table cellspacing=0><tr height=60><td id=t valign=top>t<td id=m>m<td id=b valign=bottom>b\
         <td id=s style='vertical-align:baseline'>s</tr></table>",
        "td { padding: 0 }",
    );
    let line_y = |id: &str| {
        let f = l.frag(id);
        let line = l.tree.get(f.children[0]).expect("line");
        line.border_box.y
    };
    assert!(close(l.rect("t").height, 60.0));
    assert!(close(line_y("t"), 0.0));
    // Rows inherit `vertical-align: middle` from the UA row group.
    assert!(close(line_y("m"), 22.0));
    assert!(close(line_y("b"), 44.0));
    assert!(close(line_y("s"), 0.0));
}

#[test]
fn baseline_alignment_across_cells() {
    let l = lay(
        "<table cellspacing=0><tr style='vertical-align:baseline'><td id=a style='font-size:32px'>A<td id=b>b</tr></table>",
        "td { padding: 0 }",
    );
    let baseline = |id: &str| {
        let f = l.frag(id);
        match l.tree.get(f.children[0]).map(|x| &x.kind) {
            Some(FragmentKind::Line(line)) => line.baseline,
            other => panic!("{other:?}"),
        }
    };
    assert!(close(baseline("a"), baseline("b")));
    assert!(close(l.rect("b").height, 32.0));
}

#[test]
fn captions_top_and_bottom() {
    let l = lay(
        "<table id=t cellspacing=0><caption id=c1>top</caption><caption id=c2 style='caption-side:bottom'>bot</caption>\
         <tr><td id=a>aaaaaaaa</tr></table>",
        "td { padding: 0 }",
    );
    assert_rect(l.rect("c1"), 0.0, 0.0, 64.0, 16.0);
    assert_rect(l.rect("t"), 0.0, 16.0, 64.0, 16.0);
    assert_rect(l.rect("c2"), 0.0, 32.0, 64.0, 16.0);
    // The wrapper is anonymous and holds all three.
    let t = l.frag("t");
    let wrapper = l.tree.get(t.parent.expect("parent")).expect("wrapper");
    assert_eq!(wrapper.node, None);
    assert_eq!(wrapper.children.len(), 3);
}

#[test]
fn margins_go_on_the_wrapper_and_auto_margins_centre() {
    let l = lay(
        "<table id=t align=center cellspacing=0><tr><td>aaaaaaaaaa</tr></table>",
        "td { padding: 0 }",
    );
    assert_rect(l.rect("t"), 360.0, 0.0, 80.0, 16.0);
}

#[test]
fn separated_borders_and_empty_cells() {
    let l = lay(
        "<table id=t border=1 cellspacing=2><tr><td id=a>a<td id=e style='empty-cells:hide;background:red'></tr></table>",
        "",
    );
    // table border 1, spacing 2, cell border 1 + padding 1.
    let a = l.rect("a");
    assert!(close(a.x, 3.0) && close(a.width, 12.0), "{a:?}");
    let e = l.frag("e");
    assert_eq!(e.style.background_color, Rgba::TRANSPARENT);
    assert_eq!(e.style.border.top.style, LineStyle::None);
    // Rows draw no borders.
    let row = l.tree.get(e.parent.expect("row")).expect("row");
    assert!(!row.style.border.top.is_visible());
}

#[test]
fn collapsed_borders_are_resolved_onto_cells() {
    let l = lay(
        "<table id=t style='border-collapse:collapse;border:4px solid rgb(1,0,0)'>\
         <tr><td id=a>a<td id=b style='border-left:6px dashed rgb(2,0,0)'>b</tr>\
         <tr><td id=c>c<td id=d>d</tr></table>",
        "td { border: 2px solid rgb(3,0,0); padding: 0 }",
    );
    let t = l.frag("t");
    // The table border box holds half the outer borders: 2px each side.
    assert!(close(t.style.border.left.width, 2.0));
    assert_eq!(t.style.border.left.color, Rgba::rgb(1, 0, 0));
    let a = l.frag("a");
    // a: left edge is the table's 4px (half: 2), right edge the 6px dashed
    // (half: 3), top 4px table, bottom 2px cell.
    assert!(close(a.style.border.left.width, 2.0));
    assert!(close(a.style.border.right.width, 3.0));
    assert_eq!(a.style.border.right.style, LineStyle::Dashed);
    assert_eq!(a.style.border.right.color, Rgba::rgb(2, 0, 0));
    assert!(close(a.style.border.top.width, 2.0));
    assert!(close(a.style.border.bottom.width, 1.0));
    // Geometry: content 8 wide plus halves 2 + 3.
    assert_rect(a.border_box, 2.0, 2.0, 13.0, 2.0 + 16.0 + 1.0);
    let b = l.frag("b");
    assert!(close(b.style.border.left.width, 3.0));
    assert!(close(b.border_box.x, a.border_box.max_x()));
    // Row 2 starts on the grid line.
    assert!(close(l.rect("c").y, a.border_box.max_y()));
    // The table: grid 13 + (3+8+2) = 26, plus 2 + 2.
    assert!(close(t.border_box.width, 30.0), "{:?}", t.border_box);
}

#[test]
fn hidden_collapsed_border_wins() {
    let l = lay(
        "<table style='border-collapse:collapse'><tr><td id=a style='border-right:hidden'>a<td id=b>b</tr></table>",
        "td { border: 5px solid black; padding: 0 }",
    );
    assert!(close(l.frag("a").style.border.right.width, 0.0));
    assert!(close(l.frag("b").style.border.left.width, 0.0));
    assert!(close(l.frag("b").style.border.right.width, 2.5));
}

#[test]
fn hacker_news_like_nested_tables() {
    let html = r##"<center><table id=main border=0 cellpadding=0 cellspacing=0 width="85%" bgcolor="#f6f6ef">
      <tr><td bgcolor="#ff6600"><table id=head border=0 cellpadding=0 cellspacing=0 width="100%" style="padding:2px">
        <tr><td id=logo style="width:18px;padding-right:4px">L</td>
            <td id=links style="line-height:12pt; height:10px;"><span>Hacker News</span> | new | past</td>
            <td id=login style="text-align:right;padding-right:4px;" nowrap>login</td></tr></table></td></tr>
      <tr id=spacer style="height:10px"></tr>
      <tr><td><table id=items border=0 cellpadding=0 cellspacing=0>
        <tr class=athing><td align=right valign=top class=title id=rank>1.</td>
            <td valign=top class=votelinks><center>^</center></td>
            <td class=title id=title>A story title that is fairly long</td></tr>
        <tr><td colspan=2></td><td class=subtext id=sub>100 points by someone 2 hours ago</td></tr>
      </table></td></tr></table></center>"##;
    let l = lay(html, "");
    let main = l.rect("main");
    assert!(close(main.width, 680.0), "{main:?}");
    let head = l.rect("head");
    assert!(close(head.width, 680.0), "{head:?}");
    assert!(close(head.x, main.x));
    // The nowrap login cell keeps its word on one line.
    assert!(close(l.rect("login").height, l.rect("links").height));
    // The logo column has its fixed width (18 + 4 padding).
    assert!(close(l.rect("logo").width, 22.0), "{:?}", l.rect("logo"));
    // Item rows: the title cell follows rank and vote cells; the subtext
    // sits under the title in the same column.
    assert!(close(l.rect("sub").x, l.rect("title").x));
    assert!(l.rect("sub").y >= l.rect("title").max_y());
    assert!(l.rect("rank").max_x() <= l.rect("title").x);
    // The cell backgrounds come through the style.
    let main_style = &l.frag("main").style;
    assert_eq!(main_style.background_color, Rgba::rgb(0xf6, 0xf6, 0xef));
    // The spacer row is 10px tall.
    assert!(close(l.rect("spacer").height, 10.0));
}

#[test]
fn nested_table_contributes_to_outer_columns() {
    let l = lay(
        "<table id=o cellspacing=0><tr><td id=a><table cellspacing=0><tr><td>aaaa<td>bbbb</tr></table></td><td id=b>b</tr></table>",
        "td { padding: 0 }",
    );
    assert!(close(l.rect("a").width, 64.0), "{:?}", l.rect("a"));
    assert!(close(l.rect("o").width, 72.0));
}

#[test]
fn inline_table_sits_on_the_line() {
    let l = lay(
        "<div id=d>x<table id=t style='display:inline-table' cellspacing=0><tr><td>yy</tr></table>z</div>",
        "td { padding: 0 }",
    );
    let t = l.rect("t");
    assert!(close(t.x, 8.0) && close(t.width, 16.0), "{t:?}");
    assert!(close(l.rect("d").height, 16.0));
}

#[test]
fn empty_table() {
    let l = lay("<table id=t></table>", "");
    assert_rect(l.rect("t"), 0.0, 0.0, 0.0, 0.0);
    let l = lay("<table id=t border=2 cellpadding=3><tr></tr></table>", "");
    assert!(l.rect("t").width >= 4.0);
}

#[test]
fn hidden_rows_collapse() {
    let l = lay(
        "<table cellspacing=0><tr><td>a</tr><tr style='visibility:collapse'><td id=h>h</tr><tr><td id=c>c</tr></table>",
        "td { padding: 0 }",
    );
    assert!(close(l.rect("h").height, 0.0));
    assert!(close(l.rect("c").y, 16.0));
}

#[test]
fn many_columns_are_capped() {
    let cells = "<td>x".repeat(1500);
    let l = lay(
        &format!("<table id=t cellspacing=0><tr>{cells}</tr></table>"),
        "td { padding: 0 }",
    );
    assert!(close(l.rect("t").width, 8000.0));
    assert!(l.tree.truncated());
}

#[test]
fn large_tables_stay_linear() {
    // 3000 rows × 4 columns with spans: must lay out quickly.
    let mut html = String::from("<table id=t cellspacing=0>");
    for i in 0..3000 {
        if i % 10 == 0 {
            html.push_str("<tr><td rowspan=10>r<td colspan=2>cc<td>d</tr>");
        } else {
            html.push_str("<tr><td>a<td>b<td>c</tr>");
        }
    }
    html.push_str("</table>");
    let l = lay(
        &html,
        "td { padding: 0; border: 1px solid } table { border-collapse: collapse }",
    );
    assert!(l.rect("t").height > 3000.0 * 16.0);
    assert!(!l.tree.truncated());
}

#[test]
fn deep_nesting_is_bounded() {
    let depth = 60;
    let mut html = String::new();
    for _ in 0..depth {
        html.push_str("<table><tr><td>");
    }
    html.push('x');
    for _ in 0..depth {
        html.push_str("</td></tr></table>");
    }
    let l = lay(&html, "");
    assert!(l.tree.truncated());
}

#[test]
fn stray_columns_and_captions_do_not_panic() {
    let l = lay(
        "<div><span style='display:table-column'>x</span><span style='display:table-caption' id=c>cap</span>\
         <span style='display:table-column-group'><b>no</b></span>\
         <span style='display:table-row-group'>text</span></div>",
        "",
    );
    assert!(l.rect("c").width > 0.0);
}

#[test]
fn positioned_table_is_a_containing_block() {
    let l = lay(
        "<table id=t style='position:relative;margin:10px' cellspacing=0><tr><td>x\
         <div id=a style='position:absolute;top:5px;left:7px'>a</div></td></tr></table>",
        "td { padding: 0 }",
    );
    let t = l.rect("t");
    assert_rect(l.rect("a"), t.x + 7.0, t.y + 5.0, 8.0, 16.0);
    // The absolutely positioned box does not widen the cell.
    assert!(close(t.width, 8.0));
}

#[test]
fn positioned_cell_is_a_containing_block() {
    let l = lay(
        "<table cellspacing=0><tr><td>aaaa<td id=c style='position:relative'>b\
         <span id=a style='position:absolute;left:3px;top:4px'>z</span></tr></table>",
        "td { padding: 0 }",
    );
    let c = l.rect("c");
    assert_rect(l.rect("a"), c.x + 3.0, c.y + 4.0, 8.0, 16.0);
}

#[test]
fn absolute_children_of_tables_and_rows_are_laid_out() {
    let l = lay(
        "<div id=t style='display:table;position:relative'>\
           <div id=a1 style='position:absolute;left:1px;top:2px'>p</div>\
           <div id=r style='display:table-row'>\
             <div style='display:table-cell'>xx</div>\
             <div id=a2 style='position:absolute;right:0;bottom:0'>q</div>\
           </div></div>",
        "",
    );
    let t = l.rect("t");
    // Not wrapped into cells: the table only has its one 16px column.
    assert!(close(t.width, 16.0), "{t:?}");
    assert_rect(l.rect("a1"), t.x + 1.0, t.y + 2.0, 8.0, 16.0);
    assert_rect(l.rect("a2"), t.max_x() - 8.0, t.max_y() - 16.0, 8.0, 16.0);
}

#[test]
fn cells_contain_their_floats() {
    let l = lay(
        "<table cellspacing=0><tr><td id=c width=100><div id=f style='float:left;width:20px;height:50px'></div>x</td>\
         <td id=d>y</td></tr></table>",
        "td { padding: 0 }",
    );
    // A cell is a BFC root: its auto height includes its float.
    assert!(close(l.rect("c").height, 50.0), "{:?}", l.rect("c"));
    let f = l.rect("f");
    assert_rect(f, l.rect("c").x, l.rect("c").y, 20.0, 50.0);
    // The row is as tall as the float.
    assert!(close(l.rect("d").height, 50.0));
}

#[test]
fn flex_item_table_takes_the_imposed_height() {
    let l = lay(
        "<div style='display:flex;height:100px'><table id=t cellspacing=0><tr><td id=a>a</tr>\
         <tr><td id=b>b</tr></table></div>",
        "td { padding: 0 }",
    );
    // align-items: stretch imposes 100px; the rows share the extra.
    assert!(close(l.rect("t").height, 100.0), "{:?}", l.rect("t"));
    assert!(close(l.rect("a").height, 50.0));
    assert!(close(l.rect("b").y, 50.0));
}
