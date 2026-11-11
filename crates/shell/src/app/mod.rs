//! The window: `winit` for the event loop, `wgpu` (through `egui-wgpu`)
//! for drawing, `egui` for the browser chrome. Pages are rendered on their
//! tabs' content threads and arrive here as pixel frames, which are shown
//! as a texture; input over the page is turned into `wb-ipc` messages.
//!
//! This module is not unit-tested (CI has no display or GPU); the state
//! it drives lives in [`crate::browser`], which is.

mod fonts;

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Color32, ColorImage, Context, Event, Key, Modifiers, Pos2, Rect, Sense, Stroke, TextureHandle,
    TextureOptions, Vec2, ViewportId,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle};
use winit::window::{Window, WindowId};

use wb_ipc::{CursorKind, InputEvent, NavigationRequest, PointerButton, ToContent, Viewport};
use wb_storage::Profile;
use wb_url::Url;

use crate::browser::{BrowserCore, InputOutcome, TabError};
use crate::engine::Engine;
use crate::filters::FILTER_LISTS;
use crate::omnibox::{SEARCH_ENGINES, SearchEngine};
use crate::profile::FirstRunChoice;

/// Wakes the event loop when a content thread sent something.
#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    /// Messages are waiting.
    Wake,
}

/// Runs the browser until its window closes. `start_url` is opened in the
/// first tab.
///
/// # Errors
///
/// When the window system, the profile or the network stack can't start.
pub fn run(start_url: Option<Url>) -> Result<(), String> {
    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .map_err(|e| format!("event loop: {e}"))?;
    let proxy = event_loop.create_proxy();
    let profile =
        Profile::open(wb_storage::DEFAULT_PROFILE_NAME).map_err(|e| format!("profile: {e}"))?;
    let fonts = Engine::system_fonts();
    let waker: wb_ipc::Waker = Arc::new(move || {
        let _ = proxy.send_event(AppEvent::Wake);
    });
    let viewport = Viewport {
        width_px: 1200,
        height_px: 800,
        device_scale: 1.0,
        zoom: 1.0,
    };
    let core = BrowserCore::new(profile, fonts, waker, viewport).map_err(|e| e.to_string())?;
    let mut app = App::new(core, event_loop.owned_display_handle(), start_url);
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("event loop: {e}"))
}

/// Per-window UI state that is not browser state.
#[derive(Default)]
struct Chrome {
    /// The URL bar text while it is being edited.
    url_edit: Option<String>,
    /// Focus the URL bar on the next frame.
    focus_url: bool,
    show_settings: bool,
    /// The first-run screen's choices so far.
    first_run: FirstRunChoice,
    /// Search terms waiting for the user to pick an engine.
    search_prompt: Option<String>,
    confirm_clear: bool,
    /// The page texture and the frame it shows.
    texture: Option<TextureHandle>,
    texture_pixels: Option<Arc<Vec<u8>>>,
    /// The page area's device pixel size last sent to the browser.
    viewport_px: (u32, u32, u32),
    /// Whether the primary button went down on the page.
    page_pressed: bool,
    /// Where the page is drawn (points).
    page_rect: Option<Rect>,
}

struct Gpu {
    window: Arc<Window>,
    painter: egui_wgpu::winit::Painter,
    state: egui_winit::State,
}

struct App {
    core: BrowserCore,
    display: OwnedDisplayHandle,
    ctx: Context,
    gpu: Option<Gpu>,
    chrome: Chrome,
    start_url: Option<Url>,
}

impl App {
    fn new(core: BrowserCore, display: OwnedDisplayHandle, start_url: Option<Url>) -> App {
        let ctx = Context::default();
        ctx.set_fonts(fonts::system_font_definitions());
        App {
            core,
            display,
            ctx,
            gpu: None,
            chrome: Chrome::default(),
            start_url,
        }
    }

    fn request_redraw(&self) {
        if let Some(g) = &self.gpu {
            g.window.request_redraw();
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        let raw = gpu.state.take_egui_input(&gpu.window);
        self.core.poll();
        let ctx = self.ctx.clone();
        let mut chrome = std::mem::take(&mut self.chrome);
        let core = &mut self.core;
        let mut quit = false;
        let out = ctx.run_ui(raw, |ui| {
            quit |= draw(ui, core, &mut chrome);
        });
        self.chrome = chrome;
        let Some(gpu) = self.gpu.as_mut() else { return };
        gpu.state
            .handle_platform_output(&gpu.window, out.platform_output);
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let mut textures = out.textures_delta;
        gpu.painter.paint_and_update_textures(
            ViewportId::ROOT,
            out.pixels_per_point,
            [1.0, 1.0, 1.0, 1.0],
            &prims,
            &mut textures,
            Vec::new(),
            &gpu.window,
        );
        if quit {
            event_loop.exit();
            return;
        }
        let delay = out
            .viewport_output
            .get(&ViewportId::ROOT)
            .map_or(Duration::MAX, |v| v.repaint_delay);
        if delay.is_zero() {
            gpu.window.request_redraw();
        } else if let Some(at) = Instant::now().checked_add(delay) {
            event_loop.set_control_flow(ControlFlow::WaitUntil(at));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("web_browser")
            .with_inner_size(winit::dpi::LogicalSize::new(1200.0, 800.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("web_browser: can't open a window: {e}");
                event_loop.exit();
                return;
            }
        };
        let config = egui_wgpu::WgpuConfiguration {
            wgpu_setup: egui_wgpu::WgpuSetup::from_display_handle(self.display.clone()),
            ..egui_wgpu::WgpuConfiguration::default()
        };
        let mut painter = pollster::block_on(egui_wgpu::winit::Painter::new(
            self.ctx.clone(),
            config,
            false,
            egui_wgpu::RendererOptions::default(),
        ));
        if let Err(e) =
            pollster::block_on(painter.set_window(ViewportId::ROOT, Some(Arc::clone(&window))))
        {
            eprintln!("web_browser: no usable GPU adapter: {e}");
            event_loop.exit();
            return;
        }
        let state = egui_winit::State::new(
            self.ctx.clone(),
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            painter.max_texture_side(),
        );
        self.gpu = Some(Gpu {
            window,
            painter,
            state,
        });
        if self.core.tabs.is_empty() {
            let req = self.start_url.take().map(NavigationRequest::user);
            self.core.open_tab(req);
            if self.core.active_tab().is_some_and(|t| t.url().is_none()) {
                self.chrome.focus_url = true;
            }
        }
        self.request_redraw();
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::Wake => {
                if self.core.poll() {
                    self.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop);
                return;
            }
            WindowEvent::Resized(size) => {
                if let (Some(g), Some(w), Some(h)) = (
                    self.gpu.as_mut(),
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                ) {
                    g.painter.on_window_resized(ViewportId::ROOT, w, h);
                }
            }
            _ => {}
        }
        if let Some(g) = self.gpu.as_mut() {
            let response = g.state.on_window_event(&g.window, &event);
            if response.repaint {
                g.window.request_redraw();
            }
        }
    }
}

/// Draws the whole window. Returns `true` when the browser should quit
/// (the last tab closed).
fn draw(ui: &mut egui::Ui, core: &mut BrowserCore, chrome: &mut Chrome) -> bool {
    let ctx = ui.ctx().clone();
    let mut quit = false;

    // Text the page copied goes to the clipboard.
    if let Some(text) = core.active_tab_mut().and_then(|t| t.copied.take()) {
        ctx.copy_text(text);
    }

    quit |= shortcuts(&ctx, core, chrome);

    egui::Panel::top("tabs").show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut switch = None;
            let mut close = None;
            for (i, tab) in core.tabs.iter().enumerate() {
                let mut title = if tab.title.is_empty() {
                    tab.url()
                        .map_or_else(|| "New Tab".to_string(), |u| u.as_str().to_string())
                } else {
                    tab.title.clone()
                };
                if title.chars().count() > 24 {
                    title = title.chars().take(23).collect::<String>() + "\u{2026}";
                }
                if tab.loading.is_some() {
                    title = format!("\u{25cc} {title}");
                }
                if ui.selectable_label(i == core.active, title).clicked() {
                    switch = Some(i);
                }
                if ui
                    .small_button("\u{d7}")
                    .on_hover_text("Close tab")
                    .clicked()
                {
                    close = Some(i);
                }
                ui.separator();
            }
            if ui.button("+").on_hover_text("New tab (Ctrl+T)").clicked() {
                core.open_tab(None);
                chrome.focus_url = true;
                chrome.url_edit = None;
            }
            if let Some(i) = switch {
                core.active = i;
                chrome.url_edit = None;
            }
            if let Some(i) = close {
                quit |= !core.close_tab(i);
                chrome.url_edit = None;
            }
        });
    });

    egui::Panel::top("toolbar").show(ui, |ui| toolbar(ui, core, chrome));

    egui::Panel::bottom("status").show(ui, |ui| {
        let text = match core.active_tab() {
            Some(t) => match (&t.hover_link, t.loading) {
                (Some(l), _) => l.as_str().to_string(),
                (None, Some(p)) => format!("Loading ({p:?})\u{2026}"),
                (None, None) if t.blocked_requests > 0 => {
                    format!("Blocked {} requests on this page", t.blocked_requests)
                }
                _ => String::new(),
            },
            None => String::new(),
        };
        ui.label(text);
    });

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            if chrome.show_settings || core.active_tab().is_some_and(|t| t.error.is_some()) {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
            }
            if chrome.show_settings {
                egui::Frame::central_panel(ui.style())
                    .show(ui, |ui| settings_page(ui, core, chrome));
                return;
            }
            let error = core.active_tab().and_then(|t| t.error.clone());
            match error {
                Some(e) => {
                    egui::Frame::central_panel(ui.style()).show(ui, |ui| error_page(ui, core, &e));
                }
                None => page_view(ui, core, chrome),
            }
        });

    if core.first_run {
        first_run_modal(&ctx, core, chrome);
    }
    if chrome.search_prompt.is_some() {
        search_prompt(&ctx, core, chrome);
    }
    if let Some(page) = chrome.page_rect {
        select_popup(&ctx, core, page);
    }
    quit
}

/// Global keyboard shortcuts. Returns `true` to quit.
fn shortcuts(ctx: &Context, core: &mut BrowserCore, chrome: &mut Chrome) -> bool {
    let cmd = Modifiers::COMMAND;
    let mut quit = false;
    ctx.input_mut(|i| {
        if i.consume_key(cmd, Key::L) {
            chrome.focus_url = true;
            chrome.url_edit = None;
        }
        if i.consume_key(cmd, Key::T) {
            core.open_tab(None);
            chrome.focus_url = true;
            chrome.url_edit = None;
        }
        if i.consume_key(cmd, Key::W) {
            let a = core.active;
            quit = !core.close_tab(a);
        }
        if i.consume_key(cmd, Key::R) || i.consume_key(Modifiers::NONE, Key::F5) {
            core.reload();
        }
        if i.consume_key(cmd, Key::Plus) || i.consume_key(cmd, Key::Equals) {
            core.zoom_step(true);
        }
        if i.consume_key(cmd, Key::Minus) {
            core.zoom_step(false);
        }
        if i.consume_key(cmd, Key::Num0) {
            core.set_zoom(1.0);
        }
        if i.consume_key(Modifiers::ALT, Key::ArrowLeft) {
            core.back();
        }
        if i.consume_key(Modifiers::ALT, Key::ArrowRight) {
            core.forward();
        }
    });
    quit
}

fn toolbar(ui: &mut egui::Ui, core: &mut BrowserCore, chrome: &mut Chrome) {
    ui.horizontal(|ui| {
        let (can_back, can_fwd, loading) = core.active_tab().map_or((false, false, false), |t| {
            (
                t.history.can_go_back(),
                t.history.can_go_forward(),
                t.loading.is_some(),
            )
        });
        if ui
            .add_enabled(can_back, egui::Button::new("\u{2190}"))
            .on_hover_text("Back (Alt+Left)")
            .clicked()
        {
            core.back();
        }
        if ui
            .add_enabled(can_fwd, egui::Button::new("\u{2192}"))
            .on_hover_text("Forward (Alt+Right)")
            .clicked()
        {
            core.forward();
        }
        if loading {
            if ui.button("\u{d7}").on_hover_text("Stop").clicked() {
                core.stop();
            }
        } else if ui
            .button("\u{21bb}")
            .on_hover_text("Reload (Ctrl+R)")
            .clicked()
        {
            core.reload();
        }

        // Right-hand buttons first so the URL bar can take the rest.
        let right_width = 230.0;
        let url_width = (ui.available_width() - right_width).max(120.0);
        let shown = chrome.url_edit.clone().unwrap_or_else(|| {
            core.active_tab()
                .map(|t| t.url_text.clone())
                .unwrap_or_default()
        });
        let mut text = shown;
        let edit = egui::TextEdit::singleline(&mut text)
            .id(egui::Id::unique("url-bar"))
            .hint_text("Search or enter address")
            .desired_width(url_width);
        let resp = ui.add(edit);
        if chrome.focus_url {
            resp.request_focus();
            chrome.focus_url = false;
        }
        if resp.changed() || resp.has_focus() {
            chrome.url_edit = Some(text.clone());
        }
        let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if submitted {
            chrome.url_edit = None;
            chrome.show_settings = false;
            if let InputOutcome::NeedsSearchEngine(terms) = core.submit_input(&text) {
                chrome.search_prompt = Some(terms);
            }
        } else if resp.lost_focus() {
            chrome.url_edit = None;
        }
        if resp.has_focus() && !text.trim().is_empty() {
            suggestions(ui, core, chrome, resp.rect, &text);
        }

        // Per-site blocker toggle.
        if let Some(disabled) = core.blocker_disabled_here() {
            let blocked = core.active_tab().map_or(0, |t| t.blocked_requests);
            let label = if disabled {
                "Blocker off".to_string()
            } else {
                format!("Blocker on ({blocked})")
            };
            if ui
                .selectable_label(!disabled, label)
                .on_hover_text("Turn the content blocker on or off for this site")
                .clicked()
            {
                core.toggle_blocker_here();
            }
        }
        if let Some(z) = core.active_tab().map(|t| t.zoom)
            && (z - 1.0).abs() > 0.001
            && ui
                .button(format!("{:.0}%", z * 100.0))
                .on_hover_text("Reset zoom (Ctrl+0)")
                .clicked()
        {
            core.set_zoom(1.0);
        }
        if ui
            .selectable_label(chrome.show_settings, "Settings")
            .clicked()
        {
            chrome.show_settings = !chrome.show_settings;
        }
    });
}

/// History suggestions under the URL bar.
fn suggestions(ui: &egui::Ui, core: &mut BrowserCore, chrome: &mut Chrome, bar: Rect, text: &str) {
    let entries = core.suggestions(text, 6);
    if entries.is_empty() {
        return;
    }
    let mut chosen = None;
    egui::Area::new(egui::Id::unique("url-suggestions"))
        .order(egui::Order::Foreground)
        .fixed_pos(bar.left_bottom())
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(bar.width());
                for e in &entries {
                    let label = match &e.title {
                        Some(t) if !t.is_empty() => format!("{t} \u{2014} {}", e.url),
                        _ => e.url.clone(),
                    };
                    if ui.selectable_label(false, label).clicked() {
                        chosen = Some(e.url.clone());
                    }
                }
            });
        });
    if let Some(u) = chosen.and_then(|u| Url::parse(&u).ok()) {
        chrome.url_edit = None;
        core.navigate(NavigationRequest::user(u));
        ui.ctx()
            .memory_mut(|m| m.surrender_focus(egui::Id::unique("url-bar")));
    }
}

/// The page itself: the latest frame as a texture, overlays, and input
/// forwarding.
fn page_view(ui: &mut egui::Ui, core: &mut BrowserCore, chrome: &mut Chrome) {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    chrome.page_rect = Some(rect);
    let ctx = ui.ctx().clone();
    let ppp = ctx.pixels_per_point();
    let w = (rect.width() * ppp).round().max(1.0) as u32;
    let h = (rect.height() * ppp).round().max(1.0) as u32;
    let key = (w, h, ppp.to_bits());
    if chrome.viewport_px != key {
        chrome.viewport_px = key;
        core.set_viewport(w, h, ppp);
    }
    let Some(tab) = core.active_tab() else { return };
    let zoom = tab.zoom;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::WHITE);
    let mut scroll = (0.0, 0.0);
    let mut content = (rect.width(), rect.height());
    if let Some(frame) = &tab.frame {
        scroll = frame.scroll;
        content = frame.content_size;
        let fresh = chrome
            .texture_pixels
            .as_ref()
            .is_none_or(|p| !Arc::ptr_eq(p, &frame.pixels));
        if fresh {
            let size = [frame.width as usize, frame.height as usize];
            if frame.pixels.len() == size[0] * size[1] * 4 {
                let image = ColorImage::from_rgba_premultiplied(size, &frame.pixels);
                match &mut chrome.texture {
                    Some(t) => t.set(image, TextureOptions::NEAREST),
                    None => {
                        chrome.texture =
                            Some(ctx.load_texture("page", image, TextureOptions::NEAREST));
                    }
                }
                chrome.texture_pixels = Some(Arc::clone(&frame.pixels));
            }
        }
        if let Some(t) = &chrome.texture {
            let size = Vec2::new(frame.width as f32 / ppp, frame.height as f32 / ppp);
            painter.image(
                t.id(),
                Rect::from_min_size(rect.min, size),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        // Selection highlights and the focus ring (document CSS px).
        let to_screen = |r: &[f32; 4]| {
            Rect::from_min_size(
                rect.min + Vec2::new((r[0] - scroll.0) * zoom, (r[1] - scroll.1) * zoom),
                Vec2::new(r[2] * zoom, r[3] * zoom),
            )
        };
        for r in &frame.highlights {
            painter.rect_filled(
                to_screen(r),
                0.0,
                Color32::from_rgba_unmultiplied(51, 120, 255, 90),
            );
        }
        if let Some(r) = &frame.focus_ring {
            painter.rect_stroke(
                to_screen(r).expand(1.0),
                2.0,
                Stroke::new(2.0, Color32::from_rgb(51, 120, 255)),
                egui::StrokeKind::Outside,
            );
        }
    }
    // A thin scroll position indicator.
    let view_h = rect.height() / zoom;
    if content.1 > view_h + 0.5 {
        let frac = view_h / content.1;
        let top = scroll.1 / content.1;
        let bar = Rect::from_min_max(
            Pos2::new(rect.right() - 6.0, rect.top() + top * rect.height()),
            Pos2::new(
                rect.right() - 2.0,
                rect.top() + (top + frac) * rect.height(),
            ),
        );
        painter.rect_filled(bar, 2.0, Color32::from_black_alpha(90));
    }
    if resp.hovered() {
        ctx.set_cursor_icon(match tab.cursor {
            CursorKind::Default => egui::CursorIcon::Default,
            CursorKind::Pointer => egui::CursorIcon::PointingHand,
            CursorKind::Text => egui::CursorIcon::Text,
        });
    }
    if resp.clicked() || resp.drag_started() {
        // Clicking the page takes keyboard focus from the URL bar.
        resp.request_focus();
    }
    forward_input(&ctx, core, chrome, rect, &resp, zoom, view_h);
}

/// Turns egui input over the page into content messages.
fn forward_input(
    ctx: &Context,
    core: &mut BrowserCore,
    chrome: &mut Chrome,
    rect: Rect,
    resp: &egui::Response,
    zoom: f32,
    view_h: f32,
) {
    let events = ctx.input(|i| i.events.clone());
    let page_has_keys = ctx.memory(|m| m.focused().is_none_or(|f| f == resp.id));
    let to_css = |p: Pos2| ((p.x - rect.min.x) / zoom, (p.y - rect.min.y) / zoom);
    let mut zoom_steps = 0i32;
    let mut msgs = Vec::new();
    for e in events {
        match e {
            Event::PointerMoved(p) if resp.hovered() || chrome.page_pressed => {
                let (x, y) = to_css(p);
                msgs.push(ToContent::Input(InputEvent::PointerMove { x, y }));
            }
            Event::PointerGone => msgs.push(ToContent::Input(InputEvent::PointerLeave)),
            Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                let inside = rect.contains(pos);
                if pressed && !inside {
                    continue;
                }
                if !pressed && !chrome.page_pressed {
                    continue;
                }
                let button = match button {
                    egui::PointerButton::Primary => PointerButton::Primary,
                    egui::PointerButton::Secondary => PointerButton::Secondary,
                    egui::PointerButton::Middle => PointerButton::Middle,
                    _ => continue,
                };
                chrome.page_pressed = pressed;
                let (x, y) = to_css(pos);
                let modifiers = ipc_modifiers(modifiers);
                msgs.push(ToContent::Input(if pressed {
                    InputEvent::PointerDown {
                        x,
                        y,
                        button,
                        clicks: 1,
                        modifiers,
                    }
                } else {
                    InputEvent::PointerUp {
                        x,
                        y,
                        button,
                        modifiers,
                    }
                }));
            }
            Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } if resp.hovered() => {
                if modifiers.command {
                    zoom_steps += if delta.y > 0.0 { 1 } else { -1 };
                    continue;
                }
                let factor = match unit {
                    egui::MouseWheelUnit::Point => 1.0 / zoom,
                    egui::MouseWheelUnit::Line => 40.0,
                    egui::MouseWheelUnit::Page => view_h * 0.9,
                };
                let (mut dx, dy) = (-delta.x * factor, -delta.y * factor);
                if modifiers.shift && dx == 0.0 {
                    dx = dy;
                    msgs.push(ToContent::ScrollBy { dx, dy: 0.0 });
                } else {
                    msgs.push(ToContent::ScrollBy { dx, dy });
                }
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } if page_has_keys => {
                if modifiers.command && key == Key::A {
                    msgs.push(ToContent::SelectAll);
                    continue;
                }
                if modifiers.command {
                    continue;
                }
                let k = match key {
                    Key::Enter => wb_ipc::Key::Enter,
                    Key::Tab => wb_ipc::Key::Tab,
                    Key::Backspace => wb_ipc::Key::Backspace,
                    Key::Delete => wb_ipc::Key::Delete,
                    Key::ArrowLeft => wb_ipc::Key::ArrowLeft,
                    Key::ArrowRight => wb_ipc::Key::ArrowRight,
                    Key::ArrowUp => wb_ipc::Key::ArrowUp,
                    Key::ArrowDown => wb_ipc::Key::ArrowDown,
                    Key::Home => wb_ipc::Key::Home,
                    Key::End => wb_ipc::Key::End,
                    Key::PageUp => wb_ipc::Key::PageUp,
                    Key::PageDown => wb_ipc::Key::PageDown,
                    Key::Space => wb_ipc::Key::Space,
                    Key::Escape => wb_ipc::Key::Escape,
                    _ => continue,
                };
                msgs.push(ToContent::Input(InputEvent::KeyDown {
                    key: k,
                    modifiers: ipc_modifiers(modifiers),
                }));
            }
            Event::Text(t) if page_has_keys => {
                // A space is also sent as Key::Space; the content side
                // types it only into a focused field, so pass both.
                msgs.push(ToContent::Input(InputEvent::Text(t)));
            }
            Event::Paste(t) if page_has_keys => {
                msgs.push(ToContent::Input(InputEvent::Text(t)));
            }
            Event::Copy if page_has_keys => msgs.push(ToContent::CopySelection),
            _ => {}
        }
    }
    for _ in 0..zoom_steps.max(0) {
        core.zoom_step(true);
    }
    for _ in 0..(-zoom_steps).max(0) {
        core.zoom_step(false);
    }
    if let Some(tab) = core.active_tab() {
        for m in msgs {
            tab.send(m);
        }
    }
}

fn ipc_modifiers(m: Modifiers) -> wb_ipc::Modifiers {
    wb_ipc::Modifiers {
        shift: m.shift,
        ctrl: m.ctrl,
        alt: m.alt,
        meta: m.mac_cmd,
    }
}

/// An error page, or the HTTPS-only interstitial.
fn error_page(ui: &mut egui::Ui, core: &mut BrowserCore, error: &TabError) {
    ui.add_space(40.0);
    match error {
        TabError::Https(i) => {
            ui.heading("This site doesn't offer a secure connection");
            ui.add_space(8.0);
            ui.label(format!(
                "The browser tried to connect to {} over HTTPS, but it failed: {}",
                i.host, i.reason
            ));
            ui.label(
                "If you continue, the page loads over plain HTTP: anyone on the network can \
                 read and change what you see and send on this site.",
            );
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Go back").clicked() {
                    if core
                        .active_tab()
                        .is_some_and(|t| t.history.current().is_some())
                    {
                        if let Some(t) = core.active_tab_mut() {
                            t.error = None;
                            t.url_text =
                                t.url().map(|u| u.as_str().to_string()).unwrap_or_default();
                        }
                    } else {
                        core.back();
                    }
                }
                if ui
                    .button(format!("Continue to {} (not secure)", i.host))
                    .on_hover_text("Remembered for this site; remove it from settings")
                    .clicked()
                {
                    core.allow_insecure_and_continue();
                }
            });
        }
        TabError::Failed { url, message } => {
            ui.heading("This page couldn't be loaded");
            ui.add_space(8.0);
            ui.label(url.as_str());
            ui.label(message);
            ui.add_space(12.0);
            if ui.button("Try again").clicked() {
                core.navigate(NavigationRequest::user(url.clone()));
            }
        }
    }
}

fn settings_page(ui: &mut egui::Ui, core: &mut BrowserCore, chrome: &mut Chrome) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Settings");
        ui.add_space(12.0);

        ui.strong("Search engine");
        ui.label("Used when what you type in the address bar isn't an address.");
        let mut engine = core.search_engine().map(|e| e.id);
        let before = engine;
        ui.radio_value(&mut engine, None, "None (ask me)");
        for e in SEARCH_ENGINES {
            ui.radio_value(&mut engine, Some(e.id), e.name);
        }
        if engine != before {
            core.set_search_engine(engine.and_then(SearchEngine::by_id));
        }
        ui.add_space(12.0);

        ui.strong("Content blocker");
        let mut on = core.blocker_enabled();
        if ui.checkbox(&mut on, "Block ads and trackers").changed() {
            core.set_blocker_enabled(on);
        }
        for list in FILTER_LISTS {
            let mut enabled = core.filter_list_enabled(list.id);
            if ui
                .add_enabled(on, egui::Checkbox::new(&mut enabled, list.name))
                .on_hover_text(list.url)
                .changed()
            {
                core.set_filter_list_enabled(list.id, enabled);
            }
        }
        let mut downloads = core.filter_downloads_enabled();
        if ui
            .checkbox(&mut downloads, "Download and update filter lists automatically")
            .on_hover_text(
                "The only request the browser makes on its own. Lists come from easylist.to.",
            )
            .changed()
        {
            core.set_filter_downloads(downloads);
        }
        if ui
            .add_enabled(downloads, egui::Button::new("Update lists now"))
            .clicked()
        {
            core.update_filter_lists(true);
        }
        ui.add_space(12.0);

        ui.strong("Privacy");
        ui.label("No telemetry. Third-party cookies are blocked; HTTPS is required unless you allow a site.");
        if chrome.confirm_clear {
            ui.horizontal(|ui| {
                ui.label("Delete history, cookies and cached pages?");
                if ui.button("Delete").clicked() {
                    core.clear_data();
                    chrome.confirm_clear = false;
                }
                if ui.button("Cancel").clicked() {
                    chrome.confirm_clear = false;
                }
            });
        } else if ui.button("Clear browsing data\u{2026}").clicked() {
            chrome.confirm_clear = true;
        }
        ui.add_space(12.0);
        if ui.button("Close settings").clicked() {
            chrome.show_settings = false;
        }
    });
}

fn first_run_modal(ctx: &Context, core: &mut BrowserCore, chrome: &mut Chrome) {
    egui::Modal::new(egui::Id::unique("first-run")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading("Welcome");
        ui.label("Two choices before you start. You can change both later in Settings.");
        ui.add_space(10.0);
        ui.strong("Search engine");
        ui.label("None of these pay us. Pick one, or leave it and choose when you first search.");
        let mut engine = chrome.first_run.search_engine.map(|e| e.id);
        for e in SEARCH_ENGINES {
            ui.radio_value(&mut engine, Some(e.id), e.name);
        }
        chrome.first_run.search_engine = engine.and_then(SearchEngine::by_id);
        ui.add_space(10.0);
        ui.strong("Content blocker");
        ui.checkbox(
            &mut chrome.first_run.download_filter_lists,
            "Download ad and tracker filter lists (EasyList, EasyPrivacy) from easylist.to and keep them updated",
        );
        ui.label(
            "This is the only network request the browser makes on its own. \
             Without it, nothing is blocked.",
        );
        ui.add_space(10.0);
        if ui.button("Start browsing").clicked() {
            let choice = chrome.first_run;
            core.complete_first_run(&choice);
            chrome.focus_url = true;
        }
    });
}

fn search_prompt(ctx: &Context, core: &mut BrowserCore, chrome: &mut Chrome) {
    let Some(terms) = chrome.search_prompt.clone() else {
        return;
    };
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::unique("search-prompt")).show(ctx, |ui| {
        ui.heading("Choose a search engine");
        ui.label(format!(
            "To search for \u{201c}{terms}\u{201d}, pick an engine. It is remembered."
        ));
        ui.add_space(8.0);
        for e in SEARCH_ENGINES {
            if ui.button(e.name).clicked() {
                core.set_search_engine(Some(e));
                if let Some(u) = e.search_url(&terms) {
                    core.navigate(NavigationRequest::user(u));
                }
                close = true;
            }
        }
        if ui.button("Cancel").clicked() {
            close = true;
        }
    });
    if close || resp.should_close() {
        chrome.search_prompt = None;
    }
}

/// The drop-down of a `<select>` the page opened.
fn select_popup(ctx: &Context, core: &mut BrowserCore, page: Rect) {
    let Some(tab) = core.active_tab() else { return };
    let Some(popup) = tab.select_popup.clone() else {
        return;
    };
    let zoom = tab.zoom;
    let page_top = page.min;
    let pos = page_top + Vec2::new(popup.rect[0] * zoom, (popup.rect[1] + popup.rect[3]) * zoom);
    let mut choice: Option<Option<usize>> = None;
    let area = egui::Area::new(egui::Id::unique("select-popup"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(popup.rect[2] * zoom);
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for (i, label) in popup.options.iter().enumerate() {
                            if ui
                                .selectable_label(popup.selected.contains(&i), label)
                                .clicked()
                            {
                                choice = Some(Some(i));
                            }
                        }
                    });
            });
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !area.response.rect.contains(p));
    if choice.is_none() && (clicked_outside || ctx.input(|i| i.key_pressed(Key::Escape))) {
        choice = Some(None);
    }
    if let Some(c) = choice
        && let Some(tab) = core.active_tab_mut()
    {
        tab.select_popup = None;
        tab.send(ToContent::ChooseOption(c));
    }
}
