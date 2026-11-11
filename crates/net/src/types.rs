//! Request and response types.

use std::fmt;

use wb_url::{Site, Url};

use crate::policy::BackgroundKind;
use crate::referrer::ReferrerPolicy;

/// An HTTP request method.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`
    Get,
    /// `HEAD`
    Head,
    /// `POST`
    Post,
    /// `PUT`
    Put,
    /// `DELETE`
    Delete,
    /// `OPTIONS`
    Options,
    /// `PATCH`
    Patch,
    /// Any other method token, as given (methods are case-sensitive).
    Other(String),
}

impl Method {
    /// Parses a method token. The six methods Fetch normalizes
    /// ([Fetch §2.2.1](https://fetch.spec.whatwg.org/#concept-method-normalize))
    /// are matched case-insensitively; others are kept verbatim. Returns
    /// `None` if `s` is not a valid token.
    pub fn parse(s: &str) -> Option<Method> {
        if s.is_empty() || !s.bytes().all(is_token_byte) {
            return None;
        }
        Some(match s.to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "OPTIONS" => Method::Options,
            "PATCH" if s == "PATCH" => Method::Patch,
            _ => Method::Other(s.to_owned()),
        })
    }

    /// The method token.
    pub fn as_str(&self) -> &str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
            Method::Patch => "PATCH",
            Method::Other(s) => s,
        }
    }

    /// Whether the method is safe ([RFC 9110 §9.2.1](https://www.rfc-editor.org/rfc/rfc9110#section-9.2.1)).
    pub fn is_safe(&self) -> bool {
        matches!(self, Method::Get | Method::Head | Method::Options)
            || matches!(self, Method::Other(s) if s == "TRACE")
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `tchar` from [RFC 9110 §5.6.2](https://www.rfc-editor.org/rfc/rfc9110#section-5.6.2).
pub(crate) fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// An ordered list of HTTP header fields.
///
/// Names are stored lowercase. Values are strings holding the
/// [isomorphic decoding](https://infra.spec.whatwg.org/#isomorphic-decode) of
/// the header bytes, so every byte sequence round-trips: bytes 0x80–0xFF map
/// to U+0080–U+00FF.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    list: Vec<(String, String)>,
}

impl Headers {
    /// An empty header list.
    pub fn new() -> Headers {
        Headers::default()
    }

    /// Whether `name` is a valid header name and `value` a valid header
    /// value ([Fetch §2.2](https://fetch.spec.whatwg.org/#header-value)): no
    /// CR, LF or NUL, no leading/trailing whitespace, and only code points
    /// up to U+00FF.
    pub fn is_valid(name: &str, value: &str) -> bool {
        !name.is_empty()
            && name.bytes().all(is_token_byte)
            && value
                .chars()
                .all(|c| (c as u32) <= 0xFF && !matches!(c, '\0' | '\r' | '\n'))
            && !value.starts_with([' ', '\t'])
            && !value.ends_with([' ', '\t'])
    }

    /// Appends a header. Invalid names or values are ignored; check with
    /// [`Headers::is_valid`] first if that matters.
    pub fn append(&mut self, name: &str, value: &str) {
        if Self::is_valid(name, value) {
            self.list
                .push((name.to_ascii_lowercase(), value.to_owned()));
        }
    }

    /// Replaces all headers named `name` with a single one.
    pub fn set(&mut self, name: &str, value: &str) {
        if Self::is_valid(name, value) {
            self.remove(name);
            self.list
                .push((name.to_ascii_lowercase(), value.to_owned()));
        }
    }

    /// Removes all headers named `name` (case-insensitive).
    pub fn remove(&mut self, name: &str) {
        self.list.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    /// The first value of `name`, if present.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.list
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All values of `name`, in order.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.list
            .iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// All values of `name` joined with `", "`
    /// ([Fetch "get"](https://fetch.spec.whatwg.org/#concept-header-list-get)).
    pub fn get_combined(&self, name: &str) -> Option<String> {
        let values: Vec<&str> = self.get_all(name).collect();
        if values.is_empty() {
            None
        } else {
            Some(values.join(", "))
        }
    }

    /// Whether a header named `name` is present.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Iterates over `(name, value)` pairs in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.list.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// The number of header fields.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Whether there are no header fields.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// Decodes header bytes isomorphically (each byte to the code point of the
/// same value).
pub(crate) fn isomorphic_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Encodes an isomorphic string back to bytes; `None` if any code point is
/// above U+00FF.
pub(crate) fn isomorphic_encode(s: &str) -> Option<Vec<u8>> {
    s.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect()
}

/// Who caused a request. Every request names one; the policy gate and the
/// cookie, referrer and cache rules all key off it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestContext {
    /// A top-level navigation (the user typed a URL, clicked a link, or a
    /// page navigated itself). The navigation's own target is the top-level
    /// site, so its cookies are first-party.
    Navigation {
        /// The site of the document that started the navigation, or `None`
        /// when the user started it (address bar, bookmark). A cross-site
        /// initiator makes the request cross-site for `SameSite` cookies.
        initiator: Option<Site>,
    },
    /// A subresource (image, stylesheet, script, ...) fetched for a page.
    Subresource {
        /// The site of the top-level document. Cookies are partitioned by
        /// it, and requests to any other site get no cookies at all.
        top_level_site: Site,
    },
    /// A request the browser makes on its own. These are the only
    /// non-page requests allowed (AGENTS.md "No telemetry"), and each kind
    /// can be switched off in the [`NetworkPolicy`](crate::NetworkPolicy).
    Background(BackgroundKind),
}

/// How a request interacts with the HTTP cache
/// ([Fetch cache mode](https://fetch.spec.whatwg.org/#concept-request-cache-mode)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CacheMode {
    /// Use a fresh cached response, revalidate a stale one, store the result.
    #[default]
    Default,
    /// Bypass the cache entirely: neither read nor write.
    NoStore,
    /// Don't read from the cache, but store the response.
    Reload,
    /// Always revalidate a cached response before using it.
    NoCache,
    /// Use any cached response, however stale; otherwise fetch and store.
    ForceCache,
}

/// What to do when the server redirects
/// ([Fetch redirect mode](https://fetch.spec.whatwg.org/#concept-request-redirect-mode)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RedirectMode {
    /// Follow up to 20 redirects.
    #[default]
    Follow,
    /// Fail with [`NetError::RedirectNotAllowed`](crate::NetError::RedirectNotAllowed).
    Error,
    /// Return the redirect response itself.
    Manual,
}

/// A request to fetch.
#[derive(Clone, Debug)]
pub struct Request {
    /// The URL to fetch (`http`, `https`, `data`, or `about:blank`).
    pub url: Url,
    /// The method.
    pub method: Method,
    /// Extra request headers. The client adds `Accept-Encoding`,
    /// `User-Agent`, `Cookie` and `Referer` itself; `Cookie`, `Referer`,
    /// `Host`, `Content-Length`, `Connection` and other
    /// [forbidden request-headers](https://fetch.spec.whatwg.org/#forbidden-request-header)
    /// given here are ignored.
    pub headers: Headers,
    /// The request body (must be empty for GET and HEAD).
    pub body: Vec<u8>,
    /// Who caused this request.
    pub context: RequestContext,
    /// The document URL to derive `Referer` from, if any.
    pub referrer: Option<Url>,
    /// The referrer policy (default `strict-origin-when-cross-origin`).
    pub referrer_policy: ReferrerPolicy,
    /// The cache mode.
    pub cache_mode: CacheMode,
    /// The redirect mode.
    pub redirect_mode: RedirectMode,
}

impl Request {
    /// A GET request for a top-level navigation started by the user.
    pub fn navigate(url: Url) -> Request {
        Request::with_context(url, RequestContext::Navigation { initiator: None })
    }

    /// A GET request for a subresource of a page whose top-level document
    /// is at `top_level`.
    pub fn subresource(url: Url, top_level: &Url) -> Request {
        let mut r = Request::with_context(
            url,
            RequestContext::Subresource {
                top_level_site: top_level.site(),
            },
        );
        r.referrer = Some(top_level.clone());
        r
    }

    /// A GET request of the given background kind (subject to the policy gate).
    pub fn background(url: Url, kind: BackgroundKind) -> Request {
        Request::with_context(url, RequestContext::Background(kind))
    }

    /// A GET request with an explicit context.
    pub fn with_context(url: Url, context: RequestContext) -> Request {
        Request {
            url,
            method: Method::Get,
            headers: Headers::new(),
            body: Vec::new(),
            context,
            referrer: None,
            referrer_policy: ReferrerPolicy::default(),
            cache_mode: CacheMode::Default,
            redirect_mode: RedirectMode::Follow,
        }
    }
}

/// The HTTP version a response arrived over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HttpVersion {
    /// HTTP/1.0
    Http10,
    /// HTTP/1.1
    Http11,
    /// HTTP/2
    Http2,
    /// Not fetched over HTTP (`data:`, `about:blank`).
    None,
}

/// A complete response. Bodies are fully buffered and already decoded
/// (`Content-Encoding` removed from the bytes, though the header is kept).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The final URL, after redirects.
    pub url: Url,
    /// Every URL fetched, first to last (length > 1 means redirected).
    pub url_list: Vec<Url>,
    /// The status code (e.g. 200).
    pub status: u16,
    /// The response headers.
    pub headers: Headers,
    /// The decoded body.
    pub body: Vec<u8>,
    /// The HTTP version.
    pub version: HttpVersion,
    /// Whether the response came from the memory cache (including after a
    /// successful revalidation).
    pub from_cache: bool,
}

impl Response {
    /// Whether the status is in 200..=299.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Whether more than one URL was fetched to produce this response.
    pub fn redirected(&self) -> bool {
        self.url_list.len() > 1
    }

    /// The `Content-Type` header, if any.
    pub fn content_type(&self) -> Option<&str> {
        self.headers.get("content-type")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods() {
        assert_eq!(Method::parse("get"), Some(Method::Get));
        assert_eq!(Method::parse("Patch"), Some(Method::Other("Patch".into())));
        assert_eq!(Method::parse("PATCH"), Some(Method::Patch));
        assert_eq!(Method::parse("BREW"), Some(Method::Other("BREW".into())));
        assert_eq!(Method::parse("a b"), None);
        assert_eq!(Method::parse(""), None);
        assert!(Method::Head.is_safe());
        assert!(!Method::Post.is_safe());
        assert_eq!(Method::Delete.to_string(), "DELETE");
    }

    #[test]
    fn headers() {
        let mut h = Headers::new();
        h.append("X-A", "1");
        h.append("x-a", "2");
        h.append("bad name", "x");
        h.append("x-b", "evil\r\nInjected: 1");
        h.append("x-c", " lead");
        assert_eq!(h.len(), 2);
        assert_eq!(h.get("X-A"), Some("1"));
        assert_eq!(h.get_combined("x-a").as_deref(), Some("1, 2"));
        h.set("x-a", "3");
        assert_eq!(h.get_all("x-a").collect::<Vec<_>>(), vec!["3"]);
        assert!(h.contains("X-A"));
        h.remove("X-a");
        assert!(h.is_empty());
        assert_eq!(h.get_combined("x-a"), None);
    }

    #[test]
    fn isomorphic() {
        let s = isomorphic_decode(&[0x41, 0xE9, 0xFF]);
        assert_eq!(s, "A\u{e9}\u{ff}");
        assert_eq!(isomorphic_encode(&s), Some(vec![0x41, 0xE9, 0xFF]));
        assert_eq!(isomorphic_encode("\u{100}"), None);
    }

    #[test]
    fn constructors() {
        let top = Url::parse("https://a.test/page").unwrap();
        let r = Request::subresource(Url::parse("https://a.test/x.css").unwrap(), &top);
        assert_eq!(r.referrer, Some(top.clone()));
        assert!(matches!(r.context, RequestContext::Subresource { .. }));
        let n = Request::navigate(top);
        assert_eq!(n.method, Method::Get);
        assert_eq!(n.cache_mode, CacheMode::Default);
    }
}
