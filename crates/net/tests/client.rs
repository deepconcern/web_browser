//! End-to-end tests against a local server. No test touches the internet.

// Integration tests: helpers may unwrap freely.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

use common::{black_hole, ca_der, reply, start};
use wb_net::{
    BackgroundKind, CacheMode, Client, ClientConfig, HttpVersion, Method, NetError, NetStats,
    NetworkPolicy, RedirectMode, Request, RequestContext,
};
use wb_url::Url;

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

fn config() -> ClientConfig {
    ClientConfig {
        extra_root_certificates: vec![ca_der()],
        request_timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(5),
        ..ClientConfig::default()
    }
}

/// A client that may use plain HTTP to 127.0.0.1 and localhost.
fn insecure_client(cfg: ClientConfig) -> Client {
    let c = Client::with_config(cfg).unwrap();
    c.allow_insecure_http(&u("http://127.0.0.1/").site());
    c.allow_insecure_http(&u("http://localhost/").site());
    c
}

#[test]
fn fresh_client_makes_zero_outbound_connections() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();

    let client = Client::with_config(config()).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(client.stats(), NetStats::default());

    // data: and about:blank never touch the network.
    let r = client
        .fetch(Request::navigate(u("data:text/plain,hi")))
        .unwrap();
    assert_eq!(r.body, b"hi");
    assert_eq!(r.content_type(), Some("text/plain"));
    let r = client.fetch(Request::navigate(u("about:blank"))).unwrap();
    assert_eq!(r.status, 200);

    // A background request that policy forbids never leaves the gate.
    client.set_background_allowed(BackgroundKind::UpdateCheck, false);
    let err = client
        .fetch(Request::background(
            u(&format!("https://127.0.0.1:{port}/update")),
            BackgroundKind::UpdateCheck,
        ))
        .unwrap_err();
    assert_eq!(err, NetError::BlockedByPolicy(BackgroundKind::UpdateCheck));

    assert_eq!(client.stats(), NetStats::default());
    assert!(matches!(
        listener.accept(),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
fn policy_gate_switches() {
    let server = start(true, |_| reply(200, &[], b"list"));
    let client = Client::with_config(ClientConfig {
        policy: NetworkPolicy::none(),
        ..config()
    })
    .unwrap();
    let url = u(&format!("https://127.0.0.1:{}/list.txt", server.port()));
    let req = Request::background(url.clone(), BackgroundKind::FilterListDownload);
    assert_eq!(
        client.fetch(req.clone()).unwrap_err(),
        NetError::BlockedByPolicy(BackgroundKind::FilterListDownload)
    );
    assert_eq!(server.accepts(), 0);
    client.set_background_allowed(BackgroundKind::FilterListDownload, true);
    assert!(client.policy().filter_list_downloads);
    let r = client.fetch(req).unwrap();
    assert_eq!(r.body, b"list");
    // Background requests carry neither cookies nor a referrer.
    let seen = &server.requests()[0];
    assert!(seen.header("cookie").is_none() && seen.header("referer").is_none());
}

#[test]
fn https_only_upgrade_failure_and_exception() {
    let server = start(false, |_| {
        reply(200, &[("content-type", "text/html")], b"plain")
    });
    let client = Client::with_config(config()).unwrap();
    let url = u(&format!("http://127.0.0.1:{}/page", server.port()));
    match client.fetch(Request::navigate(url.clone())) {
        Err(NetError::HttpsUpgradeFailed(f)) => {
            let wb_net::UpgradeFailure {
                url: failed,
                site,
                reason,
            } = *f;
            assert_eq!(failed, url);
            assert_eq!(site.to_string(), "http://127.0.0.1");
            assert!(!reason.is_empty());
            assert!(!client.is_insecure_http_allowed(&site));
            client.allow_insecure_http(&site);
            assert!(client.is_insecure_http_allowed(&site));
        }
        other => panic!("expected an upgrade failure, got {other:?}"),
    }
    let r = client.fetch(Request::navigate(url.clone())).unwrap();
    assert_eq!(r.body, b"plain");
    assert_eq!(r.url, url);
    client.revoke_insecure_http(&url.site());
    assert!(matches!(
        client.fetch(Request::navigate(url)),
        Err(NetError::HttpsUpgradeFailed(_))
    ));
}

#[test]
fn http_is_upgraded_when_https_works() {
    let server = start(true, |s| reply(200, &[], s.path.as_bytes()));
    let client = Client::with_config(config()).unwrap();
    let r = client
        .fetch(Request::navigate(u(&format!(
            "http://127.0.0.1:{}/x",
            server.port()
        ))))
        .unwrap();
    assert_eq!(r.url.scheme(), "https");
    assert_eq!(r.body, b"/x");
}

#[test]
fn http2_and_http1_over_tls() {
    let server = start(true, |_| reply(200, &[], b"ok"));
    let url = u(&format!("https://127.0.0.1:{}/", server.port()));
    let h2 = Client::with_config(config()).unwrap();
    let r = h2.fetch(Request::navigate(url.clone())).unwrap();
    assert_eq!(r.version, HttpVersion::Http2);
    assert_eq!(server.requests()[0].version, http::Version::HTTP_2);
    let h1 = Client::with_config(ClientConfig {
        http2: false,
        ..config()
    })
    .unwrap();
    let r = h1.fetch(Request::navigate(url)).unwrap();
    assert_eq!(r.version, HttpVersion::Http11);
    // Connections are reused.
    let before = h1.stats().connection_attempts;
    for _ in 0..3 {
        h1.fetch(Request::navigate(u(&format!(
            "https://127.0.0.1:{}/again",
            server.port()
        ))))
        .unwrap();
    }
    assert_eq!(h1.stats().connection_attempts, before);
}

#[test]
fn untrusted_certificate_is_rejected() {
    let server = start(true, |_| reply(200, &[], b"ok"));
    let client = Client::new().unwrap(); // Platform roots only.
    let err = client
        .fetch(Request::navigate(u(&format!(
            "https://127.0.0.1:{}/",
            server.port()
        ))))
        .unwrap_err();
    assert!(matches!(err, NetError::Tls(_)), "{err:?}");
}

fn redirect_server() -> common::TestServer {
    start(true, |s| {
        let path = s.path.as_str();
        if let Some(n) = path.strip_prefix("/chain/") {
            let n: u32 = n.parse().unwrap();
            if n == 0 {
                reply(200, &[], b"end")
            } else {
                reply(302, &[("location", &format!("/chain/{}", n - 1))], b"")
            }
        } else if path == "/see-other" {
            reply(303, &[("location", "/echo")], b"")
        } else if path == "/temp" {
            reply(307, &[("location", "/echo")], b"")
        } else if path == "/to-data" {
            reply(302, &[("location", "data:,x")], b"")
        } else if path == "/echo" {
            reply(
                200,
                &[],
                format!("{} {}", s.method, String::from_utf8_lossy(&s.body)).as_bytes(),
            )
        } else {
            reply(404, &[], b"")
        }
    })
}

#[test]
fn redirects() {
    let server = redirect_server();
    let client = Client::with_config(config()).unwrap();
    let base = format!("https://127.0.0.1:{}", server.port());

    let r = client
        .fetch(Request::navigate(u(&format!("{base}/chain/20#frag"))))
        .unwrap();
    assert_eq!(r.body, b"end");
    assert_eq!(r.url_list.len(), 21);
    assert!(r.redirected());
    assert_eq!(r.url.fragment(), Some("frag"), "fragment is inherited");

    assert_eq!(
        client
            .fetch(Request::navigate(u(&format!("{base}/chain/21"))))
            .unwrap_err(),
        NetError::TooManyRedirects
    );

    let mut post = Request::navigate(u(&format!("{base}/see-other")));
    post.method = Method::Post;
    post.body = b"data".to_vec();
    assert_eq!(client.fetch(post.clone()).unwrap().body, b"GET ");
    post.url = u(&format!("{base}/temp"));
    assert_eq!(
        client.fetch(post).unwrap().body,
        b"POST data",
        "307 keeps the method"
    );

    let mut manual = Request::navigate(u(&format!("{base}/chain/1")));
    manual.redirect_mode = RedirectMode::Manual;
    let r = client.fetch(manual.clone()).unwrap();
    assert_eq!(r.status, 302);
    manual.redirect_mode = RedirectMode::Error;
    assert_eq!(
        client.fetch(manual).unwrap_err(),
        NetError::RedirectNotAllowed
    );

    assert!(matches!(
        client.fetch(Request::navigate(u(&format!("{base}/to-data")))),
        Err(NetError::InvalidRedirect(_))
    ));
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[test]
fn content_decoding_and_size_limits() {
    let big = vec![b'z'; 200_000];
    let gz = gzip(&big);
    let server = start(true, move |s| match s.path.as_str() {
        "/gzip" => reply(200, &[("content-encoding", "gzip")], &gz),
        "/br" => reply(
            200,
            &[("content-encoding", "br")],
            &[0x40, 0x00, 0x10, b'h', b'e', b'l', b'l', b'o', 0x03],
        ),
        _ => reply(200, &[], &[b'r'; 50_000]),
    });
    let base = format!("https://127.0.0.1:{}", server.port());
    let client = Client::with_config(config()).unwrap();
    let r = client
        .fetch(Request::navigate(u(&format!("{base}/gzip"))))
        .unwrap();
    assert_eq!(r.body, big);
    assert_eq!(
        server.requests()[0].header("accept-encoding"),
        Some("gzip, deflate, br")
    );
    let r = client
        .fetch(Request::navigate(u(&format!("{base}/br"))))
        .unwrap();
    assert_eq!(r.body, b"hello");

    let small = Client::with_config(ClientConfig {
        max_body_bytes: 10_000,
        ..config()
    })
    .unwrap();
    assert_eq!(
        small
            .fetch(Request::navigate(u(&format!("{base}/raw"))))
            .unwrap_err(),
        NetError::BodyTooLarge { limit: 10_000 }
    );
    assert_eq!(
        small
            .fetch(Request::navigate(u(&format!("{base}/gzip"))))
            .unwrap_err(),
        NetError::BodyTooLarge { limit: 10_000 },
        "decoded size is limited too"
    );
}

#[test]
fn request_timeout_and_cancel() {
    let (_listener, port) = black_hole();
    let client = insecure_client(ClientConfig {
        request_timeout: Duration::from_millis(300),
        ..config()
    });
    let url = u(&format!("http://127.0.0.1:{port}/"));
    assert_eq!(
        client.fetch(Request::navigate(url.clone())).unwrap_err(),
        NetError::Timeout
    );

    let slow = insecure_client(config());
    let handle = slow.start(Request::navigate(url.clone()));
    assert!(handle.poll().is_none());
    handle.cancel();
    assert_eq!(handle.wait().unwrap_err(), NetError::Cancelled);

    let (tx, rx) = mpsc::channel();
    let cancel = slow.start_with_callback(Request::navigate(url), move |r| tx.send(r).unwrap());
    cancel.cancel();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err(),
        NetError::Cancelled
    );
}

#[test]
fn callbacks_and_handles() {
    let server = start(true, |_| reply(200, &[], b"cb"));
    let client = Client::with_config(config()).unwrap();
    let url = u(&format!("https://127.0.0.1:{}/", server.port()));
    let (tx, rx) = mpsc::channel();
    client.start_with_callback(Request::navigate(url.clone()), move |r| tx.send(r).unwrap());
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .body,
        b"cb"
    );
    let h = client.start(Request::navigate(url));
    let r = h.wait_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert!(r.ok());
}

#[test]
fn cookies_are_first_party_and_partitioned() {
    let server = start(false, |s| match s.path.as_str() {
        "/login" => reply(
            200,
            &[
                ("set-cookie", "sid=abc; Path=/"),
                ("set-cookie", "lax=1; SameSite=Lax; Path=/"),
            ],
            b"",
        ),
        "/tracker" => reply(200, &[("set-cookie", "track=1; Path=/")], b""),
        _ => reply(200, &[], b""),
    });
    let p = server.port();
    let client = insecure_client(config());
    let site_a = format!("http://127.0.0.1:{p}");
    let site_b = format!("http://localhost:{p}");

    client
        .fetch(Request::navigate(u(&format!("{site_a}/login"))))
        .unwrap();
    assert_eq!(client.cookies().len(), 2);

    // Same-site subresource: cookies sent.
    let top_a = u(&format!("{site_a}/page"));
    client
        .fetch(Request::subresource(u(&format!("{site_a}/img")), &top_a))
        .unwrap();
    // Third-party subresource from site B's page: nothing sent or stored.
    let top_b = u(&format!("{site_b}/page"));
    client
        .fetch(Request::subresource(
            u(&format!("{site_a}/tracker")),
            &top_b,
        ))
        .unwrap();
    assert_eq!(client.cookies().len(), 2, "third-party Set-Cookie ignored");

    // A cross-site navigation sends Lax cookies (GET) but is still first-party.
    let mut nav = Request::navigate(u(&format!("{site_a}/next")));
    nav.context = RequestContext::Navigation {
        initiator: Some(top_b.site()),
    };
    client.fetch(nav).unwrap();

    let reqs = server.requests();
    let cookie = |path: &str| {
        reqs.iter()
            .find(|r| r.path == path)
            .and_then(|r| r.header("cookie").map(str::to_owned))
    };
    assert_eq!(cookie("/img").as_deref(), Some("sid=abc; lax=1"));
    assert_eq!(cookie("/tracker"), None);
    assert_eq!(cookie("/next").as_deref(), Some("sid=abc; lax=1"));
    assert!(
        client
            .cookies()
            .all()
            .iter()
            .all(|c| c.partition == "http://127.0.0.1")
    );
}

#[test]
fn referrer_is_trimmed_across_sites() {
    let server = start(false, |_| reply(200, &[], b""));
    let p = server.port();
    let client = insecure_client(config());
    let top = u(&format!("http://127.0.0.1:{p}/private/page?secret=1#f"));
    client
        .fetch(Request::subresource(
            u(&format!("http://127.0.0.1:{p}/same")),
            &top,
        ))
        .unwrap();
    let mut cross = Request::subresource(u(&format!("http://localhost:{p}/cross")), &top);
    cross.referrer_policy = wb_net::ReferrerPolicy::UnsafeUrl;
    client.fetch(cross).unwrap();
    let reqs = server.requests();
    assert_eq!(
        reqs[0].header("referer").map(str::to_owned),
        Some(format!("http://127.0.0.1:{p}/private/page?secret=1"))
    );
    assert_eq!(
        reqs[1].header("referer").map(str::to_owned),
        Some(format!("http://127.0.0.1:{p}/"))
    );
}

#[test]
fn memory_cache() {
    let server = start(true, |s| match s.path.as_str() {
        "/fresh" => reply(200, &[("cache-control", "max-age=600")], b"fresh"),
        "/etag" => {
            if s.header("if-none-match") == Some("\"v1\"") {
                reply(304, &[("etag", "\"v1\"")], b"")
            } else {
                reply(
                    200,
                    &[("cache-control", "no-cache"), ("etag", "\"v1\"")],
                    b"tagged",
                )
            }
        }
        _ => reply(200, &[("cache-control", "no-store")], b"never"),
    });
    let base = format!("https://127.0.0.1:{}", server.port());
    let client = Client::with_config(config()).unwrap();
    let get = |path: &str| {
        client
            .fetch(Request::navigate(u(&format!("{base}{path}"))))
            .unwrap()
    };

    assert!(!get("/fresh").from_cache);
    let again = get("/fresh");
    assert!(again.from_cache);
    assert_eq!(again.body, b"fresh");

    assert!(!get("/etag").from_cache);
    let revalidated = get("/etag");
    assert!(revalidated.from_cache);
    assert_eq!(revalidated.status, 200);
    assert_eq!(revalidated.body, b"tagged");

    get("/nostore");
    assert!(!get("/nostore").from_cache);

    let mut reload = Request::navigate(u(&format!("{base}/fresh")));
    reload.cache_mode = CacheMode::Reload;
    assert!(!client.fetch(reload).unwrap().from_cache);

    let count = |p: &str| server.requests().iter().filter(|r| r.path == p).count();
    assert_eq!(count("/fresh"), 2);
    assert_eq!(count("/etag"), 2);
    assert_eq!(count("/nostore"), 2);

    client.clear_cache();
    assert!(!get("/fresh").from_cache);
}

#[test]
fn blocking_fetch_refuses_async_context() {
    let client = Client::with_config(config()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let r = rt.block_on(async { client.fetch(Request::navigate(u("data:,x"))) });
    assert!(matches!(r, Err(NetError::ClientUnavailable(_))));
    drop(rt);
}

#[test]
fn unsupported_schemes_and_bad_requests() {
    let client = Client::with_config(config()).unwrap();
    assert_eq!(
        client
            .fetch(Request::navigate(u("ftp://a.test/")))
            .unwrap_err(),
        NetError::UnsupportedScheme("ftp".into())
    );
    assert_eq!(
        client
            .fetch(Request::navigate(u("data:;base64,%")))
            .unwrap_err(),
        NetError::InvalidDataUrl
    );
    let mut get = Request::navigate(u("data:,x"));
    get.body = b"x".to_vec();
    assert!(matches!(
        client.fetch(get),
        Err(NetError::InvalidRequest(_))
    ));
    assert_eq!(client.stats(), NetStats::default());
}

#[test]
fn bad_verifier_config_inside_async_context_does_not_panic() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let r = rt.block_on(async {
        Client::with_config(ClientConfig {
            extra_root_certificates: vec![b"not a certificate".to_vec()],
            ..ClientConfig::default()
        })
    });
    assert!(matches!(r, Err(NetError::Tls(_))), "{r:?}");
}

#[test]
fn data_urls_respect_the_body_limit() {
    let client = Client::with_config(ClientConfig {
        max_body_bytes: 8,
        ..config()
    })
    .unwrap();
    assert_eq!(
        client
            .fetch(Request::navigate(u("data:,123456789")))
            .unwrap_err(),
        NetError::BodyTooLarge { limit: 8 }
    );
    assert_eq!(
        client
            .fetch(Request::navigate(u("data:,12345678")))
            .unwrap()
            .body,
        b"12345678"
    );
}

#[test]
fn followed_redirect_bodies_are_not_read() {
    let server = start(true, |s| match s.path.as_str() {
        "/corrupt" => reply(
            302,
            &[
                ("location", "/ok"),
                ("content-encoding", "gzip"),
                ("set-cookie", "r=1; Path=/"),
            ],
            b"this is not gzip",
        ),
        "/huge" => reply(301, &[("location", "/ok")], &vec![b'x'; 100_000]),
        _ => reply(200, &[], b"landed"),
    });
    let client = Client::with_config(ClientConfig {
        max_body_bytes: 1000,
        ..config()
    })
    .unwrap();
    let base = format!("https://127.0.0.1:{}", server.port());
    let r = client
        .fetch(Request::navigate(u(&format!("{base}/corrupt"))))
        .unwrap();
    assert_eq!(r.body, b"landed");
    assert_eq!(
        client.cookies().len(),
        1,
        "Set-Cookie on the redirect still applies"
    );
    let r = client
        .fetch(Request::navigate(u(&format!("{base}/huge"))))
        .unwrap();
    assert_eq!(r.body, b"landed");
    // In Manual mode the redirect body is the response, so it is read.
    let mut manual = Request::navigate(u(&format!("{base}/huge")));
    manual.redirect_mode = RedirectMode::Manual;
    assert_eq!(
        client.fetch(manual).unwrap_err(),
        NetError::BodyTooLarge { limit: 1000 }
    );
}

#[test]
fn redirect_cannot_restore_a_removed_referrer() {
    let server = start(false, |s| match s.path.as_str() {
        "/hop" => reply(
            302,
            &[("location", "/end"), ("referrer-policy", "unsafe-url")],
            b"",
        ),
        "/trim" => reply(
            302,
            &[("location", "/end2"), ("referrer-policy", "unsafe-url")],
            b"",
        ),
        _ => reply(200, &[], b""),
    });
    let p = server.port();
    let client = insecure_client(config());
    let top = u(&format!("http://127.0.0.1:{p}/secret/page?q=1"));
    let mut req = Request::subresource(u(&format!("http://127.0.0.1:{p}/hop")), &top);
    req.referrer_policy = wb_net::ReferrerPolicy::NoReferrer;
    client.fetch(req).unwrap();
    let mut req = Request::subresource(u(&format!("http://127.0.0.1:{p}/trim")), &top);
    req.referrer_policy = wb_net::ReferrerPolicy::Origin;
    client.fetch(req).unwrap();
    let reqs = server.requests();
    let referer = |path: &str| {
        reqs.iter()
            .find(|r| r.path == path)
            .and_then(|r| r.header("referer").map(str::to_owned))
    };
    assert_eq!(referer("/hop"), None);
    assert_eq!(
        referer("/end"),
        None,
        "unsafe-url on the redirect cannot bring it back"
    );
    assert_eq!(referer("/trim"), Some(format!("http://127.0.0.1:{p}/")));
    assert_eq!(referer("/end2"), Some(format!("http://127.0.0.1:{p}/")));
}

#[test]
fn preconditioned_and_authorized_requests_skip_the_cache() {
    let server = start(true, |s| {
        if s.header("if-none-match").is_some() {
            reply(304, &[], b"")
        } else if s.header("authorization").is_some() {
            reply(200, &[], b"private")
        } else {
            reply(
                200,
                &[("cache-control", "max-age=600"), ("etag", "\"v\"")],
                b"public",
            )
        }
    });
    let client = Client::with_config(config()).unwrap();
    let url = u(&format!("https://127.0.0.1:{}/doc", server.port()));
    client.fetch(Request::navigate(url.clone())).unwrap();
    assert!(
        client
            .fetch(Request::navigate(url.clone()))
            .unwrap()
            .from_cache
    );

    let mut cond = Request::navigate(url.clone());
    cond.headers.append("if-none-match", "\"v\"");
    let r = client.fetch(cond).unwrap();
    assert_eq!(
        r.status, 304,
        "the caller's precondition goes to the server"
    );
    assert!(!r.from_cache);

    let mut auth = Request::navigate(url);
    auth.headers.append("authorization", "Bearer t");
    let r = client.fetch(auth).unwrap();
    assert_eq!(r.body, b"private");
    assert!(!r.from_cache);
    assert_eq!(server.requests().len(), 3);
}
