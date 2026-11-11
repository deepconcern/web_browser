//! Box decorations: backgrounds (CSS Backgrounds 3 §2), borders and
//! border radii (§3–§5), and hit regions.

use std::sync::Arc;

use wb_image::Image;
use wb_style::{
    BackgroundAttachment, BackgroundBox, BackgroundLayer, BackgroundSize, ComputedStyle, LineStyle,
    PositionComponent, PositionEdge, RepeatStyle, Rgba,
};

use super::Builder;
use crate::geom::{Color, CornerRadii, Radius, Rect, RoundedRect};
use crate::list::{
    BackgroundImageItem, BorderItem, BorderSide, BorderStyle, DisplayItem, FillItem, HitTag,
    ImageRendering,
};

/// The most items one `background-repeat: space` layer may expand to;
/// beyond it the layer is painted as `repeat`.
const MAX_SPACED_TILES: usize = 1024;

pub(crate) fn color(c: Rgba) -> Color {
    Color::rgba(c.red, c.green, c.blue, c.alpha)
}

/// Which of a box's edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeBox {
    Border,
    Padding,
    Content,
}

/// A fragment's three boxes in unscrolled page coordinates, with its
/// border widths, padding and outer radii.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoxGeometry {
    pub border_box: Rect,
    pub padding_box: Rect,
    pub content_box: Rect,
    /// Normalized outer (border edge) radii.
    pub radii: CornerRadii,
}

impl BoxGeometry {
    pub(crate) fn of(b: &Builder<'_>, i: usize) -> BoxGeometry {
        let Some(f) = b.frag(i) else {
            return BoxGeometry {
                border_box: Rect::default(),
                padding_box: Rect::default(),
                content_box: Rect::default(),
                radii: CornerRadii::default(),
            };
        };
        let u = b.info(i).unshift;
        let border_box = b.rect(&f.border_box, u);
        let padding_box = b.rect(&f.padding_box, u);
        let content_box = b.rect(&f.content_box, u);
        let radii =
            resolve_radii(&f.style, border_box).normalized(border_box.width, border_box.height);
        BoxGeometry {
            border_box,
            padding_box,
            content_box,
            radii,
        }
    }

    pub(crate) fn rect(&self, edge: EdgeBox) -> Rect {
        match edge {
            EdgeBox::Border => self.border_box,
            EdgeBox::Padding => self.padding_box,
            EdgeBox::Content => self.content_box,
        }
    }

    /// The radii of the curve of `edge`: the outer radii reduced by the
    /// widths between the border edge and `edge` (CSS Backgrounds 3
    /// §5.2), never below zero.
    pub(crate) fn inner_radii(&self, edge: EdgeBox) -> CornerRadii {
        let inner = self.rect(edge);
        let o = self.border_box;
        let top = (inner.y - o.y).max(0.0);
        let left = (inner.x - o.x).max(0.0);
        let right = (o.right() - inner.right()).max(0.0);
        let bottom = (o.bottom() - inner.bottom()).max(0.0);
        let shrink =
            |r: Radius, dx: f32, dy: f32| Radius::new((r.x - dx).max(0.0), (r.y - dy).max(0.0));
        let r = self.radii;
        CornerRadii {
            top_left: shrink(r.top_left, left, top),
            top_right: shrink(r.top_right, right, top),
            bottom_right: shrink(r.bottom_right, right, bottom),
            bottom_left: shrink(r.bottom_left, left, bottom),
        }
    }

    pub(crate) fn shape(&self, edge: EdgeBox) -> RoundedRect {
        RoundedRect::new(self.rect(edge), self.inner_radii(edge))
    }
}

/// `border-*-radius` resolved against the border box: horizontal
/// percentages against its width, vertical against its height (§5.1).
fn resolve_radii(style: &ComputedStyle, b: Rect) -> CornerRadii {
    let r = &style.border_radius;
    let one = |c: &wb_style::CornerRadius| {
        Radius::new(c.horizontal.resolve(b.width), c.vertical.resolve(b.height))
    };
    CornerRadii {
        top_left: one(&r.top_left),
        top_right: one(&r.top_right),
        bottom_right: one(&r.bottom_right),
        bottom_left: one(&r.bottom_left),
    }
}

fn border_style(s: LineStyle) -> BorderStyle {
    match s {
        LineStyle::None => BorderStyle::None,
        LineStyle::Hidden => BorderStyle::Hidden,
        LineStyle::Dotted => BorderStyle::Dotted,
        LineStyle::Dashed => BorderStyle::Dashed,
        LineStyle::Solid => BorderStyle::Solid,
        LineStyle::Double => BorderStyle::Double,
        LineStyle::Groove => BorderStyle::Groove,
        LineStyle::Ridge => BorderStyle::Ridge,
        LineStyle::Inset => BorderStyle::Inset,
        LineStyle::Outset => BorderStyle::Outset,
    }
}

fn edge_box(b: BackgroundBox) -> Option<EdgeBox> {
    match b {
        BackgroundBox::BorderBox => Some(EdgeBox::Border),
        BackgroundBox::PaddingBox => Some(EdgeBox::Padding),
        BackgroundBox::ContentBox => Some(EdgeBox::Content),
        // M1 follow-up: `background-clip: text`; such layers are skipped.
        BackgroundBox::Text => None,
    }
}

impl Builder<'_> {
    /// Background, border and hit region of box fragment `i` (Appendix E
    /// "background and borders").
    pub(crate) fn paint_decorations(&mut self, i: usize) {
        let Some(f) = self.frag(i) else { return };
        if matches!(
            f.kind,
            wb_layout::FragmentKind::Line(_) | wb_layout::FragmentKind::Text(_)
        ) || !Self::is_visible(f)
        {
            return;
        }
        self.enter(i);
        let geo = BoxGeometry::of(self, i);
        let tag = self.tag(i);
        let style = &f.style;
        let mut covered = false;
        if !self.no_background.contains(&i) {
            covered = self.paint_background(style, &geo, None, tag);
        }
        if f.node.is_some()
            && !covered
            && let Some(tag) = tag
        {
            self.list.push(DisplayItem::HitRegion {
                shape: geo.shape(EdgeBox::Border),
                tag,
            });
        }
        self.paint_border(i, &geo, tag);
        self.paint_collapsed_borders(i);
    }

    /// The border, with widths taken from the fragment's geometry (so the
    /// sides a split inline piece does not have are not drawn).
    fn paint_border(&mut self, i: usize, geo: &BoxGeometry, tag: Option<HitTag>) {
        let Some(f) = self.frag(i) else { return };
        let s = &f.style.border;
        let (o, p) = (geo.border_box, geo.padding_box);
        let side = |width: f32, side: &wb_style::BorderSide| BorderSide {
            width: width.max(0.0),
            style: border_style(side.style),
            color: color(side.color),
        };
        let item = BorderItem {
            rect: o,
            radii: geo.radii,
            top: side(p.y - o.y, &s.top),
            right: side(o.right() - p.right(), &s.right),
            bottom: side(o.bottom() - p.bottom(), &s.bottom),
            left: side(p.x - o.x, &s.left),
            tag,
        };
        if [item.top, item.right, item.bottom, item.left]
            .iter()
            .any(BorderSide::is_visible)
        {
            self.list.push(DisplayItem::Border(item));
        }
    }

    /// Hook for collapsed table borders (CSS 2.2 §17.6.2).
    ///
    // M1 follow-up (tables): when the table module resolves collapsed
    // borders onto cell fragments, paint them here. In the collapsing
    // model cell borders are painted after the backgrounds of every table
    // part (§17.5.1), which the table module can arrange by having the
    // table fragment, not the cells, carry the resolved border segments.
    fn paint_collapsed_borders(&mut self, i: usize) {
        let _ = i;
    }

    /// `background-color` and the background layers of a box, placed
    /// against `geo`. Returns whether a fill covering the border box was
    /// emitted (so no separate hit region is needed). `canvas` replaces
    /// every painting area (for the canvas background).
    fn paint_background(
        &mut self,
        style: &ComputedStyle,
        geo: &BoxGeometry,
        canvas: Option<Rect>,
        tag: Option<HitTag>,
    ) -> bool {
        let layers = &style.background_layers;
        // The color is clipped like the bottom-most layer (§2.12).
        let bottom_clip = layers
            .last()
            .map_or(Some(EdgeBox::Border), |l| edge_box(l.clip));
        let clip_shape = |edge: EdgeBox| -> RoundedRect {
            match canvas {
                Some(c) => RoundedRect::from_rect(c),
                None => geo.shape(edge),
            }
        };
        let mut covered = false;
        let bg = color(style.background_color);
        if bg.a > 0
            && let Some(edge) = bottom_clip
        {
            covered = edge == EdgeBox::Border || canvas.is_some();
            self.list.push(DisplayItem::Fill(FillItem {
                shape: clip_shape(edge),
                color: bg,
                tag,
            }));
        }
        // The first layer is on top: paint from the last.
        for layer in layers.iter().rev() {
            let wb_style::Image::Url(url) = &layer.image else {
                // M1 follow-up: gradients.
                continue;
            };
            let Some(edge) = edge_box(layer.clip) else {
                continue;
            };
            let Some(image) = self.res.url_image(url) else {
                continue;
            };
            let viewport = self.tree.viewport();
            let viewport = Rect::new(viewport.x, viewport.y, viewport.width, viewport.height);
            self.paint_background_layer(layer, image, geo, clip_shape(edge), viewport, tag);
        }
        covered
    }

    /// One `background-image` layer: resolves `background-origin`,
    /// `-size`, `-position` and `-repeat` (§2.4–§2.9) into tiles.
    fn paint_background_layer(
        &mut self,
        layer: &BackgroundLayer,
        image: Arc<Image>,
        geo: &BoxGeometry,
        clip: RoundedRect,
        viewport: Rect,
        tag: Option<HitTag>,
    ) {
        // The positioning area (§2.7, §2.8): `fixed` uses the viewport.
        // M1 follow-up: `fixed` backgrounds do not follow the viewport's
        // scroll position, and `local` is treated as `scroll`.
        let area = if layer.attachment == BackgroundAttachment::Fixed {
            viewport
        } else {
            match layer.origin {
                BackgroundBox::BorderBox => geo.border_box,
                BackgroundBox::ContentBox => geo.content_box,
                _ => geo.padding_box,
            }
        };
        let Some((w, h)) = tile_size(layer, &image, area) else {
            return;
        };
        let axis =
            |style: RepeatStyle, pos: &PositionComponent, start: f32, len: f32, tile: f32| {
                let placed = start + position(pos, len - tile);
                match style {
                    RepeatStyle::NoRepeat => Axis::Tiled(placed, false),
                    RepeatStyle::Repeat | RepeatStyle::Round => Axis::Tiled(placed, true),
                    RepeatStyle::Space => {
                        // §2.5: as many whole tiles as fit, the first and last
                        // touching the area's edges; one tile is positioned.
                        let n = (len / tile).floor();
                        if n >= 2.0 && n.is_finite() {
                            let gap = (len - n * tile) / (n - 1.0);
                            Axis::Spaced {
                                start,
                                step: tile + gap,
                                count: n.min(MAX_SPACED_TILES as f32) as usize,
                            }
                        } else {
                            Axis::Tiled(placed, false)
                        }
                    }
                }
            };
        let ax = axis(layer.repeat_x, &layer.position_x, area.x, area.width, w);
        let ay = axis(layer.repeat_y, &layer.position_y, area.y, area.height, h);
        let item = |x: f32, y: f32, rx: bool, ry: bool| {
            DisplayItem::BackgroundImage(BackgroundImageItem {
                clip,
                tile: Rect::new(x, y, w, h),
                repeat_x: rx,
                repeat_y: ry,
                image: image.clone(),
                rendering: ImageRendering::Auto,
                tag,
            })
        };
        let xs = ax.positions();
        let ys = ay.positions();
        let spaced = matches!(ax, Axis::Spaced { .. }) || matches!(ay, Axis::Spaced { .. });
        if !spaced {
            if let (Axis::Tiled(x, rx), Axis::Tiled(y, ry)) = (ax, ay) {
                self.list.push(item(x, y, rx, ry));
            }
            return;
        }
        if xs.len().saturating_mul(ys.len()) > MAX_SPACED_TILES {
            // Too many tiles: approximate with plain repetition.
            let (x, rx) = ax.first();
            let (y, ry) = ay.first();
            self.list
                .push(item(x, y, rx || ax.is_spaced(), ry || ay.is_spaced()));
            return;
        }
        for &(y, ry) in &ys {
            for &(x, rx) in &xs {
                self.list.push(item(x, y, rx, ry));
            }
        }
    }

    /// Paints the canvas: the background of fragment `source` (the root
    /// or `<body>`) over `canvas`, with images placed against the root.
    pub(crate) fn paint_canvas_background(&mut self, root: usize, source: usize, canvas: Rect) {
        let Some(sf) = self.frag(source) else { return };
        let geo = BoxGeometry::of(self, root);
        let tag = self.tag(root);
        let covered = self.paint_background(&sf.style, &geo, Some(canvas), tag);
        if !covered && let Some(tag) = tag {
            self.list.push(DisplayItem::HitRegion {
                shape: RoundedRect::from_rect(canvas),
                tag,
            });
        }
    }
}

/// Tile placement along one axis.
#[derive(Clone, Copy, Debug)]
enum Axis {
    /// One tile at this position, repeated or not.
    Tiled(f32, bool),
    /// `count` unrepeated tiles from `start`, `step` apart.
    Spaced { start: f32, step: f32, count: usize },
}

impl Axis {
    fn positions(&self) -> Vec<(f32, bool)> {
        match *self {
            Axis::Tiled(p, r) => vec![(p, r)],
            Axis::Spaced { start, step, count } => (0..count)
                .map(|k| (start + step * k as f32, false))
                .collect(),
        }
    }

    fn first(&self) -> (f32, bool) {
        match *self {
            Axis::Tiled(p, r) => (p, r),
            Axis::Spaced { start, .. } => (start, false),
        }
    }

    fn is_spaced(&self) -> bool {
        matches!(self, Axis::Spaced { .. })
    }
}

/// A `background-position` component: its offset from the start or end
/// edge, percentages of `room` (area size minus tile size, §2.6).
fn position(p: &PositionComponent, room: f32) -> f32 {
    let off = p.offset.resolve(room);
    match p.edge {
        PositionEdge::Start => off,
        PositionEdge::End => room - off,
    }
}

/// The tile size from `background-size` (§2.9) and `round` (§2.5), or
/// `None` when the image isn't drawn (a zero or non-finite size).
fn tile_size(layer: &BackgroundLayer, image: &Image, area: Rect) -> Option<(f32, f32)> {
    let (iw, ih) = (image.width() as f32, image.height() as f32);
    if !(iw > 0.0 && ih > 0.0) {
        return None;
    }
    let (aw, ah) = (area.width.max(0.0), area.height.max(0.0));
    let (mut w, mut h, auto_w, auto_h) = match &layer.size {
        BackgroundSize::Cover | BackgroundSize::Contain => {
            let sx = aw / iw;
            let sy = ah / ih;
            let s = if matches!(layer.size, BackgroundSize::Cover) {
                sx.max(sy)
            } else {
                sx.min(sy)
            };
            (iw * s, ih * s, false, false)
        }
        BackgroundSize::Explicit { width, height } => {
            let rw = width.as_ref().map(|v| v.resolve(aw).max(0.0));
            let rh = height.as_ref().map(|v| v.resolve(ah).max(0.0));
            match (rw, rh) {
                (None, None) => (iw, ih, true, true),
                (Some(w), None) => (w, ih * w / iw, false, true),
                (None, Some(h)) => (iw * h / ih, h, true, false),
                (Some(w), Some(h)) => (w, h, false, false),
            }
        }
    };
    // §2.5 `round`: scale so a whole number of tiles fits; an `auto`
    // other dimension keeps the aspect ratio.
    let round = |len: f32, tile: f32| {
        let n = (len / tile).round().max(1.0);
        len / n
    };
    let rx = layer.repeat_x == RepeatStyle::Round && w > 0.0 && aw > 0.0;
    let ry = layer.repeat_y == RepeatStyle::Round && h > 0.0 && ah > 0.0;
    if rx {
        let nw = round(aw, w);
        if !ry && auto_h {
            h = h * nw / w;
        }
        w = nw;
    }
    if ry {
        let nh = round(ah, h);
        if !rx && auto_w {
            w = w * nh / h;
        }
        h = nh;
    }
    (w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()).then_some((w, h))
}
