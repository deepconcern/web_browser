//! An in-memory, private HTTP cache ([RFC 9111](https://www.rfc-editor.org/rfc/rfc9111)).
//!
//! Entries are keyed by (top-level site partition, URL without fragment), so
//! one site cannot probe another site's cache. Only `GET` responses are
//! stored; `Vary` is honored by keeping one variant per key and checking the
//! selecting headers on lookup.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::date::parse_cookie_date;
use crate::types::{CacheMode, Headers, HttpVersion};

/// Upper bound on heuristic freshness (§4.2.2 leaves it to us).
const MAX_HEURISTIC: i64 = 24 * 60 * 60;

/// Status codes that are heuristically cacheable (RFC 9110 §15.1). 206 is
/// excluded because we do not combine partial content.
fn heuristically_cacheable(status: u16) -> bool {
    matches!(
        status,
        200 | 203 | 204 | 300 | 301 | 308 | 404 | 405 | 410 | 414 | 501
    )
}

/// Parsed `Cache-Control` directives (§5.2): lowercase names, unquoted values.
#[derive(Debug, Default)]
pub(crate) struct CacheControl {
    list: Vec<(String, Option<String>)>,
}

impl CacheControl {
    pub(crate) fn parse(headers: &Headers) -> CacheControl {
        let mut list = Vec::new();
        for value in headers.get_all("cache-control") {
            for part in split_directives(value) {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let (name, arg) = match part.split_once('=') {
                    Some((n, v)) => {
                        let v = v.trim();
                        let v = v
                            .strip_prefix('"')
                            .and_then(|v| v.strip_suffix('"'))
                            .unwrap_or(v);
                        (n.trim(), Some(v.to_owned()))
                    }
                    None => (part, None),
                };
                list.push((name.to_ascii_lowercase(), arg));
            }
        }
        CacheControl { list }
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.list.iter().any(|(n, _)| n == name)
    }

    /// A delta-seconds argument (§1.2.2); the first occurrence wins.
    /// Invalid values make the directive count as absent, except that
    /// overflow saturates.
    pub(crate) fn seconds(&self, name: &str) -> Option<i64> {
        let (_, arg) = self.list.iter().find(|(n, _)| n == name)?;
        let arg = arg.as_deref()?;
        if arg.is_empty() || !arg.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(arg.bytes().fold(0i64, |n, b| {
            n.saturating_mul(10).saturating_add(i64::from(b - b'0'))
        }))
    }
}

/// Splits on commas that are not inside a quoted string.
fn split_directives(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;
    for (i, c) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                out.push(value.get(start..i).unwrap_or(""));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(value.get(start..).unwrap_or(""));
    out
}

fn header_time(headers: &Headers, name: &str) -> Option<i64> {
    headers.get(name).and_then(parse_cookie_date)
}

fn normalize(v: Option<String>) -> Option<String> {
    v.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// A response held in (or returned by) the cache.
#[derive(Clone, Debug)]
pub(crate) struct CachedResponse {
    pub(crate) status: u16,
    pub(crate) headers: Headers,
    pub(crate) body: Arc<Vec<u8>>,
    pub(crate) version: HttpVersion,
}

#[derive(Clone, Debug)]
struct Entry {
    response: CachedResponse,
    /// Request headers named by `Vary`, with the values they had.
    vary: Vec<(String, Option<String>)>,
    request_time: i64,
    response_time: i64,
    last_used: u64,
}

impl Entry {
    fn size(&self) -> usize {
        self.response.body.len()
            + self
                .response
                .headers
                .iter()
                .map(|(n, v)| n.len() + v.len())
                .sum::<usize>()
    }

    /// Freshness lifetime (§4.2.1), for a private cache.
    fn freshness_lifetime(&self) -> i64 {
        let h = &self.response.headers;
        let cc = CacheControl::parse(h);
        if let Some(max_age) = cc.seconds("max-age") {
            return max_age;
        }
        let date = header_time(h, "date").unwrap_or(self.response_time);
        if let Some(expires) = h.get("expires") {
            // An invalid Expires (e.g. "0") means already expired.
            return parse_cookie_date(expires).map_or(0, |e| e.saturating_sub(date));
        }
        if (heuristically_cacheable(self.response.status) || cc.has("public"))
            && let Some(lm) = header_time(h, "last-modified")
        {
            return (date.saturating_sub(lm) / 10).clamp(0, MAX_HEURISTIC);
        }
        0
    }

    /// Current age (§4.2.3).
    fn current_age(&self, now: i64) -> i64 {
        let h = &self.response.headers;
        let date = header_time(h, "date").unwrap_or(self.response_time);
        let age_value = h
            .get("age")
            .and_then(|a| a.trim().parse::<i64>().ok())
            .unwrap_or(0)
            .max(0);
        let apparent_age = (self.response_time - date).max(0);
        let response_delay = (self.response_time - self.request_time).max(0);
        let corrected_age_value = age_value.saturating_add(response_delay);
        let corrected_initial_age = apparent_age.max(corrected_age_value);
        let resident_time = (now - self.response_time).max(0);
        corrected_initial_age.saturating_add(resident_time)
    }

    fn has_validators(&self) -> bool {
        self.response.headers.contains("etag") || self.response.headers.contains("last-modified")
    }
}

/// What a cache lookup found.
#[derive(Debug)]
pub(crate) enum Lookup {
    /// Nothing usable.
    Miss,
    /// A response that may be used as is.
    Fresh(CachedResponse),
    /// A stored response that must be revalidated first, with the
    /// conditional headers to send.
    Revalidate(Vec<(&'static str, String)>),
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<(String, String), Entry>,
    total: usize,
    tick: u64,
}

/// The memory cache.
#[derive(Debug)]
pub(crate) struct MemoryCache {
    inner: Mutex<Inner>,
    capacity: usize,
    max_entry: usize,
}

fn vary_names(headers: &Headers) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for v in headers.get_all("vary") {
        for name in v.split(',') {
            let name = name.trim().to_ascii_lowercase();
            if name == "*" {
                return None;
            }
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    Some(names)
}

impl MemoryCache {
    pub(crate) fn new(capacity: usize, max_entry: usize) -> MemoryCache {
        MemoryCache {
            inner: Mutex::new(Inner::default()),
            capacity,
            max_entry,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Looks up a GET request (§4).
    pub(crate) fn lookup(
        &self,
        partition: &str,
        url: &str,
        request_headers: &Headers,
        mode: CacheMode,
        now: i64,
    ) -> Lookup {
        if matches!(mode, CacheMode::NoStore | CacheMode::Reload) {
            return Lookup::Miss;
        }
        let request_cc = CacheControl::parse(request_headers);
        if request_cc.has("no-store") {
            return Lookup::Miss;
        }
        let mut inner = self.lock();
        inner.tick += 1;
        let tick = inner.tick;
        let Some(entry) = inner
            .entries
            .get_mut(&(partition.to_owned(), url.to_owned()))
        else {
            return Lookup::Miss;
        };
        // §4.1: the selecting headers must match.
        let vary_ok = entry
            .vary
            .iter()
            .all(|(name, stored)| normalize(request_headers.get_combined(name)) == *stored);
        if !vary_ok {
            return Lookup::Miss;
        }
        entry.last_used = tick;
        if mode == CacheMode::ForceCache {
            return Lookup::Fresh(entry.response.clone());
        }
        let response_cc = CacheControl::parse(&entry.response.headers);
        let lifetime = entry.freshness_lifetime();
        let age = entry.current_age(now);
        let pragma_no_cache = !request_headers.contains("cache-control")
            && request_headers
                .get("pragma")
                .is_some_and(|p| p.to_ascii_lowercase().contains("no-cache"));
        let mut fresh = lifetime > age;
        if let Some(max_age) = request_cc.seconds("max-age") {
            fresh &= age <= max_age;
        }
        if let Some(min_fresh) = request_cc.seconds("min-fresh") {
            fresh &= lifetime >= age.saturating_add(min_fresh);
        }
        if !fresh
            && !response_cc.has("must-revalidate")
            && !response_cc.has("no-cache")
            && let Some(max_stale) = request_cc.seconds("max-stale")
        {
            fresh = age.saturating_sub(lifetime) <= max_stale;
        }
        let must_validate = mode == CacheMode::NoCache
            || request_cc.has("no-cache")
            || pragma_no_cache
            || response_cc.has("no-cache");
        if fresh && !must_validate {
            return Lookup::Fresh(entry.response.clone());
        }
        let mut conditional = Vec::new();
        if let Some(etag) = entry.response.headers.get("etag") {
            conditional.push(("if-none-match", etag.to_owned()));
        }
        if let Some(lm) = entry.response.headers.get("last-modified") {
            conditional.push(("if-modified-since", lm.to_owned()));
        }
        if conditional.is_empty() {
            Lookup::Miss
        } else {
            Lookup::Revalidate(conditional)
        }
    }

    /// Stores a GET response if RFC 9111 §3 allows it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn store(
        &self,
        partition: &str,
        url: &str,
        request_headers: &Headers,
        response: CachedResponse,
        mode: CacheMode,
        request_time: i64,
        response_time: i64,
    ) {
        if mode == CacheMode::NoStore {
            return;
        }
        let request_cc = CacheControl::parse(request_headers);
        let response_cc = CacheControl::parse(&response.headers);
        if request_cc.has("no-store")
            || response_cc.has("no-store")
            || request_headers.contains("authorization")
            || matches!(response.status, 206 | 304)
            || response.status < 200
        {
            return;
        }
        let Some(names) = vary_names(&response.headers) else {
            return;
        };
        let vary = names
            .into_iter()
            .map(|n| {
                let v = normalize(request_headers.get_combined(&n));
                (n, v)
            })
            .collect();
        let entry = Entry {
            response,
            vary,
            request_time,
            response_time,
            last_used: 0,
        };
        let explicit = response_cc.has("max-age")
            || entry.response.headers.contains("expires")
            || response_cc.has("public");
        let storable = explicit || heuristically_cacheable(entry.response.status);
        // An entry that is never fresh and cannot be revalidated is useless.
        let useful = entry.freshness_lifetime() > 0 || entry.has_validators();
        if !storable || !useful || entry.size() > self.max_entry {
            self.invalidate(partition, url);
            return;
        }
        let mut inner = self.lock();
        inner.tick += 1;
        let mut entry = entry;
        entry.last_used = inner.tick;
        let size = entry.size();
        if let Some(old) = inner
            .entries
            .insert((partition.to_owned(), url.to_owned()), entry)
        {
            inner.total = inner.total.saturating_sub(old.size());
        }
        inner.total += size;
        while inner.total > self.capacity {
            let victim = inner
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            match victim.and_then(|k| inner.entries.remove(&k)) {
                Some(e) => inner.total = inner.total.saturating_sub(e.size()),
                None => break,
            }
        }
    }

    /// Freshens a stored response with a 304 (§4.3.4) and returns it.
    pub(crate) fn freshen(
        &self,
        partition: &str,
        url: &str,
        not_modified: &Headers,
        request_time: i64,
        response_time: i64,
    ) -> Option<CachedResponse> {
        let mut inner = self.lock();
        let key = (partition.to_owned(), url.to_owned());
        let entry = inner.entries.get_mut(&key)?;
        let old_size = entry.size();
        // Replace each stored header that the 304 carries (§3.2), except
        // the framing headers that describe the 304 itself.
        let mut updated = entry.response.headers.clone();
        for (name, _) in not_modified.iter() {
            if matches!(
                name,
                "content-length" | "content-encoding" | "transfer-encoding"
            ) {
                continue;
            }
            updated.remove(name);
        }
        for (name, value) in not_modified.iter() {
            if !matches!(
                name,
                "content-length" | "content-encoding" | "transfer-encoding"
            ) {
                updated.append(name, value);
            }
        }
        entry.response.headers = updated;
        entry.request_time = request_time;
        entry.response_time = response_time;
        let response = entry.response.clone();
        let new_size = entry.size();
        inner.total = inner.total.saturating_sub(old_size) + new_size;
        Some(response)
    }

    /// Removes a stored response (§4.4 invalidation).
    pub(crate) fn invalidate(&self, partition: &str, url: &str) {
        let mut inner = self.lock();
        if let Some(e) = inner
            .entries
            .remove(&(partition.to_owned(), url.to_owned()))
        {
            inner.total = inner.total.saturating_sub(e.size());
        }
    }

    /// Drops everything.
    pub(crate) fn clear(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.total = 0;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_700_000_000;
    const URL: &str = "https://a.test/x";
    const P: &str = "https://a.test";

    fn resp(status: u16, headers: &[(&str, &str)], body: &[u8]) -> CachedResponse {
        let mut h = Headers::new();
        for (n, v) in headers {
            h.append(n, v);
        }
        CachedResponse {
            status,
            headers: h,
            body: Arc::new(body.to_vec()),
            version: HttpVersion::Http11,
        }
    }

    fn store(c: &MemoryCache, r: CachedResponse) {
        c.store(P, URL, &Headers::new(), r, CacheMode::Default, T, T);
    }

    fn look(c: &MemoryCache, at: i64) -> Lookup {
        c.lookup(P, URL, &Headers::new(), CacheMode::Default, at)
    }

    #[test]
    fn directives() {
        let mut h = Headers::new();
        h.append(
            "cache-control",
            "max-age=\"60\", No-Cache, private=\"a, b\"",
        );
        h.append("cache-control", "max-age=10, s-maxage=x");
        let cc = CacheControl::parse(&h);
        assert_eq!(cc.seconds("max-age"), Some(60));
        assert!(cc.has("no-cache"));
        assert!(cc.has("private"));
        assert_eq!(cc.seconds("s-maxage"), None);
        assert!(!cc.has("a"));
    }

    #[test]
    fn max_age_freshness() {
        let c = MemoryCache::new(1 << 20, 1 << 20);
        store(&c, resp(200, &[("cache-control", "max-age=100")], b"hi"));
        assert!(matches!(look(&c, T + 50), Lookup::Fresh(r) if *r.body == b"hi"));
        assert!(
            matches!(look(&c, T + 101), Lookup::Miss),
            "stale without validators"
        );
        // The partition matters.
        assert!(matches!(
            c.lookup(
                "https://b.test",
                URL,
                &Headers::new(),
                CacheMode::Default,
                T
            ),
            Lookup::Miss
        ));
    }

    #[test]
    fn age_and_date() {
        let c = MemoryCache::new(1 << 20, 1 << 20);
        store(
            &c,
            resp(200, &[("cache-control", "max-age=100"), ("age", "90")], b""),
        );
        assert!(matches!(look(&c, T + 5), Lookup::Fresh(_)));
        assert!(matches!(look(&c, T + 11), Lookup::Miss));
    }

    #[test]
    fn expires_and_heuristics() {
        let c = MemoryCache::new(1 << 20, 1 << 20);
        store(
            &c,
            resp(
                200,
                &[
                    ("date", "Tue, 14 Nov 2023 22:13:20 GMT"),
                    ("expires", "Tue, 14 Nov 2023 22:14:20 GMT"),
                ],
                b"",
            ),
        );
        assert!(matches!(look(&c, T + 59), Lookup::Fresh(_)));
        assert!(matches!(look(&c, T + 61), Lookup::Miss));
        // Expires: 0 is already stale; with no validator it is not stored.
        store(&c, resp(200, &[("expires", "0")], b""));
        assert_eq!(c.len(), 0);
        // Heuristic: 10% of (Date - Last-Modified) = 100s.
        store(
            &c,
            resp(
                200,
                &[
                    ("date", "Tue, 14 Nov 2023 22:13:20 GMT"),
                    ("last-modified", "Tue, 14 Nov 2023 21:56:40 GMT"),
                ],
                b"",
            ),
        );
        assert!(matches!(look(&c, T + 99), Lookup::Fresh(_)));
        assert!(
            matches!(look(&c, T + 101), Lookup::Revalidate(h) if h[0].0 == "if-modified-since")
        );
    }

    #[test]
    fn not_storable() {
        let c = MemoryCache::new(1 << 20, 1 << 20);
        store(
            &c,
            resp(200, &[("cache-control", "no-store, max-age=100")], b""),
        );
        store(
            &c,
            resp(200, &[("cache-control", "max-age=100"), ("vary", "*")], b""),
        );
        store(&c, resp(206, &[("cache-control", "max-age=100")], b""));
        store(&c, resp(201, &[], b""));
        let mut auth = Headers::new();
        auth.append("authorization", "x");
        c.store(
            P,
            URL,
            &auth,
            resp(200, &[("cache-control", "max-age=100")], b""),
            CacheMode::Default,
            T,
            T,
        );
        c.store(
            P,
            URL,
            &Headers::new(),
            resp(200, &[("cache-control", "max-age=100")], b""),
            CacheMode::NoStore,
            T,
            T,
        );
        assert_eq!(c.len(), 0);
        store(&c, resp(201, &[("cache-control", "max-age=100")], b""));
        assert_eq!(
            c.len(),
            1,
            "explicit freshness makes any final status storable"
        );
    }

    #[test]
    fn no_cache_and_validation() {
        let c = MemoryCache::new(1 << 20, 1 << 20);
        store(
            &c,
            resp(
                200,
                &[("cache-control", "no-cache"), ("etag", "\"v1\"")],
                b"body",
            ),
        );
        match look(&c, T) {
            Lookup::Revalidate(h) => assert_eq!(h, vec![("if-none-match", "\"v1\"".to_owned())]),
            other => panic!("{other:?}"),
        }
        let mut nm = Headers::new();
        nm.append("etag", "\"v1\"");
        nm.append("cache-control", "max-age=60");
        nm.append("content-length", "0");
        let r = c.freshen(P, URL, &nm, T + 1, T + 1).unwrap();
        assert_eq!(*r.body, b"body");
        assert_eq!(r.headers.get("cache-control"), Some("max-age=60"));
        assert!(!r.headers.contains("content-length"));
        assert!(matches!(look(&c, T + 10), Lookup::Fresh(_)));
    }

    #[test]
    fn request_directives_and_modes() {
        let c = MemoryCache::new(1 << 20, 1 << 20);
        store(
            &c,
            resp(200, &[("cache-control", "max-age=100"), ("etag", "e")], b""),
        );
        let mut h = Headers::new();
        h.append("cache-control", "max-age=0");
        assert!(matches!(
            c.lookup(P, URL, &h, CacheMode::Default, T + 1),
            Lookup::Revalidate(_)
        ));
        let mut h = Headers::new();
        h.append("pragma", "no-cache");
        assert!(matches!(
            c.lookup(P, URL, &h, CacheMode::Default, T),
            Lookup::Revalidate(_)
        ));
        let mut h = Headers::new();
        h.append("cache-control", "max-stale=50");
        assert!(matches!(
            c.lookup(P, URL, &h, CacheMode::Default, T + 140),
            Lookup::Fresh(_)
        ));
        let mut h = Headers::new();
        h.append("cache-control", "min-fresh=50");
        assert!(matches!(
            c.lookup(P, URL, &h, CacheMode::Default, T + 60),
            Lookup::Revalidate(_)
        ));
        assert!(matches!(
            c.lookup(P, URL, &Headers::new(), CacheMode::NoCache, T),
            Lookup::Revalidate(_)
        ));
        assert!(matches!(
            c.lookup(P, URL, &Headers::new(), CacheMode::Reload, T),
            Lookup::Miss
        ));
        assert!(matches!(
            c.lookup(P, URL, &Headers::new(), CacheMode::ForceCache, T + 9999),
            Lookup::Fresh(_)
        ));
        c.invalidate(P, URL);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn vary() {
        let c = MemoryCache::new(1 << 20, 1 << 20);
        let mut req = Headers::new();
        req.append("accept-language", "en");
        c.store(
            P,
            URL,
            &req,
            resp(
                200,
                &[
                    ("cache-control", "max-age=100"),
                    ("vary", "Accept-Language"),
                ],
                b"",
            ),
            CacheMode::Default,
            T,
            T,
        );
        assert!(matches!(
            c.lookup(P, URL, &req, CacheMode::Default, T),
            Lookup::Fresh(_)
        ));
        let mut other = Headers::new();
        other.append("accept-language", "fr");
        assert!(matches!(
            c.lookup(P, URL, &other, CacheMode::Default, T),
            Lookup::Miss
        ));
        assert!(matches!(
            c.lookup(P, URL, &Headers::new(), CacheMode::Default, T),
            Lookup::Miss
        ));
    }

    #[test]
    fn capacity() {
        // Each entry is its body plus 22 bytes of headers.
        let c = MemoryCache::new(100, 80);
        let r = |b: &[u8]| resp(200, &[("cache-control", "max-age=9")], b);
        c.store(
            P,
            "u1",
            &Headers::new(),
            r(&[0; 40]),
            CacheMode::Default,
            T,
            T,
        );
        c.store(
            P,
            "u2",
            &Headers::new(),
            r(&[0; 40]),
            CacheMode::Default,
            T,
            T,
        );
        assert_eq!(c.len(), 1, "first entry evicted to fit the second");
        c.store(
            P,
            "u3",
            &Headers::new(),
            r(&[0; 70]),
            CacheMode::Default,
            T,
            T,
        );
        assert!(
            matches!(
                c.lookup(P, "u3", &Headers::new(), CacheMode::Default, T),
                Lookup::Miss
            ),
            "too big for one entry"
        );
        c.clear();
        assert_eq!(c.len(), 0);
    }
}
