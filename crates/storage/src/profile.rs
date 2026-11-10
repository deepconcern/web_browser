//! Opening a profile: locating and creating its directory, taking the
//! single-instance lock, configuring SQLite and running migrations.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use crate::cookies::Cookies;
use crate::error::{DbResult, Result, StorageError, io_error};
use crate::history::History;
use crate::schema;
use crate::settings::Settings;

/// The name of the profile used when the user hasn't picked one.
pub const DEFAULT_PROFILE_NAME: &str = "default";

/// Directory under the XDG data directory that holds every profile.
const APP_DIR: &str = "web_browser";
/// The database file inside a profile directory.
const DB_FILE: &str = "profile.sqlite3";
/// The lock file inside a profile directory.
const LOCK_FILE: &str = "lock";
/// How long a statement waits for a busy database before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// An open browser profile: settings, cookies and history.
///
/// An on-disk profile holds an exclusive lock on its directory for as long as
/// this value lives, so a second browser instance opening the same profile
/// gets [`StorageError::ProfileLocked`] instead of corrupting it.
pub struct Profile {
    // Field order matters: the connection closes before the lock is released.
    conn: Connection,
    dir: Option<PathBuf>,
    _lock: Option<File>,
}

impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Profile {
    /// Opens (creating if needed) the profile called `name` in the default
    /// location, `$XDG_DATA_HOME/web_browser/<name>/`, falling back to
    /// `$HOME/.local/share/web_browser/<name>/`.
    pub fn open(name: &str) -> Result<Self> {
        Self::open_at(Self::default_dir(name)?)
    }

    /// The directory [`Profile::open`] would use for `name`, without creating
    /// anything.
    pub fn default_dir(name: &str) -> Result<PathBuf> {
        default_dir_from(
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("HOME"),
            name,
        )
    }

    /// Opens (creating if needed) a profile in `dir`. Missing directories are
    /// created with `0700` permissions, and the profile directory itself is
    /// restricted to `0700` even if it already existed.
    pub fn open_at(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        create_private_dir(&dir)?;
        let lock = lock_dir(&dir)?;
        let conn = Connection::open(dir.join(DB_FILE)).db()?;
        conn.busy_timeout(BUSY_TIMEOUT).db()?;
        // Write-ahead logging: readers don't block the writer, and commits
        // are cheaper. The pragma returns the resulting mode.
        let mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .db()?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StorageError::InvalidValue(format!(
                "could not enable WAL mode (got {mode})"
            )));
        }
        conn.execute_batch("PRAGMA synchronous = NORMAL;").db()?;
        Self::finish(conn, Some(dir), Some(lock))
    }

    /// Opens a fresh profile that lives only in memory and is gone when
    /// dropped. Nothing touches the disk.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().db()?;
        Self::finish(conn, None, None)
    }

    fn finish(conn: Connection, dir: Option<PathBuf>, lock: Option<File>) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA secure_delete = ON;
             PRAGMA trusted_schema = OFF;",
        )
        .db()?;
        schema::migrate(&conn)?;
        let profile = Self {
            conn,
            dir,
            _lock: lock,
        };
        // Expired cookies must never be served again; drop them up front.
        profile.cookies().purge_expired(SystemTime::now())?;
        Ok(profile)
    }

    /// The profile directory, or `None` for an in-memory profile.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Typed settings.
    pub fn settings(&self) -> Settings<'_> {
        Settings::new(&self.conn)
    }

    /// The persistent cookie store.
    pub fn cookies(&self) -> Cookies<'_> {
        Cookies::new(&self.conn)
    }

    /// Browsing history.
    pub fn history(&self) -> History<'_> {
        History::new(&self.conn)
    }

    /// Raw database access for tests elsewhere in the crate.
    #[cfg(test)]
    pub(crate) fn conn_for_tests(&self) -> &Connection {
        &self.conn
    }
}

/// Resolves the default profile directory from the environment values.
fn default_dir_from(
    xdg_data_home: Option<OsString>,
    home: Option<OsString>,
    name: &str,
) -> Result<PathBuf> {
    validate_name(name)?;
    // XDG Base Directory Specification: a relative or empty
    // $XDG_DATA_HOME is invalid and must be ignored; the default is
    // $HOME/.local/share.
    let base = xdg_data_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".local").join("share"))
        })
        .ok_or(StorageError::NoDataDirectory)?;
    Ok(base.join(APP_DIR).join(name))
}

/// A profile name must be a single, ordinary path component.
fn validate_name(name: &str) -> Result<()> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.chars().any(|c| matches!(c, '/' | '\\' | '\0'));
    if bad {
        Err(StorageError::InvalidProfileName(name.to_owned()))
    } else {
        Ok(())
    }
}

/// Creates `dir` (and missing parents) with mode `0700`, and tightens the
/// mode of `dir` itself if it already existed. The XDG spec asks for `0700`
/// on directories it has to create.
fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(dir).map_err(io_error(dir))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(io_error(dir))?;
    }
    #[cfg(not(unix))]
    builder.create(dir).map_err(io_error(dir))?;
    Ok(())
}

/// Takes an exclusive advisory lock on the profile's lock file. The lock is
/// released when the returned file is closed, including when the process
/// dies, so a crash never leaves a stale lock behind.
fn lock_dir(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK_FILE);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(io_error(&path))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(StorageError::ProfileLocked {
            dir: dir.to_path_buf(),
        }),
        Err(TryLockError::Error(e)) => Err(io_error(&path)(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_dir_prefers_xdg_data_home() {
        let dir =
            default_dir_from(Some("/xdg/data".into()), Some("/home/u".into()), "default").unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/data/web_browser/default"));
    }

    #[test]
    fn default_dir_falls_back_to_home() {
        let dir = default_dir_from(None, Some("/home/u".into()), "work").unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.local/share/web_browser/work"));
        // Relative XDG_DATA_HOME is ignored per the XDG spec.
        let dir = default_dir_from(Some("rel".into()), Some("/home/u".into()), "w").unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.local/share/web_browser/w"));
        let dir = default_dir_from(Some("".into()), Some("/home/u".into()), "w").unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.local/share/web_browser/w"));
    }

    #[test]
    fn default_dir_needs_some_base() {
        assert!(matches!(
            default_dir_from(None, None, "default"),
            Err(StorageError::NoDataDirectory)
        ));
        assert!(matches!(
            default_dir_from(None, Some("relative".into()), "default"),
            Err(StorageError::NoDataDirectory)
        ));
    }

    #[test]
    fn rejects_bad_profile_names() {
        for name in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(
                matches!(
                    default_dir_from(Some("/x".into()), None, name),
                    Err(StorageError::InvalidProfileName(_))
                ),
                "{name:?}"
            );
        }
        assert!(default_dir_from(Some("/x".into()), None, "..hidden").is_ok());
    }

    #[test]
    fn creates_private_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("web_browser").join("default");
        let profile = Profile::open_at(&dir).unwrap();
        assert_eq!(profile.dir(), Some(dir.as_path()));
        assert!(dir.join(DB_FILE).is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for d in [&dir, &tmp.path().join("web_browser")] {
                let mode = fs::metadata(d).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700, "{}", d.display());
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn tightens_existing_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let _profile = Profile::open_at(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn uses_wal_and_busy_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = Profile::open_at(tmp.path()).unwrap();
        let mode: String = profile
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        let timeout: i64 = profile
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn second_instance_is_locked_out() {
        let tmp = tempfile::tempdir().unwrap();
        let first = Profile::open_at(tmp.path()).unwrap();
        let err = Profile::open_at(tmp.path()).unwrap_err();
        match &err {
            StorageError::ProfileLocked { dir } => assert_eq!(dir, tmp.path()),
            other => panic!("expected ProfileLocked, got {other:?}"),
        }
        assert!(err.to_string().contains("already in use"));
        drop(first);
        // Released on drop.
        Profile::open_at(tmp.path()).unwrap();
    }

    #[test]
    fn data_survives_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let profile = Profile::open_at(tmp.path()).unwrap();
            profile.settings().set_blocker_enabled(false).unwrap();
            profile
                .history()
                .record_visit("https://example.com/", Some("Example"), SystemTime::now())
                .unwrap();
        }
        let profile = Profile::open_at(tmp.path()).unwrap();
        assert!(!profile.settings().blocker_enabled().unwrap());
        assert!(
            profile
                .history()
                .get("https://example.com/")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn in_memory_profile_works() {
        let profile = Profile::open_in_memory().unwrap();
        assert_eq!(profile.dir(), None);
        assert_eq!(
            schema::current_version(&profile.conn).unwrap(),
            schema::SCHEMA_VERSION
        );
        profile.settings().set_default_zoom(1.5).unwrap();
        assert_eq!(profile.settings().default_zoom().unwrap(), 1.5);
    }

    #[test]
    fn opening_upgrades_a_v1_database() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(tmp.path().join(DB_FILE)).unwrap();
            schema::migrate_to(&conn, 1).unwrap();
            conn.execute(
                "INSERT INTO history (url, title, visit_count, last_visit_ms)
                 VALUES ('https://old.example/', 'Old', 2, ?1)",
                [crate::time::to_millis(SystemTime::now())],
            )
            .unwrap();
        }
        let profile = Profile::open_at(tmp.path()).unwrap();
        let entry = profile
            .history()
            .get("https://old.example/")
            .unwrap()
            .unwrap();
        assert_eq!(entry.visit_count, 2);
        assert_eq!(entry.title.as_deref(), Some("Old"));
        let hits = profile
            .history()
            .suggest("old", SystemTime::now(), 5)
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn newer_schema_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        drop(Profile::open_at(tmp.path()).unwrap());
        {
            let conn = Connection::open(tmp.path().join(DB_FILE)).unwrap();
            conn.execute("UPDATE schema_version SET version = 999", [])
                .unwrap();
        }
        assert!(matches!(
            Profile::open_at(tmp.path()),
            Err(StorageError::SchemaTooNew { found: 999, .. })
        ));
    }
}
