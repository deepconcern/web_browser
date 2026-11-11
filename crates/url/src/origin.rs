//! [Origins](https://html.spec.whatwg.org/multipage/browsers.html#concept-origin).

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Host, Url, default_port};

/// An origin: either a (scheme, host, port) tuple or an opaque origin.
///
/// Two opaque origins are equal only if they are the same origin (one was
/// cloned from the other); every call to [`Url::origin`] on a URL with an
/// opaque origin creates a fresh one, as the URL Standard requires.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// An opaque origin, identified by a process-unique number.
    Opaque(OpaqueId),
    /// A tuple origin.
    Tuple {
        /// The scheme, e.g. `https`.
        scheme: String,
        /// The host.
        host: Host,
        /// The port, or `None` when it is the scheme's default.
        port: Option<u16>,
    },
}

/// The identity of an opaque origin. Only obtainable via [`Origin::new_opaque`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpaqueId(u64);

static NEXT_OPAQUE: AtomicU64 = AtomicU64::new(1);

impl Origin {
    /// Creates a new opaque origin, distinct from every other origin.
    pub fn new_opaque() -> Origin {
        Origin::Opaque(OpaqueId(NEXT_OPAQUE.fetch_add(1, Ordering::Relaxed)))
    }

    /// Computes a URL's origin
    /// ([URL Standard §4](https://url.spec.whatwg.org/#concept-url-origin)).
    pub(crate) fn of(url: &Url) -> Origin {
        match url.scheme() {
            "blob" => {
                // M1 follow-up: consult the blob URL store once blobs exist.
                match Url::parse(url.path()) {
                    Ok(inner) if matches!(inner.scheme(), "http" | "https" | "file") => {
                        inner.origin()
                    }
                    _ => Origin::new_opaque(),
                }
            }
            "ftp" | "http" | "https" | "ws" | "wss" => match url.host() {
                Some(host) => Origin::Tuple {
                    scheme: url.scheme().to_owned(),
                    host,
                    port: url.port(),
                },
                None => Origin::new_opaque(),
            },
            // `file:` origins are implementation-defined; we make them opaque.
            _ => Origin::new_opaque(),
        }
    }

    /// Whether this is an opaque origin.
    pub fn is_opaque(&self) -> bool {
        matches!(self, Origin::Opaque(_))
    }

    /// The scheme of a tuple origin.
    pub fn scheme(&self) -> Option<&str> {
        match self {
            Origin::Tuple { scheme, .. } => Some(scheme),
            Origin::Opaque(_) => None,
        }
    }

    /// The host of a tuple origin.
    pub fn host(&self) -> Option<&Host> {
        match self {
            Origin::Tuple { host, .. } => Some(host),
            Origin::Opaque(_) => None,
        }
    }

    /// The effective port of a tuple origin (explicit, else scheme default).
    pub fn port_or_default(&self) -> Option<u16> {
        match self {
            Origin::Tuple { scheme, port, .. } => port.or_else(|| default_port(scheme)),
            Origin::Opaque(_) => None,
        }
    }

    /// [Same origin](https://html.spec.whatwg.org/multipage/browsers.html#same-origin).
    pub fn same_origin(&self, other: &Origin) -> bool {
        self == other
    }

    /// The [ASCII serialization](https://html.spec.whatwg.org/multipage/browsers.html#ascii-serialisation-of-an-origin):
    /// `scheme://host[:port]`, or `null` for opaque origins.
    pub fn ascii_serialization(&self) -> String {
        match self {
            Origin::Opaque(_) => "null".to_owned(),
            Origin::Tuple { scheme, host, port } => match port {
                Some(p) => format!("{scheme}://{host}:{p}"),
                None => format!("{scheme}://{host}"),
            },
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.ascii_serialization())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(s: &str) -> Origin {
        Url::parse(s).unwrap().origin()
    }

    #[test]
    fn tuple_origins() {
        assert_eq!(
            o("https://a.test/x").ascii_serialization(),
            "https://a.test"
        );
        assert_eq!(
            o("https://a.test:444/").ascii_serialization(),
            "https://a.test:444"
        );
        assert_eq!(
            o("http://[::1]:8080/").ascii_serialization(),
            "http://[::1]:8080"
        );
        assert!(o("https://a.test/x").same_origin(&o("https://a.test:443/y")));
        assert!(!o("https://a.test/").same_origin(&o("http://a.test/")));
        assert!(!o("https://a.test/").same_origin(&o("https://b.a.test/")));
        assert_eq!(o("https://a.test/").port_or_default(), Some(443));
        assert_eq!(o("wss://a.test/").scheme(), Some("wss"));
    }

    #[test]
    fn opaque_origins() {
        let d = Url::parse("data:text/plain,x").unwrap();
        assert!(d.origin().is_opaque());
        assert_ne!(d.origin(), d.origin(), "each opaque origin is unique");
        let one = d.origin();
        assert_eq!(one.clone(), one);
        assert_eq!(one.ascii_serialization(), "null");
        assert!(o("file:///etc/passwd").is_opaque());
        assert!(one.host().is_none());
    }

    #[test]
    fn blob_origins() {
        assert_eq!(
            o("blob:https://a.test:8443/uuid").ascii_serialization(),
            "https://a.test:8443"
        );
        assert!(o("blob:data:text/plain,x").is_opaque());
        assert!(o("blob:not-a-url").is_opaque());
    }
}
