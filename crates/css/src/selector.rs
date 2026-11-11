//! Selectors ([Selectors 4](https://drafts.csswg.org/selectors-4/)).
//!
//! The AST is laid out for right-to-left matching: a [`Selector`] is a list
//! of [`Compound`]s starting with the subject (rightmost) compound, and each
//! compound records the combinator that links it to the next compound to its
//! left.

use crate::cursor::Cursor;
use crate::syntax::{BlockKind, ComponentValue, Numeric, parse_component_values, split_commas};

/// A selector list: matches if any selector matches.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectorList(pub Vec<Selector>);

/// A complex selector.
///
/// `compounds[0]` is the subject. For `ul > li a.x::before`, `compounds` is
/// `[a.x (Descendant), li (Child), ul (None)]` and `pseudo_element` is
/// `Before`.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    /// Compound selectors from right (subject) to left. Never empty.
    pub compounds: Vec<Compound>,
    /// The pseudo-element at the end of the selector, if any. When set, the
    /// selector styles that pseudo-element of the element matched by
    /// `compounds`.
    pub pseudo_element: Option<PseudoElement>,
    /// The selector's specificity (§17).
    pub specificity: Specificity,
}

/// A compound selector: simple selectors that all apply to one element.
#[derive(Clone, Debug, PartialEq)]
pub struct Compound {
    /// The simple selectors. An empty list matches any element (like `*`).
    pub selectors: Vec<SimpleSelector>,
    /// The combinator between this compound and the next one to the left;
    /// `None` for the leftmost compound.
    pub combinator: Option<Combinator>,
}

/// A combinator (§16).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Combinator {
    /// Whitespace: the left compound matches an ancestor.
    Descendant,
    /// `>`: the left compound matches the parent.
    Child,
    /// `+`: the left compound matches the previous element sibling.
    NextSibling,
    /// `~`: the left compound matches some previous element sibling.
    SubsequentSibling,
}

/// A simple selector.
#[derive(Clone, Debug, PartialEq)]
pub enum SimpleSelector {
    /// `*`.
    Universal,
    /// A type selector, the name as written. For HTML elements in HTML
    /// documents it matches ASCII case-insensitively (§5.1); use
    /// `lowercase_name` for that.
    Type {
        /// The name as written (case preserved, for SVG/MathML).
        name: String,
        /// The name ASCII-lowercased.
        lowercase_name: String,
    },
    /// `#id` (case-sensitive in no-quirks mode).
    Id(String),
    /// `.class` (case-sensitive in no-quirks mode).
    Class(String),
    /// An attribute selector.
    Attribute(AttributeSelector),
    /// A pseudo-class.
    PseudoClass(PseudoClass),
}

/// An attribute selector (§6).
#[derive(Clone, Debug, PartialEq)]
pub struct AttributeSelector {
    /// The attribute name as written. Attribute names on HTML elements match
    /// ASCII case-insensitively.
    pub name: String,
    /// The name ASCII-lowercased.
    pub lowercase_name: String,
    /// The operator and value; `None` for `[name]`.
    pub operation: Option<(AttributeOperator, String)>,
    /// The `i` / `s` flag.
    pub case: AttributeCase,
}

/// An attribute selector operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttributeOperator {
    /// `=`: exactly equal.
    Equals,
    /// `~=`: one of the whitespace-separated words.
    Includes,
    /// `|=`: equal, or starts with the value followed by `-`.
    DashMatch,
    /// `^=`: starts with (never matches an empty value).
    Prefix,
    /// `$=`: ends with (never matches an empty value).
    Suffix,
    /// `*=`: contains (never matches an empty value).
    Substring,
}

/// Case-sensitivity of an attribute value comparison (§6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttributeCase {
    /// No flag: the document language decides (HTML has a list of attributes
    /// whose values match case-insensitively on HTML elements).
    Default,
    /// `i`: ASCII case-insensitive.
    Insensitive,
    /// `s`: case-sensitive.
    Sensitive,
}

/// An `An+B` pattern (CSS Syntax 3 §6): matches positions `A*n + B` for
/// some `n >= 0`, positions counted from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nth {
    /// The step.
    pub a: i32,
    /// The offset.
    pub b: i32,
}

impl Nth {
    /// Whether the 1-based `position` matches.
    pub fn matches(&self, position: i32) -> bool {
        let (a, b) = (i64::from(self.a), i64::from(self.b));
        let diff = i64::from(position) - b;
        if a == 0 {
            diff == 0
        } else {
            diff % a == 0 && diff / a >= 0
        }
    }
}

/// A pseudo-class.
#[derive(Clone, Debug, PartialEq)]
pub enum PseudoClass {
    /// `:not(list)`.
    Not(SelectorList),
    /// `:is(list)` (also `:matches()`, `:-webkit-any()`).
    Is(SelectorList),
    /// `:where(list)` (zero specificity).
    Where(SelectorList),
    /// `:root`.
    Root,
    /// `:empty`.
    Empty,
    /// `:first-child`.
    FirstChild,
    /// `:last-child`.
    LastChild,
    /// `:only-child`.
    OnlyChild,
    /// `:first-of-type`.
    FirstOfType,
    /// `:last-of-type`.
    LastOfType,
    /// `:only-of-type`.
    OnlyOfType,
    /// `:nth-child(An+B [of S]?)`.
    NthChild(Nth, Option<SelectorList>),
    /// `:nth-last-child(An+B [of S]?)`.
    NthLastChild(Nth, Option<SelectorList>),
    /// `:nth-of-type(An+B)`.
    NthOfType(Nth),
    /// `:nth-last-of-type(An+B)`.
    NthLastOfType(Nth),
    /// `:link`.
    Link,
    /// `:any-link`.
    AnyLink,
    /// `:visited`.
    Visited,
    /// `:hover`.
    Hover,
    /// `:active`.
    Active,
    /// `:focus`.
    Focus,
    /// `:focus-within`.
    FocusWithin,
    /// `:focus-visible`.
    FocusVisible,
    /// `:checked`.
    Checked,
    /// `:disabled`.
    Disabled,
    /// `:enabled`.
    Enabled,
}

/// A pseudo-element (CSS Pseudo 4). Only `::before`, `::after` and
/// `::marker` generate boxes in M1; the others parse so that selector lists
/// containing them stay valid, and match nothing for now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PseudoElement {
    /// `::before`
    Before,
    /// `::after`
    After,
    /// `::marker`
    Marker,
    /// `::first-line`
    FirstLine,
    /// `::first-letter`
    FirstLetter,
    /// `::selection`
    Selection,
    /// `::placeholder`
    Placeholder,
    /// `::backdrop`
    Backdrop,
}

/// Specificity (§17): compared lexicographically by (ids, classes, types).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Specificity {
    /// ID selectors (A).
    pub ids: u32,
    /// Class, attribute and pseudo-class selectors (B).
    pub classes: u32,
    /// Type selectors and pseudo-elements (C).
    pub types: u32,
}

impl Specificity {
    fn add(self, o: Specificity) -> Specificity {
        Specificity {
            ids: self.ids.saturating_add(o.ids),
            classes: self.classes.saturating_add(o.classes),
            types: self.types.saturating_add(o.types),
        }
    }

    const CLASS: Specificity = Specificity {
        ids: 0,
        classes: 1,
        types: 0,
    };
}

impl SelectorList {
    /// The largest specificity in the list (used for `:is()`, `:not()`).
    pub fn max_specificity(&self) -> Specificity {
        self.0
            .iter()
            .map(|s| s.specificity)
            .max()
            .unwrap_or_default()
    }
}

/// Why a selector failed to parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectorParseError;

impl std::fmt::Display for SelectorParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid selector")
    }
}

impl std::error::Error for SelectorParseError {}

/// Parses a selector list from a string (e.g. for `querySelector`). Any
/// invalid selector makes the whole list invalid (§4.1).
pub fn parse_selector_list(input: &str) -> Result<SelectorList, SelectorParseError> {
    let values = parse_component_values(input);
    parse_selector_list_tokens(&values).ok_or(SelectorParseError)
}

/// Parses a style rule prelude.
pub(crate) fn parse_selector_list_tokens(values: &[ComponentValue]) -> Option<SelectorList> {
    parse_list(values, false, true)
}

/// `forgiving`: invalid entries are dropped instead of failing the list.
/// `allow_pseudo_elements`: false inside `:is()`, `:not()` and friends.
fn parse_list(
    values: &[ComponentValue],
    forgiving: bool,
    allow_pseudo_elements: bool,
) -> Option<SelectorList> {
    let mut out = Vec::new();
    for part in split_commas(values) {
        match parse_complex(part, allow_pseudo_elements) {
            Some(s) => out.push(s),
            None if forgiving => {}
            None => return None,
        }
    }
    if out.is_empty() && !forgiving {
        return None;
    }
    Some(SelectorList(out))
}

fn parse_complex(values: &[ComponentValue], allow_pseudo_elements: bool) -> Option<Selector> {
    let mut c = Cursor::new(values);
    c.skip_ws();
    // Left to right first, then reversed.
    let mut compounds: Vec<(Compound, Option<Combinator>)> = Vec::new();
    let mut pseudo_element = None;
    loop {
        let (compound, pe) = parse_compound(&mut c, allow_pseudo_elements)?;
        compounds.push((compound, None));
        if pe.is_some() {
            pseudo_element = pe;
            // Nothing may follow a pseudo-element in M1.
            if !c.is_exhausted() {
                return None;
            }
            break;
        }
        // Combinator?
        let had_ws = matches!(c.peek_including_ws(), Some(ComponentValue::Whitespace));
        c.skip_ws();
        if c.is_exhausted() {
            break;
        }
        let comb = match c.peek_including_ws() {
            Some(ComponentValue::Delim('>')) => Some(Combinator::Child),
            Some(ComponentValue::Delim('+')) => Some(Combinator::NextSibling),
            Some(ComponentValue::Delim('~')) => Some(Combinator::SubsequentSibling),
            _ => None,
        };
        let comb = match comb {
            Some(k) => {
                c.next_including_ws();
                c.skip_ws();
                k
            }
            None if had_ws => Combinator::Descendant,
            None => return None,
        };
        if let Some(last) = compounds.last_mut() {
            last.1 = Some(comb);
        }
    }
    // Reverse: each compound keeps the combinator to its left neighbour,
    // which is the combinator recorded on the previous (left) compound.
    let mut result = Vec::with_capacity(compounds.len());
    let mut left_combinators: Vec<Option<Combinator>> = vec![None];
    left_combinators.extend(compounds.iter().map(|(_, k)| *k));
    for (i, (compound, _)) in compounds.into_iter().enumerate() {
        let combinator = left_combinators.get(i).copied().flatten();
        result.push(Compound {
            selectors: compound.selectors,
            combinator,
        });
    }
    result.reverse();
    let mut specificity = result
        .iter()
        .flat_map(|c| &c.selectors)
        .map(simple_specificity)
        .fold(Specificity::default(), Specificity::add);
    if pseudo_element.is_some() {
        specificity.types = specificity.types.saturating_add(1);
    }
    Some(Selector {
        compounds: result,
        pseudo_element,
        specificity,
    })
}

fn simple_specificity(s: &SimpleSelector) -> Specificity {
    match s {
        SimpleSelector::Universal => Specificity::default(),
        SimpleSelector::Type { .. } => Specificity {
            ids: 0,
            classes: 0,
            types: 1,
        },
        SimpleSelector::Id(_) => Specificity {
            ids: 1,
            classes: 0,
            types: 0,
        },
        SimpleSelector::Class(_) | SimpleSelector::Attribute(_) => Specificity::CLASS,
        SimpleSelector::PseudoClass(pc) => match pc {
            PseudoClass::Where(_) => Specificity::default(),
            PseudoClass::Is(l) | PseudoClass::Not(l) => l.max_specificity(),
            PseudoClass::NthChild(_, Some(l)) | PseudoClass::NthLastChild(_, Some(l)) => {
                Specificity::CLASS.add(l.max_specificity())
            }
            _ => Specificity::CLASS,
        },
    }
}

/// Parses one compound selector, and a trailing pseudo-element if present.
fn parse_compound(
    c: &mut Cursor<'_>,
    allow_pseudo_elements: bool,
) -> Option<(Compound, Option<PseudoElement>)> {
    let mut selectors = Vec::new();
    let mut any = false;
    // Type or universal selector.
    match c.peek_including_ws() {
        Some(ComponentValue::Ident(name)) => {
            c.next_including_ws();
            reject_namespace(c)?;
            selectors.push(SimpleSelector::Type {
                name: name.clone(),
                lowercase_name: name.to_ascii_lowercase(),
            });
            any = true;
        }
        Some(ComponentValue::Delim('*')) => {
            c.next_including_ws();
            reject_namespace(c)?;
            selectors.push(SimpleSelector::Universal);
            any = true;
        }
        // M1 follow-up: namespace prefixes (`ns|E`, `*|E`, `|E`) with @namespace.
        Some(ComponentValue::Delim('|')) => return None,
        _ => {}
    }
    loop {
        match c.peek_including_ws() {
            Some(ComponentValue::Hash { value, is_id }) => {
                if !is_id {
                    return None;
                }
                c.next_including_ws();
                selectors.push(SimpleSelector::Id(value.clone()));
            }
            Some(ComponentValue::Delim('.')) => {
                c.next_including_ws();
                match c.next_including_ws() {
                    Some(ComponentValue::Ident(class)) => {
                        selectors.push(SimpleSelector::Class(class.clone()));
                    }
                    _ => return None,
                }
            }
            Some(ComponentValue::Block {
                kind: BlockKind::SquareBracket,
                contents,
            }) => {
                c.next_including_ws();
                selectors.push(SimpleSelector::Attribute(parse_attribute(contents)?));
            }
            Some(ComponentValue::Colon) => {
                c.next_including_ws();
                let double = matches!(c.peek_including_ws(), Some(ComponentValue::Colon));
                if double {
                    c.next_including_ws();
                    let name = match c.next_including_ws() {
                        Some(ComponentValue::Ident(n)) => n,
                        _ => return None,
                    };
                    let pe = pseudo_element_from_name(name)?;
                    if !allow_pseudo_elements {
                        return None;
                    }
                    return Some((
                        Compound {
                            selectors,
                            combinator: None,
                        },
                        Some(pe),
                    ));
                }
                match c.next_including_ws() {
                    Some(ComponentValue::Ident(name)) => {
                        // Legacy single-colon pseudo-elements (CSS 2.1 §5.12, Selectors 4 §3.6.3).
                        let lower = name.to_ascii_lowercase();
                        if matches!(
                            lower.as_str(),
                            "before" | "after" | "first-line" | "first-letter"
                        ) {
                            let pe = pseudo_element_from_name(&lower)?;
                            if !allow_pseudo_elements {
                                return None;
                            }
                            return Some((
                                Compound {
                                    selectors,
                                    combinator: None,
                                },
                                Some(pe),
                            ));
                        }
                        selectors
                            .push(SimpleSelector::PseudoClass(pseudo_class_from_name(&lower)?));
                    }
                    Some(ComponentValue::Function { name, arguments }) => {
                        selectors.push(SimpleSelector::PseudoClass(functional_pseudo_class(
                            name, arguments,
                        )?));
                    }
                    _ => return None,
                }
            }
            _ => break,
        }
        any = true;
    }
    if !any {
        return None;
    }
    Some((
        Compound {
            selectors,
            combinator: None,
        },
        None,
    ))
}

/// Namespace prefixes are not supported yet; a `|` right after a type
/// selector makes the selector invalid (which is also what happens for an
/// undeclared prefix).
fn reject_namespace(c: &mut Cursor<'_>) -> Option<()> {
    match c.peek_including_ws() {
        Some(ComponentValue::Delim('|')) => None,
        _ => Some(()),
    }
}

fn pseudo_element_from_name(name: &str) -> Option<PseudoElement> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "before" => PseudoElement::Before,
        "after" => PseudoElement::After,
        "marker" => PseudoElement::Marker,
        "first-line" => PseudoElement::FirstLine,
        "first-letter" => PseudoElement::FirstLetter,
        "selection" => PseudoElement::Selection,
        "placeholder" => PseudoElement::Placeholder,
        "backdrop" => PseudoElement::Backdrop,
        _ => return None,
    })
}

fn pseudo_class_from_name(lower: &str) -> Option<PseudoClass> {
    Some(match lower {
        "root" => PseudoClass::Root,
        "empty" => PseudoClass::Empty,
        "first-child" => PseudoClass::FirstChild,
        "last-child" => PseudoClass::LastChild,
        "only-child" => PseudoClass::OnlyChild,
        "first-of-type" => PseudoClass::FirstOfType,
        "last-of-type" => PseudoClass::LastOfType,
        "only-of-type" => PseudoClass::OnlyOfType,
        "link" => PseudoClass::Link,
        "any-link" => PseudoClass::AnyLink,
        "visited" => PseudoClass::Visited,
        "hover" => PseudoClass::Hover,
        "active" => PseudoClass::Active,
        "focus" => PseudoClass::Focus,
        "focus-within" => PseudoClass::FocusWithin,
        "focus-visible" => PseudoClass::FocusVisible,
        "checked" => PseudoClass::Checked,
        "disabled" => PseudoClass::Disabled,
        "enabled" => PseudoClass::Enabled,
        _ => return None,
    })
}

fn functional_pseudo_class(name: &str, args: &[ComponentValue]) -> Option<PseudoClass> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        // §4.2 / §4.3: forgiving selector lists.
        "is" | "matches" | "-webkit-any" => PseudoClass::Is(parse_list(args, true, false)?),
        "where" => PseudoClass::Where(parse_list(args, true, false)?),
        // §4.3: :not() takes a regular (unforgiving) list.
        "not" => PseudoClass::Not(parse_list(args, false, false)?),
        "nth-child" | "nth-last-child" => {
            let (nth, of) = parse_nth_with_of(args, true)?;
            if lower == "nth-child" {
                PseudoClass::NthChild(nth, of)
            } else {
                PseudoClass::NthLastChild(nth, of)
            }
        }
        "nth-of-type" | "nth-last-of-type" => {
            let (nth, _) = parse_nth_with_of(args, false)?;
            if lower == "nth-of-type" {
                PseudoClass::NthOfType(nth)
            } else {
                PseudoClass::NthLastOfType(nth)
            }
        }
        _ => return None,
    })
}

fn parse_nth_with_of(
    args: &[ComponentValue],
    allow_of: bool,
) -> Option<(Nth, Option<SelectorList>)> {
    let mut c = Cursor::new(args);
    let nth = parse_nth(&mut c)?;
    if c.is_exhausted() {
        return Some((nth, None));
    }
    if !allow_of || !c.eat_ident("of") {
        return None;
    }
    // `of <complex-real-selector-list>`: unforgiving, no pseudo-elements.
    let list = parse_list(c.rest(), false, false)?;
    Some((nth, Some(list)))
}

/// CSS Syntax 3 §6.2: parses `<an+b>`.
pub(crate) fn parse_nth(c: &mut Cursor<'_>) -> Option<Nth> {
    c.skip_ws();
    let first = c.next_including_ws()?;
    // (a, the rest of the `n...` identifier after the `n`).
    let (a, rest): (i32, String) = match first {
        ComponentValue::Number(Numeric {
            int_value: Some(b), ..
        }) => return Some(Nth { a: 0, b: *b }),
        ComponentValue::Ident(id) => {
            let lower = id.to_ascii_lowercase();
            if lower == "odd" {
                return Some(Nth { a: 2, b: 1 });
            }
            if lower == "even" {
                return Some(Nth { a: 2, b: 0 });
            }
            if let Some(r) = lower.strip_prefix("-n") {
                (-1, r.to_string())
            } else {
                (1, lower.strip_prefix('n')?.to_string())
            }
        }
        ComponentValue::Delim('+') => match c.next_including_ws()? {
            ComponentValue::Ident(id) => {
                let lower = id.to_ascii_lowercase();
                (1, lower.strip_prefix('n')?.to_string())
            }
            _ => return None,
        },
        ComponentValue::Dimension(
            Numeric {
                int_value: Some(a), ..
            },
            unit,
        ) => {
            let lower = unit.to_ascii_lowercase();
            (*a, lower.strip_prefix('n')?.to_string())
        }
        _ => return None,
    };
    let b = match rest.as_str() {
        "" => {
            // Optional `['+' | '-'] <integer>` or a signed integer.
            let after = c.attempt(|c| match c.next()? {
                ComponentValue::Number(Numeric {
                    int_value: Some(b),
                    has_sign: true,
                    ..
                }) => Some(*b),
                ComponentValue::Delim(sign @ ('+' | '-')) => {
                    let sign = *sign;
                    match c.next()? {
                        ComponentValue::Number(Numeric {
                            int_value: Some(b),
                            has_sign: false,
                            ..
                        }) => Some(if sign == '-' { b.checked_neg()? } else { *b }),
                        _ => None,
                    }
                }
                _ => None,
            });
            after.unwrap_or(0)
        }
        "-" => match c.next()? {
            ComponentValue::Number(Numeric {
                int_value: Some(b),
                has_sign: false,
                ..
            }) => b.checked_neg()?,
            _ => return None,
        },
        r => {
            let digits = r.strip_prefix('-')?;
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // Saturate huge values like browsers clamp to i32.
            -digits.parse::<i32>().unwrap_or(i32::MAX)
        }
    };
    Some(Nth { a, b })
}

/// §6: `[ name ]`, `[ name op value flag? ]`.
fn parse_attribute(contents: &[ComponentValue]) -> Option<AttributeSelector> {
    Cursor::parse_all(contents, |c| {
        let name = c.ident()?.to_string();
        // M1 follow-up: namespace prefixes in attribute selectors.
        if matches!(c.peek_including_ws(), Some(ComponentValue::Delim('|'))) {
            return None;
        }
        let lowercase_name = name.to_ascii_lowercase();
        if c.is_exhausted() {
            return Some(AttributeSelector {
                name,
                lowercase_name,
                operation: None,
                case: AttributeCase::Default,
            });
        }
        let op = match c.next()? {
            ComponentValue::Delim('=') => AttributeOperator::Equals,
            ComponentValue::IncludeMatch => AttributeOperator::Includes,
            ComponentValue::DashMatch => AttributeOperator::DashMatch,
            ComponentValue::PrefixMatch => AttributeOperator::Prefix,
            ComponentValue::SuffixMatch => AttributeOperator::Suffix,
            ComponentValue::SubstringMatch => AttributeOperator::Substring,
            _ => return None,
        };
        let value = match c.next()? {
            ComponentValue::Ident(v) | ComponentValue::String(v) => v.clone(),
            _ => return None,
        };
        let case = match c.ident() {
            None => AttributeCase::Default,
            Some(f) if f.eq_ignore_ascii_case("i") => AttributeCase::Insensitive,
            Some(f) if f.eq_ignore_ascii_case("s") => AttributeCase::Sensitive,
            Some(_) => return None,
        };
        Some(AttributeSelector {
            name,
            lowercase_name,
            operation: Some((op, value)),
            case,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> Selector {
        let mut l = parse_selector_list(s).unwrap_or_else(|_| panic!("{s}"));
        assert_eq!(l.0.len(), 1);
        l.0.remove(0)
    }

    fn spec(s: &str) -> (u32, u32, u32) {
        let sp = one(s).specificity;
        (sp.ids, sp.classes, sp.types)
    }

    #[test]
    fn structure_is_right_to_left() {
        let s = one("ul > li a.x::before");
        assert_eq!(s.pseudo_element, Some(PseudoElement::Before));
        assert_eq!(s.compounds.len(), 3);
        assert_eq!(s.compounds[0].combinator, Some(Combinator::Descendant));
        assert_eq!(s.compounds[1].combinator, Some(Combinator::Child));
        assert_eq!(s.compounds[2].combinator, None);
        assert!(matches!(
            &s.compounds[0].selectors[..],
            [SimpleSelector::Type { name, .. }, SimpleSelector::Class(c)] if name == "a" && c == "x"
        ));
        let s = one("a+b ~ c");
        assert_eq!(
            s.compounds[0].combinator,
            Some(Combinator::SubsequentSibling)
        );
        assert_eq!(s.compounds[1].combinator, Some(Combinator::NextSibling));
    }

    #[test]
    fn specificity() {
        assert_eq!(spec("*"), (0, 0, 0));
        assert_eq!(spec("li"), (0, 0, 1));
        assert_eq!(spec("ul li"), (0, 0, 2));
        assert_eq!(spec("ul ol+li"), (0, 0, 3));
        assert_eq!(spec("h1 + *[rel=up]"), (0, 1, 1));
        assert_eq!(spec("ul ol li.red"), (0, 1, 3));
        assert_eq!(spec("li.red.level"), (0, 2, 1));
        assert_eq!(spec("#x34y"), (1, 0, 0));
        assert_eq!(spec("#s12:not(FOO)"), (1, 0, 1));
        assert_eq!(spec(".foo :is(.bar, #baz)"), (1, 1, 0));
        assert_eq!(spec(":where(#a, .b) p"), (0, 0, 1));
        assert_eq!(spec("li:nth-child(2n+1 of .x, #y)"), (1, 1, 1));
        assert_eq!(spec("p::before"), (0, 0, 2));
        assert_eq!(spec("a:hover"), (0, 1, 1));
    }

    #[test]
    fn attributes() {
        let s = one("[data-x]");
        let SimpleSelector::Attribute(a) = &s.compounds[0].selectors[0] else {
            panic!()
        };
        assert_eq!(a.operation, None);
        for (src, op) in [
            ("[a=b]", AttributeOperator::Equals),
            ("[a~=b]", AttributeOperator::Includes),
            ("[a|=b]", AttributeOperator::DashMatch),
            ("[a^=b]", AttributeOperator::Prefix),
            ("[a$='b']", AttributeOperator::Suffix),
            ("[a*=\"b\"]", AttributeOperator::Substring),
        ] {
            let s = one(src);
            let SimpleSelector::Attribute(a) = &s.compounds[0].selectors[0] else {
                panic!()
            };
            assert_eq!(a.operation, Some((op, "b".to_string())), "{src}");
        }
        let s = one("[ Type = \"x\" i ]");
        let SimpleSelector::Attribute(a) = &s.compounds[0].selectors[0] else {
            panic!()
        };
        assert_eq!(a.case, AttributeCase::Insensitive);
        assert_eq!(a.lowercase_name, "type");
        let s = one("[a=b s]");
        let SimpleSelector::Attribute(a) = &s.compounds[0].selectors[0] else {
            panic!()
        };
        assert_eq!(a.case, AttributeCase::Sensitive);
        assert!(parse_selector_list("[a=b x]").is_err());
        assert!(parse_selector_list("[a=1]").is_err());
        assert!(parse_selector_list("[]").is_err());
    }

    fn nth(s: &str) -> Option<Nth> {
        let v = parse_component_values(s);
        Cursor::parse_all(&v, parse_nth)
    }

    #[test]
    fn an_plus_b() {
        let n = |a, b| Some(Nth { a, b });
        assert_eq!(nth("odd"), n(2, 1));
        assert_eq!(nth("EVEN"), n(2, 0));
        assert_eq!(nth("5"), n(0, 5));
        assert_eq!(nth("-5"), n(0, -5));
        assert_eq!(nth("n"), n(1, 0));
        assert_eq!(nth("-n"), n(-1, 0));
        assert_eq!(nth("+n"), n(1, 0));
        assert_eq!(nth("2n"), n(2, 0));
        assert_eq!(nth("2n+1"), n(2, 1));
        assert_eq!(nth("2n + 1"), n(2, 1));
        assert_eq!(nth("2n- 1"), n(2, -1));
        assert_eq!(nth("2n -1"), n(2, -1));
        assert_eq!(nth("2n - 1"), n(2, -1));
        assert_eq!(nth("-n+3"), n(-1, 3));
        assert_eq!(nth("-n-3"), n(-1, -3));
        assert_eq!(nth("n-3"), n(1, -3));
        assert_eq!(nth("+n-3"), n(1, -3));
        assert_eq!(nth("3n-0"), n(3, 0));
        assert_eq!(nth("+ n"), None);
        assert_eq!(nth("2n + -1"), None);
        assert_eq!(nth("2n +"), None);
        assert_eq!(nth("2.5n"), None);
        assert_eq!(nth("n-x"), None);
        assert!(Nth { a: 2, b: 1 }.matches(5));
        assert!(!Nth { a: 2, b: 1 }.matches(4));
        assert!(Nth { a: -1, b: 3 }.matches(1));
        assert!(!Nth { a: -1, b: 3 }.matches(4));
        assert!(Nth { a: 0, b: 3 }.matches(3));
    }

    #[test]
    fn pseudo_classes() {
        for s in [
            ":root",
            ":empty",
            "li:first-child",
            ":last-child",
            ":only-child",
            "p:nth-child(2n+1)",
            "p:nth-last-child(odd of .x)",
            "p:nth-of-type(3)",
            "p:nth-last-of-type(-n+2)",
            "a:link",
            "a:visited",
            "a:hover",
            "a:active",
            "a:focus",
            "input:checked",
            "input:disabled",
            "input:enabled",
            "p::after",
            "li::marker",
            "p:before",
            "p::first-line",
            "a:not(.x, #y)",
            ":is(h1, h2) span",
            ":where(a)",
        ] {
            assert!(parse_selector_list(s).is_ok(), "{s}");
        }
        for s in [
            ":unknown",
            "::unknown",
            "a::before b",
            ":not(::before)",
            ":not()",
            ":nth-of-type(2n of .x)",
            "a:not(b, :bogus)",
            "a | b",
            "ns|a",
            "#1",
            "a.",
            "a,",
            "",
            "> a",
        ] {
            assert!(parse_selector_list(s).is_err(), "{s}");
        }
    }

    #[test]
    fn forgiving_lists() {
        let s = one(":is(:bogus, .a)");
        let SimpleSelector::PseudoClass(PseudoClass::Is(l)) = &s.compounds[0].selectors[0] else {
            panic!()
        };
        assert_eq!(l.0.len(), 1);
        assert!(parse_selector_list(":where()").is_ok());
    }

    #[test]
    fn lists() {
        let l = parse_selector_list("a, b > c ,d").unwrap();
        assert_eq!(l.0.len(), 3);
        assert!(parse_selector_list("a, :bogus").is_err());
    }
}
