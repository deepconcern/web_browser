//! Connections: DNS, TCP, TLS (rustls with the `ring` provider and the
//! platform verifier), and the pooled HTTP/1.1 + HTTP/2 client from
//! `hyper-util`. HTTP/2 is chosen by ALPN.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::error::NetError;

/// Counters for everything that touches the network.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) dns_lookups: AtomicU64,
    /// TCP connection attempts.
    pub(crate) connections: AtomicU64,
    pub(crate) requests: AtomicU64,
}

/// Which connection step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectErrorKind {
    Dns,
    Connect,
    Tls,
    Timeout,
}

/// A connection failure, carried through hyper's error chain.
#[derive(Debug)]
pub(crate) struct ConnectError {
    pub(crate) kind: ConnectErrorKind,
    pub(crate) message: String,
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConnectError {}

impl ConnectError {
    fn new(kind: ConnectErrorKind, message: impl Into<String>) -> ConnectError {
        ConnectError {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn into_net_error(self) -> NetError {
        match self.kind {
            ConnectErrorKind::Dns => NetError::Dns(self.message),
            ConnectErrorKind::Connect => NetError::Connect(self.message),
            ConnectErrorKind::Tls => NetError::Tls(self.message),
            ConnectErrorKind::Timeout => NetError::Timeout,
        }
    }
}

/// A plain or TLS-wrapped TCP stream.
#[derive(Debug)]
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Stream::Plain(s) => s.is_write_vectored(),
            Stream::Tls(s) => s.is_write_vectored(),
        }
    }
}

impl Connection for Stream {
    fn connected(&self) -> Connected {
        match self {
            Stream::Tls(s) if s.get_ref().1.alpn_protocol() == Some(b"h2") => {
                Connected::new().negotiated_h2()
            }
            _ => Connected::new(),
        }
    }
}

/// Opens connections for the hyper client.
#[derive(Clone)]
pub(crate) struct Connector {
    tls: TlsConnector,
    connect_timeout: Duration,
    counters: Arc<Counters>,
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector")
            .field("connect_timeout", &self.connect_timeout)
            .finish_non_exhaustive()
    }
}

/// Builds the rustls client configuration: `ring` crypto, the platform
/// certificate verifier (plus any extra trusted roots), and ALPN.
pub(crate) fn tls_config(
    extra_roots: &[Vec<u8>],
    http2: bool,
) -> Result<rustls::ClientConfig, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots: Vec<CertificateDer<'static>> = extra_roots
        .iter()
        .map(|der| CertificateDer::from(der.clone()))
        .collect();
    let verifier = if roots.is_empty() {
        rustls_platform_verifier::Verifier::new(provider.clone())
    } else {
        rustls_platform_verifier::Verifier::new_with_extra_roots(roots, provider.clone())
    }
    .map_err(|e| NetError::Tls(format!("certificate verifier: {e}")))?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| NetError::Tls(e.to_string()))?
        .dangerous() // Only to plug in the platform verifier, which verifies fully.
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = if http2 {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    Ok(config)
}

impl Connector {
    pub(crate) fn new(
        tls: rustls::ClientConfig,
        connect_timeout: Duration,
        counters: Arc<Counters>,
    ) -> Connector {
        Connector {
            tls: TlsConnector::from(Arc::new(tls)),
            connect_timeout,
            counters,
        }
    }

    async fn connect(self, uri: http::Uri) -> Result<Stream, ConnectError> {
        let host = uri
            .host()
            .ok_or_else(|| ConnectError::new(ConnectErrorKind::Connect, "URL has no host"))?;
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let https = uri.scheme_str() == Some("https");
        let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
        let work = async {
            let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
                Ok(ip) => vec![SocketAddr::new(ip, port)],
                Err(_) => {
                    self.counters.dns_lookups.fetch_add(1, Ordering::Relaxed);
                    tokio::net::lookup_host((host.as_str(), port))
                        .await
                        .map_err(|e| {
                            ConnectError::new(ConnectErrorKind::Dns, format!("{host}: {e}"))
                        })?
                        .collect()
                }
            };
            if addrs.is_empty() {
                return Err(ConnectError::new(
                    ConnectErrorKind::Dns,
                    format!("{host}: no addresses"),
                ));
            }
            // M1 follow-up: Happy Eyeballs (RFC 8305) instead of trying
            // addresses one after another.
            let mut last = None;
            let mut tcp = None;
            for addr in addrs {
                self.counters.connections.fetch_add(1, Ordering::Relaxed);
                match TcpStream::connect(addr).await {
                    Ok(s) => {
                        tcp = Some(s);
                        break;
                    }
                    Err(e) => last = Some(e),
                }
            }
            let tcp = match tcp {
                Some(t) => t,
                None => {
                    let msg = last.map_or_else(|| "no address".to_owned(), |e| e.to_string());
                    return Err(ConnectError::new(
                        ConnectErrorKind::Connect,
                        format!("{host}:{port}: {msg}"),
                    ));
                }
            };
            // Nagle only delays small HTTP writes.
            let _ = tcp.set_nodelay(true);
            if !https {
                return Ok(Stream::Plain(tcp));
            }
            let name = ServerName::try_from(host.clone())
                .map_err(|e| ConnectError::new(ConnectErrorKind::Tls, format!("{host}: {e}")))?;
            let tls = self
                .tls
                .connect(name, tcp)
                .await
                .map_err(|e| ConnectError::new(ConnectErrorKind::Tls, e.to_string()))?;
            Ok(Stream::Tls(Box::new(tls)))
        };
        match tokio::time::timeout(self.connect_timeout, work).await {
            Ok(r) => r,
            Err(_) => Err(ConnectError::new(
                ConnectErrorKind::Timeout,
                "connect timed out",
            )),
        }
    }
}

type ConnectFuture = Pin<Box<dyn Future<Output = Result<TokioIo<Stream>, ConnectError>> + Send>>;

impl tower_service::Service<http::Uri> for Connector {
    type Response = TokioIo<Stream>;
    type Error = ConnectError;
    type Future = ConnectFuture;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let this = self.clone();
        Box::pin(async move { this.connect(uri).await.map(TokioIo::new) })
    }
}

/// The pooled HTTP client.
pub(crate) type Http = HyperClient<Connector, Full<Bytes>>;

/// Builds the pooled client. Must be called inside the runtime context.
pub(crate) fn http_client(connector: Connector) -> Http {
    HyperClient::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(6)
        .pool_timer(TokioTimer::new())
        .timer(TokioTimer::new())
        .build(connector)
}

/// Maps a hyper client error to ours, finding our connect error if there is
/// one in the chain.
pub(crate) fn map_client_error(err: hyper_util::client::legacy::Error) -> NetError {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&err);
    while let Some(e) = source {
        if let Some(c) = e.downcast_ref::<ConnectError>() {
            return ConnectError::new(c.kind, c.message.clone()).into_net_error();
        }
        source = e.source();
    }
    let mut msg = err.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&err);
    while let Some(e) = source {
        msg.push_str(": ");
        msg.push_str(&e.to_string());
        source = e.source();
    }
    if err.is_connect() {
        NetError::Connect(msg)
    } else {
        NetError::Protocol(msg)
    }
}

/// Characters `http::Uri` accepts unescaped in a path or query.
fn uri_safe(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b)
}

/// Converts a URL to the URI hyper sends: no fragment, no credentials, and
/// any byte `http::Uri` rejects percent-encoded.
pub(crate) fn to_uri(url: &wb_url::Url) -> Result<http::Uri, NetError> {
    let clean = url.without_fragment().without_credentials();
    if let Ok(uri) = clean.as_str().parse::<http::Uri>() {
        return Ok(uri);
    }
    let mut escaped = String::with_capacity(clean.as_str().len());
    for b in clean.as_str().bytes() {
        if uri_safe(b) {
            escaped.push(char::from(b));
        } else {
            escaped.push_str(&format!("%{b:02X}"));
        }
    }
    escaped
        .parse::<http::Uri>()
        .map_err(|e| NetError::InvalidRequest(format!("{url}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        let u = wb_url::Url::parse("https://u:p@a.test/x?q={|}^#frag").unwrap();
        let uri = to_uri(&u).unwrap();
        assert_eq!(uri.host(), Some("a.test"));
        assert!(!uri.to_string().contains("frag"));
        assert!(!uri.to_string().contains("u:p"));
    }

    #[test]
    fn tls_config_builds_with_ring() {
        let c = tls_config(&[], true).unwrap();
        assert_eq!(c.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
        let c = tls_config(&[], false).unwrap();
        assert_eq!(c.alpn_protocols, vec![b"http/1.1".to_vec()]);
    }
}
