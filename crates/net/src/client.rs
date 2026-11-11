//! The public client: owns a small tokio runtime and hands out request
//! handles that a GUI thread can poll, wait on, or get a callback from.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tokio::runtime::Runtime;
use tokio::task::AbortHandle;
use wb_url::Site;

use crate::cache::MemoryCache;
use crate::cookies::{CookieJar, CookieStore};
use crate::error::NetError;
use crate::fetch::{MAX_REDIRECTS, Shared, fetch};
use crate::policy::{BackgroundKind, NetworkPolicy, PolicyGate};
use crate::transport::{Connector, Counters, http_client, tls_config};
use crate::types::{Request, Response};

/// Client settings. `ClientConfig::default()` is what the browser uses.
#[derive(Clone)]
pub struct ClientConfig {
    /// The `User-Agent` header.
    pub user_agent: String,
    /// Time limit for DNS + TCP + TLS on one connection.
    pub connect_timeout: Duration,
    /// Time limit for a whole fetch, redirects and body included.
    pub request_timeout: Duration,
    /// Maximum body size in bytes, both as received and after decoding.
    pub max_body_bytes: u64,
    /// Maximum redirects to follow; values above 20 are treated as 20.
    pub max_redirects: u32,
    /// Offer HTTP/2 via ALPN (HTTP/1.1 is always available).
    pub http2: bool,
    /// Total memory cache size in bytes (0 disables caching).
    pub cache_capacity_bytes: usize,
    /// Largest single response the cache keeps, in bytes.
    pub cache_max_entry_bytes: usize,
    /// Which background requests are allowed initially.
    pub policy: NetworkPolicy,
    /// Extra trusted root certificates (DER), on top of the platform's.
    /// For enterprise roots and for tests against a local server.
    pub extra_root_certificates: Vec<Vec<u8>>,
    /// Persistent cookie storage; `None` keeps cookies in memory only.
    pub cookie_store: Option<Arc<dyn CookieStore>>,
    /// Worker threads for the internal runtime.
    pub worker_threads: usize,
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("user_agent", &self.user_agent)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("max_redirects", &self.max_redirects)
            .field("http2", &self.http2)
            .field("cache_capacity_bytes", &self.cache_capacity_bytes)
            .field("cache_max_entry_bytes", &self.cache_max_entry_bytes)
            .field("policy", &self.policy)
            .field(
                "extra_root_certificates",
                &self.extra_root_certificates.len(),
            )
            .field("cookie_store", &self.cookie_store.is_some())
            .field("worker_threads", &self.worker_threads)
            .finish()
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            user_agent: format!("Mozilla/5.0 wb/{}", env!("CARGO_PKG_VERSION")),
            connect_timeout: Duration::from_secs(15),
            request_timeout: Duration::from_secs(60),
            max_body_bytes: 64 * 1024 * 1024,
            max_redirects: MAX_REDIRECTS,
            http2: true,
            cache_capacity_bytes: 64 * 1024 * 1024,
            cache_max_entry_bytes: 8 * 1024 * 1024,
            policy: NetworkPolicy::default(),
            extra_root_certificates: Vec::new(),
            cookie_store: None,
            worker_threads: 2,
        }
    }
}

/// Counters of network activity, for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetStats {
    /// DNS lookups started.
    pub dns_lookups: u64,
    /// TCP connections attempted.
    pub connection_attempts: u64,
    /// HTTP requests sent (cache hits and `data:` URLs don't count).
    pub requests_sent: u64,
}

/// Owns the runtime and shuts it down without blocking when the last
/// client clone goes away (safe even from inside another runtime).
struct RuntimeHolder(Option<Runtime>);

impl Drop for RuntimeHolder {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            rt.shutdown_background();
        }
    }
}

/// The network client. Cheap to clone; clones share the runtime, the cookie
/// jar, the cache, the HTTPS exceptions and the policy.
///
/// Creating a client makes no network requests: nothing is resolved or
/// connected until a request is started.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
    runtime: Arc<RuntimeHolder>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("policy", &self.shared.gate.snapshot())
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// A started request. Poll it from a GUI loop with [`RequestHandle::poll`],
/// block on it with [`RequestHandle::wait`], or cancel it. Dropping the
/// handle does not cancel the request (its result is discarded).
#[derive(Debug)]
pub struct RequestHandle {
    rx: mpsc::Receiver<Result<Response, NetError>>,
    abort: Option<AbortHandle>,
}

impl RequestHandle {
    /// Returns the result if the request has finished, without blocking.
    /// Returns `Some` at most once.
    pub fn poll(&self) -> Option<Result<Response, NetError>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => None,
        }
    }

    /// Blocks until the request finishes.
    pub fn wait(self) -> Result<Response, NetError> {
        self.rx.recv().unwrap_or(Err(NetError::Cancelled))
    }

    /// Blocks for at most `timeout`; `None` if the request is still running.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<Result<Response, NetError>> {
        match self.rx.recv_timeout(timeout) {
            Ok(r) => Some(r),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err(NetError::Cancelled)),
        }
    }

    /// Cancels the request. A later `wait` returns [`NetError::Cancelled`].
    pub fn cancel(&self) {
        if let Some(a) = &self.abort {
            a.abort();
        }
    }
}

/// Cancels a request started with [`Client::start_with_callback`].
#[derive(Debug, Clone)]
pub struct CancelHandle {
    abort: Option<AbortHandle>,
}

impl CancelHandle {
    /// Cancels the request; its callback then receives [`NetError::Cancelled`].
    pub fn cancel(&self) {
        if let Some(a) = &self.abort {
            a.abort();
        }
    }
}

impl Client {
    /// A client with the default configuration.
    pub fn new() -> Result<Client, NetError> {
        Client::with_config(ClientConfig::default())
    }

    /// A client with the given configuration.
    pub fn with_config(config: ClientConfig) -> Result<Client, NetError> {
        // Everything fallible happens before the runtime exists: dropping a
        // bare runtime from inside an async context panics.
        let tls = tls_config(&config.extra_root_certificates, config.http2)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(config.worker_threads.max(1))
            // Content decoding runs on the blocking pool; keep it small.
            .max_blocking_threads(4)
            .thread_name("wb-net")
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| NetError::ClientUnavailable(e.to_string()))?;
        // From here on nothing can fail; the runtime ends up in a
        // `RuntimeHolder`, whose drop never blocks.
        let counters = Arc::new(Counters::default());
        let connector = Connector::new(tls, config.connect_timeout, counters.clone());
        let http = {
            let _guard = runtime.enter();
            http_client(connector)
        };
        let cookies = match &config.cookie_store {
            Some(store) => CookieJar::with_store(store.clone()),
            None => CookieJar::new(),
        };
        let shared = Arc::new(Shared {
            gate: PolicyGate::new(config.policy),
            insecure_hosts: Mutex::new(HashSet::new()),
            cookies,
            cache: MemoryCache::new(config.cache_capacity_bytes, config.cache_max_entry_bytes),
            http,
            counters,
            user_agent: config.user_agent,
            request_timeout: config.request_timeout,
            max_body_bytes: config.max_body_bytes,
            max_redirects: config.max_redirects.min(MAX_REDIRECTS),
        });
        Ok(Client {
            shared,
            runtime: Arc::new(RuntimeHolder(Some(runtime))),
        })
    }

    fn runtime(&self) -> Result<&Runtime, NetError> {
        self.runtime
            .0
            .as_ref()
            .ok_or_else(|| NetError::ClientUnavailable("runtime stopped".into()))
    }

    /// Spawns the fetch and a watcher that reports its outcome to `report`.
    fn spawn<F>(&self, request: Request, report: F) -> Result<AbortHandle, F>
    where
        F: FnOnce(Result<Response, NetError>) + Send + 'static,
    {
        let Ok(rt) = self.runtime() else {
            return Err(report);
        };
        let task = rt.spawn(fetch(self.shared.clone(), request));
        let abort = task.abort_handle();
        rt.spawn(async move {
            let result = match task.await {
                Ok(r) => r,
                Err(e) if e.is_cancelled() => Err(NetError::Cancelled),
                Err(e) => Err(NetError::ClientUnavailable(e.to_string())),
            };
            report(result);
        });
        Ok(abort)
    }

    /// Starts a request and returns a handle to poll or wait on.
    pub fn start(&self, request: Request) -> RequestHandle {
        let (tx, rx) = mpsc::channel();
        let send = move |r| {
            // The receiver may be gone; then nobody wants the result.
            let _ = tx.send(r);
        };
        match self.spawn(request, send) {
            Ok(abort) => RequestHandle {
                rx,
                abort: Some(abort),
            },
            Err(send) => {
                send(Err(NetError::ClientUnavailable("runtime stopped".into())));
                RequestHandle { rx, abort: None }
            }
        }
    }

    /// Starts a request and calls `callback` with the result. The callback
    /// runs on a network thread: keep it short, e.g. post the result to the
    /// GUI event loop. It is called exactly once, including on cancellation,
    /// unless the client is dropped first.
    pub fn start_with_callback<F>(&self, request: Request, callback: F) -> CancelHandle
    where
        F: FnOnce(Result<Response, NetError>) + Send + 'static,
    {
        match self.spawn(request, callback) {
            Ok(abort) => CancelHandle { abort: Some(abort) },
            Err(callback) => {
                callback(Err(NetError::ClientUnavailable("runtime stopped".into())));
                CancelHandle { abort: None }
            }
        }
    }

    /// Fetches and blocks the calling thread until done. Must not be called
    /// from async code (it returns [`NetError::ClientUnavailable`] there).
    pub fn fetch(&self, request: Request) -> Result<Response, NetError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(NetError::ClientUnavailable(
                "blocking fetch called from an async context; use start()".into(),
            ));
        }
        self.start(request).wait()
    }

    /// Allows plain `http:` for `site` (the user accepted the risk after an
    /// [`NetError::HttpsUpgradeFailed`]). Lasts for this client's lifetime.
    pub fn allow_insecure_http(&self, site: &Site) {
        if let Some(host) = site.host() {
            self.lock_insecure().insert(host.clone());
        }
    }

    /// Removes an exception added by [`Client::allow_insecure_http`].
    pub fn revoke_insecure_http(&self, site: &Site) {
        if let Some(host) = site.host() {
            self.lock_insecure().remove(host);
        }
    }

    /// Whether plain `http:` is allowed for `site`.
    pub fn is_insecure_http_allowed(&self, site: &Site) -> bool {
        site.host()
            .is_some_and(|h| self.lock_insecure().contains(h))
    }

    fn lock_insecure(&self) -> std::sync::MutexGuard<'_, HashSet<wb_url::Host>> {
        self.shared
            .insecure_hosts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Switches one kind of background request on or off.
    pub fn set_background_allowed(&self, kind: BackgroundKind, allowed: bool) {
        self.shared.gate.set(kind, allowed);
    }

    /// The current background-request policy.
    pub fn policy(&self) -> NetworkPolicy {
        self.shared.gate.snapshot()
    }

    /// The cookie jar (for `document.cookie` and the shell's cookie UI).
    pub fn cookies(&self) -> &CookieJar {
        &self.shared.cookies
    }

    /// Empties the memory cache.
    pub fn clear_cache(&self) {
        self.shared.cache.clear();
    }

    /// Network activity so far.
    pub fn stats(&self) -> NetStats {
        let c = &self.shared.counters;
        NetStats {
            dns_lookups: c.dns_lookups.load(Ordering::Relaxed),
            connection_attempts: c.connections.load(Ordering::Relaxed),
            requests_sent: c.requests.load(Ordering::Relaxed),
        }
    }
}
