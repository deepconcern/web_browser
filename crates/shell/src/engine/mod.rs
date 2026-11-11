//! The headless engine: loads a page and renders it without any UI.
//!
//! [`Engine::load`] fetches a document through a [`Fetcher`] (a
//! [`wb_net::Client`] for real browsing, a [`LocalFetcher`] for tests and
//! reference rendering), parses it with `wb-html`, fetches its stylesheets
//! and their `@import`s and its images, and returns a [`Page`]. Every
//! subresource request goes through the content blocker's network filters
//! first, and the blocker's cosmetic filters become a user-origin sheet of
//! `display: none !important` rules.
//!
//! [`Page::render`] styles (`wb-style`), lays out (`wb-layout`), builds the
//! display list (`wb-paint`) and rasterizes it for a viewport, scale and
//! scroll position. The style, layout and display list are cached until
//! the viewport, the scale or the document changes. After a render,
//! [`Page::hit_test`] and [`Page::link_at`] map page points back to nodes
//! and links.
//!
//! ```no_run
//! use std::sync::Arc;
//! use wb_shell::engine::{Engine, LocalFetcher, RenderOptions, encode_png};
//! use wb_url::Url;
//!
//! let mut fetcher = LocalFetcher::new();
//! fetcher.insert("https://example.test/", "text/html", "<p style='color:red'>Hi");
//! let engine = Engine::new(Arc::new(fetcher), Engine::system_fonts());
//! let mut page = engine.load(&Url::parse("https://example.test/").unwrap()).unwrap();
//! let raster = page.render(&RenderOptions::new(800.0, 600.0)).unwrap();
//! let png = encode_png(&raster).unwrap();
//! ```
//!
//! # Coordinates
//!
//! All page coordinates are CSS px with the origin at the top-left of the
//! document (not the viewport): add the scroll offset to a viewport point
//! before hit testing. [`RenderOptions::scale`] is the device pixels per
//! CSS px (device scale factor times zoom); the layout viewport is
//! [`RenderOptions::width`] by [`RenderOptions::height`] CSS px.
//!
//! # Threads
//!
//! [`Engine`] is cheap to clone and can be shared between threads. A
//! [`Page`] is `Send`: the shell keeps each one on its content thread.

mod fetch;
mod load;
mod mime;
mod page;
mod png;

#[cfg(test)]
mod tests;

use std::fmt;
use std::sync::{Arc, RwLock};

use wb_dom::Document;
use wb_html::ParseOptions;
use wb_net::{NetError, Request, Response};
use wb_text::FontContext;
use wb_url::Url;

pub use fetch::{Fetcher, LocalFetcher};
pub use page::{Page, RenderOptions};
pub use png::{PngError, encode_png, unpremultiply};
pub use wb_layout::Size;
pub use wb_paint::{Color, HitTarget, PaintError, Point, Raster};
pub use wb_style::ElementState;

use load::Images;

/// Limits and defaults for loading pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineOptions {
    /// Limits for decoding one image.
    pub image_limits: wb_image::Limits,
    /// Most distinct image URLs fetched per page.
    pub max_images: usize,
    /// Most decoded image bytes held per page.
    pub max_image_bytes: usize,
    /// Most stylesheets fetched per page, `@import`s included.
    pub max_stylesheets: usize,
    /// Deepest `@import` nesting followed.
    pub max_import_depth: usize,
    /// Most cosmetic (element-hiding) selectors applied per page.
    pub max_cosmetic_selectors: usize,
    /// Caps on the display list built for a page.
    pub list_limits: wb_paint::ListLimits,
    /// The viewport size (CSS px) used to decide which background images
    /// to fetch while loading, before the page is first rendered.
    pub initial_viewport: (u32, u32),
}

impl Default for EngineOptions {
    fn default() -> EngineOptions {
        EngineOptions {
            image_limits: wb_image::Limits::default(),
            max_images: 1024,
            max_image_bytes: 512 * 1024 * 1024,
            max_stylesheets: 256,
            max_import_depth: 8,
            max_cosmetic_selectors: 50_000,
            list_limits: wb_paint::ListLimits::default(),
            initial_viewport: (1280, 720),
        }
    }
}

/// What a load is doing, reported to a [`LoadObserver`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadStage {
    /// Fetching the document.
    Document,
    /// Fetching stylesheets; `requested` so far.
    Stylesheets {
        /// Stylesheets requested so far.
        requested: usize,
    },
    /// Fetching a batch of `requested` images.
    Images {
        /// Images in this batch.
        requested: usize,
    },
    /// The page is ready.
    Done,
}

/// Watches (and may cancel) a load. Both methods have defaults.
pub trait LoadObserver {
    /// Polled between and during fetches; return `true` to stop the load.
    fn cancelled(&self) -> bool {
        false
    }

    /// Called as the load moves through its stages.
    fn progress(&self, stage: LoadStage) {
        let _ = stage;
    }
}

/// A [`LoadObserver`] that never cancels and ignores progress.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoObserver;

impl LoadObserver for NoObserver {}

/// Why a page could not be loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The document fetch failed (including
    /// [`NetError::HttpsUpgradeFailed`], which the shell shows as the
    /// HTTPS-only interstitial).
    Net(NetError),
    /// The content blocker blocked the document itself (a `$document`
    /// filter).
    Blocked {
        /// The blocking filter's text.
        filter: String,
    },
    /// The response is of a type the engine can't show (e.g. a download).
    UnsupportedContentType(String),
    /// The observer cancelled the load.
    Cancelled,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Net(e) => write!(f, "{e}"),
            LoadError::Blocked { filter } => write!(f, "blocked by filter {filter}"),
            LoadError::UnsupportedContentType(t) => write!(f, "can't display {t}"),
            LoadError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Shared engine state.
pub(crate) struct Inner {
    pub(crate) fetcher: Arc<dyn Fetcher>,
    pub(crate) fonts: Arc<FontContext>,
    pub(crate) blocker: RwLock<Option<wb_blocker::Engine>>,
    pub(crate) options: EngineOptions,
}

/// Loads pages. Cheap to clone: clones share the fetcher, fonts and
/// content blocker.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine")
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

/// How a response body is shown.
enum Kind {
    Html,
    Text,
    Image,
}

impl Engine {
    /// An engine fetching through `fetcher` and shaping text with `fonts`,
    /// with default options and no content blocker.
    #[must_use]
    pub fn new(fetcher: Arc<dyn Fetcher>, fonts: Arc<FontContext>) -> Engine {
        Engine::with_options(fetcher, fonts, EngineOptions::default())
    }

    /// An engine with explicit options.
    #[must_use]
    pub fn with_options(
        fetcher: Arc<dyn Fetcher>,
        fonts: Arc<FontContext>,
        options: EngineOptions,
    ) -> Engine {
        Engine {
            inner: Arc::new(Inner {
                fetcher,
                fonts,
                blocker: RwLock::new(None),
                options,
            }),
        }
    }

    /// Every installed system font (no fonts are bundled). Loading them
    /// scans the font directories, so do it once and share the result.
    #[must_use]
    pub fn system_fonts() -> Arc<FontContext> {
        Arc::new(FontContext::with_system_fonts())
    }

    /// The font context pages are shaped with.
    #[must_use]
    pub fn fonts(&self) -> &Arc<FontContext> {
        &self.inner.fonts
    }

    /// The engine's options.
    #[must_use]
    pub fn options(&self) -> &EngineOptions {
        &self.inner.options
    }

    /// Replaces the content blocker (`None` turns blocking off). Pages
    /// loaded afterwards use it.
    pub fn set_content_blocker(&self, blocker: Option<wb_blocker::Engine>) {
        let mut guard = self
            .inner
            .blocker
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *guard = blocker;
    }

    /// Turns the content blocker off (`disabled = true`) or back on for
    /// `host` and its subdomains. Does nothing without a blocker.
    pub fn set_blocker_disabled_for_host(&self, host: &str, disabled: bool) {
        let mut guard = self
            .inner
            .blocker
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(b) = guard.as_mut() {
            b.set_site_disabled(host, disabled);
        }
    }

    /// Whether the blocker is off for `host` (or there is no blocker).
    #[must_use]
    pub fn is_blocker_disabled_for_host(&self, host: &str) -> bool {
        let guard = self.inner.blocker.read().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().is_none_or(|b| b.is_site_disabled(host))
    }

    /// Loads `url` as a user-initiated navigation.
    ///
    /// # Errors
    ///
    /// See [`LoadError`].
    pub fn load(&self, url: &Url) -> Result<Page, LoadError> {
        self.load_request(Request::navigate(url.clone()), &NoObserver)
    }

    /// Loads a page from a navigation `request` (e.g. a form submission),
    /// reporting progress to `observer`.
    ///
    /// # Errors
    ///
    /// See [`LoadError`].
    pub fn load_request(
        &self,
        request: Request,
        observer: &dyn LoadObserver,
    ) -> Result<Page, LoadError> {
        observer.progress(LoadStage::Document);
        if let Some(filter) = self.inner.blocked(
            &request.url,
            &request.url,
            wb_blocker::ResourceType::Document,
        ) {
            return Err(LoadError::Blocked { filter });
        }
        let response = self.inner.fetcher.fetch(request).map_err(|e| match e {
            NetError::Cancelled => LoadError::Cancelled,
            e => LoadError::Net(e),
        })?;
        if observer.cancelled() {
            return Err(LoadError::Cancelled);
        }
        self.load_response(response, observer)
    }

    /// Makes a page from a fetched document `response`, fetching its
    /// subresources. Non-OK statuses still show their body (an error
    /// page is a page).
    ///
    /// # Errors
    ///
    /// [`LoadError::UnsupportedContentType`] for bodies that are not HTML,
    /// plain text or an image; [`LoadError::Cancelled`].
    pub fn load_response(
        &self,
        response: Response,
        observer: &dyn LoadObserver,
    ) -> Result<Page, LoadError> {
        let content_type = response.content_type().and_then(mime::parse);
        let kind = match content_type.as_ref().map(|m| m.essence.as_str()) {
            None => {
                if wb_image::sniff(&response.body).is_some() {
                    Kind::Image
                } else {
                    Kind::Html
                }
            }
            Some("text/html" | "application/xhtml+xml") => Kind::Html,
            Some(
                "text/plain"
                | "text/css"
                | "text/javascript"
                | "application/javascript"
                | "application/json"
                | "text/xml"
                | "application/xml",
            ) => Kind::Text,
            Some(t) if t.starts_with("image/") && wb_image::sniff(&response.body).is_some() => {
                Kind::Image
            }
            Some(t) => return Err(LoadError::UnsupportedContentType(t.to_string())),
        };
        let charset = content_type.and_then(|m| m.charset);
        let url = response.url.clone();
        let (doc, encoding, images) = match kind {
            Kind::Html => {
                let parsed = wb_html::parse_document_bytes(
                    &response.body,
                    charset.as_deref(),
                    &ParseOptions::default(),
                );
                (parsed.document, parsed.encoding, Images::default())
            }
            Kind::Text => {
                let sniffed = wb_html::sniff_encoding(&response.body, charset.as_deref());
                let text = load::decode_css(&response.body, None, sniffed.name);
                (text_document(&text), sniffed.name, Images::default())
            }
            Kind::Image => {
                let mut images = Images::default();
                load::load_images_from_bytes(
                    &self.inner,
                    &mut images,
                    url.as_str(),
                    &response.body,
                );
                (image_document(url.as_str()), "UTF-8", images)
            }
        };
        self.build_page(url, response.status, doc, encoding, images, observer)
    }

    /// Makes a page from HTML source at `url` (for tests and reference
    /// rendering), fetching its subresources.
    ///
    /// # Errors
    ///
    /// [`LoadError::Cancelled`] only.
    pub fn load_html(&self, url: Url, html: &str) -> Result<Page, LoadError> {
        let parsed = wb_html::parse_document(html, &ParseOptions::default());
        self.build_page(
            url,
            200,
            parsed.document,
            "UTF-8",
            Images::default(),
            &NoObserver,
        )
    }

    /// Fetches images that styles started to refer to since the page
    /// loaded (e.g. a media query that matches only at the new viewport
    /// size). Call it after [`Page::render`] when
    /// [`Page::has_pending_images`] is true. Returns whether any image was
    /// added.
    pub fn load_pending_images(&self, page: &mut Page, observer: &dyn LoadObserver) -> bool {
        let urls = page.take_pending_images();
        if urls.is_empty() {
            return false;
        }
        let before = page.images.by_url.len();
        let page_url = page.url.clone();
        let mut blocked = 0;
        load::load_images(
            &self.inner,
            &mut page.images,
            urls,
            &page_url,
            observer,
            &mut blocked,
        );
        page.blocked_requests += blocked;
        page.invalidate();
        page.images.by_url.len() > before
    }

    fn build_page(
        &self,
        url: Url,
        status: u16,
        doc: Document,
        encoding: &'static str,
        mut images: Images,
        observer: &dyn LoadObserver,
    ) -> Result<Page, LoadError> {
        let inner = &self.inner;
        let base = load::base_url(&doc, &url);
        let mut blocked = 0usize;
        let mut sheets =
            load::load_stylesheets(inner, &doc, &url, &base, encoding, observer, &mut blocked);
        if observer.cancelled() {
            return Err(LoadError::Cancelled);
        }
        if let Some(cosmetic) = load::cosmetic_sheet(inner, &url) {
            sheets.push(cosmetic);
        }
        // Images of <img> elements, then the ones styles refer to at the
        // initial viewport size.
        let elements = load::image_elements(&doc, &base);
        let mut wanted: Vec<Url> = Vec::new();
        for (node, u) in elements {
            images.node_urls.insert(node, u.as_str().to_string());
            wanted.push(u);
        }
        let mut page = Page::new(
            url.clone(),
            status,
            base,
            doc,
            encoding,
            sheets,
            images,
            Arc::clone(&inner.fonts),
            inner.options.list_limits,
        );
        let (w, h) = inner.options.initial_viewport;
        wanted.extend(page.style_image_urls(w as f32, h as f32));
        load::load_images(
            inner,
            &mut page.images,
            wanted,
            &url,
            observer,
            &mut blocked,
        );
        if observer.cancelled() {
            return Err(LoadError::Cancelled);
        }
        page.blocked_requests = blocked;
        observer.progress(LoadStage::Done);
        Ok(page)
    }
}

/// A document showing `text` as preformatted plain text.
fn text_document(text: &str) -> Document {
    let mut doc =
        wb_html::parse_document("<!DOCTYPE html><pre></pre>", &ParseOptions::default()).document;
    let pre = doc
        .descendants(doc.root())
        .find(|&n| doc.is_html_element(n, "pre"));
    if let Some(pre) = pre {
        let t = doc.create_text(text);
        let _ = doc.append_child(pre, t);
    }
    doc
}

/// A document showing the image at `url` (already in the page's images).
fn image_document(url: &str) -> Document {
    let mut doc = wb_html::parse_document(
        "<!DOCTYPE html><style>body{margin:0}img{display:block}</style><img>",
        &ParseOptions::default(),
    )
    .document;
    let img = doc
        .descendants(doc.root())
        .find(|&n| doc.is_html_element(n, "img"));
    if let Some(el) = img.and_then(|n| doc.element_mut(n)) {
        el.set_attribute("src", url);
    }
    doc
}
