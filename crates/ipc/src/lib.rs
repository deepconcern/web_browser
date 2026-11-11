//! Messages between the browser (UI) and content (page) sides.
//!
//! Each tab's page lives on its own content thread; the UI thread talks to
//! it only through these messages. In M1 the two ends are in one process
//! and connected by [`channel`], an in-process channel; the message types
//! are plain data (no engine types), so a later milestone can carry them
//! across a process boundary.
//!
//! - [`ToContent`]: navigation, viewport changes, scrolling, input.
//! - [`ToBrowser`]: load progress and results, titles, rendered frames,
//!   hovered links and cursors, navigation requests from the page (link
//!   clicks, form submissions) and copied text.
//!
//! The content end wakes the UI's event loop through a [`Waker`] after
//! every message, so the UI never polls.
#![deny(missing_docs)]

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

pub use wb_url::Url;

/// Identifies a tab for the lifetime of the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u64);

/// Keyboard modifiers held during an input event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt / Option.
    pub alt: bool,
    /// Meta / Command / Super.
    pub meta: bool,
}

/// A pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// The primary (usually left) button.
    Primary,
    /// The secondary (usually right) button.
    Secondary,
    /// The middle button (opens links in a new tab).
    Middle,
}

/// Editing and navigation keys the page handles. Text comes separately as
/// [`InputEvent::Text`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// Enter / Return.
    Enter,
    /// Tab (focus navigation).
    Tab,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Space (when not typed into a text field).
    Space,
    /// Escape.
    Escape,
}

/// Input for the page. Coordinates are in CSS px relative to the
/// viewport's top-left corner (the content side adds the scroll offset).
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// The pointer moved to (`x`, `y`).
    PointerMove {
        /// Viewport x in CSS px.
        x: f32,
        /// Viewport y in CSS px.
        y: f32,
    },
    /// The pointer left the page area.
    PointerLeave,
    /// A button was pressed.
    PointerDown {
        /// Viewport x in CSS px.
        x: f32,
        /// Viewport y in CSS px.
        y: f32,
        /// Which button.
        button: PointerButton,
        /// 1 for a single click, 2 for a double click, 3 for a triple.
        clicks: u8,
        /// Modifiers held.
        modifiers: Modifiers,
    },
    /// A button was released.
    PointerUp {
        /// Viewport x in CSS px.
        x: f32,
        /// Viewport y in CSS px.
        y: f32,
        /// Which button.
        button: PointerButton,
        /// Modifiers held.
        modifiers: Modifiers,
    },
    /// A key was pressed.
    KeyDown {
        /// The key.
        key: Key,
        /// Modifiers held.
        modifiers: Modifiers,
    },
    /// Text was typed (after IME and keyboard layout processing).
    Text(String),
}

/// How a navigation sends its request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavigationMethod {
    /// A GET request.
    Get,
    /// A POST request (a form submission).
    Post {
        /// The `Content-Type` of the body.
        content_type: String,
        /// The body.
        body: Vec<u8>,
    },
}

/// A request to navigate a tab.
#[derive(Clone, Debug, PartialEq)]
pub struct NavigationRequest {
    /// Where to go.
    pub url: Url,
    /// GET or POST.
    pub method: NavigationMethod,
    /// The URL of the page that started the navigation (a link or form),
    /// or `None` when the user did (address bar, history, bookmarks).
    pub initiator: Option<Url>,
    /// Bypass the cache (reload).
    pub reload: bool,
    /// The scroll position to restore once loaded (back/forward).
    pub restore_scroll: Option<(f32, f32)>,
    /// Open in a new tab instead (middle click, Ctrl+click).
    pub new_tab: bool,
}

impl NavigationRequest {
    /// A user-initiated GET of `url`.
    #[must_use]
    pub fn user(url: Url) -> NavigationRequest {
        NavigationRequest {
            url,
            method: NavigationMethod::Get,
            initiator: None,
            reload: false,
            restore_scroll: None,
            new_tab: false,
        }
    }
}

/// The viewport the content side lays out and renders for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// Width in device (physical) pixels.
    pub width_px: u32,
    /// Height in device pixels.
    pub height_px: u32,
    /// The window's device scale factor (HiDPI).
    pub device_scale: f32,
    /// The page zoom (1.0 = 100%).
    pub zoom: f32,
}

impl Viewport {
    /// Device pixels per CSS px.
    #[must_use]
    pub fn scale(&self) -> f32 {
        self.device_scale * self.zoom
    }

    /// The layout viewport size in CSS px.
    #[must_use]
    pub fn css_size(&self) -> (f32, f32) {
        let s = self.scale().max(0.01);
        (self.width_px as f32 / s, self.height_px as f32 / s)
    }
}

/// Messages from the browser to a tab's content side.
#[derive(Clone, Debug, PartialEq)]
pub enum ToContent {
    /// Load a page.
    Navigate(NavigationRequest),
    /// The viewport changed size, scale or zoom.
    Viewport(Viewport),
    /// Scroll by (`dx`, `dy`) CSS px (clamped to the document).
    ScrollBy {
        /// Horizontal amount.
        dx: f32,
        /// Vertical amount.
        dy: f32,
    },
    /// Scroll to the document point (`x`, `y`) (clamped).
    ScrollTo {
        /// Horizontal position.
        x: f32,
        /// Vertical position.
        y: f32,
    },
    /// Pointer or keyboard input.
    Input(InputEvent),
    /// Send the selected text back as [`ToBrowser::Copied`].
    CopySelection,
    /// Select the whole document.
    SelectAll,
    /// The user picked option `index` in the drop-down opened by
    /// [`ToBrowser::OpenSelect`], or dismissed it (`None`).
    ChooseOption(Option<usize>),
    /// Stop the content thread.
    Shutdown,
}

/// A rendered frame: the viewport's pixels and where it is scrolled.
#[derive(Clone, PartialEq)]
pub struct Frame {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// Premultiplied RGBA8, rows top to bottom, `width * height * 4` bytes.
    pub pixels: Arc<Vec<u8>>,
    /// The scroll position (CSS px) the frame was drawn at.
    pub scroll: (f32, f32),
    /// The document's scrollable size (CSS px).
    pub content_size: (f32, f32),
    /// The viewport the frame was drawn for.
    pub viewport: Viewport,
    /// Selection highlight rectangles, `[x, y, width, height]` in document
    /// CSS px, for the UI to draw over the pixels.
    pub highlights: Vec<[f32; 4]>,
    /// The focused form control's border box (document CSS px), for a
    /// focus ring.
    pub focus_ring: Option<[f32; 4]>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Frame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("scroll", &self.scroll)
            .field("content_size", &self.content_size)
            .finish_non_exhaustive()
    }
}

/// The mouse cursor the page asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CursorKind {
    /// The default arrow.
    #[default]
    Default,
    /// A pointing hand (links, buttons).
    Pointer,
    /// An I-beam (text, text fields).
    Text,
}

/// Why a load failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadFailure {
    /// HTTPS-only mode: the upgraded connection failed. The UI shows the
    /// interstitial and may add a per-site exception for `site`.
    HttpsUpgradeFailed {
        /// The serialized site (scheme and registrable domain).
        site: String,
        /// The host the exception is stored for.
        host: String,
        /// What went wrong.
        reason: String,
    },
    /// The content blocker blocked the page.
    Blocked {
        /// The filter that matched.
        filter: String,
    },
    /// The response can't be shown.
    UnsupportedContentType(String),
    /// A network or protocol error.
    Network(String),
}

/// How far a load has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadProgress {
    /// Waiting for the document.
    Document,
    /// Fetching stylesheets.
    Stylesheets,
    /// Fetching images.
    Images,
}

/// Messages from a tab's content side to the browser.
#[derive(Clone, Debug, PartialEq)]
pub enum ToBrowser {
    /// A navigation started.
    LoadStarted {
        /// The requested URL.
        url: Url,
    },
    /// A load moved on to a new stage.
    LoadProgress(LoadProgress),
    /// The new page replaced the old one.
    LoadCommitted {
        /// The final URL (after redirects).
        url: Url,
        /// The page title (may be empty).
        title: String,
        /// How many requests the content blocker stopped.
        blocked_requests: usize,
    },
    /// The load was stopped by [`CancelFlag`].
    LoadCancelled,
    /// The load failed; the old page (if any) stays.
    LoadFailed {
        /// The URL that failed.
        url: Url,
        /// Why.
        failure: LoadFailure,
    },
    /// A new frame is ready.
    Frame(Frame),
    /// The link under the pointer changed.
    HoverLink(Option<Url>),
    /// The cursor the page wants.
    Cursor(CursorKind),
    /// The page asks to navigate (link click, form submission).
    Navigate(NavigationRequest),
    /// The selected text, answering [`ToContent::CopySelection`].
    Copied(String),
    /// Show a drop-down for a `<select>`; answer with
    /// [`ToContent::ChooseOption`].
    OpenSelect {
        /// The control's border box, `[x, y, width, height]` in viewport
        /// CSS px.
        rect: [f32; 4],
        /// The option labels.
        options: Vec<String>,
        /// The selected option indices.
        selected: Vec<usize>,
    },
}

/// Wakes the UI's event loop when a message arrives.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A cancel flag the browser sets directly (not through the queue) to stop
/// a load in progress.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Clears the flag (the content side does this when a load starts).
    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The browser's end of a tab channel.
pub struct BrowserEnd {
    tx: Sender<ToContent>,
    rx: Receiver<ToBrowser>,
    cancel: CancelFlag,
}

impl fmt::Debug for BrowserEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserEnd").finish_non_exhaustive()
    }
}

impl BrowserEnd {
    /// Sends a message; returns `false` if the content side is gone.
    pub fn send(&self, msg: ToContent) -> bool {
        self.tx.send(msg).is_ok()
    }

    /// The next message, if one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<ToBrowser> {
        self.rx.try_recv().ok()
    }

    /// Waits up to `timeout` for a message.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ToBrowser> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Stops the load in progress, if any.
    pub fn cancel_load(&self) {
        self.cancel.cancel();
    }
}

/// The content side's end of a tab channel.
pub struct ContentEnd {
    tx: Sender<ToBrowser>,
    rx: Receiver<ToContent>,
    cancel: CancelFlag,
    waker: Waker,
}

impl fmt::Debug for ContentEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContentEnd").finish_non_exhaustive()
    }
}

/// What [`ContentEnd::recv`] got.
#[derive(Debug)]
pub enum Received {
    /// A message.
    Message(ToContent),
    /// Nothing arrived in time.
    Timeout,
    /// The browser end is gone.
    Disconnected,
}

impl ContentEnd {
    /// Sends a message and wakes the UI; returns `false` if the browser
    /// side is gone.
    pub fn send(&self, msg: ToBrowser) -> bool {
        let ok = self.tx.send(msg).is_ok();
        if ok {
            (self.waker)();
        }
        ok
    }

    /// Waits for the next message (blocking).
    #[must_use]
    pub fn recv(&self) -> Option<ToContent> {
        self.rx.recv().ok()
    }

    /// Waits up to `timeout` for the next message.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Received {
        match self.rx.recv_timeout(timeout) {
            Ok(m) => Received::Message(m),
            Err(RecvTimeoutError::Timeout) => Received::Timeout,
            Err(RecvTimeoutError::Disconnected) => Received::Disconnected,
        }
    }

    /// The next message if one is already waiting.
    #[must_use]
    pub fn try_recv(&self) -> Received {
        match self.rx.try_recv() {
            Ok(m) => Received::Message(m),
            Err(TryRecvError::Empty) => Received::Timeout,
            Err(TryRecvError::Disconnected) => Received::Disconnected,
        }
    }

    /// The cancel flag shared with the browser end.
    #[must_use]
    pub fn cancel_flag(&self) -> &CancelFlag {
        &self.cancel
    }
}

/// Makes a connected pair of ends. `waker` runs after every message the
/// content end sends.
#[must_use]
pub fn channel(waker: Waker) -> (BrowserEnd, ContentEnd) {
    let (to_content, from_browser) = mpsc::channel();
    let (to_browser, from_content) = mpsc::channel();
    let cancel = CancelFlag::default();
    (
        BrowserEnd {
            tx: to_content,
            rx: from_content,
            cancel: cancel.clone(),
        },
        ContentEnd {
            tx: to_browser,
            rx: from_browser,
            cancel,
            waker,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn round_trip_wakes_and_cancels() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&wakes);
        let (browser, content) = channel(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        let url = Url::parse("https://a.test/").unwrap();
        assert!(browser.send(ToContent::Navigate(NavigationRequest::user(url.clone()))));
        let t = std::thread::spawn(move || {
            let Some(ToContent::Navigate(req)) = content.recv() else {
                panic!("expected navigate");
            };
            assert!(!content.cancel_flag().is_cancelled());
            content.send(ToBrowser::LoadStarted { url: req.url });
            while !content.cancel_flag().is_cancelled() {
                std::thread::yield_now();
            }
            content.send(ToBrowser::LoadCancelled);
        });
        let msg = browser.recv_timeout(Duration::from_secs(5));
        assert_eq!(msg, Some(ToBrowser::LoadStarted { url }));
        browser.cancel_load();
        assert_eq!(
            browser.recv_timeout(Duration::from_secs(5)),
            Some(ToBrowser::LoadCancelled)
        );
        t.join().unwrap();
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert!(!browser.send(ToContent::Shutdown));
    }

    #[test]
    fn viewport_sizes() {
        let v = Viewport {
            width_px: 1600,
            height_px: 1200,
            device_scale: 2.0,
            zoom: 1.25,
        };
        assert_eq!(v.scale(), 2.5);
        assert_eq!(v.css_size(), (640.0, 480.0));
    }
}
