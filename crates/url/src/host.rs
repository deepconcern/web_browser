//! The URL Standard's [host](https://url.spec.whatwg.org/#concept-host).

use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

/// A URL's host, after host parsing (IDNA-to-ASCII for domains of special URLs).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Host {
    /// A domain (ASCII, lowercase for special URLs) or, for non-special URLs,
    /// an opaque host string.
    Domain(String),
    /// An IPv4 address.
    Ipv4(Ipv4Addr),
    /// An IPv6 address.
    Ipv6(Ipv6Addr),
}

impl Host {
    /// Whether this host is an IP address (v4 or v6).
    pub fn is_ip(&self) -> bool {
        matches!(self, Host::Ipv4(_) | Host::Ipv6(_))
    }

    /// The domain string, if this host is a domain.
    pub fn as_domain(&self) -> Option<&str> {
        match self {
            Host::Domain(d) => Some(d),
            _ => None,
        }
    }

    /// Parses a host string the way a special URL's host would be parsed
    /// ([host parser](https://url.spec.whatwg.org/#concept-host-parser),
    /// not-special = false). IPv6 addresses must be bracketed.
    pub fn parse(input: &str) -> Option<Host> {
        match url::Host::parse(input).ok()? {
            url::Host::Domain(d) => Some(Host::Domain(d)),
            url::Host::Ipv4(a) => Some(Host::Ipv4(a)),
            url::Host::Ipv6(a) => Some(Host::Ipv6(a)),
        }
    }
}

/// Serializes as the [host serializer](https://url.spec.whatwg.org/#concept-host-serializer)
/// does: IPv6 addresses are wrapped in brackets.
impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Domain(d) => f.write_str(d),
            Host::Ipv4(a) => write!(f, "{a}"),
            Host::Ipv6(a) => write!(f, "[{a}]"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hosts() {
        assert_eq!(
            Host::parse("EXAMPLE.com"),
            Some(Host::Domain("example.com".into()))
        );
        assert_eq!(
            Host::parse("0x7f.1"),
            Some(Host::Ipv4(Ipv4Addr::new(127, 0, 0, 1)))
        );
        assert_eq!(Host::parse("[::1]"), Some(Host::Ipv6(Ipv6Addr::LOCALHOST)));
        assert_eq!(Host::parse("a b"), None);
        assert!(Host::parse("[::1]").unwrap().is_ip());
        assert_eq!(Host::parse("x.test").unwrap().as_domain(), Some("x.test"));
    }
}
