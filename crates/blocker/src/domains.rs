//! Domain lists (`$domain=a.com|~b.com` and `a.com,~b.com##sel`) and host
//! helpers shared by network and cosmetic filters.

/// Why a filter line was not turned into a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reject {
    /// Valid syntax that this engine does not implement (dropped, counted).
    Unsupported,
    /// Malformed line.
    Invalid,
    /// A `/regex/` filter that does not compile.
    BadRegex,
}

/// One entry of a domain list: a lowercase domain, maybe negated with `~`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    domain: Box<str>,
    negated: bool,
}

/// A list of domains a filter is restricted to or excluded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DomainList {
    entries: Box<[Entry]>,
}

impl DomainList {
    /// Parses a `sep`-separated domain list.
    ///
    /// Entities (`example.*`) and uBO regex domains (`/re/`) are not
    /// supported. An empty entry makes the list invalid.
    pub(crate) fn parse(text: &str, sep: char) -> Result<Self, Reject> {
        let mut entries = Vec::new();
        for raw in text.split(sep) {
            let raw = raw.trim();
            let (negated, name) = match raw.strip_prefix('~') {
                Some(rest) => (true, rest.trim()),
                None => (false, raw),
            };
            let name = name.strip_suffix('.').unwrap_or(name);
            if name.is_empty() {
                return Err(Reject::Invalid);
            }
            if name.contains(['*', '/']) {
                return Err(Reject::Unsupported);
            }
            if name.contains(|c: char| c.is_whitespace() || c == '~') {
                return Err(Reject::Invalid);
            }
            entries.push(Entry {
                domain: name.to_lowercase().into_boxed_str(),
                negated,
            });
        }
        if entries.is_empty() {
            return Err(Reject::Invalid);
        }
        Ok(Self {
            entries: entries.into_boxed_slice(),
        })
    }

    /// Whether the list has at least one non-negated entry.
    pub(crate) fn has_includes(&self) -> bool {
        self.entries.iter().any(|e| !e.negated)
    }

    /// The non-negated domains.
    pub(crate) fn includes(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|e| !e.negated)
            .map(|e| &*e.domain)
    }

    /// Whether a filter with this list applies on `host` (lowercase).
    ///
    /// The most specific matching entry decides, so
    /// `example.com|~ads.example.com` applies on `www.example.com` but not on
    /// `x.ads.example.com`. With no matching entry the filter applies only if
    /// the list has no includes (an exclusion-only list).
    pub(crate) fn allows(&self, host: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for e in self.entries.iter() {
            if !host_matches(host, &e.domain) {
                continue;
            }
            let len = e.domain.len();
            let better = match best {
                None => true,
                Some((best_len, _)) => len > best_len || (len == best_len && e.negated),
            };
            if better {
                best = Some((len, e.negated));
            }
        }
        match best {
            Some((_, negated)) => !negated,
            None => !self.has_includes(),
        }
    }
}

/// Whether `host` is `domain` or one of its subdomains.
pub(crate) fn host_matches(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    match host.len().checked_sub(domain.len() + 1) {
        Some(dot) => host.ends_with(domain) && host.as_bytes().get(dot) == Some(&b'.'),
        None => false,
    }
}

/// `host` followed by each parent domain: `a.b.c`, `b.c`, `c`.
pub(crate) fn parent_domains(host: &str) -> impl Iterator<Item = &str> {
    std::iter::successors((!host.is_empty()).then_some(host), |h| {
        h.split_once('.')
            .map(|(_, rest)| rest)
            .filter(|r| !r.is_empty())
    })
}

/// Normalizes a host: lowercase, no trailing dot, trimmed.
pub(crate) fn normalize_host(host: &str) -> String {
    let host = host.trim();
    let host = host.strip_suffix('.').unwrap_or(host);
    host.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_matching() {
        assert!(host_matches("example.com", "example.com"));
        assert!(host_matches("a.example.com", "example.com"));
        assert!(!host_matches("badexample.com", "example.com"));
        assert!(!host_matches("com", "example.com"));
    }

    #[test]
    fn parents() {
        let v: Vec<_> = parent_domains("a.b.com").collect();
        assert_eq!(v, ["a.b.com", "b.com", "com"]);
        assert_eq!(parent_domains("").count(), 0);
        let v: Vec<_> = parent_domains("a..b").collect();
        assert_eq!(v, ["a..b", ".b", "b"]);
    }

    #[test]
    fn most_specific_wins() {
        let l = DomainList::parse("example.com|~ads.example.com", '|').unwrap();
        assert!(l.allows("example.com"));
        assert!(l.allows("www.example.com"));
        assert!(!l.allows("ads.example.com"));
        assert!(!l.allows("x.ads.example.com"));
        assert!(!l.allows("other.com"));
    }

    #[test]
    fn exclusion_only() {
        let l = DomainList::parse("~a.com,~b.com", ',').unwrap();
        assert!(!l.has_includes());
        assert!(l.allows("c.com"));
        assert!(!l.allows("x.a.com"));
    }

    #[test]
    fn parse_errors() {
        assert_eq!(DomainList::parse("a.com|", '|'), Err(Reject::Invalid));
        assert_eq!(DomainList::parse("~", '|'), Err(Reject::Invalid));
        assert_eq!(DomainList::parse("google.*", '|'), Err(Reject::Unsupported));
        assert_eq!(DomainList::parse("/re/", '|'), Err(Reject::Unsupported));
        let l = DomainList::parse("A.COM.", '|').unwrap();
        assert!(l.allows("a.com"));
    }
}
