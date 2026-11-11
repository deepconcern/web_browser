//! WHATWG URLs for the browser.
//!
//! This crate wraps the [`url`](https://crates.io/crates/url) crate behind our
//! own [`Url`] type so the rest of the engine never sees borrowed types (see
//! `AGENTS.md`, "Own the seams"). On top of parsing it provides:
//!
//! - [`Origin`]: the URL Standard's origin of a URL (tuple or opaque).
//! - [`Site`]: the HTML Standard's *site* (scheme plus registrable domain),
//!   which the network stack uses to partition cookies and caches.
//! - [`registrable_domain`] and [`public_suffix`]: the URL Standard's host
//!   public suffix and registrable domain, computed against a vendored copy
//!   of the Public Suffix List (`data/public_suffix_list.dat`, MPL-2.0).
//!
//! ```
//! use wb_url::Url;
//!
//! let base = Url::parse("https://www.example.co.uk/a/b?x#y").unwrap();
//! let next = base.join("../c").unwrap();
//! assert_eq!(next.as_str(), "https://www.example.co.uk/c");
//! assert_eq!(next.site().to_string(), "https://example.co.uk");
//! ```

#![deny(missing_docs)]

mod host;
mod origin;
mod psl;
mod site;

use std::fmt;

pub use host::Host;
pub use origin::Origin;
pub use psl::{public_suffix, registrable_domain};
pub use site::Site;

/// A parsed, absolute URL as defined by the
/// [URL Standard](https://url.spec.whatwg.org/#concept-url).
///
/// Values are immutable; methods that "change" a URL return a new one.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Url {
    inner: url::Url,
}

/// Why a string could not be parsed as a URL, or a URL could not be changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ParseError {
    /// A special URL (such as `https:`) has an empty host.
    EmptyHost,
    /// The host contains a forbidden code point or failed IDNA processing.
    InvalidDomain,
    /// The host looks like IPv4 but is not a valid IPv4 address.
    InvalidIpv4Address,
    /// The bracketed host is not a valid IPv6 address.
    InvalidIpv6Address,
    /// The port is not a number in the range 0..=65535.
    InvalidPort,
    /// The input is relative and no base URL was given.
    RelativeUrlWithoutBase,
    /// The input is relative but the base URL cannot be a base (e.g. `data:`).
    RelativeUrlWithCannotBeABaseBase,
    /// The scheme cannot be changed to the requested one (URL Standard §4.4,
    /// "scheme state" override rules), or is not a valid scheme.
    CannotChangeScheme,
    /// The input exceeds the maximum length the parser supports.
    TooLong,
    /// Any other validation failure that makes parsing fail.
    Invalid,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::EmptyHost => "empty host",
            Self::InvalidDomain => "invalid domain",
            Self::InvalidIpv4Address => "invalid IPv4 address",
            Self::InvalidIpv6Address => "invalid IPv6 address",
            Self::InvalidPort => "invalid port number",
            Self::RelativeUrlWithoutBase => "relative URL without a base",
            Self::RelativeUrlWithCannotBeABaseBase => "relative URL with a cannot-be-a-base base",
            Self::CannotChangeScheme => "cannot change the URL's scheme",
            Self::TooLong => "URL is too long",
            Self::Invalid => "invalid URL",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for ParseError {}

impl From<url::ParseError> for ParseError {
    fn from(e: url::ParseError) -> Self {
        match e {
            url::ParseError::EmptyHost => Self::EmptyHost,
            url::ParseError::IdnaError | url::ParseError::InvalidDomainCharacter => {
                Self::InvalidDomain
            }
            url::ParseError::InvalidIpv4Address => Self::InvalidIpv4Address,
            url::ParseError::InvalidIpv6Address => Self::InvalidIpv6Address,
            url::ParseError::InvalidPort => Self::InvalidPort,
            url::ParseError::RelativeUrlWithoutBase => Self::RelativeUrlWithoutBase,
            url::ParseError::RelativeUrlWithCannotBeABaseBase => {
                Self::RelativeUrlWithCannotBeABaseBase
            }
            url::ParseError::Overflow => Self::TooLong,
            _ => Self::Invalid,
        }
    }
}

/// Returns the default port of a special scheme
/// ([URL Standard §4.1](https://url.spec.whatwg.org/#default-port)).
pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

impl Url {
    /// Parses an absolute URL string
    /// ([basic URL parser](https://url.spec.whatwg.org/#concept-basic-url-parser)
    /// with no base).
    pub fn parse(input: &str) -> Result<Url, ParseError> {
        Ok(Url {
            inner: url::Url::parse(input)?,
        })
    }

    /// Parses `input` relative to `self`, as `new URL(input, self)` does.
    pub fn join(&self, input: &str) -> Result<Url, ParseError> {
        Ok(Url {
            inner: self.inner.join(input)?,
        })
    }

    /// The serialized URL ([URL serializer](https://url.spec.whatwg.org/#concept-url-serializer)).
    pub fn as_str(&self) -> &str {
        self.inner.as_str()
    }

    /// The scheme, lowercase and without the trailing `:`.
    pub fn scheme(&self) -> &str {
        self.inner.scheme()
    }

    /// Whether the scheme is one of the URL Standard's
    /// [special schemes](https://url.spec.whatwg.org/#special-scheme).
    pub fn is_special(&self) -> bool {
        matches!(
            self.scheme(),
            "http" | "https" | "ws" | "wss" | "ftp" | "file"
        )
    }

    /// Whether the scheme is `http` or `https`
    /// ([Fetch §2.1 HTTP(S) scheme](https://fetch.spec.whatwg.org/#http-scheme)).
    pub fn is_http_scheme(&self) -> bool {
        matches!(self.scheme(), "http" | "https")
    }

    /// The host, if the URL has one.
    pub fn host(&self) -> Option<Host> {
        match self.inner.host()? {
            url::Host::Domain(d) => Some(Host::Domain(d.to_owned())),
            url::Host::Ipv4(a) => Some(Host::Ipv4(a)),
            url::Host::Ipv6(a) => Some(Host::Ipv6(a)),
        }
    }

    /// The serialized host (IPv6 addresses in brackets), if any.
    pub fn host_str(&self) -> Option<&str> {
        self.inner.host_str()
    }

    /// The explicit port, or `None` if absent or equal to the scheme's default.
    pub fn port(&self) -> Option<u16> {
        self.inner.port()
    }

    /// The explicit port, or else the scheme's default port.
    pub fn port_or_known_default(&self) -> Option<u16> {
        self.inner.port_or_known_default()
    }

    /// The path, serialized (e.g. `/a/b`).
    pub fn path(&self) -> &str {
        self.inner.path()
    }

    /// The query without the leading `?`, if present.
    pub fn query(&self) -> Option<&str> {
        self.inner.query()
    }

    /// The fragment without the leading `#`, if present.
    pub fn fragment(&self) -> Option<&str> {
        self.inner.fragment()
    }

    /// The username (possibly empty).
    pub fn username(&self) -> &str {
        self.inner.username()
    }

    /// The password, if present.
    pub fn password(&self) -> Option<&str> {
        self.inner.password()
    }

    /// Whether the URL includes a username or password
    /// ([URL Standard](https://url.spec.whatwg.org/#include-credentials)).
    pub fn has_credentials(&self) -> bool {
        !self.username().is_empty() || self.password().is_some()
    }

    /// The path and query, as sent in an HTTP request line (`/a?b`).
    pub fn path_and_query(&self) -> String {
        match self.query() {
            Some(q) => format!("{}?{}", self.path(), q),
            None => self.path().to_owned(),
        }
    }

    /// A copy of this URL with the fragment removed.
    pub fn without_fragment(&self) -> Url {
        let mut inner = self.inner.clone();
        inner.set_fragment(None);
        Url { inner }
    }

    /// A copy of this URL with the fragment replaced (`None` removes it).
    /// The fragment is percent-encoded as the URL parser would.
    pub fn with_fragment(&self, fragment: Option<&str>) -> Url {
        let mut inner = self.inner.clone();
        inner.set_fragment(fragment);
        Url { inner }
    }

    /// A copy of this URL with username and password removed.
    pub fn without_credentials(&self) -> Url {
        let mut inner = self.inner.clone();
        // These only fail for URLs that cannot have credentials, in which
        // case there is nothing to strip.
        let _ = inner.set_username("");
        let _ = inner.set_password(None);
        Url { inner }
    }

    /// A copy of this URL with a different scheme, following the URL
    /// Standard's scheme-setter rules (special and non-special schemes cannot
    /// be swapped). A port equal to the new scheme's default is dropped.
    pub fn with_scheme(&self, scheme: &str) -> Result<Url, ParseError> {
        let mut inner = self.inner.clone();
        inner
            .set_scheme(scheme)
            .map_err(|()| ParseError::CannotChangeScheme)?;
        Ok(Url { inner })
    }

    /// A copy of this URL with a different port (`None` removes it).
    pub fn with_port(&self, port: Option<u16>) -> Result<Url, ParseError> {
        let mut inner = self.inner.clone();
        inner.set_port(port).map_err(|()| ParseError::InvalidPort)?;
        Ok(Url { inner })
    }

    /// The URL's [origin](https://url.spec.whatwg.org/#concept-url-origin).
    pub fn origin(&self) -> Origin {
        Origin::of(self)
    }

    /// The [site](https://html.spec.whatwg.org/multipage/browsers.html#obtain-a-site)
    /// of this URL's origin.
    pub fn site(&self) -> Site {
        Site::of_origin(&self.origin())
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Url({:?})", self.as_str())
    }
}

impl std::str::FromStr for Url {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Url, ParseError> {
        Url::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_accessors() {
        let u = Url::parse("HTTPS://user:pw@Example.COM:8443/a/./b/../c?q=1#frag").unwrap();
        assert_eq!(u.scheme(), "https");
        assert_eq!(u.host_str(), Some("example.com"));
        assert_eq!(u.host(), Some(Host::Domain("example.com".into())));
        assert_eq!(u.port(), Some(8443));
        assert_eq!(u.port_or_known_default(), Some(8443));
        assert_eq!(u.path(), "/a/c");
        assert_eq!(u.query(), Some("q=1"));
        assert_eq!(u.fragment(), Some("frag"));
        assert_eq!(u.username(), "user");
        assert_eq!(u.password(), Some("pw"));
        assert!(u.has_credentials());
        assert_eq!(u.path_and_query(), "/a/c?q=1");
        assert!(u.is_special());
        assert!(u.is_http_scheme());
    }

    #[test]
    fn default_port_is_elided() {
        let u = Url::parse("http://a.test:80/").unwrap();
        assert_eq!(u.port(), None);
        assert_eq!(u.port_or_known_default(), Some(80));
        assert_eq!(u.as_str(), "http://a.test/");
    }

    #[test]
    fn join_relative() {
        let base = Url::parse("https://a.test/dir/page.html?x").unwrap();
        assert_eq!(
            base.join("img.png").unwrap().as_str(),
            "https://a.test/dir/img.png"
        );
        assert_eq!(base.join("/root").unwrap().as_str(), "https://a.test/root");
        assert_eq!(
            base.join("//b.test/x").unwrap().as_str(),
            "https://b.test/x"
        );
        assert_eq!(
            base.join("?y").unwrap().as_str(),
            "https://a.test/dir/page.html?y"
        );
        assert_eq!(
            base.join("#f").unwrap().as_str(),
            "https://a.test/dir/page.html?x#f"
        );
    }

    #[test]
    fn parse_errors() {
        assert_eq!(Url::parse("foo"), Err(ParseError::RelativeUrlWithoutBase));
        assert_eq!(Url::parse("http://"), Err(ParseError::EmptyHost));
        assert_eq!(Url::parse("http://a:99999/"), Err(ParseError::InvalidPort));
        assert_eq!(
            Url::parse("http://[::1/"),
            Err(ParseError::InvalidIpv6Address)
        );
        assert_eq!(
            Url::parse("http://1.2.3.256/"),
            Err(ParseError::InvalidIpv4Address)
        );
        let data = Url::parse("data:text/plain,hi").unwrap();
        assert_eq!(
            data.join("x"),
            Err(ParseError::RelativeUrlWithCannotBeABaseBase)
        );
    }

    #[test]
    fn ip_hosts() {
        let v4 = Url::parse("http://127.0.0.1:8080/").unwrap();
        assert_eq!(v4.host(), Some(Host::Ipv4([127, 0, 0, 1].into())));
        let v6 = Url::parse("http://[::1]/").unwrap();
        assert_eq!(v6.host_str(), Some("[::1]"));
        assert!(matches!(v6.host(), Some(Host::Ipv6(_))));
        assert_eq!(v6.host().unwrap().to_string(), "[::1]");
    }

    #[test]
    fn idna_hosts_are_ascii() {
        let u = Url::parse("https://bücher.example/").unwrap();
        assert_eq!(u.host_str(), Some("xn--bcher-kva.example"));
    }

    #[test]
    fn scheme_changes() {
        let u = Url::parse("http://a.test/x").unwrap();
        assert_eq!(u.with_scheme("https").unwrap().as_str(), "https://a.test/x");
        let p = Url::parse("http://a.test:8080/x").unwrap();
        assert_eq!(
            p.with_scheme("https").unwrap().as_str(),
            "https://a.test:8080/x"
        );
        assert_eq!(
            u.with_scheme("foo"),
            Err(ParseError::CannotChangeScheme),
            "special to non-special is forbidden"
        );
        let up = Url::parse("http://a.test:80/")
            .unwrap()
            .with_scheme("https")
            .unwrap();
        assert_eq!(up.with_port(Some(443)).unwrap().as_str(), "https://a.test/");
    }

    #[test]
    fn strip_parts() {
        let u = Url::parse("https://u:p@a.test/x#f").unwrap();
        assert_eq!(u.without_fragment().as_str(), "https://u:p@a.test/x");
        assert_eq!(
            u.with_fragment(Some("a b")).as_str(),
            "https://u:p@a.test/x#a%20b"
        );
        assert_eq!(u.with_fragment(None), u.without_fragment());
        assert_eq!(u.without_credentials().as_str(), "https://a.test/x#f");
        assert!(!u.without_credentials().has_credentials());
    }

    #[test]
    fn display_debug_fromstr() {
        let u: Url = "https://a.test/".parse().unwrap();
        assert_eq!(u.to_string(), "https://a.test/");
        assert_eq!(format!("{u:?}"), "Url(\"https://a.test/\")");
    }

    #[test]
    fn default_ports() {
        assert_eq!(default_port("https"), Some(443));
        assert_eq!(default_port("wss"), Some(443));
        assert_eq!(default_port("ftp"), Some(21));
        assert_eq!(default_port("file"), None);
    }
}
