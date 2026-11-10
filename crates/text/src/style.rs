//! Font selection inputs: the computed `font-family`, `font-weight`,
//! `font-style` and `font-stretch` values that CSS font matching uses.
//!
//! These mirror the CSS Fonts Module Level 4 properties
//! (<https://www.w3.org/TR/css-fonts-4/#font-prop-desc>) after the cascade,
//! so they hold absolute values only (no `bolder`, no keywords that need a
//! parent).

/// A CSS generic font family (CSS Fonts 4 §4.2, "Generic font families").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GenericFamily {
    /// `serif`: formal text faces with finishing strokes.
    Serif,
    /// `sans-serif`: plain stroke endings.
    SansSerif,
    /// `monospace`: every glyph has the same advance.
    Monospace,
    /// `cursive`: handwriting-like faces.
    Cursive,
    /// `fantasy`: decorative faces.
    Fantasy,
    /// `system-ui`: the platform's user interface face.
    SystemUi,
}

impl GenericFamily {
    /// Parses a generic family keyword, ASCII case-insensitively.
    ///
    /// Returns `None` for anything that is not one of the six generic
    /// families this crate supports. Note that in CSS a *quoted* name such
    /// as `"serif"` is a family name, not a generic; callers must only use
    /// this for unquoted identifiers.
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        let k = keyword.to_ascii_lowercase();
        match k.as_str() {
            "serif" => Some(Self::Serif),
            "sans-serif" => Some(Self::SansSerif),
            "monospace" => Some(Self::Monospace),
            "cursive" => Some(Self::Cursive),
            "fantasy" => Some(Self::Fantasy),
            "system-ui" => Some(Self::SystemUi),
            _ => None,
        }
    }
}

/// One entry of a `font-family` list.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FontFamily {
    /// A named family such as `"Liberation Sans"`. Matched ASCII
    /// case-insensitively against the fonts' family names (CSS Fonts 4 §5.1).
    Named(String),
    /// A generic family, mapped to an installed family by the
    /// [`FontContext`](crate::FontContext).
    Generic(GenericFamily),
}

/// A computed `font-weight` (CSS Fonts 4 §2.2): a number in `1..=1000`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontWeight(u16);

impl FontWeight {
    /// `normal` (400).
    pub const NORMAL: Self = Self(400);
    /// `bold` (700).
    pub const BOLD: Self = Self(700);

    /// Creates a weight, clamping it to the valid CSS range `1..=1000`.
    pub fn new(weight: u16) -> Self {
        Self(weight.clamp(1, 1000))
    }

    /// The numeric weight.
    pub fn get(self) -> u16 {
        self.0
    }
}

impl Default for FontWeight {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// A computed `font-style` (CSS Fonts 4 §2.4).
///
/// M1 follow-up: oblique angles (`oblique 14deg`) are not modelled; any
/// oblique is treated as the default angle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyle {
    /// `normal`.
    #[default]
    Normal,
    /// `italic`.
    Italic,
    /// `oblique`.
    Oblique,
}

/// A computed `font-stretch` (CSS Fonts 4 §2.3), stored as a percentage
/// of the normal width with 0.1% precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontStretch(u16);

impl FontStretch {
    /// `ultra-condensed` (50%).
    pub const ULTRA_CONDENSED: Self = Self(500);
    /// `extra-condensed` (62.5%).
    pub const EXTRA_CONDENSED: Self = Self(625);
    /// `condensed` (75%).
    pub const CONDENSED: Self = Self(750);
    /// `semi-condensed` (87.5%).
    pub const SEMI_CONDENSED: Self = Self(875);
    /// `normal` (100%).
    pub const NORMAL: Self = Self(1000);
    /// `semi-expanded` (112.5%).
    pub const SEMI_EXPANDED: Self = Self(1125);
    /// `expanded` (125%).
    pub const EXPANDED: Self = Self(1250);
    /// `extra-expanded` (150%).
    pub const EXTRA_EXPANDED: Self = Self(1500);
    /// `ultra-expanded` (200%).
    pub const ULTRA_EXPANDED: Self = Self(2000);

    /// Creates a stretch from a percentage (100.0 is normal). Negative and
    /// non-finite values become `normal`; huge values saturate.
    pub fn from_percentage(percent: f32) -> Self {
        if !percent.is_finite() || percent < 0.0 {
            return Self::NORMAL;
        }
        let tenths = (percent * 10.0).round();
        if tenths >= f32::from(u16::MAX) {
            Self(u16::MAX)
        } else {
            // In range 0..u16::MAX and integral after rounding.
            Self(tenths as u16)
        }
    }

    /// The stretch as a percentage of normal width.
    pub fn percentage(self) -> f32 {
        f32::from(self.0) / 10.0
    }

    /// The stretch in tenths of a percent (1000 is normal).
    pub(crate) fn tenths(self) -> u16 {
        self.0
    }

    pub(crate) fn from_tenths(tenths: u16) -> Self {
        Self(tenths)
    }
}

impl Default for FontStretch {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// What to look for when selecting fonts: the computed font properties of
/// an element (CSS Fonts 4 §5.2 inputs).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FontQuery {
    /// The `font-family` list, in priority order. An empty list uses the
    /// context's default family.
    pub families: Vec<FontFamily>,
    /// `font-weight`.
    pub weight: FontWeight,
    /// `font-style`.
    pub style: FontStyle,
    /// `font-stretch`.
    pub stretch: FontStretch,
}

impl FontQuery {
    /// A query for a single family with normal weight, style and stretch.
    pub fn new(family: FontFamily) -> Self {
        Self {
            families: vec![family],
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            stretch: FontStretch::NORMAL,
        }
    }

    /// A query for a generic family with normal weight, style and stretch.
    pub fn generic(family: GenericFamily) -> Self {
        Self::new(FontFamily::Generic(family))
    }
}

impl Default for FontQuery {
    fn default() -> Self {
        Self {
            families: Vec::new(),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
            stretch: FontStretch::NORMAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_keywords() {
        assert_eq!(
            GenericFamily::from_keyword("SERIF"),
            Some(GenericFamily::Serif)
        );
        assert_eq!(
            GenericFamily::from_keyword("sans-serif"),
            Some(GenericFamily::SansSerif)
        );
        assert_eq!(
            GenericFamily::from_keyword("System-UI"),
            Some(GenericFamily::SystemUi)
        );
        assert_eq!(GenericFamily::from_keyword("sans serif"), None);
    }

    #[test]
    fn weight_clamps() {
        assert_eq!(FontWeight::new(0).get(), 1);
        assert_eq!(FontWeight::new(5000).get(), 1000);
        assert_eq!(FontWeight::new(550).get(), 550);
    }

    #[test]
    fn stretch_percentages() {
        assert_eq!(
            FontStretch::from_percentage(62.5),
            FontStretch::EXTRA_CONDENSED
        );
        assert_eq!(FontStretch::from_percentage(100.0), FontStretch::NORMAL);
        assert_eq!(FontStretch::from_percentage(f32::NAN), FontStretch::NORMAL);
        assert_eq!(FontStretch::from_percentage(-5.0), FontStretch::NORMAL);
        assert_eq!(FontStretch::from_percentage(1e9).tenths(), u16::MAX);
        assert!((FontStretch::CONDENSED.percentage() - 75.0).abs() < 1e-6);
    }
}
