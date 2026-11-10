//! Tests that need real fonts. They use whatever the machine has installed
//! (CI's ubuntu-latest has DejaVu) and skip, with a note on stderr, when
//! no font is found.

use std::sync::{Arc, OnceLock};

use wb_text::{
    Direction, FontContext, FontFamily, FontQuery, FontStyle, FontWeight, GenericFamily, GlyphId,
    GlyphRequest, ShapedText, SubpixelBin, Synthesis, TextStyle, break_opportunities,
    subpixel_position,
};

fn ctx() -> Option<&'static FontContext> {
    static CTX: OnceLock<FontContext> = OnceLock::new();
    let ctx = CTX.get_or_init(FontContext::with_system_fonts);
    if ctx.is_empty() {
        eprintln!("no system fonts found; skipping font-dependent test");
        None
    } else {
        Some(ctx)
    }
}

fn has_family(ctx: &FontContext, name: &str) -> bool {
    let q = FontQuery::new(FontFamily::Named(name.to_owned()));
    ctx.match_families(&q)
        .first()
        .and_then(|f| ctx.face_info(*f))
        .is_some_and(|i| i.family.eq_ignore_ascii_case(name))
}

fn sans(size: f32) -> TextStyle {
    TextStyle::new(FontQuery::generic(GenericFamily::SansSerif), size)
}

fn assert_clusters_valid(text: &str, shaped: &ShapedText) {
    for run in &shaped.runs {
        assert!(text.is_char_boundary(run.range.start));
        assert!(text.is_char_boundary(run.range.end));
        for g in &run.glyphs {
            assert!(
                run.range.contains(&g.cluster),
                "{g:?} outside {:?}",
                run.range
            );
            assert!(text.is_char_boundary(g.cluster));
        }
        let clusters: Vec<usize> = run.glyphs.iter().map(|g| g.cluster).collect();
        let mut sorted = clusters.clone();
        match run.direction {
            Direction::Ltr => sorted.sort_unstable(),
            Direction::Rtl => sorted.sort_unstable_by(|a, b| b.cmp(a)),
        }
        assert_eq!(clusters, sorted, "clusters not monotonic in visual order");
    }
}

#[test]
fn discovers_fonts_and_maps_generics() {
    let Some(ctx) = ctx() else { return };
    assert!(ctx.face_count() > 0);
    for g in [
        GenericFamily::Serif,
        GenericFamily::SansSerif,
        GenericFamily::Monospace,
        GenericFamily::Cursive,
        GenericFamily::Fantasy,
        GenericFamily::SystemUi,
    ] {
        assert!(ctx.generic_family_name(g).is_some(), "{g:?} unmapped");
        assert!(ctx.first_available_font(&FontQuery::generic(g)).is_some());
    }
}

#[test]
fn monospace_is_monospaced() {
    let Some(ctx) = ctx() else { return };
    let style = TextStyle::new(FontQuery::generic(GenericFamily::Monospace), 16.0);
    let shaped = ctx.shape("iiWW..", &style);
    let advances: Vec<f32> = shaped
        .runs
        .iter()
        .flat_map(|r| r.glyphs.iter().map(|g| g.advance))
        .collect();
    assert_eq!(advances.len(), 6);
    let first = advances.first().copied().unwrap_or(0.0);
    assert!(first > 0.0);
    assert!(
        advances.iter().all(|a| (a - first).abs() < 1e-4),
        "{advances:?}"
    );
}

#[test]
fn family_names_match_case_insensitively_and_by_weight() {
    let Some(ctx) = ctx() else { return };
    if !has_family(ctx, "DejaVu Sans") {
        eprintln!("DejaVu Sans not installed; skipping");
        return;
    }
    let mut q = FontQuery::new(FontFamily::Named("dEJAVU sANS".to_owned()));
    let regular = ctx.match_families(&q);
    let info = regular.first().and_then(|f| ctx.face_info(*f));
    assert_eq!(
        info.as_ref().map(|i| i.family.as_str()),
        Some("DejaVu Sans")
    );
    assert_eq!(info.map(|i| i.style), Some(FontStyle::Normal));

    q.weight = FontWeight::BOLD;
    let bold = ctx.match_families(&q);
    let info = bold.first().and_then(|f| ctx.face_info(*f));
    assert_eq!(info.map(|i| i.weight), Some(FontWeight::BOLD));

    // A missing family is skipped; the next one is used.
    let q = FontQuery {
        families: vec![
            FontFamily::Named("No Such Family 12345".to_owned()),
            FontFamily::Named("DejaVu Sans".to_owned()),
        ],
        ..FontQuery::default()
    };
    let first = ctx
        .match_families(&q)
        .first()
        .and_then(|f| ctx.face_info(*f));
    assert_eq!(first.map(|i| i.family), Some("DejaVu Sans".to_owned()));
}

#[test]
fn shapes_latin_with_clusters() {
    let Some(ctx) = ctx() else { return };
    let text = "Hello, world";
    let shaped = ctx.shape(text, &sans(16.0));
    assert_eq!(shaped.base_direction, Direction::Ltr);
    assert!(!shaped.runs.is_empty());
    assert_clusters_valid(text, &shaped);
    assert!(shaped.width() > 0.0);
    let sum: f32 = shaped.runs.iter().map(|r| r.advance).sum();
    assert!((sum - shaped.width()).abs() < 1e-3);
    assert!((shaped.advance_for_range(0..text.len()) - shaped.width()).abs() < 1e-3);
    // "Hello," then " world": the parts add up to the whole.
    let a = shaped.advance_for_range(0..6);
    let b = shaped.advance_for_range(6..text.len());
    assert!((a + b - shaped.width()).abs() < 1e-3);
}

#[test]
fn width_scales_with_size() {
    let Some(ctx) = ctx() else { return };
    let w16 = ctx.measure("The quick brown fox", &sans(16.0));
    let w32 = ctx.measure("The quick brown fox", &sans(32.0));
    assert!(w16 > 0.0);
    assert!((w32 - 2.0 * w16).abs() < 1e-2, "{w16} {w32}");
}

#[test]
fn shaping_is_deterministic_across_contexts() {
    let Some(ctx) = ctx() else { return };
    let other = FontContext::with_system_fonts();
    let text = "Determinism: ﬁne ligatures, 日本語, שלום, 123.";
    let a = ctx.shape(text, &sans(13.0));
    let b = other.shape(text, &sans(13.0));
    assert_eq!(a, b);
    assert_eq!(a, ctx.shape(text, &sans(13.0)));
}

#[test]
fn shape_range_keeps_absolute_offsets() {
    let Some(ctx) = ctx() else { return };
    let text = "first second third";
    let part = ctx.shape_range(text, 6..12, &sans(16.0));
    assert_clusters_valid(text, &part);
    let clusters: Vec<usize> = part
        .runs
        .iter()
        .flat_map(|r| r.glyphs.iter().map(|g| g.cluster))
        .collect();
    assert_eq!(clusters.first(), Some(&6));
    assert!(clusters.iter().all(|c| (6..12).contains(c)));
    let whole = ctx.shape(text, &sans(16.0));
    let expect = whole.advance_for_range(6..12);
    assert!((part.width() - expect).abs() < 0.5);
}

#[test]
fn rtl_runs_are_visual_order() {
    let Some(ctx) = ctx() else { return };
    let text = "abc שלום def";
    let shaped = ctx.shape(text, &sans(16.0));
    assert_eq!(shaped.base_direction, Direction::Ltr);
    assert_clusters_valid(text, &shaped);
    let hebrew = shaped
        .runs
        .iter()
        .find(|r| r.direction == Direction::Rtl)
        .map(|r| r.level);
    assert_eq!(hebrew, Some(1));
    let shaped = ctx.shape("שלום", &sans(16.0));
    assert_eq!(shaped.base_direction, Direction::Rtl);
    let forced = ctx.shape(
        "abc",
        &TextStyle {
            direction: Some(Direction::Rtl),
            ..sans(16.0)
        },
    );
    assert_eq!(forced.base_direction, Direction::Rtl);
    assert!(forced.runs.iter().all(|r| r.level == 2));
}

#[test]
fn falls_back_per_character() {
    let Some(ctx) = ctx() else { return };
    let text = "a中ж😀ש";
    let shaped = ctx.shape(text, &sans(16.0));
    assert_clusters_valid(text, &shaped);
    let font_of = |offset: usize| {
        shaped
            .runs
            .iter()
            .find(|r| r.range.contains(&offset))
            .map(|r| r.font)
    };
    let mut must_cover = vec![('a', 0)];
    if has_family(ctx, "DejaVu Sans") {
        must_cover.push(('ж', text.find('ж').unwrap_or(0)));
        must_cover.push(('ש', text.find('ש').unwrap_or(0)));
    }
    if has_family(ctx, "WenQuanYi Zen Hei") || has_family(ctx, "Noto Sans CJK SC") {
        must_cover.push(('中', 1));
    }
    for (c, offset) in must_cover {
        let font = font_of(offset);
        assert!(
            font.is_some_and(|f| ctx.has_glyph(f, c)),
            "{c:?} got a face without it: {:?}",
            font.and_then(|f| ctx.face_info(f))
        );
    }
    // The Latin letter stays in the matched sans-serif face.
    let first = ctx.first_available_font(&FontQuery::generic(GenericFamily::SansSerif));
    assert_eq!(shaped.runs.first().map(|r| r.font), first);
    // A CJK character not in the primary face gets a different face.
    if let (Some(primary), Some(cjk_run)) = (first, shaped.runs.iter().find(|r| r.range.start == 1))
        && !ctx.has_glyph(primary, '中')
        && ctx.has_glyph(cjk_run.font, '中')
    {
        assert_ne!(cjk_run.font, primary);
    }
}

#[test]
fn combining_marks_stay_with_base() {
    let Some(ctx) = ctx() else { return };
    let text = "e\u{301}x";
    let shaped = ctx.shape(text, &sans(16.0));
    assert_clusters_valid(text, &shaped);
    assert_eq!(shaped.runs.len(), 1);
}

#[test]
fn metrics_are_sane_and_scale() {
    let Some(ctx) = ctx() else { return };
    let Some(font) = ctx.first_available_font(&FontQuery::generic(GenericFamily::SansSerif)) else {
        return;
    };
    let Some(m) = ctx.metrics(font, 16.0) else {
        panic!("metrics missing");
    };
    assert!(m.ascent > 0.0 && m.ascent < 32.0, "{m:?}");
    assert!(m.descent > 0.0 && m.descent < 16.0, "{m:?}");
    assert!(m.line_gap >= 0.0);
    assert!(m.x_height > 0.0 && m.x_height < m.ascent);
    assert!(m.cap_height > m.x_height);
    assert!(
        m.underline_offset > 0.0,
        "underline should be below baseline: {m:?}"
    );
    assert!(m.stroke_thickness > 0.0);
    assert!(m.normal_line_height() > 16.0 * 0.9);
    let Some(m2) = ctx.metrics(font, 32.0) else {
        panic!("metrics missing");
    };
    assert!((m2.ascent - 2.0 * m.ascent).abs() < 1e-3);
    assert_eq!(m.units_per_em, m2.units_per_em);
}

#[test]
fn rasterizes_and_caches_glyphs() {
    let Some(ctx) = ctx() else { return };
    let shaped = ctx.shape("H ", &sans(16.0));
    let Some(run) = shaped.runs.first() else {
        panic!("no runs");
    };
    let Some(h) = run.glyphs.first() else {
        panic!("no glyphs");
    };
    let req = GlyphRequest {
        font: run.font,
        glyph: h.id,
        size: 16.0,
        scale: 2.0,
        subpixel: SubpixelBin::new(0),
        synthesis: Synthesis::default(),
    };
    let Some(mask) = ctx.rasterize_glyph(&req) else {
        panic!("no mask");
    };
    assert!(!mask.is_empty());
    assert_eq!(
        mask.data.len() as u64,
        u64::from(mask.width) * u64::from(mask.height)
    );
    // At 32 device px a capital H is roughly 20-26 px tall.
    assert!(mask.height >= 15 && mask.height <= 30, "{mask:?}");
    assert!(mask.top > 0);
    assert!(mask.data.contains(&255));
    // Cached: same Arc comes back.
    let again = ctx.rasterize_glyph(&req);
    assert!(again.is_some_and(|m| Arc::ptr_eq(&m, &mask)));
    // A different subpixel offset is a different entry.
    let shifted = ctx.rasterize_glyph(&GlyphRequest {
        subpixel: SubpixelBin::new(2),
        ..req
    });
    assert!(shifted.is_some_and(|m| !Arc::ptr_eq(&m, &mask)));
    // Faux bold is heavier.
    let bold = ctx.rasterize_glyph(&GlyphRequest {
        synthesis: Synthesis {
            bold: true,
            oblique: false,
        },
        ..req
    });
    let ink = |m: &wb_text::GlyphMask| m.data.iter().map(|v| u64::from(*v)).sum::<u64>();
    assert!(bold.is_some_and(|b| ink(&b) > ink(&mask)));
    // Faux oblique leans right, so the mask is wider.
    let oblique = ctx.rasterize_glyph(&GlyphRequest {
        synthesis: Synthesis {
            bold: false,
            oblique: true,
        },
        ..req
    });
    assert!(oblique.is_some_and(|o| o.width > mask.width));
    // The space has no ink.
    if let Some(space) = run.glyphs.get(1) {
        let m = ctx.rasterize_glyph(&GlyphRequest {
            glyph: space.id,
            ..req
        });
        assert!(m.is_none_or(|m| m.is_empty() || m.data.iter().all(|v| *v == 0)));
    }
    assert!(ctx.glyph_cache_len() >= 4);
    // Unknown glyph ids do not panic.
    let _ = ctx.rasterize_glyph(&GlyphRequest {
        glyph: GlyphId(u16::MAX),
        ..req
    });
    let (x, bin) = subpixel_position(10.3);
    assert_eq!((x, bin.get()), (10, 1));
}

#[test]
fn synthesis_flags_on_runs() {
    let Some(ctx) = ctx() else { return };
    if !has_family(ctx, "DejaVu Sans Mono") {
        return;
    }
    // DejaVu Sans Mono has bold but its heaviest is 700; 900 maps to bold
    // without synthesis. It has oblique faces, so italic needs none either.
    let q = FontQuery {
        families: vec![FontFamily::Named("DejaVu Sans Mono".to_owned())],
        weight: FontWeight::new(900),
        style: FontStyle::Italic,
        ..FontQuery::default()
    };
    let shaped = ctx.shape("x", &TextStyle::new(q, 16.0));
    let run = shaped.runs.first();
    assert_eq!(run.map(|r| r.synthesis), Some(Synthesis::default()));
}

#[test]
fn explicit_font_file_loading() {
    let path = std::path::Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf");
    if !path.exists() {
        eprintln!("DejaVuSans.ttf not at the Debian path; skipping");
        return;
    }
    let mut ctx = FontContext::new();
    assert_eq!(ctx.load_font_file(path), Ok(1));
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    assert_eq!(ctx.load_font_data(bytes), Ok(1));
    assert_eq!(ctx.face_count(), 2);
    // With only DejaVu Sans, every generic maps to it.
    assert_eq!(
        ctx.generic_family_name(GenericFamily::Serif),
        Some("DejaVu Sans")
    );
    let w = ctx.measure("abc", &sans(10.0));
    assert!(w > 0.0);
    // A character no loaded font covers still produces a (notdef) glyph.
    let shaped = ctx.shape("\u{10FFFD}", &sans(10.0));
    assert_eq!(shaped.glyph_count(), 1);
}

#[test]
fn break_opportunities_line_up_with_clusters() {
    let Some(ctx) = ctx() else { return };
    let text = "Lorem ipsum dolor sit amet";
    let shaped = ctx.shape(text, &sans(16.0));
    let mut start = 0;
    let mut total = 0.0;
    for b in break_opportunities(text) {
        total += shaped.advance_for_range(start..b.offset);
        start = b.offset;
    }
    assert!((total - shaped.width()).abs() < 1e-3);
}
