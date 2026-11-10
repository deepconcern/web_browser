//! The cookie jar: [RFC 6265bis](https://datatracker.ietf.org/doc/html/draft-ietf-httpbis-rfc6265bis)
//! (draft 20) with two privacy rules on top:
//!
//! - **Partitioned by top-level site.** Every cookie belongs to the site of
//!   the top-level document it was set under, and is only sent under that
//!   same top-level site.
//! - **No third-party cookies.** A request to a site other than the
//!   top-level site neither sends nor stores cookies.
//!
//! Persistence goes through the [`CookieStore`] trait so `wb-storage` can
//! back it later without this crate depending on it.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use wb_url::{Host, Site, Url, public_suffix};

use crate::date::{now, parse_cookie_date};
use crate::referrer::is_potentially_trustworthy;

/// Upper bound on any cookie lifetime (RFC 6265bis §5.5: 400 days).
const MAX_AGE_CAP: i64 = 400 * 24 * 60 * 60;
/// Maximum combined length of name and value (§5.6 step 5).
const MAX_NAME_VALUE: usize = 4096;
/// Maximum length of an attribute value (§5.6).
const MAX_ATTRIBUTE_VALUE: usize = 1024;
/// Cookies kept per (partition, domain) before evicting (§6.1 suggests ≥ 50).
const MAX_PER_DOMAIN: usize = 50;
/// Cookies kept in total before evicting (§6.1 suggests ≥ 3000).
const MAX_TOTAL: usize = 3000;
/// The earliest time we represent; used for "already expired".
const EARLIEST: i64 = i64::MIN / 2;

/// The `SameSite` attribute ([§4.1.2.7](https://datatracker.ietf.org/doc/html/draft-ietf-httpbis-rfc6265bis#section-4.1.2.7)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SameSite {
    /// `SameSite=None`: sent in cross-site contexts (must be `Secure`).
    None,
    /// `SameSite=Lax`: sent cross-site only on safe top-level navigations.
    Lax,
    /// `SameSite=Strict`: never sent cross-site.
    Strict,
    /// No (valid) attribute given; enforced like `Lax`.
    Default,
}

/// A stored cookie (RFC 6265bis §5.7 storage model), plus the top-level
/// site partition it belongs to. Times are Unix seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cookie {
    /// The serialized top-level site this cookie is partitioned under,
    /// e.g. `https://example.com`.
    pub partition: String,
    /// The cookie name (may be empty).
    pub name: String,
    /// The cookie value.
    pub value: String,
    /// The domain (canonical host form).
    pub domain: String,
    /// The path.
    pub path: String,
    /// When it expires; `None` for a session cookie.
    pub expires: Option<i64>,
    /// When it was first created.
    pub creation: i64,
    /// When it was last sent or read.
    pub last_access: i64,
    /// Whether it is only sent to exactly `domain` (no `Domain` attribute).
    pub host_only: bool,
    /// The `Secure` flag.
    pub secure: bool,
    /// The `HttpOnly` flag.
    pub http_only: bool,
    /// The `SameSite` setting.
    pub same_site: SameSite,
}

impl Cookie {
    fn same_identity(&self, other: &Cookie) -> bool {
        self.partition == other.partition
            && self.name == other.name
            && self.domain == other.domain
            && self.host_only == other.host_only
            && self.path == other.path
    }

    fn is_expired(&self, now: i64) -> bool {
        self.expires.is_some_and(|e| e <= now)
    }
}

/// Persistent backing for a [`CookieJar`]. The jar keeps its own in-memory
/// copy and calls the store on every change; `load` runs once, when the jar
/// is created. Implementations decide whether to keep session cookies
/// (`expires == None`); they should not survive a browser restart.
pub trait CookieStore: Send + Sync {
    /// Returns every stored cookie.
    fn load(&self) -> Vec<Cookie>;
    /// Inserts or replaces a cookie (identity: partition, name, domain,
    /// host-only flag and path).
    fn save(&self, cookie: &Cookie);
    /// Deletes a cookie (same identity as `save`).
    fn delete(&self, cookie: &Cookie);
}

/// Who is asking the jar, and in what context.
#[derive(Clone, Debug)]
pub(crate) struct CookieContext {
    /// The serialized top-level site.
    pub(crate) partition: String,
    /// Whether the request is same-site for `SameSite` purposes.
    pub(crate) same_site: bool,
    /// Whether this is a top-level navigation.
    pub(crate) top_level_navigation: bool,
    /// Whether the request method is safe.
    pub(crate) safe_method: bool,
    /// HTTP API (headers) or non-HTTP API (`document.cookie`).
    pub(crate) http_api: bool,
}

/// A cookie jar. Cheap to clone; clones share the same cookies.
#[derive(Clone)]
pub struct CookieJar {
    inner: Arc<Mutex<Vec<Cookie>>>,
    store: Option<Arc<dyn CookieStore>>,
}

impl fmt::Debug for CookieJar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CookieJar")
            .field("cookies", &self.lock().len())
            .field("persistent", &self.store.is_some())
            .finish()
    }
}

impl Default for CookieJar {
    fn default() -> Self {
        CookieJar::new()
    }
}

/// [Domain-match](https://datatracker.ietf.org/doc/html/draft-ietf-httpbis-rfc6265bis#section-5.1.3).
fn domain_match(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    let is_ip = host.starts_with('[') || host.parse::<std::net::Ipv4Addr>().is_ok();
    !is_ip
        && host.len() > domain.len()
        && host.ends_with(domain)
        && host.as_bytes().get(host.len() - domain.len() - 1) == Some(&b'.')
}

/// [Default-path](https://datatracker.ietf.org/doc/html/draft-ietf-httpbis-rfc6265bis#section-5.1.4).
fn default_path(url: &Url) -> String {
    let path = url.path();
    if !path.starts_with('/') {
        return "/".to_owned();
    }
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(i) => path.get(..i).unwrap_or("/").to_owned(),
    }
}

/// [Path-match](https://datatracker.ietf.org/doc/html/draft-ietf-httpbis-rfc6265bis#section-5.1.4).
fn path_match(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    request_path.starts_with(cookie_path)
        && (cookie_path.ends_with('/')
            || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/'))
}

fn has_forbidden_control(s: &str) -> bool {
    s.bytes()
        .any(|b| matches!(b, 0x00..=0x08 | 0x0A..=0x1F | 0x7F))
}

fn trim_wsp(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t')
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
}

/// A parsed `Set-Cookie` line (§5.6), before the storage model runs.
#[derive(Debug, Default, PartialEq)]
struct Parsed {
    name: String,
    value: String,
    /// Last `Expires`, as Unix seconds.
    expires: Option<i64>,
    /// Last `Max-Age`, already converted to an expiry time.
    max_age: Option<i64>,
    domain: Option<String>,
    path: Option<String>,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
}

/// Parses a `Set-Cookie` string (§5.6). `None` means "ignore it".
fn parse_set_cookie(input: &str, now: i64) -> Option<Parsed> {
    if has_forbidden_control(input) {
        return None;
    }
    let (pair, attrs) = match input.split_once(';') {
        Some((p, a)) => (p, Some(a)),
        None => (input, None),
    };
    let (name, value) = match pair.split_once('=') {
        Some((n, v)) => (trim_wsp(n), trim_wsp(v)),
        None => ("", trim_wsp(pair)),
    };
    if name.len() + value.len() > MAX_NAME_VALUE {
        return None;
    }
    let mut out = Parsed {
        name: name.to_owned(),
        value: value.to_owned(),
        ..Parsed::default()
    };
    for av in attrs.into_iter().flat_map(|a| a.split(';')) {
        let (attr_name, attr_value) = match av.split_once('=') {
            Some((n, v)) => (trim_wsp(n), trim_wsp(v)),
            None => (trim_wsp(av), ""),
        };
        if attr_value.len() > MAX_ATTRIBUTE_VALUE {
            continue;
        }
        match attr_name.to_ascii_lowercase().as_str() {
            "expires" => {
                if let Some(t) = parse_cookie_date(attr_value) {
                    out.expires = Some(t.min(now.saturating_add(MAX_AGE_CAP)));
                }
            }
            "max-age" => {
                if let Some(delta) = parse_max_age(attr_value) {
                    out.max_age = Some(if delta <= 0 {
                        EARLIEST
                    } else {
                        now.saturating_add(delta.min(MAX_AGE_CAP))
                    });
                }
            }
            "domain" => {
                if !attr_value.is_empty() {
                    let d = attr_value.strip_prefix('.').unwrap_or(attr_value);
                    out.domain = Some(d.to_ascii_lowercase());
                }
            }
            "path" => {
                out.path = if attr_value.starts_with('/') {
                    Some(attr_value.to_owned())
                } else {
                    None
                };
            }
            "secure" => out.secure = true,
            "httponly" => out.http_only = true,
            "samesite" => {
                out.same_site = Some(match attr_value.to_ascii_lowercase().as_str() {
                    "none" => SameSite::None,
                    "strict" => SameSite::Strict,
                    "lax" => SameSite::Lax,
                    _ => SameSite::Default,
                });
            }
            _ => {}
        }
    }
    Some(out)
}

/// `Max-Age` value: optional `-`, then digits (§5.6.2). Saturates.
fn parse_max_age(v: &str) -> Option<i64> {
    let (neg, digits) = match v.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, v),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut n: i64 = 0;
    for b in digits.bytes() {
        n = n.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    Some(if neg { -n } else { n })
}

/// Whether the URL uses a "secure protocol" (§5.7; user-agent defined). We
/// count `https:` plus loopback `http:` (potentially trustworthy origins).
fn is_secure_url(url: &Url) -> bool {
    match url.scheme() {
        "https" | "wss" => true,
        "http" | "ws" => is_potentially_trustworthy(url),
        _ => false,
    }
}

impl CookieJar {
    /// An empty, memory-only jar.
    pub fn new() -> CookieJar {
        CookieJar {
            inner: Arc::new(Mutex::new(Vec::new())),
            store: None,
        }
    }

    /// A jar backed by `store`; loads its cookies now, dropping expired ones.
    pub fn with_store(store: Arc<dyn CookieStore>) -> CookieJar {
        let t = now();
        let mut cookies = Vec::new();
        for c in store.load() {
            if c.is_expired(t) {
                store.delete(&c);
            } else {
                cookies.push(c);
            }
        }
        CookieJar {
            inner: Arc::new(Mutex::new(cookies)),
            store: Some(store),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Cookie>> {
        // A poisoned lock only means another thread panicked mid-update;
        // the Vec itself is still valid.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A snapshot of every cookie.
    pub fn all(&self) -> Vec<Cookie> {
        self.lock().clone()
    }

    /// The number of stored cookies.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the jar is empty.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Removes every cookie.
    pub fn clear(&self) {
        let removed = std::mem::take(&mut *self.lock());
        if let Some(store) = &self.store {
            for c in &removed {
                store.delete(c);
            }
        }
    }

    /// Removes every cookie partitioned under the top-level site `site`.
    pub fn clear_partition(&self, site: &Site) {
        let key = site.to_string();
        let mut removed = Vec::new();
        self.lock().retain(|c| {
            let keep = c.partition != key;
            if !keep {
                removed.push(c.clone());
            }
            keep
        });
        if let Some(store) = &self.store {
            for c in &removed {
                store.delete(c);
            }
        }
    }

    /// The `document.cookie` getter for a document at `document_url` inside
    /// a top-level document whose site is `top_level_site`. Returns an empty
    /// string for third-party documents.
    pub fn script_cookie_string(&self, document_url: &Url, top_level_site: &Site) -> String {
        match script_context(document_url, top_level_site) {
            Some(ctx) => self
                .cookie_string(document_url, &ctx, now())
                .unwrap_or_default(),
            None => String::new(),
        }
    }

    /// The `document.cookie` setter. Ignored for third-party documents and
    /// for `HttpOnly` cookies.
    pub fn set_from_script(&self, document_url: &Url, top_level_site: &Site, cookie: &str) {
        if let Some(ctx) = script_context(document_url, top_level_site) {
            self.set_cookie(document_url, cookie, &ctx, now());
        }
    }

    /// The `Cookie` header value for a request, or `None` if no cookie applies.
    pub(crate) fn cookie_string(&self, url: &Url, ctx: &CookieContext, now: i64) -> Option<String> {
        let host = url.host()?.to_string();
        let path = url.path();
        let secure = is_secure_url(url);
        let mut jar = self.lock();
        let mut changed = Vec::new();
        jar.retain(|c| {
            let keep = !c.is_expired(now);
            if !keep {
                changed.push((c.clone(), false));
            }
            keep
        });
        let mut picked: Vec<&mut Cookie> = jar
            .iter_mut()
            .filter(|c| {
                c.partition == ctx.partition
                    && if c.host_only {
                        host == c.domain
                    } else {
                        domain_match(&host, &c.domain)
                    }
                    && path_match(path, &c.path)
                    && (!c.secure || secure)
                    && (!c.http_only || ctx.http_api)
                    && (ctx.same_site
                        || match c.same_site {
                            SameSite::None => true,
                            SameSite::Strict => false,
                            SameSite::Lax | SameSite::Default => {
                                ctx.top_level_navigation && ctx.safe_method
                            }
                        })
            })
            .collect();
        // §5.8.3 step 2: longer paths first, then earlier creation times.
        picked.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.creation.cmp(&b.creation))
        });
        let mut parts = Vec::with_capacity(picked.len());
        for c in picked {
            c.last_access = now;
            if c.name.is_empty() {
                parts.push(c.value.clone());
            } else {
                parts.push(format!("{}={}", c.name, c.value));
            }
            if c.expires.is_some() {
                changed.push((c.clone(), true));
            }
        }
        drop(jar);
        self.persist(&changed);
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("; "))
        }
    }

    fn persist(&self, changes: &[(Cookie, bool)]) {
        if let Some(store) = &self.store {
            for (c, keep) in changes {
                if *keep {
                    store.save(c);
                } else {
                    store.delete(c);
                }
            }
        }
    }

    /// Runs the storage model (§5.7) for one `Set-Cookie` value received
    /// from `url`. Returns whether a cookie was stored.
    pub(crate) fn set_cookie(&self, url: &Url, input: &str, ctx: &CookieContext, now: i64) -> bool {
        let Some(p) = parse_set_cookie(input, now) else {
            return false;
        };
        let Some(request_host) = url.host() else {
            return false;
        };
        let host_str = request_host.to_string();
        let secure_request = is_secure_url(url);
        // Step 3: both empty → ignore.
        if p.name.is_empty() && p.value.is_empty() {
            return false;
        }
        // Step 5: nameless cookies may not smuggle a prefix.
        if p.name.is_empty()
            && (starts_with_ignore_case(&p.value, "__Secure-")
                || starts_with_ignore_case(&p.value, "__Host-"))
        {
            return false;
        }
        let expires = match (p.max_age, p.expires) {
            (Some(m), _) => Some(m),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        };
        // Domain attribute (steps 7-10).
        let (domain, host_only) = match &p.domain {
            Some(d) => {
                let Some(canonical) = Host::parse(d) else {
                    return false;
                };
                let canonical_str = canonical.to_string();
                if public_suffix(&canonical).as_deref() == Some(canonical_str.as_str()) {
                    if canonical_str == host_str {
                        (host_str.clone(), true)
                    } else {
                        return false;
                    }
                } else if domain_match(&host_str, &canonical_str) {
                    (canonical_str, false)
                } else {
                    return false;
                }
            }
            None => (host_str.clone(), true),
        };
        let path = p.path.clone().unwrap_or_else(|| default_path(url));
        // Step 13: Secure only from secure origins.
        if p.secure && !secure_request {
            return false;
        }
        // Step 15: HttpOnly cookies cannot come from script.
        if p.http_only && !ctx.http_api {
            return false;
        }
        let same_site = p.same_site.unwrap_or(SameSite::Default);
        // Step 18: SameSite=None requires Secure.
        if same_site == SameSite::None && !p.secure {
            return false;
        }
        // Step 19: SameSite-restricted cookies only from same-site contexts
        // or top-level navigations.
        if same_site != SameSite::None
            && !ctx.same_site
            && (!ctx.http_api || !ctx.top_level_navigation)
        {
            return false;
        }
        // Steps 20-21: cookie name prefixes.
        if starts_with_ignore_case(&p.name, "__Secure-") && !p.secure {
            return false;
        }
        if starts_with_ignore_case(&p.name, "__Host-") && !(p.secure && host_only && path == "/") {
            return false;
        }
        let cookie = Cookie {
            partition: ctx.partition.clone(),
            name: p.name,
            value: p.value,
            domain,
            path,
            expires,
            creation: now,
            last_access: now,
            host_only,
            secure: p.secure,
            http_only: p.http_only,
            same_site,
        };
        let mut changes: Vec<(Cookie, bool)> = Vec::new();
        let mut jar = self.lock();
        // Step 17: an insecure origin may not shadow a secure cookie.
        if !cookie.secure && !secure_request {
            let shadows = jar.iter().any(|old| {
                old.partition == cookie.partition
                    && old.secure
                    && old.name == cookie.name
                    && (domain_match(&cookie.domain, &old.domain)
                        || domain_match(&old.domain, &cookie.domain))
                    && path_match(&cookie.path, &old.path)
            });
            if shadows {
                return false;
            }
        }
        // Step 22: replace an existing cookie with the same identity.
        let mut cookie = cookie;
        if let Some(i) = jar.iter().position(|old| old.same_identity(&cookie)) {
            if let Some(old) = jar.get(i) {
                if !ctx.http_api && old.http_only {
                    return false;
                }
                cookie.creation = old.creation;
            }
            let old = jar.remove(i);
            changes.push((old, false));
        }
        let stored = !cookie.is_expired(now);
        if stored {
            changes.push((cookie.clone(), true));
            jar.push(cookie.clone());
            evict(&mut jar, &cookie, now, &mut changes);
        }
        drop(jar);
        // Only persistent cookies go to the store; session cookies die with
        // the process.
        let changes: Vec<(Cookie, bool)> = changes
            .into_iter()
            .filter(|(c, keep)| !*keep || c.expires.is_some())
            .collect();
        self.persist(&changes);
        stored
    }
}

/// Evicts expired cookies and enforces the per-domain and total limits
/// (§5.7.1), removing the least recently accessed first.
fn evict(jar: &mut Vec<Cookie>, newest: &Cookie, now: i64, changes: &mut Vec<(Cookie, bool)>) {
    jar.retain(|c| {
        let keep = !c.is_expired(now);
        if !keep {
            changes.push((c.clone(), false));
        }
        keep
    });
    let same_domain = |c: &Cookie| c.partition == newest.partition && c.domain == newest.domain;
    while jar.iter().filter(|c| same_domain(c)).count() > MAX_PER_DOMAIN {
        let victim = jar
            .iter()
            .enumerate()
            .filter(|(_, c)| same_domain(c) && !c.same_identity(newest))
            .min_by_key(|(_, c)| c.last_access)
            .map(|(i, _)| i);
        match victim {
            Some(i) => changes.push((jar.remove(i), false)),
            None => break,
        }
    }
    while jar.len() > MAX_TOTAL {
        let victim = jar
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.same_identity(newest))
            .min_by_key(|(_, c)| c.last_access)
            .map(|(i, _)| i);
        match victim {
            Some(i) => changes.push((jar.remove(i), false)),
            None => break,
        }
    }
}

/// The context for `document.cookie`, or `None` for third-party documents.
fn script_context(document_url: &Url, top_level_site: &Site) -> Option<CookieContext> {
    if top_level_site.is_opaque() || !document_url.site().same_site(top_level_site) {
        return None;
    }
    Some(CookieContext {
        partition: top_level_site.to_string(),
        same_site: true,
        top_level_navigation: false,
        safe_method: true,
        http_api: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn ctx(top: &str) -> CookieContext {
        CookieContext {
            partition: u(top).site().to_string(),
            same_site: true,
            top_level_navigation: true,
            safe_method: true,
            http_api: true,
        }
    }

    fn get(jar: &CookieJar, url: &str, c: &CookieContext) -> Option<String> {
        jar.cookie_string(&u(url), c, NOW)
    }

    #[test]
    fn parse_basics() {
        let p = parse_set_cookie(
            " a = b ; Path=/x; Secure; HttpOnly; SameSite=strict; Domain=.Example.COM",
            NOW,
        )
        .unwrap();
        assert_eq!(p.name, "a");
        assert_eq!(p.value, "b");
        assert_eq!(p.path.as_deref(), Some("/x"));
        assert_eq!(p.domain.as_deref(), Some("example.com"));
        assert!(p.secure && p.http_only);
        assert_eq!(p.same_site, Some(SameSite::Strict));
        let nameless = parse_set_cookie("justvalue", NOW).unwrap();
        assert_eq!(
            (nameless.name.as_str(), nameless.value.as_str()),
            ("", "justvalue")
        );
        assert!(parse_set_cookie("a=b\x01", NOW).is_none());
        assert!(parse_set_cookie(&format!("a={}", "x".repeat(5000)), NOW).is_none());
        let p = parse_set_cookie("a=b; SameSite=bogus; Path=relative", NOW).unwrap();
        assert_eq!(p.same_site, Some(SameSite::Default));
        assert_eq!(p.path, None);
    }

    #[test]
    fn max_age_and_expires() {
        let p = parse_set_cookie(
            "a=b; Max-Age=60; Expires=Thu, 01 Jan 2099 00:00:00 GMT",
            NOW,
        )
        .unwrap();
        assert_eq!(p.max_age, Some(NOW + 60));
        assert_eq!(p.expires, Some(NOW + MAX_AGE_CAP), "capped at 400 days");
        assert_eq!(
            parse_set_cookie("a=b; Max-Age=0", NOW).unwrap().max_age,
            Some(EARLIEST)
        );
        assert_eq!(
            parse_set_cookie("a=b; Max-Age=-5", NOW).unwrap().max_age,
            Some(EARLIEST)
        );
        assert_eq!(
            parse_set_cookie("a=b; Max-Age=1x", NOW).unwrap().max_age,
            None
        );
        assert_eq!(
            parse_set_cookie("a=b; Max-Age=99999999999999999999999", NOW)
                .unwrap()
                .max_age,
            Some(NOW + MAX_AGE_CAP)
        );
        assert_eq!(
            parse_set_cookie("a=b; Expires=garbage", NOW)
                .unwrap()
                .expires,
            None
        );
    }

    #[test]
    fn store_and_send() {
        let jar = CookieJar::new();
        let c = ctx("https://a.example/");
        assert!(jar.set_cookie(&u("https://a.example/dir/page"), "sid=1", &c, NOW));
        assert!(jar.set_cookie(&u("https://a.example/"), "root=2; Path=/", &c, NOW));
        // Default path is /dir.
        assert_eq!(
            get(&jar, "https://a.example/dir/x", &c).as_deref(),
            Some("sid=1; root=2")
        );
        assert_eq!(
            get(&jar, "https://a.example/other", &c).as_deref(),
            Some("root=2")
        );
        assert_eq!(
            get(&jar, "https://a.example/directory", &c).as_deref(),
            Some("root=2")
        );
        // Host-only: not sent to subdomains.
        assert_eq!(get(&jar, "https://sub.a.example/dir/x", &c), None);
    }

    #[test]
    fn domain_attribute() {
        let jar = CookieJar::new();
        let c = ctx("https://www.a.example/");
        assert!(jar.set_cookie(
            &u("https://www.a.example/"),
            "d=1; Domain=a.example; Path=/",
            &c,
            NOW
        ));
        assert_eq!(
            get(&jar, "https://x.a.example/", &c).as_deref(),
            Some("d=1")
        );
        assert_eq!(get(&jar, "https://a.example/", &c).as_deref(), Some("d=1"));
        // Foreign domain and public suffixes are rejected.
        assert!(!jar.set_cookie(
            &u("https://www.a.example/"),
            "e=1; Domain=b.example",
            &c,
            NOW
        ));
        assert!(!jar.set_cookie(&u("https://www.a.co.uk/"), "e=1; Domain=co.uk", &c, NOW));
        // A public suffix equal to the host becomes host-only.
        let gh = ctx("https://github.io/");
        assert!(jar.set_cookie(&u("https://github.io/"), "h=1; Domain=github.io", &gh, NOW));
        let stored = jar.all().into_iter().find(|c| c.name == "h").unwrap();
        assert!(stored.host_only);
        // IP hosts never domain-match anything else.
        assert!(!domain_match("1.2.3.4", "2.3.4"));
        assert!(domain_match("1.2.3.4", "1.2.3.4"));
    }

    #[test]
    fn secure_rules() {
        let jar = CookieJar::new();
        let c = ctx("http://a.example/");
        assert!(!jar.set_cookie(&u("http://a.example/"), "s=1; Secure", &c, NOW));
        let cs = ctx("https://a.example/");
        assert!(jar.set_cookie(&u("https://a.example/"), "s=1; Secure; Path=/", &cs, NOW));
        assert_eq!(
            get(&jar, "http://a.example/", &cs),
            None,
            "secure cookie not sent over http"
        );
        // Insecure origin may not shadow a secure cookie.
        assert!(!jar.set_cookie(&u("http://a.example/"), "s=2; Path=/", &cs, NOW));
        // Localhost counts as secure.
        let lc = ctx("http://localhost/");
        assert!(jar.set_cookie(&u("http://localhost/"), "l=1; Secure", &lc, NOW));
    }

    #[test]
    fn prefixes() {
        let jar = CookieJar::new();
        let c = ctx("https://a.example/");
        let url = u("https://a.example/x/y");
        assert!(!jar.set_cookie(&url, "__Secure-a=1", &c, NOW));
        assert!(jar.set_cookie(&url, "__Secure-a=1; Secure", &c, NOW));
        assert!(
            !jar.set_cookie(&url, "__Host-b=1; Secure", &c, NOW),
            "path must be /"
        );
        assert!(!jar.set_cookie(
            &url,
            "__Host-b=1; Secure; Path=/; Domain=a.example",
            &c,
            NOW
        ));
        assert!(jar.set_cookie(&url, "__Host-b=1; Secure; Path=/", &c, NOW));
        assert!(
            !jar.set_cookie(&url, "__host-c=1; Path=/", &c, NOW),
            "case-insensitive"
        );
        assert!(!jar.set_cookie(&url, "=__Secure-x", &c, NOW));
        assert!(!jar.set_cookie(&url, "=", &c, NOW));
    }

    #[test]
    fn samesite() {
        let jar = CookieJar::new();
        let c = ctx("https://a.example/");
        let url = u("https://a.example/");
        assert!(jar.set_cookie(&url, "s=1; SameSite=Strict", &c, NOW));
        assert!(jar.set_cookie(&url, "l=1; SameSite=Lax", &c, NOW));
        assert!(jar.set_cookie(&url, "d=1", &c, NOW));
        assert!(jar.set_cookie(&url, "n=1; SameSite=None; Secure", &c, NOW));
        assert!(
            !jar.set_cookie(&url, "x=1; SameSite=None", &c, NOW),
            "None needs Secure"
        );
        let mut cross = c.clone();
        cross.same_site = false;
        assert_eq!(
            get(&jar, "https://a.example/", &cross).as_deref(),
            Some("l=1; d=1; n=1")
        );
        cross.safe_method = false;
        assert_eq!(
            get(&jar, "https://a.example/", &cross).as_deref(),
            Some("n=1")
        );
        cross.top_level_navigation = false;
        assert!(!jar.set_cookie(&url, "y=1; SameSite=Lax", &cross, NOW));
    }

    #[test]
    fn partitioning() {
        let jar = CookieJar::new();
        let under_a = ctx("https://a.example/");
        let under_b = ctx("https://b.example/");
        assert!(jar.set_cookie(&u("https://a.example/"), "p=1", &under_a, NOW));
        assert_eq!(get(&jar, "https://a.example/", &under_b), None);
        assert!(jar.set_cookie(&u("https://a.example/"), "p=2", &under_b, NOW));
        assert_eq!(
            get(&jar, "https://a.example/", &under_a).as_deref(),
            Some("p=1")
        );
        assert_eq!(
            get(&jar, "https://a.example/", &under_b).as_deref(),
            Some("p=2")
        );
        jar.clear_partition(&u("https://b.example/").site());
        assert_eq!(jar.len(), 1);
    }

    #[test]
    fn replace_and_expire() {
        let jar = CookieJar::new();
        let c = ctx("https://a.example/");
        let url = u("https://a.example/");
        assert!(jar.set_cookie(&url, "a=1", &c, NOW));
        assert!(jar.set_cookie(&url, "a=2", &c, NOW + 10));
        let all = jar.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].value, "2");
        assert_eq!(all[0].creation, NOW, "creation time is kept");
        assert!(!jar.set_cookie(&url, "a=3; Max-Age=0", &c, NOW));
        assert!(jar.is_empty(), "Max-Age=0 deletes");
        assert!(jar.set_cookie(&url, "b=1; Max-Age=10", &c, NOW));
        assert_eq!(get(&jar, "https://a.example/", &c).as_deref(), Some("b=1"));
        assert_eq!(jar.cookie_string(&url, &c, NOW + 11), None);
        assert!(jar.is_empty());
    }

    #[test]
    fn http_only_and_script() {
        let jar = CookieJar::new();
        let top = u("https://a.example/");
        let c = ctx("https://a.example/");
        assert!(jar.set_cookie(&top, "h=1; HttpOnly", &c, NOW));
        jar.set_from_script(&top, &top.site(), "s=1");
        assert_eq!(jar.script_cookie_string(&top, &top.site()), "s=1");
        // Script cannot overwrite an HttpOnly cookie or create one.
        jar.set_from_script(&top, &top.site(), "h=2");
        jar.set_from_script(&top, &top.site(), "z=1; HttpOnly");
        assert_eq!(
            get(&jar, "https://a.example/", &c).as_deref(),
            Some("h=1; s=1")
        );
        // Third-party documents see nothing and set nothing.
        let other = u("https://b.example/");
        assert_eq!(jar.script_cookie_string(&other, &top.site()), "");
        jar.set_from_script(&other, &top.site(), "t=1");
        assert_eq!(jar.len(), 2);
    }

    #[test]
    fn eviction() {
        let jar = CookieJar::new();
        let c = ctx("https://a.example/");
        let url = u("https://a.example/");
        for i in 0..60 {
            assert!(jar.set_cookie(&url, &format!("c{i}=1"), &c, NOW + i));
        }
        assert_eq!(jar.len(), MAX_PER_DOMAIN);
        assert!(jar.all().iter().all(|c| c.name != "c0"), "oldest evicted");
    }

    #[test]
    fn paths() {
        assert!(path_match("/a/b", "/a"));
        assert!(path_match("/a/", "/a/"));
        assert!(path_match("/a/b", "/a/"));
        assert!(!path_match("/ab", "/a"));
        assert!(!path_match("/", "/a"));
        assert_eq!(default_path(&u("https://a.example")), "/");
        assert_eq!(default_path(&u("https://a.example/x")), "/");
        assert_eq!(default_path(&u("https://a.example/x/y/z")), "/x/y");
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>, Vec<Cookie>);
    impl CookieStore for Recorder {
        fn load(&self) -> Vec<Cookie> {
            self.1.clone()
        }
        fn save(&self, c: &Cookie) {
            self.0.lock().unwrap().push(format!("save {}", c.name));
        }
        fn delete(&self, c: &Cookie) {
            self.0.lock().unwrap().push(format!("delete {}", c.name));
        }
    }

    #[test]
    fn store_trait() {
        let t = now();
        let mut keep = Cookie {
            partition: "https://a.example".into(),
            name: "kept".into(),
            value: "1".into(),
            domain: "a.example".into(),
            path: "/".into(),
            expires: Some(t + 1000),
            creation: t,
            last_access: t,
            host_only: true,
            secure: false,
            http_only: false,
            same_site: SameSite::Lax,
        };
        let mut gone = keep.clone();
        gone.name = "gone".into();
        gone.expires = Some(t - 1);
        keep.value = "v".into();
        let store = Arc::new(Recorder(Mutex::new(vec![]), vec![keep, gone]));
        let jar = CookieJar::with_store(store.clone());
        assert_eq!(jar.len(), 1);
        let c = ctx("https://a.example/");
        let url = u("https://a.example/");
        assert!(jar.set_cookie(&url, "session=1", &c, t));
        assert!(jar.set_cookie(&url, "persist=1; Max-Age=100", &c, t));
        jar.clear();
        let log = store.0.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "delete gone",
                "save persist",
                "delete kept",
                "delete session",
                "delete persist"
            ]
        );
        assert!(format!("{jar:?}").contains("persistent: true"));
    }
}
