//! Font discovery, CSS font matching, text shaping, font metrics, glyph
//! rasterization and line-break opportunities.
//!
//! Everything goes through a [`FontContext`]:
//!
//! - **Discovery**: [`FontContext::with_system_fonts`] finds installed fonts
//!   (no fonts are bundled).
//! - **Matching**: a [`FontQuery`] (the computed `font-family`,
//!   `font-weight`, `font-style` and `font-stretch`) selects faces per
//!   CSS Fonts 4 §5.2, with generic families mapped to installed fonts and
//!   per-character fallback across fonts.
//! - **Shaping**: [`FontContext::shape`] turns text and a [`TextStyle`]
//!   into [`ShapedText`]: runs of positioned glyphs with cluster offsets back
//!   into the text, which layout uses to measure, break and hit-test.
//! - **Metrics**: [`FontContext::metrics`] gives ascent, descent, line gap,
//!   x-height and decoration positions at a size.
//! - **Rasterization**: [`FontContext::rasterize_glyph`] renders a glyph to a
//!   cached 8-bit coverage mask for paint.
//! - **Line breaking**: [`break_opportunities`] (UAX #14).
//! - **Bidi**: [`paragraph_direction`] and [`reorder_visual`] (basic UAX #9).
//!
//! Results depend only on the text, the style and the set of installed
//! fonts, so measurements are reproducible for reference tests.
//!
//! The borrowed crates (`fontdb`, `harfrust`, `swash`, `unicode-*`) are
//! private: no type of theirs appears in this API.

mod bidi;
mod context;
mod linebreak;
mod matching;
mod metrics;
mod raster;
mod shape;
mod style;

pub use bidi::{Direction, paragraph_direction, reorder_visual};
pub use context::{FontContext, FontFaceInfo, FontId, FontLoadError};
pub use linebreak::{BreakOpportunity, break_opportunities};
pub use metrics::FontMetrics;
pub use raster::{GlyphMask, GlyphRequest, SUBPIXEL_BINS, SubpixelBin, subpixel_position};
pub use shape::{GlyphId, ShapedGlyph, ShapedRun, ShapedText, Synthesis, TextStyle};
pub use style::{FontFamily, FontQuery, FontStretch, FontStyle, FontWeight, GenericFamily};
