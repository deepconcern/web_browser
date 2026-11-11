//! Shared test helpers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use wb_text::{FontContext, FontId, FontQuery, GenericFamily, GlyphMask, GlyphRequest};

use crate::GlyphRasterizer;

/// A font context holding one system font, and its id. `None` (with a
/// note on stderr) when the machine has no fonts; font-dependent tests
/// then skip, like wb-text's.
pub(crate) fn font() -> Option<&'static (FontContext, FontId)> {
    static FONT: OnceLock<Option<(FontContext, FontId)>> = OnceLock::new();
    let f = FONT
        .get_or_init(|| {
            let mut ctx = FontContext::new();
            for dir in ["/usr/share/fonts", "/usr/local/share/fonts"] {
                if let Some(path) = find_font(Path::new(dir), 0)
                    && ctx.load_font_file(&path).is_ok()
                {
                    break;
                }
            }
            let id = ctx.first_available_font(&FontQuery::generic(GenericFamily::SansSerif))?;
            Some((ctx, id))
        })
        .as_ref();
    if f.is_none() {
        eprintln!("no font found; skipping font-dependent test");
    }
    f
}

pub(crate) fn font_id() -> Option<FontId> {
    font().map(|f| f.1)
}

fn find_font(dir: &Path, depth: u32) -> Option<PathBuf> {
    if depth > 6 {
        return None;
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in &entries {
        if p.extension().is_some_and(|e| e == "ttf" || e == "otf") {
            return Some(p.clone());
        }
    }
    entries
        .iter()
        .filter(|p| p.is_dir())
        .find_map(|p| find_font(p, depth + 1))
}

/// A rasterizer that draws every glyph as a solid `w` x `h` block whose
/// top-left corner is `top` px above the baseline at the pen position.
#[derive(Debug)]
pub(crate) struct BlockGlyphs {
    pub w: u32,
    pub h: u32,
    pub top: i32,
}

impl GlyphRasterizer for BlockGlyphs {
    fn rasterize_glyph(&self, req: &GlyphRequest) -> Option<Arc<GlyphMask>> {
        // Scale the block with the device scale so tests can check 2x.
        let s = req.scale.round().max(1.0) as u32;
        let (w, h) = (self.w * s, self.h * s);
        Some(Arc::new(GlyphMask {
            width: w,
            height: h,
            left: 0,
            top: self.top * s as i32,
            data: vec![255; (w * h) as usize],
        }))
    }
}

/// A rasterizer with no glyphs.
#[derive(Debug)]
pub(crate) struct NoGlyphs;

impl GlyphRasterizer for NoGlyphs {
    fn rasterize_glyph(&self, _: &GlyphRequest) -> Option<Arc<GlyphMask>> {
        None
    }
}
