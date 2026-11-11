//! The user's profile: persistent cookies for `wb-net`, the first-run
//! choices, and the settings the shell reads.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wb_net::{Cookie, CookieStore, SameSite};
use wb_storage::{Profile, StoredCookie};

use crate::filters::FILTER_LISTS;
use crate::omnibox::SearchEngine;

/// A profile shared by the UI thread and the network stack's cookie
/// store.
#[derive(Clone, Debug)]
pub struct SharedProfile(Arc<Mutex<Profile>>);

impl SharedProfile {
    /// Wraps an open profile.
    #[must_use]
    pub fn new(profile: Profile) -> SharedProfile {
        SharedProfile(Arc::new(Mutex::new(profile)))
    }

    /// Locks the profile. A panic while it was held does not lose it.
    pub fn lock(&self) -> MutexGuard<'_, Profile> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A cookie store writing persistent cookies into this profile.
    #[must_use]
    pub fn cookie_store(&self) -> Arc<dyn CookieStore> {
        Arc::new(ProfileCookies(self.clone()))
    }
}

/// [`CookieStore`] backed by `wb-storage`. Session cookies are not
/// persisted (storage refuses them), so they don't survive a restart.
struct ProfileCookies(SharedProfile);

fn to_time(secs: i64) -> SystemTime {
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs.unsigned_abs())
    } else {
        UNIX_EPOCH
            .checked_sub(Duration::from_secs(secs.unsigned_abs()))
            .unwrap_or(UNIX_EPOCH)
    }
}

fn to_secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

fn to_stored(c: &Cookie) -> StoredCookie {
    StoredCookie {
        partition_key: c.partition.clone(),
        name: c.name.clone(),
        value: c.value.clone(),
        domain: c.domain.clone(),
        path: c.path.clone(),
        creation_time: to_time(c.creation),
        last_access_time: to_time(c.last_access),
        expiry_time: c.expires.map(to_time),
        host_only: c.host_only,
        secure_only: c.secure,
        http_only: c.http_only,
        same_site: match c.same_site {
            SameSite::None => wb_storage::SameSite::None,
            SameSite::Lax => wb_storage::SameSite::Lax,
            SameSite::Strict => wb_storage::SameSite::Strict,
            SameSite::Default => wb_storage::SameSite::Default,
        },
    }
}

fn from_stored(c: StoredCookie) -> Cookie {
    Cookie {
        partition: c.partition_key,
        name: c.name,
        value: c.value,
        domain: c.domain,
        path: c.path,
        expires: c.expiry_time.map(to_secs),
        creation: to_secs(c.creation_time),
        last_access: to_secs(c.last_access_time),
        host_only: c.host_only,
        secure: c.secure_only,
        http_only: c.http_only,
        same_site: match c.same_site {
            wb_storage::SameSite::None => SameSite::None,
            wb_storage::SameSite::Lax => SameSite::Lax,
            wb_storage::SameSite::Strict => SameSite::Strict,
            wb_storage::SameSite::Default => SameSite::Default,
        },
    }
}

impl CookieStore for ProfileCookies {
    fn load(&self) -> Vec<Cookie> {
        let p = self.0.lock();
        p.cookies()
            .load_all(SystemTime::now())
            .unwrap_or_default()
            .into_iter()
            .map(from_stored)
            .collect()
    }

    fn save(&self, cookie: &Cookie) {
        let p = self.0.lock();
        // A storage error loses one cookie write; browsing goes on.
        let _ = p.cookies().save(&to_stored(cookie), SystemTime::now());
    }

    fn delete(&self, cookie: &Cookie) {
        let p = self.0.lock();
        let _ = p.cookies().remove(&to_stored(cookie));
    }
}

/// What the user chose on the first-run screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FirstRunChoice {
    /// The search engine, or `None` to choose later (none is preselected).
    pub search_engine: Option<SearchEngine>,
    /// Whether to download the content-blocker filter lists now and keep
    /// them updated.
    pub download_filter_lists: bool,
}

/// Whether the first-run screen still has to be shown: no filter list has
/// a stored state yet ([`complete_first_run`] stores one for every list,
/// enabled or not).
// M1 follow-up: a dedicated "first run done" setting in wb-storage.
#[must_use]
pub fn needs_first_run(profile: &Profile) -> bool {
    profile
        .settings()
        .filter_lists()
        .map_or(true, |l| l.is_empty())
}

/// Stores the first-run choices.
///
/// # Errors
///
/// A storage error.
pub fn complete_first_run(profile: &Profile, choice: &FirstRunChoice) -> wb_storage::Result<()> {
    let s = profile.settings();
    s.set_search_engine(choice.search_engine.map(|e| e.id))?;
    s.set_auto_update_filter_lists(choice.download_filter_lists)?;
    for list in FILTER_LISTS {
        s.set_filter_list_enabled(list.id, choice.download_filter_lists)?;
    }
    Ok(())
}

/// The chosen search engine, if any.
#[must_use]
pub fn search_engine(profile: &Profile) -> Option<SearchEngine> {
    profile
        .settings()
        .search_engine()
        .ok()
        .flatten()
        .and_then(|id| SearchEngine::by_id(&id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_flow() {
        let p = Profile::open_in_memory().unwrap();
        assert!(needs_first_run(&p));
        assert_eq!(search_engine(&p), None);
        let choice = FirstRunChoice {
            search_engine: SearchEngine::by_id("mojeek"),
            download_filter_lists: false,
        };
        complete_first_run(&p, &choice).unwrap();
        assert!(!needs_first_run(&p));
        assert_eq!(search_engine(&p).map(|e| e.id), Some("mojeek"));
        let lists = p.settings().filter_lists().unwrap();
        assert_eq!(lists.len(), FILTER_LISTS.len());
        assert!(lists.iter().all(|l| !l.enabled));
        assert!(!p.settings().auto_update_filter_lists().unwrap());
    }

    #[test]
    fn cookies_round_trip_through_storage() {
        let shared = SharedProfile::new(Profile::open_in_memory().unwrap());
        let store = shared.cookie_store();
        let now = to_secs(SystemTime::now());
        let c = Cookie {
            partition: "https://a.test".into(),
            name: "n".into(),
            value: "v".into(),
            domain: "a.test".into(),
            path: "/".into(),
            expires: Some(now + 3600),
            creation: now,
            last_access: now,
            host_only: true,
            secure: true,
            http_only: false,
            same_site: SameSite::Lax,
        };
        store.save(&c);
        let session = Cookie {
            name: "s".into(),
            expires: None,
            ..c.clone()
        };
        store.save(&session);
        assert_eq!(store.load(), vec![c.clone()]);
        store.delete(&c);
        assert!(store.load().is_empty());
        assert_eq!(to_secs(to_time(-5)), -5);
    }
}
