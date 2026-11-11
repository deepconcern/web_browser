//! Network errors.

use std::fmt;

use wb_url::{Site, Url};

use crate::policy::BackgroundKind;

/// Details of a failed HTTPS-only upgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeFailure {
    /// The original `http:` URL.
    pub url: Url,
    /// The site an exception would cover; pass it to
    /// [`Client::allow_insecure_http`](crate::Client::allow_insecure_http).
    pub site: Site,
    /// What went wrong on the `https:` attempt.
    pub reason: String,
}

/// Why a fetch failed. Every failure is a value; nothing in this crate panics
/// on network input.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetError {
    /// The request was refused by the network policy gate because its kind
    /// of background request is switched off.
    BlockedByPolicy(BackgroundKind),
    /// An `http:` URL was upgraded to `https:` (HTTPS-only mode) and the
    /// secure connection failed. The shell can offer the user an exception
    /// for `site` with [`Client::allow_insecure_http`](crate::Client::allow_insecure_http)
    /// and retry.
    HttpsUpgradeFailed(Box<UpgradeFailure>),
    /// The URL's scheme cannot be fetched (only `http`, `https`, `data` and
    /// `about:blank` are supported).
    UnsupportedScheme(String),
    /// The request itself is malformed (bad header name or value, a body on
    /// a GET, credentials in a redirect target, ...).
    InvalidRequest(String),
    /// A `data:` URL could not be processed.
    InvalidDataUrl,
    /// The host name could not be resolved.
    Dns(String),
    /// A TCP connection could not be established.
    Connect(String),
    /// The TLS handshake or certificate verification failed.
    Tls(String),
    /// The server's response violated HTTP.
    Protocol(String),
    /// The request did not complete within the configured time limit.
    Timeout,
    /// More redirects than the limit (20) were followed.
    TooManyRedirects,
    /// The redirect target is not an HTTP(S) URL, or `Location` is invalid.
    InvalidRedirect(String),
    /// A redirect was received and the request's redirect mode is `Error`.
    RedirectNotAllowed,
    /// The body (raw or decoded) exceeded the configured size limit.
    BodyTooLarge {
        /// The limit, in bytes.
        limit: u64,
    },
    /// The body could not be decoded (corrupt gzip, deflate or brotli data).
    Decode(String),
    /// The request was cancelled through its handle.
    Cancelled,
    /// The client has shut down or could not start its runtime.
    ClientUnavailable(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockedByPolicy(kind) => write!(f, "blocked by network policy: {kind:?}"),
            Self::HttpsUpgradeFailed(u) => {
                write!(f, "secure connection to {} failed: {}", u.url, u.reason)
            }
            Self::UnsupportedScheme(s) => write!(f, "unsupported URL scheme: {s}"),
            Self::InvalidRequest(s) => write!(f, "invalid request: {s}"),
            Self::InvalidDataUrl => f.write_str("invalid data: URL"),
            Self::Dns(s) => write!(f, "DNS lookup failed: {s}"),
            Self::Connect(s) => write!(f, "connection failed: {s}"),
            Self::Tls(s) => write!(f, "TLS error: {s}"),
            Self::Protocol(s) => write!(f, "HTTP protocol error: {s}"),
            Self::Timeout => f.write_str("request timed out"),
            Self::TooManyRedirects => f.write_str("too many redirects"),
            Self::InvalidRedirect(s) => write!(f, "invalid redirect: {s}"),
            Self::RedirectNotAllowed => f.write_str("redirect not allowed"),
            Self::BodyTooLarge { limit } => write!(f, "response body exceeds {limit} bytes"),
            Self::Decode(s) => write!(f, "content decoding failed: {s}"),
            Self::Cancelled => f.write_str("request cancelled"),
            Self::ClientUnavailable(s) => write!(f, "network client unavailable: {s}"),
        }
    }
}

impl std::error::Error for NetError {}

impl NetError {
    /// Whether this error happened while establishing a connection (DNS,
    /// TCP, TLS or a connect timeout), as opposed to after one was made.
    pub fn is_connect_error(&self) -> bool {
        matches!(
            self,
            Self::Dns(_) | Self::Connect(_) | Self::Tls(_) | Self::Timeout
        )
    }
}
