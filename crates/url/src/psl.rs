//! Public suffixes and registrable domains.
//!
//! Implements the URL Standard's
//! [host public suffix](https://url.spec.whatwg.org/#host-public-suffix) and
//! [host registrable domain](https://url.spec.whatwg.org/#host-registrable-domain)
//! using the [Public Suffix List](https://publicsuffix.org/list/) "formal
//! algorithm". The list is vendored in `data/public_suffix_list.dat`
//! (MPL-2.0); both the ICANN and PRIVATE sections are used, as browsers do
//! for cookie and site boundaries.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::Host;

/// The vendored Public Suffix List (fetched 2026-11-10 from the
/// publicsuffix/list repository; provenance in `docs/components.md`).
const LIST: &str = include_str!("../data/public_suffix_list.dat");

#[derive(Debug, Default)]
struct Rules {
    /// Plain rules, e.g. `co.uk`.
    normal: HashSet<String>,
    /// Wildcard rules stored without their `*.` prefix, e.g. `ck` for `*.ck`.
    wildcard: HashSet<String>,
    /// Exception rules stored without their `!` prefix, e.g. `www.ck`.
    exception: HashSet<String>,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| parse_list(LIST))
}

/// Converts a rule's domain part to the ASCII form URL hosts use.
fn to_ascii(domain: &str) -> Option<String> {
    if domain.is_ascii() {
        return Some(domain.to_ascii_lowercase());
    }
    match Host::parse(domain)? {
        Host::Domain(d) => Some(d),
        _ => None,
    }
}

fn parse_list(text: &str) -> Rules {
    let mut rules = Rules::default();
    for line in text.lines() {
        // Each rule is the first whitespace-delimited token on a line;
        // lines starting with `//` are comments.
        let Some(rule) = line.split_whitespace().next() else {
            continue;
        };
        if rule.starts_with("//") {
            continue;
        }
        if let Some(rest) = rule.strip_prefix('!') {
            if let Some(r) = to_ascii(rest) {
                rules.exception.insert(r);
            }
        } else if let Some(rest) = rule.strip_prefix("*.") {
            if let Some(r) = to_ascii(rest) {
                rules.wildcard.insert(r);
            }
        } else if let Some(r) = to_ascii(rule) {
            rules.normal.insert(r);
        }
    }
    rules
}

/// Returns the number of trailing labels of `labels` that form the public
/// suffix, per the PSL formal algorithm. `labels` must be non-empty.
fn suffix_label_count(rules: &Rules, labels: &[&str]) -> usize {
    let n = labels.len();
    let suffix = |i: usize| labels.get(i..).map(|s| s.join(".")).unwrap_or_default();
    // Exception rules take priority: the public suffix is the exception
    // rule with its leftmost label removed.
    for i in 0..n {
        if rules.exception.contains(&suffix(i)) {
            return n - i - 1;
        }
    }
    // Otherwise the longest matching normal or wildcard rule prevails.
    for i in 0..n {
        if rules.normal.contains(&suffix(i)) {
            return n - i;
        }
        if i + 1 < n && rules.wildcard.contains(&suffix(i + 1)) {
            return n - i;
        }
    }
    // The implicit default rule is `*`.
    1
}

/// Splits a domain into its public suffix (without trailing dot) and the
/// labels, or `None` for non-domains and malformed domains.
fn split(host: &Host) -> Option<(Vec<&str>, usize, bool)> {
    let domain = host.as_domain()?;
    let trailing_dot = domain.ends_with('.');
    let trimmed = domain.strip_suffix('.').unwrap_or(domain);
    if trimmed.is_empty() {
        return None;
    }
    let labels: Vec<&str> = trimmed.split('.').collect();
    if labels.iter().any(|l| l.is_empty()) {
        return None;
    }
    let lower: String = trimmed.to_ascii_lowercase();
    let lower_labels: Vec<&str> = lower.split('.').collect();
    let count = suffix_label_count(rules(), &lower_labels);
    Some((labels, count, trailing_dot))
}

fn join_tail(labels: &[&str], count: usize, trailing_dot: bool) -> Option<String> {
    let start = labels.len().checked_sub(count)?;
    let mut s = labels.get(start..)?.join(".");
    if trailing_dot {
        s.push('.');
    }
    Some(s)
}

/// The [public suffix](https://url.spec.whatwg.org/#host-public-suffix) of a
/// host, e.g. `co.uk` for `www.example.co.uk`. Returns `None` for IP
/// addresses and malformed domains. A trailing dot on the host is kept.
pub fn public_suffix(host: &Host) -> Option<String> {
    let (labels, count, dot) = split(host)?;
    join_tail(&labels, count, dot)
}

/// The [registrable domain](https://url.spec.whatwg.org/#host-registrable-domain)
/// of a host (its public suffix plus one label), e.g. `example.co.uk` for
/// `www.example.co.uk`. Returns `None` for IP addresses, malformed domains,
/// and hosts that are themselves public suffixes (e.g. `co.uk`).
pub fn registrable_domain(host: &Host) -> Option<String> {
    let (labels, count, dot) = split(host)?;
    if count >= labels.len() {
        return None;
    }
    join_tail(&labels, count + 1, dot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Host {
        Host::Domain(s.to_owned())
    }
    fn rd(s: &str) -> Option<String> {
        registrable_domain(&d(s))
    }
    fn ps(s: &str) -> Option<String> {
        public_suffix(&d(s))
    }

    #[test]
    fn list_loads() {
        let r = rules();
        assert!(r.normal.contains("com"));
        assert!(r.normal.contains("co.uk"));
        assert!(r.wildcard.contains("ck"));
        assert!(r.exception.contains("www.ck"));
        assert!(r.normal.len() > 5000);
    }

    #[test]
    fn url_standard_examples() {
        // Examples from the URL Standard §3.2 table.
        assert_eq!(ps("com"), Some("com".into()));
        assert_eq!(rd("com"), None);
        assert_eq!(ps("example.com"), Some("com".into()));
        assert_eq!(rd("example.com"), Some("example.com".into()));
        assert_eq!(ps("www.example.com"), Some("com".into()));
        assert_eq!(rd("www.example.com"), Some("example.com".into()));
        assert_eq!(ps("sub.www.example.com"), Some("com".into()));
        assert_eq!(rd("sub.www.example.com"), Some("example.com".into()));
        assert_eq!(ps("EXAMPLE.COM"), Some("COM".into()));
        assert_eq!(rd("EXAMPLE.COM"), Some("EXAMPLE.COM".into()));
        assert_eq!(ps("example.com."), Some("com.".into()));
        assert_eq!(rd("example.com."), Some("example.com.".into()));
        assert_eq!(ps("github.io"), Some("github.io".into()));
        assert_eq!(rd("github.io"), None);
        assert_eq!(ps("whatwg.github.io"), Some("github.io".into()));
        assert_eq!(rd("whatwg.github.io"), Some("whatwg.github.io".into()));
        assert_eq!(ps("إختبار"), Some("إختبار".into()));
        assert_eq!(ps("example.إختبار"), Some("إختبار".into()));
        assert_eq!(ps("xn--mgbaam7a8h"), Some("xn--mgbaam7a8h".into()));
        assert_eq!(
            rd("example.xn--mgbaam7a8h"),
            Some("example.xn--mgbaam7a8h".into())
        );
        assert_eq!(
            rd("sub.example.xn--mgbaam7a8h"),
            Some("example.xn--mgbaam7a8h".into())
        );
    }

    #[test]
    fn psl_test_vectors() {
        // A selection of the PSL project's published test vectors
        // (tests/test_psl.txt), expressed as registrable-domain checks.
        assert_eq!(rd("example"), None);
        assert_eq!(rd("b.example.example"), Some("example.example".into()));
        assert_eq!(rd("uk"), None);
        assert_eq!(rd("co.uk"), None);
        assert_eq!(rd("a.co.uk"), Some("a.co.uk".into()));
        assert_eq!(rd("b.a.co.uk"), Some("a.co.uk".into()));
        // Wildcard and exception rules (*.ck, !www.ck).
        assert_eq!(rd("ck"), None);
        assert_eq!(rd("test.ck"), None);
        assert_eq!(rd("b.test.ck"), Some("b.test.ck".into()));
        assert_eq!(rd("a.b.test.ck"), Some("b.test.ck".into()));
        assert_eq!(rd("www.ck"), Some("www.ck".into()));
        assert_eq!(rd("www.www.ck"), Some("www.ck".into()));
        // Japanese rules: jp, *.kawasaki.jp, !city.kawasaki.jp.
        assert_eq!(rd("test.jp"), Some("test.jp".into()));
        assert_eq!(rd("ac.jp"), None);
        assert_eq!(rd("kyoto.jp"), None);
        assert_eq!(rd("c.kobe.jp"), None);
        assert_eq!(rd("b.c.kobe.jp"), Some("b.c.kobe.jp".into()));
        assert_eq!(rd("city.kobe.jp"), Some("city.kobe.jp".into()));
        assert_eq!(rd("www.city.kobe.jp"), Some("city.kobe.jp".into()));
        // IDN.
        assert_eq!(
            rd("xn--85x722f.xn--55qx5d.cn"),
            Some("xn--85x722f.xn--55qx5d.cn".into())
        );
        assert_eq!(
            rd("www.xn--85x722f.xn--55qx5d.cn"),
            Some("xn--85x722f.xn--55qx5d.cn".into())
        );
        assert_eq!(rd("xn--55qx5d.cn"), None);
    }

    #[test]
    fn non_domains() {
        assert_eq!(registrable_domain(&Host::parse("127.0.0.1").unwrap()), None);
        assert_eq!(public_suffix(&Host::parse("[::1]").unwrap()), None);
        assert_eq!(rd(""), None);
        assert_eq!(rd("."), None);
        assert_eq!(rd("a..com"), None);
        assert_eq!(rd("localhost"), None);
        assert_eq!(ps("localhost"), Some("localhost".into()));
    }
}
