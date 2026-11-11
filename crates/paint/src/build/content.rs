//! Content: text runs with decorations, replaced images, list markers
//! (through their text) and form controls.

use wb_layout::{FormControl, FormControlKind, FragmentKind, ReplacedKind, TextFragment};
use wb_style::{ComputedStyle, TextDecorationStyle};
use wb_text::{Direction, FontMetrics, TextStyle};

use super::Builder;
use super::decor::{BoxGeometry, EdgeBox, color};
use crate::geom::{Color, CornerRadii, Point, Radius, Rect, RoundedRect};
use crate::list::{DisplayItem, FillItem, HitTag, ImageItem, ImageRendering, TextGlyph, TextItem};

/// The longest form-control label shaped, in bytes.
const MAX_CONTROL_TEXT: usize = 4096;
/// The most lines of a `<textarea>` drawn.
const MAX_CONTROL_LINES: usize = 1000;

/// Decoration metrics for one piece of text, in CSS px.
#[derive(Clone, Copy, Debug)]
struct DecorationMetrics {
    thickness: f32,
    /// Baseline to the underline's top edge, positive down.
    underline: f32,
    /// Baseline to the line-through's top edge, positive up.
    strikeout: f32,
}

impl DecorationMetrics {
    fn from_font(m: Option<FontMetrics>, size: f32) -> DecorationMetrics {
        let fallback = DecorationMetrics {
            thickness: (size / 14.0).max(1.0),
            underline: size * 0.1,
            strikeout: size * 0.3,
        };
        let Some(m) = m else { return fallback };
        let ok = |v: f32| v.is_finite();
        DecorationMetrics {
            thickness: if ok(m.stroke_thickness) && m.stroke_thickness > 0.0 {
                m.stroke_thickness.max(1.0)
            } else {
                fallback.thickness
            },
            // Some fonts put the underline at or above the baseline; keep it
            // below so it doesn't strike through the text.
            underline: if ok(m.underline_offset) && m.underline_offset > 0.0 {
                m.underline_offset
            } else {
                fallback.underline
            },
            strikeout: if ok(m.strikeout_offset) && m.strikeout_offset > 0.0 {
                m.strikeout_offset
            } else {
                fallback.strikeout
            },
        }
    }
}

/// The `wb-text` shaping style for a computed style (the same mapping
/// layout uses).
pub(crate) fn text_style(style: &ComputedStyle) -> TextStyle {
    use wb_style::GenericFamily as G;
    use wb_text::GenericFamily as T;
    let families = style
        .font_family
        .iter()
        .map(|f| match f {
            wb_style::FontFamily::Named(n) => wb_text::FontFamily::Named(n.clone()),
            wb_style::FontFamily::Generic(g) => wb_text::FontFamily::Generic(match g {
                G::Serif | G::UiSerif | G::Math | G::Fangsong => T::Serif,
                G::SansSerif | G::UiSansSerif | G::UiRounded | G::Emoji => T::SansSerif,
                G::Monospace | G::UiMonospace => T::Monospace,
                G::Cursive => T::Cursive,
                G::Fantasy => T::Fantasy,
                G::SystemUi => T::SystemUi,
            }),
        })
        .collect();
    let weight = if style.font_weight.is_finite() {
        wb_text::FontWeight::new(style.font_weight.clamp(1.0, 1000.0).round() as u16)
    } else {
        wb_text::FontWeight::NORMAL
    };
    let font_style = match style.font_style {
        wb_style::FontStyle::Normal => wb_text::FontStyle::Normal,
        wb_style::FontStyle::Italic => wb_text::FontStyle::Italic,
        wb_style::FontStyle::Oblique(_) => wb_text::FontStyle::Oblique,
    };
    let size = if style.font_size.is_finite() {
        style.font_size.clamp(0.0, 10_000.0)
    } else {
        0.0
    };
    let mut ts = TextStyle::new(
        wb_text::FontQuery {
            families,
            weight,
            style: font_style,
            stretch: wb_text::FontStretch::from_percentage(style.font_stretch),
        },
        size,
    );
    ts.direction = Some(Direction::Ltr);
    ts
}

impl Builder<'_> {
    /// A text fragment: underline and overline, the glyph runs, then
    /// line-through (CSS Text Decoration 3 §3, CSS 2.2 Appendix E
    /// 7.2.1.4.1.1).
    pub(crate) fn paint_text(&mut self, i: usize) {
        let Some(f) = self.frag(i) else { return };
        let FragmentKind::Text(t) = &f.kind else {
            return;
        };
        if !Self::is_visible(f) {
            return;
        }
        self.enter(i);
        let u = self.info(i).unshift;
        let bx = self.rect(&f.border_box, u);
        let baseline = t.baseline + u.1;
        let tag = self.tag(i);
        let decorations = f.style.text_decorations.clone();
        let metrics = DecorationMetrics::from_font(
            t.runs
                .first()
                .and_then(|r| self.fonts.metrics(r.font, r.size)),
            f.style.font_size,
        );
        // Underlines and overlines, outermost decorating box first.
        for d in decorations.iter() {
            if d.line.underline {
                self.decoration_line(bx, baseline + metrics.underline, d, metrics, tag);
            }
            if d.line.overline {
                self.decoration_line(bx, bx.y, d, metrics, tag);
            }
        }
        self.push_runs(t, bx, u, color(f.style.color), tag);
        for d in decorations.iter() {
            if d.line.line_through {
                self.decoration_line(bx, baseline - metrics.strikeout, d, metrics, tag);
            }
        }
    }

    /// One decoration line across the fragment, its top edge at `y`.
    // M1 follow-up: dotted, dashed and wavy decorations are drawn solid.
    fn decoration_line(
        &mut self,
        bx: Rect,
        y: f32,
        d: &wb_style::TextDecoration,
        m: DecorationMetrics,
        tag: Option<HitTag>,
    ) {
        let c = color(d.color);
        let mut fill = |y: f32| {
            self.list.push(DisplayItem::Fill(FillItem {
                shape: RoundedRect::from_rect(Rect::new(bx.x, y, bx.width, m.thickness)),
                color: c,
                tag,
            }));
        };
        fill(y);
        if d.style == TextDecorationStyle::Double {
            fill(y + 2.0 * m.thickness);
        }
    }

    /// One [`TextItem`] per glyph run of `t`. `bx` is the fragment's box
    /// in page coordinates and `u` the unshift that took it there from
    /// layout coordinates.
    fn push_runs(
        &mut self,
        t: &TextFragment,
        bx: Rect,
        u: (f32, f32),
        color: Color,
        tag: Option<HitTag>,
    ) {
        // Each run's text range ends where the next run (in logical order)
        // starts.
        let mut starts: Vec<usize> = t
            .runs
            .iter()
            .filter_map(|r| r.glyphs.iter().map(|g| g.cluster).min())
            .collect();
        starts.sort_unstable();
        // The fragment's left edge in layout coordinates.
        let frag_left = bx.x - u.0;
        for run in &t.runs {
            let Some(start) = run.glyphs.iter().map(|g| g.cluster).min() else {
                continue;
            };
            let end = starts
                .iter()
                .copied()
                .find(|s| *s > start)
                .unwrap_or(t.range.end)
                .max(start);
            let left = run
                .glyphs
                .iter()
                .map(|g| g.origin.x)
                .fold(f32::INFINITY, f32::min);
            let right = run
                .glyphs
                .iter()
                .map(|g| g.origin.x + g.advance)
                .fold(f32::NEG_INFINITY, f32::max);
            let glyphs: Vec<TextGlyph> = run
                .glyphs
                .iter()
                .map(|g| TextGlyph {
                    id: g.id,
                    x: g.origin.x - frag_left,
                    y: g.origin.y - t.baseline,
                    advance: g.advance,
                    cluster: g.cluster,
                })
                .collect();
            let bounds = Rect::new(left + u.0, bx.y, (right - left).max(0.0), bx.height);
            self.list.push(DisplayItem::Text(TextItem {
                origin: Point::new(bx.x, t.baseline + u.1),
                font: run.font,
                size: run.size,
                synthesis: run.synthesis,
                color,
                glyphs,
                bounds,
                text_range: start..end,
                direction: Direction::Ltr,
                tag,
            }));
        }
    }

    /// Replaced content: an image or a form control (embedded content
    /// draws nothing in M1).
    pub(crate) fn paint_replaced(&mut self, i: usize) {
        let Some(f) = self.frag(i) else { return };
        let FragmentKind::Replaced(r) = &f.kind else {
            return;
        };
        if !Self::is_visible(f) {
            return;
        }
        self.enter(i);
        let geo = BoxGeometry::of(self, i);
        let tag = self.tag(i);
        match &r.kind {
            ReplacedKind::Image => {
                // M1 follow-up: `object-fit`/`object-position`, alt text.
                if let Some(image) = f.node.and_then(|n| self.res.node_image(n)) {
                    self.list.push(DisplayItem::Image(ImageItem {
                        dest: geo.content_box,
                        image,
                        rendering: ImageRendering::Auto,
                        tag,
                    }));
                }
            }
            ReplacedKind::FormControl(c) => self.paint_control(c, &f.style, &geo, tag),
            ReplacedKind::Embedded => {}
        }
    }

    /// Draws a form control simply (HTML §15.5): the UA style sheet gives
    /// text fields, buttons and drop-downs their border and background;
    /// this adds the label or value, the drop-down arrow, the check mark
    /// and similar.
    fn paint_control(
        &mut self,
        c: &FormControl,
        style: &ComputedStyle,
        geo: &BoxGeometry,
        tag: Option<HitTag>,
    ) {
        let fg = color(style.color);
        let content = geo.content_box;
        let gray = Color::rgb(118, 118, 118);
        let light = Color::rgb(220, 220, 220);
        let fill = |rect: Rect, radii: CornerRadii, color: Color| {
            DisplayItem::Fill(FillItem {
                shape: RoundedRect::new(rect, radii),
                color,
                tag,
            })
        };
        match c.kind {
            FormControlKind::TextField
            | FormControlKind::TextArea
            | FormControlKind::Select
            | FormControlKind::ListBox
            | FormControlKind::Button => {
                let mut text_box = content;
                if c.kind == FormControlKind::Select {
                    // A down-pointing arrow at the end: three shrinking bars.
                    let size = (style.font_size * 0.5).clamp(4.0, 64.0);
                    let x = content.right() - size;
                    let y = content.y + (content.height - size * 0.5) / 2.0;
                    let bar = size / 4.0;
                    for k in 0..3 {
                        let inset = bar * k as f32 * 0.75;
                        self.list.push(fill(
                            Rect::new(
                                x + inset,
                                y + bar * k as f32 * 0.6,
                                size - 2.0 * inset,
                                bar * 0.6,
                            ),
                            CornerRadii::default(),
                            fg,
                        ));
                    }
                    text_box.width = (text_box.width - size - 2.0).max(0.0);
                }
                let center = c.kind == FormControlKind::Button;
                let multi_line = matches!(c.kind, FormControlKind::TextArea);
                self.list
                    .push(DisplayItem::PushClip(geo.shape(EdgeBox::Padding)));
                self.control_text(&c.text, style, text_box, center, multi_line, fg, tag);
                self.list.push(DisplayItem::PopClip);
            }
            FormControlKind::Checkbox | FormControlKind::Radio => {
                let radio = c.kind == FormControlKind::Radio;
                let pad = geo.padding_box;
                let round = |r: Rect| {
                    if radio {
                        CornerRadii::uniform(Radius::circular(r.width.max(r.height) / 2.0))
                    } else {
                        CornerRadii::default()
                    }
                };
                if style.background_color.alpha == 0 {
                    self.list.push(fill(pad, round(pad), Color::WHITE));
                }
                if c.checked {
                    let inset = (pad.width.min(pad.height) * 0.2).max(1.0);
                    let mark = Rect::new(
                        pad.x + inset,
                        pad.y + inset,
                        (pad.width - 2.0 * inset).max(0.0),
                        (pad.height - 2.0 * inset).max(0.0),
                    );
                    self.list.push(fill(mark, round(mark), fg));
                }
            }
            FormControlKind::Range => {
                let track_h = 4.0f32.min(content.height);
                let track = Rect::new(
                    content.x,
                    content.y + (content.height - track_h) / 2.0,
                    content.width,
                    track_h,
                );
                self.list.push(fill(
                    track,
                    CornerRadii::uniform(Radius::circular(2.0)),
                    light,
                ));
                // The default value is the midpoint (HTML §4.10.5.1.13).
                let thumb = content.height.min(16.0);
                let r = Rect::new(
                    content.x + (content.width - thumb) / 2.0,
                    content.y + (content.height - thumb) / 2.0,
                    thumb,
                    thumb,
                );
                self.list.push(fill(
                    r,
                    CornerRadii::uniform(Radius::circular(thumb / 2.0)),
                    gray,
                ));
            }
            FormControlKind::Color => {
                // The default value is black (HTML §4.10.5.1.15).
                let inset = 2.0f32.min(content.width / 2.0).min(content.height / 2.0);
                let swatch = Rect::new(
                    content.x + inset,
                    content.y + inset,
                    content.width - 2.0 * inset,
                    content.height - 2.0 * inset,
                );
                self.list
                    .push(fill(swatch, CornerRadii::default(), Color::BLACK));
            }
            FormControlKind::Progress | FormControlKind::Meter => {
                // M1 follow-up: the value bar.
                let r = CornerRadii::uniform(Radius::circular(content.height / 2.0));
                self.list.push(fill(content, r, light));
            }
        }
    }

    /// Shapes and draws a control's label: one line vertically centered
    /// (centered horizontally for buttons), or lines from the top for a
    /// textarea.
    #[allow(clippy::too_many_arguments)]
    fn control_text(
        &mut self,
        text: &str,
        style: &ComputedStyle,
        area: Rect,
        center: bool,
        multi_line: bool,
        color: Color,
        tag: Option<HitTag>,
    ) {
        let mut end = text.len().min(MAX_CONTROL_TEXT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let text = text.get(..end).unwrap_or("");
        if text.is_empty() {
            return;
        }
        let ts = text_style(style);
        let metrics = self
            .fonts
            .first_available_font(&ts.font)
            .and_then(|f| self.fonts.metrics(f, ts.size));
        let (ascent, descent) =
            metrics.map_or((ts.size * 0.8, ts.size * 0.2), |m| (m.ascent, m.descent));
        let line_h = style.line_height_px().max(ascent + descent);
        let lines: Vec<&str> = if multi_line {
            text.split('\n').take(MAX_CONTROL_LINES).collect()
        } else {
            vec![text]
        };
        for (n, line) in lines.iter().enumerate() {
            let top = if multi_line {
                area.y + n as f32 * line_h
            } else {
                area.y + (area.height - (ascent + descent)) / 2.0
            };
            if top > area.bottom() {
                break;
            }
            let shaped = self.fonts.shape(line, &ts);
            let width: f32 = shaped.runs.iter().map(|r| r.advance).sum();
            let mut pen = if center {
                area.x + (area.width - width) / 2.0
            } else {
                area.x
            };
            let baseline = top + ascent;
            for run in &shaped.runs {
                let mut x = 0.0;
                let glyphs: Vec<TextGlyph> = run
                    .glyphs
                    .iter()
                    .map(|g| {
                        let glyph = TextGlyph {
                            id: g.id,
                            x: x + g.x_offset,
                            y: g.y_offset,
                            advance: g.advance,
                            cluster: g.cluster,
                        };
                        x += g.advance;
                        glyph
                    })
                    .collect();
                self.list.push(DisplayItem::Text(TextItem {
                    origin: Point::new(pen, baseline),
                    font: run.font,
                    size: run.size,
                    synthesis: run.synthesis,
                    color,
                    glyphs,
                    bounds: Rect::new(pen, top, run.advance, ascent + descent),
                    text_range: run.range.clone(),
                    direction: run.direction,
                    tag,
                }));
                pen += run.advance;
            }
        }
    }
}
