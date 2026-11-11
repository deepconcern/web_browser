//! Schema versioning and forward migrations.
//!
//! The current version lives in the one-row `schema_version` table. On open,
//! every migration newer than that version runs in order, each in its own
//! transaction together with the version bump, so a crash mid-way leaves the
//! database at a consistent earlier version. Migrations are append-only: never
//! edit one that has shipped, add a new one instead.

use rusqlite::{Connection, OptionalExtension};

use crate::error::{DbResult, Result, StorageError};

/// One schema step: SQL, then optionally a Rust step for data that SQL alone
/// can't compute. Both run in the same transaction.
struct Migration {
    sql: &'static str,
    post: Option<fn(&Connection) -> Result<()>>,
}

/// Migration `n` (1-based) brings the schema from version `n - 1` to `n`.
const MIGRATIONS: &[Migration] = &[
    Migration {
        post: None,
        // v1: settings, site lists, filter lists, cookies and history.
        sql: "
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE site_lists (
        list TEXT NOT NULL,
        site TEXT NOT NULL,
        PRIMARY KEY (list, site)
    ) WITHOUT ROWID;

    CREATE TABLE filter_lists (
        id              TEXT PRIMARY KEY NOT NULL,
        enabled         INTEGER NOT NULL,
        last_updated_ms INTEGER
    ) WITHOUT ROWID;

    CREATE TABLE cookies (
        partition_key  TEXT NOT NULL,
        name           TEXT NOT NULL,
        value          TEXT NOT NULL,
        domain         TEXT NOT NULL,
        path           TEXT NOT NULL,
        host_only      INTEGER NOT NULL,
        secure_only    INTEGER NOT NULL,
        http_only      INTEGER NOT NULL,
        same_site      INTEGER NOT NULL,
        creation_ms    INTEGER NOT NULL,
        last_access_ms INTEGER NOT NULL,
        expiry_ms      INTEGER NOT NULL,
        PRIMARY KEY (partition_key, name, domain, host_only, path)
    ) WITHOUT ROWID;
    CREATE INDEX cookies_by_expiry ON cookies (expiry_ms);

    CREATE TABLE history (
        id            INTEGER PRIMARY KEY,
        url           TEXT NOT NULL UNIQUE,
        title         TEXT,
        visit_count   INTEGER NOT NULL,
        last_visit_ms INTEGER NOT NULL
    );
    CREATE INDEX history_by_last_visit ON history (last_visit_ms);
    ",
    },
    Migration {
        post: Some(crate::history::refresh_all),
        // v2: one row per visit (with the title seen on that visit), so
        // history can be deleted by time range without leaving traces, and a
        // stored frecency key for ranking. Existing entries get
        // `visit_count` visits at their last visit time, which keeps the
        // counts consistent; `post` then computes every frecency key.
        sql: "
    ALTER TABLE history ADD COLUMN frecency_key REAL NOT NULL DEFAULT 0;

    CREATE TABLE visits (
        id         INTEGER PRIMARY KEY,
        history_id INTEGER NOT NULL REFERENCES history (id) ON DELETE CASCADE,
        visit_ms   INTEGER NOT NULL,
        title      TEXT
    );
    CREATE INDEX visits_by_history ON visits (history_id, visit_ms);
    CREATE INDEX visits_by_time ON visits (visit_ms);

    WITH RECURSIVE expand (history_id, visit_ms, title, remaining) AS (
        SELECT id, last_visit_ms, title, visit_count FROM history WHERE visit_count > 0
        UNION ALL
        SELECT history_id, visit_ms, title, remaining - 1 FROM expand WHERE remaining > 1
    )
    INSERT INTO visits (history_id, visit_ms, title)
        SELECT history_id, visit_ms, title FROM expand;

    DELETE FROM history WHERE visit_count <= 0;
    ",
    },
];

/// The schema version this build creates and understands.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Reads the stored schema version; 0 for a fresh database.
pub(crate) fn current_version(conn: &Connection) -> Result<i64> {
    let has_table: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'schema_version')",
            [],
            |row| row.get(0),
        )
        .db()?;
    if !has_table {
        return Ok(0);
    }
    let version: Option<i64> = conn
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .optional()
        .db()?;
    Ok(version.unwrap_or(0))
}

/// Runs every pending migration up to [`SCHEMA_VERSION`].
pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    migrate_to(conn, SCHEMA_VERSION)
}

/// Runs pending migrations up to `target` (tests use this to build old
/// schemas).
pub(crate) fn migrate_to(conn: &Connection, target: i64) -> Result<()> {
    let found = current_version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(StorageError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let version = i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1);
        if version <= found || version > target {
            continue;
        }
        let tx = conn.unchecked_transaction().db()?;
        tx.execute_batch(migration.sql).db()?;
        if let Some(post) = migration.post {
            post(&tx)?;
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
             DELETE FROM schema_version;",
        )
        .db()?;
        tx.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [version],
        )
        .db()?;
        tx.commit().db()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn
    }

    #[test]
    fn fresh_database_reaches_current_version() {
        let conn = conn();
        assert_eq!(current_version(&conn).unwrap(), 0);
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
        // Idempotent.
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn migrates_v1_history_into_visits() {
        let conn = conn();
        migrate_to(&conn, 1).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 1);
        conn.execute_batch(
            "INSERT INTO history (url, title, visit_count, last_visit_ms)
                 VALUES ('https://a.example/', 'A', 3, 1000),
                        ('https://b.example/', NULL, 1, 2000);
             INSERT INTO settings (key, value) VALUES ('blocker.enabled', 'false');",
        )
        .unwrap();

        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);

        let visits: Vec<(String, i64)> = conn
            .prepare(
                "SELECT h.url, v.visit_ms FROM visits v JOIN history h ON h.id = v.history_id
                 ORDER BY h.url, v.visit_ms",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            visits,
            vec![
                ("https://a.example/".to_owned(), 1000),
                ("https://a.example/".to_owned(), 1000),
                ("https://a.example/".to_owned(), 1000),
                ("https://b.example/".to_owned(), 2000),
            ]
        );
        // Titles move onto the visits, and frecency keys are computed.
        let titled: i64 = conn
            .query_row("SELECT count(*) FROM visits WHERE title = 'A'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(titled, 3);
        let keys: Vec<f64> = conn
            .prepare("SELECT frecency_key FROM history ORDER BY url")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(keys.len(), 2);
        // Three visits outrank one visit a second later.
        assert!(keys[0] > keys[1], "{keys:?}");
        let setting: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'blocker.enabled'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(setting, "false");
    }

    #[test]
    fn rejects_newer_schema() {
        let conn = conn();
        migrate(&conn).unwrap();
        conn.execute(
            "UPDATE schema_version SET version = ?1",
            [SCHEMA_VERSION + 1],
        )
        .unwrap();
        match migrate(&conn) {
            Err(StorageError::SchemaTooNew { found, supported }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
    }
}
