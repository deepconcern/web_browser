//! Cosmetic (element hiding) filters: `##selector`, `domain.com##selector`,
//! `~domain.com##selector` and `#@#` exceptions, per the Adblock Plus
//! "element hiding" documentation and uBlock Origin's static filter syntax.
//!
//! Selectors are not parsed as CSS here; the caller (style system) does that
//! and must drop selectors it cannot parse. This module only rejects
//! extended syntaxes that are not plain CSS (procedural filters, scriptlets,
//! HTML filters, snippets) and text that could escape a style rule.

use std::collections::{HashMap, HashSet};

use crate::domains::{DomainList, Reject, parent_domains};

/// One cosmetic filter.
#[derive(Debug, Clone)]
pub(crate) struct CosmeticFilter {
    pub(crate) selector: Box<str>,
    pub(crate) domains: Option<DomainList>,
    pub(crate) exception: bool,
}

/// The result of trying to read a line as a cosmetic filter.
#[derive(Debug)]
pub(crate) enum Cosmetic {
    /// The line is not cosmetic syntax; try it as a network filter.
    NotThisSyntax,
    /// A usable filter.
    Filter(CosmeticFilter),
    /// Cosmetic syntax that is dropped.
    Rejected(Reject),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Hide,
    Exception,
    Unsupported,
}

/// Separators, longest first so that `#@#` wins over `##`... order matters
/// only among those starting at the same position.
const MARKERS: [(&str, Kind); 8] = [
    ("#@?#", Kind::Unsupported), // ABP extended / uBO procedural exception
    ("#@$#", Kind::Unsupported), // snippet / CSS injection exception
    ("#@%#", Kind::Unsupported), // AdGuard script exception
    ("#@#", Kind::Exception),
    ("#?#", Kind::Unsupported), // extended CSS / procedural
    ("#$#", Kind::Unsupported), // ABP snippets, AdGuard CSS injection
    ("#%#", Kind::Unsupported), // AdGuard scripts
    ("##", Kind::Hide),
];

/// uBO / ABP / AdGuard procedural or action operators that are not CSS.
const PROCEDURAL: [&str; 21] = [
    ":has-text(",
    ":-abp-",
    ":xpath(",
    ":style(",
    ":remove(",
    ":remove-attr(",
    ":remove-class(",
    ":matches-css",
    ":matches-attr(",
    ":matches-path(",
    ":matches-media(",
    ":matches-prop(",
    ":min-text-length(",
    ":upward(",
    ":watch-attr(",
    ":others(",
    ":contains(",
    ":if(",
    ":if-not(",
    ":nth-ancestor(",
    "[-ext-",
];

/// Whether `prefix` could be the domain list in front of a cosmetic marker.
fn plausible_domains(prefix: &str) -> bool {
    prefix
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ',' | '~' | '*'))
}

/// Finds the cosmetic marker in `line`: (position, marker length, kind).
fn find_marker(line: &str) -> Option<(usize, usize, Kind)> {
    for (pos, _) in line.match_indices('#') {
        let rest = line.get(pos..)?;
        if let Some((m, kind)) = MARKERS.iter().find(|(m, _)| rest.starts_with(m)) {
            return Some((pos, m.len(), *kind));
        }
    }
    None
}

/// Reads `line` (trimmed, not a comment) as a cosmetic filter.
pub(crate) fn parse(line: &str) -> Cosmetic {
    let Some((pos, len, kind)) = find_marker(line) else {
        // AdGuard HTML filtering: `example.com$$script[...]`, `$@$`.
        if let Some(pos) = line.find("$$").or_else(|| line.find("$@$")) {
            let prefix = line.get(..pos).unwrap_or("");
            if plausible_domains(prefix) {
                return Cosmetic::Rejected(Reject::Unsupported);
            }
        }
        return Cosmetic::NotThisSyntax;
    };
    let prefix = line.get(..pos).unwrap_or("");
    if !plausible_domains(prefix) {
        return Cosmetic::NotThisSyntax;
    }
    if kind == Kind::Unsupported {
        return Cosmetic::Rejected(Reject::Unsupported);
    }
    let selector = line.get(pos + len..).unwrap_or("").trim();
    if selector.is_empty() {
        return Cosmetic::Rejected(Reject::Invalid);
    }
    // `##+js(...)` scriptlets and `##^...` HTML filters.
    if selector.starts_with("+js(") || selector.starts_with('^') {
        return Cosmetic::Rejected(Reject::Unsupported);
    }
    if PROCEDURAL.iter().any(|p| selector.contains(p)) {
        return Cosmetic::Rejected(Reject::Unsupported);
    }
    // Text that could end the style rule early or comment out the rest.
    if selector.contains(['{', '}', '\0']) || selector.contains("/*") {
        return Cosmetic::Rejected(Reject::Unsupported);
    }
    let domains = if prefix.is_empty() {
        None
    } else {
        match DomainList::parse(prefix, ',') {
            Ok(d) => Some(d),
            Err(e) => return Cosmetic::Rejected(e),
        }
    };
    Cosmetic::Filter(CosmeticFilter {
        selector: selector.into(),
        domains,
        exception: kind == Kind::Exception,
    })
}

/// The compiled cosmetic filters.
#[derive(Debug, Default)]
pub(crate) struct CosmeticIndex {
    filters: Vec<CosmeticFilter>,
    /// Hide filters without domains, deduplicated, minus generic exceptions.
    generic: Vec<usize>,
    /// Hide filters with only `~domain` entries.
    generic_conditional: Vec<usize>,
    /// Hide filters with included domains, keyed by each included domain.
    specific: HashMap<Box<str>, Vec<usize>>,
    /// `#@#sel` without domains: never apply `sel`.
    global_exceptions: HashSet<Box<str>>,
    /// Exceptions with included domains, keyed by each included domain.
    exceptions: HashMap<Box<str>, Vec<usize>>,
    /// Exceptions with only `~domain` entries.
    exceptions_conditional: Vec<usize>,
}

impl CosmeticIndex {
    pub(crate) fn build(filters: Vec<CosmeticFilter>) -> Self {
        let mut idx = Self::default();
        for f in &filters {
            if f.exception && f.domains.is_none() {
                idx.global_exceptions.insert(f.selector.clone());
            }
        }
        let mut seen_generic: HashSet<&str> = HashSet::new();
        for (i, f) in filters.iter().enumerate() {
            match (&f.domains, f.exception) {
                (None, true) => {}
                (None, false) => {
                    if !idx.global_exceptions.contains(&f.selector)
                        && seen_generic.insert(&f.selector)
                    {
                        idx.generic.push(i);
                    }
                }
                (Some(d), exception) => {
                    if d.has_includes() {
                        let map = if exception {
                            &mut idx.exceptions
                        } else {
                            &mut idx.specific
                        };
                        for dom in d.includes() {
                            map.entry(dom.into()).or_default().push(i);
                        }
                    } else if exception {
                        idx.exceptions_conditional.push(i);
                    } else {
                        idx.generic_conditional.push(i);
                    }
                }
            }
        }
        drop(seen_generic);
        idx.filters = filters;
        idx
    }

    /// Number of cosmetic filters (hiding and exceptions).
    pub(crate) fn len(&self) -> usize {
        self.filters.len()
    }

    fn get(&self, i: usize) -> Option<&CosmeticFilter> {
        self.filters.get(i)
    }

    fn applies(&self, i: usize, host: &str) -> Option<&CosmeticFilter> {
        self.get(i)
            .filter(|f| f.domains.as_ref().is_none_or(|d| d.allows(host)))
    }

    /// The selectors to hide on `host`: (generic, specific).
    pub(crate) fn query(
        &self,
        host: &str,
        want_generic: bool,
        want_specific: bool,
    ) -> (Vec<&str>, Vec<&str>) {
        let mut excepted: HashSet<&str> = HashSet::new();
        for d in parent_domains(host) {
            for &i in self.exceptions.get(d).into_iter().flatten() {
                if let Some(f) = self.applies(i, host) {
                    excepted.insert(&f.selector);
                }
            }
        }
        for &i in &self.exceptions_conditional {
            if let Some(f) = self.applies(i, host) {
                excepted.insert(&f.selector);
            }
        }
        let allowed = |s: &str| !excepted.contains(s) && !self.global_exceptions.contains(s);

        let mut generic = Vec::new();
        if want_generic {
            let mut seen: HashSet<&str> = HashSet::new();
            for &i in &self.generic {
                if let Some(f) = self.get(i)
                    && !excepted.contains(&*f.selector)
                {
                    generic.push(&*f.selector);
                }
            }
            for &i in &self.generic_conditional {
                if let Some(f) = self.applies(i, host)
                    && allowed(&f.selector)
                    && seen.insert(&f.selector)
                {
                    generic.push(&*f.selector);
                }
            }
        }

        let mut specific = Vec::new();
        if want_specific {
            let mut seen: HashSet<&str> = HashSet::new();
            for d in parent_domains(host) {
                for &i in self.specific.get(d).into_iter().flatten() {
                    if let Some(f) = self.applies(i, host)
                        && allowed(&f.selector)
                        && seen.insert(&f.selector)
                    {
                        specific.push(&*f.selector);
                    }
                }
            }
        }
        (generic, specific)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(line: &str) -> CosmeticFilter {
        match parse(line) {
            Cosmetic::Filter(f) => f,
            other => panic!("{line}: {other:?}"),
        }
    }

    fn rejected(line: &str) -> Reject {
        match parse(line) {
            Cosmetic::Rejected(r) => r,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn parses_forms() {
        let f = filter("##.ad");
        assert_eq!(&*f.selector, ".ad");
        assert!(f.domains.is_none() && !f.exception);
        let f = filter("example.com,~sub.example.com##div#banner > a");
        assert_eq!(&*f.selector, "div#banner > a");
        assert!(f.domains.is_some());
        let f = filter("example.com#@#.ad");
        assert!(f.exception);
        let f = filter("###ad-box");
        assert_eq!(&*f.selector, "#ad-box");
        let f = filter("##div:has(> .sponsored)");
        assert_eq!(&*f.selector, "div:has(> .sponsored)");
    }

    #[test]
    fn not_cosmetic() {
        assert!(matches!(
            parse("||example.com/#ad"),
            Cosmetic::NotThisSyntax
        ));
        assert!(matches!(
            parse("||example.com/a##b"),
            Cosmetic::NotThisSyntax
        ));
        assert!(matches!(parse("/ads/*$script"), Cosmetic::NotThisSyntax));
    }

    #[test]
    fn rejects_extended_syntax() {
        for line in [
            "example.com##+js(nobab)",
            "example.com##^script:has-text(ad)",
            "##div:has-text(Sponsored)",
            "example.com#?#div:-abp-has(.ad)",
            "example.com#$#abort-on-property-read x",
            "example.com#%#window.x=1",
            "example.com#@?#.x",
            "##.ad:style(display: block)",
            "##.ad { color: red }",
            "##.ad /* x",
            "example.com$$script[tag-content=\"ad\"]",
            "google.*##.ad",
        ] {
            assert_eq!(rejected(line), Reject::Unsupported, "{line}");
        }
        assert_eq!(rejected("##"), Reject::Invalid);
        assert_eq!(rejected("a.com,##.x"), Reject::Invalid);
    }

    fn index(lines: &[&str]) -> CosmeticIndex {
        CosmeticIndex::build(lines.iter().map(|l| filter(l)).collect())
    }

    #[test]
    fn query_generic_specific_exceptions() {
        let idx = index(&[
            "##.ad",
            "##.ad",
            "##.banner",
            "##.global-off",
            "#@#.global-off",
            "~example.com##.not-on-example",
            "example.com##.ex-only",
            "example.com,~shop.example.com##.ex-not-shop",
            "other.com##.other",
            "shop.example.com#@#.banner",
            "shop.example.com#@#.ex-only",
        ]);
        let (g, s) = idx.query("www.example.com", true, true);
        assert_eq!(g, [".ad", ".banner"]);
        assert_eq!(s, [".ex-only", ".ex-not-shop"]);

        let (g, s) = idx.query("shop.example.com", true, true);
        assert_eq!(g, [".ad"]);
        assert!(s.is_empty());

        let (g, s) = idx.query("news.org", true, true);
        assert_eq!(g, [".ad", ".banner", ".not-on-example"]);
        assert!(s.is_empty());

        let (g, s) = idx.query("example.com", false, true);
        assert!(g.is_empty());
        assert_eq!(s.len(), 2);
    }
}
