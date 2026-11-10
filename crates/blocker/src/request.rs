//! The request being checked, and the URL views the matcher needs.

use std::borrow::Cow;

use crate::domains::normalize_host;
use crate::hash::Fnv;

/// The kind of resource a request loads.
///
/// These are the resource types of the Adblock Plus / uBlock Origin filter
/// options (`$script`, `$image`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceType {
    /// A top-level page load (`$document`, `$doc`).
    Document,
    /// A frame or iframe document (`$subdocument`, `$frame`).
    Subdocument,
    /// A style sheet (`$stylesheet`, `$css`).
    Stylesheet,
    /// A script (`$script`).
    Script,
    /// An image (`$image`).
    Image,
    /// A font (`$font`).
    Font,
    /// Audio or video (`$media`).
    Media,
    /// A `fetch()` or `XMLHttpRequest` load (`$xmlhttprequest`, `$xhr`).
    XmlHttpRequest,
    /// A ping or `sendBeacon` (`$ping`, `$beacon`).
    Ping,
    /// A WebSocket connection (`$websocket`).
    WebSocket,
    /// Anything else (`$other`).
    Other,
}

pub(crate) const DOCUMENT_BIT: u16 = 1 << 0;
/// `$object`: plugin content. No request has this type in this browser, but
/// the option is accepted so that `$object,script` still applies to scripts.
pub(crate) const OBJECT_BIT: u16 = 1 << 11;
pub(crate) const ALL_TYPES: u16 = (1 << 12) - 1;
/// Types a filter applies to when it names none: everything but the page
/// itself (Adblock Plus semantics).
pub(crate) const DEFAULT_TYPES: u16 = ALL_TYPES & !DOCUMENT_BIT;

impl ResourceType {
    pub(crate) const fn bit(self) -> u16 {
        match self {
            Self::Document => DOCUMENT_BIT,
            Self::Subdocument => 1 << 1,
            Self::Stylesheet => 1 << 2,
            Self::Script => 1 << 3,
            Self::Image => 1 << 4,
            Self::Font => 1 << 5,
            Self::Media => 1 << 6,
            Self::XmlHttpRequest => 1 << 7,
            Self::Ping => 1 << 8,
            Self::WebSocket => 1 << 9,
            Self::Other => 1 << 10,
        }
    }

    /// The type bit for a filter option name (lowercase), aliases included.
    pub(crate) fn option_bit(name: &str) -> Option<u16> {
        let ty = match name {
            "document" | "doc" => Self::Document,
            "subdocument" | "frame" => Self::Subdocument,
            "stylesheet" | "css" => Self::Stylesheet,
            "script" => Self::Script,
            "image" => Self::Image,
            "font" => Self::Font,
            "media" => Self::Media,
            "xmlhttprequest" | "xhr" => Self::XmlHttpRequest,
            "ping" | "beacon" => Self::Ping,
            "websocket" => Self::WebSocket,
            "other" => Self::Other,
            "object" | "object-subrequest" => return Some(OBJECT_BIT),
            _ => return None,
        };
        Some(ty.bit())
    }
}

/// A view of one URL prepared for matching.
#[derive(Debug, Clone)]
pub(crate) struct Target<'a> {
    /// The URL as given (for `$match-case` and regex filters).
    pub(crate) original: Cow<'a, str>,
    /// ASCII-lowercased URL; byte offsets equal those of `original`.
    pub(crate) lower: String,
    /// Offsets where a `||` pattern may start: the host start and the byte
    /// after each dot inside the host.
    pub(crate) anchors: Vec<usize>,
    /// Sorted, deduplicated hashes of the URL's alphanumeric runs.
    pub(crate) tokens: Vec<u64>,
}

impl<'a> Target<'a> {
    pub(crate) fn new(original: Cow<'a, str>) -> Self {
        let lower = original.to_ascii_lowercase();
        let mut anchors = Vec::new();
        if let Some((start, end)) = host_range(&lower) {
            anchors.push(start);
            if let Some(host) = lower.get(start..end) {
                anchors.extend(
                    host.bytes()
                        .enumerate()
                        .filter(|&(_, b)| b == b'.')
                        .map(|(i, _)| start + i + 1)
                        .filter(|&p| p < end),
                );
            }
        }
        let tokens = url_tokens(lower.as_bytes());
        Self {
            original,
            lower,
            anchors,
            tokens,
        }
    }

    /// The URL's host (lowercase), or `""` when it has none.
    pub(crate) fn host(&self) -> &str {
        host_range(&self.lower)
            .and_then(|(s, e)| self.lower.get(s..e))
            .unwrap_or("")
    }
}

/// Hashes of the alphanumeric runs of `url`, sorted and deduplicated.
fn url_tokens(url: &[u8]) -> Vec<u64> {
    let mut out = Vec::new();
    let mut cur: Option<Fnv> = None;
    for &b in url {
        if b.is_ascii_alphanumeric() {
            cur.get_or_insert_with(Fnv::new)
                .push(b.to_ascii_lowercase());
        } else if let Some(h) = cur.take() {
            out.push(h.finish());
        }
    }
    if let Some(h) = cur {
        out.push(h.finish());
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The byte range of the host in an absolute URL (`scheme://host...`).
pub(crate) fn host_range(url: &str) -> Option<(usize, usize)> {
    let colon = url.find("://")?;
    let scheme = url.get(..colon)?;
    let mut chars = scheme.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    if !first_ok || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) {
        return None;
    }
    let auth_start = colon + 3;
    let rest = url.get(auth_start..)?;
    let auth_len = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let auth = rest.get(..auth_len)?;
    let host_off = auth.rfind('@').map_or(0, |i| i + 1);
    let host_part = auth.get(host_off..)?;
    let host_len = if host_part.starts_with('[') {
        host_part.find(']').map_or(host_part.len(), |i| i + 1)
    } else {
        host_part.find(':').unwrap_or(host_part.len())
    };
    let start = auth_start + host_off;
    Some((start, start + host_len))
}

/// A network request to check against the filters.
///
/// Third-party determination by registrable domain is the caller's job (it
/// needs the public suffix list, which lives in `wb-url`); pass the result as
/// `same_site`.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub(crate) url: Target<'a>,
    pub(crate) source: Target<'a>,
    pub(crate) source_host: String,
    pub(crate) resource_type: ResourceType,
    pub(crate) third_party: bool,
}

impl<'a> Request<'a> {
    /// Prepares a request.
    ///
    /// - `url`: the absolute URL being loaded.
    /// - `source`: the URL of the page (or frame) that makes the request, or
    ///   just its host. For a top-level page load pass the page URL itself.
    ///   Pass `""` when there is no source; filters with `$domain=` includes
    ///   then never match.
    /// - `same_site`: whether `url` and `source` share a registrable domain
    ///   (drives `$third-party` / `$first-party`).
    pub fn new(
        url: &'a str,
        source: &'a str,
        resource_type: ResourceType,
        same_site: bool,
    ) -> Self {
        let url = Target::new(Cow::Borrowed(url));
        let (source, source_host) = if source.contains("://") {
            let t = Target::new(Cow::Borrowed(source));
            let host = normalize_host(t.host());
            (t, host)
        } else {
            let host = normalize_host(source);
            let synthetic = if host.is_empty() {
                String::new()
            } else {
                format!("https://{host}/")
            };
            (Target::new(Cow::Owned(synthetic)), host)
        };
        Self {
            url,
            source,
            source_host,
            resource_type,
            third_party: !same_site,
        }
    }

    /// The resource type given to [`Request::new`].
    pub fn resource_type(&self) -> ResourceType {
        self.resource_type
    }

    /// Whether the request is third-party (`!same_site`).
    pub fn is_third_party(&self) -> bool {
        self.third_party
    }

    /// The source page's host, lowercase (`""` if none).
    pub fn source_host(&self) -> &str {
        &self.source_host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_ranges() {
        let u = "https://user:pw@Sub.Example.com:8080/path?q#f";
        let (s, e) = host_range(u).unwrap();
        assert_eq!(&u[s..e], "Sub.Example.com");
        let u = "http://[::1]:80/";
        let (s, e) = host_range(u).unwrap();
        assert_eq!(&u[s..e], "[::1]");
        assert_eq!(host_range("data:text/plain,hi"), None);
        assert_eq!(host_range("1http://x/"), None);
        let u = "https://example.com";
        let (s, e) = host_range(u).unwrap();
        assert_eq!(&u[s..e], "example.com");
    }

    #[test]
    fn anchors_and_tokens() {
        let t = Target::new(Cow::Borrowed("https://a.b.com/x"));
        assert_eq!(t.anchors, vec![8, 10, 12]);
        assert_eq!(t.host(), "a.b.com");
        // https, a, b, com, x
        assert_eq!(t.tokens.len(), 5);
    }

    #[test]
    fn request_sources() {
        let r = Request::new(
            "https://x.com/a.js",
            "Example.COM",
            ResourceType::Script,
            false,
        );
        assert_eq!(r.source_host(), "example.com");
        assert!(r.is_third_party());
        assert_eq!(r.source.lower, "https://example.com/");
        let r = Request::new(
            "https://x.com/",
            "https://www.x.com/p",
            ResourceType::Image,
            true,
        );
        assert_eq!(r.source_host(), "www.x.com");
        assert!(!r.is_third_party());
        let r = Request::new("https://x.com/", "", ResourceType::Document, true);
        assert_eq!(r.source_host(), "");
        assert!(r.source.anchors.is_empty());
    }

    #[test]
    fn option_names() {
        assert_eq!(
            ResourceType::option_bit("css"),
            Some(ResourceType::Stylesheet.bit())
        );
        assert_eq!(ResourceType::option_bit("object"), Some(OBJECT_BIT));
        assert_eq!(ResourceType::option_bit("popup"), None);
    }
}
