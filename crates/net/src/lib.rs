//! HTTP, TLS, cookies, cache, and the network policy gate.
//!
//! `wb-net` fetches `http:`, `https:`, `data:` and `about:blank` URLs for
//! the browser. Borrowed crates (`hyper`, `rustls`, `tokio`, ...) stay
//! private; the API uses only our own types and `wb-url`.
//!
//! - **Transport:** HTTP/1.1 and HTTP/2 (ALPN) over `hyper`, TLS by
//!   `rustls` with the `ring` provider and the platform certificate
//!   verifier. Pooled connections.
//! - **HTTPS-only:** `http:` URLs are upgraded to `https:`. If the secure
//!   connection fails the fetch returns [`NetError::HttpsUpgradeFailed`],
//!   and the shell may call [`Client::allow_insecure_http`] for that site.
//! - **Privacy:** cookies ([`CookieJar`], RFC 6265bis) are partitioned by
//!   top-level site and third-party cookies are blocked; the cache is
//!   partitioned the same way; `Referer` never carries more than the origin
//!   across sites.
//! - **Policy gate:** every request passes [`NetworkPolicy`]'s gate. Page
//!   requests pass; background requests ([`BackgroundKind`]) pass only if
//!   switched on.
//! - **Limits:** at most 20 redirects, a body size limit (raw and decoded),
//!   a connect timeout and a whole-request timeout.
//!
//! # Threading model
//!
//! A [`Client`] owns a small multi-threaded tokio runtime. Callers on any
//! thread (typically the GUI thread) start requests and get a
//! [`RequestHandle`] to poll, wait on, or cancel; or they pass a callback
//! with [`Client::start_with_callback`]. [`Client::fetch`] is the blocking
//! shorthand. No async runtime leaks into the caller.
//!
//! ```no_run
//! use wb_net::{Client, Request};
//! use wb_url::Url;
//!
//! let client = Client::new()?;
//! let handle = client.start(Request::navigate(Url::parse("https://example.org/").unwrap()));
//! // ... later, from the event loop:
//! if let Some(result) = handle.poll() {
//!     let response = result?;
//!     println!("{} {} bytes", response.status, response.body.len());
//! }
//! # Ok::<(), wb_net::NetError>(())
//! ```

#![deny(missing_docs)]

mod cache;
mod client;
mod cookies;
mod data_url;
mod date;
mod decode;
mod error;
mod fetch;
mod policy;
mod referrer;
mod transport;
mod types;

pub use client::{CancelHandle, Client, ClientConfig, NetStats, RequestHandle};
pub use cookies::{Cookie, CookieJar, CookieStore, SameSite};
pub use error::{NetError, UpgradeFailure};
pub use policy::{BackgroundKind, NetworkPolicy};
pub use referrer::ReferrerPolicy;
pub use types::{
    CacheMode, Headers, HttpVersion, Method, RedirectMode, Request, RequestContext, Response,
};
