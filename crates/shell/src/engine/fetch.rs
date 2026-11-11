//! Where the engine gets bytes from: [`Fetcher`], implemented by
//! [`wb_net::Client`] for real browsing and by [`LocalFetcher`] for tests
//! and reference rendering.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use wb_net::{
    Client, ClientConfig, Headers, HttpVersion, NetError, NetworkPolicy, Request, Response,
};
use wb_url::Url;

/// How many requests [`Client`]'s [`Fetcher::fetch_all`] keeps in flight.
const MAX_IN_FLIGHT: usize = 16;

/// Fetches resources for the engine.
///
/// The engine calls [`Fetcher::fetch_all`] once per batch of subresources
/// (stylesheets of one `@import` level, images) and [`Fetcher::fetch`]
/// for documents. Implementations must be usable from any thread.
pub trait Fetcher: Send + Sync {
    /// Fetches one request.
    ///
    /// # Errors
    ///
    /// Any network or policy error; the engine treats an error like a
    /// missing resource (or, for a document, fails the load).
    fn fetch(&self, request: Request) -> Result<Response, NetError>;

    /// Fetches several requests, possibly concurrently, and returns one
    /// result per request in the same order. `cancelled` is polled while
    /// waiting; once it returns `true` the remaining requests may fail
    /// with [`NetError::Cancelled`]. The default fetches one by one.
    fn fetch_all(
        &self,
        requests: Vec<Request>,
        cancelled: &dyn Fn() -> bool,
    ) -> Vec<Result<Response, NetError>> {
        requests
            .into_iter()
            .map(|r| {
                if cancelled() {
                    Err(NetError::Cancelled)
                } else {
                    self.fetch(r)
                }
            })
            .collect()
    }
}

impl Fetcher for Client {
    fn fetch(&self, request: Request) -> Result<Response, NetError> {
        Client::fetch(self, request)
    }

    fn fetch_all(
        &self,
        requests: Vec<Request>,
        cancelled: &dyn Fn() -> bool,
    ) -> Vec<Result<Response, NetError>> {
        let mut out = Vec::with_capacity(requests.len());
        let mut pending = requests.into_iter().peekable();
        while pending.peek().is_some() {
            let handles: Vec<_> = pending
                .by_ref()
                .take(MAX_IN_FLIGHT)
                .map(|r| self.start(r))
                .collect();
            for h in handles {
                let result = loop {
                    if cancelled() {
                        h.cancel();
                        break Err(NetError::Cancelled);
                    }
                    if let Some(r) = h.wait_timeout(Duration::from_millis(25)) {
                        break r;
                    }
                };
                out.push(result);
            }
        }
        out
    }
}

/// A fetcher that never touches the network: it serves resources added
/// with [`LocalFetcher::insert`], `file:` URLs under an optional root
/// directory, and `data:` and `about:blank` URLs.
///
/// Everything else fails with [`NetError::UnsupportedScheme`] (for other
/// schemes) or a 404 response (for unknown `http(s)` URLs), so a page
/// rendered with it can't reach the network.
pub struct LocalFetcher {
    entries: HashMap<String, (String, Vec<u8>)>,
    root: Option<PathBuf>,
    /// A client with every background request switched off, used only for
    /// `data:` and `about:blank`, which it handles without any socket.
    data: OnceLock<Option<Client>>,
}

impl fmt::Debug for LocalFetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalFetcher")
            .field("entries", &self.entries.len())
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Default for LocalFetcher {
    fn default() -> Self {
        LocalFetcher::new()
    }
}

impl LocalFetcher {
    /// A fetcher with no resources and no file access.
    #[must_use]
    pub fn new() -> LocalFetcher {
        LocalFetcher {
            entries: HashMap::new(),
            root: None,
            data: OnceLock::new(),
        }
    }

    /// Also serves `file:` URLs for files inside `root` (after resolving
    /// symlinks). Files outside it are not found.
    #[must_use]
    pub fn with_file_root(mut self, root: impl Into<PathBuf>) -> LocalFetcher {
        let root = root.into();
        self.root = Some(root.canonicalize().unwrap_or(root));
        self
    }

    /// Serves `body` with `content_type` at `url` (compared after URL
    /// parsing, without the fragment). Replaces an earlier entry.
    pub fn insert(&mut self, url: &str, content_type: &str, body: impl Into<Vec<u8>>) {
        let key = Url::parse(url)
            .map(|u| u.without_fragment().as_str().to_string())
            .unwrap_or_else(|_| url.to_string());
        self.entries
            .insert(key, (content_type.to_string(), body.into()));
    }

    fn file(&self, url: &Url) -> Option<(String, Vec<u8>)> {
        let root = self.root.as_ref()?;
        let path = file_url_path(url)?;
        let path = path.canonicalize().ok()?;
        if !path.starts_with(root) || !path.is_file() {
            return None;
        }
        let body = std::fs::read(&path).ok()?;
        Some((content_type_for_path(&path).to_string(), body))
    }

    fn data_client(&self) -> Option<&Client> {
        self.data
            .get_or_init(|| {
                Client::with_config(ClientConfig {
                    policy: NetworkPolicy::none(),
                    worker_threads: 1,
                    cache_capacity_bytes: 0,
                    ..ClientConfig::default()
                })
                .ok()
            })
            .as_ref()
    }
}

impl Fetcher for LocalFetcher {
    fn fetch(&self, request: Request) -> Result<Response, NetError> {
        let url = request.url.clone();
        let key = url.without_fragment();
        let found = match self.entries.get(key.as_str()) {
            Some((ct, body)) => Some((ct.clone(), body.clone())),
            None if url.scheme() == "file" => self.file(&url),
            None => None,
        };
        if let Some((content_type, body)) = found {
            return Ok(local_response(url, 200, &content_type, body));
        }
        match url.scheme() {
            "data" | "about" => match self.data_client() {
                Some(c) => c.fetch(request),
                None => Err(NetError::ClientUnavailable("no data: client".into())),
            },
            "http" | "https" | "file" => Ok(local_response(url, 404, "text/plain", Vec::new())),
            other => Err(NetError::UnsupportedScheme(other.to_string())),
        }
    }
}

/// A response made up locally.
fn local_response(url: Url, status: u16, content_type: &str, body: Vec<u8>) -> Response {
    let mut headers = Headers::new();
    headers.set("content-type", content_type);
    Response {
        url_list: vec![url.clone()],
        url,
        status,
        headers,
        body,
        version: HttpVersion::None,
        from_cache: false,
    }
}

/// The local path of a `file:` URL (percent-decoded), or `None`.
fn file_url_path(url: &Url) -> Option<PathBuf> {
    if url.scheme() != "file" {
        return None;
    }
    match url.host_str() {
        None | Some("") | Some("localhost") => {}
        Some(_) => return None,
    }
    let bytes = percent_decode(url.path());
    let s = String::from_utf8(bytes).ok()?;
    Some(PathBuf::from(s))
}

/// Percent-decodes `s` (URL Standard §1.3 "percent-decode").
pub(crate) fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&x| hex(x)),
                b.get(i + 2).and_then(|&x| hex(x)),
            )
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn hex(c: u8) -> Option<u8> {
    char::from(c)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

/// A `Content-Type` for a local file, from its extension.
fn content_type_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" | "xht" | "xhtml" => "text/html",
        "css" => "text/css",
        "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "js" => "text/javascript",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_inserted_and_404s_the_rest() {
        let mut f = LocalFetcher::new();
        f.insert("https://a.test/x.css", "text/css", "p{}");
        let r = f
            .fetch(Request::navigate(
                Url::parse("https://a.test/x.css#frag").unwrap(),
            ))
            .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"p{}");
        assert_eq!(r.content_type(), Some("text/css"));
        let r = f
            .fetch(Request::navigate(Url::parse("https://a.test/y").unwrap()))
            .unwrap();
        assert_eq!(r.status, 404);
    }

    #[test]
    fn data_urls() {
        let f = LocalFetcher::new();
        let r = f
            .fetch(Request::navigate(
                Url::parse("data:text/plain,hi%21").unwrap(),
            ))
            .unwrap();
        assert_eq!(r.body, b"hi!");
    }

    #[test]
    fn files_only_under_root() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("in");
        std::fs::create_dir(&inner).unwrap();
        std::fs::write(inner.join("a b.html"), "<p>x").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "no").unwrap();
        let f = LocalFetcher::new().with_file_root(&inner);
        let base = format!("file://{}/", inner.canonicalize().unwrap().display());
        let u = Url::parse(&format!("{base}a%20b.html")).unwrap();
        let r = f.fetch(Request::navigate(u)).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"<p>x"[..]));
        assert_eq!(r.content_type(), Some("text/html"));
        let u = Url::parse(&format!("{base}../secret.txt")).unwrap();
        assert_eq!(f.fetch(Request::navigate(u)).unwrap().status, 404);
        let u = Url::parse(&format!("{base}%2E%2E/secret.txt")).unwrap();
        assert_eq!(f.fetch(Request::navigate(u)).unwrap().status, 404);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b%zz%4"), b"a b%zz%4");
    }
}
