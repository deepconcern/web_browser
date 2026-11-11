//! Preparing an inline formatting context: collecting its inline-level
//! content into one paragraph, white-space processing phase I (CSS Text 3
//! §4.1.1), `text-transform` (§2.1), shaping, and line-break
//! opportunities (UAX #14 through `wb-text`, adjusted for `white-space`
//! and `word-break`, CSS Text 3 §5).

use std::ops::Range;
use std::sync::Arc;

use wb_style::{ComputedStyle, TextTransform, WhiteSpace, WordBreak};
use wb_text::{BreakOpportunity, ShapedRun, break_opportunities};

use crate::box_tree::{BoxId, BoxKind, MAX_BOX_DEPTH};
use crate::context::LayoutCtx;
use crate::geom::sane;

/// The character standing for an atomic inline in the paragraph text
/// (UAX #14 class CB: break opportunities before and after).
pub(crate) const OBJECT_CHAR: char = '\u{FFFC}';

/// The most bytes of rendered text one inline formatting context holds;
/// the rest is dropped (bounds shaping and line breaking work).
pub(crate) const MAX_PARAGRAPH_BYTES: usize = 8 << 20;

/// A run of text from one text box, after white-space processing.
#[derive(Debug)]
pub(crate) struct TextItem {
    /// The rendered text of this item (a slice of the paragraph).
    pub text: Arc<str>,
    /// Glyph runs, clusters relative to [`TextItem::text`].
    pub runs: Vec<ShapedRun>,
    /// Cluster start offsets (relative), sorted.
    pub clusters: Vec<usize>,
    /// `prefix[i]` is the advance of clusters `0..i`, including letter-
    /// and word-spacing; `prefix.len() == clusters.len() + 1`.
    pub prefix: Vec<f32>,
    /// `(rendered, source)` anchors (see `TextFragment::offset_map`).
    pub offset_map: Arc<[(usize, usize)]>,
}

impl TextItem {
    fn index_of(&self, local: usize) -> usize {
        self.clusters.partition_point(|c| *c < local)
    }

    /// The advance of the clusters starting in `a..b` (relative offsets).
    pub(crate) fn measure(&self, a: usize, b: usize) -> f32 {
        if b <= a {
            return 0.0;
        }
        let pa = self.prefix.get(self.index_of(a)).copied().unwrap_or(0.0);
        let pb = self.prefix.get(self.index_of(b)).copied().unwrap_or(0.0);
        (pb - pa).max(0.0)
    }

    /// The clusters starting in `a..b`: `(offset, end, advance)`.
    pub(crate) fn clusters_in(
        &self,
        a: usize,
        b: usize,
    ) -> impl Iterator<Item = (usize, usize, f32)> + '_ {
        let start = self.index_of(a);
        let end = self.index_of(b);
        (start..end).filter_map(move |i| {
            let off = *self.clusters.get(i)?;
            let next = self.clusters.get(i + 1).copied().unwrap_or(self.text.len());
            let adv = self.prefix.get(i + 1)? - self.prefix.get(i)?;
            Some((off, next, adv))
        })
    }

    /// Maps a relative rendered offset to the source text.
    pub(crate) fn source_offset(&self, rendered: usize) -> usize {
        let i = self.offset_map.partition_point(|(r, _)| *r <= rendered);
        match i.checked_sub(1).and_then(|i| self.offset_map.get(i)) {
            Some((r, s)) => s.saturating_add(rendered - r),
            None => rendered,
        }
    }
}

/// What one paragraph item is.
#[derive(Debug)]
pub(crate) enum ItemKind {
    /// Text.
    Text(TextItem),
    /// The start of an inline box.
    Open,
    /// The end of an inline box.
    Close,
    /// An atomic inline (one [`OBJECT_CHAR`]).
    Atomic,
    /// A forced line break: `<br>` or a preserved newline (one `\n`).
    ForcedBreak,
    /// A preserved tab (one `\t`), as wide as needed to reach the next tab
    /// stop.
    Tab,
    /// A `<wbr>` (one U+200B, zero width).
    WordBreak,
}

/// One item of the paragraph.
#[derive(Debug)]
pub(crate) struct Item {
    pub kind: ItemKind,
    /// The box the item belongs to (the text box, inline box, atomic box).
    pub box_id: BoxId,
    /// The style of the text or box.
    pub style: Arc<ComputedStyle>,
    /// The bytes of the paragraph text the item covers (empty for
    /// open/close).
    pub range: Range<usize>,
    /// The `white-space` that decides break opportunities after this
    /// item's characters (for atomics, their parent's).
    pub white_space: WhiteSpace,
}

/// A prepared inline formatting context.
#[derive(Debug)]
pub(crate) struct Prepared {
    /// The paragraph text.
    pub text: String,
    /// The items in order.
    pub items: Vec<Item>,
    /// Break opportunities, sorted, unique; the last is at `text.len()`
    /// when the text is not empty.
    pub breaks: Vec<BreakOpportunity>,
    /// The width of a space in each tab item's style (for tab stops),
    /// indexed like `items` (0 for other items).
    pub tab_space: Vec<f32>,
}

struct Collector<'c, 'a> {
    ctx: &'c mut LayoutCtx<'a>,
    text: String,
    items: Vec<Item>,
    tab_space: Vec<f32>,
    /// The last character emitted was a collapsible space (also true at
    /// the start and after forced breaks: phase II removes spaces there).
    prev_space: bool,
    /// The last character was part of a word (for `capitalize`).
    prev_word: bool,
    full: bool,
}

/// Collects and prepares the inline content of block container `id`.
pub(crate) fn prepare(ctx: &mut LayoutCtx<'_>, id: BoxId) -> Prepared {
    let Some(b) = ctx.get(id) else {
        return Prepared {
            text: String::new(),
            items: Vec::new(),
            breaks: Vec::new(),
            tab_space: Vec::new(),
        };
    };
    let root_ws = b.style.white_space;
    let mut c = Collector {
        ctx,
        text: String::new(),
        items: Vec::new(),
        tab_space: Vec::new(),
        prev_space: true,
        prev_word: false,
        full: false,
    };
    c.collect(&b.children, root_ws, 0);
    let breaks = compute_breaks(&c.text, &c.items);
    Prepared {
        text: c.text,
        items: c.items,
        breaks,
        tab_space: c.tab_space,
    }
}

impl Collector<'_, '_> {
    fn push_item(
        &mut self,
        kind: ItemKind,
        box_id: BoxId,
        style: Arc<ComputedStyle>,
        s: &str,
        ws: WhiteSpace,
    ) {
        let start = self.text.len();
        self.text.push_str(s);
        self.items.push(Item {
            kind,
            box_id,
            style,
            range: start..self.text.len(),
            white_space: ws,
        });
        self.tab_space.push(0.0);
    }

    fn collect(&mut self, children: &[BoxId], parent_ws: WhiteSpace, depth: usize) {
        for &cid in children {
            if self.full || depth > MAX_BOX_DEPTH {
                self.ctx.truncated = true;
                return;
            }
            if self.text.len() > MAX_PARAGRAPH_BYTES {
                self.full = true;
                self.ctx.truncated = true;
                return;
            }
            let Some(b) = self.ctx.get(cid) else { continue };
            let style = b.style.clone();
            match &b.kind {
                BoxKind::Text { text } => self.text_box(cid, &style, text),
                BoxKind::LineBreak => {
                    self.push_item(ItemKind::ForcedBreak, cid, style, "\n", parent_ws);
                    self.prev_space = true;
                    self.prev_word = false;
                }
                BoxKind::WordBreakOpportunity => {
                    self.push_item(ItemKind::WordBreak, cid, style, "\u{200B}", parent_ws);
                }
                _ if b.is_inline_box() => {
                    let ws = style.white_space;
                    self.push_item(ItemKind::Open, cid, style.clone(), "", ws);
                    self.collect(&b.children, ws, depth + 1);
                    self.push_item(ItemKind::Close, cid, style, "", ws);
                }
                _ => {
                    // Atomic inlines (and, defensively, any block-level box
                    // that ends up here) are one unbreakable object.
                    let mut buf = [0u8; 4];
                    self.push_item(
                        ItemKind::Atomic,
                        cid,
                        style,
                        OBJECT_CHAR.encode_utf8(&mut buf),
                        parent_ws,
                    );
                    self.prev_space = false;
                    self.prev_word = false;
                }
            }
        }
    }

    /// White-space processing phase I and `text-transform` for one text
    /// box, splitting it at preserved tabs and newlines.
    fn text_box(&mut self, id: BoxId, style: &Arc<ComputedStyle>, source: &str) {
        let ws = style.white_space;
        let collapse_spaces = matches!(
            ws,
            WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
        );
        let preserve_newlines = !matches!(ws, WhiteSpace::Normal | WhiteSpace::Nowrap);
        let mut buf = String::new();
        let mut map: Vec<(usize, usize)> = Vec::new();
        let mut map_anchor: Option<(usize, usize)> = None;
        for (src, ch) in source.char_indices() {
            if self.text.len() + buf.len() > MAX_PARAGRAPH_BYTES {
                self.full = true;
                self.ctx.truncated = true;
                break;
            }
            let is_newline = ch == '\n';
            if is_newline && preserve_newlines {
                self.flush_text(id, style, &mut buf, &mut map, &mut map_anchor);
                self.push_item(ItemKind::ForcedBreak, id, style.clone(), "\n", ws);
                self.prev_space = true;
                self.prev_word = false;
                continue;
            }
            if ch == '\r' && !collapse_spaces {
                // A lone carriage return in preserved text is dropped.
                continue;
            }
            if matches!(ch, ' ' | '\t' | '\n' | '\r') {
                self.prev_word = false;
                if collapse_spaces {
                    if self.prev_space {
                        continue;
                    }
                    self.prev_space = true;
                    emit(&mut buf, &mut map, &mut map_anchor, src, " ");
                } else if ch == '\t' {
                    self.flush_text(id, style, &mut buf, &mut map, &mut map_anchor);
                    let space = space_width(self.ctx, style);
                    self.push_item(ItemKind::Tab, id, style.clone(), "\t", ws);
                    if let Some(t) = self.tab_space.last_mut() {
                        *t = space;
                    }
                    self.prev_space = false;
                } else {
                    self.prev_space = false;
                    emit(&mut buf, &mut map, &mut map_anchor, src, " ");
                }
                continue;
            }
            self.prev_space = false;
            let word = ch.is_alphanumeric();
            let mut tmp = [0u8; 4];
            match style.text_transform {
                TextTransform::None => emit(
                    &mut buf,
                    &mut map,
                    &mut map_anchor,
                    src,
                    ch.encode_utf8(&mut tmp),
                ),
                TextTransform::Uppercase => {
                    let s: String = ch.to_uppercase().collect();
                    emit(&mut buf, &mut map, &mut map_anchor, src, &s);
                }
                TextTransform::Lowercase => {
                    let s: String = ch.to_lowercase().collect();
                    emit(&mut buf, &mut map, &mut map_anchor, src, &s);
                }
                TextTransform::Capitalize => {
                    if word && !self.prev_word {
                        let s: String = ch.to_uppercase().collect();
                        emit(&mut buf, &mut map, &mut map_anchor, src, &s);
                    } else {
                        emit(
                            &mut buf,
                            &mut map,
                            &mut map_anchor,
                            src,
                            ch.encode_utf8(&mut tmp),
                        );
                    }
                }
            }
            self.prev_word = word;
        }
        self.flush_text(id, style, &mut buf, &mut map, &mut map_anchor);
    }

    fn flush_text(
        &mut self,
        id: BoxId,
        style: &Arc<ComputedStyle>,
        buf: &mut String,
        map: &mut Vec<(usize, usize)>,
        anchor: &mut Option<(usize, usize)>,
    ) {
        if buf.is_empty() {
            return;
        }
        let text = std::mem::take(buf);
        // Offsets are relative to this piece (`buf` starts empty).
        let offset_map = std::mem::take(map);
        *anchor = None;
        let item = shape_item(self.ctx, style, &text, offset_map);
        let ws = style.white_space;
        self.push_item(ItemKind::Text(item), id, style.clone(), &text, ws);
    }
}

/// Appends `s` (the rendering of the source character at `src`) to `buf`,
/// recording an offset-map anchor when the mapping stops being linear.
fn emit(
    buf: &mut String,
    map: &mut Vec<(usize, usize)>,
    anchor: &mut Option<(usize, usize)>,
    src: usize,
    s: &str,
) {
    let r = buf.len();
    let linear = anchor.is_some_and(|(ar, asrc)| asrc + (r - ar) == src);
    if !linear {
        map.push((r, src));
        *anchor = Some((r, src));
    }
    buf.push_str(s);
    if s.chars().nth(1).is_some() {
        // A multi-character transform: force a new anchor next time.
        *anchor = None;
    }
}

fn space_width(ctx: &mut LayoutCtx<'_>, style: &Arc<ComputedStyle>) -> f32 {
    let s = ctx.text.shape(" ", style);
    s.clusters.iter().map(|(_, a)| *a).sum::<f32>()
        + sane(style.word_spacing)
        + sane(style.letter_spacing)
}

/// Shapes one text item and precomputes its cluster advances, adding
/// `letter-spacing` after every character and `word-spacing` to every
/// word separator (CSS Text 3 §8).
fn shape_item(
    ctx: &mut LayoutCtx<'_>,
    style: &Arc<ComputedStyle>,
    text: &str,
    offset_map: Vec<(usize, usize)>,
) -> TextItem {
    let shaped = ctx.text.shape(text, style);
    let letter = sane(style.letter_spacing);
    let word = sane(style.word_spacing);
    let mut clusters = Vec::with_capacity(shaped.clusters.len());
    let mut prefix = Vec::with_capacity(shaped.clusters.len() + 1);
    prefix.push(0.0f32);
    let mut acc = 0.0f32;
    let n = shaped.clusters.len();
    for i in 0..n {
        let Some((off, adv)) = shaped.clusters.get(i).copied() else {
            continue;
        };
        let end = shaped
            .clusters
            .get(i + 1)
            .map_or(text.len(), |(o, _)| *o)
            .max(off);
        let slice = text.get(off..end).unwrap_or("");
        let mut extra = 0.0;
        if letter != 0.0 || word != 0.0 {
            for ch in slice.chars() {
                extra += letter;
                if matches!(
                    ch,
                    ' ' | '\u{A0}' | '\u{1361}' | '\u{10100}' | '\u{10101}' | '\u{1039F}'
                ) {
                    extra += word;
                }
            }
        }
        acc = sane(acc + adv + extra);
        clusters.push(off);
        prefix.push(acc);
    }
    TextItem {
        text: Arc::from(text),
        runs: shaped.runs,
        clusters,
        prefix,
        offset_map: Arc::from(offset_map),
    }
}

/// The break opportunities of the paragraph: UAX #14, with soft breaks
/// removed after characters whose `white-space` forbids wrapping (`pre`,
/// `nowrap`), and added between characters of `word-break: break-all`
/// text (CSS Text 3 §5.2, §5.4).
// M1 follow-up: `word-break: keep-all` and `line-break` tailorings.
fn compute_breaks(text: &str, items: &[Item]) -> Vec<BreakOpportunity> {
    let raw = break_opportunities(text);
    let mut out: Vec<BreakOpportunity> = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    for b in raw {
        // The item holding the character before the break.
        while items
            .get(i)
            .is_some_and(|it| it.range.end < b.offset || it.range.is_empty())
        {
            i += 1;
        }
        let Some(it) = items.get(i) else {
            out.push(b);
            continue;
        };
        let nowrap = matches!(it.white_space, WhiteSpace::Pre | WhiteSpace::Nowrap);
        if b.mandatory || !nowrap {
            out.push(b);
        }
    }
    let mut extra = Vec::new();
    for it in items {
        if let ItemKind::Text(t) = &it.kind
            && matches!(it.style.word_break, WordBreak::BreakAll)
            && !matches!(it.white_space, WhiteSpace::Pre | WhiteSpace::Nowrap)
        {
            for (off, _) in t.text.char_indices().skip(1) {
                extra.push(BreakOpportunity {
                    offset: it.range.start + off,
                    mandatory: false,
                });
            }
        }
    }
    if !extra.is_empty() {
        out.extend(extra);
        out.sort_by(|a, b| a.offset.cmp(&b.offset).then(b.mandatory.cmp(&a.mandatory)));
        out.dedup_by_key(|b| b.offset);
    }
    out
}
