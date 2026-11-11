//! Basic bidirectional text support (UAX #9, <https://www.unicode.org/reports/tr9/>).
//!
//! M1 scope: the paragraph direction (rules P2/P3), embedding levels for
//! each character (via the `unicode-bidi` crate, kept private), and the
//! line reordering rule L2 for a sequence of run levels. Layout calls
//! [`reorder_visual`] per line once it knows where lines break.
//!
//! M1 follow-up: CSS `unicode-bidi` / `direction` isolation across inline
//! elements (UAX #9 §6.3 via the CSS Writing Modes 4 §2.4 mapping) and rules
//! L1 (trailing whitespace) and L3/L4 belong to layout and are not done here.

use unicode_bidi::{BidiInfo, Level};

/// Inline base direction of a paragraph or of a shaped run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Left to right.
    #[default]
    Ltr,
    /// Right to left.
    Rtl,
}

impl Direction {
    /// The direction of an embedding level: odd levels are right to left.
    pub fn from_level(level: u8) -> Self {
        if level % 2 == 1 { Self::Rtl } else { Self::Ltr }
    }

    /// The lowest embedding level with this direction (0 or 1).
    pub fn base_level(self) -> u8 {
        match self {
            Self::Ltr => 0,
            Self::Rtl => 1,
        }
    }
}

/// Finds the paragraph direction of `text` with UAX #9 rules P2 and P3:
/// the direction of the first strong character (L, R or AL) outside
/// isolates, or `None` when the text has no strong character.
pub fn paragraph_direction(text: &str) -> Option<Direction> {
    match unicode_bidi::get_base_direction(text) {
        unicode_bidi::Direction::Ltr => Some(Direction::Ltr),
        unicode_bidi::Direction::Rtl => Some(Direction::Rtl),
        unicode_bidi::Direction::Mixed => None,
    }
}

/// Resolved bidi levels for a whole text (UAX #9 up to rule I2).
pub(crate) struct BidiLevels {
    /// The embedding level of each byte of the text.
    pub(crate) levels: Vec<u8>,
    /// The direction of the first paragraph.
    pub(crate) base: Direction,
}

/// The paragraph-wide analysis that shaping needs (bidi embedding levels
/// and the base direction), computed once per text.
///
/// Layout shapes a paragraph, then reshapes each line with
/// [`FontContext::shape_analyzed`](crate::FontContext::shape_analyzed)
/// passing the same analysis, so reshaping a line costs O(line), not
/// O(paragraph).
pub struct TextAnalysis {
    pub(crate) bidi: BidiLevels,
}

impl std::fmt::Debug for TextAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextAnalysis")
            .field("len", &self.bidi.levels.len())
            .field("base", &self.bidi.base)
            .finish()
    }
}

impl TextAnalysis {
    /// Analyzes `text`. `direction` forces the paragraph direction (CSS
    /// `direction`); `None` detects it (UAX #9 P2/P3), defaulting to
    /// left-to-right.
    pub fn new(text: &str, direction: Option<Direction>) -> Self {
        Self {
            bidi: resolve_levels(text, direction),
        }
    }

    /// The base direction of the (first) paragraph.
    pub fn base_direction(&self) -> Direction {
        self.bidi.base
    }

    /// The length in bytes of the analyzed text.
    pub fn len(&self) -> usize {
        self.bidi.levels.len()
    }

    /// Whether the analyzed text was empty.
    pub fn is_empty(&self) -> bool {
        self.bidi.levels.is_empty()
    }

    /// The embedding level of the character at byte `offset`.
    pub fn level_at(&self, offset: usize) -> Option<u8> {
        self.bidi.levels.get(offset).copied()
    }
}

/// Resolves the embedding level of every byte of `text` (UAX #9 up to
/// rule I2). `base` forces the paragraph direction; `None` uses P2/P3 with a
/// left-to-right default.
pub(crate) fn resolve_levels(text: &str, base: Option<Direction>) -> BidiLevels {
    let forced = base.map(|d| match d {
        Direction::Ltr => Level::ltr(),
        Direction::Rtl => Level::rtl(),
    });
    let info = BidiInfo::new(text, forced);
    let base = info
        .paragraphs
        .first()
        .map(|p| Direction::from_level(p.level.number()))
        .or(base)
        .unwrap_or_default();
    BidiLevels {
        levels: info.levels.iter().map(|l| l.number()).collect(),
        base,
    }
}

/// UAX #9 rule L2: given the embedding levels of a line's runs in logical
/// order, returns the run indices in visual (left-to-right) order.
///
/// "From the highest level found in the text to the lowest odd level on
/// each line, ... reverse any contiguous sequence of characters that are at
/// that level or higher."
pub fn reorder_visual(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let Some(&highest) = levels.iter().max() else {
        return order;
    };
    let Some(lowest_odd) = levels.iter().copied().filter(|l| l % 2 == 1).min() else {
        return order;
    };
    let mut level = highest;
    while level >= lowest_odd {
        let mut i = 0;
        while i < order.len() {
            let at_or_above = |idx: usize| {
                order
                    .get(idx)
                    .and_then(|run| levels.get(*run))
                    .is_some_and(|l| *l >= level)
            };
            if at_or_above(i) {
                let start = i;
                while at_or_above(i) {
                    i += 1;
                }
                if let Some(slice) = order.get_mut(start..i) {
                    slice.reverse();
                }
            } else {
                i += 1;
            }
        }
        if level == 0 {
            break;
        }
        level -= 1;
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_direction_p2_p3() {
        assert_eq!(paragraph_direction("hello"), Some(Direction::Ltr));
        assert_eq!(paragraph_direction("123 שלום"), Some(Direction::Rtl));
        assert_eq!(paragraph_direction("مرحبا abc"), Some(Direction::Rtl));
        assert_eq!(paragraph_direction("123 !"), None);
        assert_eq!(paragraph_direction(""), None);
    }

    #[test]
    fn levels_mixed() {
        let text = "ab שלום cd";
        let r = resolve_levels(text, None);
        assert_eq!(r.base, Direction::Ltr);
        assert_eq!(r.levels.len(), text.len());
        assert_eq!(r.levels.first(), Some(&0));
        let hebrew_start = text.find('ש').unwrap_or(0);
        assert_eq!(r.levels.get(hebrew_start), Some(&1));
    }

    #[test]
    fn forced_rtl_base() {
        let r = resolve_levels("abc", Some(Direction::Rtl));
        assert_eq!(r.base, Direction::Rtl);
        // Latin inside an RTL paragraph gets level 2.
        assert!(r.levels.iter().all(|l| *l == 2));
    }

    #[test]
    fn empty_text_levels() {
        let r = resolve_levels("", Some(Direction::Rtl));
        assert!(r.levels.is_empty());
        assert_eq!(r.base, Direction::Rtl);
        assert_eq!(resolve_levels("", None).base, Direction::Ltr);
    }

    #[test]
    fn l2_reordering() {
        assert_eq!(reorder_visual(&[]), Vec::<usize>::new());
        assert_eq!(reorder_visual(&[0, 0, 0]), vec![0, 1, 2]);
        assert_eq!(reorder_visual(&[0, 1, 1, 0]), vec![0, 2, 1, 3]);
        assert_eq!(reorder_visual(&[1, 1, 1]), vec![2, 1, 0]);
        // RTL paragraph with an embedded LTR pair: [1, 2, 2, 1].
        assert_eq!(reorder_visual(&[1, 2, 2, 1]), vec![3, 1, 2, 0]);
    }

    #[test]
    fn direction_levels() {
        assert_eq!(Direction::from_level(0), Direction::Ltr);
        assert_eq!(Direction::from_level(3), Direction::Rtl);
        assert_eq!(Direction::Rtl.base_level(), 1);
    }
}
