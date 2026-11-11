//! Stylesheets and rules (CSS Syntax 3 §5, CSS Cascade 4 §6 for `@import`,
//! CSS Conditional 3 for `@media` / `@supports`).

use crate::cursor::Cursor;
use crate::media::{MediaQueryList, parse_media_query_list_tokens};
use crate::properties::{Declaration, DeclarationBlock, DeclarationKind, parse_declaration};
use crate::selector::{SelectorList, parse_selector_list_tokens};
use crate::syntax::{
    BlockKind, ComponentValue, RawRule, consume_block_contents, consume_declaration_list,
    consume_rule_list, parse_component_values, trim_ws,
};
use crate::values::parse_url;

/// A parsed stylesheet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stylesheet {
    /// Top-level rules in source order.
    pub rules: Vec<CssRule>,
}

/// A rule.
#[derive(Clone, Debug, PartialEq)]
pub enum CssRule {
    /// A style rule.
    Style(StyleRule),
    /// `@media`.
    Media(MediaRule),
    /// `@import`.
    Import(ImportRule),
    /// `@supports`, already evaluated.
    Supports(SupportsRule),
    /// `@layer name? { ... }`. Cascade layers are left to `wb-style`.
    LayerBlock(LayerBlockRule),
    /// `@layer a, b;`: declares layer order.
    LayerStatement(Vec<String>),
}

/// A style rule: selectors and declarations. Nested rules inside the block
/// (CSS Nesting) are ignored for now.
// M1 follow-up: CSS Nesting.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleRule {
    /// The selectors.
    pub selectors: SelectorList,
    /// The declarations.
    pub declarations: DeclarationBlock,
}

/// An `@media` rule.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaRule {
    /// The media query list; evaluate with [`MediaQueryList::matches`].
    pub queries: MediaQueryList,
    /// The nested rules.
    pub rules: Vec<CssRule>,
}

/// An `@import` rule. The caller fetches `url` (resolved against the
/// stylesheet's URL) and applies it when `media` matches.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportRule {
    /// The URL as written, unresolved.
    pub url: String,
    /// The media query list (empty means all).
    pub media: MediaQueryList,
    /// The `layer` / `layer(name)` modifier: `Some(None)` for an anonymous
    /// layer.
    pub layer: Option<Option<String>>,
    /// The `supports(...)` condition result; `false` means skip the import.
    pub supports: bool,
}

/// An `@supports` rule (CSS Conditional 3 §2). The condition is evaluated
/// while parsing because it only depends on what this parser supports.
#[derive(Clone, Debug, PartialEq)]
pub struct SupportsRule {
    /// Whether the condition holds; if false, ignore `rules`.
    pub condition: bool,
    /// The nested rules.
    pub rules: Vec<CssRule>,
}

/// An `@layer` block rule.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerBlockRule {
    /// The layer name (dotted names joined with `.`); `None` for anonymous.
    pub name: Option<String>,
    /// The nested rules.
    pub rules: Vec<CssRule>,
}

impl Stylesheet {
    /// The `@import` rules, in order.
    pub fn imports(&self) -> impl Iterator<Item = &ImportRule> {
        self.rules.iter().filter_map(|r| match r {
            CssRule::Import(i) => Some(i),
            _ => None,
        })
    }
}

/// Parses a stylesheet (CSS Syntax 3 §5.3.3). Never fails: errors are
/// recovered from as the spec says, and unsupported rules are dropped.
///
/// The input must already be decoded; see [`charset_from_bytes`] for the
/// `@charset` rule, which is only meaningful at the byte level.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    let values = parse_component_values(css);
    let raw = consume_rule_list(&values, true);
    let mut rules = Vec::new();
    let mut imports_allowed = true;
    for r in &raw {
        if let RawRule::At { name, .. } = r {
            let lower = name.to_ascii_lowercase();
            if lower == "charset" {
                // Handled during decoding (CSS Syntax 3 §3.2); ignored here.
                continue;
            }
            if lower == "import" {
                if imports_allowed && let Some(i) = import_rule(r) {
                    rules.push(CssRule::Import(i));
                }
                continue;
            }
            if lower == "layer" {
                // Layer statements may precede imports.
                let rule = convert_rule(r);
                if !matches!(rule, Some(CssRule::LayerStatement(_))) {
                    imports_allowed = false;
                }
                rules.extend(rule);
                continue;
            }
            if lower == "namespace" {
                // M1 follow-up: @namespace (must also come before other rules).
                continue;
            }
        }
        imports_allowed = false;
        rules.extend(convert_rule(r));
    }
    Stylesheet { rules }
}

/// Parses a `style` attribute value (CSS Style Attributes §3).
pub fn parse_style_attribute(css: &str) -> DeclarationBlock {
    let values = parse_component_values(css);
    declarations_from(&values)
}

fn declarations_from(values: &[ComponentValue]) -> DeclarationBlock {
    let mut declarations: Vec<Declaration> = Vec::new();
    for raw in consume_declaration_list(values) {
        parse_declaration(&raw, &mut declarations);
    }
    DeclarationBlock { declarations }
}

fn nested_rules(block: &[ComponentValue]) -> Vec<CssRule> {
    consume_rule_list(block, false)
        .iter()
        .filter(|r| {
            // `@import` and `@charset` are invalid inside blocks.
            !matches!(r, RawRule::At { name, .. }
                if name.eq_ignore_ascii_case("import") || name.eq_ignore_ascii_case("charset"))
        })
        .filter_map(convert_rule)
        .collect()
}

fn convert_rule(r: &RawRule<'_>) -> Option<CssRule> {
    match r {
        RawRule::Qualified { prelude, block } => {
            let selectors = parse_selector_list_tokens(prelude)?;
            // Only declarations; nested rules are dropped safely.
            let contents = consume_block_contents(block);
            let mut declarations = Vec::new();
            for raw in &contents.declarations {
                parse_declaration(raw, &mut declarations);
            }
            Some(CssRule::Style(StyleRule {
                selectors,
                declarations: DeclarationBlock { declarations },
            }))
        }
        RawRule::At {
            name,
            prelude,
            block,
        } => {
            let lower = name.to_ascii_lowercase();
            match (lower.as_str(), block) {
                ("media", Some(block)) => Some(CssRule::Media(MediaRule {
                    queries: parse_media_query_list_tokens(prelude),
                    rules: nested_rules(block),
                })),
                ("supports", Some(block)) => {
                    let condition = Cursor::parse_all(prelude, supports_condition)?;
                    Some(CssRule::Supports(SupportsRule {
                        condition,
                        rules: nested_rules(block),
                    }))
                }
                ("layer", Some(block)) => {
                    let names = layer_names(prelude)?;
                    let name = match names.as_slice() {
                        [] => None,
                        [one] => Some(one.clone()),
                        _ => return None,
                    };
                    Some(CssRule::LayerBlock(LayerBlockRule {
                        name,
                        rules: nested_rules(block),
                    }))
                }
                ("layer", None) => {
                    let names = layer_names(prelude)?;
                    (!names.is_empty()).then_some(CssRule::LayerStatement(names))
                }
                // @font-face, @keyframes, @page, @container, unknown at-rules...
                // M1 follow-up: @font-face, @keyframes, @page.
                _ => None,
            }
        }
    }
}

/// `<layer-name>#`, dotted names joined with `.`.
fn layer_names(prelude: &[ComponentValue]) -> Option<Vec<String>> {
    if trim_ws(prelude).is_empty() {
        return Some(Vec::new());
    }
    crate::syntax::split_commas(prelude)
        .into_iter()
        .map(|part| {
            let part = trim_ws(part);
            let mut name = String::new();
            let mut expect_ident = true;
            for v in part {
                match (v, expect_ident) {
                    (ComponentValue::Ident(i), true) => {
                        name.push_str(i);
                        expect_ident = false;
                    }
                    (ComponentValue::Delim('.'), false) => {
                        name.push('.');
                        expect_ident = true;
                    }
                    _ => return None,
                }
            }
            (!expect_ident).then_some(name)
        })
        .collect()
}

/// `@import [ <url> | <string> ] [ layer | layer(<name>) ]?
/// [ supports( ... ) ]? <media-query-list>? ;` (CSS Cascade 5 §6.1).
fn import_rule(r: &RawRule<'_>) -> Option<ImportRule> {
    let RawRule::At {
        prelude,
        block: None,
        ..
    } = r
    else {
        return None;
    };
    let mut c = Cursor::new(prelude);
    let url = match c.peek()? {
        ComponentValue::String(s) => {
            c.next();
            s.clone()
        }
        _ => parse_url(&mut c)?,
    };
    let mut layer = None;
    if c.eat_ident("layer") {
        layer = Some(None);
    } else if let Some(ComponentValue::Function { name, arguments }) = c.peek()
        && name.eq_ignore_ascii_case("layer")
    {
        c.next();
        let names = layer_names(arguments)?;
        let [one] = names.as_slice() else {
            return None;
        };
        layer = Some(Some(one.clone()));
    }
    let mut supports = true;
    if let Some(ComponentValue::Function { name, arguments }) = c.peek()
        && name.eq_ignore_ascii_case("supports")
    {
        c.next();
        supports = Cursor::parse_all(arguments, supports_condition)
            .or_else(|| supports_declaration(arguments))
            .unwrap_or(false);
    }
    Some(ImportRule {
        url,
        media: parse_media_query_list_tokens(c.rest()),
        layer,
        supports,
    })
}

/// `<supports-condition>` (CSS Conditional 3 §2.1), evaluated directly.
fn supports_condition(c: &mut Cursor<'_>) -> Option<bool> {
    if c.eat_ident("not") {
        return Some(!supports_in_parens(c)?);
    }
    let mut result = supports_in_parens(c)?;
    let mut joiner: Option<bool> = None; // true = and
    loop {
        let is_and = if c.eat_ident("and") {
            true
        } else if c.eat_ident("or") {
            false
        } else {
            break;
        };
        if joiner.is_some_and(|j| j != is_and) {
            return None;
        }
        joiner = Some(is_and);
        let next = supports_in_parens(c)?;
        result = if is_and {
            result && next
        } else {
            result || next
        };
    }
    Some(result)
}

/// `( condition )`, `( declaration )`, `selector(...)`, or general-enclosed
/// (false).
fn supports_in_parens(c: &mut Cursor<'_>) -> Option<bool> {
    match c.next()? {
        ComponentValue::Block {
            kind: BlockKind::Parenthesis,
            contents,
        } => Some(
            Cursor::parse_all(contents, supports_condition)
                .or_else(|| supports_declaration(contents))
                .unwrap_or(false),
        ),
        ComponentValue::Function { name, arguments } => {
            if name.eq_ignore_ascii_case("selector") {
                Some(parse_selector_list_tokens(arguments).is_some_and(|l| l.0.len() == 1))
            } else {
                Some(false)
            }
        }
        _ => None,
    }
}

/// Whether a declaration is supported: it parses to something other than
/// an unknown property.
fn supports_declaration(contents: &[ComponentValue]) -> Option<bool> {
    let raws = consume_declaration_list(contents);
    let [raw] = raws.as_slice() else {
        return None;
    };
    let mut out = Vec::new();
    parse_declaration(raw, &mut out);
    Some(
        !out.is_empty()
            && out
                .iter()
                .all(|d| !matches!(d.kind, DeclarationKind::Unknown { .. })),
    )
}

/// CSS Syntax 3 §3.2 "determine the fallback encoding": if the bytes start
/// with exactly `@charset "<name>";`, returns the label `<name>`. The caller
/// maps the label to an encoding (`utf-16be`/`utf-16le` mean UTF-8).
pub fn charset_from_bytes(bytes: &[u8]) -> Option<&str> {
    const PREFIX: &[u8] = b"@charset \"";
    let rest = bytes.strip_prefix(PREFIX)?;
    let end = rest.iter().take(1024).position(|&b| b == b'"')?;
    let label = rest.get(..end)?;
    if rest.get(end + 1) != Some(&b';') {
        return None;
    }
    let label = std::str::from_utf8(label).ok()?;
    label.is_ascii().then_some(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{Color, Rgba};
    use crate::media::MediaContext;
    use crate::properties::{CssWideKeyword, CustomValue, Longhand, LonghandId, PropertyId};

    fn style(r: &CssRule) -> &StyleRule {
        match r {
            CssRule::Style(s) => s,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn basic_sheet() {
        let s = parse_stylesheet(
            "p { color: red; margin: 0 auto } a:hover, b { color: blue !important }",
        );
        assert_eq!(s.rules.len(), 2);
        let p = style(&s.rules[0]);
        assert_eq!(p.declarations.declarations.len(), 5);
        assert_eq!(
            p.declarations.declarations[0].kind,
            DeclarationKind::Longhand(Longhand::Color(Color::Rgba(Rgba::rgb(255, 0, 0))))
        );
        let a = style(&s.rules[1]);
        assert_eq!(a.selectors.0.len(), 2);
        assert!(a.declarations.declarations[0].important);
    }

    #[test]
    fn error_recovery() {
        let s = parse_stylesheet(
            "a { color: red; color: ; width: 10 } :bogus { color: red } b { } \
             @unknown foo { x { y: z } } c { color: blue; } d { color: green",
        );
        let kinds: Vec<_> = s.rules.iter().map(|r| style(r).selectors.0.len()).collect();
        // a, b, c, d (unclosed block at EOF is still a rule).
        assert_eq!(kinds.len(), 4);
        assert_eq!(style(&s.rules[0]).declarations.declarations.len(), 1);
        assert_eq!(style(&s.rules[3]).declarations.declarations.len(), 1);
    }

    #[test]
    fn nested_rules_ignored_safely() {
        let s = parse_stylesheet(
            "a { color: red; &:hover { color: blue } @media print { x: y } width: 1px }",
        );
        let a = style(&s.rules[0]);
        assert_eq!(a.declarations.declarations.len(), 2);
    }

    #[test]
    fn media_rules() {
        let s = parse_stylesheet(
            "@media screen and (min-width: 100px) { a { color: red } @media print { b {} } }",
        );
        let CssRule::Media(m) = &s.rules[0] else {
            panic!()
        };
        assert!(m.queries.matches(&MediaContext::default()));
        assert_eq!(m.rules.len(), 2);
    }

    #[test]
    fn imports() {
        let s = parse_stylesheet(
            "@charset \"utf-8\"; @layer a, b; @import url(a.css); @import 'b.css' screen; \
             @import url(\"c.css\") layer(x) supports(display: flex) print; \
             @import 'd.css' supports(foo: bar); p {} @import 'late.css';",
        );
        let urls: Vec<_> = s.imports().map(|i| i.url.as_str()).collect();
        assert_eq!(urls, ["a.css", "b.css", "c.css", "d.css"]);
        let i: Vec<_> = s.imports().collect();
        assert_eq!(i[1].media.0.len(), 1);
        assert_eq!(i[2].layer, Some(Some("x".into())));
        assert!(i[2].supports);
        assert!(!i[3].supports);
        assert!(matches!(&s.rules[0], CssRule::LayerStatement(n) if n.len() == 2));
    }

    #[test]
    fn supports() {
        let s = parse_stylesheet(
            "@supports (display: flex) { a {} } @supports not (display: flex) { b {} } \
             @supports (foo: bar) or (color: red) { c {} } @supports selector(a > b) { d {} } \
             @supports (display: flex) and (bogus: 1) { e {} }",
        );
        let conds: Vec<bool> = s
            .rules
            .iter()
            .map(|r| match r {
                CssRule::Supports(s) => s.condition,
                _ => panic!(),
            })
            .collect();
        assert_eq!(conds, [true, false, true, true, false]);
    }

    #[test]
    fn layers() {
        let s = parse_stylesheet("@layer base { a { color: red } } @layer { b {} }");
        assert!(
            matches!(&s.rules[0], CssRule::LayerBlock(l) if l.name.as_deref() == Some("base") && l.rules.len() == 1)
        );
        assert!(matches!(&s.rules[1], CssRule::LayerBlock(l) if l.name.is_none()));
    }

    #[test]
    fn style_attribute() {
        let d = parse_style_attribute(
            "color: red; --Foo: { a b } ; width: var(--x, 10px); unknown-prop: 1; margin: inherit",
        );
        let k: Vec<_> = d.declarations.iter().map(|d| &d.kind).collect();
        assert!(matches!(
            k[0],
            DeclarationKind::Longhand(Longhand::Color(_))
        ));
        assert!(
            matches!(k[1], DeclarationKind::Custom { name, value: CustomValue::Tokens(t) } if name == "--Foo" && t.len() == 1)
        );
        assert!(matches!(
            k[2],
            DeclarationKind::WithVariables {
                property: PropertyId::Longhand(LonghandId::Width),
                ..
            }
        ));
        assert!(matches!(k[3], DeclarationKind::Unknown { name, .. } if name == "unknown-prop"));
        assert_eq!(k.len(), 8);
        assert!(matches!(
            k[4],
            DeclarationKind::CssWide(LonghandId::MarginTop, CssWideKeyword::Inherit)
        ));
    }

    #[test]
    fn invalid_custom_property_values_are_dropped() {
        let d = parse_style_attribute(
            "--c: red; --c: url(bad url); --d: a ! b; --e: x); --f: var(x); --g: 1 !important",
        );
        let names: Vec<_> = d
            .declarations
            .iter()
            .map(|d| match &d.kind {
                DeclarationKind::Custom { name, .. } => name.as_str(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["--c", "--g"]);
        let d = parse_style_attribute("--h: { a; b ! c }");
        assert_eq!(d.declarations.len(), 1);
    }

    #[test]
    fn invalid_var_syntax_drops_declaration() {
        let d = parse_style_attribute(
            "width: 10px; width: var(x); width: var(--a, var(y)); color: var(--c, x)",
        );
        assert_eq!(d.declarations.len(), 2, "{:?}", d.declarations);
        assert!(matches!(
            d.declarations[0].kind,
            DeclarationKind::Longhand(_)
        ));
        assert!(matches!(
            d.declarations[1].kind,
            DeclarationKind::WithVariables { .. }
        ));
    }

    #[test]
    fn charset() {
        assert_eq!(
            charset_from_bytes(b"@charset \"iso-8859-1\"; a{}"),
            Some("iso-8859-1")
        );
        assert_eq!(charset_from_bytes(b"@charset 'x';"), None);
        assert_eq!(charset_from_bytes(b" @charset \"x\";"), None);
    }
}
