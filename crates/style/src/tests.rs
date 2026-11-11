//! End-to-end tests: HTML in, computed styles out.

use std::sync::Arc;

use wb_css::{
    Clear, Content, ContentItem, Display, DisplayInside, DisplayOutside, Float, FontFamily,
    GenericFamily, Image, LineStyle, ListStyleType, MediaContext, Overflow, Position, Rgba,
    TextAlign, VerticalAlignKeyword, WhiteSpace, parse_stylesheet,
};
use wb_dom::{Document, Namespace, NodeId};
use wb_html::{ParseOptions, parse_document};

use crate::*;

fn parse(html: &str) -> Document {
    parse_document(html, &ParseOptions::default()).document
}

fn styled_with(
    html: &str,
    sheets: &[SheetInput],
    options: &StyleOptions,
) -> (Document, StyledDocument) {
    let doc = parse(html);
    let s = style_document(&doc, sheets, &MediaContext::default(), options);
    (doc, s)
}

fn styled(html: &str, css: &str) -> (Document, StyledDocument) {
    styled_with(
        html,
        &[SheetInput::author(parse_stylesheet(css))],
        &StyleOptions::default(),
    )
}

fn by_id(doc: &Document, id: &str) -> NodeId {
    doc.element_by_id(id).expect("element")
}

fn get(doc: &Document, s: &StyledDocument, id: &str) -> Arc<ComputedStyle> {
    s.style(by_id(doc, id)).expect("style").clone()
}

const RED: Rgba = Rgba::rgb(255, 0, 0);
const GREEN: Rgba = Rgba::rgb(0, 128, 0);
const BLUE: Rgba = Rgba::rgb(0, 0, 255);

const STD: &str = "<!DOCTYPE html>";

#[test]
fn ua_stylesheet_basics() {
    let (doc, s) = styled(
        &format!("{STD}<body id=b><h1 id=h>T</h1><div id=d><span id=s>x</span></div><p id=p></p>"),
        "",
    );
    let body = get(&doc, &s, "b");
    assert_eq!(body.display, Display::BLOCK);
    assert_eq!(
        body.margin.top,
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(8.0))
    );
    let h = get(&doc, &s, "h");
    assert_eq!(h.font_size, 32.0);
    assert_eq!(h.font_weight, 700.0);
    // 0.67em of 32px.
    assert!((h.margin.top.resolve(0.0).unwrap_or(0.0) - 21.44).abs() < 0.01);
    assert_eq!(get(&doc, &s, "d").display, Display::BLOCK);
    assert_eq!(get(&doc, &s, "s").display, Display::INLINE);
    assert_eq!(get(&doc, &s, "p").margin.bottom.resolve(0.0), Some(16.0));
    let head = doc.html_child_of_document_element("head").expect("head");
    assert!(s.style(head).expect("head style").is_display_none());
    let root = s.root_style();
    assert_eq!(root.display, Display::BLOCK);
    assert_eq!(s.root_element(), doc.document_element());
}

#[test]
fn text_nodes_use_parent_style() {
    let (doc, s) = styled(&format!("{STD}<p id=p>hello</p>"), "p { color: red }");
    let p = by_id(&doc, "p");
    let text = doc.first_child(p).expect("text");
    assert!(Arc::ptr_eq(
        s.style(text).expect("text style"),
        s.style(p).expect("p style")
    ));
}

#[test]
fn specificity_and_order() {
    let (doc, s) = styled(
        &format!("{STD}<div id=a class=b><span id=c class='x y'></span></div>"),
        "#a { color: red } .b { color: blue } div { color: green }
         span.x { color: blue } .x.y { color: green } .y { color: red }",
    );
    assert_eq!(get(&doc, &s, "a").color, RED);
    assert_eq!(get(&doc, &s, "c").color, GREEN);
    let (doc, s) = styled(
        &format!("{STD}<p id=p class=a>"),
        ".a { color: red } .a { color: blue }",
    );
    assert_eq!(get(&doc, &s, "p").color, BLUE);
    // :where() has no specificity, :is() takes its most specific argument.
    let (doc, s) = styled(
        &format!("{STD}<p id=p class=a>"),
        ":is(#p, p) { color: red } .a.a { color: blue } :where(#p) { color: green }",
    );
    assert_eq!(get(&doc, &s, "p").color, RED);
}

#[test]
fn importance_style_attribute_and_origins() {
    let (doc, s) = styled(
        &format!("{STD}<p id=p style='color: blue; margin-top: 3px !important'></p>"),
        "#p { color: red; margin-top: 5px !important }",
    );
    let p = get(&doc, &s, "p");
    assert_eq!(p.color, BLUE);
    assert_eq!(p.margin.top.resolve(0.0), Some(3.0));
    let (doc, s) = styled(
        &format!("{STD}<p id=p style='color: blue'></p>"),
        "p { color: red !important }",
    );
    assert_eq!(get(&doc, &s, "p").color, RED);

    // Important UA declarations beat author ones: `input[type=hidden]`.
    let (doc, s) = styled(
        &format!("{STD}<input id=i type=hidden>"),
        "input { display: block !important }",
    );
    assert!(get(&doc, &s, "i").is_display_none());

    // User sheets sit between UA and author for normal declarations, and
    // beat author !important when important themselves.
    let user = SheetInput {
        origin: Origin::User,
        ..SheetInput::author(parse_stylesheet(
            "p { color: green !important; margin-left: 9px }",
        ))
    };
    let author = SheetInput::author(parse_stylesheet(
        "p { color: red !important; margin-left: 1px }",
    ));
    let (doc, s) = styled_with(
        &format!("{STD}<p id=p>"),
        &[author, user],
        &StyleOptions::default(),
    );
    let p = get(&doc, &s, "p");
    assert_eq!(p.color, GREEN);
    assert_eq!(p.margin.left.resolve(0.0), Some(1.0));
}

#[test]
fn inheritance_and_css_wide_keywords() {
    let (doc, s) = styled(
        &format!(
            "{STD}<div id=a><p id=b><span id=c></span><span id=d></span><span id=e></span><span id=f></span></p></div>"
        ),
        "#a { color: red; margin-left: 10px; border: 2px solid blue }
         #b { color: blue }
         #c { margin-left: inherit; color: initial }
         #d { color: unset; margin-left: unset; border-top-color: inherit }
         #e { color: inherit; border-top-style: solid; }
         #f { all: initial }",
    );
    let c = get(&doc, &s, "c");
    assert_eq!(c.margin.left.resolve(0.0), Some(0.0)); // inherits from #b, whose margin is 0
    assert_eq!(c.color, Rgba::rgb(0, 0, 0));
    let d = get(&doc, &s, "d");
    assert_eq!(d.color, BLUE);
    assert_eq!(d.margin.left.resolve(0.0), Some(0.0));
    let e = get(&doc, &s, "e");
    assert_eq!(e.color, BLUE);
    // The initial border color is currentcolor.
    assert_eq!(e.border.top.color, BLUE);
    assert_eq!(e.border.top.width, 3.0);
    assert_eq!(e.border.left.width, 0.0);
    assert_eq!(get(&doc, &s, "a").border.top.width, 2.0);
}

#[test]
fn revert_and_layers() {
    let (doc, s) = styled(
        &format!("{STD}<div id=d></div><h1 id=h></h1>"),
        "div { display: inline } #d { display: revert } h1 { font-weight: normal; font-weight: revert }",
    );
    assert_eq!(get(&doc, &s, "d").display, Display::BLOCK);
    assert_eq!(get(&doc, &s, "h").font_weight, 700.0);

    // Unlayered beats layered; later layers beat earlier ones; the order is
    // fixed by the first mention.
    let (doc, s) = styled(
        &format!("{STD}<p id=p class=c></p><span id=s></span><b id=b></b>"),
        "@layer base, theme;
         @layer theme { #p { color: red } span { color: green } }
         @layer base { #p { color: blue } span { color: red } }
         p { color: green }
         @layer base { b { margin-left: 1px !important } }
         @layer theme { b { margin-left: 2px !important } }
         b { margin-left: 3px !important }",
    );
    assert_eq!(get(&doc, &s, "p").color, GREEN);
    assert_eq!(get(&doc, &s, "s").color, GREEN);
    // Important: earlier layers win.
    assert_eq!(get(&doc, &s, "b").margin.left.resolve(0.0), Some(1.0));

    // revert-layer falls back to the previous layer.
    let (doc, s) = styled(
        &format!("{STD}<p id=p></p>"),
        "@layer a { p { color: red } } @layer b { p { color: blue } } p { color: revert-layer }",
    );
    assert_eq!(get(&doc, &s, "p").color, BLUE);

    // Nested layers: a.b comes before a's own rules.
    let (doc, s) = styled(
        &format!("{STD}<p id=p></p>"),
        "@layer a { p { color: red } @layer b { p { color: blue } } }",
    );
    assert_eq!(get(&doc, &s, "p").color, RED);
}

#[test]
fn custom_properties() {
    let (doc, s) = styled(
        &format!("{STD}<div id=a><p id=b><span id=c></span></p></div>"),
        ":root { --main: rgb(0, 128, 0); --pad: 4px; --cycle-a: var(--cycle-b); --cycle-b: var(--cycle-a) }
         #a { color: var(--main); padding-left: calc(var(--pad) * 2) }
         #b { --main: blue; color: var(--main); margin: var(--pad) var(--missing, 7px) }
         #c { color: var(--cycle-a, red); margin-left: var(--nope); border: var(--pad) solid }",
    );
    let a = get(&doc, &s, "a");
    assert_eq!(a.color, GREEN);
    assert_eq!(a.padding.left, LengthPercentage::Length(8.0));
    let b = get(&doc, &s, "b");
    assert_eq!(b.color, BLUE);
    assert_eq!(b.margin.top.resolve(0.0), Some(4.0));
    assert_eq!(b.margin.right.resolve(0.0), Some(7.0));
    let c = get(&doc, &s, "c");
    assert_eq!(c.color, RED);
    // Invalid at computed-value time: margin-left is not inherited, so 0.
    assert_eq!(c.margin.left.resolve(0.0), Some(0.0));
    assert_eq!(c.border.top.width, 4.0);
    assert_eq!(c.border.top.style, LineStyle::Solid);
    assert!(c.custom_property("--cycle-a").is_none());
    assert!(c.custom_property("--pad").is_some());
}

#[test]
fn long_custom_property_chain() {
    let mut css = String::from(":root { --v0: 3px;");
    for i in 1..5000 {
        css.push_str(&format!("--v{i}: var(--v{});", i - 1));
    }
    css.push_str("} p { margin-left: var(--v4999) }");
    let (doc, s) = styled(&format!("{STD}<p id=p></p>"), &css);
    assert_eq!(get(&doc, &s, "p").margin.left.resolve(0.0), Some(3.0));
}

#[test]
fn computed_lengths() {
    let (doc, s) = styled(
        &format!("{STD}<html style='font-size: 10px'><body><div id=a><p id=b></p></div>"),
        "#a { font-size: 2em; width: 50%; height: 10vh; margin-left: 2rem; line-height: 150%; padding-left: calc(50% + 1em) }
         #b { font-size: smaller; line-height: 2; text-indent: 1in; letter-spacing: 0.1em; max-width: calc(10px + 5px) }",
    );
    let a = get(&doc, &s, "a");
    assert_eq!(a.font_size, 20.0);
    assert_eq!(
        a.width,
        Size::LengthPercentage(LengthPercentage::Percentage(50.0))
    );
    assert_eq!(
        a.height,
        Size::LengthPercentage(LengthPercentage::Length(72.0))
    );
    assert_eq!(a.margin.left.resolve(0.0), Some(20.0));
    assert_eq!(a.line_height, LineHeight::Length(30.0));
    assert!(matches!(a.padding.left, LengthPercentage::Calc(_)));
    assert_eq!(a.padding.left.resolve(100.0), 70.0);
    let b = get(&doc, &s, "b");
    assert!((b.font_size - 20.0 / 1.2).abs() < 0.001);
    assert_eq!(b.line_height, LineHeight::Number(2.0));
    assert_eq!(b.text_indent, LengthPercentage::Length(96.0));
    assert!((b.letter_spacing - b.font_size * 0.1).abs() < 0.001);
    assert_eq!(
        b.max_width,
        MaxSize::LengthPercentage(LengthPercentage::Length(15.0))
    );
}

#[test]
fn rem_uses_root_font_size() {
    let (doc, s) = styled(
        &format!("{STD}<html id=h><body><p id=p></p>"),
        "html { font-size: 2rem; } p { width: 3rem; font-size: 0.5rem }",
    );
    // On the root, rem refers to the initial font size.
    assert_eq!(get(&doc, &s, "h").font_size, 32.0);
    let p = get(&doc, &s, "p");
    assert_eq!(
        p.width,
        Size::LengthPercentage(LengthPercentage::Length(96.0))
    );
    assert_eq!(p.font_size, 16.0);
}

#[test]
fn hacker_news_presentational_hints() {
    // Quirks mode (no doctype), like news.ycombinator.com.
    let html = r##"<html lang="en" op="news"><head></head><body><center>
      <table id="hnmain" border="0" cellpadding="0" cellspacing="0" width="85%" bgcolor="#f6f6ef">
      <tr><td id=top bgcolor="#ff6600"><table border="0" cellpadding="0" cellspacing="0" width="100%" style="padding:2px">
        <tr><td id=logo style="width:18px;padding-right:4px"><a href="https://news.ycombinator.com"><img id=img src="y18.svg" width="18" height="18" style="border:1px white solid; display:block"></a></td>
        <td id=links style="line-height:12pt; height:10px;"><span class="pagetop"><b class="hnname"><a id=link href="news">Hacker News</a></b></span></td>
        <td id=right style="text-align:right;padding-right:4px;"><span class="pagetop"><font id=f color="#3c963c" size=2>x</font></span></td>
      </tr></table></td></tr>
      <tr id=row style="height:10px"/><tr><td id=c align=right valign=top nowrap class="title"></td></tr>
      </table></center></body></html>"##;
    let (doc, s) = styled(
        html,
        "body { font-family: Verdana, Geneva, sans-serif; font-size: 10pt; color: #828282; }
         .pagetop { font-family: Verdana; font-size: 10pt; color: #222222; }
         a:link { color: #000000; text-decoration: none; }",
    );
    assert_eq!(doc.quirks_mode(), wb_dom::QuirksMode::Quirks);
    let main = get(&doc, &s, "hnmain");
    assert_eq!(
        main.width,
        Size::LengthPercentage(LengthPercentage::Percentage(85.0))
    );
    assert_eq!(main.background_color, Rgba::rgb(0xf6, 0xf6, 0xef));
    assert_eq!(main.border_spacing, (0.0, 0.0));
    assert_eq!(main.border.top.width, 0.0);
    assert_eq!(
        main.display,
        Display::Pair {
            outside: DisplayOutside::Block,
            inside: DisplayInside::Table,
            list_item: false
        }
    );
    // Quirks: tables reset font-size to medium.
    assert_eq!(main.font_size, 16.0);
    let top = get(&doc, &s, "top");
    assert_eq!(top.background_color, Rgba::rgb(0xff, 0x66, 0x00));
    // cellpadding=0 on the table.
    assert_eq!(top.padding.top, LengthPercentage::Length(0.0));
    let img = get(&doc, &s, "img");
    assert_eq!(
        img.width,
        Size::LengthPercentage(LengthPercentage::Length(18.0))
    );
    assert_eq!(img.border.top.width, 1.0);
    assert_eq!(img.border.top.color, Rgba::rgb(255, 255, 255));
    assert_eq!(img.display, Display::BLOCK);
    let links = get(&doc, &s, "links");
    assert_eq!(links.line_height, LineHeight::Length(16.0));
    let link = get(&doc, &s, "link");
    assert_eq!(link.color, Rgba::rgb(0, 0, 0));
    assert!(link.text_decorations.is_empty());
    let f = get(&doc, &s, "f");
    assert_eq!(f.color, Rgba::rgb(0x3c, 0x96, 0x3c));
    assert_eq!(f.font_size, 16.0 * 8.0 / 9.0);
    let c = get(&doc, &s, "c");
    assert_eq!(c.text_align, TextAlign::Right);
    assert_eq!(
        c.vertical_align,
        VerticalAlign::Keyword(VerticalAlignKeyword::Top)
    );
    assert_eq!(c.white_space, WhiteSpace::Nowrap);
    let right = get(&doc, &s, "right");
    assert_eq!(right.text_align, TextAlign::Right);
    assert_eq!(
        right.font_family.first(),
        Some(&FontFamily::Named("Verdana".to_string()))
    );
}

#[test]
fn more_presentational_hints() {
    let (doc, s) = styled(
        &format!(
            "{STD}<body id=b bgcolor=black text=white link=yellow>
             <table id=t border=2 align=center cellspacing=5><tr><td id=td>x</td></tr></table>
             <table id=tl align=left></table>
             <img id=i align=left hspace=4 vspace=3 border=2>
             <br id=br clear=all><hr id=hr noshade size=4 width=50%>
             <ol id=ol type=a start=5><li id=li type=I>x</li></ol><ul id=ul type=square></ul>
             <div id=div align=center></div><p id=p align=justify></p>
             <font id=f face='Arial, sans-serif' size=+2></font><a id=a href=x>l</a>"
        ),
        "",
    );
    let b = get(&doc, &s, "b");
    assert_eq!(b.background_color, Rgba::rgb(0, 0, 0));
    assert_eq!(b.color, Rgba::rgb(255, 255, 255));
    assert_eq!(get(&doc, &s, "a").color, Rgba::rgb(255, 255, 0));
    let t = get(&doc, &s, "t");
    assert_eq!(t.border.left.width, 2.0);
    assert_eq!(t.border.left.style, LineStyle::Outset);
    assert_eq!(t.border.left.color, Rgba::rgb(128, 128, 128));
    assert_eq!(t.margin.left, LengthPercentageOrAuto::Auto);
    assert_eq!(t.border_spacing, (5.0, 5.0));
    let td = get(&doc, &s, "td");
    assert_eq!(td.border.top.width, 1.0);
    assert_eq!(td.border.top.style, LineStyle::Inset);
    assert_eq!(get(&doc, &s, "tl").float, Float::Left);
    let i = get(&doc, &s, "i");
    assert_eq!(i.float, Float::Left);
    assert_eq!(i.margin.left.resolve(0.0), Some(4.0));
    assert_eq!(i.margin.top.resolve(0.0), Some(3.0));
    assert_eq!(i.border.top.width, 2.0);
    // Floats are blockified.
    assert_eq!(i.display, Display::BLOCK);
    assert_eq!(get(&doc, &s, "br").clear, Clear::Both);
    let hr = get(&doc, &s, "hr");
    assert_eq!(
        hr.height,
        Size::LengthPercentage(LengthPercentage::Length(2.0))
    );
    assert_eq!(
        hr.width,
        Size::LengthPercentage(LengthPercentage::Percentage(50.0))
    );
    assert_eq!(hr.border.top.style, LineStyle::Solid);
    let ol = get(&doc, &s, "ol");
    assert_eq!(
        ol.list_style_type,
        ListStyleType::Named("lower-alpha".into())
    );
    assert_eq!(&*ol.counter_reset, &[("list-item".to_string(), 4)]);
    assert_eq!(
        get(&doc, &s, "li").list_style_type,
        ListStyleType::Named("upper-roman".into())
    );
    assert_eq!(
        get(&doc, &s, "ul").list_style_type,
        ListStyleType::Named("square".into())
    );
    assert_eq!(get(&doc, &s, "div").text_align, TextAlign::Center);
    assert_eq!(get(&doc, &s, "p").text_align, TextAlign::Justify);
    let f = get(&doc, &s, "f");
    assert_eq!(f.font_size, 16.0 * 1.5);
    assert_eq!(
        &*f.font_family,
        &[
            FontFamily::Named("Arial".into()),
            FontFamily::Generic(GenericFamily::SansSerif)
        ]
    );
}

#[test]
fn hints_lose_to_author_rules() {
    let (doc, s) = styled(
        &format!("{STD}<table id=t width=100 bgcolor=red></table>"),
        "table { width: 50px }",
    );
    let t = get(&doc, &s, "t");
    assert_eq!(
        t.width,
        Size::LengthPercentage(LengthPercentage::Length(50.0))
    );
    assert_eq!(t.background_color, RED);
}

#[test]
fn pseudo_elements() {
    let (doc, s) = styled(
        &format!(
            "{STD}<p id=p data-x=hi>a</p><div id=d></div><q id=q></q><ul><li id=li></li></ul><span id=n></span>"
        ),
        "p::before { content: 'pre ' attr(data-x); color: red; display: block }
         p::after { content: url(a.png) }
         #d::before { color: blue }
         *::before { box-sizing: border-box }
         #n::after { content: 'x'; display: none }
         li::marker { color: green }
         #li { color: blue }",
    );
    let p = by_id(&doc, "p");
    let before = s.pseudo_style(p, PseudoElement::Before).expect("::before");
    assert_eq!(before.color, RED);
    assert_eq!(before.display, Display::BLOCK);
    assert_eq!(
        before.content,
        Content::Items(vec![
            ContentItem::String("pre ".into()),
            ContentItem::String("hi".into())
        ])
    );
    assert!(s.pseudo_style(p, PseudoElement::After).is_some());
    // No content: no box.
    assert!(
        s.pseudo_style(by_id(&doc, "d"), PseudoElement::Before)
            .is_none()
    );
    assert!(
        s.pseudo_style(by_id(&doc, "n"), PseudoElement::After)
            .is_none()
    );
    // q gets quotes from the UA sheet.
    let q = s
        .pseudo_style(by_id(&doc, "q"), PseudoElement::Before)
        .expect("q::before");
    assert_eq!(q.content, Content::Items(vec![ContentItem::OpenQuote]));
    let marker = s
        .pseudo_style(by_id(&doc, "li"), PseudoElement::Marker)
        .expect("marker");
    assert_eq!(marker.color, GREEN);
    assert_eq!(marker.white_space, WhiteSpace::Pre);
    assert!(s.pseudo_style(p, PseudoElement::Marker).is_none());
    assert!(s.pseudo_style(p, PseudoElement::FirstLine).is_none());
}

#[test]
fn links_and_user_actions() {
    let html = format!(
        "{STD}<div id=d><a id=a href=x><span id=s>t</span></a><a id=n>no</a><input id=i></div>"
    );
    let css = "a:hover { color: green } div:hover { margin-left: 1px } :focus { margin-left: 2px }
               :focus-within { margin-right: 3px } a:visited { color: red }";
    let doc = parse(&html);
    let stylist = Stylist::new(
        &[SheetInput::author(parse_stylesheet(css))],
        &MediaContext::default(),
    );
    let s = stylist.style_document(&doc, &StyleOptions::default());
    let a = get(&doc, &s, "a");
    assert_eq!(a.color, Rgba::rgb(0, 0, 0xEE));
    assert_eq!(a.cursor.keyword, CursorKeyword::Pointer);
    assert!(!a.text_decorations.is_empty());
    // Text decorations propagate to descendants.
    assert_eq!(get(&doc, &s, "s").text_decorations.len(), 1);
    assert_eq!(get(&doc, &s, "n").color, Rgba::rgb(0, 0, 0));

    let options = StyleOptions {
        state: ElementState {
            hover: Some(by_id(&doc, "s")),
            focus: Some(by_id(&doc, "i")),
            ..ElementState::default()
        },
        ..StyleOptions::default()
    };
    let s = stylist.style_document(&doc, &options);
    assert_eq!(get(&doc, &s, "a").color, GREEN);
    let d = get(&doc, &s, "d");
    assert_eq!(d.margin.left.resolve(0.0), Some(1.0));
    assert_eq!(d.margin.right.resolve(0.0), Some(3.0));
    assert_eq!(get(&doc, &s, "i").margin.left.resolve(0.0), Some(2.0));
}

#[test]
fn media_queries() {
    let css = "p { color: red } @media (min-width: 800px) { p { color: green } }
               @media print { p { color: blue } }
               @media (prefers-color-scheme: dark) { p { margin-left: 5px } }";
    let doc = parse(&format!("{STD}<p id=p>"));
    let sheets = [SheetInput::author(parse_stylesheet(css))];
    let wide = style_document(
        &doc,
        &sheets,
        &MediaContext::default(),
        &StyleOptions::default(),
    );
    assert_eq!(get(&doc, &wide, "p").color, GREEN);
    let narrow_ctx = MediaContext {
        viewport_width: 400.0,
        prefers_color_scheme: ColorScheme::Dark,
        ..MediaContext::default()
    };
    let narrow = style_document(&doc, &sheets, &narrow_ctx, &StyleOptions::default());
    let p = get(&doc, &narrow, "p");
    assert_eq!(p.color, RED);
    assert_eq!(p.margin.left.resolve(0.0), Some(5.0));
    // Sheet-level media.
    let sheet = SheetInput {
        media: wb_css::parse_media_query_list("print"),
        ..SheetInput::author(parse_stylesheet("p { color: blue }"))
    };
    let s = style_document(
        &doc,
        &[sheet],
        &MediaContext::default(),
        &StyleOptions::default(),
    );
    assert_eq!(get(&doc, &s, "p").color, Rgba::rgb(0, 0, 0));
}

#[test]
fn imports_with_layers_and_urls() {
    let base = wb_url::Url::parse("https://example.com/css/main.css").ok();
    let imported = SheetInput {
        base_url: wb_url::Url::parse("https://cdn.example.com/lib/x.css").ok(),
        ..SheetInput::author(parse_stylesheet(
            "p { color: red; background-image: url(bg.png) }",
        ))
    };
    let main = SheetInput {
        base_url: base,
        imports: vec![Some(imported)],
        ..SheetInput::author(parse_stylesheet(
            "@import url(x.css) layer(lib); p { list-style-image: url(../i/dot.png) } p { color: green }",
        ))
    };
    let (doc, s) = styled_with(&format!("{STD}<p id=p>"), &[main], &StyleOptions::default());
    let p = get(&doc, &s, "p");
    assert_eq!(p.color, GREEN);
    assert_eq!(
        p.background_layers.first().map(|l| l.image.clone()),
        Some(Image::Url("https://cdn.example.com/lib/bg.png".into()))
    );
    assert_eq!(
        p.list_style_image,
        Image::Url("https://example.com/i/dot.png".into())
    );
}

#[test]
fn selectors() {
    let (doc, s) = styled(
        &format!(
            "{STD}<ul id=u><li id=l1 lang=en-US></li><li id=l2 title='a b'></li><li id=l3></li><li id=l4 data-v=Foo></li></ul>
             <div id=e></div><div id=f><!-- c --></div><p id=g>x</p>
             <section><h2 id=h></h2><p id=i></p><p id=j></p></section>
             <input id=c type=CHECKBOX checked><input id=x disabled><fieldset disabled><input id=y></fieldset>"
        ),
        "li:first-child { margin-left: 1px } li:last-child { margin-left: 4px }
         li:nth-child(2) { margin-left: 2px } li:nth-child(3 of :not([title])) { margin-right: 9px }
         li:nth-last-child(2) { margin-top: 3px }
         [lang|=en] { color: red } [title~=b] { color: green } [data-v^=f i] { color: blue }
         [data-v=foo] { margin-bottom: 7px }
         div:empty { margin-left: 5px } p:not(:empty) { margin-left: 6px }
         h2 + p { color: red } h2 ~ p { margin-left: 8px } section > p:last-of-type { color: blue }
         input[type=checkbox]:checked { margin-left: 11px } input:disabled { margin-left: 12px }
         input:enabled { margin-right: 13px }",
    );
    let ml = |id: &str| get(&doc, &s, id).margin.left.resolve(0.0);
    assert_eq!(ml("l1"), Some(1.0));
    assert_eq!(ml("l2"), Some(2.0));
    assert_eq!(ml("l4"), Some(4.0));
    assert_eq!(get(&doc, &s, "l4").margin.right.resolve(0.0), Some(9.0));
    assert_eq!(get(&doc, &s, "l3").margin.top.resolve(0.0), Some(3.0));
    assert_eq!(get(&doc, &s, "l1").color, RED);
    assert_eq!(get(&doc, &s, "l2").color, GREEN);
    assert_eq!(get(&doc, &s, "l4").color, BLUE);
    // Attribute values are case-sensitive by default for data-* attributes.
    assert_eq!(get(&doc, &s, "l4").margin.bottom.resolve(0.0), Some(0.0));
    assert_eq!(ml("e"), Some(5.0));
    assert_eq!(ml("f"), Some(5.0));
    assert_eq!(ml("g"), Some(6.0));
    assert_eq!(get(&doc, &s, "i").color, RED);
    assert_eq!(ml("i"), Some(8.0));
    assert_eq!(get(&doc, &s, "j").color, BLUE);
    assert_eq!(ml("c"), Some(11.0));
    assert_eq!(ml("x"), Some(12.0));
    assert_eq!(ml("y"), Some(12.0));
    assert_eq!(get(&doc, &s, "c").margin.right.resolve(0.0), Some(13.0));
}

#[test]
fn descendant_backtracking() {
    let (doc, s) = styled(
        &format!(
            "{STD}<div class=a><div class=b><div class=a><div><span id=s></span></div></div></div></div>"
        ),
        ".a > .b .a span { color: red } .b > div > span { color: blue }",
    );
    assert_eq!(get(&doc, &s, "s").color, RED);
}

#[test]
fn quirks_mode_matching() {
    let (doc, s) = styled(
        "<p id=p class=Foo></p>",
        ".foo { color: red } #P { margin-left: 1px }",
    );
    let p = get(&doc, &s, "p");
    assert_eq!(p.color, RED);
    assert_eq!(p.margin.left.resolve(0.0), Some(1.0));
    let (doc, s) = styled(
        &format!("{STD}<p id=p class=Foo></p>"),
        ".foo { color: red }",
    );
    assert_eq!(get(&doc, &s, "p").color, Rgba::rgb(0, 0, 0));
}

#[test]
fn blockification_and_fixups() {
    let (doc, s) = styled(
        &format!(
            "{STD}<div id=f><span id=item></span></div><span id=abs></span><span id=fl></span>
             <div id=o></div><div id=b></div><span id=ib></span>"
        ),
        "#f { display: flex } #abs { position: absolute; float: left } #fl { float: inline-end; display: inline-block }
         #o { overflow-x: hidden; overflow-y: visible; opacity: 2 } #b { border-style: none; border-width: 5px }
         #ib { display: inline-table; float: right }",
    );
    assert_eq!(get(&doc, &s, "item").display, Display::BLOCK);
    let abs = get(&doc, &s, "abs");
    assert_eq!(abs.display, Display::BLOCK);
    assert_eq!(abs.float, Float::None);
    assert_eq!(abs.position, Position::Absolute);
    let fl = get(&doc, &s, "fl");
    assert_eq!(fl.float, Float::Right);
    assert_eq!(
        fl.display,
        Display::Pair {
            outside: DisplayOutside::Block,
            inside: DisplayInside::FlowRoot,
            list_item: false
        }
    );
    let o = get(&doc, &s, "o");
    assert_eq!(o.overflow_y, Overflow::Auto);
    assert_eq!(o.opacity, 1.0);
    assert_eq!(get(&doc, &s, "b").border.top.width, 0.0);
    assert_eq!(
        get(&doc, &s, "ib").display,
        Display::Pair {
            outside: DisplayOutside::Block,
            inside: DisplayInside::Table,
            list_item: false
        }
    );
}

#[test]
fn backgrounds_and_colors() {
    let (doc, s) = styled(
        &format!("{STD}<div id=d></div><p id=p></p>"),
        "#d { color: blue; background: url(a.png) no-repeat 10px 50%, red url(b.png); border: 1px solid; text-decoration: underline }
         #p { background-position: 1px 2px, 3px 4px; background-image: url(x), url(y), url(z); color: Canvastext; background-color: currentcolor }",
    );
    let d = get(&doc, &s, "d");
    assert_eq!(d.background_color, RED);
    assert_eq!(d.background_layers.len(), 2);
    let first = d.background_layers.first().expect("layer");
    assert_eq!(first.repeat_x, RepeatStyle::NoRepeat);
    assert_eq!(first.position_x.offset, LengthPercentage::Length(10.0));
    assert_eq!(first.position_y.offset, LengthPercentage::Percentage(50.0));
    assert_eq!(d.border.left.color, BLUE);
    assert_eq!(d.text_decoration_color, BLUE);
    let p = get(&doc, &s, "p");
    assert_eq!(p.background_layers.len(), 3);
    let third = p.background_layers.get(2).expect("layer");
    assert_eq!(third.position_x.offset, LengthPercentage::Length(1.0));
    assert_eq!(p.background_color, Rgba::rgb(0, 0, 0));
}

#[test]
fn style_sharing() {
    let mut html = String::from(STD);
    html.push_str("<ul>");
    for _ in 0..500 {
        html.push_str("<li class=item><a href=x>link</a> text</li>");
    }
    html.push_str("</ul>");
    let (doc, s) = styled(&html, ".item { color: red } .item a { color: blue }");
    // html, head, body, ul, li, a, plus the li marker parent chain: far
    // fewer distinct styles than the ~1000 elements.
    assert!(
        s.distinct_style_count() < 12,
        "{}",
        s.distinct_style_count()
    );
    let _ = doc;
}

#[test]
fn deep_tree_does_not_overflow() {
    let mut doc = Document::new();
    let html = doc.create_element(Namespace::Html, "html", Vec::new());
    // Built bottom-up so that each append is O(1).
    let divs: Vec<NodeId> = (0..100_000)
        .map(|_| doc.create_element(Namespace::Html, "div", Vec::new()))
        .collect();
    for pair in divs.windows(2).rev() {
        doc.append_child(pair[0], pair[1]).expect("append");
    }
    doc.append_child(html, divs[0]).expect("append");
    let root = doc.root();
    doc.append_child(root, html).expect("append");
    let last = *divs.last().expect("div");
    let sheets = [SheetInput::author(parse_stylesheet(
        "div div div div span { color: red } div { margin-left: 1em } :is(div) > div { padding-left: 1px }",
    ))];
    let s = style_document(
        &doc,
        &sheets,
        &MediaContext::default(),
        &StyleOptions::default(),
    );
    let st = s.style(last).expect("style");
    assert_eq!(st.margin.left.resolve(0.0), Some(16.0));
    assert_eq!(st.padding.left, LengthPercentage::Length(1.0));
}

#[test]
fn many_rules_and_elements() {
    let mut css = String::new();
    for i in 0..3000 {
        css.push_str(&format!(".c{i} .d{i} > span.e{i} {{ color: red }} #id{i} {{ margin: {i}px }} div.k{} a {{ color: blue }}\n", i % 50));
    }
    let mut html = String::from(STD);
    for i in 0..2000 {
        html.push_str(&format!(
            "<div class='k{} c{}'><p id=id{i}><span class=e{i}><a href=#>x</a></span></p></div>",
            i % 50,
            i % 7
        ));
    }
    let start = std::time::Instant::now();
    let (doc, s) = styled(&html, &css);
    // Generous even for debug builds; guards against quadratic behaviour.
    assert!(start.elapsed().as_secs() < 60);
    assert_eq!(
        get(&doc, &s, "id1999").margin.top.resolve(0.0),
        Some(1999.0)
    );
}

#[test]
fn pathological_selectors() {
    // A long chain of descendant combinators against a deep tree must not
    // explode (each step fails globally instead of backtracking).
    let mut css = String::new();
    for _ in 0..200 {
        css.push_str("div ");
    }
    css.push_str("span { color: red }");
    let mut html = String::from(STD);
    for _ in 0..300 {
        html.push_str("<div>");
    }
    html.push_str("<span id=s></span>");
    let (doc, s) = styled(&html, &css);
    assert_eq!(get(&doc, &s, "s").color, RED);
    let mut deep = String::from(":is(");
    for _ in 0..20 {
        deep.push_str(":not(:is(");
    }
    deep.push('p');
    for _ in 0..20 {
        deep.push_str("))");
    }
    deep.push_str(") { color: red }");
    let (doc, s) = styled(&format!("{STD}<p id=p>"), &deep);
    assert_eq!(get(&doc, &s, "p").color, RED);
}

#[test]
fn document_sources() {
    let doc = parse(&format!(
        "{STD}<head><style>p{{}}</style><link rel=stylesheet href=a.css media=print>
         <link rel='alternate stylesheet' href=b.css><style type=text/plain>x</style>
         <link rel=StyleSheet href=c.css></head><body><style>q{{}}</style>"
    ));
    let sources = document_style_sources(&doc);
    assert_eq!(sources.len(), 4);
    assert_eq!(
        sources.first().map(|s| s.kind.clone()),
        Some(StyleSourceKind::Inline("p{}".into()))
    );
    assert_eq!(
        sources.get(1).map(|s| s.kind.clone()),
        Some(StyleSourceKind::External {
            href: "a.css".into()
        })
    );
    assert!(!sources.get(1).map(|s| s.media.0.is_empty()).unwrap_or(true));
    assert_eq!(
        sources.get(2).map(|s| s.kind.clone()),
        Some(StyleSourceKind::External {
            href: "c.css".into()
        })
    );
}

#[test]
fn style_attribute_urls_use_document_base() {
    let options = StyleOptions {
        base_url: wb_url::Url::parse("https://example.com/dir/page.html").ok(),
        ..StyleOptions::default()
    };
    let (doc, s) = styled_with(
        &format!(
            "{STD}<div id=d style='background-image: url(img.png)'></div><table id=t background=t.png></table>"
        ),
        &[],
        &options,
    );
    assert_eq!(
        get(&doc, &s, "d")
            .background_layers
            .first()
            .map(|l| l.image.clone()),
        Some(Image::Url("https://example.com/dir/img.png".into()))
    );
    assert_eq!(
        get(&doc, &s, "t")
            .background_layers
            .first()
            .map(|l| l.image.clone()),
        Some(Image::Url("https://example.com/dir/t.png".into()))
    );
}

#[test]
fn font_weights_and_families() {
    let (doc, s) = styled(
        &format!("{STD}<div id=a><b id=b><b id=c><span id=d></span></b></b></div><pre id=p></pre>"),
        "#a { font-weight: 300 } #d { font-weight: lighter }",
    );
    assert_eq!(get(&doc, &s, "b").font_weight, 400.0);
    assert_eq!(get(&doc, &s, "c").font_weight, 700.0);
    assert_eq!(get(&doc, &s, "d").font_weight, 400.0);
    assert_eq!(
        &*get(&doc, &s, "p").font_family,
        &[FontFamily::Generic(GenericFamily::Monospace)]
    );
    assert_eq!(
        s.root_style().font_family.first(),
        Some(&FontFamily::Generic(GenericFamily::Serif))
    );
}

#[test]
fn template_contents_are_not_styled() {
    let (doc, s) = styled(&format!("{STD}<template id=t><p>x</p></template>"), "");
    let t = by_id(&doc, "t");
    assert!(s.style(t).expect("template").is_display_none());
    let contents = doc.template_contents(t).expect("contents");
    let p = doc.first_child(contents).expect("p");
    assert!(s.style(p).is_none());
}
