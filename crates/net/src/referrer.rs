//! The `Referer` header: W3C [Referrer Policy](https://w3c.github.io/webappsec-referrer-policy/)
//! §8.3 "Determine request's Referrer", plus a privacy cap of our own: a
//! referrer sent to a *different site* is never more than the origin,
//! whatever the policy says.

use wb_url::{Host, Url};

/// A [referrer policy](https://w3c.github.io/webappsec-referrer-policy/#referrer-policies).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReferrerPolicy {
    /// `no-referrer`
    NoReferrer,
    /// `no-referrer-when-downgrade`
    NoReferrerWhenDowngrade,
    /// `same-origin`
    SameOrigin,
    /// `origin`
    Origin,
    /// `strict-origin`
    StrictOrigin,
    /// `origin-when-cross-origin`
    OriginWhenCrossOrigin,
    /// `strict-origin-when-cross-origin` (the default)
    #[default]
    StrictOriginWhenCrossOrigin,
    /// `unsafe-url`
    UnsafeUrl,
}

impl ReferrerPolicy {
    /// Parses one policy token (ASCII case-insensitive).
    pub fn from_token(token: &str) -> Option<ReferrerPolicy> {
        Some(match token.trim().to_ascii_lowercase().as_str() {
            "no-referrer" => Self::NoReferrer,
            "no-referrer-when-downgrade" => Self::NoReferrerWhenDowngrade,
            "same-origin" => Self::SameOrigin,
            "origin" => Self::Origin,
            "strict-origin" => Self::StrictOrigin,
            "origin-when-cross-origin" => Self::OriginWhenCrossOrigin,
            "strict-origin-when-cross-origin" => Self::StrictOriginWhenCrossOrigin,
            "unsafe-url" => Self::UnsafeUrl,
            _ => return None,
        })
    }

    /// [Parses a `Referrer-Policy` header](https://w3c.github.io/webappsec-referrer-policy/#parse-referrer-policy-from-header):
    /// the last recognized token wins; `None` if there is none.
    pub fn from_header(value: &str) -> Option<ReferrerPolicy> {
        value.rsplit(',').find_map(Self::from_token)
    }
}

/// [Is a URL potentially trustworthy](https://w3c.github.io/webappsec-secure-contexts/#is-url-trustworthy)?
pub(crate) fn is_potentially_trustworthy(url: &Url) -> bool {
    match url.scheme() {
        "about" => matches!(url.path(), "blank" | "srcdoc"),
        "data" | "https" | "wss" | "file" => true,
        _ => match url.host() {
            Some(Host::Ipv4(a)) => a.is_loopback(),
            Some(Host::Ipv6(a)) => a.is_loopback(),
            Some(Host::Domain(d)) => {
                let d = d.strip_suffix('.').unwrap_or(&d).to_owned();
                d == "localhost" || d.ends_with(".localhost")
            }
            None => false,
        },
    }
}

/// [Strip a URL for use as a referrer](https://w3c.github.io/webappsec-referrer-policy/#strip-url).
fn strip(url: &Url, origin_only: bool) -> Option<Url> {
    if matches!(url.scheme(), "about" | "blob" | "data") {
        return None;
    }
    let url = url.without_credentials().without_fragment();
    if origin_only {
        let origin = url.origin();
        if origin.is_opaque() {
            return None;
        }
        return Url::parse(&format!("{}/", origin.ascii_serialization())).ok();
    }
    Some(url)
}

/// Determines the `Referer` to send for a request to `current` from a
/// document at `referrer`, under `policy`.
pub(crate) fn determine(
    referrer: Option<&Url>,
    policy: ReferrerPolicy,
    current: &Url,
) -> Option<Url> {
    let referrer = referrer?;
    let origin = strip(referrer, true)?;
    let mut full = strip(referrer, false)?;
    if full.as_str().len() > 4096 {
        full = origin.clone();
    }
    if origin.as_str().len() > 4096 {
        return None;
    }
    let same_origin = referrer.origin().same_origin(&current.origin());
    let downgrade = is_potentially_trustworthy(referrer) && !is_potentially_trustworthy(current);
    let chosen = match policy {
        ReferrerPolicy::NoReferrer => None,
        ReferrerPolicy::Origin => Some(origin.clone()),
        ReferrerPolicy::UnsafeUrl => Some(full),
        ReferrerPolicy::StrictOrigin => (!downgrade).then(|| origin.clone()),
        ReferrerPolicy::StrictOriginWhenCrossOrigin => {
            if same_origin {
                Some(full)
            } else if downgrade {
                None
            } else {
                Some(origin.clone())
            }
        }
        ReferrerPolicy::SameOrigin => same_origin.then_some(full),
        ReferrerPolicy::OriginWhenCrossOrigin => {
            Some(if same_origin { full } else { origin.clone() })
        }
        ReferrerPolicy::NoReferrerWhenDowngrade => (!downgrade).then_some(full),
    }?;
    // Privacy cap: across sites, never reveal more than the origin.
    if !referrer.site().same_site(&current.site()) {
        return Some(origin);
    }
    Some(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }
    fn d(r: &str, p: ReferrerPolicy, c: &str) -> Option<String> {
        determine(Some(&u(r)), p, &u(c)).map(|u| u.to_string())
    }

    #[test]
    fn default_policy() {
        let p = ReferrerPolicy::default();
        assert_eq!(
            d("https://u:p@a.test/page?q#frag", p, "https://a.test/img").as_deref(),
            Some("https://a.test/page?q")
        );
        assert_eq!(
            d("https://a.test/page", p, "https://b.a.test/").as_deref(),
            Some("https://a.test/")
        );
        assert_eq!(
            d("https://a.test/page", p, "https://other.test/").as_deref(),
            Some("https://a.test/")
        );
        assert_eq!(d("https://a.test/page", p, "http://other.test/"), None);
        assert_eq!(determine(None, p, &u("https://a.test/")), None);
        assert_eq!(d("data:text/html,x", p, "https://a.test/"), None);
    }

    #[test]
    fn cross_site_is_capped() {
        for p in [
            ReferrerPolicy::UnsafeUrl,
            ReferrerPolicy::NoReferrerWhenDowngrade,
            ReferrerPolicy::OriginWhenCrossOrigin,
        ] {
            assert_eq!(
                d("https://a.test/secret/path?x", p, "https://b.test/").as_deref(),
                Some("https://a.test/"),
                "{p:?}"
            );
        }
        // Same site, different origin: the policy decides.
        assert_eq!(
            d(
                "https://a.test/p",
                ReferrerPolicy::UnsafeUrl,
                "https://x.a.test/"
            )
            .as_deref(),
            Some("https://a.test/p")
        );
    }

    #[test]
    fn other_policies() {
        assert_eq!(
            d(
                "https://a.test/p",
                ReferrerPolicy::NoReferrer,
                "https://a.test/"
            ),
            None
        );
        assert_eq!(
            d(
                "https://a.test/p",
                ReferrerPolicy::Origin,
                "https://a.test/"
            )
            .as_deref(),
            Some("https://a.test/")
        );
        assert_eq!(
            d(
                "https://a.test/p",
                ReferrerPolicy::SameOrigin,
                "https://a.test/x"
            )
            .as_deref(),
            Some("https://a.test/p")
        );
        assert_eq!(
            d(
                "https://a.test/p",
                ReferrerPolicy::SameOrigin,
                "https://b.test/x"
            ),
            None
        );
        assert_eq!(
            d(
                "https://a.test/p",
                ReferrerPolicy::StrictOrigin,
                "http://a.test/"
            ),
            None
        );
        assert_eq!(
            d(
                "http://a.test/p",
                ReferrerPolicy::StrictOrigin,
                "http://a.test/"
            )
            .as_deref(),
            Some("http://a.test/")
        );
        assert_eq!(
            d(
                "https://a.test/p",
                ReferrerPolicy::NoReferrerWhenDowngrade,
                "http://a.test/"
            ),
            None
        );
        assert_eq!(
            d(
                "https://a.test:8443/p",
                ReferrerPolicy::Origin,
                "https://a.test/"
            )
            .as_deref(),
            Some("https://a.test:8443/")
        );
    }

    #[test]
    fn parse_header() {
        assert_eq!(
            ReferrerPolicy::from_header("no-referrer, bogus"),
            Some(ReferrerPolicy::NoReferrer)
        );
        assert_eq!(
            ReferrerPolicy::from_header("origin, Unsafe-URL"),
            Some(ReferrerPolicy::UnsafeUrl)
        );
        assert_eq!(ReferrerPolicy::from_header("bogus"), None);
        assert_eq!(ReferrerPolicy::from_header(""), None);
    }

    #[test]
    fn trustworthy() {
        assert!(is_potentially_trustworthy(&u("https://a.test/")));
        assert!(is_potentially_trustworthy(&u("http://127.0.0.2/")));
        assert!(is_potentially_trustworthy(&u("http://[::1]/")));
        assert!(is_potentially_trustworthy(&u("http://x.localhost/")));
        assert!(is_potentially_trustworthy(&u("about:blank")));
        assert!(!is_potentially_trustworthy(&u("http://a.test/")));
        assert!(!is_potentially_trustworthy(&u("about:config")));
    }

    #[test]
    fn long_referrers_fall_back_to_origin() {
        let long = format!("https://a.test/{}", "x".repeat(5000));
        assert_eq!(
            d(&long, ReferrerPolicy::UnsafeUrl, "https://a.test/").as_deref(),
            Some("https://a.test/")
        );
    }
}
