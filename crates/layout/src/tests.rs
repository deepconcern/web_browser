//! Layout tests: real HTML styled by `wb-style`, laid out with a
//! deterministic text engine (every character is 0.5em wide; ascent 0.8em,
//! descent 0.2em, so `line-height: normal` is 1em: 16px at the default
//! size, and each character is 8px).

use std::collections::HashMap;

use wb_css::parse_stylesheet;
use wb_dom::{Document, NodeId};
use wb_style::{MediaContext, SheetInput, StyleOptions, style_document};

use crate::context::layout_tree_with;
use crate::text::FakeText;
use crate::*;

struct Laid {
    doc: Document,
    tree: FragmentTree,
    boxes: BoxTree,
}

#[derive(Default)]
struct Host {
    images: HashMap<NodeId, Size>,
    scroll: HashMap<NodeId, Point>,
}

impl LayoutHost for Host {
    fn image_size(&self, node: NodeId) -> Option<Size> {
        self.images.get(&node).copied()
    }
    fn scroll_offset(&self, node: NodeId) -> Point {
        self.scroll.get(&node).copied().unwrap_or(Point::ZERO)
    }
}

fn styled(html: &str, css: &str) -> (Document, wb_style::StyledDocument) {
    let doc = wb_html::parse_document(html, &Default::default()).document;
    let sheets = if css.is_empty() {
        Vec::new()
    } else {
        vec![SheetInput::author(parse_stylesheet(css))]
    };
    let media = MediaContext {
        viewport_width: 800.0,
        viewport_height: 600.0,
        ..MediaContext::default()
    };
    let styles = style_document(&doc, &sheets, &media, &StyleOptions::default());
    (doc, styles)
}

fn lay_with(html: &str, css: &str, width: f32, host: &dyn Fn(&Document) -> Host) -> Laid {
    let (doc, styles) = styled(html, css);
    let boxes = build_box_tree(&doc, &styles);
    let h = host(&doc);
    let tree = layout_tree_with(
        &boxes,
        &FakeText,
        &h,
        &LayoutOptions {
            viewport: Size::new(width, 600.0),
        },
    );
    Laid { doc, tree, boxes }
}

fn lay(html: &str, css: &str) -> Laid {
    lay_with(html, css, 800.0, &|_| Host::default())
}

const RESET: &str = "body { margin: 0 } p { margin: 0 }";

impl Laid {
    fn node(&self, id: &str) -> NodeId {
        self.doc.element_by_id(id).expect("element")
    }
    /// The first fragment of element `#id`.
    fn frag(&self, id: &str) -> &Fragment {
        let n = self.node(id);
        let f = *self.tree.fragments_for_node(n).first().expect("fragment");
        self.tree.get(f).expect("fragment")
    }
    fn frags(&self, id: &str) -> Vec<&Fragment> {
        let n = self.node(id);
        self.tree
            .fragments_for_node(n)
            .into_iter()
            .filter_map(|f| self.tree.get(f))
            .collect()
    }
    fn rect(&self, id: &str) -> Rect {
        self.frag(id).border_box
    }
    /// Line boxes inside `#id`'s first fragment.
    fn lines(&self, id: &str) -> Vec<&Fragment> {
        let n = self.node(id);
        let f = *self.tree.fragments_for_node(n).first().expect("fragment");
        self.tree
            .children(f)
            .iter()
            .filter_map(|c| self.tree.get(*c))
            .filter(|c| matches!(c.kind, FragmentKind::Line(_)))
            .collect()
    }
    /// The rendered text of every text fragment under `f`, in order.
    fn texts_under(&self, f: FragmentId) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![f];
        while let Some(id) = stack.pop() {
            let Some(fr) = self.tree.get(id) else {
                continue;
            };
            if let FragmentKind::Text(t) = &fr.kind {
                out.push(t.text[t.range.clone()].to_string());
            }
            stack.extend(fr.children.iter().rev().copied());
        }
        out
    }
    fn line_texts(&self, id: &str) -> Vec<String> {
        let n = self.node(id);
        let f = *self.tree.fragments_for_node(n).first().expect("fragment");
        self.tree
            .children(f)
            .iter()
            .filter(|c| {
                matches!(
                    self.tree.get(**c).map(|x| &x.kind),
                    Some(FragmentKind::Line(_))
                )
            })
            .map(|c| self.texts_under(*c).concat())
            .collect()
    }
}

fn approx(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

macro_rules! assert_approx {
    ($a:expr, $b:expr) => {
        assert!(approx($a, $b), "{} != {}", $a, $b)
    };
}

// ---------------------------------------------------------------------
// Block layout
// ---------------------------------------------------------------------

#[test]
fn block_fills_width_inside_body_margin() {
    let l = lay("<div id=a style='height:50px'></div>", "");
    let r = l.rect("a");
    assert_eq!(r, Rect::new(8.0, 8.0, 784.0, 50.0));
    let root = l.tree.get(l.tree.root().unwrap()).unwrap();
    assert!(root.stacking.creates_stacking_context);
    assert_eq!(root.border_box.width, 800.0);
}

#[test]
fn sibling_margins_collapse() {
    let l = lay(
        "<div id=a style='height:10px;margin-bottom:20px'></div>\
         <div id=b style='height:10px;margin-top:30px'></div>\
         <div id=c style='height:10px;margin-top:-5px'></div>",
        RESET,
    );
    assert_approx!(l.rect("b").y, l.rect("a").max_y() + 30.0);
    assert_approx!(l.rect("c").y, l.rect("b").max_y() - 5.0);
}

#[test]
fn positive_and_negative_margins_sum() {
    let l = lay(
        "<div id=a style='height:10px;margin-bottom:20px'></div>\
         <div id=b style='height:10px;margin-top:-5px'></div>",
        RESET,
    );
    assert_approx!(l.rect("b").y, 25.0);
}

#[test]
fn parent_and_first_child_margins_collapse() {
    let l = lay(
        "<div id=p style='margin-top:10px'><div id=c style='margin-top:20px;height:5px'></div></div>",
        RESET,
    );
    assert_approx!(l.rect("p").y, 20.0);
    assert_approx!(l.rect("c").y, 20.0);
    assert_approx!(l.rect("p").height, 5.0);
}

#[test]
fn padding_and_bfc_prevent_collapse() {
    let l = lay(
        "<div id=p style='margin-top:10px;padding-top:1px'><div id=c style='margin-top:20px;height:5px'></div></div>\
         <div id=q style='margin-top:10px;overflow:hidden'><div id=d style='margin-top:20px;height:5px'></div></div>",
        RESET,
    );
    assert_approx!(l.rect("p").y, 10.0);
    assert_approx!(l.rect("c").y, 31.0);
    assert_approx!(l.rect("p").height, 26.0);
    let q = l.rect("q");
    assert_approx!(l.rect("d").y, q.y + 20.0);
    assert_approx!(q.height, 25.0);
}

#[test]
fn bottom_margins_collapse_with_auto_height_parent() {
    let l = lay(
        "<div id=p><div style='height:5px;margin-bottom:30px'></div></div><div id=n style='height:1px;margin-top:10px'></div>",
        RESET,
    );
    assert_approx!(l.rect("p").height, 5.0);
    assert_approx!(l.rect("n").y, 35.0);
}

#[test]
fn empty_block_margins_collapse_through() {
    let l = lay(
        "<div id=a style='height:10px'></div><div id=e style='margin:15px 0 25px'></div><div id=b style='height:10px;margin-top:5px'></div>",
        RESET,
    );
    assert_approx!(l.rect("b").y, 35.0);
    assert_approx!(l.rect("e").height, 0.0);
}

#[test]
fn auto_margins_center_and_max_width() {
    let l = lay(
        "<div id=a style='width:200px;margin:0 auto;height:1px'></div>\
         <div id=b style='max-width:400px;margin:0 auto;height:1px'></div>\
         <div id=c style='width:300px;margin-left:auto;height:1px'></div>",
        RESET,
    );
    assert_eq!(l.rect("a").x, 300.0);
    assert_eq!(l.rect("b").x, 200.0);
    assert_eq!(l.rect("b").width, 400.0);
    assert_eq!(l.rect("c").x, 500.0);
}

#[test]
fn box_sizing_and_percentages() {
    let l = lay(
        "<div style='width:400px'><div id=a style='width:50%;padding:10px;border:5px solid;height:10px'></div>\
         <div id=b style='box-sizing:border-box;width:50%;padding:10px;border:5px solid'></div></div>",
        RESET,
    );
    assert_eq!(l.rect("a").width, 230.0);
    assert_eq!(l.rect("a").height, 40.0);
    let a = l.frag("a");
    assert_eq!(a.content_box.width, 200.0);
    assert_eq!(a.content_box.x, a.border_box.x + 15.0);
    assert_eq!(l.rect("b").width, 200.0);
}

#[test]
fn min_and_max_height() {
    let l = lay(
        "<div id=a style='min-height:50px'></div><div id=b style='max-height:20px'><div style='height:100px'></div></div>\
         <div style='height:200px'><div id=c style='height:50%'></div></div>\
         <div><div id=d style='height:50%'>x</div></div>",
        RESET,
    );
    assert_eq!(l.rect("a").height, 50.0);
    assert_eq!(l.rect("b").height, 20.0);
    assert_eq!(l.rect("c").height, 100.0);
    // Percentage of an auto-height containing block behaves as auto.
    assert_eq!(l.rect("d").height, 16.0);
}

#[test]
fn display_none_and_contents() {
    let l = lay(
        "<div id=n style='display:none'><p id=inner>x</p></div>\
         <div id=c style='display:contents'><p id=k style='height:7px'></p></div>",
        RESET,
    );
    assert!(l.tree.fragments_for_node(l.node("n")).is_empty());
    assert!(l.tree.fragments_for_node(l.node("inner")).is_empty());
    assert!(l.tree.fragments_for_node(l.node("c")).is_empty());
    assert_eq!(l.rect("k").height, 7.0);
}

#[test]
fn anonymous_blocks_wrap_inline_runs() {
    let l = lay("<div id=d>before<p id=p>para</p>after</div>", RESET);
    let d = l.frag("d");
    // anonymous block, p, anonymous block
    assert_eq!(d.children.len(), 3);
    let first = l.tree.get(d.children[0]).unwrap();
    assert!(first.node.is_none());
    assert_eq!(first.border_box.height, 16.0);
    assert_eq!(l.rect("p").y, 16.0);
    assert_eq!(d.border_box.height, 48.0);
    // White space between blocks makes no anonymous box.
    let l = lay("<div id=d>\n  <p>a</p>\n  <p>b</p>\n</div>", RESET);
    assert_eq!(l.frag("d").children.len(), 2);
}

#[test]
fn block_inside_inline_splits_the_inline() {
    let l = lay(
        "<div id=d><span id=s style='padding:0 4px'>a<p id=p>b</p>c</span></div>",
        RESET,
    );
    let pieces = l.frags("s");
    assert_eq!(pieces.len(), 2);
    // The first piece has the start padding only, the last the end padding.
    assert_eq!(pieces[0].padding_box.x - pieces[0].content_box.x, -4.0);
    assert_eq!(pieces[0].content_box.max_x(), pieces[0].border_box.max_x());
    assert_eq!(pieces[1].content_box.x, pieces[1].border_box.x);
    assert_eq!(l.rect("p").y, 16.0);
    assert_eq!(l.frag("d").border_box.height, 48.0);
}

// ---------------------------------------------------------------------
// Inline layout
// ---------------------------------------------------------------------

#[test]
fn text_wraps_at_spaces() {
    let l = lay("<div id=d style='width:80px'>aaaa bbbb cccc</div>", RESET);
    assert_eq!(l.line_texts("d"), vec!["aaaa bbbb", "cccc"]);
    assert_eq!(l.rect("d").height, 32.0);
    let lines = l.lines("d");
    assert_eq!(lines[1].border_box.y, 16.0);
}

#[test]
fn white_space_collapses_across_elements() {
    let l = lay("<div id=d>  a   <b> b</b>\n c  </div>", RESET);
    assert_eq!(l.line_texts("d"), vec!["a b c"]);
}

#[test]
fn pre_keeps_spaces_and_newlines() {
    let l = lay(
        "<pre id=d style='margin:0'>a  b\n\nc</pre><div id=n style='width:20px;white-space:nowrap'>aaa bbb ccc</div>",
        RESET,
    );
    assert_eq!(l.line_texts("d"), vec!["a  b", "", "c"]);
    assert_eq!(l.rect("d").height, 48.0);
    assert_eq!(l.line_texts("n"), vec!["aaa bbb ccc"]);
}

#[test]
fn pre_line_and_pre_wrap() {
    let l = lay(
        "<div id=a style='white-space:pre-line'>a   b\nc</div>\
         <div id=b style='white-space:pre-wrap;width:48px'>aa  bb  cc</div>",
        RESET,
    );
    assert_eq!(l.line_texts("a"), vec!["a b", "c"]);
    assert_eq!(l.line_texts("b"), vec!["aa  bb", "cc"]);
}

#[test]
fn tabs_advance_to_tab_stops() {
    let l = lay("<pre id=d style='margin:0'>a\tb</pre>", RESET);
    let texts: Vec<&Fragment> = l
        .tree
        .fragments()
        .iter()
        .filter(|f| matches!(f.kind, FragmentKind::Text(_)))
        .collect();
    assert_eq!(texts.len(), 2);
    // Space is 8px wide; the tab stop is at 64px.
    assert_eq!(texts[1].border_box.x, 64.0);
}

#[test]
fn br_forces_breaks_and_empty_lines_have_height() {
    let l = lay("<div id=d>a<br>b<br><br>c</div><div id=e><br></div>", RESET);
    assert_eq!(l.line_texts("d"), vec!["a", "b", "", "c"]);
    assert_eq!(l.rect("d").height, 64.0);
    // middle: the span's midpoint (2.4px below its baseline at 16px) sits
    // 4px (half the parent's x-height) above the parent's baseline, so it
    // drops 0.8px and its descent extends the line.
    assert_eq!(l.rect("e").height, 16.0);
}

#[test]
fn whitespace_only_block_is_empty() {
    let l = lay(
        "<div id=d>   </div><div id=e style='height:1px'></div>",
        RESET,
    );
    assert_eq!(l.rect("d").height, 0.0);
}

#[test]
fn text_align_and_indent() {
    let l = lay(
        "<div id=c style='text-align:center;width:100px'>abcd</div>\
         <div id=r style='text-align:right;width:100px'>abcd</div>\
         <div id=i style='text-indent:20px;width:100px'>abcd</div>",
        RESET,
    );
    let tx = |id: &str| {
        let line = l.lines(id)[0];
        l.tree.get(line.children[0]).unwrap().border_box.x - line.border_box.x
    };
    assert_eq!(tx("c"), 34.0);
    assert_eq!(tx("r"), 68.0);
    assert_eq!(tx("i"), 20.0);
}

#[test]
fn justify_stretches_all_but_last_line() {
    let l = lay(
        "<div id=d style='text-align:justify;width:100px'>aa bb cc dd ee ff</div>",
        RESET,
    );
    let lines = l.lines("d");
    assert_eq!(lines.len(), 2);
    let first = l.tree.get(lines[0].children[0]).unwrap();
    // The first line fills the width.
    assert_approx!(first.border_box.max_x() - lines[0].border_box.x, 100.0);
    let last = l.tree.get(lines[1].children[0]).unwrap();
    assert!(last.border_box.width < 100.0);
}

#[test]
fn overflow_wrap_breaks_long_words() {
    let l = lay(
        "<div id=a style='width:40px'>abcdefghij</div>\
         <div id=b style='width:40px;overflow-wrap:anywhere'>abcdefghij</div>\
         <div id=c style='width:40px;word-break:break-all'>abcdefghij</div>",
        RESET,
    );
    assert_eq!(l.line_texts("a"), vec!["abcdefghij"]);
    assert_eq!(l.line_texts("b"), vec!["abcde", "fghij"]);
    assert_eq!(l.line_texts("c"), vec!["abcde", "fghij"]);
}

#[test]
fn text_transform() {
    let l = lay(
        "<div id=u style='text-transform:uppercase'>straße</div>\
         <div id=c style='text-transform:capitalize'>hello big-world</div>",
        RESET,
    );
    assert_eq!(l.line_texts("u"), vec!["STRASSE"]);
    assert_eq!(l.line_texts("c"), vec!["Hello Big-World"]);
}

#[test]
fn text_fragments_map_back_to_source() {
    let l = lay("<div id=d style='width:40px'>  aaa   bbb</div>", RESET);
    let text = l.doc.children(l.node("d")).next().unwrap();
    let frags: Vec<&Fragment> = l
        .tree
        .fragments_for_node(text)
        .into_iter()
        .map(|f| l.tree.get(f).unwrap())
        .collect();
    assert_eq!(frags.len(), 2);
    let FragmentKind::Text(t) = &frags[1].kind else {
        panic!()
    };
    assert_eq!(&t.text[t.range.clone()], "bbb");
    assert_eq!(t.source_range, 8..11);
}

#[test]
fn inline_box_edges_and_letter_spacing() {
    let l = lay(
        "<div id=d>a<span id=s style='margin-left:10px;border-left:2px solid;padding-left:3px'>b</span>c\
         <span id=w style='letter-spacing:2px'>xy</span></div>",
        RESET,
    );
    let s = l.rect("s");
    assert_eq!(s.x, 18.0);
    assert_eq!(s.width, 13.0);
    // "c " = 16px after the span; "xy" with 2px letter-spacing = 20px.
    assert_eq!(l.rect("w").width, 20.0);
}

#[test]
fn line_height_and_vertical_align() {
    let l = lay(
        "<div id=a style='line-height:30px'>x</div>\
         <div id=b>x<span id=big style='font-size:32px'>Y</span></div>\
         <div id=c>x<span id=sup style='vertical-align:10px'>y</span></div>",
        RESET,
    );
    assert_eq!(l.rect("a").height, 30.0);
    // The 32px span: ascent 25.6 above the shared baseline; the root strut
    // has 3.2px descent, the span 6.4px.
    assert_approx!(l.rect("b").height, 32.0);
    let line = l.lines("b")[0];
    let FragmentKind::Line(lf) = line.kind else {
        panic!()
    };
    assert_approx!(lf.baseline - line.border_box.y, 25.6);
    assert_approx!(l.rect("c").height, 26.0);
    assert_approx!(l.rect("sup").y, l.rect("c").y);
}

#[test]
fn vertical_align_middle_top_bottom() {
    let l = lay(
        "<div id=d>x<span id=t style='vertical-align:top;line-height:40px'>t</span></div>\
         <div id=e>x<span id=m style='vertical-align:middle'>m</span></div>",
        RESET,
    );
    assert_eq!(l.rect("d").height, 40.0);
    // top-aligned: its 40px layout box sits at the line top; content area
    // (16px) is centred in it.
    assert_approx!(l.rect("t").y - l.rect("d").y, 12.0);
    // middle: the span's midpoint (2.4px below its baseline at 16px) sits
    // 4px (half the parent's x-height) above the parent's baseline, so it
    // drops 0.8px and its descent extends the line.
    assert_approx!(l.rect("e").height, 16.8);
}

#[test]
fn inline_block_shrinks_to_fit_and_sits_on_baseline() {
    let l = lay(
        "<div id=d>ab <span id=ib style='display:inline-block'>xyz<br>w</span> c</div>\
         <div id=e style='width:50px'><span id=ib2 style='display:inline-block'>aaaa bbbb cccc</span></div>",
        RESET,
    );
    let ib = l.rect("ib");
    assert_eq!(ib.width, 24.0);
    assert_eq!(ib.height, 32.0);
    assert_eq!(ib.x, 24.0);
    // Its last line's baseline (12.8 + 16) is on the line's baseline.
    assert_approx!(l.rect("d").height, 32.0);
    // Shrink-to-fit limited by the available width.
    assert_eq!(l.rect("ib2").width, 50.0);
    assert_eq!(l.rect("ib2").height, 48.0);
}

#[test]
fn intrinsic_keywords_for_width() {
    let l = lay(
        "<div id=a style='width:min-content'>aaa bb</div><div id=b style='width:max-content'>aaa bb</div>\
         <div id=c style='width:fit-content'>aaa bb</div>",
        RESET,
    );
    assert_eq!(l.rect("a").width, 24.0);
    assert_eq!(l.rect("b").width, 48.0);
    assert_eq!(l.rect("c").width, 48.0);
}

// ---------------------------------------------------------------------
// Replaced elements and form controls
// ---------------------------------------------------------------------

#[test]
fn images_use_natural_size_and_ratio() {
    let host = |doc: &Document| {
        let mut h = Host::default();
        for id in ["a", "b", "c", "d"] {
            h.images
                .insert(doc.element_by_id(id).unwrap(), Size::new(200.0, 100.0));
        }
        h
    };
    let l = lay_with(
        "<img id=a><img id=b style='width:50px'><img id=c style='max-width:100px'>\
         <div style='width:80px'><img id=d style='max-width:100%;display:block'></div><img id=e>",
        RESET,
        800.0,
        &host,
    );
    assert_eq!(l.rect("a").size(), Size::new(200.0, 100.0));
    assert_eq!(l.rect("b").size(), Size::new(50.0, 25.0));
    assert_eq!(l.rect("c").size(), Size::new(100.0, 50.0));
    assert_eq!(l.rect("d").size(), Size::new(80.0, 40.0));
    // Not loaded: 0x0.
    assert_eq!(l.rect("e").size(), Size::new(0.0, 0.0));
    assert!(matches!(
        l.frag("a").kind,
        FragmentKind::Replaced(ReplacedFragment {
            kind: ReplacedKind::Image
        })
    ));
    // Images sit on the baseline: bottom margin edge at the baseline.
    let line = l.tree.get(l.frag("a").parent.unwrap()).unwrap();
    let FragmentKind::Line(lf) = line.kind else {
        panic!()
    };
    assert_approx!(l.rect("a").max_y(), lf.baseline);
}

#[test]
fn form_controls_are_replaced_boxes() {
    let l = lay(
        "<input id=t value=hi><input id=c type=checkbox checked><input type=hidden id=h>\
         <select id=s><option>one</option><option selected>three</option></select>\
         <textarea id=ta cols=10 rows=3>x</textarea><input id=b type=submit>",
        "body{margin:0} input,select,textarea{padding:0;border:0;margin:0;font-size:16px}",
    );
    let t = l.frag("t");
    let FragmentKind::Replaced(r) = &t.kind else {
        panic!()
    };
    let ReplacedKind::FormControl(fc) = &r.kind else {
        panic!()
    };
    assert_eq!(fc.kind, FormControlKind::TextField);
    assert_eq!(fc.text, "hi");
    // 20 characters of 8px.
    assert_eq!(t.content_box.width, 160.0);
    let c = l.frag("c");
    assert_eq!(c.content_box.size(), Size::new(13.0, 13.0));
    let FragmentKind::Replaced(r) = &c.kind else {
        panic!()
    };
    assert!(matches!(&r.kind, ReplacedKind::FormControl(f) if f.checked));
    assert!(l.tree.fragments_for_node(l.node("h")).is_empty());
    let FragmentKind::Replaced(r) = &l.frag("s").kind else {
        panic!()
    };
    assert!(matches!(&r.kind, ReplacedKind::FormControl(f) if f.text == "three"));
    assert_eq!(l.frag("ta").content_box.size(), Size::new(80.0, 48.0));
    let FragmentKind::Replaced(r) = &l.frag("b").kind else {
        panic!()
    };
    assert!(matches!(&r.kind, ReplacedKind::FormControl(f) if f.text == "Submit"));
}

// ---------------------------------------------------------------------
// Lists, positioning, overflow
// ---------------------------------------------------------------------

#[test]
fn list_markers() {
    let l = lay(
        "<ol id=o start=3><li id=a>x</li><li id=b value=10>y</li><li id=c>z</li></ol>\
         <ol reversed><li id=r1>x</li><li id=r2>y</li></ol>\
         <ul><li id=u style='list-style-position:inside'>i</li></ul>",
        RESET,
    );
    let marker_text = |id: &str| {
        let f = l
            .frags(id)
            .into_iter()
            .find(|f| {
                matches!(f.kind, FragmentKind::Marker)
                    || f.pseudo == Some(wb_style::PseudoElement::Marker)
            })
            .expect("marker");
        let fid = l
            .tree
            .fragments_for_node(l.node(id))
            .into_iter()
            .find(|x| std::ptr::eq(l.tree.get(*x).unwrap(), f))
            .unwrap();
        (l.texts_under(fid).concat(), f.border_box)
    };
    assert_eq!(marker_text("a").0, "3. ");
    assert_eq!(marker_text("b").0, "10. ");
    assert_eq!(marker_text("c").0, "11. ");
    assert_eq!(marker_text("r1").0, "2. ");
    assert_eq!(marker_text("r2").0, "1. ");
    // Outside marker: ends at the content box start, on the first line.
    let (_, mb) = marker_text("a");
    let li = l.frag("a");
    assert_approx!(mb.max_x(), li.content_box.x);
    assert_approx!(mb.y, li.content_box.y);
    // Inside marker is part of the first line.
    let lines = l.line_texts("u");
    assert_eq!(lines, vec!["\u{2022} i"]);
}

#[test]
fn relative_positioning_moves_box_and_descendants() {
    let l = lay(
        "<div id=a style='position:relative;left:10px;top:5px;height:20px'><div id=c style='height:5px'></div></div>\
         <div id=b style='height:10px'></div>\
         <div id=r style='position:relative;right:10px;bottom:3px'>x<span id=s style='position:relative;top:2px'>y</span></div>",
        RESET,
    );
    assert_eq!(l.rect("a").origin(), Point::new(10.0, 5.0));
    assert_eq!(l.rect("c").origin(), Point::new(10.0, 5.0));
    assert_eq!(l.frag("a").relative_offset, Point::new(10.0, 5.0));
    // Siblings are not affected.
    assert_eq!(l.rect("b").y, 20.0);
    assert_eq!(l.rect("r").origin(), Point::new(-10.0, 27.0));
    assert_eq!(l.rect("s").y, 29.0);
    assert!(l.frag("a").stacking.positioned);
}

#[test]
fn overflow_clip_and_scroll_offsets() {
    let host = |doc: &Document| {
        let mut h = Host::default();
        h.scroll
            .insert(doc.element_by_id("s").unwrap(), Point::new(0.0, 500.0));
        h
    };
    let l = lay_with(
        "<div id=s style='height:50px;overflow:auto;padding:5px'><div id=c style='height:200px'></div></div>\
         <div id=v style='height:10px'><div id=big style='height:100px'></div></div>",
        RESET,
        800.0,
        &host,
    );
    let s = l.frag("s");
    let clip = s.clip.expect("clip");
    assert!(clip.x && clip.y && clip.scroll_container);
    assert_eq!(clip.rect, s.padding_box);
    // Scrollable overflow: padding 5 + content 200 + end padding 5 = 210,
    // minus the 60px padding box: the offset is clamped to 150.
    assert_eq!(s.scroll_offset, Point::new(0.0, 150.0));
    assert_eq!(l.rect("c").y, 5.0 - 150.0);
    assert!(l.frag("v").clip.is_none());
    // Visible overflow reaches the document overflow.
    assert!(l.tree.document_overflow().height >= 600.0);
}

#[test]
fn overflow_on_body_propagates_to_viewport() {
    let l = lay(
        "<body id=b style='overflow:hidden'><div style='height:5000px'></div></body>",
        "",
    );
    assert!(l.frag("b").clip.is_none());
    assert_eq!(l.boxes.viewport_overflow().1, wb_style::Overflow::Hidden);
}

#[test]
fn stacking_information() {
    let l = lay(
        "<div id=z style='position:relative;z-index:3'>a</div><div id=o style='opacity:0.5'>b</div><div id=n style='z-index:3'>c</div>",
        RESET,
    );
    let z = l.frag("z").stacking;
    assert_eq!(z.z_index, Some(3));
    assert!(z.creates_stacking_context && z.positioned);
    let o = l.frag("o").stacking;
    assert!(o.creates_stacking_context);
    assert_eq!(o.opacity, 0.5);
    // z-index on a static box does not apply.
    assert_eq!(l.frag("n").stacking.z_index, None);
}

#[test]
fn generated_content() {
    let l = lay(
        "<div id=d>b</div>",
        "body{margin:0} #d::before{content:'a'} #d::after{content:'c' attr(id)}",
    );
    assert_eq!(l.line_texts("d"), vec!["abcd"]);
}

// ---------------------------------------------------------------------
// Hit testing and the tree contract
// ---------------------------------------------------------------------

#[test]
fn hit_testing_finds_text_and_offset() {
    let l = lay(
        "<div id=d style='width:100px'>hello world</div><div id=e style='height:20px'></div>",
        RESET,
    );
    let hit = l.tree.hit_test(Point::new(17.0, 5.0)).expect("hit");
    let text = l.doc.children(l.node("d")).next().unwrap();
    assert_eq!(hit.node, Some(text));
    // Nearest caret position to x=17 is after "he" (16px).
    assert_eq!(hit.text_offset, Some(2));
    let hit = l.tree.hit_test(Point::new(50.0, 20.0)).expect("hit");
    assert_eq!(hit.node, Some(l.node("e")));
    // Clipped content cannot be hit outside its clip.
    let l = lay(
        "<div id=c style='height:10px;overflow:hidden'><div id=k style='height:50px'></div></div>",
        RESET,
    );
    let hit = l.tree.hit_test(Point::new(5.0, 30.0)).expect("hit");
    assert_ne!(hit.node, Some(l.node("k")));
}

#[test]
fn hit_testing_prefers_positioned_boxes() {
    let l = lay(
        "<div id=p style='position:relative;top:10px;height:20px;z-index:1'></div><div id=n style='height:20px'></div>",
        RESET,
    );
    let hit = l.tree.hit_test(Point::new(5.0, 25.0)).unwrap();
    assert_eq!(hit.node, Some(l.node("p")));
}

#[test]
fn fragment_tree_is_pre_order_with_parent_links() {
    let l = lay("<div><p>a <b>b</b></p><p>c</p></div>", "");
    for (i, f) in l.tree.fragments().iter().enumerate() {
        for c in &f.children {
            assert!(c.index() > i);
            assert_eq!(l.tree.get(*c).unwrap().parent.unwrap().index(), i);
        }
        // Children lie inside their parent's coordinate space after
        // finalisation (absolute positions): spot-check non-negative.
        assert!(f.border_box.width >= 0.0 && f.border_box.height >= 0.0);
    }
    assert_eq!(l.tree.fragments()[0].parent, None);
}

// ---------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------

#[test]
fn pathological_sizes_are_clamped() {
    let l = lay(
        "<div id=a style='width:1e30px;height:1e30px;margin-left:-1e30px;padding:1e30px'>x</div>\
         <div id=b style='font-size:1e20px'>y</div><div id=c style='letter-spacing:1e30px'>abc</div>",
        RESET,
    );
    for f in l.tree.fragments() {
        for v in [
            f.border_box.x,
            f.border_box.y,
            f.border_box.width,
            f.border_box.height,
        ] {
            assert!(v.is_finite() && v.abs() <= MAX_COORD * 4.0, "{v}");
        }
    }
}

#[test]
fn deep_nesting_is_truncated_without_overflowing_the_stack() {
    let depth = 5000;
    let html = format!(
        "{}x{}",
        "<div><span>".repeat(depth),
        "</span></div>".repeat(depth)
    );
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let l = lay(&html, "");
            assert!(l.boxes.truncated());
            assert!(l.tree.truncated());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_nesting_within_limit_on_default_stack() {
    // Test threads have a 2 MiB stack; the box depth limit must fit.
    let depth = MAX_BOX_DEPTH / 2 - 4;
    let html = format!(
        "{}x{}",
        "<div><span>".repeat(depth),
        "</span></div>".repeat(depth)
    );
    let l = lay(&html, "");
    assert!(!l.tree.is_empty());
}

#[test]
fn long_paragraphs_are_linear() {
    let words = "word ".repeat(40_000);
    let html = format!("<div id=d style='width:200px'>{words}</div>");
    let start = std::time::Instant::now();
    let l = lay(&html, RESET);
    // 25 chars per line: 5 words per line.
    assert_eq!(l.lines("d").len(), 8000);
    assert!(start.elapsed().as_secs() < 20);
}

#[test]
fn many_siblings_are_linear() {
    let html = "<div style='margin:3px 0;height:1px'></div>".repeat(20_000);
    let l = lay(&html, RESET);
    let body = l
        .tree
        .get(l.tree.children(l.tree.root().unwrap())[0])
        .unwrap();
    assert_approx!(body.border_box.height, 20_000.0 + 19_999.0 * 3.0);
}

#[test]
fn empty_document_and_zero_viewport() {
    let l = lay_with("", "", 0.0, &|_| Host::default());
    assert!(l.tree.root().is_some());
    let l = lay_with("<p>some text here</p>", "", f32::NAN, &|_| Host::default());
    assert!(
        l.tree
            .fragments()
            .iter()
            .all(|f| f.border_box.x.is_finite())
    );
}

#[test]
fn real_fonts_smoke_test() {
    let (doc, styles) = styled("<p>Hello, world! <b>Bold</b> <img></p>", "");
    let fonts = wb_text::FontContext::with_system_fonts();
    let tree = layout_document(&doc, &styles, &fonts, &NoHost, &LayoutOptions::default());
    let texts: Vec<&TextFragment> = tree
        .fragments()
        .iter()
        .filter_map(|f| match &f.kind {
            FragmentKind::Text(t) => Some(t),
            _ => None,
        })
        .collect();
    assert!(!texts.is_empty());
    if !fonts.is_empty() {
        assert!(texts.iter().any(|t| !t.runs.is_empty()));
        // Glyphs sit on the fragment's baseline.
        for t in &texts {
            for g in t.runs.iter().flat_map(|r| r.glyphs.iter()) {
                assert!((g.origin.y - t.baseline).abs() < 50.0);
            }
        }
    }
}
