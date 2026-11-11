//! Text shaping: itemizing text into runs of one font, script and bidi
//! level, shaping each run with `harfrust` (kept private), and mapping
//! glyphs back to byte offsets.

use std::ops::Range;
use std::sync::Arc;

use unicode_script::{Script, UnicodeScript};

use crate::bidi::{Direction, TextAnalysis};
use crate::context::{FontContext, FontId, attrs_of, lock};
use crate::matching::FaceAttrs;
use crate::style::{FontQuery, FontStyle};

/// A glyph index within one font face.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlyphId(pub u16);

/// Faux styles applied when the matched face lacks the requested weight or
/// slant (CSS Fonts 4 §2.8, `font-synthesis`). Paint passes this through to
/// [`FontContext::rasterize_glyph`].
///
/// M1 follow-up: synthetic bold does not widen advances yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Synthesis {
    /// Draw strokes thicker.
    pub bold: bool,
    /// Slant the glyphs by the default oblique angle (14 degrees).
    pub oblique: bool,
}

/// How to shape a piece of text: the computed font properties plus the
/// size, direction and language.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Which fonts to use.
    pub font: FontQuery,
    /// The computed `font-size`, in CSS px.
    pub size: f32,
    /// The paragraph's base direction (CSS `direction`). `None` detects it
    /// from the text (UAX #9 P2/P3), defaulting to left-to-right.
    pub direction: Option<Direction>,
    /// The content language as a BCP 47 tag (from `lang`), used for
    /// language-specific glyph forms.
    pub language: Option<String>,
}

impl TextStyle {
    /// A style with the given font query and size, automatic direction
    /// and no language.
    pub fn new(font: FontQuery, size: f32) -> Self {
        Self {
            font,
            size,
            direction: None,
            language: None,
        }
    }
}

/// One positioned glyph. Distances are in CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    /// The glyph in the run's font.
    pub id: GlyphId,
    /// Byte offset in the shaped text of the first character of the
    /// cluster this glyph belongs to. Several glyphs may share a cluster
    /// (a decomposed character) and one glyph may stand for several
    /// characters (a ligature): its cluster then spans up to the next
    /// cluster offset in logical order.
    pub cluster: usize,
    /// How far the pen moves after this glyph.
    pub advance: f32,
    /// Horizontal offset of the glyph from the pen position.
    pub x_offset: f32,
    /// Vertical offset of the glyph from the baseline, positive *down*.
    pub y_offset: f32,
    /// `true` when breaking a line before this glyph's cluster and
    /// shaping the two halves separately would give different glyphs (for
    /// example inside a ligature or a joined Arabic word). Layout should
    /// reshape the line pieces in that case.
    pub unsafe_to_break: bool,
}

/// A sequence of glyphs from one font, in one script and one bidi level.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedRun {
    /// The face the glyphs come from.
    pub font: FontId,
    /// The font size in CSS px.
    pub size: f32,
    /// The byte range of the text this run covers.
    pub range: Range<usize>,
    /// The UAX #9 embedding level.
    pub level: u8,
    /// The run direction (from the level's parity).
    pub direction: Direction,
    /// Faux bold/oblique to apply when drawing.
    pub synthesis: Synthesis,
    /// The glyphs in *visual* order (left to right), so right-to-left runs
    /// have decreasing cluster offsets.
    pub glyphs: Vec<ShapedGlyph>,
    /// The total advance (width) of the run.
    pub advance: f32,
}

impl ShapedRun {
    /// The caret x position (from the run's left edge) before the
    /// character at byte `offset`. Offsets inside a cluster snap to the
    /// cluster's start.
    pub fn x_for_offset(&self, offset: usize) -> f32 {
        let take = |g: &&ShapedGlyph| match self.direction {
            Direction::Ltr => g.cluster < offset,
            Direction::Rtl => g.cluster >= offset,
        };
        self.glyphs.iter().filter(take).map(|g| g.advance).sum()
    }

    /// Hit testing: the byte offset of the caret position closest to `x`
    /// (measured from the run's left edge). Clicking the left half of a
    /// left-to-right cluster gives its start; the right half gives its end.
    pub fn offset_for_x(&self, x: f32) -> usize {
        // Group glyphs into clusters in visual order.
        let mut clusters: Vec<(usize, f32, f32)> = Vec::new();
        let mut pen = 0.0;
        for g in &self.glyphs {
            match clusters.last_mut() {
                Some((c, _, w)) if *c == g.cluster => *w += g.advance,
                _ => clusters.push((g.cluster, pen, g.advance)),
            }
            pen += g.advance;
        }
        let end_of = |cluster: usize| {
            clusters
                .iter()
                .map(|(c, _, _)| *c)
                .filter(|c| *c > cluster)
                .min()
                .unwrap_or(self.range.end)
        };
        let ltr = self.direction == Direction::Ltr;
        for (cluster, start, width) in &clusters {
            if x < start + width {
                let left_half = x < start + width / 2.0;
                return if left_half == ltr {
                    *cluster
                } else {
                    end_of(*cluster)
                };
            }
        }
        if ltr {
            self.range.end
        } else {
            self.range.start
        }
    }
}

/// The result of shaping: runs in *logical* order. Layout reorders the
/// runs of each line with [`crate::reorder_visual`].
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedText {
    /// The runs, in logical (text) order.
    pub runs: Vec<ShapedRun>,
    /// The paragraph's base direction.
    pub base_direction: Direction,
}

impl ShapedText {
    /// The total advance of all runs.
    pub fn width(&self) -> f32 {
        self.runs.iter().map(|r| r.advance).sum()
    }

    /// The advance of the glyphs whose cluster starts inside `range`.
    /// Layout uses this to measure candidate lines between break
    /// opportunities without reshaping (exact unless a glyph is
    /// [`ShapedGlyph::unsafe_to_break`] at a range edge).
    pub fn advance_for_range(&self, range: Range<usize>) -> f32 {
        self.runs
            .iter()
            .filter(|r| r.range.start < range.end && range.start < r.range.end)
            .flat_map(|r| r.glyphs.iter())
            .filter(|g| range.contains(&g.cluster))
            .map(|g| g.advance)
            .sum()
    }

    /// The total number of glyphs.
    pub fn glyph_count(&self) -> usize {
        self.runs.iter().map(|r| r.glyphs.len()).sum()
    }
}

/// Characters that never pick a font of their own: they join the font of
/// the character before them.
fn joins_previous(c: char) -> bool {
    c.is_control()
        || matches!(u32::from(c),
            0x200B..=0x200F   // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | 0x202A..=0x202E // bidi embedding controls
            | 0x2060..=0x2069 // word joiner, bidi isolates
            | 0xFE00..=0xFE0F // variation selectors
            | 0xFEFF
            | 0x1F3FB..=0x1F3FF // emoji skin-tone modifiers
            | 0xE0000..=0xE0FFF) // tags, variation selectors supplement
}

/// Characters that belong to the cluster of the character before them:
/// combining marks (Script=Inherited) and the joiners and selectors of
/// [`joins_previous`], but not line or tab controls.
fn extends_cluster(c: char) -> bool {
    !c.is_control() && (joins_previous(c) || c.script() == Script::Inherited)
}

fn is_real_script(s: Script) -> bool {
    !matches!(s, Script::Common | Script::Inherited | Script::Unknown)
}

/// Resolves Common and Inherited characters to the script of the
/// surrounding text (UAX #24 §5.2, simplified: previous, else next).
fn resolve_scripts(chars: &[(usize, char)]) -> Vec<Script> {
    let raw: Vec<Script> = chars.iter().map(|(_, c)| c.script()).collect();
    let mut out = raw.clone();
    let mut last: Option<Script> = None;
    for (i, s) in raw.iter().enumerate() {
        if is_real_script(*s) {
            last = Some(*s);
        } else if let (Some(l), Some(slot)) = (last, out.get_mut(i)) {
            *slot = l;
        }
    }
    let mut next: Option<Script> = None;
    for (i, s) in raw.iter().enumerate().rev() {
        if is_real_script(*s) {
            next = Some(*s);
        } else if let Some(slot) = out.get_mut(i)
            && !is_real_script(*slot)
        {
            *slot = next.unwrap_or(Script::Common);
        }
    }
    out
}

fn to_hb_script(s: Script) -> Option<harfrust::Script> {
    let name = s.short_name().as_bytes();
    let tag: [u8; 4] = name.try_into().ok()?;
    harfrust::Script::from_iso15924_tag(harfrust::Tag::new(&tag))
}

/// The last few characters before `at` (shaping context).
fn pre_context(text: &str, at: usize) -> &str {
    let Some(before) = text.get(..at) else {
        return "";
    };
    let start = before
        .char_indices()
        .rev()
        .nth(4)
        .map(|(i, _)| i)
        .unwrap_or(0);
    before.get(start..).unwrap_or("")
}

/// The first few characters from `at` on (shaping context).
fn post_context(text: &str, at: usize) -> &str {
    let Some(after) = text.get(at..) else {
        return "";
    };
    let end = after
        .char_indices()
        .nth(5)
        .map(|(i, _)| i)
        .unwrap_or(after.len());
    after.get(..end).unwrap_or("")
}

fn synthesis_for(want: FaceAttrs, have: Option<FaceAttrs>) -> Synthesis {
    let Some(have) = have else {
        return Synthesis::default();
    };
    Synthesis {
        bold: want.weight.get() >= 600 && have.weight.get() <= 500,
        oblique: want.style != FontStyle::Normal && have.style == FontStyle::Normal,
    }
}

struct Item {
    range: Range<usize>,
    font: FontId,
    script: Script,
    level: u8,
}

impl FontContext {
    /// Shapes `text` with `style`. See [`FontContext::shape_range`].
    pub fn shape(&self, text: &str, style: &TextStyle) -> ShapedText {
        self.shape_range(text, 0..text.len(), style)
    }

    /// Shapes the part of `text` in `range`, using the rest of `text` as
    /// context for bidi levels and contextual glyph forms. Cluster offsets
    /// in the result are offsets into the whole `text`. Layout shapes a
    /// paragraph once, then reshapes single lines with this.
    ///
    /// Each character gets the first face from the family list that has a
    /// glyph for it, else a system fallback face (CSS Fonts 4 §5.2);
    /// combining marks and joiners stay with the preceding character's
    /// face. The output is deterministic for a given set of fonts.
    ///
    /// A range that is out of bounds or not on `char` boundaries is
    /// clamped and shrunk to the nearest boundaries inside it.
    pub fn shape_range(&self, text: &str, range: Range<usize>, style: &TextStyle) -> ShapedText {
        let analysis = TextAnalysis::new(text, style.direction);
        self.shape_analyzed(text, &analysis, range, style)
    }

    /// Like [`FontContext::shape_range`], but reuses a [`TextAnalysis`] of
    /// the whole `text` instead of recomputing it, so shaping a paragraph
    /// line by line stays linear. The analysis decides the direction
    /// (`style.direction` is not consulted). An analysis of a different
    /// length than `text` is ignored and recomputed.
    pub fn shape_analyzed(
        &self,
        text: &str,
        analysis: &TextAnalysis,
        range: Range<usize>,
        style: &TextStyle,
    ) -> ShapedText {
        let recomputed;
        let analysis = if analysis.len() == text.len() {
            analysis
        } else {
            recomputed = TextAnalysis::new(text, style.direction);
            &recomputed
        };
        let bidi = &analysis.bidi;
        let base_direction = bidi.base;
        let range = clamp_range(text, range);
        let Some(slice) = text.get(range.clone()) else {
            return ShapedText {
                runs: Vec::new(),
                base_direction,
            };
        };
        let chars: Vec<(usize, char)> = slice
            .char_indices()
            .map(|(i, c)| (i + range.start, c))
            .collect();
        let scripts = resolve_scripts(&chars);
        let want = attrs_of(&style.font);
        let list = self.match_families(&style.font);

        let mut items: Vec<Item> = Vec::new();
        let mut prev_font: Option<FontId> = None;
        for (i, (offset, c)) in chars.iter().enumerate() {
            let script = scripts.get(i).copied().unwrap_or(Script::Common);
            let font = if prev_font.is_some() && extends_cluster(*c) {
                // Marks and joiners use the face chosen for their cluster.
                prev_font
            } else if joins_previous(*c) {
                prev_font.or_else(|| list.first().copied())
            } else {
                let rest = chars.get(i + 1..).unwrap_or(&[]);
                let marks = rest
                    .iter()
                    .map(|(_, m)| *m)
                    .take_while(|m| extends_cluster(*m));
                self.font_for_cluster(&list, *c, marks, want)
            };
            let Some(font) = font else { continue };
            prev_font = Some(font);
            let level = bidi.levels.get(*offset).copied().unwrap_or(0);
            let end = offset + c.len_utf8();
            match items.last_mut() {
                Some(it)
                    if it.font == font
                        && it.script == script
                        && it.level == level
                        && it.range.end == *offset =>
                {
                    it.range.end = end;
                }
                _ => items.push(Item {
                    range: *offset..end,
                    font,
                    script,
                    level,
                }),
            }
        }

        let size = if style.size.is_finite() && style.size > 0.0 {
            style.size
        } else {
            0.0
        };
        let language = style
            .language
            .as_deref()
            .and_then(|l| l.parse::<harfrust::Language>().ok());
        let runs = items
            .into_iter()
            .map(|item| {
                let synthesis = synthesis_for(want, self.face_attrs(item.font));
                self.shape_item(text, item, size, language.as_ref(), synthesis)
            })
            .collect();
        ShapedText {
            runs,
            base_direction,
        }
    }

    /// Chooses one face for a base character and the combining marks that
    /// follow it, so a cluster is not split across faces: the first face in
    /// the family list that covers the whole cluster, else a system
    /// fallback face that does, else the base character's own face
    /// (CSS Fonts 4 §5.2 steps 5-7 applied to the cluster).
    fn font_for_cluster(
        &self,
        list: &[FontId],
        base: char,
        marks: impl Iterator<Item = char>,
        want: FaceAttrs,
    ) -> Option<FontId> {
        let required: Vec<char> = marks.filter(|m| !joins_previous(*m)).collect();
        if required.is_empty() {
            return self.font_for_char(list, base, want, base.script());
        }
        let covers =
            |f: FontId| self.has_glyph(f, base) && required.iter().all(|m| self.has_glyph(f, *m));
        list.iter()
            .copied()
            .find(|f| covers(*f))
            .or_else(|| {
                self.system_fallback(base, want, base.script())
                    .filter(|f| covers(*f))
            })
            .or_else(|| self.font_for_char(list, base, want, base.script()))
    }

    /// Measures the advance width of `text` (convenience for
    /// `shape(text, style).width()`).
    pub fn measure(&self, text: &str, style: &TextStyle) -> f32 {
        self.shape(text, style).width()
    }

    fn shape_item(
        &self,
        text: &str,
        item: Item,
        size: f32,
        language: Option<&harfrust::Language>,
        synthesis: Synthesis,
    ) -> ShapedRun {
        let direction = Direction::from_level(item.level);
        let mut run = ShapedRun {
            font: item.font,
            size,
            range: item.range.clone(),
            level: item.level,
            direction,
            synthesis,
            glyphs: Vec::new(),
            advance: 0.0,
        };
        let Some((bytes, index)) = self.face_data(item.font) else {
            return run;
        };
        let data: &[u8] = (*bytes).as_ref();
        let Ok(font) = harfrust::FontRef::from_index(data, index) else {
            return run;
        };
        let cached = lock(&self.shapers).get(&item.font).cloned();
        let shaper_data = match cached {
            Some(d) => d,
            None => {
                let d = Arc::new(harfrust::ShaperData::new(&font));
                lock(&self.shapers).insert(item.font, d.clone());
                d
            }
        };
        let shaper = shaper_data.shaper(&font).build();
        let Some(slice) = text.get(item.range.clone()) else {
            return run;
        };
        let mut buffer = harfrust::UnicodeBuffer::new();
        for (i, c) in slice.char_indices() {
            let cluster = u32::try_from(item.range.start + i).unwrap_or(u32::MAX);
            buffer.add(c, cluster);
        }
        buffer.set_pre_context(pre_context(text, item.range.start));
        buffer.set_post_context(post_context(text, item.range.end));
        buffer.set_direction(match direction {
            Direction::Ltr => harfrust::Direction::LeftToRight,
            Direction::Rtl => harfrust::Direction::RightToLeft,
        });
        if let Some(script) = to_hb_script(item.script) {
            buffer.set_script(script);
        }
        if let Some(lang) = language {
            buffer.set_language(lang.clone());
        }
        let output = shaper.shape(buffer, harfrust::ShapeOptions::new());
        let upem = shaper.units_per_em().max(1) as f32;
        let scale = size / upem;
        let to_px = |v: i32| v as f32 * scale;
        for (info, pos) in output
            .glyph_infos()
            .iter()
            .zip(output.glyph_positions().iter())
        {
            let glyph = ShapedGlyph {
                id: GlyphId(u16::try_from(info.glyph_id).unwrap_or(0)),
                cluster: info.cluster as usize,
                advance: to_px(pos.x_advance),
                x_offset: to_px(pos.x_offset),
                y_offset: -to_px(pos.y_offset),
                unsafe_to_break: info.unsafe_to_break(),
            };
            run.advance += glyph.advance;
            run.glyphs.push(glyph);
        }
        run
    }
}

fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let mut start = range.start.min(text.len());
    let mut end = range.end.min(text.len()).max(start);
    while !text.is_char_boundary(start) && start < end {
        start += 1;
    }
    while !text.is_char_boundary(end) && end > start {
        end -= 1;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_resolution() {
        let chars: Vec<(usize, char)> = "1 ab, שלום!".char_indices().collect();
        let s = resolve_scripts(&chars);
        // Leading common takes the following script.
        assert_eq!(s.first(), Some(&Script::Latin));
        assert_eq!(s.get(4), Some(&Script::Latin)); // ','
        assert_eq!(s.last(), Some(&Script::Hebrew)); // '!'
        let digits: Vec<(usize, char)> = "123".char_indices().collect();
        assert!(
            resolve_scripts(&digits)
                .iter()
                .all(|s| *s == Script::Common)
        );
    }

    #[test]
    fn contexts() {
        assert_eq!(pre_context("abcdefgh", 8), "defgh");
        assert_eq!(pre_context("ab", 1), "a");
        assert_eq!(post_context("abcdefgh", 1), "bcdef");
        assert_eq!(post_context("ab", 2), "");
        assert_eq!(pre_context("ab", 9), "");
    }

    #[test]
    fn range_clamping() {
        let t = "aé"; // é is 2 bytes at 1..3
        assert_eq!(clamp_range(t, 0..10), 0..3);
        assert_eq!(clamp_range(t, 2..3), 3..3);
        assert_eq!(clamp_range(t, 0..2), 0..1);
        assert_eq!(clamp_range(t, Range { start: 5, end: 1 }), 3..3);
    }

    #[test]
    fn hb_script_tags() {
        assert!(to_hb_script(Script::Latin).is_some());
        assert!(to_hb_script(Script::Arabic).is_some());
    }

    #[test]
    fn synthesis_rules() {
        use crate::style::{FontStretch, FontWeight};
        let a = |w: u16, s: FontStyle| FaceAttrs {
            weight: FontWeight::new(w),
            style: s,
            stretch: FontStretch::NORMAL,
        };
        let s = synthesis_for(a(700, FontStyle::Italic), Some(a(400, FontStyle::Normal)));
        assert!(s.bold && s.oblique);
        let s = synthesis_for(a(700, FontStyle::Italic), Some(a(700, FontStyle::Italic)));
        assert!(!s.bold && !s.oblique);
        assert_eq!(
            synthesis_for(a(400, FontStyle::Normal), None),
            Synthesis::default()
        );
    }

    fn run(direction: Direction, clusters: &[usize], end: usize) -> ShapedRun {
        ShapedRun {
            font: crate::context::tests_support::font_id(0),
            size: 10.0,
            range: 0..end,
            level: direction.base_level(),
            direction,
            synthesis: Synthesis::default(),
            glyphs: clusters
                .iter()
                .map(|c| ShapedGlyph {
                    id: GlyphId(1),
                    cluster: *c,
                    advance: 10.0,
                    x_offset: 0.0,
                    y_offset: 0.0,
                    unsafe_to_break: false,
                })
                .collect(),
            advance: 10.0 * clusters.len() as f32,
        }
    }

    #[test]
    fn caret_positions_ltr() {
        // "a" + ligature "fi" (one glyph for bytes 1..3) + "b".
        let r = run(Direction::Ltr, &[0, 1, 3], 4);
        assert_eq!(r.x_for_offset(0), 0.0);
        assert_eq!(r.x_for_offset(1), 10.0);
        assert_eq!(r.x_for_offset(2), 20.0); // inside ligature snaps after it
        assert_eq!(r.x_for_offset(4), 30.0);
        assert_eq!(r.offset_for_x(-5.0), 0);
        assert_eq!(r.offset_for_x(4.0), 0);
        assert_eq!(r.offset_for_x(6.0), 1);
        assert_eq!(r.offset_for_x(16.0), 3);
        assert_eq!(r.offset_for_x(99.0), 4);
    }

    #[test]
    fn caret_positions_rtl() {
        // Three one-byte clusters, visual order reversed.
        let r = run(Direction::Rtl, &[2, 1, 0], 3);
        assert_eq!(r.x_for_offset(0), 30.0);
        assert_eq!(r.x_for_offset(3), 0.0);
        assert_eq!(r.x_for_offset(2), 10.0);
        // Rightmost half of the rightmost glyph (cluster 0) is offset 0.
        assert_eq!(r.offset_for_x(28.0), 0);
        assert_eq!(r.offset_for_x(22.0), 1);
        assert_eq!(r.offset_for_x(2.0), 3);
        assert_eq!(r.offset_for_x(40.0), 0);
    }

    #[test]
    fn range_advance() {
        let t = ShapedText {
            runs: vec![run(Direction::Ltr, &[0, 1, 2], 3)],
            base_direction: Direction::Ltr,
        };
        assert_eq!(t.width(), 30.0);
        assert_eq!(t.advance_for_range(1..3), 20.0);
        assert_eq!(t.advance_for_range(3..3), 0.0);
        assert_eq!(t.glyph_count(), 3);
    }

    #[test]
    fn shaping_without_fonts_is_empty() {
        let ctx = FontContext::new();
        let style = TextStyle::new(FontQuery::default(), 16.0);
        let shaped = ctx.shape("hello", &style);
        assert!(shaped.runs.is_empty());
        assert_eq!(shaped.width(), 0.0);
    }

    #[test]
    fn joiners() {
        assert!(joins_previous('\u{200d}'));
        assert!(joins_previous('\n'));
        assert!(joins_previous('\u{fe0f}'));
        assert!(!joins_previous('a'));
    }
}
