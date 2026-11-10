//! Glyph rasterization to 8-bit coverage masks, with a cache.
//!
//! Outlines are scaled and rendered by the `swash` crate (kept private)
//! without hinting, so that glyph shapes and the advances that layout used
//! agree at every size. The cache key is (font, glyph, device size,
//! horizontal subpixel bin, synthesis).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::{Angle, Format, Transform, Vector};

use crate::context::{FontContext, FontId, lock};
use crate::shape::{GlyphId, Synthesis};

/// Number of horizontal subpixel positions a glyph is rendered at.
pub const SUBPIXEL_BINS: u8 = 4;

/// The CSS default angle for `oblique` (CSS Fonts 4 §2.4), used for
/// synthesized obliques.
const SYNTHETIC_OBLIQUE_DEGREES: f32 = 14.0;

/// Entries kept before the cache is emptied. Simple and deterministic; a
/// least-recently-used policy is an M1 follow-up.
const MAX_CACHE_ENTRIES: usize = 16_384;

/// A horizontal subpixel position, in quarters of a device pixel
/// (`0..SUBPIXEL_BINS`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SubpixelBin(u8);

impl SubpixelBin {
    /// Creates a bin; values wrap modulo [`SUBPIXEL_BINS`].
    pub fn new(bin: u8) -> Self {
        Self(bin % SUBPIXEL_BINS)
    }

    /// The bin index.
    pub fn get(self) -> u8 {
        self.0
    }

    /// The offset this bin stands for, in device pixels (`0.0..1.0`).
    pub fn offset(self) -> f32 {
        f32::from(self.0) / f32::from(SUBPIXEL_BINS)
    }
}

/// Splits a device-pixel x coordinate into a whole pixel and a subpixel
/// bin, rounding to the nearest bin. Paint draws the mask returned for that
/// bin at `whole + mask.left`.
pub fn subpixel_position(x: f32) -> (i32, SubpixelBin) {
    if !x.is_finite() {
        return (0, SubpixelBin(0));
    }
    let bins = f32::from(SUBPIXEL_BINS);
    let q = (f64::from(x) * f64::from(bins)).round();
    let q = q.clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    // `q` is integral and within i32 range after the clamp.
    let q = q as i64;
    let n = i64::from(SUBPIXEL_BINS);
    let whole = i32::try_from(q.div_euclid(n)).unwrap_or(0);
    let bin = u8::try_from(q.rem_euclid(n)).unwrap_or(0);
    (whole, SubpixelBin(bin))
}

/// What to rasterize.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphRequest {
    /// The face.
    pub font: FontId,
    /// The glyph in that face.
    pub glyph: GlyphId,
    /// Font size in CSS px.
    pub size: f32,
    /// Device pixels per CSS px.
    pub scale: f32,
    /// Horizontal subpixel position (see [`subpixel_position`]).
    pub subpixel: SubpixelBin,
    /// Faux bold / oblique to apply.
    pub synthesis: Synthesis,
}

/// A rendered glyph: an 8-bit coverage (alpha) mask, one byte per pixel,
/// rows top to bottom.
#[derive(Clone, PartialEq, Eq)]
pub struct GlyphMask {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// Offset of the mask's left edge from the glyph origin, in device px.
    pub left: i32,
    /// Distance from the baseline *up* to the mask's top edge, in device
    /// px. The mask's top row is drawn at `baseline_y - top`.
    pub top: i32,
    /// `width * height` coverage values.
    pub data: Vec<u8>,
}

impl fmt::Debug for GlyphMask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlyphMask")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("left", &self.left)
            .field("top", &self.top)
            .finish_non_exhaustive()
    }
}

impl GlyphMask {
    /// Whether the mask has no pixels (for example a space).
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The coverage at (`x`, `y`), or 0 outside the mask.
    pub fn coverage(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        let i = u64::from(y) * u64::from(self.width) + u64::from(x);
        usize::try_from(i)
            .ok()
            .and_then(|i| self.data.get(i).copied())
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    font: FontId,
    glyph: u16,
    /// Device size in 1/64 px.
    size: u32,
    subpixel: u8,
    bold: bool,
    oblique: bool,
}

pub(crate) struct GlyphCache {
    scaler: ScaleContext,
    masks: HashMap<CacheKey, Option<Arc<GlyphMask>>>,
}

impl GlyphCache {
    pub(crate) fn new() -> Self {
        Self {
            scaler: ScaleContext::new(),
            masks: HashMap::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.masks.clear();
    }
}

fn device_size_64ths(size: f32, scale: f32) -> u32 {
    let px = size * scale;
    if !px.is_finite() || px <= 0.0 {
        return 0;
    }
    let v = (px * 64.0).round();
    if v >= u32::MAX as f32 {
        u32::MAX
    } else {
        v as u32
    }
}

impl FontContext {
    /// Rasterizes a glyph to a grayscale coverage mask, from the cache when
    /// possible. Returns `None` for an unknown font, an unreadable file, or
    /// a glyph with no outline (such as a color-bitmap-only emoji; color
    /// glyphs are an M1 follow-up).
    pub fn rasterize_glyph(&self, request: &GlyphRequest) -> Option<Arc<GlyphMask>> {
        let key = CacheKey {
            font: request.font,
            glyph: request.glyph.0,
            size: device_size_64ths(request.size, request.scale),
            subpixel: request.subpixel.get(),
            bold: request.synthesis.bold,
            oblique: request.synthesis.oblique,
        };
        if let Some(hit) = lock(&self.glyphs).masks.get(&key) {
            return hit.clone();
        }
        let (bytes, index) = self.face_data(request.font)?;
        let mut cache = lock(&self.glyphs);
        let mask = render(&mut cache.scaler, (*bytes).as_ref(), index, key).map(Arc::new);
        if cache.masks.len() >= MAX_CACHE_ENTRIES {
            cache.masks.clear();
        }
        cache.masks.insert(key, mask.clone());
        mask
    }

    /// The number of glyph masks currently cached.
    pub fn glyph_cache_len(&self) -> usize {
        lock(&self.glyphs).masks.len()
    }
}

fn render(scaler: &mut ScaleContext, data: &[u8], index: u32, key: CacheKey) -> Option<GlyphMask> {
    let font = swash::FontRef::from_index(data, index as usize)?;
    let px = key.size as f32 / 64.0;
    let mut s = scaler.builder(font).size(px).hint(false).build();
    let sources = [
        Source::Outline,
        Source::Bitmap(swash::scale::StrikeWith::ExactSize),
    ];
    let mut render = Render::new(&sources);
    render
        .format(Format::Alpha)
        .offset(Vector::new(SubpixelBin::new(key.subpixel).offset(), 0.0));
    if key.bold {
        // Faux bold: widen strokes by about 1/32 em on each side.
        render.embolden(px / 32.0);
    }
    if key.oblique {
        render.transform(Some(Transform::skew(
            Angle::from_degrees(SYNTHETIC_OBLIQUE_DEGREES),
            Angle::ZERO,
        )));
    }
    let image = render.render(&mut s, key.glyph)?;
    if image.content != swash::scale::image::Content::Mask {
        return None;
    }
    let p = image.placement;
    let expected = u64::from(p.width) * u64::from(p.height);
    if image.data.len() as u64 != expected {
        return None;
    }
    Some(GlyphMask {
        width: p.width,
        height: p.height,
        left: p.left,
        top: p.top,
        data: image.data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subpixel_rounding() {
        assert_eq!(subpixel_position(0.0), (0, SubpixelBin(0)));
        assert_eq!(subpixel_position(10.24), (10, SubpixelBin(1)));
        assert_eq!(subpixel_position(10.5), (10, SubpixelBin(2)));
        assert_eq!(subpixel_position(10.9), (11, SubpixelBin(0)));
        assert_eq!(subpixel_position(-0.25), (-1, SubpixelBin(3)));
        assert_eq!(subpixel_position(f32::NAN), (0, SubpixelBin(0)));
        assert_eq!(subpixel_position(f32::INFINITY), (0, SubpixelBin(0)));
        assert_eq!(subpixel_position(1e30).0, i32::MAX / 4);
    }

    #[test]
    fn bins_wrap() {
        assert_eq!(SubpixelBin::new(5).get(), 1);
        assert!((SubpixelBin::new(2).offset() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn device_size_quantization() {
        assert_eq!(device_size_64ths(16.0, 1.0), 1024);
        assert_eq!(device_size_64ths(16.0, 2.0), 2048);
        assert_eq!(device_size_64ths(-1.0, 1.0), 0);
        assert_eq!(device_size_64ths(f32::NAN, 1.0), 0);
    }

    #[test]
    fn mask_coverage_bounds() {
        let m = GlyphMask {
            width: 2,
            height: 1,
            left: 0,
            top: 0,
            data: vec![10, 20],
        };
        assert_eq!(m.coverage(1, 0), 20);
        assert_eq!(m.coverage(2, 0), 0);
        assert_eq!(m.coverage(0, 1), 0);
        assert!(!m.is_empty());
    }
}
