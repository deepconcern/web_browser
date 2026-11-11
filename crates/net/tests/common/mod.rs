//! A local HTTP(S) test server. Tests never touch the internet.

#![allow(dead_code)]
// Test-only helper: panicking on setup failure is the right behavior.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::runtime::Runtime;

/// What the server saw for one request.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub version: http::Version,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

pub type Reply = http::Response<Vec<u8>>;
pub type Handler = Arc<dyn Fn(&Seen) -> Reply + Send + Sync>;

pub struct TestServer {
    pub addr: SocketAddr,
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub accepts: Arc<AtomicUsize>,
    _rt: Runtime,
}

impl TestServer {
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
    pub fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }
}

pub fn reply(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let mut b = http::Response::builder().status(status);
    for (n, v) in headers {
        b = b.header(*n, *v);
    }
    b.body(body.to_vec()).unwrap()
}

pub fn ca_der() -> Vec<u8> {
    include_bytes!("../certs/ca.der").to_vec()
}

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cert =
        rustls::pki_types::CertificateDer::from(include_bytes!("../certs/leaf.der").to_vec());
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(
        include_bytes!("../certs/leaf.key.der").to_vec().into(),
    );
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
}

pub fn start(tls: bool, handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> TestServer {
    let handler: Handler = Arc::new(handler);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let accepts = Arc::new(AtomicUsize::new(0));
    let acceptor = tls.then(tls_acceptor);
    {
        let seen = seen.clone();
        let accepts = accepts.clone();
        rt.spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                accepts.fetch_add(1, Ordering::SeqCst);
                let handler = handler.clone();
                let seen = seen.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let svc = service_fn(move |req: http::Request<hyper::body::Incoming>| {
                        let handler = handler.clone();
                        let seen = seen.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            let body = body
                                .collect()
                                .await
                                .map(|b| b.to_bytes().to_vec())
                                .unwrap_or_default();
                            let s = Seen {
                                method: parts.method.to_string(),
                                path: parts
                                    .uri
                                    .path_and_query()
                                    .map(|p| p.to_string())
                                    .unwrap_or_default(),
                                headers: parts
                                    .headers
                                    .iter()
                                    .map(|(n, v)| {
                                        (
                                            n.to_string(),
                                            String::from_utf8_lossy(v.as_bytes()).into_owned(),
                                        )
                                    })
                                    .collect(),
                                version: parts.version,
                                body,
                            };
                            seen.lock().unwrap().push(s.clone());
                            let r = handler(&s);
                            let (parts, body) = r.into_parts();
                            Ok::<_, Infallible>(http::Response::from_parts(
                                parts,
                                Full::new(Bytes::from(body)),
                            ))
                        }
                    });
                    let builder =
                        hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                    match acceptor {
                        Some(a) => {
                            if let Ok(stream) = a.accept(tcp).await {
                                let _ = builder.serve_connection(TokioIo::new(stream), svc).await;
                            }
                        }
                        None => {
                            let _ = builder.serve_connection(TokioIo::new(tcp), svc).await;
                        }
                    }
                });
            }
        });
    }
    TestServer {
        addr,
        seen,
        accepts,
        _rt: rt,
    }
}

/// A server that accepts connections and never answers.
pub fn black_hole() -> (std::net::TcpListener, u16) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    (l, port)
}
