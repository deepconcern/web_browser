//! Font-wide metrics at a given size.

use crate::context::{FontContext, FontId};

/// Font-wide metrics scaled to a font size, in CSS px.
///
/// All distances are positive in their natural direction, as noted on
/// each field, so layout never has to remember OpenType sign conventions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMetrics {
    /// The font size these metrics are scaled to (the em size), in px.
    pub size: f32,
    /// Font design units per em.
    pub units_per_em: u16,
    /// Distance from the baseline up to the top of the ascent.
    pub ascent: f32,
    /// Distance from the baseline down to the bottom of the descent.
    pub descent: f32,
    /// Recommended extra space between lines (the line gap).
    pub line_gap: f32,
    /// Height of a lowercase `x` above the baseline. CSS Values 4 §6.1.1:
    /// when the font has none, 0.5em is used.
    pub x_height: f32,
    /// Height of a capital letter above the baseline (0.7em when the font
    /// has none).
    pub cap_height: f32,
    /// Distance from the baseline *down* to the underline, from the font's
    /// `post.underlinePosition` (OpenType says it is the stroke's top edge,
    /// though many fonts treat it as the centre). Can be zero or negative
    /// for unusual fonts.
    pub underline_offset: f32,
    /// Distance from the baseline *up* to the top of a line-through.
    pub strikeout_offset: f32,
    /// Thickness of underline and line-through strokes.
    pub stroke_thickness: f32,
}

impl FontMetrics {
    /// The default (`line-height: normal`) line height: ascent + descent +
    /// line gap (CSS Inline 3 §5).
    pub fn normal_line_height(&self) -> f32 {
        self.ascent + self.descent + self.line_gap
    }
}

impl FontContext {
    /// Font-wide metrics for `font` at `size` CSS px. Returns `None` for an
    /// unknown handle or an unreadable font file.
    pub fn metrics(&self, font: FontId, size: f32) -> Option<FontMetrics> {
        let (bytes, index) = self.face_data(font)?;
        let data: &[u8] = (*bytes).as_ref();
        let font_ref = swash::FontRef::from_index(data, index as usize)?;
        let raw = font_ref.metrics(&[]);
        let size = if size.is_finite() && size > 0.0 {
            size
        } else {
            0.0
        };
        let m = raw.scale(size);
        let positive = |v: f32, fallback: f32| {
            if v > 0.0 && v.is_finite() {
                v
            } else {
                fallback
            }
        };
        Some(FontMetrics {
            size,
            units_per_em: raw.units_per_em,
            ascent: m.ascent,
            descent: m.descent.abs(),
            line_gap: m.leading.max(0.0),
            x_height: positive(m.x_height, size * 0.5),
            cap_height: positive(m.cap_height, size * 0.7),
            // OpenType `post.underlinePosition` is negative below the
            // baseline.
            underline_offset: -m.underline_offset,
            strikeout_offset: positive(m.strikeout_offset, size * 0.3),
            stroke_thickness: positive(m.stroke_size, size / 14.0),
        })
    }
}
