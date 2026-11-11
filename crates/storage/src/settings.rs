//! Typed settings with defaults.
//!
//! Scalar settings live in a key/value table. A key that was never written
//! (or holds a value this build can't parse) reads as its default, so new
//! settings need no migration.

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{DbResult, Result, StorageError};
use crate::time::{from_millis, to_millis};

/// The default page zoom factor (100%).
pub const DEFAULT_ZOOM: f64 = 1.0;
/// The smallest accepted zoom factor.
pub const MIN_ZOOM: f64 = 0.25;
/// The largest accepted zoom factor.
pub const MAX_ZOOM: f64 = 5.0;

/// Keys of the scalar settings.
#[derive(Clone, Copy)]
enum Key {
    SearchEngine,
    BlockerEnabled,
    AutoUpdateFilterLists,
    DefaultZoom,
}

impl Key {
    fn as_str(self) -> &'static str {
        match self {
            Self::SearchEngine => "search.engine",
            Self::BlockerEnabled => "blocker.enabled",
            Self::AutoUpdateFilterLists => "blocker.auto_update_lists",
            Self::DefaultZoom => "view.default_zoom",
        }
    }
}

/// A per-site list of exceptions. Sites are stored as given after trimming,
/// ASCII-lowercasing and dropping one trailing dot; the caller decides what a
/// "site" is (normally the host or registrable domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SiteList {
    /// Sites the user allowed to load over plain HTTP despite HTTPS-only mode.
    HttpsOnlyExceptions,
    /// Sites on which the content blocker is turned off.
    BlockerDisabled,
}

impl SiteList {
    fn as_str(self) -> &'static str {
        match self {
            Self::HttpsOnlyExceptions => "https_only_exception",
            Self::BlockerDisabled => "blocker_disabled",
        }
    }
}

/// The stored state of one filter list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterListState {
    /// The list's identifier, chosen by the blocker (e.g. `"easylist"`).
    pub id: String,
    /// Whether the user has the list turned on.
    pub enabled: bool,
    /// When the list was last downloaded successfully, if ever.
    pub last_updated: Option<SystemTime>,
}

/// Typed access to the profile's settings. Get one from
/// [`Profile::settings`](crate::Profile::settings).
#[derive(Debug, Clone, Copy)]
pub struct Settings<'p> {
    conn: &'p Connection,
}

impl<'p> Settings<'p> {
    pub(crate) fn new(conn: &'p Connection) -> Self {
        Self { conn }
    }

    fn get_raw(&self, key: Key) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [key.as_str()],
                |row| row.get(0),
            )
            .optional()
            .db()
    }

    fn set_raw(&self, key: Key, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key.as_str(), value],
            )
            .db()?;
        Ok(())
    }

    fn remove_raw(&self, key: Key) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", [key.as_str()])
            .db()?;
        Ok(())
    }

    fn get_bool(&self, key: Key, default: bool) -> Result<bool> {
        Ok(match self.get_raw(key)?.as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => default,
        })
    }

    fn set_bool(&self, key: Key, value: bool) -> Result<()> {
        self.set_raw(key, if value { "true" } else { "false" })
    }

    /// The identifier of the chosen search engine, or `None` while the user
    /// hasn't chosen one (the default: none is preselected).
    pub fn search_engine(&self) -> Result<Option<String>> {
        Ok(self.get_raw(Key::SearchEngine)?.filter(|s| !s.is_empty()))
    }

    /// Sets the search engine identifier; `None` clears the choice.
    pub fn set_search_engine(&self, engine: Option<&str>) -> Result<()> {
        match engine.map(str::trim) {
            Some("") => Err(StorageError::InvalidValue(
                "search engine identifier is empty".to_owned(),
            )),
            Some(id) => self.set_raw(Key::SearchEngine, id),
            None => self.remove_raw(Key::SearchEngine),
        }
    }

    /// Whether the content blocker is on globally. Default: `true`.
    pub fn blocker_enabled(&self) -> Result<bool> {
        self.get_bool(Key::BlockerEnabled, true)
    }

    /// Turns the content blocker on or off globally.
    pub fn set_blocker_enabled(&self, enabled: bool) -> Result<()> {
        self.set_bool(Key::BlockerEnabled, enabled)
    }

    /// Whether filter lists are re-downloaded automatically. Default: `true`.
    pub fn auto_update_filter_lists(&self) -> Result<bool> {
        self.get_bool(Key::AutoUpdateFilterLists, true)
    }

    /// Turns automatic filter-list updates on or off.
    pub fn set_auto_update_filter_lists(&self, enabled: bool) -> Result<()> {
        self.set_bool(Key::AutoUpdateFilterLists, enabled)
    }

    /// The default page zoom factor. Default: [`DEFAULT_ZOOM`].
    pub fn default_zoom(&self) -> Result<f64> {
        Ok(self
            .get_raw(Key::DefaultZoom)?
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|z| (MIN_ZOOM..=MAX_ZOOM).contains(z))
            .unwrap_or(DEFAULT_ZOOM))
    }

    /// Sets the default page zoom factor, which must be within
    /// [`MIN_ZOOM`]`..=`[`MAX_ZOOM`].
    pub fn set_default_zoom(&self, zoom: f64) -> Result<()> {
        if !(MIN_ZOOM..=MAX_ZOOM).contains(&zoom) {
            return Err(StorageError::InvalidValue(format!(
                "zoom {zoom} is outside {MIN_ZOOM}..={MAX_ZOOM}"
            )));
        }
        self.set_raw(Key::DefaultZoom, &zoom.to_string())
    }

    /// The stored state of filter list `id`, or `None` if it was never
    /// configured (for example before the first-run download is confirmed).
    pub fn filter_list(&self, id: &str) -> Result<Option<FilterListState>> {
        self.conn
            .query_row(
                "SELECT id, enabled, last_updated_ms FROM filter_lists WHERE id = ?1",
                [id],
                filter_list_from_row,
            )
            .optional()
            .db()
    }

    /// Every configured filter list, ordered by id.
    pub fn filter_lists(&self) -> Result<Vec<FilterListState>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, enabled, last_updated_ms FROM filter_lists ORDER BY id")
            .db()?;
        let rows = stmt.query_map([], filter_list_from_row).db()?;
        rows.collect::<std::result::Result<_, _>>().db()
    }

    /// Turns filter list `id` on or off, creating its entry if needed.
    pub fn set_filter_list_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        check_list_id(id)?;
        self.conn
            .execute(
                "INSERT INTO filter_lists (id, enabled, last_updated_ms) VALUES (?1, ?2, NULL)
                 ON CONFLICT (id) DO UPDATE SET enabled = excluded.enabled",
                params![id, enabled],
            )
            .db()?;
        Ok(())
    }

    /// Records that filter list `id` was downloaded at `when`. A list that
    /// had no entry yet is created enabled.
    pub fn set_filter_list_updated(&self, id: &str, when: SystemTime) -> Result<()> {
        check_list_id(id)?;
        self.conn
            .execute(
                "INSERT INTO filter_lists (id, enabled, last_updated_ms) VALUES (?1, 1, ?2)
                 ON CONFLICT (id) DO UPDATE SET last_updated_ms = excluded.last_updated_ms",
                params![id, to_millis(when)],
            )
            .db()?;
        Ok(())
    }

    /// Forgets filter list `id`. Returns whether it existed.
    pub fn remove_filter_list(&self, id: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM filter_lists WHERE id = ?1", [id])
            .db()?;
        Ok(n > 0)
    }

    /// Every site on `list`, sorted.
    pub fn site_list(&self, list: SiteList) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT site FROM site_lists WHERE list = ?1 ORDER BY site")
            .db()?;
        let rows = stmt.query_map([list.as_str()], |row| row.get(0)).db()?;
        rows.collect::<std::result::Result<_, _>>().db()
    }

    /// Whether `site` is on `list`.
    pub fn site_list_contains(&self, list: SiteList, site: &str) -> Result<bool> {
        let Some(site) = normalize_site(site) else {
            return Ok(false);
        };
        self.conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM site_lists WHERE list = ?1 AND site = ?2)",
                params![list.as_str(), site],
                |row| row.get(0),
            )
            .db()
    }

    /// Adds `site` to `list`. Returns `true` if it wasn't there already.
    pub fn add_to_site_list(&self, list: SiteList, site: &str) -> Result<bool> {
        let site = normalize_site(site)
            .ok_or_else(|| StorageError::InvalidValue("site is empty".to_owned()))?;
        let n = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO site_lists (list, site) VALUES (?1, ?2)",
                params![list.as_str(), site],
            )
            .db()?;
        Ok(n > 0)
    }

    /// Removes `site` from `list`. Returns `true` if it was there.
    pub fn remove_from_site_list(&self, list: SiteList, site: &str) -> Result<bool> {
        let Some(site) = normalize_site(site) else {
            return Ok(false);
        };
        let n = self
            .conn
            .execute(
                "DELETE FROM site_lists WHERE list = ?1 AND site = ?2",
                params![list.as_str(), site],
            )
            .db()?;
        Ok(n > 0)
    }
}

fn filter_list_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FilterListState> {
    Ok(FilterListState {
        id: row.get(0)?,
        enabled: row.get(1)?,
        last_updated: row.get::<_, Option<i64>>(2)?.map(from_millis),
    })
}

fn check_list_id(id: &str) -> Result<()> {
    if id.trim().is_empty() {
        Err(StorageError::InvalidValue(
            "filter list id is empty".to_owned(),
        ))
    } else {
        Ok(())
    }
}

/// Trims, ASCII-lowercases and drops one trailing dot; `None` if empty.
fn normalize_site(site: &str) -> Option<String> {
    let site = site.trim();
    let site = site.strip_suffix('.').unwrap_or(site);
    (!site.is_empty()).then(|| site.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use crate::{DEFAULT_ZOOM, FilterListState, Profile, SiteList, StorageError};

    #[test]
    fn defaults() {
        let p = Profile::open_in_memory().unwrap();
        let s = p.settings();
        assert_eq!(s.search_engine().unwrap(), None);
        assert!(s.blocker_enabled().unwrap());
        assert!(s.auto_update_filter_lists().unwrap());
        assert_eq!(s.default_zoom().unwrap(), DEFAULT_ZOOM);
        assert!(s.filter_lists().unwrap().is_empty());
        assert_eq!(s.filter_list("easylist").unwrap(), None);
        assert!(
            s.site_list(SiteList::HttpsOnlyExceptions)
                .unwrap()
                .is_empty()
        );
        assert!(s.site_list(SiteList::BlockerDisabled).unwrap().is_empty());
    }

    #[test]
    fn scalar_round_trips() {
        let p = Profile::open_in_memory().unwrap();
        let s = p.settings();
        s.set_search_engine(Some("duckduckgo")).unwrap();
        assert_eq!(s.search_engine().unwrap().as_deref(), Some("duckduckgo"));
        s.set_search_engine(Some("mojeek")).unwrap();
        assert_eq!(s.search_engine().unwrap().as_deref(), Some("mojeek"));
        s.set_search_engine(None).unwrap();
        assert_eq!(s.search_engine().unwrap(), None);
        assert!(matches!(
            s.set_search_engine(Some("  ")),
            Err(StorageError::InvalidValue(_))
        ));

        s.set_blocker_enabled(false).unwrap();
        assert!(!s.blocker_enabled().unwrap());
        s.set_blocker_enabled(true).unwrap();
        assert!(s.blocker_enabled().unwrap());

        s.set_auto_update_filter_lists(false).unwrap();
        assert!(!s.auto_update_filter_lists().unwrap());

        s.set_default_zoom(1.25).unwrap();
        assert_eq!(s.default_zoom().unwrap(), 1.25);
    }

    #[test]
    fn zoom_is_validated() {
        let p = Profile::open_in_memory().unwrap();
        let s = p.settings();
        for bad in [0.0, -1.0, 0.1, 10.0, f64::NAN, f64::INFINITY] {
            assert!(
                matches!(s.set_default_zoom(bad), Err(StorageError::InvalidValue(_))),
                "{bad}"
            );
        }
        assert_eq!(s.default_zoom().unwrap(), DEFAULT_ZOOM);
    }

    #[test]
    fn corrupt_values_read_as_defaults() {
        let p = Profile::open_in_memory().unwrap();
        p.conn_for_tests()
            .execute_batch(
                "INSERT INTO settings VALUES ('blocker.enabled', 'maybe');
                 INSERT INTO settings VALUES ('view.default_zoom', 'big');",
            )
            .unwrap();
        assert!(p.settings().blocker_enabled().unwrap());
        assert_eq!(p.settings().default_zoom().unwrap(), DEFAULT_ZOOM);
    }

    #[test]
    fn filter_lists() {
        let p = Profile::open_in_memory().unwrap();
        let s = p.settings();
        s.set_filter_list_enabled("easylist", true).unwrap();
        s.set_filter_list_enabled("easyprivacy", false).unwrap();
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        s.set_filter_list_updated("easylist", t).unwrap();
        assert_eq!(
            s.filter_lists().unwrap(),
            vec![
                FilterListState {
                    id: "easylist".into(),
                    enabled: true,
                    last_updated: Some(t),
                },
                FilterListState {
                    id: "easyprivacy".into(),
                    enabled: false,
                    last_updated: None,
                },
            ]
        );
        // Toggling keeps the update time.
        s.set_filter_list_enabled("easylist", false).unwrap();
        let el = s.filter_list("easylist").unwrap().unwrap();
        assert!(!el.enabled);
        assert_eq!(el.last_updated, Some(t));
        // Updating a new list creates it enabled.
        s.set_filter_list_updated("custom", t).unwrap();
        assert!(s.filter_list("custom").unwrap().unwrap().enabled);
        assert!(s.remove_filter_list("custom").unwrap());
        assert!(!s.remove_filter_list("custom").unwrap());
        assert!(s.set_filter_list_enabled("", true).is_err());
    }

    #[test]
    fn site_lists_are_separate_and_normalized() {
        let p = Profile::open_in_memory().unwrap();
        let s = p.settings();
        assert!(
            s.add_to_site_list(SiteList::HttpsOnlyExceptions, "Example.COM.")
                .unwrap()
        );
        assert!(
            !s.add_to_site_list(SiteList::HttpsOnlyExceptions, "example.com")
                .unwrap()
        );
        s.add_to_site_list(SiteList::HttpsOnlyExceptions, "a.example")
            .unwrap();
        s.add_to_site_list(SiteList::BlockerDisabled, "news.example")
            .unwrap();

        assert_eq!(
            s.site_list(SiteList::HttpsOnlyExceptions).unwrap(),
            vec!["a.example".to_owned(), "example.com".to_owned()]
        );
        assert!(
            s.site_list_contains(SiteList::HttpsOnlyExceptions, " EXAMPLE.com ")
                .unwrap()
        );
        assert!(
            !s.site_list_contains(SiteList::BlockerDisabled, "example.com")
                .unwrap()
        );
        assert!(
            s.site_list_contains(SiteList::BlockerDisabled, "news.example")
                .unwrap()
        );
        assert!(
            s.remove_from_site_list(SiteList::HttpsOnlyExceptions, "example.com")
                .unwrap()
        );
        assert!(
            !s.remove_from_site_list(SiteList::HttpsOnlyExceptions, "example.com")
                .unwrap()
        );
        assert!(s.add_to_site_list(SiteList::BlockerDisabled, " ").is_err());
        assert!(!s.site_list_contains(SiteList::BlockerDisabled, "").unwrap());
    }
}
