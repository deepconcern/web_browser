//! `var()` substitution on token lists (CSS Variables 1 §3).
//!
//! Resolving custom properties (inheritance, cycles) is `wb-style`'s job;
//! this module only replaces `var()` references in one value, given a way to
//! look up already-resolved custom property values.

use crate::syntax::{ComponentValue, trim_ws};

/// Replaces every `var(--name [, fallback]?)` in `value` (§3.1).
///
/// `lookup` returns the computed value of a custom property, or `None` when
/// it is the guaranteed-invalid value (unset, or part of a cycle). When the
/// lookup fails the fallback is used; with no fallback the whole value is
/// invalid at computed-value time and this returns `None`. `env()` is not
/// supported and always uses its fallback.
pub fn substitute_variables(
    value: &[ComponentValue],
    lookup: &mut dyn FnMut(&str) -> Option<Vec<ComponentValue>>,
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
                    (Some(tokens), _) => out.extend(tokens),
                    (None, Some(fb)) => out.extend(substitute_variables(fb, lookup)?),
                    (None, None) => return None,
                }
            }
            ComponentValue::Function { name, arguments } => out.push(ComponentValue::Function {
                name: name.clone(),
                arguments: substitute_variables(arguments, lookup)?,
            }),
            ComponentValue::Block { kind, contents } => out.push(ComponentValue::Block {
                kind: *kind,
                contents: substitute_variables(contents, lookup)?,
            }),
            other => out.push(other.clone()),
        }
    }
    Some(out)
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
