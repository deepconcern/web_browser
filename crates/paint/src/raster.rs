//! Rasterizing a display list with tiny-skia. The rules are in the crate
//! docs.

use std::fmt;
use std::sync::Arc;

use tiny_skia::{
    BlendMode, FillRule, FilterQuality, Mask, Paint, PathBuilder, Pattern, Pixmap, PixmapPaint,
    PixmapRef, SpreadMode, Transform,
};
use wb_image::Image;
use wb_text::{FontContext, GlyphMask, GlyphRequest, subpixel_position};

use crate::border::border_fills;
use crate::geom::{Color, CornerRadii, Rect, RoundedRect};
use crate::list::{
    BackgroundImageItem, BorderItem, DisplayItem, DisplayList, FillItem, ImageItem, ImageRendering,
    TextItem,
};
use crate::shape::{DRRect, DRect, IRect, Poly, clip_to_rect, has_area, positive, snap};

/// Where glyph coverage masks come from. [`FontContext`] implements it;
/// tests and tools can supply their own.
pub trait GlyphRasterizer {
    /// Renders one glyph, or `None` if it can't be drawn (it is then
    /// skipped).
    fn rasterize_glyph(&self, request: &GlyphRequest) -> Option<Arc<GlyphMask>>;
}

impl GlyphRasterizer for FontContext {
    fn rasterize_glyph(&self, request: &GlyphRequest) -> Option<Arc<GlyphMask>> {
        FontContext::rasterize_glyph(self, request)
    }
}

/// Caps on the memory one rasterization may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RasterLimits {
    /// Largest output width or height in device pixels.
    pub max_dimension: u32,
    /// Largest output buffer in bytes (4 per pixel).
    pub max_bytes: usize,
    /// Memory for opacity layers and rounded-clip masks alive at once.
    pub max_scratch_bytes: usize,
    /// Deepest nesting of opacity layers.
    pub max_layers: usize,
}

impl Default for RasterLimits {
    fn default() -> RasterLimits {
        RasterLimits {
            max_dimension: 16_384,
            max_bytes: 256 * 1024 * 1024,
            max_scratch_bytes: 256 * 1024 * 1024,
            max_layers: 16,
        }
    }
}

/// What part of the list to draw, and how.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterOptions {
    /// The area of the page to draw, in CSS px. Its top-left corner lands
    /// on the output's top-left pixel.
    pub viewport: Rect,
    /// Device pixels per CSS px (the device scale factor times the zoom).
    pub scale: f32,
    /// The color the output is cleared to before drawing.
    pub background: Color,
    /// Memory caps.
    pub limits: RasterLimits,
}

impl RasterOptions {
    /// Options for `viewport` at `scale`, cleared to transparent, with the
    /// default limits.
    #[must_use]
    pub fn new(viewport: Rect, scale: f32) -> RasterOptions {
        RasterOptions {
            viewport,
            scale,
            background: Color::TRANSPARENT,
            limits: RasterLimits::default(),
        }
    }

    /// The output size in device pixels: the viewport size times the
    /// scale, rounded up.
    fn device_size(&self) -> Result<(u32, u32), PaintError> {
        if !(self.scale.is_finite() && self.scale > 0.0 && self.scale <= 64.0) {
            return Err(PaintError::InvalidScale);
        }
        let v = &self.viewport;
        if !v.is_finite() || v.is_empty() {
            return Err(PaintError::InvalidViewport);
        }
        let w = (v.width * self.scale).ceil();
        let h = (v.height * self.scale).ceil();
        let max = self.limits.max_dimension as f32;
        if !(w <= max && h <= max) {
            return Err(PaintError::TooLarge);
        }
        // Both are in 1..=max_dimension here.
        let (w, h) = (w as u32, h as u32);
        let bytes = u64::from(w) * u64::from(h) * 4;
        if bytes > self.limits.max_bytes as u64 {
            return Err(PaintError::TooLarge);
        }
        Ok((w.max(1), h.max(1)))
    }
}

/// Why a rasterization failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaintError {
    /// The scale is not a positive finite number up to 64.
    InvalidScale,
    /// The viewport is empty or not finite.
    InvalidViewport,
    /// The output would exceed [`RasterLimits`].
    TooLarge,
    /// Allocating the output failed.
    AllocationFailed,
    /// [`repaint`] was given a raster of a different size than the options
    /// describe.
    SizeMismatch,
}

impl fmt::Display for PaintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PaintError::InvalidScale => "invalid scale factor",
            PaintError::InvalidViewport => "invalid viewport",
            PaintError::TooLarge => "output too large",
            PaintError::AllocationFailed => "allocation failed",
            PaintError::SizeMismatch => "raster size does not match the options",
        })
    }
}

impl std::error::Error for PaintError {}

/// A rasterized image: premultiplied RGBA8 pixels, rows top to bottom with
/// no padding, ready to upload as a texture.
pub struct Raster {
    pixmap: Pixmap,
}

impl fmt::Debug for Raster {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Raster")
            .field("width", &self.width())
            .field("height", &self.height())
            .finish_non_exhaustive()
    }
}

impl Raster {
    /// Width in device pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.pixmap.width()
    }

    /// Height in device pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.pixmap.height()
    }

    /// The premultiplied RGBA8 pixels.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        self.pixmap.data()
    }

    /// Consumes the raster and returns its premultiplied RGBA8 pixels.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixmap.take()
    }

    /// The premultiplied `[r, g, b, a]` at (`x`, `y`), or `None` outside.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.pixmap
            .pixel(x, y)
            .map(|p| [p.red(), p.green(), p.blue(), p.alpha()])
    }
}

/// Draws the part of `list` inside `options.viewport` into a new raster.
///
/// # Errors
///
/// Fails if the options are invalid or the output would exceed the limits.
pub fn rasterize(
    list: &DisplayList,
    options: &RasterOptions,
    glyphs: &dyn GlyphRasterizer,
) -> Result<Raster, PaintError> {
    let (w, h) = options.device_size()?;
    let pixmap = Pixmap::new(w, h).ok_or(PaintError::AllocationFailed)?;
    let mut raster = Raster { pixmap };
    let full = IRect {
        x0: 0,
        y0: 0,
        x1: i32::try_from(w).unwrap_or(i32::MAX),
        y1: i32::try_from(h).unwrap_or(i32::MAX),
    };
    paint_region(list, options, glyphs, &mut raster, full);
    Ok(raster)
}

/// Redraws only `dirty` (CSS px, rounded outwards to whole device pixels)
/// of an existing raster made with the same options. Items that miss the
/// dirty area are skipped.
///
/// # Errors
///
/// Fails if the options are invalid or `raster` has a different size.
pub fn repaint(
    list: &DisplayList,
    options: &RasterOptions,
    glyphs: &dyn GlyphRasterizer,
    raster: &mut Raster,
    dirty: Rect,
) -> Result<(), PaintError> {
    let (w, h) = options.device_size()?;
    if raster.width() != w || raster.height() != h {
        return Err(PaintError::SizeMismatch);
    }
    if !dirty.is_finite() {
        return Ok(());
    }
    let s = options.scale;
    let v = &options.viewport;
    let d = DRect {
        l: (dirty.x - v.x) * s,
        t: (dirty.y - v.y) * s,
        r: (dirty.right() - v.x) * s,
        b: (dirty.bottom() - v.y) * s,
    };
    let full = IRect {
        x0: 0,
        y0: 0,
        x1: i32::try_from(w).unwrap_or(i32::MAX),
        y1: i32::try_from(h).unwrap_or(i32::MAX),
    };
    let area = IRect::enclosing(&d).intersect(&full);
    if !area.is_empty() {
        paint_region(list, options, glyphs, raster, area);
    }
    Ok(())
}

fn paint_region(
    list: &DisplayList,
    options: &RasterOptions,
    glyphs: &dyn GlyphRasterizer,
    raster: &mut Raster,
    area: IRect,
) {
    // Clear the area to the background.
    if let Some(rect) = tiny_skia::Rect::from_ltrb(
        area.x0 as f32,
        area.y0 as f32,
        area.x1 as f32,
        area.y1 as f32,
    ) {
        let mut paint = solid_paint(options.background, 1.0);
        paint.blend_mode = BlendMode::Source;
        raster
            .pixmap
            .fill_rect(rect, &paint, Transform::identity(), None);
    }
    let mut painter = Painter {
        root: &mut raster.pixmap,
        surfaces: Vec::new(),
        clips: vec![ClipState {
            rect: area,
            mask: None,
        }],
        groups: Vec::new(),
        scroll: (0, 0),
        alpha: 1.0,
        scale: options.scale,
        origin: (options.viewport.x, options.viewport.y),
        scratch: 0,
        limits: options.limits,
        glyphs,
    };
    for item in list.items() {
        painter.item(item);
    }
    while !painter.groups.is_empty() {
        painter.pop_group();
    }
}

/// An opacity layer.
struct Surface {
    pixmap: Pixmap,
    /// The surface's top-left corner in root device px.
    ox: i32,
    oy: i32,
}

struct ClipState {
    /// The clip's bounds in root device px (all clipping, when there is no
    /// mask).
    rect: IRect,
    /// Coverage for rounded clips, the size of the current surface.
    mask: Option<Arc<Mask>>,
}

enum Group {
    /// A clip; `bytes` of scratch were taken for its mask.
    Clip { bytes: usize },
    /// An opacity layer.
    Layer {
        opacity: f32,
        prev_alpha: f32,
        bytes: usize,
    },
    /// An opacity group applied per item (or invisible: alpha 0 pushes an
    /// empty clip instead).
    Flat { prev_alpha: f32, hidden: bool },
    /// A scroll offset.
    Scroll { prev: (i64, i64) },
}

struct Painter<'a> {
    /// The output.
    root: &'a mut Pixmap,
    /// Open opacity layers, innermost last.
    surfaces: Vec<Surface>,
    clips: Vec<ClipState>,
    groups: Vec<Group>,
    scroll: (i64, i64),
    /// Opacity of flattened groups, multiplied into every item.
    alpha: f32,
    scale: f32,
    origin: (f32, f32),
    scratch: usize,
    limits: RasterLimits,
    glyphs: &'a dyn GlyphRasterizer,
}

impl Painter<'_> {
    fn item(&mut self, item: &DisplayItem) {
        match item {
            DisplayItem::Fill(f) => self.fill(f),
            DisplayItem::Border(b) => self.border(b),
            DisplayItem::Text(t) => self.text(t),
            DisplayItem::Image(i) => self.image(i),
            DisplayItem::BackgroundImage(b) => self.background(b),
            DisplayItem::HitRegion { .. } => {}
            DisplayItem::PushClip(shape) => self.push_clip(shape),
            DisplayItem::PushOpacity(a) => self.push_opacity(*a),
            DisplayItem::PushScroll { dx, dy } => {
                let prev = self.scroll;
                self.scroll = (
                    prev.0.saturating_add(i64::from(*dx)),
                    prev.1.saturating_add(i64::from(*dy)),
                );
                self.groups.push(Group::Scroll { prev });
            }
            DisplayItem::PopClip | DisplayItem::PopOpacity | DisplayItem::PopScroll => {
                self.pop_group();
            }
        }
    }

    // ----- coordinates -----

    fn dx(&self, x: f32) -> f32 {
        (x + self.scroll.0 as f32 - self.origin.0) * self.scale
    }

    fn dy(&self, y: f32) -> f32 {
        (y + self.scroll.1 as f32 - self.origin.1) * self.scale
    }

    /// A CSS rect in root device px with snapped edges.
    fn snap_rect(&self, r: &Rect) -> DRect {
        DRect {
            l: snap(self.dx(r.x)),
            t: snap(self.dy(r.y)),
            r: snap(self.dx(r.right())),
            b: snap(self.dy(r.bottom())),
        }
    }

    fn snap_rrect(&self, rr: &RoundedRect) -> DRRect {
        let rect = self.snap_rect(&rr.rect);
        let radii = scaled_radii(
            &rr.radii.normalized(rr.rect.width, rr.rect.height),
            self.scale,
        );
        // Snapping can shrink the box a little; renormalize so the corners
        // still fit.
        DRRect {
            rect,
            radii: fit_radii(radii, rect.width(), rect.height()),
        }
    }

    fn clip(&self) -> &ClipState {
        // There is always a root clip; this fallback is never used.
        static NONE: ClipState = ClipState {
            rect: IRect::EMPTY,
            mask: None,
        };
        self.clips.last().unwrap_or(&NONE)
    }

    fn surface_origin(&self) -> (i32, i32) {
        self.surfaces.last().map_or((0, 0), |s| (s.ox, s.oy))
    }

    /// The pixmap being drawn to.
    fn target(&mut self) -> &mut Pixmap {
        match self.surfaces.last_mut() {
            Some(s) => &mut s.pixmap,
            None => self.root,
        }
    }

    fn target_size(&self) -> (u32, u32) {
        match self.surfaces.last() {
            Some(s) => (s.pixmap.width(), s.pixmap.height()),
            None => (self.root.width(), self.root.height()),
        }
    }

    // ----- drawing -----

    /// Fills polygons (root device px) with the even-odd rule, clipped.
    fn fill_polys(&mut self, polys: &[Poly], paint: &Paint) {
        let clip = self.clip().rect.to_drect();
        let (ox, oy) = self.surface_origin();
        let mut pb = PathBuilder::new();
        let mut any = false;
        for p in polys {
            let p = clip_to_rect(p, &clip);
            if !has_area(&p) {
                continue;
            }
            let mut pts = p.iter();
            if let Some(first) = pts.next() {
                pb.move_to(first[0] - ox as f32, first[1] - oy as f32);
                for q in pts {
                    pb.line_to(q[0] - ox as f32, q[1] - oy as f32);
                }
                pb.close();
                any = true;
            }
        }
        if !any {
            return;
        }
        let Some(path) = pb.finish() else { return };
        let mask = self.clip().mask.clone();
        self.target().fill_path(
            &path,
            paint,
            FillRule::EvenOdd,
            Transform::identity(),
            mask.as_deref(),
        );
    }

    /// Fills an axis-aligned device rect (snapped), clipped.
    fn fill_drect(&mut self, r: &DRect, paint: &Paint) {
        let c = self.clip().rect.to_drect();
        let r = r.intersect(&c);
        if r.is_empty() {
            return;
        }
        let (ox, oy) = self.surface_origin();
        let Some(rect) = tiny_skia::Rect::from_ltrb(
            r.l - ox as f32,
            r.t - oy as f32,
            r.r - ox as f32,
            r.b - oy as f32,
        ) else {
            return;
        };
        let mask = self.clip().mask.clone();
        self.target()
            .fill_rect(rect, paint, Transform::identity(), mask.as_deref());
    }

    fn visible(&self, r: &DRect) -> bool {
        !IRect::enclosing(r).intersect(&self.clip().rect).is_empty()
    }

    fn fill(&mut self, f: &FillItem) {
        if f.color.a == 0 {
            return;
        }
        let shape = self.snap_rrect(&f.shape);
        if !self.visible(&shape.rect) {
            return;
        }
        let mut paint = solid_paint(f.color, self.alpha);
        if shape.is_rect() {
            paint.anti_alias = false;
            self.fill_drect(&shape.rect, &paint);
        } else {
            self.fill_polys(&[shape.poly()], &paint);
        }
    }

    fn border(&mut self, b: &BorderItem) {
        let outer = self.snap_rrect(&RoundedRect {
            rect: b.rect,
            radii: b.radii,
        });
        if !self.visible(&outer.rect) {
            return;
        }
        let clip = self.clip().rect.to_drect();
        let fills = border_fills(
            &outer,
            [&b.top, &b.right, &b.bottom, &b.left],
            self.scale,
            &clip,
        );
        for fill in fills {
            let paint = solid_paint(fill.color, self.alpha);
            self.fill_polys(&fill.polys, &paint);
        }
    }

    fn image(&mut self, i: &ImageItem) {
        self.draw_image(
            &i.image,
            &RoundedRect::from_rect(i.dest),
            &i.dest,
            false,
            false,
            i.rendering,
        );
    }

    fn background(&mut self, b: &BackgroundImageItem) {
        self.draw_image(
            &b.image,
            &b.clip,
            &b.tile,
            b.repeat_x,
            b.repeat_y,
            b.rendering,
        );
    }

    /// Paints `image` scaled into `tile`, repeated along the chosen axes,
    /// inside `clip`.
    fn draw_image(
        &mut self,
        image: &Image,
        clip: &RoundedRect,
        tile: &Rect,
        repeat_x: bool,
        repeat_y: bool,
        rendering: ImageRendering,
    ) {
        let area = self.snap_rrect(clip);
        let t = self.snap_rect(tile);
        if t.is_empty() || !self.visible(&area.rect) {
            return;
        }
        // Without repetition along an axis, paint only the tile's band.
        let huge = 1.0e9;
        let band = DRect {
            l: if repeat_x { -huge } else { t.l },
            r: if repeat_x { huge } else { t.r },
            t: if repeat_y { -huge } else { t.t },
            b: if repeat_y { huge } else { t.b },
        };
        let region = area.rect.intersect(&band);
        if region.is_empty() || !self.visible(&region) {
            return;
        }
        let (iw, ih) = (image.width(), image.height());
        let Some(src) = PixmapRef::from_bytes(image.pixels(), iw, ih) else {
            return;
        };
        let (sx, sy) = (t.width() / iw as f32, t.height() / ih as f32);
        let (ox, oy) = self.surface_origin();
        let quality = match rendering {
            ImageRendering::Pixelated => FilterQuality::Nearest,
            ImageRendering::Auto if sx == 1.0 && sy == 1.0 => FilterQuality::Nearest,
            ImageRendering::Auto => FilterQuality::Bilinear,
        };
        let spread = if repeat_x || repeat_y {
            SpreadMode::Repeat
        } else {
            SpreadMode::Pad
        };
        let transform = Transform::from_row(sx, 0.0, 0.0, sy, t.l - ox as f32, t.t - oy as f32);
        let paint = Paint {
            shader: Pattern::new(src, spread, quality, self.alpha, transform),
            anti_alias: false,
            ..Paint::default()
        };
        if area.is_rect() {
            self.fill_drect(&region, &paint);
        } else {
            let poly = clip_to_rect(&area.poly(), &region);
            let mut paint = paint;
            paint.anti_alias = true;
            self.fill_polys(&[poly], &paint);
        }
    }

    fn text(&mut self, t: &TextItem) {
        if t.color.a == 0 {
            return;
        }
        let pad = t.bounds.inflate(t.size);
        let bounds = DRect {
            l: self.dx(pad.x),
            t: self.dy(pad.y),
            r: self.dx(pad.right()),
            b: self.dy(pad.bottom()),
        };
        // The run's box padded by an em is a culling hint only: also check
        // the glyph origins, so a run with stale bounds is still drawn.
        let clip = self.clip().rect;
        if !self.visible(&bounds) {
            let any = t.glyphs.iter().any(|g| {
                let x = self.dx(t.origin.x + g.x);
                let y = self.dy(t.origin.y + g.y);
                let em = t.size * self.scale;
                (x + em) >= clip.x0 as f32
                    && (x - em) < clip.x1 as f32
                    && (y + em) >= clip.y0 as f32
                    && (y - em) < clip.y1 as f32
            });
            if !any {
                return;
            }
        }
        let (ox, oy) = self.surface_origin();
        let mask = self.clip().mask.clone();
        let alpha = (f32::from(t.color.a) * self.alpha)
            .round()
            .clamp(0.0, 255.0) as u32;
        if alpha == 0 {
            return;
        }
        let em = t.size * self.scale;
        for g in &t.glyphs {
            let x = self.dx(t.origin.x + g.x);
            let y = snap(self.dy(t.origin.y + g.y));
            // Quick reject before rasterizing: the glyph's em box.
            if x + 2.0 * em < clip.x0 as f32
                || x - em > clip.x1 as f32
                || y + em < clip.y0 as f32
                || y - 2.0 * em > clip.y1 as f32
            {
                continue;
            }
            let (whole, subpixel) = subpixel_position(x);
            let request = GlyphRequest {
                font: t.font,
                glyph: g.id,
                size: t.size,
                scale: self.scale,
                subpixel,
                synthesis: t.synthesis,
            };
            let Some(gm) = self.glyphs.rasterize_glyph(&request) else {
                continue;
            };
            if gm.is_empty() {
                continue;
            }
            let left = i64::from(whole) + i64::from(gm.left);
            let top = y as i64 - i64::from(gm.top);
            blit_mask(
                self.target(),
                (i64::from(ox), i64::from(oy)),
                &gm,
                (left, top),
                t.color,
                alpha,
                &clip,
                mask.as_deref(),
            );
        }
    }

    // ----- groups -----

    fn push_clip(&mut self, shape: &RoundedRect) {
        let s = self.snap_rrect(shape);
        let parent_rect = self.clip().rect;
        let parent_mask = self.clip().mask.clone();
        let rect = IRect::enclosing(&s.rect).intersect(&parent_rect);
        let mut bytes = 0;
        let mut mask = parent_mask.clone();
        // Out of budget, a rounded clip clips to its bounding rect only.
        if !s.is_rect()
            && !rect.is_empty()
            && let Some((m, b)) = self.make_mask(&s, &rect, parent_mask.as_deref())
        {
            mask = Some(Arc::new(m));
            bytes = b;
        }
        self.scratch = self.scratch.saturating_add(bytes);
        self.clips.push(ClipState { rect, mask });
        self.groups.push(Group::Clip { bytes });
    }

    /// A mask for a rounded clip on the current surface, intersected with
    /// the parent's mask. `None` if over budget.
    fn make_mask(&self, s: &DRRect, rect: &IRect, parent: Option<&Mask>) -> Option<(Mask, usize)> {
        let (w, h) = self.target_size();
        let bytes = usize::try_from(u64::from(w) * u64::from(h)).ok()?;
        if self.scratch.saturating_add(bytes) > self.limits.max_scratch_bytes {
            return None;
        }
        let mut mask = Mask::new(w, h)?;
        let poly = clip_to_rect(&s.poly(), &rect.to_drect());
        if !has_area(&poly) {
            return Some((mask, bytes));
        }
        let mut pb = PathBuilder::new();
        let (ox, oy) = self.surface_origin();
        let (ox, oy) = (ox as f32, oy as f32);
        let mut pts = poly.iter();
        if let Some(first) = pts.next() {
            pb.move_to(first[0] - ox, first[1] - oy);
        }
        for q in pts {
            pb.line_to(q[0] - ox, q[1] - oy);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        }
        if let Some(parent) = parent {
            for (a, b) in mask.data_mut().iter_mut().zip(parent.data()) {
                *a = mul255(*a, *b);
            }
        }
        Some((mask, bytes))
    }

    fn push_opacity(&mut self, a: f32) {
        let prev_alpha = self.alpha;
        if a >= 1.0 {
            self.groups.push(Group::Flat {
                prev_alpha,
                hidden: false,
            });
            return;
        }
        if !positive(a * prev_alpha * 255.0 - 0.5) {
            // Invisible: clip everything inside away.
            self.clips.push(ClipState {
                rect: IRect::EMPTY,
                mask: None,
            });
            self.groups.push(Group::Flat {
                prev_alpha,
                hidden: true,
            });
            return;
        }
        let rect = self.clip().rect;
        if rect.is_empty() {
            self.groups.push(Group::Flat {
                prev_alpha,
                hidden: false,
            });
            return;
        }
        let (w, h) = (rect.width(), rect.height());
        let px = u64::from(w) * u64::from(h);
        let parent_mask = self.clip().mask.clone();
        let mask_bytes = if parent_mask.is_some() { px } else { 0 };
        let bytes = usize::try_from(px * 4 + mask_bytes).unwrap_or(usize::MAX);
        let fits = self.surfaces.len() < self.limits.max_layers
            && self.scratch.saturating_add(bytes) <= self.limits.max_scratch_bytes;
        let layer = if fits { Pixmap::new(w, h) } else { None };
        let Some(pixmap) = layer else {
            // Fallback: apply the opacity to each item.
            self.alpha = prev_alpha * a;
            self.groups.push(Group::Flat {
                prev_alpha,
                hidden: false,
            });
            return;
        };
        let (pox, poy) = self.surface_origin();
        let mask = parent_mask
            .and_then(|m| crop_mask(&m, (rect.x0 - pox, rect.y0 - poy), w, h).map(Arc::new));
        self.scratch = self.scratch.saturating_add(bytes);
        self.surfaces.push(Surface {
            pixmap,
            ox: rect.x0,
            oy: rect.y0,
        });
        self.clips.push(ClipState { rect, mask });
        self.alpha = 1.0;
        self.groups.push(Group::Layer {
            opacity: a,
            prev_alpha,
            bytes,
        });
    }

    fn pop_group(&mut self) {
        let Some(group) = self.groups.pop() else {
            return;
        };
        match group {
            Group::Clip { bytes } => {
                self.clips.pop();
                self.scratch = self.scratch.saturating_sub(bytes);
            }
            Group::Scroll { prev } => self.scroll = prev,
            Group::Flat { prev_alpha, hidden } => {
                if hidden {
                    self.clips.pop();
                }
                self.alpha = prev_alpha;
            }
            Group::Layer {
                opacity,
                prev_alpha,
                bytes,
            } => {
                self.clips.pop();
                self.scratch = self.scratch.saturating_sub(bytes);
                self.alpha = prev_alpha;
                let Some(layer) = self.surfaces.pop() else {
                    return;
                };
                let (pox, poy) = self.surface_origin();
                // Contents were clipped while drawing, so no mask here.
                let paint = PixmapPaint {
                    opacity: (opacity * prev_alpha).clamp(0.0, 1.0),
                    blend_mode: BlendMode::SourceOver,
                    quality: FilterQuality::Nearest,
                };
                self.target().draw_pixmap(
                    layer.ox - pox,
                    layer.oy - poy,
                    layer.pixmap.as_ref(),
                    &paint,
                    Transform::identity(),
                    None,
                );
            }
        }
    }
}

fn solid_paint(c: Color, alpha: f32) -> Paint<'static> {
    let a = (f32::from(c.a) * alpha).round().clamp(0.0, 255.0) as u8;
    let mut paint = Paint::default();
    paint.set_color_rgba8(c.r, c.g, c.b, a);
    paint
}

fn scaled_radii(r: &CornerRadii, s: f32) -> [[f32; 2]; 4] {
    [r.top_left, r.top_right, r.bottom_right, r.bottom_left].map(|c| [c.x * s, c.y * s])
}

/// Scales radii down so that they fit a `w` x `h` box (CSS Backgrounds 3
/// §5.5).
fn fit_radii(r: [[f32; 2]; 4], w: f32, h: f32) -> [[f32; 2]; 4] {
    let [tl, tr, br, bl] = r;
    let mut f = 1.0f32;
    for (len, sum) in [
        (w, tl[0] + tr[0]),
        (w, bl[0] + br[0]),
        (h, tl[1] + bl[1]),
        (h, tr[1] + br[1]),
    ] {
        if sum > 0.0 {
            f = f.min(len / sum);
        }
    }
    let f = if f.is_finite() { f.max(0.0) } else { 0.0 };
    r.map(|c| {
        let v = [c[0] * f, c[1] * f];
        if v[0] > 0.0 && v[1] > 0.0 {
            v
        } else {
            [0.0, 0.0]
        }
    })
}

/// `a * b / 255`, rounded.
fn mul255(a: u8, b: u8) -> u8 {
    let x = u32::from(a) * u32::from(b) + 128;
    ((x + (x >> 8)) >> 8) as u8
}

/// Copies the `w` x `h` part of `m` at `at` into a new mask (outside parts
/// are zero).
fn crop_mask(m: &Mask, at: (i32, i32), w: u32, h: u32) -> Option<Mask> {
    let mut out = Mask::new(w, h)?;
    let mw = i64::from(m.width());
    let mh = i64::from(m.height());
    let w64 = i64::from(w);
    let src = m.data();
    for (row, dst) in out.data_mut().chunks_exact_mut(w as usize).enumerate() {
        let sy = i64::from(at.1) + row as i64;
        if sy < 0 || sy >= mh {
            continue;
        }
        let x0 = i64::from(at.0).max(0);
        let x1 = (i64::from(at.0) + w64).min(mw);
        if x1 <= x0 {
            continue;
        }
        let s0 = usize::try_from(sy * mw + x0).ok()?;
        let s1 = usize::try_from(sy * mw + x1).ok()?;
        let d0 = usize::try_from(x0 - i64::from(at.0)).ok()?;
        let d1 = usize::try_from(x1 - i64::from(at.0)).ok()?;
        if let (Some(s), Some(d)) = (src.get(s0..s1), dst.get_mut(d0..d1)) {
            d.copy_from_slice(s);
        }
    }
    Some(out)
}

/// Composites a glyph coverage mask in `color` (non-premultiplied, with
/// `alpha` replacing its alpha) at `pos` (root device px) onto a surface
/// whose origin is `origin`, source-over, inside `clip` and `mask`.
#[allow(clippy::too_many_arguments)]
fn blit_mask(
    pixmap: &mut Pixmap,
    origin: (i64, i64),
    glyph: &GlyphMask,
    pos: (i64, i64),
    color: Color,
    alpha: u32,
    clip: &IRect,
    mask: Option<&Mask>,
) {
    let pw = i64::from(pixmap.width());
    let gw = i64::from(glyph.width);
    let gh = i64::from(glyph.height);
    // Root-space bounds of the glyph, clipped.
    let x0 = pos.0.max(i64::from(clip.x0));
    let x1 = (pos.0 + gw).min(i64::from(clip.x1));
    let y0 = pos.1.max(i64::from(clip.y0));
    let y1 = (pos.1 + gh).min(i64::from(clip.y1));
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let mask_data = mask.map(|m| m.data());
    let data = pixmap.data_mut();
    for y in y0..y1 {
        let gy = y - pos.1;
        let sy = y - origin.1;
        for x in x0..x1 {
            let gx = x - pos.0;
            let sx = x - origin.0;
            if sx < 0 || sx >= pw || sy < 0 {
                continue;
            }
            let Ok(gi) = usize::try_from(gy * gw + gx) else {
                continue;
            };
            let Ok(si) = usize::try_from(sy * pw + sx) else {
                continue;
            };
            let mut cov = u32::from(glyph.data.get(gi).copied().unwrap_or(0));
            if let Some(md) = mask_data {
                cov = cov * u32::from(md.get(si).copied().unwrap_or(0)) / 255;
            }
            if cov == 0 {
                continue;
            }
            // Source alpha and premultiplied channels.
            let sa = div255(alpha * cov);
            if sa == 0 {
                continue;
            }
            let src = [
                div255(u32::from(color.r) * sa),
                div255(u32::from(color.g) * sa),
                div255(u32::from(color.b) * sa),
                sa,
            ];
            let Some(px) = si.checked_mul(4).and_then(|i| data.get_mut(i..i + 4)) else {
                continue;
            };
            for (d, s) in px.iter_mut().zip(src) {
                *d = (s + div255(u32::from(*d) * (255 - sa))).min(255) as u8;
            }
        }
    }
}

/// `x / 255`, rounded, for `x <= 255 * 255`.
fn div255(x: u32) -> u32 {
    let x = x + 128;
    (x + (x >> 8)) >> 8
}
