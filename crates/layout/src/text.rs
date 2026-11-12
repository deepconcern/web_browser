//! The seam between layout and `wb-text`: shaping and font metrics for a
//! computed style.
//!
//! Layout talks to text through the private [`TextEngine`] trait so unit
//! tests can use a deterministic fake with fixed advances; production uses
//! [`wb_text::FontContext`].

use wb_style::ComputedStyle;
use wb_text::{FontContext, FontQuery, FontStretch, FontWeight, ShapedRun, TextStyle};

use crate::geom::sane_non_negative;

/// The largest font size layout shapes at, in CSS px. Larger computed
/// sizes are clamped (keeps glyph metrics finite and bounded).
pub(crate) const MAX_FONT_SIZE: f32 = 10_000.0;

/// Font-wide metrics layout needs, in CSS px, for one style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Metrics {
    /// Baseline to the top of the em box ascent.
    pub ascent: f32,
    /// Baseline to the bottom of the descent (positive).
    pub descent: f32,
    /// The line gap.
    pub line_gap: f32,
    /// The x-height.
    pub x_height: f32,
}

impl Metrics {
    /// The used `line-height` for a style with these metrics (CSS Inline 3
    /// §5.1: `normal` uses the font's ascent + descent + line gap).
    pub(crate) fn line_height(&self, style: &ComputedStyle) -> f32 {
        let size = font_size(style);
        let v = match style.line_height {
            wb_style::LineHeight::Normal => self.ascent + self.descent + self.line_gap,
            other => other.to_px(size, 1.2),
        };
        sane_non_negative(v)
    }
}

/// Shaped text: the glyph runs plus the advance of every cluster, which is
/// what line breaking measures.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shaped {
    /// Glyph runs in logical order (empty without fonts).
    pub runs: Vec<ShapedRun>,
    /// `(cluster start byte, advance)` sorted by offset, one entry per
    /// cluster.
    pub clusters: Vec<(usize, f32)>,
}

/// Shaping and metrics.
pub(crate) trait TextEngine {
    /// Shapes `text` in `style`.
    fn shape(&self, text: &str, style: &ComputedStyle) -> Shaped;
    /// The primary font's metrics for `style`.
    fn metrics(&self, style: &ComputedStyle) -> Metrics;
}

/// The computed font size, clamped.
pub(crate) fn font_size(style: &ComputedStyle) -> f32 {
    sane_non_negative(style.font_size).min(MAX_FONT_SIZE)
}

/// The `wb-text` style for a computed style.
pub(crate) fn text_style(style: &ComputedStyle) -> TextStyle {
    use wb_style::GenericFamily as G;
    use wb_text::GenericFamily as T;
    let families = style
        .font_family
        .iter()
        .map(|f| match f {
            wb_style::FontFamily::Named(n) => wb_text::FontFamily::Named(n.clone()),
            wb_style::FontFamily::Generic(g) => wb_text::FontFamily::Generic(match g {
                G::Serif | G::UiSerif | G::Math | G::Fangsong => T::Serif,
                G::SansSerif | G::UiSansSerif | G::UiRounded | G::Emoji => T::SansSerif,
                G::Monospace | G::UiMonospace => T::Monospace,
                G::Cursive => T::Cursive,
                G::Fantasy => T::Fantasy,
                G::SystemUi => T::SystemUi,
            }),
        })
        .collect();
    let weight = if style.font_weight.is_finite() {
        // Clamped to 1..=1000 by `FontWeight::new`.
        FontWeight::new(style.font_weight.clamp(1.0, 1000.0).round() as u16)
    } else {
        FontWeight::NORMAL
    };
    let font_style = match style.font_style {
        wb_style::FontStyle::Normal => wb_text::FontStyle::Normal,
        wb_style::FontStyle::Italic => wb_text::FontStyle::Italic,
        wb_style::FontStyle::Oblique(_) => wb_text::FontStyle::Oblique,
    };
    let mut ts = TextStyle::new(
        FontQuery {
            families,
            weight,
            style: font_style,
            stretch: FontStretch::from_percentage(style.font_stretch),
        },
        font_size(style),
    );
    // M1 follow-up: the `direction` property and bidi reordering; layout
    // is left-to-right in M1.
    ts.direction = Some(wb_text::Direction::Ltr);
    ts
}

impl TextEngine for FontContext {
    fn shape(&self, text: &str, style: &ComputedStyle) -> Shaped {
        let shaped = FontContext::shape(self, text, &text_style(style));
        let mut clusters: Vec<(usize, f32)> = shaped
            .runs
            .iter()
            .flat_map(|r| r.glyphs.iter().map(|g| (g.cluster, g.advance)))
            .collect();
        clusters.sort_by_key(|(c, _)| *c);
        let mut merged: Vec<(usize, f32)> = Vec::with_capacity(clusters.len());
        for (c, a) in clusters {
            match merged.last_mut() {
                Some((lc, la)) if *lc == c => *la += sane_non_negative(a),
                _ => merged.push((c, sane_non_negative(a))),
            }
        }
        Shaped {
            runs: shaped.runs,
            clusters: merged,
        }
    }

    fn metrics(&self, style: &ComputedStyle) -> Metrics {
        let ts = text_style(style);
        let size = ts.size;
        let m = self
            .first_available_font(&ts.font)
            .and_then(|f| FontContext::metrics(self, f, size));
        match m {
            Some(m) => Metrics {
                ascent: sane_non_negative(m.ascent),
                descent: sane_non_negative(m.descent),
                line_gap: sane_non_negative(m.line_gap),
                x_height: sane_non_negative(m.x_height),
            },
            None => fallback_metrics(size),
        }
    }
}

/// Metrics used when no font is available.
pub(crate) fn fallback_metrics(size: f32) -> Metrics {
    Metrics {
        ascent: size * 0.8,
        descent: size * 0.2,
        line_gap: 0.0,
        x_height: size * 0.5,
    }
}

/// A deterministic engine for tests: every character advances half the
/// font size (zero-width characters and line feeds advance 0); ascent is
/// 0.8em and descent 0.2em.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FakeText;

#[cfg(test)]
impl TextEngine for FakeText {
    fn shape(&self, text: &str, style: &ComputedStyle) -> Shaped {
        let size = font_size(style);
        let clusters = text
            .char_indices()
            .map(|(i, c)| {
                let w = if matches!(c, '\u{200B}' | '\n' | '\r') {
                    0.0
                } else {
                    size * 0.5
                };
                (i, w)
            })
            .collect();
        Shaped {
            runs: Vec::new(),
            clusters,
        }
    }

    fn metrics(&self, style: &ComputedStyle) -> Metrics {
        fallback_metrics(font_size(style))
    }
}
