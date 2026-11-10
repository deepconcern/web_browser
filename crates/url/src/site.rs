//! [Sites](https://html.spec.whatwg.org/multipage/browsers.html#site): the
//! unit the network stack partitions cookies and caches by.

use std::fmt;

use crate::{Host, Origin, Url, registrable_domain};

/// A *site*: an opaque origin, or a scheme together with a registrable domain
/// (or, for IP addresses and hosts without one, the host itself).
///
/// Sites are schemeful: `http://example.com` and `https://example.com` are
/// different sites.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Site {
    /// The site of an opaque origin is the origin itself.
    Opaque(Origin),
    /// A scheme-and-host site.
    SchemeAndHost {
        /// The scheme, e.g. `https`.
        scheme: String,
        /// The registrable domain, or the whole host if it has none.
        host: Host,
    },
}

impl Site {
    /// [Obtains the site](https://html.spec.whatwg.org/multipage/browsers.html#obtain-a-site)
    /// of an origin.
    pub fn of_origin(origin: &Origin) -> Site {
        match origin {
            Origin::Opaque(_) => Site::Opaque(origin.clone()),
            Origin::Tuple { scheme, host, .. } => {
                let host = match registrable_domain(host) {
                    Some(d) => Host::Domain(d),
                    None => host.clone(),
                };
                Site::SchemeAndHost {
                    scheme: scheme.clone(),
                    host,
                }
            }
        }
    }

    /// The site of a URL's origin.
    pub fn of_url(url: &Url) -> Site {
        url.site()
    }

    /// [Same site](https://html.spec.whatwg.org/multipage/browsers.html#same-site)
    /// (schemeful).
    pub fn same_site(&self, other: &Site) -> bool {
        self == other
    }

    /// Whether this is the site of an opaque origin.
    pub fn is_opaque(&self) -> bool {
        matches!(self, Site::Opaque(_))
    }

    /// The scheme, unless opaque.
    pub fn scheme(&self) -> Option<&str> {
        match self {
            Site::SchemeAndHost { scheme, .. } => Some(scheme),
            Site::Opaque(_) => None,
        }
    }

    /// The host (registrable domain or full host), unless opaque.
    pub fn host(&self) -> Option<&Host> {
        match self {
            Site::SchemeAndHost { host, .. } => Some(host),
            Site::Opaque(_) => None,
        }
    }
}

/// Serializes as `scheme://host`, or `null` for an opaque site.
impl fmt::Display for Site {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Site::Opaque(_) => f.write_str("null"),
            Site::SchemeAndHost { scheme, host } => write!(f, "{scheme}://{host}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(u: &str) -> Site {
        Url::parse(u).unwrap().site()
    }

    #[test]
    fn sites() {
        assert_eq!(
            s("https://www.example.co.uk/x").to_string(),
            "https://example.co.uk"
        );
        assert!(s("https://a.example.com/").same_site(&s("https://b.example.com:8443/")));
        assert!(!s("https://example.com/").same_site(&s("http://example.com/")));
        assert!(!s("https://a.github.io/").same_site(&s("https://b.github.io/")));
        assert_eq!(s("http://127.0.0.1:8080/").to_string(), "http://127.0.0.1");
        assert_eq!(s("http://localhost:1/").to_string(), "http://localhost");
        assert_eq!(s("http://[::1]/").to_string(), "http://[::1]");
        let opaque = s("data:,x");
        assert!(opaque.is_opaque());
        assert_eq!(opaque.to_string(), "null");
        assert_ne!(opaque, s("data:,x"));
        assert_eq!(s("https://a.b.test/").scheme(), Some("https"));
        assert_eq!(
            s("https://a.b.test/").host(),
            Some(&Host::Domain("b.test".into()))
        );
        assert_eq!(
            Site::of_url(&Url::parse("https://x.com/").unwrap()),
            s("https://y.x.com/")
        );
    }
}
