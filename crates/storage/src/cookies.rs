//! Persistent cookie storage.
//!
//! This is the on-disk half of the cookie jar. The jar's logic (parsing
//! `Set-Cookie`, matching, eviction, third-party blocking) lives in `wb-net`,
//! which converts to and from [`StoredCookie`]. Storage only persists what it
//! is given, with two rules of its own:
//!
//! - Session cookies (no expiry time, `persistent-flag` false in RFC 6265bis
//!   §5.7) are never written to disk; saving one removes any persistent
//!   cookie it replaces.
//! - Expired cookies are never returned, and saving one removes the stored
//!   cookie it replaces (the RFC's way to delete a cookie).

use std::time::SystemTime;

use rusqlite::{Connection, params};

use crate::error::{DbResult, Result};
use crate::time::{from_millis, to_millis};

/// The `SameSite` enforcement mode (RFC 6265bis §5.6.7 and the `same-site-flag`
/// of §5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SameSite {
    /// `SameSite=Strict`.
    Strict,
    /// `SameSite=Lax`.
    Lax,
    /// `SameSite=None`.
    None,
    /// No (valid) `SameSite` attribute was given: the "Default" enforcement
    /// mode.
    #[default]
    Default,
}

impl SameSite {
    fn to_db(self) -> i64 {
        match self {
            Self::Default => 0,
            Self::Strict => 1,
            Self::Lax => 2,
            Self::None => 3,
        }
    }

    fn from_db(v: i64) -> Self {
        match v {
            1 => Self::Strict,
            2 => Self::Lax,
            3 => Self::None,
            _ => Self::Default,
        }
    }
}

/// A cookie as stored, with the fields of the RFC 6265bis §5.7 storage model
/// plus the partition key.
///
/// A stored cookie's identity is (`partition_key`, `name`, `domain`,
/// `host_only`, `path`): saving a cookie with the same identity replaces the
/// old one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookie {
    /// The partition the cookie belongs to: the top-level site it was set
    /// under. Opaque to storage and compared exactly; `wb-net` chooses the
    /// serialization (e.g. `https://example.com`).
    pub partition_key: String,
    /// The cookie name.
    pub name: String,
    /// The cookie value.
    pub value: String,
    /// The `domain` field (canonicalized host, without a leading dot).
    pub domain: String,
    /// The `path` field.
    pub path: String,
    /// `creation-time`.
    pub creation_time: SystemTime,
    /// `last-access-time`.
    pub last_access_time: SystemTime,
    /// `expiry-time`. `None` means a session cookie (`persistent-flag`
    /// false), which is never written to disk.
    pub expiry_time: Option<SystemTime>,
    /// `host-only-flag`.
    pub host_only: bool,
    /// `secure-only-flag`.
    pub secure_only: bool,
    /// `http-only-flag`.
    pub http_only: bool,
    /// `same-site-flag`.
    pub same_site: SameSite,
}

impl StoredCookie {
    /// Whether the cookie has expired at `now` (§5.7: a cookie whose
    /// expiry-time is in the past is evicted).
    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.expiry_time.is_some_and(|t| t <= now)
    }
}

const COLUMNS: &str = "partition_key, name, value, domain, path, host_only, secure_only, \
                       http_only, same_site, creation_ms, last_access_ms, expiry_ms";

/// The persistent cookie store. Get one from
/// [`Profile::cookies`](crate::Profile::cookies).
#[derive(Debug, Clone, Copy)]
pub struct Cookies<'p> {
    conn: &'p Connection,
}

impl<'p> Cookies<'p> {
    pub(crate) fn new(conn: &'p Connection) -> Self {
        Self { conn }
    }

    /// Saves `cookie`, replacing any stored cookie with the same identity.
    /// A session cookie or one already expired at `now` is not stored, and
    /// removes the stored cookie it replaces.
    pub fn save(&self, cookie: &StoredCookie, now: SystemTime) -> Result<()> {
        save_with(self.conn, cookie, now)
    }

    /// Saves many cookies in one transaction, with the same rules as
    /// [`Cookies::save`].
    pub fn save_all(&self, cookies: &[StoredCookie], now: SystemTime) -> Result<()> {
        let tx = self.conn.unchecked_transaction().db()?;
        for cookie in cookies {
            save_with(&tx, cookie, now)?;
        }
        tx.commit().db()
    }

    /// Every unexpired cookie, in all partitions, ordered by partition, then
    /// creation time.
    pub fn load_all(&self, now: SystemTime) -> Result<Vec<StoredCookie>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM cookies WHERE expiry_ms > ?1
                 ORDER BY partition_key, creation_ms, name"
            ),
            params![to_millis(now)],
        )
    }

    /// The unexpired cookies of one partition, ordered by creation time.
    pub fn load_partition(
        &self,
        partition_key: &str,
        now: SystemTime,
    ) -> Result<Vec<StoredCookie>> {
        self.query(
            &format!(
                "SELECT {COLUMNS} FROM cookies WHERE partition_key = ?1 AND expiry_ms > ?2
                 ORDER BY creation_ms, name"
            ),
            params![partition_key, to_millis(now)],
        )
    }

    fn query(&self, sql: &str, args: impl rusqlite::Params) -> Result<Vec<StoredCookie>> {
        let mut stmt = self.conn.prepare(sql).db()?;
        let rows = stmt.query_map(args, cookie_from_row).db()?;
        rows.collect::<std::result::Result<_, _>>().db()
    }

    /// Removes the stored cookie with `cookie`'s identity. Returns whether
    /// one existed.
    pub fn remove(&self, cookie: &StoredCookie) -> Result<bool> {
        remove_with(self.conn, cookie)
    }

    /// Deletes every cookie whose expiry time is at or before `now`, and
    /// returns how many were deleted. Opening a profile does this too.
    pub fn purge_expired(&self, now: SystemTime) -> Result<usize> {
        self.conn
            .execute(
                "DELETE FROM cookies WHERE expiry_ms <= ?1",
                [to_millis(now)],
            )
            .db()
    }

    /// Deletes every cookie in one partition; returns how many.
    pub fn clear_partition(&self, partition_key: &str) -> Result<usize> {
        self.conn
            .execute(
                "DELETE FROM cookies WHERE partition_key = ?1",
                [partition_key],
            )
            .db()
    }

    /// Deletes every cookie; returns how many.
    pub fn clear(&self) -> Result<usize> {
        self.conn.execute("DELETE FROM cookies", []).db()
    }
}

/// Inserts or replaces `cookie`, or removes it if it must not be stored.
fn save_with(conn: &Connection, cookie: &StoredCookie, now: SystemTime) -> Result<()> {
    let expiry = match cookie.expiry_time {
        Some(t) if t > now => t,
        _ => {
            remove_with(conn, cookie)?;
            return Ok(());
        }
    };
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO cookies ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
        ),
        params![
            cookie.partition_key,
            cookie.name,
            cookie.value,
            cookie.domain,
            cookie.path,
            cookie.host_only,
            cookie.secure_only,
            cookie.http_only,
            cookie.same_site.to_db(),
            to_millis(cookie.creation_time),
            to_millis(cookie.last_access_time),
            to_millis(expiry),
        ],
    )
    .db()?;
    Ok(())
}

fn remove_with(conn: &Connection, c: &StoredCookie) -> Result<bool> {
    let n = conn
        .execute(
            "DELETE FROM cookies WHERE partition_key = ?1 AND name = ?2 AND domain = ?3
             AND host_only = ?4 AND path = ?5",
            params![c.partition_key, c.name, c.domain, c.host_only, c.path],
        )
        .db()?;
    Ok(n > 0)
}

fn cookie_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredCookie> {
    Ok(StoredCookie {
        partition_key: row.get(0)?,
        name: row.get(1)?,
        value: row.get(2)?,
        domain: row.get(3)?,
        path: row.get(4)?,
        host_only: row.get(5)?,
        secure_only: row.get(6)?,
        http_only: row.get(7)?,
        same_site: SameSite::from_db(row.get(8)?),
        creation_time: from_millis(row.get(9)?),
        last_access_time: from_millis(row.get(10)?),
        expiry_time: Some(from_millis(row.get(11)?)),
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::Profile;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    const NOW: u64 = 1_700_000_000;

    fn cookie(partition: &str, name: &str, expiry: Option<u64>) -> StoredCookie {
        StoredCookie {
            partition_key: partition.to_owned(),
            name: name.to_owned(),
            value: format!("{name}-value"),
            domain: "example.com".to_owned(),
            path: "/".to_owned(),
            creation_time: at(NOW - 100),
            last_access_time: at(NOW - 10),
            expiry_time: expiry.map(at),
            host_only: true,
            secure_only: true,
            http_only: false,
            same_site: SameSite::Lax,
        }
    }

    #[test]
    fn round_trips_every_field() {
        let p = Profile::open_in_memory().unwrap();
        let c = p.cookies();
        for same_site in [
            SameSite::Strict,
            SameSite::Lax,
            SameSite::None,
            SameSite::Default,
        ] {
            let mut k = cookie("https://top.example", "id", Some(NOW + 3600));
            k.same_site = same_site;
            k.host_only = false;
            k.secure_only = false;
            k.http_only = true;
            k.path = "/a/b".into();
            k.value = "va=lue; ünïcode".into();
            c.save(&k, at(NOW)).unwrap();
            assert_eq!(c.load_all(at(NOW)).unwrap(), vec![k]);
        }
    }

    #[test]
    fn session_cookies_are_not_persisted() {
        let p = Profile::open_in_memory().unwrap();
        let c = p.cookies();
        c.save(&cookie("p", "session", None), at(NOW)).unwrap();
        assert!(c.load_all(at(NOW)).unwrap().is_empty());

        // A session cookie replacing a persistent one removes it from disk.
        c.save(&cookie("p", "x", Some(NOW + 60)), at(NOW)).unwrap();
        assert_eq!(c.load_all(at(NOW)).unwrap().len(), 1);
        c.save(&cookie("p", "x", None), at(NOW)).unwrap();
        assert!(c.load_all(at(NOW)).unwrap().is_empty());
    }

    #[test]
    fn saving_an_expired_cookie_deletes() {
        let p = Profile::open_in_memory().unwrap();
        let c = p.cookies();
        c.save(&cookie("p", "x", Some(NOW + 60)), at(NOW)).unwrap();
        c.save(&cookie("p", "x", Some(NOW - 1)), at(NOW)).unwrap();
        assert!(c.load_all(at(NOW)).unwrap().is_empty());
    }

    #[test]
    fn same_identity_replaces() {
        let p = Profile::open_in_memory().unwrap();
        let c = p.cookies();
        c.save(&cookie("p", "x", Some(NOW + 60)), at(NOW)).unwrap();
        let mut newer = cookie("p", "x", Some(NOW + 120));
        newer.value = "new".into();
        c.save(&newer, at(NOW)).unwrap();
        assert_eq!(c.load_all(at(NOW)).unwrap(), vec![newer.clone()]);

        // Differing host-only flag or path is a different cookie.
        let mut other = newer.clone();
        other.host_only = false;
        c.save(&other, at(NOW)).unwrap();
        let mut other_path = newer;
        other_path.path = "/x".into();
        c.save(&other_path, at(NOW)).unwrap();
        assert_eq!(c.load_all(at(NOW)).unwrap().len(), 3);
    }

    #[test]
    fn partitions_are_separate() {
        let p = Profile::open_in_memory().unwrap();
        let c = p.cookies();
        let a = cookie("https://a.example", "x", Some(NOW + 60));
        let b = cookie("https://b.example", "x", Some(NOW + 60));
        c.save_all(&[a.clone(), b.clone()], at(NOW)).unwrap();
        assert_eq!(
            c.load_partition("https://a.example", at(NOW)).unwrap(),
            vec![a]
        );
        assert_eq!(c.load_all(at(NOW)).unwrap().len(), 2);
        assert_eq!(c.clear_partition("https://b.example").unwrap(), 1);
        assert!(
            c.load_partition("https://b.example", at(NOW))
                .unwrap()
                .is_empty()
        );
        assert_eq!(c.clear().unwrap(), 1);
        assert!(c.load_all(at(NOW)).unwrap().is_empty());
    }

    #[test]
    fn expiry_is_honored_and_purged() {
        let p = Profile::open_in_memory().unwrap();
        let c = p.cookies();
        c.save(&cookie("p", "short", Some(NOW + 10)), at(NOW))
            .unwrap();
        c.save(&cookie("p", "long", Some(NOW + 1000)), at(NOW))
            .unwrap();
        let later = at(NOW + 10);
        let names: Vec<_> = c
            .load_all(later)
            .unwrap()
            .into_iter()
            .map(|k| k.name)
            .collect();
        assert_eq!(names, vec!["long"]);
        assert_eq!(c.purge_expired(later).unwrap(), 1);
        assert_eq!(c.purge_expired(later).unwrap(), 0);
        assert_eq!(c.load_all(at(NOW)).unwrap().len(), 1);
    }

    #[test]
    fn remove_by_identity() {
        let p = Profile::open_in_memory().unwrap();
        let c = p.cookies();
        let k = cookie("p", "x", Some(NOW + 60));
        c.save(&k, at(NOW)).unwrap();
        assert!(c.remove(&k).unwrap());
        assert!(!c.remove(&k).unwrap());
    }

    #[test]
    fn open_purges_expired_cookies() {
        let tmp = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        {
            let p = Profile::open_at(tmp.path()).unwrap();
            let mut k = cookie("p", "soon", None);
            k.expiry_time = Some(now + Duration::from_millis(1));
            p.cookies().save(&k, now).unwrap();
            let mut k = cookie("p", "later", None);
            k.expiry_time = Some(now + Duration::from_secs(3600));
            p.cookies().save(&k, now).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        let p = Profile::open_at(tmp.path()).unwrap();
        let count: i64 = p
            .conn_for_tests()
            .query_row("SELECT COUNT(*) FROM cookies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn unknown_same_site_reads_as_default() {
        assert_eq!(SameSite::from_db(42), SameSite::Default);
        for s in [
            SameSite::Strict,
            SameSite::Lax,
            SameSite::None,
            SameSite::Default,
        ] {
            assert_eq!(SameSite::from_db(s.to_db()), s);
        }
    }
}
