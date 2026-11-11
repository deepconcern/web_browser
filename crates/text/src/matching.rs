//! The face-selection step of the CSS font matching algorithm.
//!
//! CSS Fonts Module Level 4, §5.2 "Matching font styles", step 4: within
//! one family, the set of faces is narrowed by `font-stretch`, then by
//! `font-style`, then by `font-weight`, each with its own fallback order.
//! This module is pure (no font files) so it can be tested exhaustively.

use crate::style::{FontStretch, FontStyle, FontWeight};

/// The style attributes of one face, as the matching algorithm sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FaceAttrs {
    pub(crate) weight: FontWeight,
    pub(crate) style: FontStyle,
    pub(crate) stretch: FontStretch,
}

/// Returns the index in `faces` of the face that best matches the
/// requested attributes, or `None` if `faces` is empty. Ties (faces with
/// identical attributes) go to the earliest face, which keeps the result
/// deterministic.
pub(crate) fn best_match(faces: &[FaceAttrs], want: FaceAttrs) -> Option<usize> {
    let mut candidates: Vec<usize> = (0..faces.len()).collect();
    if candidates.is_empty() {
        return None;
    }
    let attrs = |i: &usize| faces.get(*i).copied();

    // 4a. font-stretch.
    let stretches: Vec<u16> = candidates
        .iter()
        .filter_map(|i| attrs(i).map(|a| a.stretch.tenths()))
        .collect();
    if let Some(s) = pick_stretch(&stretches, want.stretch.tenths()) {
        candidates.retain(|i| attrs(i).is_some_and(|a| a.stretch.tenths() == s));
    }

    // 4b. font-style.
    let order: [FontStyle; 3] = match want.style {
        // "If the value of font-style is italic, italic faces are checked
        // first, then oblique, then normal faces."
        FontStyle::Italic => [FontStyle::Italic, FontStyle::Oblique, FontStyle::Normal],
        // "If the value is oblique, oblique faces are checked first, then
        // italic faces and then normal faces."
        FontStyle::Oblique => [FontStyle::Oblique, FontStyle::Italic, FontStyle::Normal],
        // "If the value is normal, normal faces are checked first, then
        // oblique faces, then italic faces."
        FontStyle::Normal => [FontStyle::Normal, FontStyle::Oblique, FontStyle::Italic],
    };
    if let Some(style) = order.iter().copied().find(|s| {
        candidates
            .iter()
            .any(|i| attrs(i).is_some_and(|a| a.style == *s))
    }) {
        candidates.retain(|i| attrs(i).is_some_and(|a| a.style == style));
    }

    // 4c. font-weight.
    let weights: Vec<u16> = candidates
        .iter()
        .filter_map(|i| attrs(i).map(|a| a.weight.get()))
        .collect();
    if let Some(w) = pick_weight(&weights, want.weight.get()) {
        candidates.retain(|i| attrs(i).is_some_and(|a| a.weight.get() == w));
    }

    candidates.first().copied()
}

/// CSS Fonts 4 §5.2 step 4a: "If the desired stretch value is less than or
/// equal to 100%, stretch values below the desired stretch value are
/// checked in descending order followed by stretch values above the
/// desired stretch value in ascending order. Otherwise, stretch values
/// above the desired stretch value are checked in ascending order followed
/// by stretch values below the desired stretch value in descending order."
fn pick_stretch(available: &[u16], want: u16) -> Option<u16> {
    let at_or_below = available.iter().copied().filter(|s| *s <= want).max();
    let at_or_above = available.iter().copied().filter(|s| *s >= want).min();
    if want <= FontStretch::NORMAL.tenths() {
        at_or_below.or(at_or_above)
    } else {
        at_or_above.or(at_or_below)
    }
}

/// CSS Fonts 4 §5.2 step 4c (font-weight), for the single-valued weights
/// of static fonts.
fn pick_weight(available: &[u16], want: u16) -> Option<u16> {
    if available.contains(&want) {
        return Some(want);
    }
    let below = || available.iter().copied().filter(|w| *w < want).max();
    let above = || available.iter().copied().filter(|w| *w > want).min();
    if (400..=500).contains(&want) {
        // "If the desired weight is inclusively between 400 and 500,
        // weights greater than or equal to the target weight are checked in
        // ascending order until 500 is hit and checked, followed by weights
        // less than the target weight in descending order, followed by
        // weights greater than 500, until a match is found."
        available
            .iter()
            .copied()
            .filter(|w| *w > want && *w <= 500)
            .min()
            .or_else(below)
            .or_else(above)
    } else if want < 400 {
        // "If the desired weight is less than 400, weights less than or
        // equal to the desired weight are checked in descending order
        // followed by weights above the desired weight in ascending order."
        below().or_else(above)
    } else {
        // "If the desired weight is greater than 500, weights greater than
        // or equal to the desired weight are checked in ascending order
        // followed by weights below the desired weight in descending order."
        above().or_else(below)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(weight: u16, style: FontStyle, stretch: FontStretch) -> FaceAttrs {
        FaceAttrs {
            weight: FontWeight::new(weight),
            style,
            stretch,
        }
    }

    fn normal(weight: u16) -> FaceAttrs {
        face(weight, FontStyle::Normal, FontStretch::NORMAL)
    }

    #[test]
    fn empty_has_no_match() {
        assert_eq!(best_match(&[], normal(400)), None);
    }

    #[test]
    fn weight_400_to_500_prefers_up_to_500_then_down_then_up() {
        assert_eq!(pick_weight(&[300, 500, 600], 400), Some(500));
        assert_eq!(pick_weight(&[300, 600], 400), Some(300));
        assert_eq!(pick_weight(&[600, 700], 400), Some(600));
        assert_eq!(pick_weight(&[400, 500], 450), Some(500));
        assert_eq!(pick_weight(&[300, 400], 450), Some(400));
        assert_eq!(pick_weight(&[400], 400), Some(400));
    }

    #[test]
    fn weight_below_400_prefers_lighter() {
        assert_eq!(pick_weight(&[100, 200, 400], 300), Some(200));
        assert_eq!(pick_weight(&[400, 700], 300), Some(400));
    }

    #[test]
    fn weight_above_500_prefers_heavier() {
        assert_eq!(pick_weight(&[400, 800, 900], 700), Some(800));
        assert_eq!(pick_weight(&[300, 400], 700), Some(400));
        assert_eq!(pick_weight(&[], 700), None);
    }

    #[test]
    fn stretch_order() {
        let n = FontStretch::NORMAL.tenths();
        let c = FontStretch::CONDENSED.tenths();
        let e = FontStretch::EXPANDED.tenths();
        let ue = FontStretch::ULTRA_EXPANDED.tenths();
        // Narrow request: narrower first, then wider.
        assert_eq!(
            pick_stretch(&[c, n], FontStretch::SEMI_CONDENSED.tenths()),
            Some(c)
        );
        assert_eq!(
            pick_stretch(&[n, e], FontStretch::SEMI_CONDENSED.tenths()),
            Some(n)
        );
        // Wide request: wider first, then narrower.
        assert_eq!(pick_stretch(&[n, ue], e), Some(ue));
        assert_eq!(pick_stretch(&[c, n], e), Some(n));
    }

    #[test]
    fn style_fallback_orders() {
        let faces = [
            normal(400),
            face(400, FontStyle::Oblique, FontStretch::NORMAL),
        ];
        // Italic wanted, no italic face: oblique beats normal.
        let want = face(400, FontStyle::Italic, FontStretch::NORMAL);
        assert_eq!(best_match(&faces, want), Some(1));

        let faces = [
            face(400, FontStyle::Italic, FontStretch::NORMAL),
            face(400, FontStyle::Oblique, FontStretch::NORMAL),
        ];
        // Normal wanted, no normal face: oblique beats italic.
        assert_eq!(best_match(&faces, normal(400)), Some(1));
        // Oblique wanted, only italic and normal: italic.
        let faces = [
            normal(400),
            face(400, FontStyle::Italic, FontStretch::NORMAL),
        ];
        let want = face(400, FontStyle::Oblique, FontStretch::NORMAL);
        assert_eq!(best_match(&faces, want), Some(1));
    }

    #[test]
    fn stretch_narrows_before_style_and_weight() {
        let faces = [
            face(700, FontStyle::Italic, FontStretch::CONDENSED),
            face(400, FontStyle::Normal, FontStretch::NORMAL),
        ];
        // Normal stretch wins even though the condensed face matches the
        // weight and style exactly: stretch is narrowed first.
        let want = face(700, FontStyle::Italic, FontStretch::NORMAL);
        assert_eq!(best_match(&faces, want), Some(1));
    }

    #[test]
    fn full_family_selection() {
        let faces = [
            normal(400),
            normal(700),
            face(400, FontStyle::Italic, FontStretch::NORMAL),
            face(700, FontStyle::Italic, FontStretch::NORMAL),
        ];
        assert_eq!(best_match(&faces, normal(400)), Some(0));
        assert_eq!(best_match(&faces, normal(900)), Some(1));
        assert_eq!(best_match(&faces, normal(100)), Some(0));
        let want = face(600, FontStyle::Italic, FontStretch::NORMAL);
        assert_eq!(best_match(&faces, want), Some(3));
    }

    #[test]
    fn ties_go_to_first_face() {
        let faces = [normal(400), normal(400)];
        assert_eq!(best_match(&faces, normal(400)), Some(0));
    }
}
