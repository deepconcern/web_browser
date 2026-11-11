//! End-to-end tests: HTML, CSS (linked, imported, inline) and images
//! loaded through a [`LocalFetcher`], rendered, and checked pixel by pixel.

use std::sync::{Arc, OnceLock};

use wb_text::FontContext;
use wb_url::Url;

use super::*;

/// Fonts are not needed for the box-only pages here; an empty context
/// keeps the tests fast and machine independent.
fn fonts() -> Arc<FontContext> {
    static F: OnceLock<Arc<FontContext>> = OnceLock::new();
    Arc::clone(F.get_or_init(|| Arc::new(FontContext::new())))
}

fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = ::png::Encoder::new(&mut out, w, h);
    enc.set_color(::png::ColorType::Rgba);
    enc.set_depth(::png::BitDepth::Eight);
    let mut wr = enc.write_header().unwrap();
    let data: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
    wr.write_image_data(&data).unwrap();
    wr.finish().unwrap();
    out
}

const PAGE: &str = r#"<!DOCTYPE html>
<html><head><title>  My
  Page </title>
<link rel=stylesheet href="style.css">
<style>
body { margin: 0 }
#a { width: 50px; height: 50px; background: rgb(255, 0, 0) }
.ad { width: 50px; height: 50px; background: black }
#bg { width: 20px; height: 20px; background: url(bg.png) }
#tracked { width: 20px; height: 20px; background: rgb(0, 0, 255) url(/ads/pixel.png) }
img { display: block }
a { display: block; width: 100px; height: 20px; background: rgb(0, 0, 255) }
</style></head>
<body><div id=a></div><div id=b></div><div id=c></div><img src="img.png"><div class=ad></div><div id=bg></div><div id=tracked></div><a href="next?x=1#f"></a></body></html>"#;

fn fetcher() -> LocalFetcher {
    let mut f = LocalFetcher::new();
    f.insert(
        "https://site.test/dir/page.html",
        "text/html; charset=utf-8",
        PAGE,
    );
    f.insert(
        "https://site.test/dir/style.css",
        "text/css",
        "@import url(\"more.css\");\n#b { width: 50px; height: 50px; background: #00ff00 }",
    );
    f.insert(
        "https://site.test/dir/more.css",
        "text/css",
        "@import \"style.css\"; #c { width: 30px; height: 10px; background: rgb(255, 0, 255) }",
    );
    f.insert(
        "https://site.test/dir/img.png",
        "image/png",
        solid_png(4, 6, [255, 255, 0, 255]),
    );
    f.insert(
        "https://site.test/dir/bg.png",
        "image/png",
        solid_png(2, 2, [0, 128, 0, 255]),
    );
    f.insert(
        "https://site.test/ads/pixel.png",
        "image/png",
        solid_png(1, 1, [255, 0, 0, 255]),
    );
    f
}

fn url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn px(r: &Raster, x: u32, y: u32) -> [u8; 4] {
    r.pixel(x, y).unwrap()
}

#[test]
fn renders_page_with_linked_imported_css_and_images() {
    let engine = Engine::new(Arc::new(fetcher()), fonts());
    let mut page = engine
        .load(&url("https://site.test/dir/page.html"))
        .unwrap();
    assert_eq!(page.title(), "My Page");
    assert_eq!(page.status(), 200);
    let r = page.render(&RenderOptions::new(200.0, 300.0)).unwrap();
    assert_eq!((r.width(), r.height()), (200, 300));
    // #a: red 50x50 at the top.
    assert_eq!(px(&r, 10, 10), [255, 0, 0, 255]);
    assert_eq!(px(&r, 60, 10), [255, 255, 255, 255]);
    // #b from the linked sheet: green, y 50..100.
    assert_eq!(px(&r, 10, 75), [0, 255, 0, 255]);
    // #c from the @import (the import cycle back to style.css is ignored).
    assert_eq!(px(&r, 5, 105), [255, 0, 255, 255]);
    assert_eq!(px(&r, 35, 105), [255, 255, 255, 255]);
    // The image: 4x6 yellow at y 110..116.
    assert_eq!(px(&r, 1, 112), [255, 255, 0, 255]);
    assert_eq!(px(&r, 5, 112), [255, 255, 255, 255]);
    // No blocker: the .ad box is black at y 116..166.
    assert_eq!(px(&r, 10, 140), [0, 0, 0, 255]);
    // Background image tiles: green at y 166..186.
    assert_eq!(px(&r, 10, 170), [0, 128, 0, 255]);
    // The tracking pixel loaded (red over the blue box) at y 186..206.
    assert_eq!(px(&r, 0, 186), [255, 0, 0, 255]);
}

#[test]
fn hit_testing_and_links() {
    let engine = Engine::new(Arc::new(fetcher()), fonts());
    let mut page = engine
        .load(&url("https://site.test/dir/page.html"))
        .unwrap();
    assert!(
        page.link_at(Point::new(10.0, 10.0)).is_none(),
        "before render"
    );
    page.render(&RenderOptions::new(200.0, 300.0)).unwrap();
    let hit = page.hit_test(Point::new(10.0, 10.0)).unwrap();
    let a = page.document().element_by_id("a").unwrap();
    assert_eq!(hit.node, Some(a));
    assert!(page.link_at(Point::new(10.0, 10.0)).is_none());
    let link = page.link_at(Point::new(50.0, 215.0)).unwrap();
    assert_eq!(link.as_str(), "https://site.test/dir/next?x=1#f");
    let size = page.content_size().unwrap();
    assert_eq!(size.width, 200.0);
    assert_eq!(size.height, 300.0);
    assert_eq!(page.fragment_position("a"), Some(Point::new(0.0, 0.0)));
    assert_eq!(page.fragment_position("b"), Some(Point::new(0.0, 50.0)));
    assert_eq!(page.fragment_position("nope"), None);
}

#[test]
fn blocker_filters_requests_and_hides_elements() {
    let engine = Engine::new(Arc::new(fetcher()), fonts());
    let (blocker, _) = wb_blocker::Engine::from_list("/ads/*\n##.ad\nsite.test###nope{");
    engine.set_content_blocker(Some(blocker));
    let mut page = engine
        .load(&url("https://site.test/dir/page.html"))
        .unwrap();
    assert_eq!(page.blocked_requests(), 1);
    let r = page.render(&RenderOptions::new(200.0, 300.0)).unwrap();
    // .ad is gone, so #bg moves up to y 116 and #tracked to 136, showing
    // its blue background without the blocked pixel.
    assert_eq!(px(&r, 10, 120), [0, 128, 0, 255]);
    assert_eq!(px(&r, 0, 136), [0, 0, 255, 255]);

    // Turning the blocker off for the site restores both.
    engine.set_blocker_disabled_for_host("site.test", true);
    assert!(engine.is_blocker_disabled_for_host("www.site.test"));
    let mut page = engine
        .load(&url("https://site.test/dir/page.html"))
        .unwrap();
    assert_eq!(page.blocked_requests(), 0);
    let r = page.render(&RenderOptions::new(200.0, 300.0)).unwrap();
    assert_eq!(px(&r, 10, 140), [0, 0, 0, 255]);
}

#[test]
fn document_filter_blocks_the_load() {
    let engine = Engine::new(Arc::new(fetcher()), fonts());
    let (blocker, _) = wb_blocker::Engine::from_list("||site.test^$document");
    engine.set_content_blocker(Some(blocker));
    let err = engine
        .load(&url("https://site.test/dir/page.html"))
        .unwrap_err();
    assert!(matches!(err, LoadError::Blocked { .. }), "{err:?}");
}

#[test]
fn scale_and_scroll() {
    let engine = Engine::new(Arc::new(fetcher()), fonts());
    let mut page = engine
        .load(&url("https://site.test/dir/page.html"))
        .unwrap();
    let r = page
        .render(&RenderOptions::new(100.0, 100.0).with_scale(2.0))
        .unwrap();
    assert_eq!((r.width(), r.height()), (200, 200));
    assert_eq!(px(&r, 99, 99), [255, 0, 0, 255]);
    assert_eq!(px(&r, 101, 99), [255, 255, 255, 255]);
    let r = page
        .render(&RenderOptions::new(100.0, 100.0).with_scroll(0.0, 50.0))
        .unwrap();
    assert_eq!(px(&r, 10, 10), [0, 255, 0, 255]);
}

#[test]
fn css_types_and_status_are_checked() {
    let mut f = LocalFetcher::new();
    f.insert(
        "https://x.test/",
        "text/html",
        "<!DOCTYPE html><link rel=stylesheet href=a.css><link rel=stylesheet href=b.css>\
         <style>body{margin:0}</style><div id=a></div><div id=b></div>",
    );
    // Wrong type in standards mode: ignored.
    f.insert(
        "https://x.test/a.css",
        "text/plain",
        "#a{height:10px;background:red}",
    );
    // Missing (404 in LocalFetcher): ignored.
    let engine = Engine::new(Arc::new(f), fonts());
    let mut page = engine.load(&url("https://x.test/")).unwrap();
    let r = page.render(&RenderOptions::new(20.0, 20.0)).unwrap();
    assert_eq!(px(&r, 5, 5), [255, 255, 255, 255]);
}

#[test]
fn quirks_mode_accepts_same_origin_css_of_any_type() {
    let mut f = LocalFetcher::new();
    f.insert(
        "https://x.test/",
        "text/html",
        "<link rel=stylesheet href=a.css><style>body{margin:0}</style><div id=a></div>",
    );
    f.insert(
        "https://x.test/a.css",
        "text/plain",
        "#a{height:10px;background:red}",
    );
    let engine = Engine::new(Arc::new(f), fonts());
    let mut page = engine.load(&url("https://x.test/")).unwrap();
    let r = page.render(&RenderOptions::new(20.0, 20.0)).unwrap();
    assert_eq!(px(&r, 5, 5), [255, 0, 0, 255]);
}

#[test]
fn css_decoding() {
    // windows-1252 é (0xE9) via the Content-Type charset, @charset, and the
    // environment encoding; a BOM wins over all of them.
    let bytes = b"a{content:'\xE9'}";
    assert!(load::decode_css(bytes, Some("text/css;charset=windows-1252"), "UTF-8").contains('é'));
    let bytes2 = b"@charset \"windows-1252\"; a{content:'\xE9'}";
    assert!(load::decode_css(bytes2, None, "UTF-8").contains('é'));
    assert!(load::decode_css(bytes, None, "windows-1252").contains('é'));
    assert!(load::decode_css(bytes, None, "UTF-8").contains('\u{FFFD}'));
    let bom = b"\xEF\xBB\xBFa{content:'\xC3\xA9'}";
    assert!(load::decode_css(bom, Some("text/css;charset=windows-1252"), "x").contains('é'));
    // @charset utf-16 means UTF-8.
    let u16 = b"@charset \"utf-16le\"; a{content:'\xC3\xA9'}";
    assert!(load::decode_css(u16, None, "windows-1252").contains('é'));
}

#[test]
fn base_url_and_title() {
    let engine = Engine::new(Arc::new(LocalFetcher::new()), fonts());
    let page = engine
        .load_html(
            url("https://a.test/x/y"),
            "<base href='/other/'><title>\tA\n b </title><title>2</title>",
        )
        .unwrap();
    assert_eq!(page.base_url().as_str(), "https://a.test/other/");
    assert_eq!(page.title(), "A b");
    assert_eq!(
        page.resolve(" z ").unwrap().as_str(),
        "https://a.test/other/z"
    );
}

#[test]
fn plain_text_and_image_documents() {
    let mut f = LocalFetcher::new();
    f.insert("https://a.test/t.txt", "text/plain", "<b>not html</b>");
    f.insert(
        "https://a.test/i",
        "image/png",
        solid_png(3, 3, [0, 0, 255, 255]),
    );
    f.insert("https://a.test/z.zip", "application/zip", "PK");
    let engine = Engine::new(Arc::new(f), fonts());
    let page = engine.load(&url("https://a.test/t.txt")).unwrap();
    let doc = page.document();
    assert_eq!(
        doc.text_content(doc.document_element().unwrap()).unwrap(),
        "<b>not html</b>"
    );
    let mut page = engine.load(&url("https://a.test/i")).unwrap();
    let r = page.render(&RenderOptions::new(10.0, 10.0)).unwrap();
    assert_eq!(px(&r, 1, 1), [0, 0, 255, 255]);
    assert_eq!(px(&r, 5, 5), [255, 255, 255, 255]);
    assert_eq!(
        engine.load(&url("https://a.test/z.zip")).unwrap_err(),
        LoadError::UnsupportedContentType("application/zip".into())
    );
}

#[test]
fn pending_images_after_viewport_change() {
    let mut f = LocalFetcher::new();
    f.insert(
        "https://a.test/",
        "text/html",
        "<!DOCTYPE html><style>body{margin:0}div{width:10px;height:10px}\
         @media (max-width: 300px) { div { background: url(small.png) } }</style><div></div>",
    );
    f.insert(
        "https://a.test/small.png",
        "image/png",
        solid_png(1, 1, [0, 255, 0, 255]),
    );
    let engine = Engine::new(Arc::new(f), fonts());
    let mut page = engine.load(&url("https://a.test/")).unwrap();
    let r = page.render(&RenderOptions::new(200.0, 100.0)).unwrap();
    assert_eq!(px(&r, 5, 5), [255, 255, 255, 255]);
    assert!(page.has_pending_images());
    assert!(engine.load_pending_images(&mut page, &NoObserver));
    assert!(!page.has_pending_images());
    let r = page.render(&RenderOptions::new(200.0, 100.0)).unwrap();
    assert_eq!(px(&r, 5, 5), [0, 255, 0, 255]);
}

#[test]
fn cancelled_loads_stop() {
    struct Cancel;
    impl LoadObserver for Cancel {
        fn cancelled(&self) -> bool {
            true
        }
    }
    let engine = Engine::new(Arc::new(fetcher()), fonts());
    let req = wb_net::Request::navigate(url("https://site.test/dir/page.html"));
    assert_eq!(
        engine.load_request(req, &Cancel).unwrap_err(),
        LoadError::Cancelled
    );
}

#[test]
fn png_round_trip() {
    let engine = Engine::new(Arc::new(fetcher()), fonts());
    let mut page = engine
        .load(&url("https://site.test/dir/page.html"))
        .unwrap();
    let r = page.render(&RenderOptions::new(60.0, 60.0)).unwrap();
    let png = encode_png(&r).unwrap();
    let decoded = wb_image::decode(&png).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (60, 60));
    assert_eq!(decoded.pixels(), r.pixels());
    assert_eq!(
        unpremultiply(&[64, 0, 0, 128, 0, 0, 0, 0]),
        [128, 0, 0, 128, 0, 0, 0, 0]
    );
}

#[test]
fn text_renders_with_system_fonts() {
    let fonts = Engine::system_fonts();
    if fonts.is_empty() {
        eprintln!("no system fonts; skipping");
        return;
    }
    let engine = Engine::new(Arc::new(LocalFetcher::new()), fonts);
    let mut page = engine
        .load_html(
            url("https://a.test/"),
            "<body style='margin:0;font-size:40px;color:#000'>WWWW",
        )
        .unwrap();
    let r = page.render(&RenderOptions::new(200.0, 60.0)).unwrap();
    let dark = r.pixels().chunks_exact(4).filter(|p| p[0] < 100).count();
    assert!(dark > 50, "{dark} dark pixels");
}
