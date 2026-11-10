//! Network filters: parsing and matching one filter against one URL.
//!
//! Syntax follows the public documentation of Adblock Plus ("How to write
//! filters": basic filter rules, special characters `*`, `^`, `|`, `||`,
//! regular expressions, filter options, exception rules) and uBlock Origin's
//! static filter syntax wiki (`$1p`, `$3p`, `$from`, `$important`,
//! `$badfilter`, `$all`, option aliases).

use crate::domains::{DomainList, Reject};
use crate::hash::{Fnv, hash_lower};
use crate::request::{ALL_TYPES, DEFAULT_TYPES, DOCUMENT_BIT, ResourceType, Target};

/// `@@...$document`: the whole page is excepted.
pub(crate) const PAGE_DOCUMENT: u8 = 1;
/// `@@...$elemhide`: no cosmetic filtering on the page.
pub(crate) const PAGE_ELEMHIDE: u8 = 1 << 1;
/// `@@...$generichide`: no generic cosmetic filters on the page.
pub(crate) const PAGE_GENERICHIDE: u8 = 1 << 2;
/// `@@...$specifichide`: no site-specific cosmetic filters on the page.
pub(crate) const PAGE_SPECIFICHIDE: u8 = 1 << 3;

/// Where a pattern is anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// Anywhere in the URL.
    None,
    /// `|`: at the start of the URL.
    Start,
    /// `||`: at the start of the host or of any of its labels.
    Host,
}

/// The URL pattern of a filter.
#[derive(Debug, Clone)]
pub(crate) enum Pattern {
    /// A wildcard pattern. In `bytes`, `*` is a wildcard and `^` a separator
    /// placeholder; everything else is literal. Lowercase unless the filter
    /// has `$match-case`. Leading/trailing implicit wildcards are removed.
    Glob {
        bytes: Box<[u8]>,
        anchor: Anchor,
        end: bool,
    },
    /// `/regex/`.
    Regex(regex::Regex),
}

/// The `$third-party` / `$first-party` restriction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Party {
    Any,
    First,
    Third,
}

/// One parsed network filter.
#[derive(Debug, Clone)]
pub(crate) struct NetworkFilter {
    pub(crate) pattern: Pattern,
    pub(crate) match_case: bool,
    pub(crate) exception: bool,
    pub(crate) important: bool,
    /// Resource type bits this filter applies to.
    pub(crate) types: u16,
    /// `PAGE_*` bits (exceptions only).
    pub(crate) page_flags: u8,
    pub(crate) party: Party,
    pub(crate) domains: Option<DomainList>,
    /// The filter line, for reporting and `$badfilter`.
    pub(crate) raw: Box<str>,
}

/// The result of parsing a network filter line.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub(crate) filter: NetworkFilter,
    /// The line had `$badfilter`: it disables the matching filter.
    pub(crate) badfilter: bool,
}

/// Size limit for compiled regex filters, so one hostile list line cannot
/// take a lot of memory.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Splits `body` (without `@@`) into the pattern and the option text.
fn split_options(body: &str) -> (&str, Option<&str>) {
    // For `/regex/$opts`, the `$` must come after the closing slash, since
    // `$` is also a regex anchor.
    let search_from = if body.starts_with('/') {
        body.rfind('/').unwrap_or(0)
    } else {
        0
    };
    let dollar = body
        .get(search_from..)
        .and_then(|tail| tail.rfind('$'))
        .map(|i| i + search_from);
    match dollar {
        Some(i) => (
            body.get(..i).unwrap_or(""),
            Some(body.get(i + 1..).unwrap_or("")),
        ),
        None => (body, None),
    }
}

/// A key identifying a filter for `$badfilter`: the pattern plus its
/// options in sorted order, without `badfilter` itself.
pub(crate) fn badfilter_key(line: &str) -> String {
    let (exception, body) = match line.strip_prefix("@@") {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let (pattern, options) = split_options(body);
    let mut opts: Vec<String> = options
        .map(|o| {
            o.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty() && s != "badfilter")
                .collect()
        })
        .unwrap_or_default();
    opts.sort_unstable();
    opts.dedup();
    format!(
        "{}{}${}",
        if exception { "@@" } else { "" },
        pattern,
        opts.join(",")
    )
}

/// Parses a network filter line (already trimmed, not a comment).
pub(crate) fn parse(line: &str) -> Result<Parsed, Reject> {
    let (exception, body) = match line.strip_prefix("@@") {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let (pattern_text, options) = split_options(body);
    if pattern_text.contains(char::is_whitespace) {
        // URLs contain no whitespace. (Hosts-file lines also land here; they
        // are not part of this syntax.) Whitespace in option values is left
        // to the option parser: only unsupported options (`$csp=`) have it.
        return Err(Reject::Invalid);
    }

    let mut match_case = false;
    let mut important = false;
    let mut badfilter = false;
    let mut party = Party::Any;
    let mut domains = None;
    let mut pos_types: u16 = 0;
    let mut neg_types: u16 = 0;
    let mut page_flags: u8 = 0;

    if let Some(options) = options {
        for opt in options.split(',') {
            if opt.is_empty() {
                return Err(Reject::Invalid);
            }
            let (negated, opt) = match opt.strip_prefix('~') {
                Some(rest) => (true, rest),
                None => (false, opt),
            };
            let (name, value) = match opt.split_once('=') {
                Some((n, v)) => (n, Some(v)),
                None => (opt, None),
            };
            let name = name.to_ascii_lowercase();
            if value.is_some() && !matches!(name.as_str(), "domain" | "from") {
                return Err(Reject::Unsupported);
            }
            match name.as_str() {
                "third-party" | "3p" => {
                    party = if negated { Party::First } else { Party::Third };
                }
                "first-party" | "1p" => {
                    party = if negated { Party::Third } else { Party::First };
                }
                "match-case" => match_case = !negated,
                "important" if !negated => important = true,
                "badfilter" if !negated => badfilter = true,
                "domain" | "from" if !negated => {
                    let value = value.ok_or(Reject::Invalid)?;
                    domains = Some(DomainList::parse(value, '|')?);
                }
                "all" if !negated => pos_types |= ALL_TYPES,
                "elemhide" | "ehide" if !negated && exception => page_flags |= PAGE_ELEMHIDE,
                "generichide" | "ghide" if !negated && exception => {
                    page_flags |= PAGE_GENERICHIDE;
                }
                "specifichide" | "shide" if !negated && exception => {
                    page_flags |= PAGE_SPECIFICHIDE;
                }
                other => {
                    let bit = ResourceType::option_bit(other).ok_or(Reject::Unsupported)?;
                    if negated {
                        neg_types |= bit;
                    } else {
                        pos_types |= bit;
                    }
                }
            }
        }
    }

    let types = if pos_types == 0 {
        if page_flags != 0 && neg_types == 0 {
            // Only page-level options such as `$generichide`.
            0
        } else {
            DEFAULT_TYPES & !neg_types
        }
    } else {
        pos_types & !neg_types
    };
    if exception && types & DOCUMENT_BIT != 0 {
        page_flags |= PAGE_DOCUMENT;
    }
    if types == 0 && page_flags == 0 {
        return Err(Reject::Unsupported);
    }

    let pattern = parse_pattern(pattern_text, match_case)?;
    if options.is_none() && is_match_all(&pattern) {
        // A bare `*` or empty filter would block everything.
        return Err(Reject::Invalid);
    }

    Ok(Parsed {
        filter: NetworkFilter {
            pattern,
            match_case,
            exception,
            important,
            types,
            page_flags,
            party,
            domains,
            raw: line.into(),
        },
        badfilter,
    })
}

fn is_match_all(p: &Pattern) -> bool {
    matches!(p, Pattern::Glob { bytes, .. } if bytes.is_empty())
}

fn parse_pattern(text: &str, match_case: bool) -> Result<Pattern, Reject> {
    if text.len() > 2 && text.starts_with('/') && text.ends_with('/') {
        let inner = text.get(1..text.len() - 1).ok_or(Reject::Invalid)?;
        return regex::RegexBuilder::new(inner)
            .case_insensitive(!match_case)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map(Pattern::Regex)
            .map_err(|_| Reject::BadRegex);
    }

    let (mut anchor, rest) = if let Some(r) = text.strip_prefix("||") {
        (Anchor::Host, r)
    } else if let Some(r) = text.strip_prefix('|') {
        (Anchor::Start, r)
    } else {
        (Anchor::None, text)
    };
    let (mut end, rest) = match rest.strip_suffix('|') {
        Some(r) => (true, r),
        None => (false, rest),
    };

    let mut bytes: Vec<u8> = Vec::with_capacity(rest.len());
    for b in rest.bytes() {
        if b == b'*' && bytes.last() == Some(&b'*') {
            continue;
        }
        bytes.push(if match_case {
            b
        } else {
            b.to_ascii_lowercase()
        });
    }
    if bytes.first() == Some(&b'*') {
        anchor = Anchor::None;
        bytes.remove(0);
    }
    if bytes.last() == Some(&b'*') {
        end = false;
        bytes.pop();
    }
    if bytes.is_empty() {
        anchor = Anchor::None;
        end = false;
    }
    Ok(Pattern::Glob {
        bytes: bytes.into_boxed_slice(),
        anchor,
        end,
    })
}

/// The ABP separator class: anything but a letter, a digit, or `_-.%`.
/// The end of the URL also matches `^`, handled by the matcher.
fn is_separator(b: u8) -> bool {
    !(b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'%'))
}

/// Matches `pat` against `text` starting at `start`.
///
/// `leading_star` lets the match begin anywhere at or after `start`.
/// With `end_anchor` the match must reach the end of `text`; otherwise any
/// prefix match is enough. Classic linear-backtracking wildcard matching: on
/// mismatch, resume after the most recent `*` with one more byte consumed.
fn glob_at(pat: &[u8], text: &[u8], start: usize, leading_star: bool, end_anchor: bool) -> bool {
    let mut p = 0usize;
    let mut t = start;
    let mut star: Option<(usize, usize)> = leading_star.then_some((0, start));
    loop {
        match pat.get(p) {
            None => {
                if !end_anchor || t == text.len() {
                    return true;
                }
            }
            Some(b'*') => {
                p += 1;
                star = Some((p, t));
                continue;
            }
            Some(&c) => match text.get(t) {
                Some(&tc) => {
                    if c == tc || (c == b'^' && is_separator(tc)) {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
                None => {
                    if c == b'^' {
                        p += 1;
                        continue;
                    }
                }
            },
        }
        match star {
            Some((sp, st)) if st < text.len() => {
                star = Some((sp, st + 1));
                p = sp;
                t = st + 1;
            }
            _ => return false,
        }
    }
}

impl NetworkFilter {
    /// Whether the URL matches the pattern.
    pub(crate) fn matches_url(&self, target: &Target<'_>) -> bool {
        match &self.pattern {
            Pattern::Regex(re) => re.is_match(&target.original),
            Pattern::Glob { bytes, anchor, end } => {
                let text = if self.match_case {
                    target.original.as_bytes()
                } else {
                    target.lower.as_bytes()
                };
                match anchor {
                    Anchor::None => glob_at(bytes, text, 0, true, *end),
                    Anchor::Start => glob_at(bytes, text, 0, false, *end),
                    Anchor::Host => target
                        .anchors
                        .iter()
                        .any(|&s| glob_at(bytes, text, s, false, *end)),
                }
            }
        }
    }

    /// Whether the party and domain options allow this request.
    pub(crate) fn matches_context(&self, source_host: &str, third_party: bool) -> bool {
        let party_ok = match self.party {
            Party::Any => true,
            Party::First => !third_party,
            Party::Third => third_party,
        };
        party_ok && self.domains.as_ref().is_none_or(|d| d.allows(source_host))
    }

    /// Hashes of the pattern's alphanumeric runs that are guaranteed to be
    /// whole tokens of any matching URL (bounded on both sides by a literal
    /// non-alphanumeric byte, `^`, an anchor, or the end anchor). Only those
    /// can be used to look the filter up from a URL's tokens.
    pub(crate) fn index_tokens(&self) -> Vec<u64> {
        let Pattern::Glob { bytes, anchor, end } = &self.pattern else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if !bytes.get(i).is_some_and(u8::is_ascii_alphanumeric) {
                i += 1;
                continue;
            }
            let start = i;
            let mut h = Fnv::new();
            while let Some(&b) = bytes.get(i).filter(|b| b.is_ascii_alphanumeric()) {
                h.push(b.to_ascii_lowercase());
                i += 1;
            }
            let left_ok = match start.checked_sub(1).and_then(|j| bytes.get(j)) {
                None => *anchor != Anchor::None,
                Some(&b) => b != b'*',
            };
            let right_ok = match bytes.get(i) {
                None => *end,
                Some(&b) => b != b'*',
            };
            if left_ok && right_ok {
                out.push(h.finish());
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Hash of a domain for the domain index.
pub(crate) fn domain_hash(domain: &str) -> u64 {
    hash_lower(domain.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn f(line: &str) -> NetworkFilter {
        parse(line).unwrap().filter
    }

    fn m(line: &str, url: &str) -> bool {
        f(line).matches_url(&Target::new(Cow::Borrowed(url)))
    }

    #[test]
    fn plain_substring() {
        assert!(m("/banner/", "https://example.com/banners/"));
        assert!(m("ads.js", "https://example.com/js/ads.js?x=1"));
        assert!(m("ADS.JS", "https://example.com/js/ads.js"));
        assert!(!m("ads.js", "https://example.com/js/ad.js"));
    }

    #[test]
    fn wildcards() {
        assert!(m("/ad*.gif", "https://x.com/advert/banner.gif"));
        assert!(m("a*b*c", "https://x.com/aXXbYYc"));
        assert!(!m("a*b*c", "https://x.com/aXXcYYb"));
        assert!(m("*ads*", "https://x.com/ads"));
    }

    #[test]
    fn separator() {
        assert!(m("||example.com^", "https://example.com/"));
        assert!(m("||example.com^", "https://example.com:8000/"));
        assert!(m("||example.com^", "https://example.com"));
        assert!(!m("||example.com^", "https://example.com.ar/"));
        assert!(m("^foo^", "https://x.com/foo?"));
        assert!(m("^foo^", "https://x.com/foo"));
        assert!(!m("^foo^", "https://x.com/foo.bar"));
        assert!(!m("^foo^", "https://x.com/foo_bar"));
        assert!(!m("^foo^", "https://x.com/foo%20"));
        assert!(m("^foo^", "https://x.com/foo&a"));
    }

    #[test]
    fn host_anchor() {
        assert!(m("||ads.example.com", "https://ads.example.com/x"));
        assert!(m(
            "||example.com/banner",
            "http://sub.example.com/banner.gif"
        ));
        assert!(!m(
            "||example.com/banner",
            "http://badexample.com/banner.gif"
        ));
        assert!(!m("||example.com", "https://x.com/?u=example.com"));
        assert!(m("||example.com", "https://user@example.com/"));
    }

    #[test]
    fn start_end_anchors() {
        assert!(m("|https://ads.", "https://ads.example.com/"));
        assert!(!m("|https://ads.", "http://x.com/?https://ads."));
        assert!(m("swf|", "https://x.com/a.swf"));
        assert!(!m("swf|", "https://x.com/a.swf?x"));
        assert!(m("|https://x.com/a.swf|", "https://x.com/a.swf"));
        assert!(m("ad*.swf|", "https://x.com/ad/b.swf"));
    }

    #[test]
    fn match_case() {
        assert!(m("Banner$match-case", "https://x.com/Banner"));
        assert!(!m("Banner$match-case", "https://x.com/banner"));
        assert!(m("/Ban+er/$match-case", "https://x.com/Banner"));
        assert!(!m("/Ban+er/$match-case", "https://x.com/banner"));
    }

    #[test]
    fn regex_filters() {
        assert!(m("/banner\\d+\\.gif/", "https://x.com/BANNER12.gif"));
        assert!(!m("/banner\\d+\\.gif/", "https://x.com/banner.gif"));
        assert!(m("/ads$/$script", "https://x.com/ads"));
        assert_eq!(parse("/a(/").unwrap_err(), Reject::BadRegex);
        // A dollar inside the regex is not the option separator.
        assert!(m("/x$/", "https://a.com/x"));
    }

    #[test]
    fn options() {
        let p = f("||x.com^$script,image,third-party,domain=a.com|~b.a.com");
        assert_eq!(
            p.types,
            ResourceType::Script.bit() | ResourceType::Image.bit()
        );
        assert_eq!(p.party, Party::Third);
        assert!(p.domains.is_some());
        let p = f("ads$~script");
        assert_eq!(p.types & ResourceType::Script.bit(), 0);
        assert_ne!(p.types & ResourceType::Image.bit(), 0);
        assert_eq!(p.types & DOCUMENT_BIT, 0);
        assert_eq!(f("x$1p").party, Party::First);
        assert_eq!(f("x$~3p").party, Party::First);
        assert_eq!(f("x$~first-party").party, Party::Third);
        assert!(f("x$important").important);
        assert!(parse("x$badfilter").unwrap().badfilter);
        assert_eq!(f("x$all").types, ALL_TYPES);
        assert_eq!(f("x$from=a.com").domains, f("x$domain=a.com").domains);
        assert_eq!(f("x$Script").types, ResourceType::Script.bit());
    }

    #[test]
    fn page_options() {
        let p = f("@@||x.com^$document");
        assert!(p.exception);
        assert_eq!(p.page_flags, PAGE_DOCUMENT);
        let p = f("@@||x.com^$generichide");
        assert_eq!(p.page_flags, PAGE_GENERICHIDE);
        assert_eq!(p.types, 0);
        assert_eq!(parse("||x.com^$elemhide").unwrap_err(), Reject::Unsupported);
    }

    #[test]
    fn unsupported_and_invalid() {
        for line in [
            "x$popup",
            "x$redirect=noop.js",
            "x$csp=default-src",
            "x$csp=script-src 'self' data:",
            "x$removeparam=utm",
            "x$domain=google.*",
            "x$~important",
            "x$~object,~script,~image,~stylesheet,~font,~media,~xhr,~ping,~websocket,~other,~frame",
        ] {
            assert_eq!(parse(line).unwrap_err(), Reject::Unsupported, "{line}");
        }
        for line in ["*", "", "a b", "x$script,,image", "x$domain="] {
            assert_eq!(parse(line).unwrap_err(), Reject::Invalid, "{line}");
        }
        // Options make a match-all pattern acceptable.
        assert!(parse("*$script,domain=a.com").is_ok());
    }

    #[test]
    fn tokens() {
        assert_eq!(f("||ads.example.com^").index_tokens().len(), 3);
        // `ads` could be inside `uploads`; `banner` could be `banners`.
        assert!(f("ads/banner").index_tokens().is_empty());
        assert_eq!(f("/ads/banner").index_tokens().len(), 1);
        assert_eq!(f("/ads/banner|").index_tokens().len(), 2);
        assert_eq!(f("|http://ads*x").index_tokens().len(), 1);
        assert!(f("/re/").index_tokens().is_empty());
    }

    #[test]
    fn badfilter_keys() {
        assert_eq!(
            badfilter_key("||x.com^$script,badfilter,image"),
            badfilter_key("||x.com^$image,script")
        );
        assert_ne!(badfilter_key("@@x$badfilter"), badfilter_key("x"));
        assert_eq!(badfilter_key("@@x$badfilter"), badfilter_key("@@x"));
    }

    #[test]
    fn glob_edge_cases() {
        assert!(glob_at(b"", b"", 0, false, true));
        assert!(glob_at(b"^", b"", 0, false, true));
        assert!(!glob_at(b"a^b", b"a", 0, false, false));
        assert!(glob_at(b"a*", b"a", 0, false, true));
        assert!(glob_at(b"*a*a*a*a", b"aaaa", 0, false, true));
        assert!(!glob_at(
            b"*a*a*a*a*b",
            b"aaaaaaaaaaaaaaaaaaaaaaaa",
            0,
            false,
            true
        ));
    }
}
