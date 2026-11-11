//! Tokens, component values and the generic rule/declaration parsing
//! algorithms of [CSS Syntax 3](https://drafts.csswg.org/css-syntax-3/).
//!
//! `cssparser` is used only to tokenize and to pair up blocks. Its tokens are
//! converted right away into our own [`ComponentValue`] tree (§5.3.8 "consume
//! a component value"), and every algorithm above that (rules, declarations,
//! error recovery) is ours and works on that tree.

use std::fmt::Write as _;

/// A numeric value from a number, percentage or dimension token.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Numeric {
    /// The value. For a percentage token this is the number before the `%`
    /// (so `50%` has `value == 50.0`).
    pub value: f32,
    /// The value as an integer when the source had no fractional part or
    /// exponent (the token's "integer" type flag).
    pub int_value: Option<i32>,
    /// Whether the source started with an explicit `+` or `-` sign.
    pub has_sign: bool,
}

/// The kind of a simple block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// `( ... )`
    Parenthesis,
    /// `[ ... ]`
    SquareBracket,
    /// `{ ... }`
    CurlyBracket,
}

/// A CSS component value (CSS Syntax 3 §5.3.8): a preserved token, a
/// function, or a simple block. Comments are dropped.
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentValue {
    /// `<ident-token>`.
    Ident(String),
    /// `<at-keyword-token>`, without the `@`.
    AtKeyword(String),
    /// `<hash-token>`, without the `#`. `is_id` is the "id" type flag (the
    /// value would be a valid identifier).
    Hash {
        /// The value after `#`.
        value: String,
        /// Whether the value starts like an identifier.
        is_id: bool,
    },
    /// `<string-token>`, without quotes and with escapes resolved.
    String(String),
    /// `<url-token>` (an unquoted `url(...)`), the URL text only.
    Url(String),
    /// `<bad-url-token>`; always a parse error.
    BadUrl,
    /// `<bad-string-token>`; always a parse error.
    BadString,
    /// `<delim-token>`.
    Delim(char),
    /// `<number-token>`.
    Number(Numeric),
    /// `<percentage-token>`; the value is in percent (`50%` is `50.0`).
    Percentage(Numeric),
    /// `<dimension-token>`: a number and its unit, as written.
    Dimension(Numeric, String),
    /// `<whitespace-token>`.
    Whitespace,
    /// `:`
    Colon,
    /// `;`
    Semicolon,
    /// `,`
    Comma,
    /// `~=`
    IncludeMatch,
    /// `|=`
    DashMatch,
    /// `^=`
    PrefixMatch,
    /// `$=`
    SuffixMatch,
    /// `*=`
    SubstringMatch,
    /// `<!--`
    Cdo,
    /// `-->`
    Cdc,
    /// An unmatched `)`.
    CloseParenthesis,
    /// An unmatched `]`.
    CloseSquareBracket,
    /// An unmatched `}`.
    CloseCurlyBracket,
    /// A function: its name (as written) and its argument component values.
    Function {
        /// The function name, without the `(`.
        name: String,
        /// Everything between the parentheses.
        arguments: Vec<ComponentValue>,
    },
    /// A simple block and its contents.
    Block {
        /// Which bracket opened the block.
        kind: BlockKind,
        /// Everything between the brackets.
        contents: Vec<ComponentValue>,
    },
}

impl ComponentValue {
    /// Whether this is a whitespace token.
    pub fn is_whitespace(&self) -> bool {
        matches!(self, ComponentValue::Whitespace)
    }

    /// If this is an identifier, its value.
    pub fn as_ident(&self) -> Option<&str> {
        match self {
            ComponentValue::Ident(s) => Some(s),
            _ => None,
        }
    }

    /// Whether this is an identifier that ASCII-case-insensitively equals `s`.
    pub fn is_ident(&self, s: &str) -> bool {
        self.as_ident().is_some_and(|v| v.eq_ignore_ascii_case(s))
    }

    /// Whether this is a function named `name` (ASCII case-insensitive).
    pub fn is_function(&self, name: &str) -> bool {
        matches!(self, ComponentValue::Function { name: n, .. } if n.eq_ignore_ascii_case(name))
    }
}

/// Nested-block depth limit. `cssparser` refuses to descend deeper than its own
/// limit (75); blocks past the limit come out empty instead of overflowing the
/// stack in our recursive algorithms.
const MAX_NESTING: u8 = 64;

/// Parses a string into a list of component values (CSS Syntax 3 §5.3.10
/// "parse a list of component values"). Comments are removed. Never fails.
pub fn parse_component_values(input: &str) -> Vec<ComponentValue> {
    let mut parser = cssparser::Parser::new(input);
    parser.set_nested_block_limit(MAX_NESTING);
    let mut out = Vec::new();
    consume_list(&mut parser, &mut out);
    out
}

fn consume_list(parser: &mut cssparser::Parser<'_>, out: &mut Vec<ComponentValue>) {
    use cssparser::Token as T;
    loop {
        let token = match parser.next_including_whitespace() {
            Ok(t) => t.clone(),
            Err(_) => return,
        };
        let block = |kind| ComponentValue::Block {
            kind,
            contents: Vec::new(),
        };
        let value = match token {
            T::Ident(s) => ComponentValue::Ident(s.to_string()),
            T::AtKeyword(s) => ComponentValue::AtKeyword(s.to_string()),
            T::Hash(s) => ComponentValue::Hash {
                value: s.to_string(),
                is_id: false,
            },
            T::IDHash(s) => ComponentValue::Hash {
                value: s.to_string(),
                is_id: true,
            },
            T::QuotedString(s) => ComponentValue::String(s.to_string()),
            T::UnquotedUrl(s) => ComponentValue::Url(s.to_string()),
            T::Delim(c) => ComponentValue::Delim(c),
            T::Number {
                has_sign,
                value,
                int_value,
            } => ComponentValue::Number(Numeric {
                value,
                int_value,
                has_sign,
            }),
            T::Percentage {
                has_sign,
                unit_value,
                int_value,
            } => ComponentValue::Percentage(Numeric {
                // Prefer the exact integer when there is one.
                value: int_value.map_or(unit_value * 100.0, |i| i as f32),
                int_value,
                has_sign,
            }),
            T::Dimension {
                has_sign,
                value,
                int_value,
                unit,
            } => ComponentValue::Dimension(
                Numeric {
                    value,
                    int_value,
                    has_sign,
                },
                unit.to_string(),
            ),
            T::WhiteSpace(_) => ComponentValue::Whitespace,
            T::Comment(_) => continue,
            T::Colon => ComponentValue::Colon,
            T::Semicolon => ComponentValue::Semicolon,
            T::Comma => ComponentValue::Comma,
            T::IncludeMatch => ComponentValue::IncludeMatch,
            T::DashMatch => ComponentValue::DashMatch,
            T::PrefixMatch => ComponentValue::PrefixMatch,
            T::SuffixMatch => ComponentValue::SuffixMatch,
            T::SubstringMatch => ComponentValue::SubstringMatch,
            T::CDO => ComponentValue::Cdo,
            T::CDC => ComponentValue::Cdc,
            T::BadUrl(_) => ComponentValue::BadUrl,
            T::BadString(_) => ComponentValue::BadString,
            T::CloseParenthesis => ComponentValue::CloseParenthesis,
            T::CloseSquareBracket => ComponentValue::CloseSquareBracket,
            T::CloseCurlyBracket => ComponentValue::CloseCurlyBracket,
            T::Function(name) => {
                let mut arguments = Vec::new();
                nested(parser, &mut arguments);
                ComponentValue::Function {
                    name: name.to_string(),
                    arguments,
                }
            }
            T::ParenthesisBlock => {
                let mut b = block(BlockKind::Parenthesis);
                if let ComponentValue::Block { contents, .. } = &mut b {
                    nested(parser, contents);
                }
                b
            }
            T::SquareBracketBlock => {
                let mut b = block(BlockKind::SquareBracket);
                if let ComponentValue::Block { contents, .. } = &mut b {
                    nested(parser, contents);
                }
                b
            }
            T::CurlyBracketBlock => {
                let mut b = block(BlockKind::CurlyBracket);
                if let ComponentValue::Block { contents, .. } = &mut b {
                    nested(parser, contents);
                }
                b
            }
        };
        out.push(value);
    }
}

fn nested(parser: &mut cssparser::Parser<'_>, out: &mut Vec<ComponentValue>) {
    // On error (the nesting limit) the block is skipped by cssparser and stays
    // empty here.
    let _ = parser.parse_nested_block(|p| {
        consume_list(p, out);
        Ok::<(), cssparser::ParseError<()>>(())
    });
}

/// Serializes component values back to CSS text. The output re-tokenizes to
/// the same component values (CSS Syntax 3 §9), though spacing and number
/// formatting may differ from the source.
pub fn serialize(values: &[ComponentValue]) -> String {
    let mut out = String::new();
    serialize_into(values, &mut out);
    out
}

fn serialize_into(values: &[ComponentValue], out: &mut String) {
    let mut previous: Option<&ComponentValue> = None;
    for v in values {
        if let Some(p) = previous
            && needs_comment_between(p, v)
        {
            out.push_str("/**/");
        }
        serialize_one(v, out);
        previous = Some(v);
    }
}

/// Conservative check for two adjacent tokens that would merge when printed
/// back to back (CSS Syntax 3 §9, the "serialization types" table).
fn needs_comment_between(a: &ComponentValue, b: &ComponentValue) -> bool {
    use ComponentValue as C;
    let a_word = matches!(
        a,
        C::Ident(_) | C::AtKeyword(_) | C::Hash { .. } | C::Dimension(..) | C::Number(_)
    ) || matches!(
        a,
        C::Delim('#') | C::Delim('-') | C::Delim('@') | C::Delim('.') | C::Delim('+')
    );
    let b_word = matches!(
        b,
        C::Ident(_)
            | C::Function { .. }
            | C::Url(_)
            | C::BadUrl
            | C::Number(_)
            | C::Percentage(_)
            | C::Dimension(..)
            | C::Cdc
            | C::Delim('-')
            | C::Delim('%')
            | C::Block {
                kind: BlockKind::Parenthesis,
                ..
            }
    );
    (a_word && b_word)
        || matches!(
            (a, b),
            (C::Delim('/'), C::Delim('*'))
                // `~=`, `|=`, `^=`, `$=`, `*=` match tokens.
                | (C::Delim('~' | '|' | '^' | '$' | '*'), C::Delim('='))
                // `<!--` (CDO) from `<` `!` `--`.
                | (C::Delim('<'), C::Delim('!'))
                | (C::Delim('|'), C::Delim('|'))
        )
        // `-->` (CDC) from an ident `--` followed by `>`.
        || matches!((a, b), (C::Ident(s), C::Delim('>')) if s == "--")
}

fn serialize_number(n: &Numeric, out: &mut String) {
    if let Some(i) = n.int_value {
        if n.has_sign && i >= 0 {
            out.push('+');
        }
        let _ = write!(out, "{i}");
    } else {
        if n.has_sign && n.value >= 0.0 {
            out.push('+');
        }
        let _ = write!(out, "{}", n.value);
        if n.value.fract() == 0.0 && n.value.is_finite() {
            // Keep the "number" type: `1.0` must not come back as an integer.
            out.push_str(".0");
        }
    }
}

fn serialize_one(v: &ComponentValue, out: &mut String) {
    use ComponentValue as C;
    match v {
        C::Ident(s) => {
            let _ = cssparser::serialize_identifier(s, out);
        }
        C::AtKeyword(s) => {
            out.push('@');
            let _ = cssparser::serialize_identifier(s, out);
        }
        C::Hash { value, is_id } => {
            out.push('#');
            // An "id" hash must stay one after re-tokenizing, so it is
            // serialized as an identifier (`#\31 23`), CSS Syntax 3 §9.
            if *is_id {
                let _ = cssparser::serialize_identifier(value, out);
            } else {
                let _ = cssparser::serialize_name(value, out);
            }
        }
        C::String(s) => {
            let _ = cssparser::serialize_string(s, out);
        }
        C::Url(s) => {
            out.push_str("url(");
            for ch in s.chars() {
                if matches!(ch, '(' | ')' | '"' | '\'' | '\\')
                    || ch.is_whitespace()
                    || ch.is_control()
                {
                    let _ = write!(out, "\\{:x} ", u32::from(ch));
                } else {
                    out.push(ch);
                }
            }
            out.push(')');
        }
        C::BadUrl => out.push_str("url(bad url)"),
        C::BadString => out.push_str("\"\n"),
        C::Delim(c) => {
            if *c == '\\' {
                out.push_str("\\\n");
            } else {
                out.push(*c);
            }
        }
        C::Number(n) => serialize_number(n, out),
        C::Percentage(n) => {
            serialize_number(n, out);
            out.push('%');
        }
        C::Dimension(n, unit) => {
            serialize_number(n, out);
            // A unit that starts with `e` followed by a digit or sign would be
            // read back as an exponent.
            if unit.starts_with(['e', 'E']) {
                out.push_str("\\65 ");
                let rest = unit.get(1..).unwrap_or("");
                let _ = cssparser::serialize_name(rest, out);
            } else {
                let _ = cssparser::serialize_identifier(unit, out);
            }
        }
        C::Whitespace => out.push(' '),
        C::Colon => out.push(':'),
        C::Semicolon => out.push(';'),
        C::Comma => out.push(','),
        C::IncludeMatch => out.push_str("~="),
        C::DashMatch => out.push_str("|="),
        C::PrefixMatch => out.push_str("^="),
        C::SuffixMatch => out.push_str("$="),
        C::SubstringMatch => out.push_str("*="),
        C::Cdo => out.push_str("<!--"),
        C::Cdc => out.push_str("-->"),
        C::CloseParenthesis => out.push(')'),
        C::CloseSquareBracket => out.push(']'),
        C::CloseCurlyBracket => out.push('}'),
        C::Function { name, arguments } => {
            let _ = cssparser::serialize_identifier(name, out);
            out.push('(');
            serialize_into(arguments, out);
            out.push(')');
        }
        C::Block { kind, contents } => {
            let (open, close) = match kind {
                BlockKind::Parenthesis => ('(', ')'),
                BlockKind::SquareBracket => ('[', ']'),
                BlockKind::CurlyBracket => ('{', '}'),
            };
            out.push(open);
            serialize_into(contents, out);
            out.push(close);
        }
    }
}

/// Trims leading and trailing whitespace tokens.
pub(crate) fn trim_ws(values: &[ComponentValue]) -> &[ComponentValue] {
    let start = values
        .iter()
        .position(|v| !v.is_whitespace())
        .unwrap_or(values.len());
    let end = values
        .iter()
        .rposition(|v| !v.is_whitespace())
        .map_or(start, |i| i + 1);
    values.get(start..end).unwrap_or(&[])
}

/// Splits on top-level commas.
pub(crate) fn split_commas(values: &[ComponentValue]) -> Vec<&[ComponentValue]> {
    values
        .split(|v| matches!(v, ComponentValue::Comma))
        .collect()
}

// ---------------------------------------------------------------------------
// Rules and declarations (CSS Syntax 3 §5.4).
// ---------------------------------------------------------------------------

/// A rule before its prelude has been interpreted.
#[derive(Debug)]
pub(crate) enum RawRule<'a> {
    /// An at-rule: lowercase-insensitive name, prelude, optional `{}` block.
    At {
        name: &'a str,
        prelude: &'a [ComponentValue],
        block: Option<&'a [ComponentValue]>,
    },
    /// A qualified rule (prelude plus `{}` block).
    Qualified {
        prelude: &'a [ComponentValue],
        block: &'a [ComponentValue],
    },
}

/// A declaration before its value has been interpreted.
#[derive(Debug)]
pub(crate) struct RawDeclaration<'a> {
    pub name: &'a str,
    /// The value, with whitespace trimmed and `!important` removed.
    pub value: &'a [ComponentValue],
    pub important: bool,
}

/// The contents of a style block: declarations plus nested rules (CSS
/// Nesting). Nested rules are returned so callers can ignore them safely.
#[derive(Debug, Default)]
pub(crate) struct BlockContents<'a> {
    pub declarations: Vec<RawDeclaration<'a>>,
    pub rules: Vec<RawRule<'a>>,
}

/// CSS Syntax 3 §5.4.1 "consume a stylesheet's contents" (top level, so CDO
/// and CDC are dropped) or §5.4.2 for the contents of a rule's block when
/// `top_level` is false (the "list of rules" mode used by `@media`).
pub(crate) fn consume_rule_list(values: &[ComponentValue], top_level: bool) -> Vec<RawRule<'_>> {
    let mut rules = Vec::new();
    let mut i = 0;
    while let Some(v) = values.get(i) {
        match v {
            ComponentValue::Whitespace => i += 1,
            ComponentValue::Cdo | ComponentValue::Cdc if top_level => i += 1,
            ComponentValue::AtKeyword(name) => {
                let (rule, next) = consume_at_rule(values, i + 1, name, false);
                rules.push(rule);
                i = next;
            }
            _ => {
                let (rule, next) = consume_qualified_rule(values, i, false);
                if let Some(rule) = rule {
                    rules.push(rule);
                }
                i = next;
            }
        }
    }
    rules
}

/// §5.4.3 "consume an at-rule", starting just after the at-keyword. In a
/// nested context the rule also ends at `;`; at top level likewise (the at-rule
/// prelude ends at `;` or a `{}` block in both cases).
fn consume_at_rule<'a>(
    values: &'a [ComponentValue],
    start: usize,
    name: &'a str,
    _nested: bool,
) -> (RawRule<'a>, usize) {
    let mut i = start;
    while let Some(v) = values.get(i) {
        match v {
            ComponentValue::Semicolon => {
                let prelude = values.get(start..i).unwrap_or(&[]);
                return (
                    RawRule::At {
                        name,
                        prelude,
                        block: None,
                    },
                    i + 1,
                );
            }
            ComponentValue::Block {
                kind: BlockKind::CurlyBracket,
                contents,
            } => {
                let prelude = values.get(start..i).unwrap_or(&[]);
                return (
                    RawRule::At {
                        name,
                        prelude,
                        block: Some(contents),
                    },
                    i + 1,
                );
            }
            _ => i += 1,
        }
    }
    // EOF: a parse error, but the rule is returned.
    let prelude = values.get(start..).unwrap_or(&[]);
    (
        RawRule::At {
            name,
            prelude,
            block: None,
        },
        i,
    )
}

/// §5.4.4 "consume a qualified rule". When `nested`, a `;` ends the rule as a
/// parse error (the rule is dropped).
fn consume_qualified_rule(
    values: &[ComponentValue],
    start: usize,
    nested: bool,
) -> (Option<RawRule<'_>>, usize) {
    let mut i = start;
    while let Some(v) = values.get(i) {
        match v {
            ComponentValue::Semicolon if nested => return (None, i + 1),
            ComponentValue::Block {
                kind: BlockKind::CurlyBracket,
                contents,
            } => {
                let prelude = values.get(start..i).unwrap_or(&[]);
                // A prelude that looks like a custom property declaration
                // (`--foo: {}`) makes the rule invalid.
                let mut p = trim_ws(prelude).iter().filter(|v| !v.is_whitespace());
                if let (Some(ComponentValue::Ident(n)), Some(ComponentValue::Colon)) =
                    (p.next(), p.next())
                    && n.starts_with("--")
                {
                    return (None, i + 1);
                }
                return (
                    Some(RawRule::Qualified {
                        prelude,
                        block: contents,
                    }),
                    i + 1,
                );
            }
            _ => i += 1,
        }
    }
    // EOF without a block: parse error, nothing returned.
    (None, i)
}

/// §5.4.5 "consume a block's contents": declarations and nested rules mixed.
pub(crate) fn consume_block_contents(values: &[ComponentValue]) -> BlockContents<'_> {
    let mut out = BlockContents::default();
    let mut i = 0;
    // The index of the next top-level `;` at or after `i` (or the end). It is
    // cached so that a long run of nested rules without semicolons does not
    // rescan the rest of the block for every rule (keeps parsing linear).
    let mut next_semicolon: Option<usize> = None;
    while let Some(v) = values.get(i) {
        match v {
            ComponentValue::Whitespace | ComponentValue::Semicolon => i += 1,
            ComponentValue::AtKeyword(name) => {
                let (rule, next) = consume_at_rule(values, i + 1, name, true);
                out.rules.push(rule);
                i = next;
            }
            _ => {
                // Find the end of a would-be declaration: the next top-level
                // `;` or the end of the block.
                let end = match next_semicolon {
                    Some(e) if e >= i => e,
                    _ => {
                        let e = values
                            .get(i..)
                            .and_then(|rest| {
                                rest.iter()
                                    .position(|v| matches!(v, ComponentValue::Semicolon))
                            })
                            .map_or(values.len(), |p| i + p);
                        next_semicolon = Some(e);
                        e
                    }
                };
                let slice = values.get(i..end).unwrap_or(&[]);
                if let Some(decl) = consume_declaration(slice) {
                    out.declarations.push(decl);
                    i = end + 1;
                } else {
                    // Not a declaration: re-consume as a nested qualified rule.
                    let (rule, next) = consume_qualified_rule(values, i, true);
                    if let Some(rule) = rule {
                        out.rules.push(rule);
                    }
                    i = next;
                }
            }
        }
    }
    out
}

/// Parses a list of declarations from a `style` attribute or similar. Nested
/// rules are dropped.
pub(crate) fn consume_declaration_list(values: &[ComponentValue]) -> Vec<RawDeclaration<'_>> {
    consume_block_contents(values).declarations
}

/// §5.4.6 "consume a declaration" on the tokens up to (not including) the
/// terminating `;`.
fn consume_declaration(values: &[ComponentValue]) -> Option<RawDeclaration<'_>> {
    let mut iter = values.iter().enumerate();
    let name = match iter.next() {
        Some((_, ComponentValue::Ident(name))) => name.as_str(),
        _ => return None,
    };
    let mut colon_at = None;
    for (idx, v) in iter {
        match v {
            ComponentValue::Whitespace => {}
            ComponentValue::Colon => {
                colon_at = Some(idx);
                break;
            }
            _ => return None,
        }
    }
    let colon_at = colon_at?;
    let mut value = trim_ws(values.get(colon_at + 1..).unwrap_or(&[]));
    let mut important = false;
    // `! important` at the end, with optional whitespace in between.
    if let Some((ComponentValue::Ident(last), rest)) = value.split_last()
        && last.eq_ignore_ascii_case("important")
    {
        let rest = trim_ws(rest);
        if let Some((ComponentValue::Delim('!'), before)) = rest.split_last() {
            important = true;
            value = trim_ws(before);
        }
    }
    let is_custom = name.starts_with("--");
    // A non-custom declaration whose value contains a top-level `{}` block
    // alongside anything else is invalid (CSS Syntax 3 §5.4.6, step on
    // "contains a top-level simple block with an associated token of <{-token>").
    // A value consisting only of such a block is invalid too unless custom;
    // in a nesting context this lets `a:hover { ... }` be read as a rule.
    if !is_custom
        && value.iter().any(|v| {
            matches!(
                v,
                ComponentValue::Block {
                    kind: BlockKind::CurlyBracket,
                    ..
                }
            )
        })
    {
        return None;
    }
    Some(RawDeclaration {
        name,
        value,
        important,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_into_tree() {
        let v = parse_component_values("a(b [c]) {d} 10px 50% #x");
        assert!(v[0].is_function("a"));
        match &v[0] {
            ComponentValue::Function { arguments, .. } => {
                assert_eq!(arguments.len(), 3);
                assert!(matches!(
                    arguments[2],
                    ComponentValue::Block {
                        kind: BlockKind::SquareBracket,
                        ..
                    }
                ));
            }
            _ => panic!("not a function"),
        }
        assert!(matches!(
            &v[4],
            ComponentValue::Dimension(n, u) if n.value == 10.0 && u == "px"
        ));
        assert!(matches!(&v[6], ComponentValue::Percentage(n) if n.value == 50.0));
        assert!(matches!(&v[8], ComponentValue::Hash { value, is_id: true } if value == "x"));
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let s = "(".repeat(100_000);
        let v = parse_component_values(&s);
        assert_eq!(v.len(), 1);
        let s = "{".repeat(100_000) + &"}".repeat(100_000);
        let _ = parse_component_values(&s);
    }

    #[test]
    fn serialize_round_trips() {
        for src in [
            "a b(c, d) [e] {f: g}",
            "1px -2.5em +3 4.0",
            "\"str\\\"ing\" url(x.png) #abc",
            "a/**/b",
            "--x 1e3",
            "1\\65 x",
        ] {
            let v = parse_component_values(src);
            let s = serialize(&v);
            assert_eq!(parse_component_values(&s), v, "{src} -> {s}");
        }
    }

    #[test]
    fn serialize_separates_merging_punctuation() {
        use ComponentValue as C;
        for v in [
            vec![C::Delim('~'), C::Delim('=')],
            vec![C::Delim('|'), C::Delim('=')],
            vec![C::Delim('^'), C::Delim('=')],
            vec![C::Delim('$'), C::Delim('=')],
            vec![C::Delim('*'), C::Delim('=')],
            vec![C::Delim('<'), C::Delim('!'), C::Ident("--".into())],
            vec![C::Ident("--".into()), C::Delim('>')],
            vec![C::Delim('-'), C::Delim('-'), C::Delim('>')],
        ] {
            let s = serialize(&v);
            assert_eq!(parse_component_values(&s), v, "{s}");
        }
    }

    #[test]
    fn serialize_keeps_id_hash_flag() {
        let v = parse_component_values("#\\31 23");
        assert!(matches!(&v[0], ComponentValue::Hash { value, is_id: true } if value == "123"));
        let s = serialize(&v);
        assert_eq!(parse_component_values(&s), v, "{s}");
        assert!(crate::selector::parse_selector_list(&s).is_ok());
    }

    #[test]
    fn many_nested_rules_without_semicolons_is_linear() {
        // 50k rules in one block: quadratic rescanning would take minutes.
        let src = "& {} ".repeat(50_000);
        let v = parse_component_values(&src);
        let start = std::time::Instant::now();
        let c = consume_block_contents(&v);
        assert_eq!(c.rules.len(), 50_000);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn declarations_with_recovery() {
        let v = parse_component_values("color: red; ; bad; 1px: x; width :10px !important; x:y");
        let d = consume_declaration_list(&v);
        let names: Vec<_> = d.iter().map(|d| d.name).collect();
        assert_eq!(names, ["color", "width", "x"]);
        assert!(d[1].important);
        assert_eq!(d[1].value.len(), 1);
    }

    #[test]
    fn nested_rule_does_not_eat_following_declaration() {
        let v = parse_component_values("color: red; & b { color: blue } a:hover {x:y} margin: 0");
        let c = consume_block_contents(&v);
        let names: Vec<_> = c.declarations.iter().map(|d| d.name).collect();
        assert_eq!(names, ["color", "margin"]);
        assert_eq!(c.rules.len(), 2);
    }

    #[test]
    fn stylesheet_rules() {
        let v = parse_component_values("<!-- @import 'a'; a{} @media x { b {} } c --> d {} e");
        let r = consume_rule_list(&v, true);
        assert_eq!(r.len(), 4);
    }
}
