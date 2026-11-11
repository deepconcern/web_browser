//! Loading a document's subresources: stylesheets (with `@import`s),
//! cosmetic filters and images, all through the content blocker.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use encoding_rs::{Encoding, UTF_8};
use wb_blocker::ResourceType;
use wb_css::{
    Image as CssImage, MediaQueryList, Stylesheet, parse_selector_list, parse_stylesheet,
};
use wb_dom::{Document, NodeId, QuirksMode};
use wb_image::Image;
use wb_net::{NetError, Request, Response};
use wb_style::{
    Content, ContentItem, Origin, PseudoElement, SheetInput, StyleSourceKind, StyledDocument,
    document_style_sources,
};
use wb_url::Url;

use super::{Inner, LoadObserver, LoadStage, mime};

/// Decoded images of a page, keyed by absolute URL.
#[derive(Debug, Default)]
pub(crate) struct Images {
    /// `None` when the fetch or decode failed, or the request was blocked.
    pub(crate) by_url: HashMap<String, Option<Arc<Image>>>,
    /// The image URL of each `<img>` / `<input type=image>`.
    pub(crate) node_urls: HashMap<NodeId, String>,
    /// Decoded bytes held so far.
    pub(crate) decoded_bytes: usize,
}

impl Images {
    pub(crate) fn node_image(&self, node: NodeId) -> Option<&Arc<Image>> {
        let url = self.node_urls.get(&node)?;
        self.by_url.get(url)?.as_ref()
    }

    pub(crate) fn url_image(&self, url: &str) -> Option<&Arc<Image>> {
        self.by_url.get(url)?.as_ref()
    }
}

/// A subresource request the engine wants to make, before the blocker.
struct Wanted {
    url: Url,
    kind: ResourceType,
}

impl Inner {
    /// Whether the blocker blocks `url` (of type `kind`) requested by the
    /// page at `page`. Returns the matching filter when blocked.
    pub(crate) fn blocked(&self, url: &Url, page: &Url, kind: ResourceType) -> Option<String> {
        let guard = self.blocker.read().unwrap_or_else(|e| e.into_inner());
        let engine = guard.as_ref()?;
        let same_site = url.site().same_site(&page.site());
        let req = wb_blocker::Request::new(url.as_str(), page.as_str(), kind, same_site);
        match engine.check(&req) {
            wb_blocker::Verdict::Blocked { filter } => Some(filter.to_string()),
            _ => None,
        }
    }

    /// Fetches `wanted` as subresources of `page`, skipping blocked ones
    /// (`None` result, counted in `blocked`).
    fn fetch_subresources(
        &self,
        wanted: Vec<Wanted>,
        page: &Url,
        observer: &dyn LoadObserver,
        blocked: &mut usize,
    ) -> Vec<Option<Response>> {
        let mut slots: Vec<Option<usize>> = Vec::with_capacity(wanted.len());
        let mut requests = Vec::new();
        for w in wanted {
            if self.blocked(&w.url, page, w.kind).is_some() {
                *blocked += 1;
                slots.push(None);
                continue;
            }
            slots.push(Some(requests.len()));
            requests.push(Request::subresource(w.url, page));
        }
        let cancelled = || observer.cancelled();
        let mut results: Vec<Option<Result<Response, NetError>>> = self
            .fetcher
            .fetch_all(requests, &cancelled)
            .into_iter()
            .map(Some)
            .collect();
        slots
            .into_iter()
            .map(|slot| {
                let r = results.get_mut(slot?)?.take()?;
                r.ok().filter(Response::ok)
            })
            .collect()
    }
}

/// HTML §4.2.3: the document base URL is the frozen base URL of the first
/// `<base href>` in tree order, or the document's URL.
pub(crate) fn base_url(doc: &Document, url: &Url) -> Url {
    doc.descendants(doc.root())
        .find_map(|n| {
            let el = doc.element(n)?;
            if !el.is_html("base") {
                return None;
            }
            el.attribute("href")
        })
        .and_then(|href| url.join(href).ok())
        .unwrap_or_else(|| url.clone())
}

/// HTML §3.1.3 `document.title`: the child text of the first `<title>`,
/// with ASCII whitespace stripped and collapsed.
pub(crate) fn title(doc: &Document) -> String {
    let Some(t) = doc
        .descendants(doc.root())
        .find(|&n| doc.is_html_element(n, "title"))
    else {
        return String::new();
    };
    let mut text = String::new();
    for c in doc.children(t) {
        if let Some(s) = doc.text(c) {
            text.push_str(s);
        }
    }
    text.split(|c: char| c.is_ascii_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// CSS Syntax 3 §3.2: decodes stylesheet bytes. The fallback encoding is
/// the `Content-Type` charset, then `@charset`, then `environment` (the
/// referring document's encoding), then UTF-8; a BOM overrides all of them.
pub(crate) fn decode_css(bytes: &[u8], content_type: Option<&str>, environment: &str) -> String {
    let from_label = |label: &str| Encoding::for_label(label.trim().as_bytes());
    let charset = content_type
        .and_then(mime::parse)
        .and_then(|m| m.charset)
        .and_then(|l| from_label(&l));
    let fallback = charset
        .or_else(|| {
            wb_css::charset_from_bytes(bytes)
                .and_then(from_label)
                .map(|e| {
                    // "If the return value was utf-16be or utf-16le, use utf-8."
                    if e == encoding_rs::UTF_16BE || e == encoding_rs::UTF_16LE {
                        UTF_8
                    } else {
                        e
                    }
                })
        })
        .or_else(|| from_label(environment))
        .unwrap_or(UTF_8);
    let (text, _, _) = fallback.decode(bytes);
    text.into_owned()
}

/// Whether a fetched stylesheet may be applied (HTML §4.6.7.17 and CSS
/// Cascade 5 §2.1): its type must be `text/css`, except that a quirks-mode
/// document accepts any type from its own origin.
fn css_type_ok(resp: &Response, doc_url: &Url, quirks: bool) -> bool {
    let is_css = resp
        .content_type()
        .and_then(mime::parse)
        .is_some_and(|m| m.essence == "text/css");
    is_css || (quirks && resp.url.origin().same_origin(&doc_url.origin()))
}

/// A fetched, parsed stylesheet.
#[derive(Clone)]
struct Fetched {
    /// The response URL, for resolving the sheet's own URLs.
    url: Url,
    sheet: Arc<Stylesheet>,
}

/// Fetches the document's stylesheets and their imports, level by level,
/// and returns the author sheets in tree order. `blocked` counts requests
/// the blocker stopped.
pub(crate) fn load_stylesheets(
    inner: &Inner,
    doc: &Document,
    doc_url: &Url,
    base: &Url,
    encoding: &str,
    observer: &dyn LoadObserver,
    blocked: &mut usize,
) -> Vec<SheetInput> {
    let max = inner.options.max_stylesheets;
    let sources = document_style_sources(doc);

    // Inline sheets are parsed now; linked ones and every import are
    // fetched below.
    enum Top {
        Inline(Arc<Stylesheet>),
        External(Option<Url>),
    }
    let mut tops = Vec::new();
    let mut first_level: Vec<Url> = Vec::new();
    let want = |u: Url, list: &mut Vec<Url>| {
        if !list.contains(&u) {
            list.push(u);
        }
    };
    for s in sources.iter().take(max) {
        match &s.kind {
            StyleSourceKind::Inline(text) => {
                let sheet = Arc::new(parse_stylesheet(text));
                for imp in sheet.imports() {
                    if let Ok(u) = base.join(&imp.url) {
                        want(u.without_fragment(), &mut first_level);
                    }
                }
                tops.push((Top::Inline(sheet), s.media.clone()));
            }
            StyleSourceKind::External { href } => {
                let url = base.join(href).ok().map(|u| u.without_fragment());
                if let Some(u) = &url {
                    want(u.clone(), &mut first_level);
                }
                tops.push((Top::External(url), s.media.clone()));
            }
        }
    }

    let cache = fetch_sheet_levels(
        inner,
        doc,
        doc_url,
        encoding,
        first_level,
        observer,
        blocked,
    );

    let mut budget = max;
    let mut out = Vec::new();
    for (top, media) in tops {
        let (sheet, url) = match top {
            Top::Inline(sheet) => (sheet, base.clone()),
            Top::External(Some(u)) => match cache.get(&u).cloned().flatten() {
                Some(f) => (f.sheet, f.url),
                None => continue,
            },
            Top::External(None) => continue,
        };
        if budget == 0 {
            break;
        }
        budget -= 1;
        let mut chain = vec![url.clone()];
        let imports = build_imports(&sheet, &url, &cache, &mut chain, 1, inner, &mut budget);
        out.push(SheetInput {
            sheet,
            origin: Origin::Author,
            media,
            base_url: Some(url),
            imports,
        });
    }
    out
}

/// Fetches `to_fetch` and then their imports, breadth first, so the
/// requests of one level run concurrently. At most
/// [`super::EngineOptions::max_stylesheets`] sheets are fetched, at most
/// [`super::EngineOptions::max_import_depth`] levels deep; each URL once.
fn fetch_sheet_levels(
    inner: &Inner,
    doc: &Document,
    doc_url: &Url,
    encoding: &str,
    mut to_fetch: Vec<Url>,
    observer: &dyn LoadObserver,
    blocked: &mut usize,
) -> HashMap<Url, Option<Fetched>> {
    let quirks = doc.quirks_mode() == QuirksMode::Quirks;
    let mut cache: HashMap<Url, Option<Fetched>> = HashMap::new();
    let mut fetched = 0usize;
    for _ in 0..=inner.options.max_import_depth {
        to_fetch.retain(|u| !cache.contains_key(u));
        to_fetch.truncate(inner.options.max_stylesheets.saturating_sub(fetched));
        if to_fetch.is_empty() || observer.cancelled() {
            break;
        }
        fetched += to_fetch.len();
        observer.progress(LoadStage::Stylesheets { requested: fetched });
        let wanted = to_fetch
            .iter()
            .map(|u| Wanted {
                url: u.clone(),
                kind: ResourceType::Stylesheet,
            })
            .collect();
        let responses = inner.fetch_subresources(wanted, doc_url, observer, blocked);
        let mut next: Vec<Url> = Vec::new();
        for (url, resp) in to_fetch.drain(..).zip(responses) {
            let parsed = resp.filter(|r| css_type_ok(r, doc_url, quirks)).map(|r| {
                let text = decode_css(&r.body, r.content_type(), encoding);
                Fetched {
                    url: r.url.clone(),
                    sheet: Arc::new(parse_stylesheet(&text)),
                }
            });
            if let Some(f) = &parsed {
                for imp in f.sheet.imports() {
                    if let Ok(u) = f.url.join(&imp.url) {
                        let u = u.without_fragment();
                        if !next.contains(&u) {
                            next.push(u);
                        }
                    }
                }
            }
            cache.insert(url, parsed);
        }
        to_fetch = next;
    }
    cache
}

/// Builds the [`SheetInput::imports`] of `sheet` from the fetched cache.
/// `chain` holds the URLs of the sheets above it, to skip import cycles;
/// `budget` caps the total number of sheet inputs built.
fn build_imports(
    sheet: &Stylesheet,
    url: &Url,
    cache: &HashMap<Url, Option<Fetched>>,
    chain: &mut Vec<Url>,
    depth: usize,
    inner: &Inner,
    budget: &mut usize,
) -> Vec<Option<SheetInput>> {
    let mut out = Vec::new();
    for imp in sheet.imports() {
        let entry = url
            .join(&imp.url)
            .ok()
            .map(|u| u.without_fragment())
            .filter(|u| !chain.contains(u) && depth <= inner.options.max_import_depth)
            .and_then(|u| cache.get(&u).cloned().flatten());
        let Some(f) = entry else {
            out.push(None);
            continue;
        };
        if *budget == 0 {
            out.push(None);
            continue;
        }
        *budget -= 1;
        chain.push(f.url.clone());
        let imports = build_imports(&f.sheet, &f.url, cache, chain, depth + 1, inner, budget);
        chain.pop();
        out.push(Some(SheetInput {
            sheet: f.sheet,
            origin: Origin::Author,
            media: MediaQueryList::default(),
            base_url: Some(f.url),
            imports,
        }));
    }
    out
}

/// The element-hiding sheet for `page`: every cosmetic selector that
/// parses, as `display: none !important` in the user origin (so it beats
/// author `!important` declarations, CSS Cascade 5 §6.2).
pub(crate) fn cosmetic_sheet(inner: &Inner, page: &Url) -> Option<SheetInput> {
    let guard = inner.blocker.read().unwrap_or_else(|e| e.into_inner());
    let engine = guard.as_ref()?;
    let selectors = engine.cosmetic_selectors(page.as_str());
    let mut css = String::new();
    let mut count = 0usize;
    for sel in selectors.generic.iter().chain(selectors.specific.iter()) {
        if count >= inner.options.max_cosmetic_selectors {
            break;
        }
        // Each selector is checked on its own so one bad selector can't
        // invalidate a whole rule, and so `{`, `}` or `;` in filter text
        // can't inject declarations: the text must parse as a selector list.
        if sel.contains(['{', '}', ';']) || parse_selector_list(sel).is_err() {
            continue;
        }
        css.push_str(sel);
        css.push_str("{display:none!important}\n");
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let mut input = SheetInput::author(parse_stylesheet(&css));
    input.origin = Origin::User;
    Some(input)
}

/// The URL each `<img src>` and `<input type=image src>` shows (HTML
/// §4.8.4.3 "update the image data", without `srcset` in M1).
pub(crate) fn image_elements(doc: &Document, base: &Url) -> Vec<(NodeId, Url)> {
    let mut out = Vec::new();
    for n in doc.descendants(doc.root()) {
        let Some(el) = doc.element(n) else { continue };
        let is_img = el.is_html("img")
            || (el.is_html("input")
                && el
                    .attribute("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("image")));
        if !is_img {
            continue;
        }
        // M1 follow-up: `srcset`, `sizes` and `<picture>` source selection.
        let Some(src) = el.attribute("src") else {
            continue;
        };
        let src = src.trim_matches(|c: char| c.is_ascii_whitespace());
        if src.is_empty() {
            continue;
        }
        if let Ok(u) = base.join(src) {
            out.push((n, u));
        }
    }
    out
}

/// The image URLs computed styles refer to: background layers, list-style
/// images and `content: url()`, of elements and their pseudo-elements.
pub(crate) fn style_image_urls(doc: &Document, styles: &StyledDocument) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut add = |img: &CssImage| {
        if let CssImage::Url(u) = img
            && !u.is_empty()
            && seen.insert(u.clone())
        {
            out.push(u.clone());
        }
    };
    for n in doc.descendants(doc.root()) {
        if !doc.is_element(n) {
            continue;
        }
        let base = styles.style(n);
        let pseudos = [
            PseudoElement::Before,
            PseudoElement::After,
            PseudoElement::Marker,
        ]
        .map(|p| styles.pseudo_style(n, p));
        for s in std::iter::once(base).chain(pseudos).flatten() {
            for layer in s.background_layers.iter() {
                add(&layer.image);
            }
            add(&s.list_style_image);
            if let Content::Items(items) = &s.content {
                for item in items {
                    if let ContentItem::Url(u) = item {
                        add(&CssImage::Url(u.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Fetches and decodes the images at `urls` that `images` doesn't have
/// yet, within the engine's image limits.
pub(crate) fn load_images(
    inner: &Inner,
    images: &mut Images,
    urls: Vec<Url>,
    page: &Url,
    observer: &dyn LoadObserver,
    blocked: &mut usize,
) {
    let mut wanted = Vec::new();
    let mut keys = Vec::new();
    for u in urls {
        let key = u.as_str().to_string();
        if images.by_url.contains_key(&key) || keys.contains(&key) {
            continue;
        }
        if images.by_url.len() + keys.len() >= inner.options.max_images {
            break;
        }
        keys.push(key);
        wanted.push(Wanted {
            url: u,
            kind: ResourceType::Image,
        });
    }
    if wanted.is_empty() {
        return;
    }
    observer.progress(LoadStage::Images {
        requested: wanted.len(),
    });
    let responses = inner.fetch_subresources(wanted, page, observer, blocked);
    for (key, resp) in keys.into_iter().zip(responses) {
        match resp {
            Some(r) => load_images_from_bytes(inner, images, &key, &r.body),
            None => {
                images.by_url.insert(key, None);
            }
        }
    }
}

/// Decodes `bytes` as the image at `url` within the page's remaining image
/// budget and stores the result (a failed decode is stored as `None`).
pub(crate) fn load_images_from_bytes(inner: &Inner, images: &mut Images, url: &str, bytes: &[u8]) {
    let remaining = inner
        .options
        .max_image_bytes
        .saturating_sub(images.decoded_bytes);
    let limits = wb_image::Limits::new(
        inner.options.image_limits.max_dimension(),
        inner.options.image_limits.max_bytes().min(remaining),
    );
    let image = wb_image::decode_with_limits(bytes, &limits).ok();
    if let Some(img) = &image {
        images.decoded_bytes = images.decoded_bytes.saturating_add(img.pixels().len());
    }
    images.by_url.insert(url.to_string(), image.map(Arc::new));
}
