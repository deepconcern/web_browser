//! Local browsing history and URL bar suggestions.
//!
//! Each URL has one entry (title, visit count, last visit) and one row per
//! visit. Per-visit times let the user delete a time range, and feed the
//! frecency score used to rank suggestions.

use std::collections::BTreeMap;
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{DbResult, Result, StorageError};
use crate::time::{from_millis, to_millis};

/// How many matching entries (most recently visited first) are scored for a
/// suggestion query. Older matches beyond this are not suggested.
const MAX_CANDIDATES: i64 = 1000;
/// How many of an entry's most recent visits are sampled for its frecency.
const SAMPLED_VISITS: i64 = 10;
/// A visit's weight halves every this many days.
const HALF_LIFE_DAYS: f64 = 30.0;
const MILLIS_PER_DAY: f64 = 86_400_000.0;

/// One history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The visited URL, as recorded.
    pub url: String,
    /// The page title, if one was recorded.
    pub title: Option<String>,
    /// How many visits are recorded for the URL.
    pub visit_count: u64,
    /// The most recent visit.
    pub last_visit: SystemTime,
}

/// The browsing history. Get one from
/// [`Profile::history`](crate::Profile::history).
#[derive(Debug, Clone, Copy)]
pub struct History<'p> {
    conn: &'p Connection,
}

impl<'p> History<'p> {
    pub(crate) fn new(conn: &'p Connection) -> Self {
        Self { conn }
    }

    /// Records a visit to `url` at `at`. A `Some` title replaces the stored
    /// one; `None` keeps it.
    pub fn record_visit(&self, url: &str, title: Option<&str>, at: SystemTime) -> Result<()> {
        if url.is_empty() {
            return Err(StorageError::InvalidValue(
                "history URL is empty".to_owned(),
            ));
        }
        let ms = to_millis(at);
        let tx = self.conn.unchecked_transaction().db()?;
        tx.execute(
            "INSERT INTO history (url, title, visit_count, last_visit_ms) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT (url) DO UPDATE SET
                 visit_count = visit_count + 1,
                 last_visit_ms = max(last_visit_ms, excluded.last_visit_ms),
                 title = coalesce(excluded.title, title)",
            params![url, title, ms],
        )
        .db()?;
        tx.execute(
            "INSERT INTO visits (history_id, visit_ms) SELECT id, ?2 FROM history WHERE url = ?1",
            params![url, ms],
        )
        .db()?;
        tx.commit().db()
    }

    /// Sets the title of an existing entry (pages often get their title after
    /// the visit is recorded). Returns whether the entry exists.
    pub fn set_title(&self, url: &str, title: Option<&str>) -> Result<bool> {
        let n = self
            .conn
            .execute(
                "UPDATE history SET title = ?2 WHERE url = ?1",
                params![url, title],
            )
            .db()?;
        Ok(n > 0)
    }

    /// The entry for exactly `url`, if any.
    pub fn get(&self, url: &str) -> Result<Option<HistoryEntry>> {
        self.conn
            .query_row(
                "SELECT url, title, visit_count, last_visit_ms FROM history WHERE url = ?1",
                [url],
                entry_from_row,
            )
            .optional()
            .db()
    }

    /// The `limit` most recently visited entries, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT url, title, visit_count, last_visit_ms FROM history
                 ORDER BY last_visit_ms DESC, id DESC LIMIT ?1",
            )
            .db()?;
        let rows = stmt.query_map([sql_limit(limit)], entry_from_row).db()?;
        rows.collect::<std::result::Result<_, _>>().db()
    }

    /// URL bar suggestions for `query`, at most `limit` of them.
    ///
    /// An entry matches when `query` (trimmed, ASCII case-insensitive) occurs
    /// in its URL or title. Prefix matches come first: the URL starts with the
    /// query, either as written or after dropping the scheme and a leading
    /// `www.` (so `exa` matches `https://www.example.com/`). Within each group
    /// entries are ranked by frecency at `now`, then by last visit. An empty
    /// query ranks all history by frecency.
    pub fn suggest(&self, query: &str, now: SystemTime, limit: usize) -> Result<Vec<HistoryEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = query.trim().to_ascii_lowercase();
        let pattern = format!("%{}%", escape_like(&query));
        let mut stmt = self
            .conn
            .prepare(
                "WITH cand AS (
                     SELECT id, url, title, visit_count, last_visit_ms FROM history
                     WHERE url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
                     ORDER BY last_visit_ms DESC LIMIT ?2
                 ),
                 recent AS (
                     SELECT history_id, visit_ms,
                            row_number() OVER (PARTITION BY history_id ORDER BY visit_ms DESC) AS rn
                     FROM visits WHERE history_id IN (SELECT id FROM cand)
                 )
                 SELECT c.id, c.url, c.title, c.visit_count, c.last_visit_ms, r.visit_ms
                 FROM cand c LEFT JOIN recent r ON r.history_id = c.id AND r.rn <= ?3",
            )
            .db()?;
        let rows = stmt
            .query_map(params![pattern, MAX_CANDIDATES, SAMPLED_VISITS], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    entry_from_row_at(row, 1)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            })
            .db()?;

        let now_ms = to_millis(now);
        let mut scored: BTreeMap<i64, Scored> = BTreeMap::new();
        for row in rows {
            let (id, entry, visit_ms) = row.db()?;
            let s = scored.entry(id).or_insert_with(|| Scored {
                entry,
                weight_sum: 0.0,
                sampled: 0,
            });
            if let Some(ms) = visit_ms {
                s.weight_sum += visit_weight(now_ms, ms);
                s.sampled += 1;
            }
        }

        let mut ranked: Vec<(bool, f64, HistoryEntry)> = scored
            .into_values()
            .map(|s| {
                let prefix = is_prefix_match(&s.entry.url, &query);
                let frecency = s.frecency();
                (prefix, frecency, s.entry)
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.total_cmp(&a.1))
                .then_with(|| b.2.last_visit.cmp(&a.2.last_visit))
                .then_with(|| a.2.url.cmp(&b.2.url))
        });
        Ok(ranked.into_iter().take(limit).map(|(_, _, e)| e).collect())
    }

    /// Deletes the entry for `url` and all its visits. Returns whether it
    /// existed.
    pub fn delete_url(&self, url: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM history WHERE url = ?1", [url])
            .db()?;
        Ok(n > 0)
    }

    /// Deletes every visit in `start..end` (start inclusive, end exclusive).
    /// Entries keep their remaining visits with updated counts and last visit
    /// times; entries left with none are deleted. Returns how many visits
    /// were deleted.
    pub fn delete_range(&self, start: SystemTime, end: SystemTime) -> Result<usize> {
        let (start, end) = (to_millis(start), to_millis(end));
        if start >= end {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction().db()?;
        let deleted = tx
            .execute(
                "DELETE FROM visits WHERE visit_ms >= ?1 AND visit_ms < ?2",
                params![start, end],
            )
            .db()?;
        // Every entry that lost a visit had last_visit_ms >= start.
        tx.execute(
            "UPDATE history SET
                 visit_count = (SELECT count(*) FROM visits WHERE history_id = history.id),
                 last_visit_ms = coalesce(
                     (SELECT max(visit_ms) FROM visits WHERE history_id = history.id),
                     last_visit_ms)
             WHERE last_visit_ms >= ?1",
            [start],
        )
        .db()?;
        tx.execute("DELETE FROM history WHERE visit_count <= 0", [])
            .db()?;
        tx.commit().db()?;
        Ok(deleted)
    }

    /// Deletes all history; returns how many entries were deleted. Deleted
    /// content is overwritten (`secure_delete`) and the write-ahead log is
    /// truncated so no copy lingers on disk.
    pub fn clear(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction().db()?;
        tx.execute("DELETE FROM visits", []).db()?;
        let n = tx.execute("DELETE FROM history", []).db()?;
        tx.commit().db()?;
        // Best effort: a concurrent reader can make the checkpoint partial,
        // and the next automatic checkpoint finishes the job.
        let _ = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        Ok(n)
    }
}

/// Frecency accumulator for one candidate.
struct Scored {
    entry: HistoryEntry,
    weight_sum: f64,
    sampled: u32,
}

impl Scored {
    /// Average recency weight of the sampled visits, times the total visit
    /// count: frequent and recent both raise the score.
    fn frecency(&self) -> f64 {
        if self.sampled == 0 {
            return 0.0;
        }
        let count = self.entry.visit_count as f64;
        self.weight_sum / f64::from(self.sampled) * count
    }
}

/// Exponential decay with a half-life of [`HALF_LIFE_DAYS`]. Visits in the
/// future (clock changes) count as brand new.
fn visit_weight(now_ms: i64, visit_ms: i64) -> f64 {
    let age_days = (now_ms.saturating_sub(visit_ms).max(0) as f64) / MILLIS_PER_DAY;
    0.5_f64.powf(age_days / HALF_LIFE_DAYS)
}

/// Whether `url` starts with `query` (already lowercased), as written or with
/// the scheme and a leading `www.` removed.
fn is_prefix_match(url: &str, query: &str) -> bool {
    let url = url.to_ascii_lowercase();
    if url.starts_with(query) {
        return true;
    }
    let rest = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
    rest.starts_with(query)
        || rest
            .strip_prefix("www.")
            .is_some_and(|r| r.starts_with(query))
}

/// Escapes `%`, `_` and the escape character itself for a `LIKE … ESCAPE '\'`.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn sql_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    entry_from_row_at(row, 0)
}

/// Reads `url, title, visit_count, last_visit_ms` starting at column `first`.
fn entry_from_row_at(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        url: row.get(first)?,
        title: row.get(first + 1)?,
        visit_count: u64::try_from(row.get::<_, i64>(first + 2)?).unwrap_or(0),
        last_visit: from_millis(row.get(first + 3)?),
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::Profile;

    const DAY: u64 = 86_400;
    const NOW: u64 = 1_700_000_000;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn urls(entries: &[HistoryEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.url.as_str()).collect()
    }

    #[test]
    fn records_visits() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://a.example/", None, at(NOW - 10))
            .unwrap();
        h.record_visit("https://a.example/", Some("A"), at(NOW))
            .unwrap();
        h.record_visit("https://a.example/", None, at(NOW - 5))
            .unwrap();
        let e = h.get("https://a.example/").unwrap().unwrap();
        assert_eq!(e.visit_count, 3);
        assert_eq!(e.title.as_deref(), Some("A"));
        assert_eq!(e.last_visit, at(NOW));
        assert!(h.get("https://b.example/").unwrap().is_none());
        assert!(h.record_visit("", None, at(NOW)).is_err());
    }

    #[test]
    fn set_title() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        assert!(!h.set_title("https://a.example/", Some("A")).unwrap());
        h.record_visit("https://a.example/", None, at(NOW)).unwrap();
        assert!(h.set_title("https://a.example/", Some("A")).unwrap());
        assert_eq!(
            h.get("https://a.example/")
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("A")
        );
    }

    #[test]
    fn recent_orders_newest_first() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://1.example/", None, at(NOW - 3))
            .unwrap();
        h.record_visit("https://2.example/", None, at(NOW - 1))
            .unwrap();
        h.record_visit("https://3.example/", None, at(NOW - 2))
            .unwrap();
        assert_eq!(
            urls(&h.recent(2).unwrap()),
            vec!["https://2.example/", "https://3.example/"]
        );
    }

    #[test]
    fn suggest_prefers_prefix_then_frecency() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        // Substring-only match, visited a lot.
        for i in 0..20 {
            h.record_visit("https://docs.example/rust", None, at(NOW - i))
                .unwrap();
        }
        // Prefix matches (after scheme and www.), one frequent, one rare.
        for i in 0..5 {
            h.record_visit("https://www.rust-lang.example/", Some("Rust"), at(NOW - i))
                .unwrap();
        }
        h.record_visit("http://rustacean.example/", None, at(NOW))
            .unwrap();
        h.record_visit("https://unrelated.example/", None, at(NOW))
            .unwrap();

        let got = h.suggest("  RUST ", at(NOW), 10).unwrap();
        assert_eq!(
            urls(&got),
            vec![
                "https://www.rust-lang.example/",
                "http://rustacean.example/",
                "https://docs.example/rust",
            ]
        );
        assert_eq!(h.suggest("rust", at(NOW), 1).unwrap().len(), 1);
        assert!(h.suggest("rust", at(NOW), 0).unwrap().is_empty());
    }

    #[test]
    fn suggest_matches_titles_and_full_url_prefix() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://x.example/", Some("Weather Report"), at(NOW))
            .unwrap();
        h.record_visit("https://y.example/", None, at(NOW)).unwrap();
        assert_eq!(
            urls(&h.suggest("weather", at(NOW), 5).unwrap()),
            vec!["https://x.example/"]
        );
        assert_eq!(
            urls(&h.suggest("https://y", at(NOW), 5).unwrap()),
            vec!["https://y.example/"]
        );
    }

    #[test]
    fn recency_beats_old_frequency() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        // Five visits a year ago vs two visits today.
        for i in 0..5 {
            h.record_visit("https://old.example/", None, at(NOW - 365 * DAY - i))
                .unwrap();
        }
        for i in 0..2 {
            h.record_visit("https://new.example/", None, at(NOW - i))
                .unwrap();
        }
        assert_eq!(
            urls(&h.suggest("example", at(NOW), 5).unwrap()),
            vec!["https://new.example/", "https://old.example/"]
        );
        // Among equally recent pages, the more frequent wins.
        for _ in 0..3 {
            h.record_visit("https://old.example/", None, at(NOW))
                .unwrap();
        }
        assert_eq!(
            urls(&h.suggest("", at(NOW), 5).unwrap()),
            vec!["https://old.example/", "https://new.example/"]
        );
    }

    #[test]
    fn like_wildcards_are_literal() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://a.example/100%_off", None, at(NOW))
            .unwrap();
        h.record_visit("https://a.example/100xyoff", None, at(NOW))
            .unwrap();
        h.record_visit("https://a.example/back\\slash", None, at(NOW))
            .unwrap();
        assert_eq!(
            urls(&h.suggest("100%_", at(NOW), 5).unwrap()),
            vec!["https://a.example/100%_off"]
        );
        assert_eq!(
            urls(&h.suggest("k\\s", at(NOW), 5).unwrap()),
            vec!["https://a.example/back\\slash"]
        );
    }

    #[test]
    fn delete_range_updates_counts() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://a.example/", None, at(NOW - 3 * DAY))
            .unwrap();
        h.record_visit("https://a.example/", None, at(NOW - DAY))
            .unwrap();
        h.record_visit("https://b.example/", None, at(NOW - DAY))
            .unwrap();
        h.record_visit("https://c.example/", None, at(NOW)).unwrap();

        // Delete the last two days, excluding `now` itself.
        let n = h.delete_range(at(NOW - 2 * DAY), at(NOW)).unwrap();
        assert_eq!(n, 2);
        let a = h.get("https://a.example/").unwrap().unwrap();
        assert_eq!(a.visit_count, 1);
        assert_eq!(a.last_visit, at(NOW - 3 * DAY));
        assert!(h.get("https://b.example/").unwrap().is_none());
        assert_eq!(h.get("https://c.example/").unwrap().unwrap().visit_count, 1);
        assert_eq!(h.delete_range(at(NOW), at(NOW)).unwrap(), 0);
    }

    #[test]
    fn delete_url_and_clear() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://a.example/", None, at(NOW)).unwrap();
        h.record_visit("https://b.example/", None, at(NOW)).unwrap();
        assert!(h.delete_url("https://a.example/").unwrap());
        assert!(!h.delete_url("https://a.example/").unwrap());
        let visits: i64 = p
            .conn_for_tests()
            .query_row("SELECT count(*) FROM visits", [], |r| r.get(0))
            .unwrap();
        assert_eq!(visits, 1, "visits cascade with their entry");
        assert_eq!(h.clear().unwrap(), 1);
        assert!(h.recent(10).unwrap().is_empty());
    }

    #[test]
    fn clear_on_disk_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Profile::open_at(tmp.path()).unwrap();
        p.history()
            .record_visit("https://secret.example/", Some("Secret"), SystemTime::now())
            .unwrap();
        assert_eq!(p.history().clear().unwrap(), 1);
        assert!(
            p.history()
                .suggest("secret", SystemTime::now(), 5)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn weights_decay() {
        let now = 100 * MILLIS_PER_DAY as i64;
        assert_eq!(visit_weight(now, now), 1.0);
        assert_eq!(visit_weight(now, now + 1000), 1.0);
        let half = visit_weight(now, now - 30 * MILLIS_PER_DAY as i64);
        assert!((half - 0.5).abs() < 1e-9);
    }

    #[test]
    fn prefix_rules() {
        assert!(is_prefix_match("https://www.example.com/", "exa"));
        assert!(is_prefix_match("https://www.example.com/", "www.ex"));
        assert!(is_prefix_match("https://www.example.com/", "https://www"));
        assert!(is_prefix_match("HTTPS://Example.com/", "example"));
        assert!(!is_prefix_match("https://www.example.com/", "ample"));
    }
}
