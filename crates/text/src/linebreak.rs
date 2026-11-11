//! Line-break opportunities (UAX #14, <https://www.unicode.org/reports/tr14/>).
//!
//! The pair table and class data come from the `unicode-linebreak` crate,
//! kept private. CSS Text 3 §5 controls (`word-break`, `line-break`,
//! `overflow-wrap`, `white-space`) are applied by layout on top of these
//! default opportunities.
//!
//! M1 follow-up: `word-break: keep-all`/`break-all` and `line-break: strict`
//! tailorings.

/// One place where a line may (or must) end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BreakOpportunity {
    /// Byte offset in the text where the next line would start. Always on a
    /// `char` boundary, and never 0.
    pub offset: usize,
    /// `true` for a mandatory break (UAX #14 classes BK, CR, LF, NL, and the
    /// end of the text); `false` for an allowed break.
    pub mandatory: bool,
}

/// Returns the line-break opportunities of `text` in increasing offset
/// order, per the UAX #14 default algorithm.
///
/// A non-empty text always ends with a mandatory break at `text.len()`
/// (rule LB3). An empty text has no opportunities.
pub fn break_opportunities(text: &str) -> Vec<BreakOpportunity> {
    unicode_linebreak::linebreaks(text)
        .map(|(offset, kind)| BreakOpportunity {
            offset,
            mandatory: matches!(kind, unicode_linebreak::BreakOpportunity::Mandatory),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offsets(text: &str) -> Vec<(usize, bool)> {
        break_opportunities(text)
            .iter()
            .map(|b| (b.offset, b.mandatory))
            .collect()
    }

    #[test]
    fn empty() {
        assert!(break_opportunities("").is_empty());
    }

    #[test]
    fn spaces() {
        // Breaks come after the spaces (LB18).
        assert_eq!(offsets("hello world"), vec![(6, false), (11, true)]);
        assert_eq!(offsets("a  b"), vec![(3, false), (4, true)]);
    }

    #[test]
    fn hard_breaks() {
        assert_eq!(offsets("a\nb"), vec![(2, true), (3, true)]);
        // CR LF is not broken in the middle (LB5).
        assert_eq!(offsets("a\r\nb"), vec![(3, true), (4, true)]);
    }

    #[test]
    fn no_break_before_punctuation_or_in_numbers() {
        assert_eq!(offsets("Hi, you!"), vec![(4, false), (8, true)]);
        assert_eq!(offsets("3.14"), vec![(4, true)]);
    }

    #[test]
    fn hyphen_and_ideographs() {
        // Break after a hyphen (LB21 / HY rules allow after).
        assert_eq!(offsets("well-known"), vec![(5, false), (10, true)]);
        // Ideographs break between each other (LB31); each is 3 bytes.
        assert_eq!(offsets("日本語"), vec![(3, false), (6, false), (9, true)]);
    }

    #[test]
    fn nbsp_prevents_break() {
        assert_eq!(offsets("a\u{a0}b"), vec![(4, true)]);
    }

    #[test]
    fn offsets_are_char_boundaries() {
        let text = "naïve café — déjà vu";
        for b in break_opportunities(text) {
            assert!(text.is_char_boundary(b.offset));
        }
    }
}
