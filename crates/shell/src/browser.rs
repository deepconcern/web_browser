//! The browser core: tabs, navigation, history, settings and the network
//! stack, independent of the window and widgets so it can be tested
//! headlessly. The UI (`app`) draws it and feeds it user actions.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::SystemTime;

use wb_ipc::{
    BrowserEnd, CursorKind, Frame, LoadFailure, LoadProgress, NavigationMethod,
    NavigationRequest, TabId, ToBrowser, ToContent, Url, Viewport, Waker,
};
use wb_net::{BackgroundKind, Client, ClientConfig, NetStats, NetworkPolicy};
use wb_storage::{HistoryEntry, Profile, SiteList};
use wb_text::FontContext;

use crate::content;
use crate::engine::{Engine, Fetcher};
use crate::filters::{self, FILTER_LISTS};
use crate::history::SessionHistory;
use crate::omnibox::{self, Input, SearchEngine};
use crate::profile::{self, FirstRunChoice, SharedProfile};

/// Zoom steps offered by Ctrl +/- (CSS zoom factors).
pub const ZOOM_STEPS: [f32; 13] = [
    0.3, 0.5, 0.67, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];

/// The HTTPS-only interstitial shown instead of a page whose secure
/// connection failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interstitial {
    /// The URL that was requested.
    pub url: Url,
    /// The host (registrable domain) the one-click exception is for.
    pub host: String,
    /// What went wrong.
    pub reason: String,
}

/// An error page shown in place of a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabError {
    /// HTTPS-only interstitial.
    Https(Interstitial),
    /// Any other failure, as text.
    Failed {
        /// The URL that failed.
        url: Url,
        /// Why.
        message: String,
    },
}

/// How the pending navigation affects session history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// A new entry.
    Push,
    /// Back/forward/reload: the current entry is updated in place.
    Replace,
}

/// The `<select>` drop-down a page asked to show.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectPopup {
    /// The control's rectangle in viewport CSS px.
    pub rect: [f32; 4],
    /// The option labels.
    pub options: Vec<String>,
    /// The selected indices.
    pub selected: Vec<usize>,
}

/// One tab.
#[derive(Debug)]
pub struct Tab {
    /// The tab's identifier.
    pub id: TabId,
    end: BrowserEnd,
    thread: Option<JoinHandle<()>>,
    /// Back/forward list.
    pub history: SessionHistory,
    /// The URL bar text.
    pub url_text: String,
    /// The page title.
    pub title: String,
    /// Whether a load is in progress, and how far it got.
    pub loading: Option<LoadProgress>,
    /// The latest rendered frame.
    pub frame: Option<Frame>,
    /// The link under the pointer.
    pub hover_link: Option<Url>,
    /// The cursor the page wants.
    pub cursor: CursorKind,
    /// An error page or interstitial shown instead of the page.
    pub error: Option<TabError>,
    /// The page zoom.
    pub zoom: f32,
    /// Requests the blocker stopped on the current page.
    pub blocked_requests: usize,
    /// An open `<select>` drop-down.
    pub select_popup: Option<SelectPopup>,
    /// Text the page copied, for the UI to put on the clipboard.
    pub copied: Option<String>,
    pending: Option<(NavigationRequest, Pending)>,
}

impl Tab {
    /// The current page's URL (the committed one, not the URL bar text).
    #[must_use]
    pub fn url(&self) -> Option<&Url> {
        self.history.current().map(|e| &e.url)
    }

    /// Sends a message to the tab's content thread.
    pub fn send(&self, msg: ToContent) {
        let _ = self.end.send(msg);
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.end.cancel_load();
        let _ = self.end.send(ToContent::Shutdown);
        // The thread exits on its own; don't block the UI on a slow load.
        drop(self.thread.take());
    }
}

/// What the UI should do after URL bar input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputOutcome {
    /// A navigation started.
    Navigating,
    /// Nothing to do.
    Ignored,
    /// The input is a search but no engine is chosen: ask the user.
    NeedsSearchEngine(String),
}

/// Browser state shared by every window.
pub struct BrowserCore {
    profile: SharedProfile,
    client: Client,
    engine: Engine,
    waker: Waker,
    viewport: Viewport,
    /// The open tabs, in strip order.
    pub tabs: Vec<Tab>,
    /// The index of the active tab.
    pub active: usize,
    next_id: u64,
    /// Whether the first-run screen must be shown.
    pub first_run: bool,
    filter_thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for BrowserCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserCore")
            .field("tabs", &self.tabs.len())
            .field("first_run", &self.first_run)
            .finish_non_exhaustive()
    }
}

/// Why the browser could not start.
#[derive(Debug)]
pub enum StartError {
    /// The network client could not be created.
    Net(wb_net::NetError),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Net(e) => write!(f, "network: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

impl BrowserCore {
    /// Starts the browser on `profile`. Makes no network request: the
    /// policy gate allows filter-list downloads only once the first run is
    /// complete and the user enabled them, and update checks never (none
    /// exist in M1).
    ///
    /// # Errors
    ///
    /// When the network client can't be created.
    pub fn new(
        profile: Profile,
        fonts: Arc<FontContext>,
        waker: Waker,
        viewport: Viewport,
    ) -> Result<BrowserCore, StartError> {
        let first_run = profile::needs_first_run(&profile);
        let settings = profile.settings();
        let downloads = !first_run && settings.auto_update_filter_lists().unwrap_or(false);
        let https_exceptions = settings
            .site_list(SiteList::HttpsOnlyExceptions)
            .unwrap_or_default();
        let blocker_disabled = settings.site_list(SiteList::BlockerDisabled).unwrap_or_default();
        let blocker_on = settings.blocker_enabled().unwrap_or(true);
        let shared = SharedProfile::new(profile);
        let client = Client::with_config(ClientConfig {
            policy: NetworkPolicy {
                filter_list_downloads: downloads,
                update_checks: false,
            },
            cookie_store: Some(shared.cookie_store()),
            ..ClientConfig::default()
        })
        .map_err(StartError::Net)?;
        for host in &https_exceptions {
            if let Some(site) = site_for_host(host) {
                client.allow_insecure_http(&site);
            }
        }
        let fetcher: Arc<dyn Fetcher> = Arc::new(client.clone());
        let engine = Engine::new(fetcher, fonts);
        let mut core = BrowserCore {
            profile: shared,
            client,
            engine,
            waker,
            viewport,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            first_run,
            filter_thread: None,
        };
        if blocker_on {
            core.reload_blocker();
        }
        for host in blocker_disabled {
            core.engine.set_blocker_disabled_for_host(&host, true);
        }
        if downloads {
            core.update_filter_lists(false);
        }
        Ok(core)
    }

    /// The network activity counters (for the privacy test and the
    /// settings page).
    #[must_use]
    pub fn net_stats(&self) -> NetStats {
        self.client.stats()
    }

    /// The shared profile.
    #[must_use]
    pub fn profile(&self) -> &SharedProfile {
        &self.profile
    }

    /// The page engine.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    fn profile_dir(&self) -> Option<PathBuf> {
        self.profile.lock().dir().map(PathBuf::from)
    }

    /// Recompiles the content blocker from the enabled, downloaded lists.
    pub fn reload_blocker(&self) {
        let enabled: Vec<String> = {
            let p = self.profile.lock();
            if !p.settings().blocker_enabled().unwrap_or(true) {
                self.engine.set_content_blocker(None);
                return;
            }
            p.settings()
                .filter_lists()
                .unwrap_or_default()
                .into_iter()
                .filter(|l| l.enabled)
                .map(|l| l.id)
                .collect()
        };
        let ids: Vec<&str> = enabled.iter().map(String::as_str).collect();
        let blocker = self
            .profile_dir()
            .and_then(|dir| filters::compile(&dir, &ids));
        let disabled = self
            .profile
            .lock()
            .settings()
            .site_list(SiteList::BlockerDisabled)
            .unwrap_or_default();
        self.engine.set_content_blocker(blocker);
        for host in disabled {
            self.engine.set_blocker_disabled_for_host(&host, true);
        }
    }

    /// Downloads the enabled filter lists that are due (or all enabled
    /// ones when `force`) on a background thread, then recompiles the
    /// blocker. Each request passes `wb-net`'s policy gate.
    pub fn update_filter_lists(&mut self, force: bool) {
        if self
            .filter_thread
            .as_ref()
            .is_some_and(|t| !t.is_finished())
        {
            return;
        }
        let Some(dir) = self.profile_dir() else { return };
        let now = SystemTime::now();
        let due: Vec<_> = {
            let p = self.profile.lock();
            FILTER_LISTS
                .iter()
                .copied()
                .filter(|l| {
                    p.settings()
                        .filter_list(l.id)
                        .ok()
                        .flatten()
                        .is_some_and(|s| s.enabled && (force || filters::is_due(s.last_updated, now)))
                })
                .collect()
        };
        if due.is_empty() {
            return;
        }
        let client = self.client.clone();
        let profile = self.profile.clone();
        let engine = self.engine.clone();
        let waker = Arc::clone(&self.waker);
        let spawned = std::thread::Builder::new()
            .name("filter-lists".into())
            .spawn(move || {
                for list in due {
                    if let Ok(text) = filters::download(&client, &list)
                        && filters::save(&dir, list.id, &text).is_ok()
                    {
                        let p = profile.lock();
                        let _ = p.settings().set_filter_list_updated(list.id, SystemTime::now());
                    }
                }
                let (enabled, disabled) = {
                    let p = profile.lock();
                    let s = p.settings();
                    if !s.blocker_enabled().unwrap_or(true) {
                        return;
                    }
                    let enabled: Vec<String> = s
                        .filter_lists()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|l| l.enabled)
                        .map(|l| l.id)
                        .collect();
                    (enabled, s.site_list(SiteList::BlockerDisabled).unwrap_or_default())
                };
                let ids: Vec<&str> = enabled.iter().map(String::as_str).collect();
                if let Some(b) = filters::compile(&dir, &ids) {
                    engine.set_content_blocker(Some(b));
                    for host in disabled {
                        engine.set_blocker_disabled_for_host(&host, true);
                    }
                }
                waker();
            });
        self.filter_thread = spawned.ok();
    }

    /// Stores the first-run choices and, if the user agreed, opens the
    /// policy gate for filter-list downloads and starts them.
    pub fn complete_first_run(&mut self, choice: &FirstRunChoice) {
        {
            let p = self.profile.lock();
            let _ = profile::complete_first_run(&p, choice);
        }
        self.first_run = false;
        self.client
            .set_background_allowed(BackgroundKind::FilterListDownload, choice.download_filter_lists);
        if choice.download_filter_lists {
            self.update_filter_lists(true);
        }
    }

    /// The chosen search engine.
    #[must_use]
    pub fn search_engine(&self) -> Option<SearchEngine> {
        profile::search_engine(&self.profile.lock())
    }

    /// Chooses the search engine (`None` to unset).
    pub fn set_search_engine(&self, engine: Option<SearchEngine>) {
        let p = self.profile.lock();
        let _ = p.settings().set_search_engine(engine.map(|e| e.id));
    }

    /// Whether filter-list downloads (first run and automatic updates) are
    /// on.
    #[must_use]
    pub fn filter_downloads_enabled(&self) -> bool {
        self.client.policy().filter_list_downloads
    }

    /// Turns automatic filter-list downloads on or off.
    pub fn set_filter_downloads(&mut self, on: bool) {
        {
            let p = self.profile.lock();
            let _ = p.settings().set_auto_update_filter_lists(on);
        }
        self.client
            .set_background_allowed(BackgroundKind::FilterListDownload, on);
        if on {
            self.update_filter_lists(false);
        }
    }

    /// Whether the content blocker is on globally.
    #[must_use]
    pub fn blocker_enabled(&self) -> bool {
        self.profile.lock().settings().blocker_enabled().unwrap_or(true)
    }

    /// Turns the content blocker on or off globally.
    pub fn set_blocker_enabled(&self, on: bool) {
        {
            let p = self.profile.lock();
            let _ = p.settings().set_blocker_enabled(on);
        }
        self.reload_blocker();
    }

    /// Turns one filter list on or off (downloading it if needed).
    pub fn set_filter_list_enabled(&mut self, id: &str, on: bool) {
        {
            let p = self.profile.lock();
            let _ = p.settings().set_filter_list_enabled(id, on);
        }
        self.reload_blocker();
        if on && self.filter_downloads_enabled() {
            self.update_filter_lists(false);
        }
    }

    /// Whether filter list `id` is on.
    #[must_use]
    pub fn filter_list_enabled(&self, id: &str) -> bool {
        self.profile
            .lock()
            .settings()
            .filter_list(id)
            .ok()
            .flatten()
            .is_some_and(|s| s.enabled)
    }

    /// Whether the blocker is off for the active tab's site.
    #[must_use]
    pub fn blocker_disabled_here(&self) -> Option<bool> {
        let host = self.active_tab()?.url()?.host_str()?.to_string();
        Some(
            self.profile
                .lock()
                .settings()
                .site_list_contains(SiteList::BlockerDisabled, &host)
                .unwrap_or(false),
        )
    }

    /// Toggles the content blocker for the active tab's host and reloads.
    pub fn toggle_blocker_here(&mut self) {
        let Some(host) = self
            .active_tab()
            .and_then(Tab::url)
            .and_then(|u| u.host_str())
            .map(str::to_string)
        else {
            return;
        };
        let disable = !self.blocker_disabled_here().unwrap_or(false);
        {
            let p = self.profile.lock();
            let s = p.settings();
            let _ = if disable {
                s.add_to_site_list(SiteList::BlockerDisabled, &host)
            } else {
                s.remove_from_site_list(SiteList::BlockerDisabled, &host)
            };
        }
        self.engine.set_blocker_disabled_for_host(&host, disable);
        self.reload();
    }

    /// Deletes history, cookies and cached responses.
    pub fn clear_data(&self) {
        {
            let p = self.profile.lock();
            let _ = p.history().clear();
        }
        // Clearing the jar deletes the stored cookies through the store.
        self.client.cookies().clear();
        {
            let p = self.profile.lock();
            let _ = p.cookies().clear();
        }
        self.client.clear_cache();
    }

    /// History entries matching URL bar input, best first.
    #[must_use]
    pub fn suggestions(&self, input: &str, limit: usize) -> Vec<HistoryEntry> {
        if input.trim().is_empty() {
            return Vec::new();
        }
        self.profile
            .lock()
            .history()
            .suggest(input.trim(), limit)
            .unwrap_or_default()
    }

    /// Changes the viewport of every tab (window resize, scale factor).
    pub fn set_viewport(&mut self, width_px: u32, height_px: u32, device_scale: f32) {
        self.viewport = Viewport {
            width_px,
            height_px,
            device_scale,
            zoom: 1.0,
        };
        for t in &self.tabs {
            t.send(ToContent::Viewport(Viewport {
                zoom: t.zoom,
                ..self.viewport
            }));
        }
    }

    /// Opens a tab, optionally loading `request`, and makes it active.
    pub fn open_tab(&mut self, request: Option<NavigationRequest>) -> TabId {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let (end, content_end) = wb_ipc::channel(Arc::clone(&self.waker));
        let zoom = self
            .profile
            .lock()
            .settings()
            .default_zoom()
            .map_or(1.0, |z| z as f32);
        let viewport = Viewport { zoom, ..self.viewport };
        let thread = content::spawn(self.engine.clone(), content_end, viewport).ok();
        self.tabs.push(Tab {
            id,
            end,
            thread,
            history: SessionHistory::new(),
            url_text: String::new(),
            title: String::new(),
            loading: None,
            frame: None,
            hover_link: None,
            cursor: CursorKind::Default,
            error: None,
            zoom,
            blocked_requests: 0,
            select_popup: None,
            copied: None,
            pending: None,
        });
        self.active = self.tabs.len() - 1;
        if let Some(req) = request {
            self.navigate(req);
        }
        id
    }

    /// Closes tab `index`. Returns `false` when it was the last tab (the
    /// UI then exits).
    pub fn close_tab(&mut self, index: usize) -> bool {
        if index < self.tabs.len() {
            self.tabs.remove(index);
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        !self.tabs.is_empty()
    }

    /// The active tab.
    #[must_use]
    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// The active tab, mutably.
    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// Handles URL bar input in the active tab.
    pub fn submit_input(&mut self, text: &str) -> InputOutcome {
        match omnibox::classify(text, self.search_engine()) {
            Input::Url(u) | Input::Search(u) => {
                self.navigate(NavigationRequest::user(u));
                InputOutcome::Navigating
            }
            Input::NeedsSearchEngine(t) => InputOutcome::NeedsSearchEngine(t),
            Input::Empty => InputOutcome::Ignored,
        }
    }

    /// Navigates the active tab (or a new tab for `request.new_tab`).
    pub fn navigate(&mut self, request: NavigationRequest) {
        self.navigate_with(request, Pending::Push);
    }

    fn navigate_with(&mut self, request: NavigationRequest, kind: Pending) {
        if request.new_tab {
            let active = self.active;
            self.open_tab(Some(NavigationRequest {
                new_tab: false,
                ..request
            }));
            // Background tab: keep the current one in front.
            self.active = active;
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        tab.url_text = request.url.as_str().to_string();
        tab.error = None;
        tab.select_popup = None;
        tab.loading = Some(LoadProgress::Document);
        if let Some(f) = &tab.frame {
            tab.history.set_scroll(f.scroll);
        }
        tab.pending = Some((request.clone(), kind));
        tab.send(ToContent::Navigate(request));
    }

    /// Goes back in the active tab.
    pub fn back(&mut self) {
        self.traverse(true);
    }

    /// Goes forward in the active tab.
    pub fn forward(&mut self) {
        self.traverse(false);
    }

    fn traverse(&mut self, back: bool) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if let Some(f) = &tab.frame {
            tab.history.set_scroll(f.scroll);
        }
        let entry = if back {
            tab.history.back()
        } else {
            tab.history.forward()
        };
        let Some(entry) = entry.cloned() else { return };
        let mut req = NavigationRequest::user(entry.url);
        req.restore_scroll = Some(entry.scroll);
        // M1 follow-up: keep POST bodies to resubmit (with a prompt); a
        // POST entry is reloaded with GET for now.
        self.navigate_with(req, Pending::Replace);
    }

    /// Reloads the active tab's page.
    pub fn reload(&mut self) {
        let Some(tab) = self.active_tab() else { return };
        let Some(url) = tab.url().cloned() else { return };
        let mut req = NavigationRequest::user(url);
        req.reload = true;
        req.restore_scroll = tab.frame.as_ref().map(|f| f.scroll);
        self.navigate_with(req, Pending::Replace);
    }

    /// Stops the active tab's load.
    pub fn stop(&mut self) {
        if let Some(tab) = self.active_tab() {
            tab.end.cancel_load();
        }
    }

    /// HTTPS-only interstitial "continue": stores a per-site exception and
    /// loads the page over plain HTTP.
    pub fn allow_insecure_and_continue(&mut self) {
        let Some(tab) = self.active_tab() else { return };
        let Some(TabError::Https(i)) = tab.error.clone() else { return };
        {
            let p = self.profile.lock();
            let _ = p
                .settings()
                .add_to_site_list(SiteList::HttpsOnlyExceptions, &i.host);
        }
        if let Some(site) = site_for_host(&i.host) {
            self.client.allow_insecure_http(&site);
        }
        self.navigate_with(NavigationRequest::user(i.url), Pending::Push);
    }

    /// Sets the active tab's zoom (clamped to the zoom range).
    pub fn set_zoom(&mut self, zoom: f32) {
        let viewport = self.viewport;
        let Some(tab) = self.active_tab_mut() else { return };
        tab.zoom = zoom.clamp(
            wb_storage::MIN_ZOOM as f32,
            wb_storage::MAX_ZOOM as f32,
        );
        tab.send(ToContent::Viewport(Viewport {
            zoom: tab.zoom,
            ..viewport
        }));
    }

    /// Steps the active tab's zoom in (`true`) or out along
    /// [`ZOOM_STEPS`].
    pub fn zoom_step(&mut self, zoom_in: bool) {
        let Some(cur) = self.active_tab().map(|t| t.zoom) else { return };
        let next = if zoom_in {
            ZOOM_STEPS.iter().copied().find(|&z| z > cur + 0.001)
        } else {
            ZOOM_STEPS.iter().rev().copied().find(|&z| z < cur - 0.001)
        };
        if let Some(z) = next {
            self.set_zoom(z);
        }
    }

    /// Handles every waiting message from the content threads. Returns
    /// whether anything changed (the UI repaints).
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut navigations = Vec::new();
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            while let Some(msg) = tab.end.try_recv() {
                changed = true;
                if let Some(req) = handle_message(&self.profile, tab, msg) {
                    navigations.push((i, req));
                }
            }
        }
        for (i, req) in navigations {
            let active = self.active;
            self.active = i;
            self.navigate(req);
            if self.active == i {
                self.active = active;
            }
        }
        changed
    }
}

/// Applies one content message to `tab`; returns a navigation the page
/// asked for.
fn handle_message(profile: &SharedProfile, tab: &mut Tab, msg: ToBrowser) -> Option<NavigationRequest> {
    match msg {
        ToBrowser::LoadStarted { .. } => {
            tab.loading = Some(LoadProgress::Document);
        }
        ToBrowser::LoadProgress(p) => tab.loading = Some(p),
        ToBrowser::LoadCommitted {
            url,
            title,
            blocked_requests,
        } => {
            tab.loading = None;
            tab.error = None;
            tab.blocked_requests = blocked_requests;
            let (method, kind) = tab
                .pending
                .take()
                .map_or((NavigationMethod::Get, Pending::Push), |(r, k)| (r.method, k));
            match kind {
                Pending::Push => tab.history.push(url.clone(), title.clone(), &method),
                Pending::Replace if tab.history.is_empty() => {
                    tab.history.push(url.clone(), title.clone(), &method);
                }
                Pending::Replace => tab.history.replace_current(url.clone(), title.clone()),
            }
            tab.url_text = url.as_str().to_string();
            tab.title = title.clone();
            if url.is_http_scheme() {
                let p = profile.lock();
                let t = (!title.is_empty()).then_some(title.as_str());
                let _ = p.history().record_visit(url.as_str(), t, SystemTime::now());
            }
        }
        ToBrowser::LoadCancelled => {
            tab.loading = None;
            tab.pending = None;
            if let Some(u) = tab.url() {
                tab.url_text = u.as_str().to_string();
            }
        }
        ToBrowser::LoadFailed { url, failure } => {
            tab.loading = None;
            tab.pending = None;
            tab.error = Some(match failure {
                LoadFailure::HttpsUpgradeFailed { host, reason, .. } => {
                    TabError::Https(Interstitial { url, host, reason })
                }
                LoadFailure::Blocked { filter } => TabError::Failed {
                    url,
                    message: format!("This page was blocked by the content blocker ({filter})."),
                },
                LoadFailure::UnsupportedContentType(t) => TabError::Failed {
                    url,
                    message: format!("This browser can't display {t} files yet."),
                },
                LoadFailure::Network(m) => TabError::Failed { url, message: m },
            });
        }
        ToBrowser::Frame(f) => tab.frame = Some(f),
        ToBrowser::HoverLink(l) => tab.hover_link = l,
        ToBrowser::Cursor(c) => tab.cursor = c,
        ToBrowser::Navigate(req) => return Some(req),
        ToBrowser::Copied(t) => tab.copied = Some(t),
        ToBrowser::OpenSelect {
            rect,
            options,
            selected,
        } => {
            tab.select_popup = Some(SelectPopup {
                rect,
                options,
                selected,
            });
        }
    }
    None
}

/// The site whose `http:` exception covers `host`.
fn site_for_host(host: &str) -> Option<wb_url::Site> {
    Url::parse(&format!("http://{host}/")).ok().map(|u| u.site())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn viewport() -> Viewport {
        Viewport {
            width_px: 800,
            height_px: 600,
            device_scale: 1.0,
            zoom: 1.0,
        }
    }

    fn core(profile: Profile) -> BrowserCore {
        BrowserCore::new(
            profile,
            Arc::new(FontContext::new()),
            Arc::new(|| {}),
            viewport(),
        )
        .unwrap()
    }

    fn poll_until(c: &mut BrowserCore, f: impl Fn(&BrowserCore) -> bool) {
        let start = Instant::now();
        while !f(c) {
            c.poll();
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "timed out: {:?}",
                c.active_tab().map(|t| (&t.title, &t.loading, &t.error, t.history.len()))
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Privacy default (issues #1, #3): a fresh profile makes zero
    /// outbound connections, through startup, the first-run screen being
    /// pending, opening tabs and local pages, and declining downloads.
    #[test]
    fn fresh_profile_makes_no_outbound_connections() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = core(Profile::open_at(dir.path()).unwrap());
        assert!(c.first_run);
        assert!(!c.filter_downloads_enabled());
        c.open_tab(None);
        c.open_tab(Some(NavigationRequest::user(Url::parse("about:blank").unwrap())));
        poll_until(&mut c, |c| c.active_tab().is_some_and(|t| t.loading.is_none() && t.url().is_some()));
        c.update_filter_lists(true);
        c.complete_first_run(&FirstRunChoice {
            search_engine: SearchEngine::by_id("duckduckgo"),
            download_filter_lists: false,
        });
        assert!(!c.first_run);
        c.update_filter_lists(true);
        std::thread::sleep(Duration::from_millis(50));
        c.poll();
        assert_eq!(c.net_stats(), NetStats::default());
        drop(c);
        // Restarting the same profile stays quiet too.
        // The content threads release the profile shortly after shutdown.
        let start = Instant::now();
        let profile = loop {
            match Profile::open_at(dir.path()) {
                Ok(p) => break p,
                Err(e) => {
                    assert!(start.elapsed() < Duration::from_secs(10), "{e:?}");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        let c = core(profile);
        assert!(!c.first_run);
        assert_eq!(c.net_stats(), NetStats::default());
    }

    #[test]
    fn navigation_history_and_input() {
        let mut c = core(Profile::open_in_memory().unwrap());
        c.open_tab(None);
        assert_eq!(
            c.submit_input("two words"),
            InputOutcome::NeedsSearchEngine("two words".into())
        );
        assert_eq!(c.submit_input("  "), InputOutcome::Ignored);
        let a = "data:text/html,<title>A</title>a";
        let b = "data:text/html,<title>B</title>b";
        assert_eq!(c.submit_input(a), InputOutcome::Navigating);
        poll_until(&mut c, |c| c.active_tab().is_some_and(|t| t.title == "A"));
        c.submit_input(b);
        poll_until(&mut c, |c| c.active_tab().is_some_and(|t| t.title == "B"));
        let t = c.active_tab().unwrap();
        assert!(t.history.can_go_back());
        assert_eq!(t.history.len(), 2);
        c.back();
        poll_until(&mut c, |c| c.active_tab().is_some_and(|t| t.title == "A" && t.loading.is_none()));
        let t = c.active_tab().unwrap();
        assert_eq!(t.history.len(), 2);
        assert!(t.history.can_go_forward());
        c.reload();
        poll_until(&mut c, |c| c.active_tab().is_some_and(|t| t.loading.is_none()));
        assert_eq!(c.active_tab().unwrap().history.len(), 2);
        // Zoom steps and clamps.
        c.zoom_step(true);
        assert_eq!(c.active_tab().unwrap().zoom, 1.1);
        c.set_zoom(100.0);
        assert_eq!(c.active_tab().unwrap().zoom, wb_storage::MAX_ZOOM as f32);
        c.zoom_step(true);
        assert_eq!(c.active_tab().unwrap().zoom, wb_storage::MAX_ZOOM as f32);
        // Failures show an error page.
        c.submit_input("data:application/zip,PK");
        poll_until(&mut c, |c| c.active_tab().is_some_and(|t| t.error.is_some()));
        // Tabs.
        c.open_tab(None);
        assert_eq!(c.tabs.len(), 2);
        assert_eq!(c.active, 1);
        assert!(c.close_tab(1));
        assert_eq!(c.active, 0);
        assert!(!c.close_tab(0));
    }

    #[test]
    fn settings_round_trip() {
        let c = core(Profile::open_in_memory().unwrap());
        assert_eq!(c.search_engine(), None);
        c.set_search_engine(SearchEngine::by_id("wikipedia"));
        assert_eq!(c.search_engine().map(|e| e.id), Some("wikipedia"));
        assert!(c.blocker_enabled());
        c.set_blocker_enabled(false);
        assert!(!c.blocker_enabled());
        c.clear_data();
        assert!(c.suggestions("x", 5).is_empty());
    }
}
