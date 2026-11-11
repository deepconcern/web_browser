//! The content side of a tab: a thread that owns the tab's [`Page`],
//! loads pages, renders frames and handles input, talking to the UI only
//! through `wb-ipc` messages. Loading and rendering never run on the UI
//! thread.

use std::thread::JoinHandle;

use wb_dom::NodeId;
use wb_ipc::{
    ContentEnd, CursorKind, Frame, InputEvent, Key, LoadFailure, LoadProgress, NavigationMethod,
    NavigationRequest, PointerButton, Received, ToBrowser, ToContent, Viewport,
};
use wb_net::{CacheMode, NetError, Request, RequestContext};
use wb_url::Url;

use crate::engine::{
    ElementState, Engine, LoadError, LoadObserver, LoadStage, Page, Point, RenderOptions,
};
use crate::forms::{self, Activation, FormState};
use crate::selection::{self, Caret, Selection, TreeOrder};

/// Pixels scrolled by an arrow key.
const LINE_SCROLL: f32 = 40.0;

/// Starts the content thread for one tab.
///
/// # Errors
///
/// When the OS can't start a thread.
pub fn spawn(
    engine: Engine,
    end: ContentEnd,
    viewport: Viewport,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("content".into())
        .spawn(move || ContentTab::new(engine, end, viewport).run())
}

/// Reports load progress to the UI and polls the cancel flag.
struct Observer<'a> {
    end: &'a ContentEnd,
}

impl LoadObserver for Observer<'_> {
    fn cancelled(&self) -> bool {
        self.end.cancel_flag().is_cancelled()
    }

    fn progress(&self, stage: LoadStage) {
        let p = match stage {
            LoadStage::Document => LoadProgress::Document,
            LoadStage::Stylesheets { .. } => LoadProgress::Stylesheets,
            LoadStage::Images { .. } => LoadProgress::Images,
            LoadStage::Done => return,
        };
        self.end.send(ToBrowser::LoadProgress(p));
    }
}

/// A tab's content state.
pub struct ContentTab {
    engine: Engine,
    end: ContentEnd,
    page: Option<Page>,
    viewport: Viewport,
    scroll: (f32, f32),
    forms: FormState,
    focus: Option<NodeId>,
    selection: Option<Selection>,
    selecting: bool,
    pressed: Option<NodeId>,
    open_select: Option<NodeId>,
    hover: Option<NodeId>,
    hover_link: Option<Url>,
    cursor: CursorKind,
    /// The last frame's pixels, reused when only overlays change.
    last: Option<Frame>,
    needs_render: bool,
    needs_overlay: bool,
}

impl std::fmt::Debug for ContentTab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentTab")
            .field("page", &self.page)
            .field("scroll", &self.scroll)
            .finish_non_exhaustive()
    }
}

impl ContentTab {
    /// A tab with no page yet.
    #[must_use]
    pub fn new(engine: Engine, end: ContentEnd, viewport: Viewport) -> ContentTab {
        ContentTab {
            engine,
            end,
            page: None,
            viewport,
            scroll: (0.0, 0.0),
            forms: FormState::default(),
            focus: None,
            selection: None,
            selecting: false,
            pressed: None,
            open_select: None,
            hover: None,
            hover_link: None,
            cursor: CursorKind::Default,
            last: None,
            needs_render: false,
            needs_overlay: false,
        }
    }

    /// Handles messages until [`ToContent::Shutdown`] or the UI goes away.
    /// Messages that queued up while busy are handled together and render
    /// once.
    pub fn run(mut self) {
        while let Some(first) = self.end.recv() {
            let mut batch = vec![first];
            loop {
                match self.end.try_recv() {
                    Received::Message(m) => batch.push(m),
                    Received::Timeout => break,
                    Received::Disconnected => return,
                }
            }
            for msg in batch {
                if !self.handle(msg) {
                    return;
                }
            }
            self.flush();
        }
    }

    /// Handles one message; returns `false` to stop.
    pub fn handle(&mut self, msg: ToContent) -> bool {
        match msg {
            ToContent::Shutdown => return false,
            ToContent::Navigate(req) => self.navigate(req),
            ToContent::Viewport(v) => {
                if v != self.viewport {
                    self.viewport = v;
                    self.needs_render = true;
                }
            }
            ToContent::ScrollBy { dx, dy } => {
                self.scroll_to(self.scroll.0 + dx, self.scroll.1 + dy)
            }
            ToContent::ScrollTo { x, y } => self.scroll_to(x, y),
            ToContent::Input(e) => self.input(e),
            ToContent::CopySelection => {
                let text = self.selected_text();
                self.end.send(ToBrowser::Copied(text));
            }
            ToContent::SelectAll => {
                if let Some(p) = &self.page {
                    self.selection = Selection::all(p.document());
                    self.needs_overlay = true;
                }
            }
            ToContent::ChooseOption(choice) => {
                if let (Some(select), Some(i), Some(page)) =
                    (self.open_select.take(), choice, self.page.as_mut())
                {
                    self.forms.select_option(page.document_mut(), select, i);
                    self.needs_render = true;
                }
                self.open_select = None;
            }
        }
        true
    }

    /// Renders and sends a frame if anything changed.
    pub fn flush(&mut self) {
        if self.needs_render {
            self.render();
        } else if self.needs_overlay {
            self.send_overlay_only();
        }
        self.needs_render = false;
        self.needs_overlay = false;
    }

    fn css_viewport(&self) -> (f32, f32) {
        self.viewport.css_size()
    }

    fn scroll_to(&mut self, x: f32, y: f32) {
        let (vw, vh) = self.css_viewport();
        let (cw, ch) = self
            .page
            .as_ref()
            .and_then(Page::content_size)
            .map_or((vw, vh), |s| (s.width, s.height));
        let clamp = |v: f32, max: f32| {
            if v.is_finite() {
                v.clamp(0.0, max.max(0.0))
            } else {
                0.0
            }
        };
        let new = (clamp(x, cw - vw).round(), clamp(y, ch - vh).round());
        if new != self.scroll {
            self.scroll = new;
            self.needs_render = true;
        }
    }

    fn navigate(&mut self, req: NavigationRequest) {
        let end = &self.end;
        // URLs cross the channel serialized; re-parse them here.
        let Ok(url) = Url::parse(&req.url) else {
            end.send(ToBrowser::LoadFailed {
                url: req.url,
                failure: LoadFailure::Network("The address isn't a valid URL.".into()),
            });
            return;
        };
        let initiator = req.initiator.as_deref().and_then(|u| Url::parse(u).ok());
        // A fragment navigation within the current document only scrolls
        // (HTML §7.4.2.3.3 "navigate to a fragment").
        if let Some(page) = &self.page
            && req.method == NavigationMethod::Get
            && !req.reload
            && url.fragment().is_some()
            && url.without_fragment() == page.url().without_fragment()
        {
            let pos = page.fragment_position(url.fragment().unwrap_or(""));
            end.send(ToBrowser::LoadCommitted {
                url: req.url.clone(),
                title: page.title().to_string(),
                blocked_requests: page.blocked_requests(),
            });
            if let Some(p) = pos {
                self.scroll_to(p.x, p.y);
            }
            return;
        }
        end.cancel_flag().reset();
        end.send(ToBrowser::LoadStarted {
            url: req.url.clone(),
        });
        let mut request = Request::with_context(
            url.clone(),
            RequestContext::Navigation {
                initiator: initiator.as_ref().map(Url::site),
            },
        );
        request.referrer = initiator;
        if req.reload {
            request.cache_mode = CacheMode::NoCache;
        }
        if let NavigationMethod::Post { content_type, body } = &req.method {
            request.method = wb_net::Method::Post;
            request.headers.set("content-type", content_type);
            request.body = body.clone();
        }
        let observer = Observer { end };
        match self.engine.load_request(request, &observer) {
            Ok(page) => {
                end.send(ToBrowser::LoadCommitted {
                    url: page.url().to_string(),
                    title: page.title().to_string(),
                    blocked_requests: page.blocked_requests(),
                });
                self.page = Some(page);
                self.forms = FormState::default();
                self.focus = None;
                self.selection = None;
                self.selecting = false;
                self.pressed = None;
                self.open_select = None;
                self.hover = None;
                self.scroll = (0.0, 0.0);
                // Lay out once so fragment and scroll restoration work.
                self.render();
                let target = match (req.restore_scroll, url.fragment()) {
                    (Some((x, y)), _) => Some((x, y)),
                    (None, Some(f)) => self
                        .page
                        .as_ref()
                        .and_then(|p| p.fragment_position(f))
                        .map(|p| (p.x, p.y)),
                    _ => None,
                };
                if let Some((x, y)) = target {
                    self.scroll_to(x, y);
                }
                self.needs_render = self.needs_render || target.is_some();
            }
            Err(LoadError::Cancelled) => {
                end.send(ToBrowser::LoadCancelled);
            }
            Err(e) => {
                let failure = match e {
                    LoadError::Net(NetError::HttpsUpgradeFailed(f)) => {
                        LoadFailure::HttpsUpgradeFailed {
                            site: f.site.to_string(),
                            host: f.site.host().map(ToString::to_string).unwrap_or_default(),
                            reason: f.reason.clone(),
                        }
                    }
                    LoadError::Net(e) => LoadFailure::Network(e.to_string()),
                    LoadError::Blocked { filter } => LoadFailure::Blocked { filter },
                    LoadError::UnsupportedContentType(t) => LoadFailure::UnsupportedContentType(t),
                    LoadError::Cancelled => LoadFailure::Network("cancelled".into()),
                };
                end.send(ToBrowser::LoadFailed {
                    url: req.url,
                    failure,
                });
            }
        }
    }

    fn render_options(&self) -> RenderOptions {
        let (w, h) = self.css_viewport();
        RenderOptions::new(w.max(1.0), h.max(1.0))
            .with_scale(self.viewport.scale())
            .with_scroll(self.scroll.0, self.scroll.1)
    }

    fn render(&mut self) {
        let mut options = self.render_options();
        let Some(page) = self.page.as_mut() else {
            return;
        };
        let mut raster = page.render(&options);
        // Re-clamp the scroll position now the content size is known.
        if let Some(size) = page.content_size() {
            let (vw, vh) = (options.width, options.height);
            let x = self.scroll.0.clamp(0.0, (size.width - vw).max(0.0));
            let y = self.scroll.1.clamp(0.0, (size.height - vh).max(0.0));
            if (x, y) != self.scroll {
                self.scroll = (x, y);
                options = options.with_scroll(x, y);
                raster = page.render(&options);
            }
        }
        if page.has_pending_images()
            && self
                .engine
                .load_pending_images(page, &Observer { end: &self.end })
        {
            raster = page.render(&options);
        }
        let Ok(raster) = raster else { return };
        let content = page
            .content_size()
            .map_or((options.width, options.height), |s| (s.width, s.height));
        let frame = Frame {
            width: raster.width(),
            height: raster.height(),
            pixels: std::sync::Arc::new(raster.into_pixels()),
            scroll: self.scroll,
            content_size: content,
            viewport: self.viewport,
            highlights: self.highlights(),
            focus_ring: self.focus_ring(),
        };
        self.last = Some(frame.clone());
        self.end.send(ToBrowser::Frame(frame));
    }

    fn send_overlay_only(&mut self) {
        let highlights = self.highlights();
        let focus_ring = self.focus_ring();
        if let Some(last) = &mut self.last {
            last.highlights = highlights;
            last.focus_ring = focus_ring;
            let f = last.clone();
            self.end.send(ToBrowser::Frame(f));
        }
    }

    fn highlights(&self) -> Vec<[f32; 4]> {
        let (Some(page), Some(sel)) = (&self.page, &self.selection) else {
            return Vec::new();
        };
        let Some(tree) = page.fragment_tree() else {
            return Vec::new();
        };
        let order = TreeOrder::new(page.document());
        selection::highlight_rects(tree, &order, sel)
            .into_iter()
            .map(|(x, y, w, h)| [x, y, w, h])
            .collect()
    }

    fn node_rect(&self, node: NodeId) -> Option<[f32; 4]> {
        let tree = self.page.as_ref()?.fragment_tree()?;
        let id = *tree.fragments_for_node(node).first()?;
        let b = tree.get(id)?.border_box;
        Some([b.x, b.y, b.width, b.height])
    }

    fn focus_ring(&self) -> Option<[f32; 4]> {
        self.node_rect(self.focus?)
    }

    fn selected_text(&self) -> String {
        let (Some(page), Some(sel)) = (&self.page, &self.selection) else {
            return String::new();
        };
        let Some(tree) = page.fragment_tree() else {
            return String::new();
        };
        selection::selected_text(tree, &TreeOrder::new(page.document()), sel)
    }

    fn doc_point(&self, x: f32, y: f32) -> Point {
        Point::new(x + self.scroll.0, y + self.scroll.1)
    }

    fn input(&mut self, e: InputEvent) {
        // Earlier messages in this batch may have changed the document;
        // hit testing needs a current layout.
        let options = self.render_options();
        if let Some(p) = self.page.as_mut() {
            p.layout(&options);
        }
        match e {
            InputEvent::PointerMove { x, y } => self.pointer_move(x, y),
            InputEvent::PointerLeave => {
                self.set_hover(None);
            }
            InputEvent::PointerDown {
                x,
                y,
                button,
                modifiers,
                ..
            } => {
                let p = self.doc_point(x, y);
                let Some(page) = &self.page else { return };
                let hit = page.hit_test(p);
                self.pressed = hit.and_then(|h| h.node);
                if button != PointerButton::Primary || modifiers.ctrl {
                    return;
                }
                let target = self.pressed.map(|n| element_of(page, n));
                if self.focus.is_some() && self.focus != target {
                    self.focus = None;
                    self.needs_overlay = true;
                }
                // Start a selection on text that is not inside a link.
                let caret = hit.and_then(|h| {
                    Some(Caret {
                        node: h.node?,
                        offset: h.text_offset?,
                    })
                });
                let in_link = self.pressed.and_then(|n| page.link_element(n)).is_some();
                if let Some(c) = caret
                    && !in_link
                {
                    if modifiers.shift
                        && let Some(sel) = &mut self.selection
                    {
                        sel.focus = c;
                    } else {
                        self.selection = Some(Selection {
                            anchor: c,
                            focus: c,
                        });
                    }
                    self.selecting = true;
                    self.needs_overlay = true;
                } else if self.selection.take().is_some() {
                    self.needs_overlay = true;
                }
            }
            InputEvent::PointerUp {
                x,
                y,
                button,
                modifiers,
            } => {
                self.selecting = false;
                let p = self.doc_point(x, y);
                let Some(pressed) = self.pressed.take() else {
                    return;
                };
                let Some(page) = &self.page else { return };
                let Some(now) = page.hit_test(p).and_then(|h| h.node) else {
                    return;
                };
                if now != pressed || self.selection.is_some_and(|s| !s.is_collapsed()) {
                    return;
                }
                self.click(now, button, modifiers);
            }
            InputEvent::KeyDown { key, modifiers } => self.key(key, modifiers),
            InputEvent::Text(t) => {
                let Some(focus) = self.focus else { return };
                let Some(page) = self.page.as_mut() else {
                    return;
                };
                let mut value = forms::text_value(page.document(), focus);
                let typed: String = t.chars().filter(|c| !c.is_control()).collect();
                if typed.is_empty() {
                    return;
                }
                value.push_str(&typed);
                self.forms
                    .set_text_value(page.document_mut(), focus, &value);
                self.needs_render = true;
            }
        }
    }

    fn pointer_move(&mut self, x: f32, y: f32) {
        let p = self.doc_point(x, y);
        let Some(page) = &self.page else { return };
        let hit = page.hit_test(p);
        if self.selecting {
            if let (Some(sel), Some(h)) = (&mut self.selection, hit)
                && let (Some(node), Some(offset)) = (h.node, h.text_offset)
            {
                let c = Caret { node, offset };
                if sel.focus != c {
                    sel.focus = c;
                    self.needs_overlay = true;
                }
            }
            return;
        }
        let node = hit.and_then(|h| h.node);
        let link = node
            .and_then(|n| page.link_element(n))
            .and_then(|l| page.document().attribute(l, "href"))
            .and_then(|href| page.resolve(href));
        let cursor = match node {
            _ if link.is_some() => CursorKind::Pointer,
            Some(n) => {
                let el = element_of(page, n);
                let doc = page.document();
                if forms::is_text_control(doc, el) {
                    CursorKind::Text
                } else if forms::is_submit_button(doc, el)
                    || doc.is_html_element(el, "select")
                    || doc.is_html_element(el, "label")
                    || (doc.is_html_element(el, "input")
                        && matches!(
                            forms::input_type(doc, el).as_str(),
                            "checkbox" | "radio" | "reset" | "button"
                        ))
                {
                    CursorKind::Pointer
                } else if hit.is_some_and(|h| h.text_offset.is_some()) {
                    CursorKind::Text
                } else {
                    CursorKind::Default
                }
            }
            None => CursorKind::Default,
        };
        if link != self.hover_link {
            self.hover_link = link.clone();
            self.end
                .send(ToBrowser::HoverLink(link.map(|l| l.to_string())));
        }
        if cursor != self.cursor {
            self.cursor = cursor;
            self.end.send(ToBrowser::Cursor(cursor));
        }
        let element = node.map(|n| element_of(page, n));
        self.set_hover(element);
    }

    fn set_hover(&mut self, element: Option<NodeId>) {
        if element == self.hover {
            return;
        }
        self.hover = element;
        if let Some(page) = &mut self.page {
            let state = ElementState {
                hover: element,
                focus: self.focus,
                ..page.element_state()
            };
            page.set_element_state(state);
            self.needs_render = true;
        }
    }

    fn click(&mut self, node: NodeId, button: PointerButton, modifiers: wb_ipc::Modifiers) {
        let Some(page) = self.page.as_mut() else {
            return;
        };
        if let Some(link) = page.link_element(node)
            && let Some(url) = page
                .document()
                .attribute(link, "href")
                .and_then(|h| page.resolve(h))
        {
            let new_tab = button == PointerButton::Middle || modifiers.ctrl || modifiers.meta;
            let mut req = NavigationRequest::user(url);
            req.initiator = Some(page.url().to_string());
            req.new_tab = new_tab;
            self.end.send(ToBrowser::Navigate(req));
            return;
        }
        if button != PointerButton::Primary {
            return;
        }
        let doc_url = page.url().clone();
        let base = page.base_url().clone();
        let el = element_of(page, node);
        let activation = self
            .forms
            .activate(page.document_mut(), el, &doc_url, &base);
        match activation {
            Activation::None => {
                // `activate` may have mutated nothing; the cache was
                // invalidated by `document_mut`, so repaint anyway.
                self.needs_render = true;
            }
            Activation::Changed => self.needs_render = true,
            Activation::Focus(n) => {
                self.focus = Some(n);
                self.needs_render = true;
            }
            Activation::OpenSelect(s) => {
                self.needs_render = true;
                let doc = page.document();
                let options = forms::select_options(doc, s);
                let selected = forms::selected_options(doc, s);
                let labels = options
                    .iter()
                    .map(|&o| forms::option_label(doc, o))
                    .collect();
                let selected = options
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| selected.contains(o))
                    .map(|(i, _)| i)
                    .collect();
                if let Some(r) = self.node_rect(s) {
                    self.open_select = Some(s);
                    self.end.send(ToBrowser::OpenSelect {
                        rect: [r[0] - self.scroll.0, r[1] - self.scroll.1, r[2], r[3]],
                        options: labels,
                        selected,
                    });
                }
            }
            Activation::Submit(req) => {
                self.needs_render = true;
                self.end.send(ToBrowser::Navigate(req));
            }
        }
    }

    fn key(&mut self, key: Key, modifiers: wb_ipc::Modifiers) {
        if let (Some(focus), Some(page)) = (self.focus, self.page.as_mut()) {
            let is_textarea = page.document().is_html_element(focus, "textarea");
            match key {
                Key::Backspace => {
                    let mut v = forms::text_value(page.document(), focus);
                    if v.pop().is_some() {
                        self.forms.set_text_value(page.document_mut(), focus, &v);
                        self.needs_render = true;
                    }
                    return;
                }
                Key::Enter if is_textarea => {
                    let mut v = forms::text_value(page.document(), focus);
                    v.push('\n');
                    self.forms.set_text_value(page.document_mut(), focus, &v);
                    self.needs_render = true;
                    return;
                }
                Key::Enter => {
                    let (u, b) = (page.url().clone(), page.base_url().clone());
                    if let Some(req) = forms::implicit_submission(page.document(), focus, &u, &b) {
                        self.end.send(ToBrowser::Navigate(req));
                    }
                    return;
                }
                Key::Escape | Key::Tab => {
                    self.focus = None;
                    self.needs_overlay = true;
                    return;
                }
                // M1 follow-up: a movable caret inside text fields.
                Key::ArrowLeft
                | Key::ArrowRight
                | Key::Home
                | Key::End
                | Key::Delete
                | Key::Space => return,
                _ => {}
            }
        }
        let (_, vh) = self.css_viewport();
        let (x, y) = self.scroll;
        match key {
            Key::ArrowDown => self.scroll_to(x, y + LINE_SCROLL),
            Key::ArrowUp => self.scroll_to(x, y - LINE_SCROLL),
            Key::ArrowRight => self.scroll_to(x + LINE_SCROLL, y),
            Key::ArrowLeft => self.scroll_to(x - LINE_SCROLL, y),
            Key::PageDown => self.scroll_to(x, y + vh * 0.9),
            Key::Space if !modifiers.shift => self.scroll_to(x, y + vh * 0.9),
            Key::PageUp | Key::Space => self.scroll_to(x, y - vh * 0.9),
            Key::Home => self.scroll_to(x, 0.0),
            Key::End => self.scroll_to(x, f32::MAX),
            Key::Escape if self.selection.take().is_some() => self.needs_overlay = true,
            _ => {}
        }
    }
}

/// `node` if it is an element, else its parent element.
fn element_of(page: &Page, node: NodeId) -> NodeId {
    let doc = page.document();
    if doc.is_element(node) {
        node
    } else {
        doc.parent(node).unwrap_or(node)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use wb_ipc::{BrowserEnd, Modifiers};

    use super::*;
    use crate::engine::LocalFetcher;

    fn setup(pages: &[(&str, &str)]) -> (BrowserEnd, std::thread::JoinHandle<()>) {
        let mut f = LocalFetcher::new();
        for (u, html) in pages {
            f.insert(u, "text/html", *html);
        }
        let engine = Engine::new(Arc::new(f), Arc::new(wb_text::FontContext::new()));
        let (b, c) = wb_ipc::channel(Arc::new(|| {}));
        let v = Viewport {
            width_px: 200,
            height_px: 100,
            device_scale: 2.0,
            zoom: 1.0,
        };
        let t = spawn(engine, c, v).unwrap();
        (b, t)
    }

    fn wait<F: Fn(&ToBrowser) -> bool>(b: &BrowserEnd, f: F) -> ToBrowser {
        loop {
            let m = b.recv_timeout(Duration::from_secs(10)).expect("message");
            if f(&m) {
                return m;
            }
        }
    }

    const PAGE: &str = "<!DOCTYPE html><style>body{margin:0} div{height:300px}</style>\
        <a href='/next' style='display:block;height:20px'>x</a><div id=d></div>\
        <form action=/s><input type=checkbox name=c id=c style='display:block;width:20px;height:20px'>\
        <input type=submit id=go style='display:block;width:40px;height:20px'></form>";

    #[test]
    fn load_render_scroll_click() {
        let (b, t) = setup(&[("https://a.test/", PAGE)]);
        let url = Url::parse("https://a.test/").unwrap();
        b.send(ToContent::Navigate(NavigationRequest::user(url.clone())));
        assert_eq!(
            wait(&b, |m| matches!(m, ToBrowser::LoadStarted { .. })),
            ToBrowser::LoadStarted {
                url: url.to_string()
            }
        );
        let ToBrowser::LoadCommitted { url: u, .. } =
            wait(&b, |m| matches!(m, ToBrowser::LoadCommitted { .. }))
        else {
            panic!()
        };
        assert_eq!(u, url.as_str());
        let ToBrowser::Frame(f) = wait(&b, |m| matches!(m, ToBrowser::Frame(_))) else {
            panic!()
        };
        // 100x50 CSS px at 2x.
        assert_eq!((f.width, f.height), (200, 100));
        assert_eq!(f.scroll, (0.0, 0.0));
        assert!(f.content_size.1 >= 360.0, "{:?}", f.content_size);
        // Scrolling clamps to the content.
        b.send(ToContent::ScrollBy {
            dx: 0.0,
            dy: 10_000.0,
        });
        let ToBrowser::Frame(f) = wait(&b, |m| matches!(m, ToBrowser::Frame(_))) else {
            panic!()
        };
        assert_eq!(f.scroll.1, f.content_size.1 - 50.0);
        b.send(ToContent::ScrollTo { x: 0.0, y: 0.0 });
        wait(&b, |m| matches!(m, ToBrowser::Frame(_)));
        // Hovering the link reports it and the pointer cursor.
        b.send(ToContent::Input(InputEvent::PointerMove { x: 5.0, y: 5.0 }));
        let ToBrowser::HoverLink(Some(l)) = wait(&b, |m| matches!(m, ToBrowser::HoverLink(_)))
        else {
            panic!()
        };
        assert_eq!(l.as_str(), "https://a.test/next");
        assert_eq!(
            wait(&b, |m| matches!(m, ToBrowser::Cursor(_))),
            ToBrowser::Cursor(CursorKind::Pointer)
        );
        // Clicking it asks the browser to navigate.
        let m = Modifiers::default();
        b.send(ToContent::Input(InputEvent::PointerDown {
            x: 5.0,
            y: 5.0,
            button: PointerButton::Primary,
            clicks: 1,
            modifiers: m,
        }));
        b.send(ToContent::Input(InputEvent::PointerUp {
            x: 5.0,
            y: 5.0,
            button: PointerButton::Primary,
            modifiers: m,
        }));
        let ToBrowser::Navigate(req) = wait(&b, |m| matches!(m, ToBrowser::Navigate(_))) else {
            panic!()
        };
        assert_eq!(req.url.as_str(), "https://a.test/next");
        assert_eq!(req.initiator.as_deref(), Some(url.as_str()));
        // Check the checkbox, then submit.
        b.send(ToContent::ScrollTo { x: 0.0, y: 320.0 });
        for (y, _) in [(14.0, "c"), (34.0, "go")] {
            b.send(ToContent::Input(InputEvent::PointerDown {
                x: 10.0,
                y,
                button: PointerButton::Primary,
                clicks: 1,
                modifiers: m,
            }));
            b.send(ToContent::Input(InputEvent::PointerUp {
                x: 10.0,
                y,
                button: PointerButton::Primary,
                modifiers: m,
            }));
        }
        let ToBrowser::Navigate(req) = wait(&b, |m| matches!(m, ToBrowser::Navigate(_))) else {
            panic!()
        };
        assert_eq!(req.url.as_str(), "https://a.test/s?c=on");
        // A fragment navigation only scrolls.
        b.send(ToContent::Navigate(NavigationRequest::user(
            Url::parse("https://a.test/#d").unwrap(),
        )));
        let ToBrowser::LoadCommitted { url: u, .. } =
            wait(&b, |m| matches!(m, ToBrowser::LoadCommitted { .. }))
        else {
            panic!()
        };
        assert_eq!(u.as_str(), "https://a.test/#d");
        let ToBrowser::Frame(f) = wait(&b, |m| matches!(m, ToBrowser::Frame(_))) else {
            panic!()
        };
        assert_eq!(f.scroll.1, 20.0);
        b.send(ToContent::Shutdown);
        t.join().unwrap();
    }

    #[test]
    fn load_failures_are_reported() {
        let (b, t) = setup(&[]);
        let url = Url::parse("https://a.test/x.zip").unwrap();
        b.send(ToContent::Navigate(NavigationRequest::user(url)));
        let m = wait(&b, |m| {
            matches!(
                m,
                ToBrowser::LoadFailed { .. } | ToBrowser::LoadCommitted { .. }
            )
        });
        // LocalFetcher answers 404 text/plain for unknown URLs: a page.
        assert!(matches!(m, ToBrowser::LoadCommitted { .. }), "{m:?}");
        let url = Url::parse("gopher://a.test/").unwrap();
        b.send(ToContent::Navigate(NavigationRequest::user(url)));
        let m = wait(&b, |m| matches!(m, ToBrowser::LoadFailed { .. }));
        assert!(
            matches!(
                m,
                ToBrowser::LoadFailed {
                    failure: LoadFailure::Network(_),
                    ..
                }
            ),
            "{m:?}"
        );
        drop(b);
        t.join().unwrap();
    }
}
