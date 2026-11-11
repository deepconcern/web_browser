//! A cheap stand-in for the fuzzer that runs in CI: random concatenations of
//! CSS fragments through every parser must not panic.

use crate::*;

const FRAGMENTS: &[&str] = &[
    "a",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ";",
    ":",
    ",",
    " ",
    "!important",
    "@media",
    "@import",
    "@supports",
    "@layer",
    "screen",
    "and",
    "not",
    "or",
    "(min-width: 10px)",
    "url(",
    "\"",
    "'",
    "\\",
    "/*",
    "*/",
    "<!--",
    "-->",
    "#",
    ".",
    "*",
    ">",
    "+",
    "~",
    "=",
    "|",
    "^=",
    "calc(",
    "min(",
    "var(--x",
    "--x",
    "1",
    "-2.5",
    "3px",
    "50%",
    "1e9",
    "n",
    "-n+",
    "2n+1",
    ":nth-child(",
    ":is(",
    ":not(",
    "::before",
    "color",
    "margin",
    "background",
    "font",
    "flex",
    "border",
    "rgb(",
    "hsl(",
    "oklch(",
    "#fff",
    "inherit",
    "none",
    "auto",
    "/",
    "content",
    "counter(",
    "attr(",
    "\n",
    "\u{0}",
    "é",
    "🦀",
];

#[test]
fn random_inputs_do_not_panic() {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let ctx = MediaContext::default();
    for _ in 0..4000 {
        let len = (next() % 40) as usize;
        let mut s = String::new();
        for _ in 0..len {
            let i = (next() % FRAGMENTS.len() as u64) as usize;
            s.push_str(FRAGMENTS[i]);
        }
        let sheet = parse_stylesheet(&s);
        for r in &sheet.rules {
            if let CssRule::Media(m) = r {
                let _ = m.queries.matches(&ctx);
            }
        }
        let block = parse_style_attribute(&s);
        for d in &block.declarations {
            if let DeclarationKind::WithVariables { property, value } = &d.kind {
                let mut lookup = |_: &str| Some(parse_component_values("1px"));
                if let Some(t) = substitute_variables(value, &mut lookup) {
                    let _ = parse_property_value(*property, &t);
                }
            }
        }
        let _ = parse_selector_list(&s);
        let _ = parse_media_query_list(&s).matches(&ctx);
        let values = parse_component_values(&s);
        let _ = serialize(&values);
        for id in LonghandId::ALL {
            let _ = parse_property_value(PropertyId::Longhand(*id), &values);
        }
        for name in [
            "margin",
            "border",
            "background",
            "font",
            "flex",
            "list-style",
        ] {
            if let Some(id) = PropertyId::from_name(name) {
                let _ = parse_property_value(id, &values);
            }
        }
    }
}

#[test]
fn deeply_nested_input_does_not_overflow() {
    let deep = ":is(".repeat(10_000);
    let _ = parse_selector_list(&deep);
    let deep = format!("a {{ width: {} }}", "calc(".repeat(10_000));
    let _ = parse_stylesheet(&deep);
    let deep = format!("@media {} {{}}", "(".repeat(10_000));
    let _ = parse_stylesheet(&deep);
    let deep = "@media screen {".repeat(10_000);
    let _ = parse_stylesheet(&deep);
}
