//! A loaded page and its render pipeline caches.

use std::fmt;
use std::sync::Arc;

use wb_css::MediaContext;
use wb_dom::{Document, NodeId};
use wb_image::Image;
use wb_layout::{FragmentTree, LayoutHost, LayoutOptions, Size};
use wb_paint::{
    BuildOptions, Color, DisplayList, HitTarget, ListLimits, PaintError, PaintResources, Point,
    Raster, RasterOptions, Rect, build_display_list, hit_test_topmost, rasterize, resolve_hit,
};
use wb_style::{ElementState, SheetInput, StyleOptions, StyledDocument, Stylist};
use wb_text::FontContext;
use wb_url::Url;

use super::load::{self, Images};

/// How to render a page: the layout viewport, the device scale and the
/// scroll position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderOptions {
    /// Viewport width in CSS px (the initial containing block width).
    pub width: f32,
    /// Viewport height in CSS px.
    pub height: f32,
    /// Device pixels per CSS px: the device scale factor times the zoom.
    /// The output is `ceil(width * scale)` by `ceil(height * scale)`.
    pub scale: f32,
    /// The document point (CSS px) drawn at the output's top-left corner.
    pub scroll: Point,
    /// The color behind the page where it paints no canvas background
    /// (CSS Backgrounds 3 §2.11.1 leaves it to the UA; white by default).
    pub background: Color,
}

impl RenderOptions {
    /// A `width` × `height` CSS px viewport at scale 1, unscrolled, on
    /// white.
    #[must_use]
    pub fn new(width: f32, height: f32) -> RenderOptions {
        RenderOptions {
            width,
            height,
            scale: 1.0,
            scroll: Point::new(0.0, 0.0),
            background: Color::WHITE,
        }
    }

    /// The same options at `scale` device px per CSS px.
    #[must_use]
    pub fn with_scale(self, scale: f32) -> RenderOptions {
        RenderOptions { scale, ..self }
    }

    /// The same options scrolled to `(x, y)`.
    #[must_use]
    pub fn with_scroll(self, x: f32, y: f32) -> RenderOptions {
        RenderOptions {
            scroll: Point::new(x, y),
            ..self
        }
    }
}

/// The compiled stylesheets for one media context.
struct StylistCache {
    media: MediaContext,
    stylist: Stylist,
}

/// Layout and display list for one viewport size.
struct Laid {
    width: f32,
    height: f32,
    tree: FragmentTree,
    list: DisplayList,
}

/// A loaded document with its stylesheets and images, ready to render.
///
/// Rendering caches the computed styles, the layout and the display list;
/// [`Page::set_element_state`] and [`Page::document_mut`] invalidate them.
pub struct Page {
    pub(crate) url: Url,
    status: u16,
    base_url: Url,
    document: Document,
    encoding: &'static str,
    title: String,
    sheets: Vec<SheetInput>,
    pub(crate) images: Images,
    fonts: Arc<FontContext>,
    list_limits: ListLimits,
    state: ElementState,
    stylist: Option<StylistCache>,
    styles: Option<StyledDocument>,
    laid: Option<Laid>,
    pending_images: Vec<Url>,
    pub(crate) blocked_requests: usize,
}

impl fmt::Debug for Page {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Page")
            .field("url", &self.url.as_str())
            .field("status", &self.status)
            .field("title", &self.title)
            .field("sheets", &self.sheets.len())
            .field("images", &self.images.by_url.len())
            .finish_non_exhaustive()
    }
}

/// Image sizes for layout and decoded images for paint.
struct Resources<'a> {
    images: &'a Images,
}

fn size_of(img: &Image) -> Size {
    Size::new(img.width() as f32, img.height() as f32)
}

impl LayoutHost for Resources<'_> {
    fn image_size(&self, node: NodeId) -> Option<Size> {
        self.images.node_image(node).map(|i| size_of(i))
    }

    fn url_image_size(&self, url: &str) -> Option<Size> {
        self.images.url_image(url).map(|i| size_of(i))
    }
}

impl PaintResources for Resources<'_> {
    fn node_image(&self, node: NodeId) -> Option<Arc<Image>> {
        self.images.node_image(node).cloned()
    }

    fn url_image(&self, url: &str) -> Option<Arc<Image>> {
        self.images.url_image(url).cloned()
    }
}

impl Page {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        url: Url,
        status: u16,
        base_url: Url,
        document: Document,
        encoding: &'static str,
        sheets: Vec<SheetInput>,
        images: Images,
        fonts: Arc<FontContext>,
        list_limits: ListLimits,
    ) -> Page {
        let title = load::title(&document);
        Page {
            url,
            status,
            base_url,
            document,
            encoding,
            title,
            sheets,
            images,
            fonts,
            list_limits,
            state: ElementState::default(),
            stylist: None,
            styles: None,
            laid: None,
            pending_images: Vec::new(),
            blocked_requests: 0,
        }
    }

    /// The document's URL (the final one, after redirects).
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The document base URL (HTML §4.2.3), for resolving links.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// The HTTP status of the document response (200 for local pages).
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The document's title (`document.title`), possibly empty.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The name of the encoding the document was decoded with.
    #[must_use]
    pub fn encoding(&self) -> &'static str {
        self.encoding
    }

    /// The DOM.
    #[must_use]
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// The DOM, for changes such as form control state. Invalidates the
    /// cached style and layout.
    pub fn document_mut(&mut self) -> &mut Document {
        self.invalidate();
        &mut self.document
    }

    /// The `:hover`, `:active` and `:focus` state.
    #[must_use]
    pub fn element_state(&self) -> ElementState {
        self.state
    }

    /// Changes the `:hover`/`:active`/`:focus` state. Restyles on the next
    /// render only when it actually changed.
    pub fn set_element_state(&mut self, state: ElementState) {
        if state != self.state {
            self.state = state;
            self.invalidate();
        }
    }

    /// How many subresource requests the content blocker stopped.
    #[must_use]
    pub fn blocked_requests(&self) -> usize {
        self.blocked_requests
    }

    /// Whether styles refer to images that were never fetched; see
    /// [`super::Engine::load_pending_images`].
    #[must_use]
    pub fn has_pending_images(&self) -> bool {
        !self.pending_images.is_empty()
    }

    pub(crate) fn take_pending_images(&mut self) -> Vec<Url> {
        std::mem::take(&mut self.pending_images)
    }

    /// Drops the cached styles and layout.
    pub(crate) fn invalidate(&mut self) {
        self.styles = None;
        self.laid = None;
    }

    /// Styles the document for a viewport of `width` × `height` CSS px
    /// at `scale`, reusing the cached stylist and styles when possible.
    fn ensure_styles(&mut self, width: f32, height: f32, scale: f32) {
        let media = MediaContext {
            viewport_width: width,
            viewport_height: height,
            resolution_dppx: scale,
            ..MediaContext::default()
        };
        if self.stylist.as_ref().is_none_or(|s| s.media != media) {
            self.stylist = Some(StylistCache {
                stylist: Stylist::new(&self.sheets, &media),
                media,
            });
            self.styles = None;
            self.laid = None;
        }
        if self.styles.is_some() {
            return;
        }
        let Some(cache) = &self.stylist else { return };
        let options = StyleOptions {
            base_url: Some(self.base_url.clone()),
            state: self.state,
        };
        let styles = cache.stylist.style_document(&self.document, &options);
        for u in load::style_image_urls(&self.document, &styles) {
            if !self.images.by_url.contains_key(&u)
                && let Ok(url) = Url::parse(&u)
                && !self.pending_images.contains(&url)
            {
                self.pending_images.push(url);
            }
        }
        self.styles = Some(styles);
    }

    /// The image URLs styles refer to at this viewport that are not loaded
    /// yet (used while loading).
    pub(crate) fn style_image_urls(&mut self, width: f32, height: f32) -> Vec<Url> {
        self.ensure_styles(width, height, 1.0);
        self.take_pending_images()
    }

    /// Lays the page out for `options` (if not cached) and builds its
    /// display list.
    fn ensure_layout(&mut self, options: &RenderOptions) {
        self.ensure_styles(options.width, options.height, options.scale);
        if self
            .laid
            .as_ref()
            .is_some_and(|l| l.width == options.width && l.height == options.height)
        {
            return;
        }
        let Some(styles) = &self.styles else { return };
        let res = Resources {
            images: &self.images,
        };
        let layout_options = LayoutOptions {
            viewport: Size::new(options.width, options.height),
        };
        let tree =
            wb_layout::layout_document(&self.document, styles, &self.fonts, &res, &layout_options);
        let list = build_display_list(
            &self.document,
            &tree,
            &self.fonts,
            &res,
            &BuildOptions {
                limits: self.list_limits,
            },
        );
        self.laid = Some(Laid {
            width: options.width,
            height: options.height,
            tree,
            list,
        });
    }

    /// Styles and lays out the page for `options` (cached) without
    /// rasterizing, so that hit testing works after a change.
    pub fn layout(&mut self, options: &RenderOptions) {
        if options.width.is_finite()
            && options.height.is_finite()
            && options.width > 0.0
            && options.height > 0.0
            && options.scale.is_finite()
            && options.scale > 0.0
        {
            self.ensure_layout(options);
        }
    }

    /// Lays out the page (cached) and rasterizes the viewport described by
    /// `options` into premultiplied RGBA8 pixels.
    ///
    /// # Errors
    ///
    /// Invalid sizes or scales, or an output beyond the raster limits.
    pub fn render(&mut self, options: &RenderOptions) -> Result<Raster, PaintError> {
        if !(options.width.is_finite()
            && options.height.is_finite()
            && options.width > 0.0
            && options.height > 0.0)
        {
            return Err(PaintError::InvalidViewport);
        }
        if !(options.scale.is_finite() && options.scale > 0.0) {
            return Err(PaintError::InvalidScale);
        }
        self.ensure_layout(options);
        let viewport = Rect::new(
            options.scroll.x,
            options.scroll.y,
            options.width,
            options.height,
        );
        let raster_options = RasterOptions {
            background: options.background,
            ..RasterOptions::new(viewport, options.scale)
        };
        match &self.laid {
            Some(l) => rasterize(&l.list, &raster_options, &*self.fonts),
            None => rasterize(&DisplayList::new(), &raster_options, &*self.fonts),
        }
    }

    /// The size of the document's scrollable area (CSS px) from the last
    /// render, or `None` before the first one.
    #[must_use]
    pub fn content_size(&self) -> Option<Size> {
        let l = self.laid.as_ref()?;
        let r = l.tree.document_overflow();
        Some(Size::new(
            (r.x + r.width).max(l.width),
            (r.y + r.height).max(l.height),
        ))
    }

    /// The fragment tree of the last render.
    #[must_use]
    pub fn fragment_tree(&self) -> Option<&FragmentTree> {
        self.laid.as_ref().map(|l| &l.tree)
    }

    /// The display list of the last render.
    #[must_use]
    pub fn display_list(&self) -> Option<&DisplayList> {
        self.laid.as_ref().map(|l| &l.list)
    }

    /// The computed styles of the last render.
    #[must_use]
    pub fn styles(&self) -> Option<&StyledDocument> {
        self.styles.as_ref()
    }

    /// What is topmost at the document point `point` (CSS px) in the last
    /// render: the fragment, its DOM node and, over text, the offset in
    /// the text node.
    #[must_use]
    pub fn hit_test(&self, point: Point) -> Option<HitTarget> {
        let l = self.laid.as_ref()?;
        let hit = hit_test_topmost(&l.list, point)?;
        resolve_hit(&l.tree, &hit)
    }

    /// The nearest inclusive ancestor of `node` that is a hyperlink (an
    /// HTML `<a>` or `<area>` with `href`, HTML §4.6.1).
    #[must_use]
    pub fn link_element(&self, node: NodeId) -> Option<NodeId> {
        std::iter::once(node)
            .chain(self.document.ancestors(node))
            .find(|&n| {
                self.document.element(n).is_some_and(|e| {
                    (e.is_html("a") || e.is_html("area")) && e.has_attribute("href")
                })
            })
    }

    /// The absolute URL of the link at the document point `point` in the
    /// last render, if any (HTML §4.6.4: `href` parsed against the base
    /// URL).
    #[must_use]
    pub fn link_at(&self, point: Point) -> Option<Url> {
        let node = self.hit_test(point)?.node?;
        let link = self.link_element(node)?;
        let href = self.document.attribute(link, "href")?;
        self.resolve(href)
    }

    /// Parses `input` against the document base URL.
    #[must_use]
    pub fn resolve(&self, input: &str) -> Option<Url> {
        self.base_url
            .join(input.trim_matches(|c: char| c.is_ascii_whitespace()))
            .ok()
    }

    /// The document point to scroll to for the URL fragment `fragment`
    /// (HTML §7.4.6.3 "the indicated part of the document": the element
    /// with that id, else the first `<a name>` with it), from the last
    /// render. `top` (or empty) gives the document's top.
    #[must_use]
    pub fn fragment_position(&self, fragment: &str) -> Option<Point> {
        let decoded = super::fetch::percent_decode(fragment);
        let decoded = String::from_utf8_lossy(&decoded);
        let doc = &self.document;
        let target = doc.element_by_id(&decoded).or_else(|| {
            doc.descendants(doc.root()).find(|&n| {
                doc.is_html_element(n, "a") && doc.attribute(n, "name") == Some(&*decoded)
            })
        });
        let Some(target) = target else {
            if decoded.is_empty() || decoded.eq_ignore_ascii_case("top") {
                return Some(Point::new(0.0, 0.0));
            }
            return None;
        };
        let tree = &self.laid.as_ref()?.tree;
        let first = *tree.fragments_for_node(target).first()?;
        let b = tree.get(first)?.border_box;
        Some(Point::new(b.x, b.y))
    }
}
