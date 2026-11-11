//! `var()` substitution on token lists (CSS Variables 1 §3).
//!
//! Resolving custom properties (inheritance, cycles) is `wb-style`'s job;
//! this module only replaces `var()` references in one value, given a way to
//! look up already-resolved custom property values.

use crate::syntax::{ComponentValue, trim_ws};

/// The substitution limit of CSS Variables 1 §3.3: the most "weight" (one
/// unit per component value plus the bytes of its text) a single
/// substitution may produce. Exceeding it makes the value invalid, which
/// stops small stylesheets from expanding into huge allocations.
pub const SUBSTITUTION_LIMIT: usize = 1 << 20;

/// Replaces every `var(--name [, fallback]?)` in `value` (§3.1).
///
/// `lookup` returns the computed value of a custom property, or `None` when
/// it is the guaranteed-invalid value (unset, or part of a cycle). When the
/// lookup fails the fallback is used; with no fallback the whole value is
/// invalid at computed-value time and this returns `None`. `env()` is not
/// supported and always uses its fallback. The result is also `None` when it
/// would exceed [`SUBSTITUTION_LIMIT`] (§3.3).
pub fn substitute_variables(
    value: &[ComponentValue],
    lookup: &mut dyn FnMut(&str) -> Option<Vec<ComponentValue>>,
) -> Option<Vec<ComponentValue>> {
    let mut budget = SUBSTITUTION_LIMIT;
    substitute(value, lookup, &mut budget)
}

/// The weight of a value for the substitution budget.
fn weight(values: &[ComponentValue]) -> usize {
    values
        .iter()
        .map(|v| {
            1usize.saturating_add(match v {
                ComponentValue::Ident(s)
                | ComponentValue::AtKeyword(s)
                | ComponentValue::String(s)
                | ComponentValue::Url(s)
                | ComponentValue::Hash { value: s, .. }
                | ComponentValue::Dimension(_, s) => s.len(),
                ComponentValue::Function { name, arguments } => {
                    name.len().saturating_add(weight(arguments))
                }
                ComponentValue::Block { contents, .. } => weight(contents),
                _ => 0,
            })
        })
        .fold(0usize, usize::saturating_add)
}

fn spend(budget: &mut usize, values: &[ComponentValue]) -> Option<()> {
    *budget = budget.checked_sub(weight(values))?;
    Some(())
}

fn substitute(
    value: &[ComponentValue],
    lookup: &mut dyn FnMut(&str) -> Option<Vec<ComponentValue>>,
    budget: &mut usize,
) -> Option<Vec<ComponentValue>> {
    let mut out = Vec::with_capacity(value.len());
    for v in value {
        match v {
            ComponentValue::Function { name, arguments }
                if name.eq_ignore_ascii_case("var") || name.eq_ignore_ascii_case("env") =>
            {
                let is_var = name.eq_ignore_ascii_case("var");
                let (name_part, fallback) = split_var_arguments(arguments)?;
                let resolved = match name_part {
                    [ComponentValue::Ident(n)] if is_var && n.starts_with("--") => lookup(n),
                    [ComponentValue::Ident(_)] if !is_var => None,
                    _ => return None,
                };
                match (resolved, fallback) {
                    (Some(tokens), _) => {
                        spend(budget, &tokens)?;
                        out.extend(tokens);
                    }
                    (None, Some(fb)) => out.extend(substitute(fb, lookup, budget)?),
                    (None, None) => return None,
                }
            }
            ComponentValue::Function { name, arguments } => {
                *budget = budget.checked_sub(1usize.saturating_add(name.len()))?;
                out.push(ComponentValue::Function {
                    name: name.clone(),
                    arguments: substitute(arguments, lookup, budget)?,
                });
            }
            ComponentValue::Block { kind, contents } => {
                *budget = budget.checked_sub(1)?;
                out.push(ComponentValue::Block {
                    kind: *kind,
                    contents: substitute(contents, lookup, budget)?,
                });
            }
            other => {
                spend(budget, std::slice::from_ref(other))?;
                out.push(other.clone());
            }
        }
    }
    Some(out)
}

/// Whether every `var()` in `values` (including those in fallbacks) has
/// valid syntax: `var( <custom-property-name> [, <declaration-value>? ]? )`
/// (CSS Variables 1 §3). A declaration with an invalid reference is invalid
/// at parse time. `env()` only needs an identifier first.
pub(crate) fn var_references_valid(values: &[ComponentValue]) -> bool {
    values.iter().all(|v| match v {
        ComponentValue::Function { name, arguments }
            if name.eq_ignore_ascii_case("var") || name.eq_ignore_ascii_case("env") =>
        {
            let is_var = name.eq_ignore_ascii_case("var");
            let Some((name_part, fallback)) = split_var_arguments(arguments) else {
                return false;
            };
            let name_ok = match name_part {
                [ComponentValue::Ident(n)] => !is_var || n.starts_with("--"),
                _ => false,
            };
            name_ok
                && fallback
                    .is_none_or(|fb| is_declaration_value(fb, false) && var_references_valid(fb))
        }
        ComponentValue::Function { arguments, .. } => var_references_valid(arguments),
        ComponentValue::Block { contents, .. } => var_references_valid(contents),
        _ => true,
    })
}

/// CSS Syntax 3 `<declaration-value>`: no bad strings or URLs, no unmatched
/// closing brackets, and no top-level `;` or `!` (unless `top_level_bang`).
/// Inside functions and blocks, `;` and `!` are allowed.
pub(crate) fn is_declaration_value(values: &[ComponentValue], top_level_bang: bool) -> bool {
    values.iter().all(|v| match v {
        ComponentValue::BadString
        | ComponentValue::BadUrl
        | ComponentValue::CloseParenthesis
        | ComponentValue::CloseSquareBracket
        | ComponentValue::CloseCurlyBracket
        | ComponentValue::Semicolon => false,
        ComponentValue::Delim('!') => top_level_bang,
        ComponentValue::Function { arguments, .. } => is_any_value(arguments),
        ComponentValue::Block { contents, .. } => is_any_value(contents),
        _ => true,
    })
}

/// `<any-value>` inside a block: like `<declaration-value>` but `;` and `!`
/// are allowed.
fn is_any_value(values: &[ComponentValue]) -> bool {
    values.iter().all(|v| match v {
        ComponentValue::BadString
        | ComponentValue::BadUrl
        | ComponentValue::CloseParenthesis
        | ComponentValue::CloseSquareBracket
        | ComponentValue::CloseCurlyBracket => false,
        ComponentValue::Function { arguments, .. } => is_any_value(arguments),
        ComponentValue::Block { contents, .. } => is_any_value(contents),
        _ => true,
    })
}

/// Splits `var()` arguments into the (trimmed) name and the optional
/// fallback (everything after the first comma, which may itself be empty).
fn split_var_arguments(
    args: &[ComponentValue],
) -> Option<(&[ComponentValue], Option<&[ComponentValue]>)> {
    let comma = args.iter().position(|v| matches!(v, ComponentValue::Comma));
    match comma {
        None => Some((trim_ws(args), None)),
        Some(i) => {
            let name = trim_ws(args.get(..i)?);
            let fallback = trim_ws(args.get(i + 1..)?);
            Some((name, Some(fallback)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::properties::{Longhand, PropertyId, parse_property_value};
    use crate::syntax::parse_component_values;
    use crate::types::LengthPercentageOrAuto;

    fn sub(s: &str) -> Option<String> {
        let v = parse_component_values(s);
        let mut lookup = |n: &str| match n {
            "--a" => Some(parse_component_values("10px")),
            "--b" => Some(parse_component_values("1px 2px")),
            _ => None,
        };
        substitute_variables(&v, &mut lookup).map(|v| crate::syntax::serialize(&v))
    }

    #[test]
    fn substitution() {
        assert_eq!(sub("var(--a)").as_deref(), Some("10px"));
        assert_eq!(
            sub("calc(var(--a) + 1px)").as_deref(),
            Some("calc(10px + 1px)")
        );
        assert_eq!(sub("var(--nope, var(--b))").as_deref(), Some("1px 2px"));
        assert_eq!(sub("var(--nope, )").as_deref(), Some(""));
        assert_eq!(sub("var(--nope)"), None);
        assert_eq!(sub("var(a)"), None);
        assert_eq!(sub("env(safe-area-inset-top, 3px)").as_deref(), Some("3px"));
    }

    #[test]
    fn substitution_has_a_size_budget() {
        // 2000 references to a 2000-token value would be 4M tokens.
        let big = parse_component_values(&"a ".repeat(1000));
        let src = "var(--big) ".repeat(2000);
        let v = parse_component_values(&src);
        let mut lookup = |_: &str| Some(big.clone());
        assert_eq!(substitute_variables(&v, &mut lookup), None);
        // Small expansions stay fine.
        let v = parse_component_values("var(--big) var(--big)");
        assert!(substitute_variables(&v, &mut lookup).is_some());
    }

    #[test]
    fn var_syntax_validation() {
        let ok = |s: &str| var_references_valid(&parse_component_values(s));
        assert!(ok("var(--a)"));
        assert!(ok("var( --a , 1px var(--b, ) )"));
        assert!(ok("calc(var(--a) + 1px)"));
        assert!(!ok("var(x)"));
        assert!(!ok("var(--a --b)"));
        assert!(!ok("var()"));
        assert!(!ok("var(--a, var(y))"));
        assert!(!ok("calc(1px + var(1))"));
    }

    #[test]
    fn reparse_after_substitution() {
        let v = parse_component_values("var(--b)");
        let mut lookup = |_: &str| Some(parse_component_values("1px 2px"));
        let tokens = substitute_variables(&v, &mut lookup).unwrap();
        let margin = PropertyId::from_name("margin").unwrap();
        let longhands = parse_property_value(margin, &tokens).unwrap();
        assert_eq!(longhands.len(), 4);
        assert!(matches!(
            &longhands[1],
            Longhand::MarginRight(LengthPercentageOrAuto::LengthPercentage(_))
        ));
    }
}
