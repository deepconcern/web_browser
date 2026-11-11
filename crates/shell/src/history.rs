//! A tab's session history (HTML §7.4 "session history"), reduced to what
//! M1 needs: a list of entries and a current index, where a new
//! navigation drops the forward entries.

use wb_ipc::{NavigationMethod, Url};

/// The most entries a tab keeps; older ones are dropped.
pub const MAX_ENTRIES: usize = 50;

/// One session history entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The document's URL.
    pub url: Url,
    /// The document's title.
    pub title: String,
    /// The scroll position when the user left the entry.
    pub scroll: (f32, f32),
    /// Whether the document came from a POST (reloading it resubmits).
    pub post: bool,
}

/// A tab's back/forward list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionHistory {
    entries: Vec<Entry>,
    /// Index of the current entry; meaningless when `entries` is empty.
    current: usize,
}

impl SessionHistory {
    /// An empty history.
    #[must_use]
    pub fn new() -> SessionHistory {
        SessionHistory::default()
    }

    /// The current entry.
    #[must_use]
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.current)
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether [`SessionHistory::back`] would move.
    #[must_use]
    pub fn can_go_back(&self) -> bool {
        !self.entries.is_empty() && self.current > 0
    }

    /// Whether [`SessionHistory::forward`] would move.
    #[must_use]
    pub fn can_go_forward(&self) -> bool {
        self.current + 1 < self.entries.len()
    }

    /// Records a committed navigation to `url`: drops the forward entries
    /// and appends a new current entry. A navigation to the current URL
    /// with GET (a reload or same-URL link) replaces the entry instead
    /// (HTML §7.4.2.2 "navigate", historyHandling "replace" when the URL
    /// equals the current one).
    pub fn push(&mut self, url: Url, title: String, method: &NavigationMethod) {
        let post = matches!(method, NavigationMethod::Post { .. });
        let entry = Entry {
            url,
            title,
            scroll: (0.0, 0.0),
            post,
        };
        if let Some(cur) = self.entries.get_mut(self.current)
            && cur.url == entry.url
            && !post
        {
            *cur = entry;
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.current + 1);
        }
        self.entries.push(entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.current = self.entries.len() - 1;
    }

    /// Updates the current entry after a load that replaced it in place
    /// (reload, back/forward commit): its final URL and title.
    pub fn replace_current(&mut self, url: Url, title: String) {
        if let Some(cur) = self.entries.get_mut(self.current) {
            cur.url = url;
            cur.title = title;
        }
    }

    /// Sets the current entry's title.
    pub fn set_title(&mut self, title: String) {
        if let Some(cur) = self.entries.get_mut(self.current) {
            cur.title = title;
        }
    }

    /// Remembers the scroll position of the current entry.
    pub fn set_scroll(&mut self, scroll: (f32, f32)) {
        if let Some(cur) = self.entries.get_mut(self.current) {
            cur.scroll = scroll;
        }
    }

    /// Moves back one entry and returns it.
    pub fn back(&mut self) -> Option<&Entry> {
        if !self.can_go_back() {
            return None;
        }
        self.current -= 1;
        self.entries.get(self.current)
    }

    /// Moves forward one entry and returns it.
    pub fn forward(&mut self) -> Option<&Entry> {
        if !self.can_go_forward() {
            return None;
        }
        self.current += 1;
        self.entries.get(self.current)
    }

    /// The entries, oldest first, and the current index.
    #[must_use]
    pub fn entries(&self) -> (&[Entry], usize) {
        (&self.entries, self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(&format!("https://{s}.test/")).unwrap()
    }

    fn push(h: &mut SessionHistory, s: &str) {
        h.push(u(s), s.into(), &NavigationMethod::Get);
    }

    #[test]
    fn back_forward_and_truncation() {
        let mut h = SessionHistory::new();
        assert!(!h.can_go_back() && !h.can_go_forward() && h.current().is_none());
        assert!(h.back().is_none());
        push(&mut h, "a");
        push(&mut h, "b");
        push(&mut h, "c");
        assert_eq!(h.len(), 3);
        h.set_scroll((0.0, 120.0));
        assert_eq!(h.back().unwrap().url, u("b"));
        assert_eq!(h.back().unwrap().url, u("a"));
        assert!(h.back().is_none());
        let fwd = h.forward().unwrap();
        assert_eq!(fwd.url, u("b"));
        assert!(h.can_go_forward());
        let c = h.forward().unwrap();
        assert_eq!(c.scroll, (0.0, 120.0));
        h.back();
        // A new navigation from b drops c.
        push(&mut h, "d");
        assert!(!h.can_go_forward());
        let (entries, cur) = h.entries();
        let urls: Vec<_> = entries.iter().map(|e| e.url.clone()).collect();
        assert_eq!(urls, [u("a"), u("b"), u("d")]);
        assert_eq!(cur, 2);
    }

    #[test]
    fn same_url_replaces_but_post_appends() {
        let mut h = SessionHistory::new();
        push(&mut h, "a");
        push(&mut h, "a");
        assert_eq!(h.len(), 1);
        let post = NavigationMethod::Post {
            content_type: "x".into(),
            body: vec![],
        };
        h.push(u("a"), "a".into(), &post);
        assert_eq!(h.len(), 2);
        assert!(h.current().unwrap().post);
        h.replace_current(u("z"), "Z".into());
        assert_eq!(h.current().unwrap().title, "Z");
    }

    #[test]
    fn capped() {
        let mut h = SessionHistory::new();
        for i in 0..(MAX_ENTRIES + 10) {
            push(&mut h, &format!("p{i}"));
        }
        assert_eq!(h.len(), MAX_ENTRIES);
        assert_eq!(h.current().unwrap().url, u(&format!("p{}", MAX_ENTRIES + 9)));
        assert_eq!(h.entries().0.first().unwrap().url, u("p10"));
    }
}
