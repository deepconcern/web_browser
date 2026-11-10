//! The fetch loop: policy gate, scheme dispatch, HTTPS-only upgrade,
//! cookies, referrer, cache, HTTP, decoding and redirects. Follows the
//! shape of the Fetch Standard's [main fetch](https://fetch.spec.whatwg.org/#main-fetch),
//! [HTTP-redirect fetch](https://fetch.spec.whatwg.org/#http-redirect-fetch) and
//! [HTTP-network-or-cache fetch](https://fetch.spec.whatwg.org/#http-network-or-cache-fetch),
//! minus CORS and service workers (M1 follow-ups).

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use wb_url::{Host, Site, Url};

use crate::cache::{CachedResponse, Lookup, MemoryCache};
use crate::cookies::{CookieContext, CookieJar};
use crate::data_url;
use crate::date::now;
use crate::decode::{ACCEPT_ENCODING, decode};
use crate::error::{NetError, UpgradeFailure};
use crate::policy::PolicyGate;
use crate::referrer::{ReferrerPolicy, determine};
use crate::transport::{Counters, Http, map_client_error, to_uri};
use crate::types::{
    Headers, HttpVersion, Method, RedirectMode, Request, RequestContext, Response,
    isomorphic_decode, isomorphic_encode,
};

/// The hard upper bound on redirects (Fetch §4.4 step 7).
pub(crate) const MAX_REDIRECTS: u32 = 20;

/// Everything a fetch needs, shared by all clones of a client.
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) gate: PolicyGate,
    pub(crate) insecure_hosts: Mutex<HashSet<Host>>,
    pub(crate) cookies: CookieJar,
    pub(crate) cache: MemoryCache,
    pub(crate) http: Http,
    pub(crate) counters: Arc<Counters>,
    pub(crate) user_agent: String,
    pub(crate) request_timeout: Duration,
    pub(crate) max_body_bytes: u64,
    pub(crate) max_redirects: u32,
}

impl Shared {
    pub(crate) fn insecure_allowed(&self, url: &Url) -> bool {
        let site = url.site();
        let Some(host) = site.host() else {
            return false;
        };
        self.insecure_hosts
            .lock()
            .map(|set| set.contains(host))
            .unwrap_or_else(|e| e.into_inner().contains(host))
    }
}

/// [Forbidden request-headers](https://fetch.spec.whatwg.org/#forbidden-request-header)
/// that the caller may not set; we set the ones we need ourselves.
fn is_forbidden_request_header(name: &str) -> bool {
    matches!(
        name,
        "accept-charset"
            | "accept-encoding"
            | "access-control-request-headers"
            | "access-control-request-method"
            | "connection"
            | "content-length"
            | "cookie"
            | "cookie2"
            | "date"
            | "dnt"
            | "expect"
            | "host"
            | "keep-alive"
            | "origin"
            | "referer"
            | "set-cookie"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "via"
    ) || name.starts_with("proxy-")
        || name.starts_with("sec-")
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// A response for one hop, before redirect handling.
struct Hop {
    status: u16,
    headers: Headers,
    body: Vec<u8>,
    version: HttpVersion,
    from_cache: bool,
}

/// Runs a whole fetch under the request time limit.
pub(crate) async fn fetch(shared: Arc<Shared>, request: Request) -> Result<Response, NetError> {
    let limit = shared.request_timeout;
    match tokio::time::timeout(limit, fetch_inner(&shared, request)).await {
        Ok(r) => r,
        Err(_) => Err(NetError::Timeout),
    }
}

async fn fetch_inner(shared: &Shared, mut req: Request) -> Result<Response, NetError> {
    if matches!(req.method, Method::Get | Method::Head) && !req.body.is_empty() {
        return Err(NetError::InvalidRequest(
            "GET and HEAD requests cannot have a body".into(),
        ));
    }
    let mut url_list: Vec<Url> = Vec::new();
    let mut redirects = 0u32;
    let mut policy = req.referrer_policy;
    loop {
        // The single policy gate, before anything else happens, on every hop.
        shared.gate.admit(&req.context)?;
        let current = req.url.clone();
        match current.scheme() {
            "data" => {
                url_list.push(current.clone());
                let d = data_url::process(&current).ok_or(NetError::InvalidDataUrl)?;
                let mut headers = Headers::new();
                headers.append("content-type", &d.mime_type);
                return Ok(Response {
                    url: current,
                    url_list,
                    status: 200,
                    headers,
                    body: d.body,
                    version: HttpVersion::None,
                    from_cache: false,
                });
            }
            "about" if current.path() == "blank" => {
                url_list.push(current.clone());
                let mut headers = Headers::new();
                headers.append("content-type", "text/html;charset=utf-8");
                return Ok(Response {
                    url: current,
                    url_list,
                    status: 200,
                    headers,
                    body: Vec::new(),
                    version: HttpVersion::None,
                    from_cache: false,
                });
            }
            "http" | "https" => {}
            other => return Err(NetError::UnsupportedScheme(other.to_owned())),
        }
        // HTTPS-only mode: upgrade unless the user made an exception.
        let upgraded = current.scheme() == "http" && !shared.insecure_allowed(&current);
        let target = if upgraded {
            current
                .with_scheme("https")
                .map_err(|e| NetError::InvalidRequest(e.to_string()))?
        } else {
            current.clone()
        };
        url_list.push(target.clone());
        let hop = match http_fetch(shared, &req, &target, &url_list, policy).await {
            Ok(h) => h,
            Err(e)
                if upgraded
                    && matches!(
                        e,
                        NetError::Connect(_) | NetError::Tls(_) | NetError::Timeout
                    ) =>
            {
                return Err(NetError::HttpsUpgradeFailed(Box::new(UpgradeFailure {
                    site: current.site(),
                    url: current,
                    reason: e.to_string(),
                })));
            }
            Err(e) => return Err(e),
        };
        let location = hop.headers.get("location").map(str::to_owned);
        let Some(location) = location.filter(|_| is_redirect_status(hop.status)) else {
            return Ok(Response {
                url: target,
                url_list,
                status: hop.status,
                headers: hop.headers,
                body: hop.body,
                version: hop.version,
                from_cache: hop.from_cache,
            });
        };
        match req.redirect_mode {
            RedirectMode::Manual => {
                return Ok(Response {
                    url: target,
                    url_list,
                    status: hop.status,
                    headers: hop.headers,
                    body: hop.body,
                    version: hop.version,
                    from_cache: hop.from_cache,
                });
            }
            RedirectMode::Error => return Err(NetError::RedirectNotAllowed),
            RedirectMode::Follow => {}
        }
        // HTTP-redirect fetch.
        redirects += 1;
        if redirects > shared.max_redirects {
            return Err(NetError::TooManyRedirects);
        }
        let mut next = target
            .join(&location)
            .map_err(|e| NetError::InvalidRedirect(format!("{location}: {e}")))?;
        if !next.is_http_scheme() {
            return Err(NetError::InvalidRedirect(format!(
                "{next} is not an HTTP(S) URL"
            )));
        }
        if next.fragment().is_none()
            && let Some(f) = target.fragment()
        {
            next = next.with_fragment(Some(f));
        }
        let rewrite_to_get = ((hop.status == 301 || hop.status == 302)
            && req.method == Method::Post)
            || (hop.status == 303 && !matches!(req.method, Method::Get | Method::Head));
        if rewrite_to_get {
            req.method = Method::Get;
            req.body.clear();
            for name in [
                "content-encoding",
                "content-language",
                "content-location",
                "content-type",
            ] {
                req.headers.remove(name);
            }
        }
        if !target.origin().same_origin(&next.origin()) {
            req.headers.remove("authorization");
        }
        if let Some(p) = hop
            .headers
            .get("referrer-policy")
            .and_then(ReferrerPolicy::from_header)
        {
            policy = p;
        }
        req.url = next;
    }
}

/// The cookie context for a request, or `None` if it gets no cookies
/// (background requests, opaque sites, and third-party requests).
fn cookie_context(req: &Request, url: &Url, url_list: &[Url]) -> Option<(CookieContext, Site)> {
    let site = url.site();
    let (partition, same_site, top_level_navigation) = match &req.context {
        RequestContext::Background(_) => return None,
        RequestContext::Subresource { top_level_site } => {
            if !site.same_site(top_level_site) {
                // Third-party cookies are blocked outright.
                return None;
            }
            (top_level_site.clone(), true, false)
        }
        RequestContext::Navigation { initiator } => {
            let first = url_list.first().map(Url::site);
            let reference = initiator.clone().or(first)?;
            let same = url_list.iter().all(|u| u.site().same_site(&reference));
            (site.clone(), same, true)
        }
    };
    if partition.is_opaque() {
        return None;
    }
    Some((
        CookieContext {
            partition: partition.to_string(),
            same_site,
            top_level_navigation,
            safe_method: req.method.is_safe(),
            http_api: true,
        },
        partition,
    ))
}

/// The cache partition for a request.
fn cache_partition(req: &Request, url: &Url) -> String {
    match &req.context {
        RequestContext::Navigation { .. } => url.site().to_string(),
        RequestContext::Subresource { top_level_site } => top_level_site.to_string(),
        RequestContext::Background(kind) => format!("background:{kind:?}"),
    }
}

/// One HTTP(S) round trip, through the cache.
async fn http_fetch(
    shared: &Shared,
    req: &Request,
    url: &Url,
    url_list: &[Url],
    policy: ReferrerPolicy,
) -> Result<Hop, NetError> {
    let cookie_ctx = cookie_context(req, url, url_list);
    let partition = cache_partition(req, url);
    let cache_key = url.without_fragment().to_string();

    let mut headers = Headers::new();
    for (name, value) in req.headers.iter() {
        if !is_forbidden_request_header(name) {
            headers.append(name, value);
        }
    }
    if !headers.contains("accept") {
        headers.append("accept", "*/*");
    }
    if !headers.contains("user-agent") {
        headers.append("user-agent", &shared.user_agent);
    }
    headers.append("accept-encoding", ACCEPT_ENCODING);
    if !matches!(req.context, RequestContext::Background(_))
        && let Some(r) = determine(req.referrer.as_ref(), policy, url)
    {
        headers.append("referer", r.as_str());
    }
    if let Some((ctx, _)) = &cookie_ctx
        && let Some(c) = shared.cookies.cookie_string(url, ctx, now())
    {
        headers.append("cookie", &c);
    }

    let is_get = req.method == Method::Get;
    let mut revalidating = false;
    if is_get {
        match shared
            .cache
            .lookup(&partition, &cache_key, &headers, req.cache_mode, now())
        {
            Lookup::Fresh(r) => {
                return Ok(Hop {
                    status: r.status,
                    headers: r.headers,
                    body: r.body.as_ref().clone(),
                    version: r.version,
                    from_cache: true,
                });
            }
            Lookup::Revalidate(conditional) => {
                let caller_conditional =
                    headers.contains("if-none-match") || headers.contains("if-modified-since");
                if !caller_conditional {
                    for (name, value) in conditional {
                        headers.append(name, &value);
                    }
                    revalidating = true;
                }
            }
            Lookup::Miss => {}
        }
    }

    let mut builder = http::Request::builder()
        .method(
            http::Method::from_bytes(req.method.as_str().as_bytes())
                .map_err(|e| NetError::InvalidRequest(e.to_string()))?,
        )
        .uri(to_uri(url)?);
    for (name, value) in headers.iter() {
        let bytes = isomorphic_encode(value)
            .ok_or_else(|| NetError::InvalidRequest(format!("header {name} is not Latin-1")))?;
        let value = http::HeaderValue::from_bytes(&bytes)
            .map_err(|e| NetError::InvalidRequest(format!("header {name}: {e}")))?;
        builder = builder.header(name, value);
    }
    let http_request = builder
        .body(Full::new(Bytes::from(req.body.clone())))
        .map_err(|e| NetError::InvalidRequest(e.to_string()))?;

    let request_time = now();
    shared.counters.requests.fetch_add(1, Ordering::Relaxed);
    let response = shared
        .http
        .request(http_request)
        .await
        .map_err(map_client_error)?;
    let status = response.status().as_u16();
    let version = match response.version() {
        http::Version::HTTP_10 | http::Version::HTTP_09 => HttpVersion::Http10,
        http::Version::HTTP_2 => HttpVersion::Http2,
        _ => HttpVersion::Http11,
    };
    let mut response_headers = Headers::new();
    for (name, value) in response.headers() {
        response_headers.append(name.as_str(), &isomorphic_decode(value.as_bytes()));
    }
    let limit = shared.max_body_bytes;
    let raw = Limited::new(
        response.into_body(),
        usize::try_from(limit).unwrap_or(usize::MAX),
    )
    .collect()
    .await
    .map_err(|e| {
        if e.downcast_ref::<LengthLimitError>().is_some() {
            NetError::BodyTooLarge { limit }
        } else {
            NetError::Protocol(e.to_string())
        }
    })?
    .to_bytes();
    let response_time = now();

    if let Some((ctx, _)) = &cookie_ctx {
        for value in response_headers.get_all("set-cookie") {
            shared.cookies.set_cookie(url, value, ctx, response_time);
        }
    }

    let has_body = req.method != Method::Head && !matches!(status, 204 | 304);
    let body = if has_body {
        match response_headers.get_combined("content-encoding") {
            Some(ce) => decode(raw.to_vec(), &ce, limit)?,
            None => raw.to_vec(),
        }
    } else {
        Vec::new()
    };

    if status == 304
        && revalidating
        && let Some(r) = shared.cache.freshen(
            &partition,
            &cache_key,
            &response_headers,
            request_time,
            response_time,
        )
    {
        return Ok(Hop {
            status: r.status,
            headers: r.headers,
            body: r.body.as_ref().clone(),
            version: r.version,
            from_cache: true,
        });
    }

    if is_get {
        shared.cache.store(
            &partition,
            &cache_key,
            &headers,
            CachedResponse {
                status,
                headers: response_headers.clone(),
                body: Arc::new(body.clone()),
                version,
            },
            req.cache_mode,
            request_time,
            response_time,
        );
    } else if !req.method.is_safe() && status < 400 {
        // RFC 9111 §4.4: unsafe methods invalidate the target and, if same
        // origin, the Location and Content-Location URLs.
        shared.cache.invalidate(&partition, &cache_key);
        for name in ["location", "content-location"] {
            if let Some(other) = response_headers.get(name).and_then(|l| url.join(l).ok())
                && other.origin().same_origin(&url.origin())
            {
                shared
                    .cache
                    .invalidate(&partition, other.without_fragment().as_str());
            }
        }
    }

    Ok(Hop {
        status,
        headers: response_headers,
        body,
        version,
        from_cache: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::BackgroundKind;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn forbidden_headers() {
        assert!(is_forbidden_request_header("cookie"));
        assert!(is_forbidden_request_header("sec-fetch-mode"));
        assert!(is_forbidden_request_header("proxy-authorization"));
        assert!(!is_forbidden_request_header("accept-language"));
    }

    #[test]
    fn cookie_contexts() {
        let top = u("https://a.test/");
        let same = Request::subresource(u("https://cdn.a.test/x"), &top);
        let (ctx, _) = cookie_context(&same, &same.url, std::slice::from_ref(&same.url)).unwrap();
        assert_eq!(ctx.partition, "https://a.test");
        assert!(ctx.same_site && !ctx.top_level_navigation);

        let third = Request::subresource(u("https://tracker.test/x"), &top);
        assert!(cookie_context(&third, &third.url, std::slice::from_ref(&third.url)).is_none());

        let bg = Request::background(u("https://lists.test/"), BackgroundKind::FilterListDownload);
        assert!(cookie_context(&bg, &bg.url, std::slice::from_ref(&bg.url)).is_none());

        let mut nav = Request::navigate(u("https://b.test/"));
        nav.context = RequestContext::Navigation {
            initiator: Some(u("https://a.test/").site()),
        };
        let (ctx, _) = cookie_context(&nav, &nav.url, &[nav.url.clone()]).unwrap();
        assert_eq!(ctx.partition, "https://b.test");
        assert!(!ctx.same_site && ctx.top_level_navigation);

        let typed = Request::navigate(u("https://b.test/"));
        let chain = [u("https://b.test/"), u("https://c.test/")];
        let (ctx, _) = cookie_context(&typed, &chain[1], &chain).unwrap();
        assert_eq!(ctx.partition, "https://c.test");
        assert!(!ctx.same_site, "a cross-site redirect taints the chain");

        let data_top = Request::subresource(u("https://a.test/"), &u("data:text/html,x"));
        assert!(cookie_context(&data_top, &data_top.url, &[]).is_none());
    }

    #[test]
    fn partitions() {
        let top = u("https://a.test/");
        let r = Request::subresource(u("https://a.test/x"), &top);
        assert_eq!(cache_partition(&r, &r.url), "https://a.test");
        let n = Request::navigate(u("https://www.b.test/"));
        assert_eq!(cache_partition(&n, &n.url), "https://b.test");
        let b = Request::background(u("https://x.test/"), BackgroundKind::UpdateCheck);
        assert_eq!(cache_partition(&b, &b.url), "background:UpdateCheck");
    }
}
