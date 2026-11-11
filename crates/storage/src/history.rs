//! Local browsing history and URL bar suggestions.
//!
//! Each URL has one entry (title, visit count, last visit, frecency key) and
//! one row per visit, holding the visit time and the title seen on that
//! visit. The entry's fields are derived from its visits, so deleting a time
//! range leaves no trace of the deleted visits, and they are recomputed only
//! for the entries a change touches.
//!
//! # Frecency
//!
//! An entry's frecency at time `now` is the average recency weight of its
//! (at most [`SAMPLED_VISITS`]) most recent visits, times its visit count,
//! where a visit's weight halves every [`HALF_LIFE_DAYS`] days:
//! `count / n * Σ 2^-((now - tᵢ) / H)`. Because every weight shares the
//! factor `2^-(now / H)`, the order of two entries never changes as time
//! passes, so we store the time-independent key
//! `log2(count / n * Σ 2^(tᵢ / H))` and rank by it directly in SQL. (The
//! one difference from the formula: a visit timestamped in the future, after
//! a clock change, weighs slightly more than 1 instead of being capped.)

use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{DbResult, Result, StorageError};
use crate::time::{from_millis, to_millis, to_millis_ceil};

/// How many of an entry's most recent visits are sampled for its frecency.
const SAMPLED_VISITS: i64 = 10;
/// A visit's weight halves every this many days.
const HALF_LIFE_DAYS: f64 = 30.0;
const MILLIS_PER_DAY: f64 = 86_400_000.0;
/// How many attempts `clear` makes to truncate the write-ahead log.
const CHECKPOINT_ATTEMPTS: u32 = 3;

/// One history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// The visited URL, as recorded.
    pub url: String,
    /// The most recent title recorded for a remaining visit, if any.
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

    /// Records a visit to `url` at `at`, with the title seen on that visit.
    /// The entry shows the title of its most recent visit that has one, so
    /// `None` keeps showing an earlier title.
    pub fn record_visit(&self, url: &str, title: Option<&str>, at: SystemTime) -> Result<()> {
        if url.is_empty() {
            return Err(StorageError::InvalidValue(
                "history URL is empty".to_owned(),
            ));
        }
        let ms = to_millis(at);
        let tx = self.conn.unchecked_transaction().db()?;
        tx.execute(
            "INSERT INTO history (url, title, visit_count, last_visit_ms) VALUES (?1, NULL, 0, ?2)
             ON CONFLICT (url) DO NOTHING",
            params![url, ms],
        )
        .db()?;
        let id: i64 = tx
            .query_row("SELECT id FROM history WHERE url = ?1", [url], |row| {
                row.get(0)
            })
            .db()?;
        tx.execute(
            "INSERT INTO visits (history_id, visit_ms, title) VALUES (?1, ?2, ?3)",
            params![id, ms, title],
        )
        .db()?;
        refresh_entry(&tx, id)?;
        tx.commit().db()
    }

    /// Sets the title of an existing entry (pages often get their title after
    /// the visit is recorded). `Some` attaches the title to the most recent
    /// visit; `None` forgets every title recorded for the URL. Returns
    /// whether the entry exists.
    pub fn set_title(&self, url: &str, title: Option<&str>) -> Result<bool> {
        let tx = self.conn.unchecked_transaction().db()?;
        let id: Option<i64> = tx
            .query_row("SELECT id FROM history WHERE url = ?1", [url], |row| {
                row.get(0)
            })
            .optional()
            .db()?;
        let Some(id) = id else {
            return Ok(false);
        };
        match title {
            Some(title) => tx.execute(
                "UPDATE visits SET title = ?2 WHERE id = (
                     SELECT id FROM visits WHERE history_id = ?1
                     ORDER BY visit_ms DESC, id DESC LIMIT 1)",
                params![id, title],
            ),
            None => tx.execute("UPDATE visits SET title = NULL WHERE history_id = ?1", [id]),
        }
        .db()?;
        refresh_entry(&tx, id)?;
        tx.commit().db()?;
        Ok(true)
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
    /// entries are ranked by frecency (see the module docs), then by last
    /// visit. An empty query ranks all history by frecency. Every match is
    /// considered; nothing is dropped before ranking.
    pub fn suggest(&self, query: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = escape_like(&query.trim().to_ascii_lowercase());
        let substring = format!("%{query}%");
        let prefix = format!("{query}%");
        let mut stmt = self
            .conn
            .prepare(
                "SELECT url, title, visit_count, last_visit_ms FROM (
                     SELECT url, title, visit_count, last_visit_ms, frecency_key,
                            CASE WHEN instr(url, '://') > 0
                                 THEN substr(url, instr(url, '://') + 3)
                                 ELSE url END AS rest
                     FROM history
                     WHERE url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\'
                 )
                 ORDER BY (url LIKE ?2 ESCAPE '\\' OR rest LIKE ?2 ESCAPE '\\'
                           OR rest LIKE 'www.' || ?2 ESCAPE '\\') DESC,
                          frecency_key DESC, last_visit_ms DESC, url
                 LIMIT ?3",
            )
            .db()?;
        let rows = stmt
            .query_map(params![substring, prefix, sql_limit(limit)], entry_from_row)
            .db()?;
        rows.collect::<std::result::Result<_, _>>().db()
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

    /// Deletes every visit in `start..end` (start inclusive, end exclusive),
    /// including the titles seen on those visits. Affected entries keep their
    /// remaining visits, with count, last visit, title and frecency
    /// recomputed from them; entries left with none are deleted. Returns how
    /// many visits were deleted.
    pub fn delete_range(&self, start: SystemTime, end: SystemTime) -> Result<usize> {
        // Visits are stored in whole milliseconds; rounding both bounds up
        // keeps exactly the stored times t with start <= t < end.
        let (start, end) = (to_millis_ceil(start), to_millis_ceil(end));
        if start >= end {
            return Ok(0);
        }
        let tx = self.conn.unchecked_transaction().db()?;
        let affected: Vec<i64> = {
            let mut stmt = tx
                .prepare(
                    "SELECT DISTINCT history_id FROM visits WHERE visit_ms >= ?1 AND visit_ms < ?2",
                )
                .db()?;
            let rows = stmt.query_map(params![start, end], |row| row.get(0)).db()?;
            rows.collect::<std::result::Result<_, _>>().db()?
        };
        let deleted = tx
            .execute(
                "DELETE FROM visits WHERE visit_ms >= ?1 AND visit_ms < ?2",
                params![start, end],
            )
            .db()?;
        for id in affected {
            refresh_entry(&tx, id)?;
        }
        tx.commit().db()?;
        Ok(deleted)
    }

    /// Deletes all history; returns how many entries were deleted.
    ///
    /// Deleted content is overwritten (`secure_delete`) and the write-ahead
    /// log is truncated so no copy lingers on disk. If another connection
    /// keeps the log from being truncated, the history is still deleted but
    /// this returns [`StorageError::WalNotTruncated`]; calling `clear` again
    /// later finishes the job.
    pub fn clear(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction().db()?;
        tx.execute("DELETE FROM visits", []).db()?;
        let n = tx.execute("DELETE FROM history", []).db()?;
        tx.commit().db()?;
        // The busy timeout already makes each attempt wait for readers; a
        // couple of retries cover a reader that starts between attempts.
        for _ in 0..CHECKPOINT_ATTEMPTS {
            // Returns (busy, log frames, checkpointed frames); busy is 0 once
            // the log was fully checkpointed and truncated, and also for an
            // in-memory database, which has no log.
            let busy: i64 = self
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
                .db()?;
            if busy == 0 {
                return Ok(n);
            }
        }
        Err(StorageError::WalNotTruncated)
    }
}

/// Recomputes every entry's derived fields (used by the schema migration).
pub(crate) fn refresh_all(conn: &Connection) -> Result<()> {
    let ids: Vec<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM history").db()?;
        let rows = stmt.query_map([], |row| row.get(0)).db()?;
        rows.collect::<std::result::Result<_, _>>().db()?
    };
    for id in ids {
        refresh_entry(conn, id)?;
    }
    Ok(())
}

/// Recomputes the visit count, last visit, title and frecency key of entry
/// `id` from its visits, or deletes the entry if it has none left. Every
/// query uses the `(history_id, visit_ms)` index, so the work is bounded by
/// the entry's own visits.
fn refresh_entry(conn: &Connection, id: i64) -> Result<()> {
    let (count, last): (i64, Option<i64>) = conn
        .query_row(
            "SELECT count(*), max(visit_ms) FROM visits WHERE history_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .db()?;
    let Some(last) = last.filter(|_| count > 0) else {
        conn.execute("DELETE FROM history WHERE id = ?1", [id])
            .db()?;
        return Ok(());
    };
    let title: Option<String> = conn
        .query_row(
            "SELECT title FROM visits WHERE history_id = ?1 AND title IS NOT NULL
             ORDER BY visit_ms DESC, id DESC LIMIT 1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .db()?;
    let recent: Vec<i64> = {
        let mut stmt = conn
            .prepare(
                "SELECT visit_ms FROM visits WHERE history_id = ?1
                 ORDER BY visit_ms DESC LIMIT ?2",
            )
            .db()?;
        let rows = stmt
            .query_map(params![id, SAMPLED_VISITS], |row| row.get(0))
            .db()?;
        rows.collect::<std::result::Result<_, _>>().db()?
    };
    conn.execute(
        "UPDATE history SET visit_count = ?2, last_visit_ms = ?3, title = ?4, frecency_key = ?5
         WHERE id = ?1",
        params![id, count, last, title, frecency_key(count, &recent)],
    )
    .db()?;
    Ok(())
}

/// The time-independent frecency key described in the module docs, for an
/// entry with `count` visits whose most recent visit times are `recent`
/// (newest first). Computed relative to the newest visit so the powers of two
/// never overflow.
fn frecency_key(count: i64, recent: &[i64]) -> f64 {
    let Some(&newest) = recent.first() else {
        return f64::NEG_INFINITY;
    };
    let half_life_ms = HALF_LIFE_DAYS * MILLIS_PER_DAY;
    let sum: f64 = recent
        .iter()
        .map(|&t| (t.saturating_sub(newest) as f64 / half_life_ms).exp2())
        .sum();
    let sampled = recent.len() as f64;
    newest as f64 / half_life_ms + sum.log2() + (count as f64 / sampled).log2()
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

/// Reads `url, title, visit_count, last_visit_ms`.
fn entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        url: row.get(0)?,
        title: row.get(1)?,
        visit_count: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0),
        last_visit: from_millis(row.get(3)?),
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

        let got = h.suggest("  RUST ", 10).unwrap();
        assert_eq!(
            urls(&got),
            vec![
                "https://www.rust-lang.example/",
                "http://rustacean.example/",
                "https://docs.example/rust",
            ]
        );
        assert_eq!(h.suggest("rust", 1).unwrap().len(), 1);
        assert!(h.suggest("rust", 0).unwrap().is_empty());
    }

    #[test]
    fn suggest_matches_titles_and_full_url_prefix() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://x.example/", Some("Weather Report"), at(NOW))
            .unwrap();
        h.record_visit("https://y.example/", None, at(NOW)).unwrap();
        assert_eq!(
            urls(&h.suggest("weather", 5).unwrap()),
            vec!["https://x.example/"]
        );
        assert_eq!(
            urls(&h.suggest("https://y", 5).unwrap()),
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
            urls(&h.suggest("example", 5).unwrap()),
            vec!["https://new.example/", "https://old.example/"]
        );
        // Among equally recent pages, the more frequent wins.
        for _ in 0..3 {
            h.record_visit("https://old.example/", None, at(NOW))
                .unwrap();
        }
        assert_eq!(
            urls(&h.suggest("", 5).unwrap()),
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
            urls(&h.suggest("100%_", 5).unwrap()),
            vec!["https://a.example/100%_off"]
        );
        assert_eq!(
            urls(&h.suggest("k\\s", 5).unwrap()),
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
        assert!(p.history().suggest("secret", 5).unwrap().is_empty());
    }

    #[test]
    fn frecency_key_matches_the_formula() {
        let h = HALF_LIFE_DAYS * MILLIS_PER_DAY;
        let day = MILLIS_PER_DAY as i64;
        let now = 1_000 * day;
        // frecency(now) = 2^(key - now / H) = count / n * sum of weights.
        let frecency =
            |count, recent: &[i64]| (frecency_key(count, recent) - now as f64 / h).exp2();
        assert!((frecency(1, &[now]) - 1.0).abs() < 1e-9);
        assert!((frecency(1, &[now - 30 * day]) - 0.5).abs() < 1e-9);
        // Two samples, one fresh and one a half-life old, out of 4 visits.
        assert!((frecency(4, &[now, now - 30 * day]) - 3.0).abs() < 1e-9);
        assert_eq!(frecency_key(0, &[]), f64::NEG_INFINITY);
        // No overflow for far-future timestamps.
        assert!(frecency_key(5, &[i64::MAX / 2, i64::MAX / 2 - day]).is_finite());
    }

    #[test]
    fn prefix_rules() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        h.record_visit("https://www.example.com/", None, at(NOW))
            .unwrap();
        // A substring match with far higher frecency, to show prefix wins.
        for i in 0..50 {
            h.record_visit(
                "https://other.test/?q=example&ample&www.ex",
                None,
                at(NOW - i),
            )
            .unwrap();
        }
        for q in ["exa", "www.ex", "https://www", "EXAMPLE"] {
            assert_eq!(
                urls(&h.suggest(q, 5).unwrap()).first().copied(),
                Some("https://www.example.com/"),
                "{q}"
            );
        }
        assert_eq!(
            urls(&h.suggest("ample", 5).unwrap()).first().copied(),
            Some("https://other.test/?q=example&ample&www.ex")
        );
    }

    #[test]
    fn suggest_keeps_old_prefix_matches() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        // An old prefix match, then more than a thousand newer substring-only
        // matches.
        h.record_visit("https://rust.example/", None, at(NOW - 400 * DAY))
            .unwrap();
        for i in 0..1001 {
            h.record_visit(&format!("https://x.example/rust/{i}"), None, at(NOW - i))
                .unwrap();
        }
        let got = h.suggest("rust", 1).unwrap();
        assert_eq!(urls(&got), vec!["https://rust.example/"]);
    }

    #[test]
    fn suggest_keeps_old_frequent_entries() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        // Visited a thousand times two months ago: frecency ~250.
        for i in 0..1000 {
            h.record_visit("https://x.example/often", None, at(NOW - 60 * DAY - i))
                .unwrap();
        }
        // More than a thousand newer pages, visited once each: frecency ~1.
        for i in 0..1001 {
            h.record_visit(&format!("https://x.example/once/{i}"), None, at(NOW - i))
                .unwrap();
        }
        let got = h.suggest("x.example", 1).unwrap();
        assert_eq!(urls(&got), vec!["https://x.example/often"]);
    }

    #[test]
    fn delete_range_forgets_titles_of_deleted_visits() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        let url = "https://a.example/";
        h.record_visit(url, Some("Old title"), at(NOW - 3 * DAY))
            .unwrap();
        h.record_visit(url, Some("Secret title"), at(NOW - DAY))
            .unwrap();
        h.record_visit("https://b.example/", Some("B"), at(NOW - 3 * DAY))
            .unwrap();
        h.set_title("https://b.example/", Some("Later B")).unwrap();
        h.record_visit("https://b.example/", None, at(NOW - DAY))
            .unwrap();

        h.delete_range(at(NOW - 2 * DAY), at(NOW)).unwrap();
        assert_eq!(
            h.get(url).unwrap().unwrap().title.as_deref(),
            Some("Old title")
        );
        assert!(h.suggest("secret", 5).unwrap().is_empty());
        // A title set after a surviving visit belongs to that visit.
        assert_eq!(
            h.get("https://b.example/")
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("Later B")
        );

        // Without an older title the entry ends up untitled.
        h.record_visit("https://c.example/", None, at(NOW - 3 * DAY))
            .unwrap();
        h.record_visit("https://c.example/", Some("C"), at(NOW - DAY))
            .unwrap();
        h.delete_range(at(NOW - 2 * DAY), at(NOW)).unwrap();
        assert_eq!(h.get("https://c.example/").unwrap().unwrap().title, None);
    }

    #[test]
    fn delete_range_rounds_bounds_up() {
        let p = Profile::open_in_memory().unwrap();
        let h = p.history();
        let ms = |m: u64| UNIX_EPOCH + Duration::from_millis(m);
        let half = Duration::from_micros(500);
        h.record_visit("https://a.example/", None, ms(1000))
            .unwrap();
        h.record_visit("https://b.example/", None, ms(2000))
            .unwrap();
        // 1000 ms is before 1000.5 ms, so it stays.
        assert_eq!(h.delete_range(ms(1000) + half, ms(1500)).unwrap(), 0);
        // 2000 ms is before the exclusive end 2000.5 ms, so it goes.
        assert_eq!(h.delete_range(ms(1500), ms(2000) + half).unwrap(), 1);
        assert!(h.get("https://a.example/").unwrap().is_some());
        assert!(h.get("https://b.example/").unwrap().is_none());
    }

    #[test]
    fn clear_reports_untruncated_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Profile::open_at(tmp.path()).unwrap();
        p.conn_for_tests()
            .busy_timeout(Duration::from_millis(50))
            .unwrap();
        p.history()
            .record_visit("https://secret.example/", None, SystemTime::now())
            .unwrap();
        // Another connection holding a read snapshot keeps the WAL alive.
        let reader = Connection::open(tmp.path().join("profile.sqlite3")).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let _: i64 = reader
            .query_row("SELECT count(*) FROM history", [], |r| r.get(0))
            .unwrap();
        assert!(matches!(
            p.history().clear(),
            Err(StorageError::WalNotTruncated)
        ));
        reader.execute_batch("COMMIT").unwrap();
        assert_eq!(p.history().clear().unwrap(), 0);
    }
}
