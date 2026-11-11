//! Content-blocker filter lists: the catalog, downloading them through the
//! network policy gate, caching them in the profile directory, and
//! compiling the enabled ones into a `wb-blocker` engine.
//!
//! Downloads are background requests ([`BackgroundKind::FilterListDownload`])
//! and only happen after the user confirmed them on the first-run screen
//! or in settings; the gate in `wb-net` refuses them otherwise.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use wb_net::{BackgroundKind, NetError, Request};
use wb_url::Url;

use crate::engine::Fetcher;

/// The largest filter list accepted (bytes).
const MAX_LIST_BYTES: usize = 32 * 1024 * 1024;

/// How old a downloaded list may get before an automatic update.
pub const UPDATE_INTERVAL: Duration = Duration::from_secs(4 * 24 * 60 * 60);

/// A filter list the browser offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FilterList {
    /// The identifier stored in the profile and used as the file name.
    pub id: &'static str,
    /// The display name.
    pub name: &'static str,
    /// Where it is downloaded from.
    pub url: &'static str,
}

/// The lists offered. They are downloaded at run time, never bundled.
pub const FILTER_LISTS: [FilterList; 2] = [
    FilterList {
        id: "easylist",
        name: "EasyList (ads)",
        url: "https://easylist.to/easylist/easylist.txt",
    },
    FilterList {
        id: "easyprivacy",
        name: "EasyPrivacy (trackers)",
        url: "https://easylist.to/easylist/easyprivacy.txt",
    },
];

/// Why a download failed.
#[derive(Debug)]
pub enum FilterError {
    /// The network refused or failed (including the policy gate).
    Net(NetError),
    /// The server answered with a non-OK status.
    Status(u16),
    /// The body is too big or not text.
    Invalid,
    /// Writing the cache file failed.
    Io(std::io::Error),
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilterError::Net(e) => write!(f, "{e}"),
            FilterError::Status(s) => write!(f, "HTTP status {s}"),
            FilterError::Invalid => f.write_str("not a filter list"),
            FilterError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FilterError {}

/// Where list `id` is cached under the profile directory.
#[must_use]
pub fn list_path(profile_dir: &Path, id: &str) -> PathBuf {
    profile_dir.join("filter-lists").join(format!("{id}.txt"))
}

/// Downloads `list` as a background request and returns its text.
///
/// # Errors
///
/// See [`FilterError`].
pub fn download(fetcher: &dyn Fetcher, list: &FilterList) -> Result<String, FilterError> {
    let url = Url::parse(list.url).map_err(|_| FilterError::Invalid)?;
    let resp = fetcher
        .fetch(Request::background(url, BackgroundKind::FilterListDownload))
        .map_err(FilterError::Net)?;
    if !resp.ok() {
        return Err(FilterError::Status(resp.status));
    }
    if resp.body.len() > MAX_LIST_BYTES {
        return Err(FilterError::Invalid);
    }
    String::from_utf8(resp.body).map_err(|_| FilterError::Invalid)
}

/// Saves a downloaded list's text in the profile directory.
///
/// # Errors
///
/// An I/O error.
pub fn save(profile_dir: &Path, id: &str, text: &str) -> Result<(), FilterError> {
    let path = list_path(profile_dir, id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(FilterError::Io)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(FilterError::Io)?;
    std::fs::rename(&tmp, &path).map_err(FilterError::Io)
}

/// Whether a list last updated at `last` is due for an update at `now`.
#[must_use]
pub fn is_due(last: Option<SystemTime>, now: SystemTime) -> bool {
    match last {
        None => true,
        Some(t) => now.duration_since(t).is_ok_and(|d| d >= UPDATE_INTERVAL),
    }
}

/// Compiles the cached text of the lists in `enabled` into a blocker
/// engine. Lists without a cached file are skipped. Returns `None` when no
/// list could be read.
#[must_use]
pub fn compile(profile_dir: &Path, enabled: &[&str]) -> Option<wb_blocker::Engine> {
    let mut builder = wb_blocker::EngineBuilder::new();
    let mut any = false;
    for id in enabled {
        if let Ok(text) = std::fs::read_to_string(list_path(profile_dir, id)) {
            let _ = builder.add_list(&text);
            any = true;
        }
    }
    any.then(|| builder.build())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wb_net::{Client, ClientConfig, NetworkPolicy};

    use super::*;
    use crate::engine::LocalFetcher;

    #[test]
    fn policy_gate_blocks_unconfirmed_downloads() {
        let client = Client::with_config(ClientConfig {
            policy: NetworkPolicy::none(),
            ..ClientConfig::default()
        })
        .unwrap();
        let err = download(&client, &FILTER_LISTS[0]).unwrap_err();
        assert!(
            matches!(err, FilterError::Net(NetError::BlockedByPolicy(_))),
            "{err:?}"
        );
        assert_eq!(client.stats(), wb_net::NetStats::default());
    }

    #[test]
    fn download_save_compile() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = LocalFetcher::new();
        f.insert(FILTER_LISTS[0].url, "text/plain", "||ads.test^\n##.ad");
        let f: Arc<dyn Fetcher> = Arc::new(f);
        let text = download(&*f, &FILTER_LISTS[0]).unwrap();
        save(dir.path(), "easylist", &text).unwrap();
        assert!(matches!(
            download(&*f, &FILTER_LISTS[1]),
            Err(FilterError::Status(404))
        ));
        assert!(compile(dir.path(), &["easyprivacy"]).is_none());
        let engine = compile(dir.path(), &["easylist", "easyprivacy"]).unwrap();
        assert_eq!(engine.network_filter_count(), 1);
        assert_eq!(engine.cosmetic_filter_count(), 1);
    }

    #[test]
    fn update_due() {
        let now = SystemTime::now();
        assert!(is_due(None, now));
        assert!(!is_due(Some(now), now));
        assert!(is_due(Some(now - UPDATE_INTERVAL), now));
    }
}
