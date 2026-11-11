//! List marker text (CSS Lists 3 §3, CSS Counter Styles 3 §6 predefined
//! counter styles).

use wb_style::ListStyleType;

/// The marker string for a list item with `list-style-type: ty` and
/// ordinal `value`: the counter representation plus the style's suffix
/// (". " for numeric and alphabetic styles, " " for symbols). A
/// `<string>` type is used as is.
pub(crate) fn marker_text(ty: &ListStyleType, value: i64) -> String {
    match ty {
        ListStyleType::None => String::new(),
        ListStyleType::String(s) => s.chars().take(1024).collect(),
        ListStyleType::Named(name) => {
            let (body, symbolic) = represent(name, value);
            if symbolic {
                format!("{body} ")
            } else {
                format!("{body}. ")
            }
        }
    }
}

/// The representation of `value` in the counter style `ty`, without a
/// suffix (for `counter()` in `content`).
pub(crate) fn format_counter(ty: &ListStyleType, value: i64) -> String {
    match ty {
        ListStyleType::None => String::new(),
        ListStyleType::String(s) => s.chars().take(1024).collect(),
        ListStyleType::Named(name) => represent(name, value).0,
    }
}

/// Returns the representation and whether the style is symbolic (suffix
/// " " rather than ". "). Unknown names fall back to `decimal` (CSS
/// Counter Styles 3 §1).
fn represent(name: &str, value: i64) -> (String, bool) {
    match name {
        "disc" => ("\u{2022}".to_string(), true),
        "circle" => ("\u{25E6}".to_string(), true),
        "square" => ("\u{25AA}".to_string(), true),
        "disclosure-open" => ("\u{25BE}".to_string(), true),
        "disclosure-closed" => ("\u{25B8}".to_string(), true),
        "decimal-leading-zero" => {
            if (0..10).contains(&value) {
                (format!("0{value}"), false)
            } else if (-9..0).contains(&value) {
                (format!("-0{}", -value), false)
            } else {
                (value.to_string(), false)
            }
        }
        "lower-roman" => (
            roman(value).map_or_else(|| value.to_string(), |r| r.to_lowercase()),
            false,
        ),
        "upper-roman" => (roman(value).unwrap_or_else(|| value.to_string()), false),
        "lower-alpha" | "lower-latin" => (alphabetic(value, &LATIN_LOWER), false),
        "upper-alpha" | "upper-latin" => (alphabetic(value, &LATIN_UPPER), false),
        "lower-greek" => (alphabetic(value, &GREEK_LOWER), false),
        _ => (value.to_string(), false),
    }
}

const LATIN_LOWER: [char; 26] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z',
];
const LATIN_UPPER: [char; 26] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
];
const GREEK_LOWER: [char; 24] = [
    'α', 'β', 'γ', 'δ', 'ε', 'ζ', 'η', 'θ', 'ι', 'κ', 'λ', 'μ', 'ν', 'ξ', 'ο', 'π', 'ρ', 'σ', 'τ',
    'υ', 'φ', 'χ', 'ψ', 'ω',
];

/// The `alphabetic` counter algorithm (CSS Counter Styles 3 §3.1.4): only
/// defined for values ≥ 1, else the `decimal` fallback.
fn alphabetic(value: i64, symbols: &[char]) -> String {
    let n = i64::try_from(symbols.len()).unwrap_or(1).max(1);
    if value < 1 {
        return value.to_string();
    }
    let mut v = value;
    let mut out = Vec::new();
    while v > 0 {
        v -= 1;
        let idx = usize::try_from(v % n).unwrap_or(0);
        out.push(symbols.get(idx).copied().unwrap_or('?'));
        v /= n;
    }
    out.iter().rev().collect()
}

/// The `additive` upper-roman style (CSS Counter Styles 3 §6.1): defined
/// for 1..=3999.
fn roman(value: i64) -> Option<String> {
    if !(1..=3999).contains(&value) {
        return None;
    }
    const TABLE: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut v = value;
    let mut s = String::new();
    for (weight, sym) in TABLE {
        while v >= weight {
            s.push_str(sym);
            v -= weight;
        }
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(n: &str) -> ListStyleType {
        ListStyleType::Named(n.to_string())
    }

    #[test]
    fn styles() {
        assert_eq!(marker_text(&named("disc"), 3), "\u{2022} ");
        assert_eq!(marker_text(&named("decimal"), 3), "3. ");
        assert_eq!(marker_text(&named("decimal"), -2), "-2. ");
        assert_eq!(marker_text(&named("unknown-style"), 7), "7. ");
        assert_eq!(marker_text(&named("lower-roman"), 1994), "mcmxciv. ");
        assert_eq!(marker_text(&named("upper-roman"), 4000), "4000. ");
        assert_eq!(marker_text(&named("lower-alpha"), 1), "a. ");
        assert_eq!(marker_text(&named("lower-alpha"), 27), "aa. ");
        assert_eq!(marker_text(&named("upper-alpha"), 52), "AZ. ");
        assert_eq!(marker_text(&named("lower-alpha"), 0), "0. ");
        assert_eq!(marker_text(&named("lower-greek"), 2), "β. ");
        assert_eq!(marker_text(&named("decimal-leading-zero"), 5), "05. ");
        assert_eq!(marker_text(&ListStyleType::String("-> ".into()), 5), "-> ");
        assert_eq!(marker_text(&ListStyleType::None, 5), "");
        assert_eq!(format_counter(&named("upper-roman"), 9), "IX");
    }

    #[test]
    fn huge_values_are_bounded() {
        let s = alphabetic(i64::MAX, &LATIN_LOWER);
        assert!(s.len() < 20);
    }
}
