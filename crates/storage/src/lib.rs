//! Profiles, cookies, history and settings storage.
//!
//! A [`Profile`] is one user's browser state: a directory (by default
//! `$XDG_DATA_HOME/web_browser/<profile>/`, created with `0700` permissions)
//! holding a single SQLite database, or a purely in-memory profile for tests
//! and throwaway sessions. Three typed views give access to the data:
//!
//! - [`Settings`]: typed key/value settings with defaults, per-site lists and
//!   filter-list state.
//! - [`Cookies`]: persistent cookies with every RFC 6265bis storage field plus
//!   the partition key (the top-level site). Session cookies are never
//!   written to disk.
//! - [`History`]: visited URLs with titles, visit times and counts, and
//!   frecency-ranked suggestions for the URL bar.
//!
//! Everything stays on the local disk: this crate has no network code.
//! Times are stored with millisecond precision.
//!
//! SQLite (via `rusqlite`) is a private implementation detail; the public API
//! only uses this crate's own types and `std` types.

mod cookies;
mod error;
mod history;
mod profile;
mod schema;
mod settings;
mod time;

pub use cookies::{Cookies, SameSite, StoredCookie};
pub use error::{DatabaseError, Result, StorageError};
pub use history::{History, HistoryEntry};
pub use profile::{DEFAULT_PROFILE_NAME, Profile};
pub use schema::SCHEMA_VERSION;
pub use settings::{DEFAULT_ZOOM, FilterListState, MAX_ZOOM, MIN_ZOOM, Settings, SiteList};
