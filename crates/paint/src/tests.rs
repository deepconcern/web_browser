//! Pixel-exact rasterization tests and hit-test tests.

use std::sync::Arc;

use wb_image::Image;
use wb_text::{Direction, GlyphId, Synthesis};

use crate::test_support::{BlockGlyphs, NoGlyphs, font, font_id};
use crate::*;

const RED: Color = Color::rgb(255, 0, 0);
const GREEN: Color = Color::rgb(0, 255, 0);
const BLUE: Color = Color::rgb(0, 0, 255);
const YELLOW: Color = Color::rgb(255, 255, 0);
const CLEAR: [u8; 4] = [0, 0, 0, 0];

fn px(c: Color) -> [u8; 4] {
    [c.r, c.g, c.b, c.a]
}

fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::new(x, y, w, h)
}

fn render(list: &DisplayList, w: f32, h: f32, scale: f32) -> Raster {
    rasterize(
        list,
        &RasterOptions::new(r(0.0, 0.0, w, h), scale),
        &NoGlyphs,
    )
    .unwrap()
}

fn at(raster: &Raster, x: u32, y: u32) -> [u8; 4] {
    raster.pixel(x, y).unwrap()
}

fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

/// Asserts that exactly the device pixels in `inside` have `color` and all
/// others are clear.
fn assert_only(raster: &Raster, inside: (u32, u32, u32, u32), color: [u8; 4]) {
    let (x0, y0, x1, y1) = inside;
    for y in 0..raster.height() {
        for x in 0..raster.width() {
            let expected = if x >= x0 && x < x1 && y >= y0 && y < y1 {
                color
            } else {
                CLEAR
            };
            assert_eq!(at(raster, x, y), expected, "pixel ({x}, {y})");
        }
    }
}

fn border(rect: Rect, w: f32, style: BorderStyle, colors: [Color; 4]) -> DisplayItem {
    let side = |c: Color| BorderSide {
        width: w,
        style,
        color: c,
    };
    DisplayItem::Border(BorderItem {
        rect,
        radii: CornerRadii::default(),
        top: side(colors[0]),
        right: side(colors[1]),
        bottom: side(colors[2]),
        left: side(colors[3]),
        tag: Some(HitTag(9)),
    })
}

// ----- fills and snapping -----

#[test]
fn fill_rect_scale_1_and_2() {
    let mut list = DisplayList::new();
    list.push_rect(r(2.0, 3.0, 4.0, 2.0), RED, None);
    assert_only(&render(&list, 10.0, 10.0, 1.0), (2, 3, 6, 5), px(RED));
    assert_only(&render(&list, 10.0, 10.0, 2.0), (4, 6, 12, 10), px(RED));
}

#[test]
fn output_size_rounds_up() {
    let list = DisplayList::new();
    let raster = render(&list, 10.0, 7.0, 1.5);
    assert_eq!((raster.width(), raster.height()), (15, 11));
    assert_eq!(raster.pixels().len(), 15 * 11 * 4);
}

#[test]
fn background_and_viewport_origin() {
    let mut list = DisplayList::new();
    list.push_rect(r(100.0, 100.0, 2.0, 2.0), RED, None);
    let mut opts = RasterOptions::new(r(99.0, 99.0, 4.0, 4.0), 1.0);
    opts.background = Color::WHITE;
    let raster = rasterize(&list, &opts, &NoGlyphs).unwrap();
    assert_eq!(at(&raster, 0, 0), [255; 4]);
    assert_eq!(at(&raster, 1, 1), px(RED));
    assert_eq!(at(&raster, 2, 2), px(RED));
    assert_eq!(at(&raster, 3, 3), [255; 4]);
}

#[test]
fn adjacent_fractional_rects_share_edges() {
    // At 1.5x, 1.0 css px -> 1.5 device px: edges snap half-up.
    let mut list = DisplayList::new();
    list.push_rect(r(0.0, 0.0, 1.0, 2.0), RED, None);
    list.push_rect(r(1.0, 0.0, 1.0, 2.0), BLUE, None);
    let raster = render(&list, 2.0, 2.0, 1.5);
    assert_eq!(raster.width(), 3);
    // Edge at 1.5 snaps to 2: red covers 0..2, blue 2..3.
    assert_eq!(at(&raster, 0, 0), px(RED));
    assert_eq!(at(&raster, 1, 0), px(RED));
    assert_eq!(at(&raster, 2, 0), px(BLUE));
}

#[test]
fn translucent_fill_blends() {
    let mut list = DisplayList::new();
    list.push_rect(r(0.0, 0.0, 2.0, 2.0), Color::BLACK, None);
    list.push_rect(r(0.0, 0.0, 2.0, 2.0), Color::rgba(255, 255, 255, 128), None);
    let raster = render(&list, 2.0, 2.0, 1.0);
    assert!(
        close(at(&raster, 0, 0), [128, 128, 128, 255]),
        "{:?}",
        at(&raster, 0, 0)
    );
}

#[test]
fn rounded_fill() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::Fill(FillItem {
        shape: RoundedRect::new(
            r(0.0, 0.0, 20.0, 20.0),
            CornerRadii::uniform(Radius::circular(10.0)),
        ),
        color: RED,
        tag: None,
    }));
    for scale in [1.0, 2.0] {
        let raster = render(&list, 20.0, 20.0, scale);
        let s = scale as u32;
        assert_eq!(at(&raster, 0, 0), CLEAR);
        assert_eq!(at(&raster, 20 * s - 1, 20 * s - 1), CLEAR);
        assert_eq!(at(&raster, 10 * s, 10 * s), px(RED));
        assert_eq!(at(&raster, 10 * s, 0), px(RED));
        assert_eq!(at(&raster, 0, 10 * s), px(RED));
        // The curve is antialiased: some corner pixels are partly covered.
        let partial = (0..10 * s)
            .flat_map(|y| (0..10 * s).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let a = at(&raster, x, y)[3];
                a > 0 && a < 255
            })
            .count();
        assert!(partial > 3, "partial {partial}");
    }
}

// ----- borders -----

#[test]
fn solid_uniform_border() {
    let mut list = DisplayList::new();
    list.push(border(
        r(1.0, 1.0, 6.0, 6.0),
        1.0,
        BorderStyle::Solid,
        [RED; 4],
    ));
    let raster = render(&list, 8.0, 8.0, 1.0);
    for y in 0..8 {
        for x in 0..8 {
            let on_ring = (1..7).contains(&x)
                && (1..7).contains(&y)
                && (x == 1 || x == 6 || y == 1 || y == 6);
            let expected = if on_ring { px(RED) } else { CLEAR };
            assert_eq!(at(&raster, x, y), expected, "({x}, {y})");
        }
    }
    // At 2x the ring is 2 device px wide.
    let raster = render(&list, 8.0, 8.0, 2.0);
    assert_eq!(at(&raster, 2, 7), px(RED));
    assert_eq!(at(&raster, 3, 7), px(RED));
    assert_eq!(at(&raster, 4, 7), CLEAR);
}

#[test]
fn thin_border_snaps_to_one_device_pixel() {
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 4.0, 4.0),
        0.3,
        BorderStyle::Solid,
        [RED; 4],
    ));
    let raster = render(&list, 4.0, 4.0, 2.0);
    assert_eq!(at(&raster, 4, 0), px(RED));
    assert_eq!(at(&raster, 4, 1), CLEAR);
    assert_eq!(at(&raster, 0, 4), px(RED));
    assert_eq!(at(&raster, 1, 4), CLEAR);
}

#[test]
fn per_side_colors() {
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 12.0, 12.0),
        3.0,
        BorderStyle::Solid,
        [RED, GREEN, BLUE, YELLOW],
    ));
    for scale in [1.0f32, 2.0] {
        let raster = render(&list, 12.0, 12.0, scale);
        let s = |v: u32| v * scale as u32;
        assert_eq!(at(&raster, s(6), s(1)), px(RED));
        assert_eq!(at(&raster, s(10), s(6)), px(GREEN));
        assert_eq!(at(&raster, s(6), s(10)), px(BLUE));
        assert_eq!(at(&raster, s(1), s(6)), px(YELLOW));
        assert_eq!(at(&raster, s(6), s(6)), CLEAR);
        // Near the top-left corner, above the diagonal is top, below is
        // left.
        assert_eq!(at(&raster, s(2), s(0)), px(RED));
        assert_eq!(at(&raster, s(0), s(2)), px(YELLOW));
    }
}

#[test]
fn transparent_side_leaves_gap() {
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 10.0, 10.0),
        2.0,
        BorderStyle::Solid,
        [RED, Color::TRANSPARENT, RED, RED],
    ));
    let raster = render(&list, 10.0, 10.0, 1.0);
    assert_eq!(at(&raster, 9, 5), CLEAR);
    assert_eq!(at(&raster, 5, 0), px(RED));
}

#[test]
fn rounded_border() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::Border(BorderItem {
        rect: r(0.0, 0.0, 20.0, 20.0),
        radii: CornerRadii::uniform(Radius::circular(8.0)),
        top: BorderSide {
            width: 2.0,
            style: BorderStyle::Solid,
            color: RED,
        },
        right: BorderSide {
            width: 2.0,
            style: BorderStyle::Solid,
            color: RED,
        },
        bottom: BorderSide {
            width: 2.0,
            style: BorderStyle::Solid,
            color: RED,
        },
        left: BorderSide {
            width: 2.0,
            style: BorderStyle::Solid,
            color: RED,
        },
        tag: None,
    }));
    let raster = render(&list, 20.0, 20.0, 1.0);
    assert_eq!(at(&raster, 0, 0), CLEAR);
    assert_eq!(at(&raster, 10, 0), px(RED));
    assert_eq!(at(&raster, 10, 1), px(RED));
    assert_eq!(at(&raster, 10, 2), CLEAR);
    assert_eq!(at(&raster, 10, 10), CLEAR);
    // Inner corner: just inside the padding edge's curve is clear, the
    // ring at the diagonal is painted.
    assert_eq!(at(&raster, 4, 4), CLEAR);
    assert!(at(&raster, 2, 2)[3] > 0);
}

#[test]
fn double_border() {
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 12.0, 12.0),
        3.0,
        BorderStyle::Double,
        [RED; 4],
    ));
    let raster = render(&list, 12.0, 12.0, 1.0);
    assert_eq!(at(&raster, 6, 0), px(RED));
    assert_eq!(at(&raster, 6, 1), CLEAR);
    assert_eq!(at(&raster, 6, 2), px(RED));
    assert_eq!(at(&raster, 6, 3), CLEAR);
    // Too thin for double: solid.
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 12.0, 12.0),
        2.0,
        BorderStyle::Double,
        [RED; 4],
    ));
    let raster = render(&list, 12.0, 12.0, 1.0);
    assert_eq!(at(&raster, 6, 1), px(RED));
}

#[test]
fn dashed_and_dotted_borders_have_gaps() {
    for style in [BorderStyle::Dashed, BorderStyle::Dotted] {
        let mut list = DisplayList::new();
        list.push(border(r(0.0, 0.0, 40.0, 10.0), 2.0, style, [RED; 4]));
        let raster = render(&list, 40.0, 10.0, 1.0);
        let row: Vec<bool> = (0..40).map(|x| at(&raster, x, 0)[3] > 0).collect();
        let painted = row.iter().filter(|p| **p).count();
        assert!(painted > 10 && painted < 36, "{style:?}: {painted}");
        // Interior stays clear, and corners are painted.
        assert_eq!(at(&raster, 20, 5), CLEAR);
        assert!(at(&raster, 0, 0)[3] > 0 || at(&raster, 1, 1)[3] > 0);
    }
}

#[test]
fn other_styles_paint_solid_and_none_paints_nothing() {
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 6.0, 6.0),
        1.0,
        BorderStyle::Groove,
        [RED; 4],
    ));
    let raster = render(&list, 6.0, 6.0, 1.0);
    assert_eq!(at(&raster, 3, 0), px(RED));
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 6.0, 6.0),
        1.0,
        BorderStyle::None,
        [RED; 4],
    ));
    assert_only(&render(&list, 6.0, 6.0, 1.0), (0, 0, 0, 0), CLEAR);
}

// ----- clips -----

#[test]
fn rect_clip() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(r(2.0, 2.0, 3.0, 3.0).into()));
    list.push_rect(r(0.0, 0.0, 10.0, 10.0), RED, None);
    list.push(DisplayItem::PopClip);
    list.push_rect(r(8.0, 8.0, 2.0, 2.0), BLUE, None);
    let raster = render(&list, 10.0, 10.0, 2.0);
    assert_eq!(at(&raster, 3, 3), CLEAR);
    assert_eq!(at(&raster, 4, 4), px(RED));
    assert_eq!(at(&raster, 9, 9), px(RED));
    assert_eq!(at(&raster, 10, 10), CLEAR);
    assert_eq!(at(&raster, 16, 16), px(BLUE));
}

#[test]
fn nested_clips_intersect() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(r(0.0, 0.0, 6.0, 6.0).into()));
    list.push(DisplayItem::PushClip(r(4.0, 4.0, 6.0, 6.0).into()));
    list.push_rect(r(0.0, 0.0, 10.0, 10.0), RED, None);
    list.close_all();
    assert_only(&render(&list, 10.0, 10.0, 1.0), (4, 4, 6, 6), px(RED));
}

#[test]
fn rounded_clip_clips_everything() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(RoundedRect::new(
        r(0.0, 0.0, 20.0, 20.0),
        CornerRadii::uniform(Radius::circular(10.0)),
    )));
    list.push_rect(r(0.0, 0.0, 20.0, 20.0), RED, None);
    list.push(border(
        r(0.0, 0.0, 20.0, 20.0),
        1.0,
        BorderStyle::Solid,
        [BLUE; 4],
    ));
    list.push(DisplayItem::PopClip);
    let raster = render(&list, 20.0, 20.0, 1.0);
    assert_eq!(at(&raster, 0, 0), CLEAR);
    assert_eq!(at(&raster, 1, 1), CLEAR);
    assert_eq!(at(&raster, 10, 10), px(RED));
    assert_eq!(at(&raster, 10, 0), px(BLUE));
}

#[test]
fn rounded_clip_falls_back_to_rect_without_budget() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(RoundedRect::new(
        r(0.0, 0.0, 10.0, 10.0),
        CornerRadii::uniform(Radius::circular(5.0)),
    )));
    list.push_rect(r(0.0, 0.0, 20.0, 20.0), RED, None);
    list.close_all();
    let mut opts = RasterOptions::new(r(0.0, 0.0, 20.0, 20.0), 1.0);
    opts.limits.max_scratch_bytes = 0;
    let raster = rasterize(&list, &opts, &NoGlyphs).unwrap();
    assert_only(&raster, (0, 0, 10, 10), px(RED));
}

// ----- opacity -----

#[test]
fn opacity_group_composites_once() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushOpacity(0.5));
    list.push_rect(r(0.0, 0.0, 2.0, 1.0), RED, None);
    list.push_rect(r(1.0, 0.0, 2.0, 1.0), BLUE, None);
    list.push(DisplayItem::PopOpacity);
    let raster = render(&list, 3.0, 1.0, 1.0);
    assert!(
        close(at(&raster, 0, 0), [128, 0, 0, 128]),
        "{:?}",
        at(&raster, 0, 0)
    );
    // The overlap shows only blue: the group is flattened first.
    assert!(
        close(at(&raster, 1, 0), [0, 0, 128, 128]),
        "{:?}",
        at(&raster, 1, 0)
    );
    assert!(close(at(&raster, 2, 0), [0, 0, 128, 128]));
}

#[test]
fn nested_opacity_multiplies() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushOpacity(0.5));
    list.push(DisplayItem::PushOpacity(0.5));
    list.push_rect(r(0.0, 0.0, 1.0, 1.0), Color::WHITE, None);
    list.close_all();
    let raster = render(&list, 1.0, 1.0, 1.0);
    let p = at(&raster, 0, 0);
    assert!(p[3].abs_diff(64) <= 1, "{p:?}");
}

#[test]
fn opacity_without_layers_applies_per_item() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushOpacity(0.5));
    list.push_rect(r(0.0, 0.0, 2.0, 1.0), RED, None);
    list.push_rect(r(1.0, 0.0, 2.0, 1.0), BLUE, None);
    list.push(DisplayItem::PopOpacity);
    let mut opts = RasterOptions::new(r(0.0, 0.0, 3.0, 1.0), 1.0);
    opts.limits.max_layers = 0;
    let raster = rasterize(&list, &opts, &NoGlyphs).unwrap();
    // The overlap mixes both colors.
    let p = at(&raster, 1, 0);
    assert!(p[0] > 30 && p[2] > 100 && p[3] > 180, "{p:?}");
}

#[test]
fn opacity_zero_and_one() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushOpacity(0.0));
    list.push_rect(r(0.0, 0.0, 2.0, 2.0), RED, None);
    list.push(DisplayItem::PopOpacity);
    list.push(DisplayItem::PushOpacity(1.0));
    list.push_rect(r(2.0, 0.0, 2.0, 2.0), BLUE, None);
    list.push(DisplayItem::PopOpacity);
    assert_only(&render(&list, 4.0, 2.0, 1.0), (2, 0, 4, 2), px(BLUE));
}

#[test]
fn opacity_layer_inside_rounded_clip() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(RoundedRect::new(
        r(2.0, 2.0, 16.0, 16.0),
        CornerRadii::uniform(Radius::circular(8.0)),
    )));
    list.push(DisplayItem::PushOpacity(0.5));
    list.push_rect(r(0.0, 0.0, 20.0, 20.0), Color::WHITE, None);
    list.close_all();
    let raster = render(&list, 20.0, 20.0, 1.0);
    assert_eq!(at(&raster, 2, 2), CLEAR);
    assert_eq!(at(&raster, 1, 10), CLEAR);
    assert!(close(at(&raster, 10, 10), [128, 128, 128, 128]));
}

// ----- scroll offsets -----

#[test]
fn scroll_offset_moves_contents() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(r(0.0, 0.0, 10.0, 5.0).into()));
    list.push(DisplayItem::PushScroll { dx: 0, dy: -5 });
    list.push_rect(r(0.0, 7.0, 2.0, 2.0), RED, None);
    list.push(DisplayItem::PopScroll);
    list.push(DisplayItem::PopClip);
    list.push_rect(r(5.0, 7.0, 1.0, 1.0), BLUE, None);
    let raster = render(&list, 10.0, 10.0, 2.0);
    assert_eq!(at(&raster, 0, 4), px(RED));
    assert_eq!(at(&raster, 3, 7), px(RED));
    assert_eq!(at(&raster, 0, 14), CLEAR);
    assert_eq!(at(&raster, 10, 14), px(BLUE));
}

// ----- text -----

fn text_item(origin: Point, n: usize, color: Color) -> Option<DisplayItem> {
    let glyphs = (0..n)
        .map(|i| TextGlyph {
            id: GlyphId(1),
            x: 4.0 * i as f32,
            y: 0.0,
            advance: 4.0,
            cluster: i,
        })
        .collect();
    Some(DisplayItem::Text(TextItem {
        origin,
        font: font_id()?,
        size: 8.0,
        synthesis: Synthesis::default(),
        color,
        glyphs,
        bounds: r(origin.x, origin.y - 6.0, 4.0 * n as f32, 8.0),
        text_range: 0..n,
        direction: Direction::Ltr,
        tag: Some(HitTag(5)),
    }))
}

#[test]
fn text_glyph_masks_are_colored_and_positioned() {
    let Some(item) = text_item(Point::new(1.0, 6.0), 2, RED) else {
        return;
    };
    let mut list = DisplayList::new();
    list.push(item);
    let glyphs = BlockGlyphs { w: 3, h: 4, top: 4 };
    let raster = rasterize(
        &list,
        &RasterOptions::new(r(0.0, 0.0, 10.0, 8.0), 1.0),
        &glyphs,
    )
    .unwrap();
    for y in 0..8 {
        for x in 0..10 {
            let on = (2..6).contains(&y) && ((1..4).contains(&x) || (5..8).contains(&x));
            assert_eq!(
                at(&raster, x, y),
                if on { px(RED) } else { CLEAR },
                "({x}, {y})"
            );
        }
    }
    // At 2x everything doubles.
    let raster = rasterize(
        &list,
        &RasterOptions::new(r(0.0, 0.0, 10.0, 8.0), 2.0),
        &glyphs,
    )
    .unwrap();
    assert_eq!(at(&raster, 2, 4), px(RED));
    assert_eq!(at(&raster, 7, 11), px(RED));
    assert_eq!(at(&raster, 8, 4), CLEAR);
    assert_eq!(at(&raster, 2, 3), CLEAR);
}

#[test]
fn text_respects_clip_and_opacity() {
    let Some(item) = text_item(Point::new(0.0, 4.0), 2, Color::WHITE) else {
        return;
    };
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(r(0.0, 0.0, 2.0, 8.0).into()));
    list.push(DisplayItem::PushOpacity(0.5));
    list.push(item);
    list.close_all();
    let glyphs = BlockGlyphs { w: 4, h: 4, top: 4 };
    let raster = rasterize(
        &list,
        &RasterOptions::new(r(0.0, 0.0, 8.0, 8.0), 1.0),
        &glyphs,
    )
    .unwrap();
    assert!(
        close(at(&raster, 1, 1), [128, 128, 128, 128]),
        "{:?}",
        at(&raster, 1, 1)
    );
    assert_eq!(at(&raster, 2, 1), CLEAR);
    assert_eq!(at(&raster, 5, 1), CLEAR);
}

#[test]
fn text_with_a_real_font() {
    let Some((ctx, font)) = font() else { return };
    let shaped = ctx.shape(
        "Hi",
        &wb_text::TextStyle::new(
            wb_text::FontQuery::generic(wb_text::GenericFamily::SansSerif),
            16.0,
        ),
    );
    let Some(run) = shaped.runs.first() else {
        return;
    };
    let mut x = 0.0;
    let glyphs = run
        .glyphs
        .iter()
        .map(|g| {
            let tg = TextGlyph {
                id: g.id,
                x: x + g.x_offset,
                y: g.y_offset,
                advance: g.advance,
                cluster: g.cluster,
            };
            x += g.advance;
            tg
        })
        .collect();
    let mut list = DisplayList::new();
    list.push(DisplayItem::Text(TextItem {
        origin: Point::new(2.0, 16.0),
        font: run.font,
        size: 16.0,
        synthesis: run.synthesis,
        color: Color::BLACK,
        glyphs,
        bounds: r(2.0, 2.0, x, 18.0),
        text_range: 0..2,
        direction: Direction::Ltr,
        tag: None,
    }));
    let _ = font;
    let raster = rasterize(
        &list,
        &RasterOptions::new(r(0.0, 0.0, 40.0, 20.0), 1.0),
        ctx,
    )
    .unwrap();
    let inked = raster.pixels().chunks(4).filter(|p| p[3] > 128).count();
    assert!(inked > 20, "inked {inked}");
}

// ----- images -----

fn quad_image() -> Arc<Image> {
    // 2x2: red, green / blue, half-transparent white (premultiplied).
    let pixels = vec![
        255, 0, 0, 255, 0, 255, 0, 255, //
        0, 0, 255, 255, 128, 128, 128, 128,
    ];
    Arc::new(Image::from_premultiplied_rgba8(2, 2, pixels).unwrap())
}

#[test]
fn image_one_to_one_is_exact() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::Image(ImageItem {
        dest: r(1.0, 1.0, 2.0, 2.0),
        image: quad_image(),
        rendering: ImageRendering::Auto,
        tag: None,
    }));
    let raster = render(&list, 4.0, 4.0, 1.0);
    assert_eq!(at(&raster, 1, 1), px(RED));
    assert_eq!(at(&raster, 2, 1), px(GREEN));
    assert_eq!(at(&raster, 1, 2), px(BLUE));
    assert_eq!(at(&raster, 2, 2), [128, 128, 128, 128]);
    assert_eq!(at(&raster, 0, 0), CLEAR);
    assert_eq!(at(&raster, 3, 3), CLEAR);
}

#[test]
fn image_scaled_pixelated_and_smooth() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::Image(ImageItem {
        dest: r(0.0, 0.0, 2.0, 2.0),
        image: quad_image(),
        rendering: ImageRendering::Pixelated,
        tag: None,
    }));
    let raster = render(&list, 2.0, 2.0, 2.0);
    assert_eq!(at(&raster, 0, 0), px(RED));
    assert_eq!(at(&raster, 1, 1), px(RED));
    assert_eq!(at(&raster, 2, 0), px(GREEN));
    assert_eq!(at(&raster, 3, 3), [128, 128, 128, 128]);

    let mut list = DisplayList::new();
    list.push(DisplayItem::Image(ImageItem {
        dest: r(0.0, 0.0, 8.0, 8.0),
        image: quad_image(),
        rendering: ImageRendering::Auto,
        tag: None,
    }));
    let raster = render(&list, 8.0, 8.0, 1.0);
    // Bilinear: the middle mixes red and green.
    let p = at(&raster, 4, 0);
    assert!(p[0] > 40 && p[1] > 40, "{p:?}");
    assert_eq!(at(&raster, 0, 0), px(RED));
}

#[test]
fn background_image_repeat_x_inside_clip() {
    let one = Arc::new(Image::from_premultiplied_rgba8(1, 1, vec![0, 0, 255, 255]).unwrap());
    let mut list = DisplayList::new();
    list.push(DisplayItem::BackgroundImage(BackgroundImageItem {
        clip: r(1.0, 0.0, 8.0, 10.0).into(),
        tile: r(3.0, 2.0, 2.0, 2.0),
        repeat_x: true,
        repeat_y: false,
        image: one,
        rendering: ImageRendering::Auto,
        tag: None,
    }));
    assert_only(&render(&list, 10.0, 10.0, 1.0), (1, 2, 9, 4), px(BLUE));
}

#[test]
fn background_image_no_repeat_and_tiles() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::BackgroundImage(BackgroundImageItem {
        clip: r(0.0, 0.0, 8.0, 8.0).into(),
        tile: r(2.0, 2.0, 2.0, 2.0),
        repeat_x: false,
        repeat_y: false,
        image: quad_image(),
        rendering: ImageRendering::Auto,
        tag: None,
    }));
    let raster = render(&list, 8.0, 8.0, 1.0);
    assert_eq!(at(&raster, 2, 2), px(RED));
    assert_eq!(at(&raster, 3, 3), [128, 128, 128, 128]);
    assert_eq!(at(&raster, 4, 2), CLEAR);
    assert_eq!(at(&raster, 1, 2), CLEAR);

    let mut list = DisplayList::new();
    list.push(DisplayItem::BackgroundImage(BackgroundImageItem {
        clip: r(0.0, 0.0, 8.0, 8.0).into(),
        tile: r(2.0, 2.0, 2.0, 2.0),
        repeat_x: true,
        repeat_y: true,
        image: quad_image(),
        rendering: ImageRendering::Auto,
        tag: None,
    }));
    let raster = render(&list, 8.0, 8.0, 1.0);
    assert_eq!(at(&raster, 0, 0), px(RED));
    assert_eq!(at(&raster, 7, 6), px(GREEN));
    assert_eq!(at(&raster, 4, 5), px(BLUE));
}

// ----- repaint, limits and robustness -----

#[test]
fn repaint_only_touches_dirty_area() {
    let mut list = DisplayList::new();
    list.push_rect(r(0.0, 0.0, 10.0, 10.0), RED, None);
    let opts = RasterOptions::new(r(0.0, 0.0, 10.0, 10.0), 1.0);
    let mut raster = rasterize(&list, &opts, &NoGlyphs).unwrap();
    let mut list2 = DisplayList::new();
    list2.push_rect(r(0.0, 0.0, 10.0, 10.0), BLUE, None);
    repaint(&list2, &opts, &NoGlyphs, &mut raster, r(2.5, 2.5, 2.0, 2.0)).unwrap();
    assert_eq!(at(&raster, 2, 2), px(BLUE));
    assert_eq!(at(&raster, 4, 4), px(BLUE));
    assert_eq!(at(&raster, 5, 5), px(RED));
    assert_eq!(at(&raster, 1, 1), px(RED));
    let other = RasterOptions::new(r(0.0, 0.0, 5.0, 5.0), 1.0);
    assert_eq!(
        repaint(
            &list2,
            &other,
            &NoGlyphs,
            &mut raster,
            r(0.0, 0.0, 1.0, 1.0)
        ),
        Err(PaintError::SizeMismatch)
    );
}

#[test]
fn invalid_options_are_errors() {
    let list = DisplayList::new();
    let bad = |v: Rect, s: f32| rasterize(&list, &RasterOptions::new(v, s), &NoGlyphs).err();
    assert_eq!(
        bad(r(0.0, 0.0, 10.0, 10.0), 0.0),
        Some(PaintError::InvalidScale)
    );
    assert_eq!(
        bad(r(0.0, 0.0, 10.0, 10.0), f32::NAN),
        Some(PaintError::InvalidScale)
    );
    assert_eq!(
        bad(r(0.0, 0.0, 0.0, 10.0), 1.0),
        Some(PaintError::InvalidViewport)
    );
    assert_eq!(
        bad(r(0.0, 0.0, f32::INFINITY, 1.0), 1.0),
        Some(PaintError::InvalidViewport)
    );
    assert_eq!(
        bad(r(0.0, 0.0, 20_000.0, 1.0), 1.0),
        Some(PaintError::TooLarge)
    );
    assert_eq!(
        bad(r(0.0, 0.0, 10_000.0, 10_000.0), 1.0),
        Some(PaintError::TooLarge)
    );
}

#[test]
fn huge_and_offscreen_geometry_is_harmless() {
    let mut list = DisplayList::new();
    list.push_rect(r(-1.0e30, -1.0e30, 2.0e30, 2.0e30), RED, None);
    list.push(DisplayItem::Fill(FillItem {
        shape: RoundedRect::new(
            r(-1.0e12, 0.0, 2.0e12, 5.0),
            CornerRadii::uniform(Radius::circular(1.0e9)),
        ),
        color: BLUE,
        tag: None,
    }));
    list.push(border(
        r(-1.0e7, -1.0e7, 2.0e7, 2.0e7),
        1.0,
        BorderStyle::Dotted,
        [GREEN; 4],
    ));
    list.push(border(
        r(0.0, 0.0, 1.0e8, 3.0),
        1.0,
        BorderStyle::Dashed,
        [GREEN; 4],
    ));
    list.push(DisplayItem::PushClip(RoundedRect::new(
        r(-1.0e20, -1.0e20, 1.0e21, 1.0e21),
        CornerRadii::uniform(Radius::circular(1.0e19)),
    )));
    list.push(DisplayItem::PushScroll {
        dx: i32::MAX,
        dy: i32::MIN,
    });
    list.push_rect(r(0.0, 0.0, 1.0, 1.0), RED, None);
    list.close_all();
    let raster = render(&list, 32.0, 32.0, 2.0);
    assert_eq!(raster.width(), 64);
    assert_eq!(at(&raster, 10, 30), px(RED));
}

#[test]
fn deep_nesting_is_bounded() {
    let mut list = DisplayList::new();
    for i in 0..200 {
        if i % 2 == 0 {
            list.push(DisplayItem::PushOpacity(0.99));
        } else {
            list.push(DisplayItem::PushClip(RoundedRect::new(
                r(0.0, 0.0, 100.0, 100.0),
                CornerRadii::uniform(Radius::circular(5.0)),
            )));
        }
    }
    list.push_rect(r(0.0, 0.0, 100.0, 100.0), RED, None);
    list.close_all();
    assert!(list.len() <= ListLimits::default().max_depth * 2 + 1);
    // 100 opacity groups of 0.99: only 16 get layers, the rest are
    // flattened, and the result is the same product.
    let raster = render(&list, 100.0, 100.0, 1.0);
    let a = at(&raster, 50, 50)[3];
    assert!(a.abs_diff((0.99f32.powi(100) * 255.0) as u8) <= 3, "{a}");

    // Past the depth limit the group and its contents are dropped.
    let mut list = DisplayList::new();
    for _ in 0..10_000 {
        list.push(DisplayItem::PushClip(r(0.0, 0.0, 100.0, 100.0).into()));
    }
    assert!(!list.push_rect(r(0.0, 0.0, 100.0, 100.0), RED, None));
    list.close_all();
    assert_eq!(list.len(), 2 * ListLimits::default().max_depth);
}

// ----- hit testing -----

fn tagged(rect: Rect, tag: u64) -> DisplayItem {
    DisplayItem::Fill(FillItem {
        shape: rect.into(),
        color: RED,
        tag: Some(HitTag(tag)),
    })
}

fn tags(list: &DisplayList, x: f32, y: f32) -> Vec<u64> {
    hit_test(list, Point::new(x, y))
        .iter()
        .map(|h| h.tag.0)
        .collect()
}

#[test]
fn hit_order_is_topmost_first() {
    let mut list = DisplayList::new();
    list.push(tagged(r(0.0, 0.0, 10.0, 10.0), 1));
    list.push_rect(r(0.0, 0.0, 10.0, 10.0), BLUE, None);
    list.push(tagged(r(5.0, 5.0, 10.0, 10.0), 2));
    assert_eq!(tags(&list, 7.0, 7.0), vec![2, 1]);
    assert_eq!(tags(&list, 1.0, 1.0), vec![1]);
    assert_eq!(tags(&list, 12.0, 12.0), vec![2]);
    assert_eq!(tags(&list, 20.0, 20.0), Vec::<u64>::new());
    assert_eq!(
        hit_test_topmost(&list, Point::new(7.0, 7.0)).map(|h| h.item),
        Some(2)
    );
    assert!(hit_test(&list, Point::new(f32::NAN, 1.0)).is_empty());
}

#[test]
fn hit_respects_clips_and_rounded_corners() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushClip(r(0.0, 0.0, 5.0, 5.0).into()));
    list.push(tagged(r(0.0, 0.0, 10.0, 10.0), 1));
    list.push(DisplayItem::PopClip);
    list.push(DisplayItem::Fill(FillItem {
        shape: RoundedRect::new(
            r(20.0, 0.0, 10.0, 10.0),
            CornerRadii::uniform(Radius::circular(5.0)),
        ),
        color: RED,
        tag: Some(HitTag(2)),
    }));
    list.push(DisplayItem::PushClip(RoundedRect::new(
        r(40.0, 0.0, 10.0, 10.0),
        CornerRadii::uniform(Radius::circular(5.0)),
    )));
    list.push(DisplayItem::HitRegion {
        shape: r(40.0, 0.0, 10.0, 10.0).into(),
        tag: HitTag(3),
    });
    list.close_all();
    assert_eq!(tags(&list, 2.0, 2.0), vec![1]);
    assert_eq!(tags(&list, 7.0, 7.0), Vec::<u64>::new());
    assert_eq!(tags(&list, 20.5, 0.5), Vec::<u64>::new());
    assert_eq!(tags(&list, 25.0, 5.0), vec![2]);
    assert_eq!(tags(&list, 40.5, 0.5), Vec::<u64>::new());
    assert_eq!(tags(&list, 45.0, 5.0), vec![3]);
}

#[test]
fn hit_with_scroll_and_hidden_opacity() {
    let mut list = DisplayList::new();
    list.push(DisplayItem::PushScroll { dx: 0, dy: -100 });
    list.push(DisplayItem::PushOpacity(0.0));
    list.push(tagged(r(0.0, 100.0, 10.0, 10.0), 1));
    list.close_all();
    assert_eq!(tags(&list, 5.0, 5.0), vec![1]);
    assert_eq!(tags(&list, 5.0, 105.0), Vec::<u64>::new());
}

#[test]
fn hit_border_ring_only() {
    let mut list = DisplayList::new();
    list.push(border(
        r(0.0, 0.0, 10.0, 10.0),
        2.0,
        BorderStyle::Solid,
        [RED; 4],
    ));
    assert_eq!(tags(&list, 1.0, 5.0), vec![9]);
    assert_eq!(tags(&list, 5.0, 5.0), Vec::<u64>::new());
}

#[test]
fn hit_text_gives_offset() {
    let Some(item) = text_item(Point::new(10.0, 20.0), 3, RED) else {
        return;
    };
    let mut list = DisplayList::new();
    list.push(item);
    let hit = hit_test_topmost(&list, Point::new(15.0, 18.0)).unwrap();
    assert_eq!(hit.tag, HitTag(5));
    assert_eq!(hit.text_offset, Some(1));
    let hit = hit_test_topmost(&list, Point::new(21.5, 18.0)).unwrap();
    assert_eq!(hit.text_offset, Some(3));
    assert!(hit_test_topmost(&list, Point::new(15.0, 30.0)).is_none());
}
