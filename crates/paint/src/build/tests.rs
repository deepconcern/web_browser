//! Builder tests: real HTML parsed by `wb-html`, styled by `wb-style` and
//! laid out by `wb-layout`, then built into a display list and checked
//! for order and a few rasterized pixels.

use std::collections::HashMap;
use std::sync::Arc;

use wb_css::parse_stylesheet;
use wb_dom::{Document, NodeId};
use wb_image::Image;
use wb_layout::{FragmentTree, LayoutHost, LayoutOptions, Size};
use wb_style::{MediaContext, SheetInput, StyleOptions, style_document};
use wb_text::FontContext;

use super::*;
use crate::test_support::{BlockGlyphs, NoGlyphs, font};
use crate::{Color, DisplayItem, Point, RasterOptions, hit_test_topmost, rasterize};

#[derive(Default)]
struct Host {
    scroll: HashMap<String, wb_layout::Point>,
    sizes: HashMap<String, Size>,
    doc_ids: HashMap<NodeId, String>,
}

impl LayoutHost for Host {
    fn image_size(&self, node: NodeId) -> Option<Size> {
        self.doc_ids
            .get(&node)
            .and_then(|id| self.sizes.get(id))
            .copied()
    }
    fn scroll_offset(&self, node: NodeId) -> wb_layout::Point {
        self.doc_ids
            .get(&node)
            .and_then(|id| self.scroll.get(id))
            .copied()
            .unwrap_or(wb_layout::Point::ZERO)
    }
}

#[derive(Default)]
struct Res {
    urls: HashMap<String, Arc<Image>>,
    nodes: HashMap<NodeId, Arc<Image>>,
}

impl PaintResources for Res {
    fn node_image(&self, node: NodeId) -> Option<Arc<Image>> {
        self.nodes.get(&node).cloned()
    }
    fn url_image(&self, url: &str) -> Option<Arc<Image>> {
        self.urls.get(url).cloned()
    }
}

struct Built {
    doc: Document,
    tree: FragmentTree,
    list: DisplayList,
}

impl Built {
    fn node(&self, id: &str) -> NodeId {
        self.doc.element_by_id(id).expect("element")
    }
}

fn solid(w: u32, h: u32, c: [u8; 4]) -> Arc<Image> {
    let px: Vec<u8> = (0..w * h).flat_map(|_| c).collect();
    Arc::new(Image::from_premultiplied_rgba8(w, h, px).expect("image"))
}

fn build_with(
    html: &str,
    css: &str,
    fonts: &FontContext,
    mut host: Host,
    res: &dyn Fn(&Document) -> Res,
) -> Built {
    let doc = wb_html::parse_document(html, &Default::default()).document;
    let sheets = vec![SheetInput::author(parse_stylesheet(&format!(
        "body {{ margin: 0 }} {css}"
    )))];
    let media = MediaContext {
        viewport_width: 800.0,
        viewport_height: 600.0,
        ..MediaContext::default()
    };
    let styles = style_document(&doc, &sheets, &media, &StyleOptions::default());
    for id in host
        .scroll
        .keys()
        .chain(host.sizes.keys())
        .cloned()
        .collect::<Vec<_>>()
    {
        if let Some(n) = doc.element_by_id(&id) {
            host.doc_ids.insert(n, id);
        }
    }
    let tree = wb_layout::layout_document(
        &doc,
        &styles,
        fonts,
        &host,
        &LayoutOptions {
            viewport: Size::new(800.0, 600.0),
        },
    );
    let r = res(&doc);
    let list = build_display_list(&doc, &tree, fonts, &r, &BuildOptions::default());
    Built { doc, tree, list }
}

fn build(html: &str, css: &str) -> Built {
    build_with(html, css, &FontContext::new(), Host::default(), &|_| {
        Res::default()
    })
}

/// The blue channels of opaque fills whose red and green are 0, in list
/// order: tests color boxes `rgb(0,0,N)` to name them.
fn fill_order(list: &DisplayList) -> Vec<u8> {
    list.items()
        .iter()
        .filter_map(|i| match i {
            DisplayItem::Fill(f) if f.color.r == 0 && f.color.g == 0 && f.color.b > 0 => {
                Some(f.color.b)
            }
            _ => None,
        })
        .collect()
}

fn raster(list: &DisplayList) -> crate::Raster {
    rasterize(
        list,
        &RasterOptions::new(crate::Rect::new(0.0, 0.0, 800.0, 600.0), 1.0),
        &NoGlyphs,
    )
    .expect("raster")
}

fn bg(n: u8) -> String {
    format!("background: rgb(0,0,{n});")
}

#[test]
fn appendix_e_layers() {
    let html = format!(
        r#"<div style="position:relative;z-index:-1;{};height:10px"></div>
        <div style="{};height:10px"></div>
        <div style="float:left;{};width:10px;height:10px"></div>
        <div style="position:relative;{};height:10px"></div>
        <div style="position:relative;z-index:3;{};height:10px"></div>
        <div style="position:relative;z-index:1;{};height:10px"></div>
        <div style="opacity:0.5;{};height:10px"></div>"#,
        bg(1),
        bg(2),
        bg(3),
        bg(4),
        bg(5),
        bg(6),
        bg(7)
    );
    let b = build(&html, "");
    assert_eq!(fill_order(&b.list), vec![1, 2, 3, 4, 7, 6, 5]);
    // The opacity group wraps box 7 only.
    let items = b.list.items();
    let open = items
        .iter()
        .position(|i| matches!(i, DisplayItem::PushOpacity(o) if (*o - 0.5).abs() < 1e-6))
        .expect("opacity group");
    assert!(matches!(&items[open + 1], DisplayItem::Fill(f) if f.color.b == 7));
    assert!(matches!(items[open + 2], DisplayItem::PopOpacity));
}

#[test]
fn positioned_auto_is_a_pseudo_stacking_context() {
    // P (positioned, z auto) holds a block child, a positioned child and a
    // negative z-index child; R is a later positioned sibling.
    let html = format!(
        r#"<div style="position:relative;{};height:40px">
          <div style="{};height:10px"></div>
          <div style="position:relative;{};height:10px"></div>
          <div style="position:relative;z-index:-1;{};height:10px"></div>
        </div>
        <div style="position:relative;{};height:10px"></div>
        <div style="{};height:10px"></div>"#,
        bg(1),
        bg(2),
        bg(3),
        bg(5),
        bg(4),
        bg(6)
    );
    let b = build(&html, "");
    // Negative z first (root stacking context), then the in-flow block 6,
    // then P atomically (1, 2), then its positioned child 3, then R (4).
    assert_eq!(fill_order(&b.list), vec![5, 6, 1, 2, 3, 4]);
}

#[test]
fn inline_content_paints_after_blocks_and_floats() {
    // An inline-block (atomic, step 5) paints above a later block's
    // background (step 3) and above a float (step 4).
    let html = format!(
        r#"<div><span style="display:inline-block;width:10px;height:10px;{}"></span></div>
        <div style="{};height:10px"></div>
        <div style="float:left;width:5px;height:5px;{}"></div>"#,
        bg(1),
        bg(2),
        bg(3)
    );
    let b = build(&html, "");
    assert_eq!(fill_order(&b.list), vec![2, 3, 1]);
}

#[test]
fn body_background_propagates_to_canvas() {
    let b = build(
        "<p style='height:20px'>x</p>",
        "body { background: rgb(0,0,9); height: 50px }",
    );
    let fills: Vec<_> = b
        .list
        .items()
        .iter()
        .filter_map(|i| match i {
            DisplayItem::Fill(f) if f.color == Color::rgb(0, 0, 9) => Some(f.shape.rect),
            _ => None,
        })
        .collect();
    // Painted once, over the whole canvas (at least the viewport).
    assert_eq!(fills.len(), 1);
    assert!(fills[0].width >= 800.0 && fills[0].height >= 600.0);
    let r = raster(&b.list);
    assert_eq!(r.pixel(700, 500), Some([0, 0, 9, 255]));
    // The canvas hits as the root element.
    let hit = hit_test_topmost(&b.list, Point::new(700.0, 500.0)).expect("hit");
    let target = resolve_hit(&b.tree, &hit).expect("target");
    assert_eq!(target.node, b.doc.document_element());
}

#[test]
fn root_background_wins_over_body() {
    let b = build(
        "",
        "html { background: rgb(0,0,8) } body { background: rgb(0,0,9); height: 50px }",
    );
    assert_eq!(fill_order(&b.list), vec![8, 9]);
    let r = raster(&b.list);
    assert_eq!(r.pixel(10, 10), Some([0, 0, 9, 255]));
    assert_eq!(r.pixel(10, 100), Some([0, 0, 8, 255]));
}

#[test]
fn borders_and_radii() {
    let b = build(
        "<div id=d></div>",
        "#d { width: 100px; height: 50px; border: 5px solid rgb(0,0,3);
              border-radius: 50%; background: rgb(0,0,4) }",
    );
    let border = b
        .list
        .items()
        .iter()
        .find_map(|i| match i {
            DisplayItem::Border(b) => Some(b.clone()),
            _ => None,
        })
        .expect("border");
    assert_eq!(border.rect, crate::Rect::new(0.0, 0.0, 110.0, 60.0));
    assert_eq!(border.top.width, 5.0);
    // 50% of 110 x 60.
    assert!((border.radii.top_left.x - 55.0).abs() < 1e-3);
    assert!((border.radii.top_left.y - 30.0).abs() < 1e-3);
    // The background fill is rounded too.
    let r = raster(&b.list);
    assert_eq!(r.pixel(1, 1), Some([0, 0, 0, 0]));
    assert_eq!(r.pixel(55, 2), Some([0, 0, 3, 255]));
    assert_eq!(r.pixel(55, 30), Some([0, 0, 4, 255]));
}

#[test]
fn overflow_clip_with_rounded_padding_edge() {
    let b = build(
        r#"<div id=c><div id=big></div></div>"#,
        "#c { width: 40px; height: 40px; overflow: hidden; border: 2px solid rgb(0,0,1);
              border-radius: 12px }
         #big { width: 100px; height: 100px; background: rgb(0,0,2) }",
    );
    let clip = b
        .list
        .items()
        .iter()
        .find_map(|i| match i {
            DisplayItem::PushClip(s) => Some(*s),
            _ => None,
        })
        .expect("clip");
    assert_eq!(clip.rect, crate::Rect::new(2.0, 2.0, 40.0, 40.0));
    // Inner radius = 12 - 2.
    assert!((clip.radii.top_left.x - 10.0).abs() < 1e-3);
    let r = raster(&b.list);
    assert_eq!(r.pixel(20, 20), Some([0, 0, 2, 255]));
    assert_eq!(r.pixel(60, 20), Some([0, 0, 0, 0]));
    // The rounded corner of the clip.
    assert_ne!(r.pixel(3, 3), Some([0, 0, 2, 255]));
}

#[test]
fn single_axis_clip_is_a_band() {
    let b = build(
        r#"<div id=c><div id=big></div></div>"#,
        "#c { width: 40px; height: 40px; overflow-x: clip }
         #big { width: 100px; height: 100px; background: rgb(0,0,2) }",
    );
    let r = raster(&b.list);
    assert_eq!(r.pixel(20, 80), Some([0, 0, 2, 255]));
    assert_eq!(r.pixel(60, 20), Some([0, 0, 0, 0]));
}

#[test]
fn scroll_container_uses_push_scroll() {
    let mut host = Host::default();
    host.scroll
        .insert("s".into(), wb_layout::Point::new(0.0, 30.0));
    let b = build_with(
        r#"<div id=s><div id=a></div><div id=b></div></div>"#,
        "#s { width: 50px; height: 50px; overflow: auto }
         #a { height: 40px; background: rgb(0,0,1) } #b { height: 40px; background: rgb(0,0,2) }",
        &FontContext::new(),
        host,
        &|_| Res::default(),
    );
    let items = b.list.items();
    let scroll = items
        .iter()
        .position(|i| matches!(i, DisplayItem::PushScroll { dx: 0, dy: -30 }))
        .expect("scroll group");
    assert!(matches!(items[scroll - 1], DisplayItem::PushClip(_)));
    // The items inside are in unscrolled coordinates.
    let b_rect = items
        .iter()
        .find_map(|i| match i {
            DisplayItem::Fill(f) if f.color.b == 2 => Some(f.shape.rect),
            _ => None,
        })
        .expect("fill");
    assert_eq!(b_rect.y, 40.0);
    let r = raster(&b.list);
    assert_eq!(r.pixel(10, 5), Some([0, 0, 1, 255]));
    assert_eq!(r.pixel(10, 15), Some([0, 0, 2, 255]));
    assert_eq!(r.pixel(10, 55), Some([0, 0, 0, 0]));
    // Hit testing sees the scrolled content.
    let hit = hit_test_topmost(&b.list, Point::new(10.0, 15.0)).expect("hit");
    let t = resolve_hit(&b.tree, &hit).expect("target");
    assert_eq!(t.node, Some(b.node("b")));
}

#[test]
fn background_image_position_size_repeat() {
    let html = r#"<div id=a></div><div id=b></div><div id=c></div><div id=d></div>"#;
    let css = "div { width: 100px; height: 50px; padding: 10px; border: 5px solid transparent }
        #a { background: url(i.png) no-repeat right 4px bottom 0 }
        #b { background: url(i.png) repeat-x; background-origin: content-box;
             background-clip: padding-box }
        #c { background: url(i.png) no-repeat; background-size: contain }
        #d { background: url(i.png) round; background-size: 30px auto }";
    let b = build_with(html, css, &FontContext::new(), Host::default(), &|_| {
        let mut r = Res::default();
        r.urls
            .insert("i.png".into(), solid(20, 10, [0, 0, 255, 255]));
        r
    });
    let tiles: Vec<_> = b
        .list
        .items()
        .iter()
        .filter_map(|i| match i {
            DisplayItem::BackgroundImage(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tiles.len(), 4);
    // #a: padding box 5..125 x 5..75, tile 20x10 at right 4px, bottom.
    let a = &tiles[0];
    assert_eq!(
        a.tile,
        crate::Rect::new(125.0 - 4.0 - 20.0, 65.0, 20.0, 10.0)
    );
    assert!(!a.repeat_x && !a.repeat_y);
    assert_eq!(a.clip.rect, crate::Rect::new(0.0, 0.0, 130.0, 80.0));
    // #b: content box origin, padding-box clip.
    let b0 = &tiles[1];
    assert_eq!(b0.tile.x, 15.0);
    assert_eq!(b0.tile.y, 80.0 + 15.0);
    assert!(b0.repeat_x && !b0.repeat_y);
    assert_eq!(b0.clip.rect, crate::Rect::new(5.0, 85.0, 120.0, 70.0));
    // #c: contain in a 120x70 area: 120x60.
    assert_eq!((tiles[2].tile.width, tiles[2].tile.height), (120.0, 60.0));
    // #d: round on both axes rounds each independently (§2.5): 120 / 30
    // = 4 tiles of 30; 70 / 15 rounds to 5 tiles of 14.
    assert_eq!((tiles[3].tile.width, tiles[3].tile.height), (30.0, 14.0));
    let r = raster(&b.list);
    assert_eq!(r.pixel(110, 70), Some([0, 0, 255, 255]));
    assert_eq!(r.pixel(10, 10), Some([0, 0, 0, 0]));
}

#[test]
fn background_space_expands_tiles() {
    let b = build_with(
        "<div></div>",
        "div { width: 100px; height: 20px; background: url(i.png) space no-repeat }",
        &FontContext::new(),
        Host::default(),
        &|_| {
            let mut r = Res::default();
            r.urls
                .insert("i.png".into(), solid(30, 20, [0, 0, 255, 255]));
            r
        },
    );
    let xs: Vec<f32> = b
        .list
        .items()
        .iter()
        .filter_map(|i| match i {
            DisplayItem::BackgroundImage(t) => Some(t.tile.x),
            _ => None,
        })
        .collect();
    // Three tiles fit; the gap is (100 - 90) / 2.
    assert_eq!(xs, vec![0.0, 35.0, 70.0]);
}

#[test]
fn replaced_image_and_hit() {
    let mut host = Host::default();
    host.sizes.insert("i".into(), Size::new(20.0, 10.0));
    let b = build_with(
        r#"<a href="/x"><img id=i style="padding:2px"></a>"#,
        "",
        &FontContext::new(),
        host,
        &|doc| {
            let mut r = Res::default();
            r.nodes.insert(
                doc.element_by_id("i").expect("img"),
                solid(20, 10, [0, 255, 0, 255]),
            );
            r
        },
    );
    let dest = b
        .list
        .items()
        .iter()
        .find_map(|i| match i {
            DisplayItem::Image(i) => Some(i.dest),
            _ => None,
        })
        .expect("image");
    assert_eq!((dest.width, dest.height), (20.0, 10.0));
    let r = raster(&b.list);
    let (x, y) = ((dest.x + 5.0) as u32, (dest.y + 5.0) as u32);
    assert_eq!(r.pixel(x, y), Some([0, 255, 0, 255]));
    let hit = hit_test_topmost(&b.list, Point::new(dest.x + 5.0, dest.y + 5.0)).expect("hit");
    let t = resolve_hit(&b.tree, &hit).expect("target");
    assert_eq!(t.node, Some(b.node("i")));
    assert_eq!(fragment_tag(t.fragment), hit.tag);
}

#[test]
fn visibility_hidden_skips_box_but_not_visible_children() {
    let html = format!(
        r#"<div id=h style="visibility:hidden;{};height:30px">
          <div style="visibility:visible;{};height:10px"></div></div>"#,
        bg(1),
        bg(2)
    );
    let b = build(&html, "");
    assert_eq!(fill_order(&b.list), vec![2]);
    // The hidden box is not a hit target; its visible child is.
    let hit = hit_test_topmost(&b.list, Point::new(5.0, 20.0)).expect("hit");
    let t = resolve_hit(&b.tree, &hit).expect("target");
    assert_ne!(t.node, Some(b.node("h")));
}

#[test]
fn checkbox_and_controls() {
    let b = build(
        r#"<input type=checkbox checked id=c><input type=radio id=r><input type=range>"#,
        "",
    );
    let fills: Vec<Color> = b
        .list
        .items()
        .iter()
        .filter_map(|i| match i {
            DisplayItem::Fill(f) => Some(f.color),
            _ => None,
        })
        .collect();
    // Two white control faces, one check mark in the text color.
    assert_eq!(fills.iter().filter(|c| **c == Color::WHITE).count(), 2);
    assert!(fills.contains(&Color::BLACK));
}

#[test]
fn opacity_group_rasterizes_translucent() {
    let b = build(
        "<div></div>",
        "div { opacity: 0.5; width: 10px; height: 10px; background: rgb(0,0,255) }",
    );
    let r = raster(&b.list);
    let p = r.pixel(5, 5).expect("pixel");
    assert!((p[3] as i32 - 128).abs() <= 1, "{p:?}");
}

#[test]
fn abspos_escapes_clip_of_non_positioned_ancestor() {
    // The overflow box is not positioned and the abspos box's containing
    // block (the relative outer box) is outside it: no clip applies.
    let html = format!(
        r#"<div style="position:relative">
          <div style="overflow:hidden;height:10px;width:10px">
            <div style="position:absolute;width:50px;height:50px;{}"></div>
          </div></div>"#,
        bg(1)
    );
    let b = build(&html, "");
    let items = b.list.items();
    let fill = items
        .iter()
        .position(|i| matches!(i, DisplayItem::Fill(f) if f.color.b == 1))
        .expect("fill");
    let depth: i32 = items[..fill]
        .iter()
        .map(|i| match i {
            DisplayItem::PushClip(_) => 1,
            DisplayItem::PopClip => -1,
            _ => 0,
        })
        .sum();
    assert_eq!(depth, 0);
}

#[test]
fn text_runs_decorations_and_selection_hit() {
    let Some((fonts, _)) = font() else { return };
    let b = build_with(
        r#"<p><a id=a href="/x" style="color:rgb(0,0,200)">Hello world</a>
           <s id=s>gone</s></p>"#,
        "p { margin: 0; font-size: 20px }",
        fonts,
        Host::default(),
        &|_| Res::default(),
    );
    let items = b.list.items();
    let first_text = items
        .iter()
        .position(|i| matches!(i, DisplayItem::Text(_)))
        .expect("text");
    let DisplayItem::Text(t) = &items[first_text] else {
        panic!("not text")
    };
    assert_eq!(t.color, Color::rgb(0, 0, 200));
    assert!(!t.glyphs.is_empty());
    // The link's underline (UA style) comes before its text.
    let underline = items
        .iter()
        .position(|i| matches!(i, DisplayItem::Fill(f) if f.color == Color::rgb(0, 0, 200)))
        .expect("underline");
    assert!(underline < first_text);
    // `<s>`'s line-through comes after its text.
    let s_text = items
        .iter()
        .rposition(|i| matches!(i, DisplayItem::Text(_)))
        .expect("text");
    let last_fill = items
        .iter()
        .rposition(|i| matches!(i, DisplayItem::Fill(_)))
        .expect("fill");
    assert!(last_fill > s_text);
    // Clicking the middle of "Hello" selects inside the text node of <a>.
    let bounds = t.bounds;
    let p = Point::new(bounds.x + 12.0, bounds.y + bounds.height / 2.0);
    let hit = hit_test_topmost(&b.list, p).expect("hit");
    let target = resolve_hit(&b.tree, &hit).expect("target");
    let node = target.node.expect("node");
    assert_eq!(b.doc.parent(node), Some(b.node("a")));
    let off = target.text_offset.expect("offset");
    assert!(off > 0 && off < 5, "{off}");
    // The glyphs land on pixels.
    let glyphs = BlockGlyphs { w: 4, h: 4, top: 4 };
    let r = rasterize(
        &b.list,
        &RasterOptions::new(crate::Rect::new(0.0, 0.0, 800.0, 600.0), 1.0),
        &glyphs,
    )
    .expect("raster");
    let g = t.glyphs[0];
    let (gx, gy) = ((t.origin.x + g.x + 1.0) as u32, (t.origin.y - 2.0) as u32);
    assert_eq!(r.pixel(gx, gy), Some([0, 0, 200, 255]));
}

#[test]
fn select_shows_selected_option() {
    let Some((fonts, _)) = font() else { return };
    let b = build_with(
        r#"<select><option>One</option><option selected>Two</option></select>
           <input value="abc"><button>Go</button>"#,
        "",
        fonts,
        Host::default(),
        &|_| Res::default(),
    );
    let texts = b
        .list
        .items()
        .iter()
        .filter(|i| matches!(i, DisplayItem::Text(_)))
        .count();
    // "Two", "abc" and the button's "Go" (laid out as text by layout).
    assert!(texts >= 3, "{texts}");
}

#[test]
fn list_markers_paint() {
    let Some((fonts, _)) = font() else { return };
    let b = build_with(
        "<ol><li>a</li><li>b</li></ol>",
        "",
        fonts,
        Host::default(),
        &|_| Res::default(),
    );
    let texts = b
        .list
        .items()
        .iter()
        .filter(|i| matches!(i, DisplayItem::Text(_)))
        .count();
    assert!(texts >= 4, "{texts}");
}

#[test]
fn empty_document_and_limits() {
    let b = build("", "");
    assert!(!b.list.is_empty());
    let doc = wb_html::parse_document("<div></div>", &Default::default()).document;
    let empty = FragmentTree::default();
    let list = build_display_list(
        &doc,
        &empty,
        &FontContext::new(),
        &NoResources,
        &BuildOptions::default(),
    );
    assert!(list.is_empty());
}

#[test]
fn deep_nesting_is_bounded() {
    let html = "<div style='overflow:hidden;position:relative;z-index:1;opacity:.9'>".repeat(400);
    let b = build(&html, "");
    // Groups stay balanced.
    let mut depth = 0i64;
    for i in b.list.items() {
        match i {
            DisplayItem::PushClip(_)
            | DisplayItem::PushOpacity(_)
            | DisplayItem::PushScroll { .. } => depth += 1,
            DisplayItem::PopClip | DisplayItem::PopOpacity | DisplayItem::PopScroll => depth -= 1,
            _ => {}
        }
        assert!(depth >= 0);
    }
    assert_eq!(depth, 0);
}
