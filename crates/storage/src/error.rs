//! Error type for every storage operation.

use std::fmt;
use std::path::PathBuf;

/// Result alias used throughout this crate.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Everything that can go wrong while opening or using a profile.
#[derive(Debug)]
#[non_exhaustive]
pub enum StorageError {
    /// Another browser instance already has this profile open.
    ProfileLocked {
        /// The profile directory that is in use.
        dir: PathBuf,
    },
    /// Neither `$XDG_DATA_HOME` nor `$HOME` is set (to an absolute path), so
    /// the default profile location cannot be found.
    NoDataDirectory,
    /// A profile name was empty or would escape the profiles directory.
    InvalidProfileName(String),
    /// The database was written by a newer version of the browser.
    SchemaTooNew {
        /// The schema version found in the database.
        found: i64,
        /// The newest schema version this build understands.
        supported: i64,
    },
    /// A value passed in was out of range or malformed.
    InvalidValue(String),
    /// A file system operation failed.
    Io {
        /// The path the operation was about.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The database reported an error.
    Database(DatabaseError),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProfileLocked { dir } => write!(
                f,
                "the profile at {} is already in use by another browser instance",
                dir.display()
            ),
            Self::NoDataDirectory => {
                f.write_str("cannot find the data directory: set XDG_DATA_HOME or HOME")
            }
            Self::InvalidProfileName(name) => write!(f, "invalid profile name {name:?}"),
            Self::SchemaTooNew { found, supported } => write!(
                f,
                "the profile uses schema version {found}, but this build only supports up to {supported}"
            ),
            Self::InvalidValue(what) => write!(f, "invalid value: {what}"),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Database(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Database(e) => Some(e),
            _ => None,
        }
    }
}

/// An error from the database engine. Its details are only available through
/// `Display` and `Error::source`, so the engine stays an implementation
/// detail.
#[derive(Debug)]
pub struct DatabaseError(rusqlite::Error);

impl fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for DatabaseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Converts `rusqlite` results into ours without a public `From` impl (which
/// would leak the borrowed crate's type into our API).
pub(crate) trait DbResult<T> {
    /// Maps a database error into [`StorageError::Database`].
    fn db(self) -> Result<T>;
}

impl<T> DbResult<T> for std::result::Result<T, rusqlite::Error> {
    fn db(self) -> Result<T> {
        self.map_err(|e| StorageError::Database(DatabaseError(e)))
    }
}

/// Builds a [`StorageError::Io`] for `path`.
pub(crate) fn io_error(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> StorageError {
    let path = path.into();
    move |source| StorageError::Io { path, source }
}
