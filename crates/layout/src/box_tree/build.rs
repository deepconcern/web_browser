//! Box generation (CSS Display 3 §2, CSS 2.2 §9.2, CSS Lists 3, CSS
//! Generated Content 3, HTML §15 for replaced elements and form controls).

use std::collections::HashMap;
use std::sync::Arc;

use wb_dom::{Document, ElementData, Namespace, NodeData, NodeId};
use wb_style::{
    ComputedStyle, Content, ContentItem, Display, DisplayInside, DisplayInternal, DisplayOutside,
    ListStylePosition, ListStyleType, Overflow, PseudoElement, StyledDocument, WhiteSpace,
};

use super::{
    BoxId, BoxKind, BoxTree, FormControl, FormControlKind, Formatting, LayoutBox, Level,
    MAX_BOX_DEPTH, ReplacedKind, SplitEdges,
};
use crate::formatting::marker::marker_text;

/// A box produced for a parent, tagged with its level.
#[derive(Clone, Copy, Debug)]
enum Item {
    Inline(BoxId),
    Block(BoxId),
}

struct Builder<'a> {
    doc: &'a Document,
    styles: &'a StyledDocument,
    tree: BoxTree,
    /// HTML ordinal values of `li` elements (HTML §4.4.8).
    ordinals: HashMap<NodeId, i64>,
    /// Anonymous box styles, keyed by the parent style's address.
    anon_styles: HashMap<usize, Arc<ComputedStyle>>,
    /// The element whose `overflow` was propagated to the viewport (CSS
    /// Overflow 3 §3.3); its own box behaves as `overflow: visible`.
    overflow_propagated_from: Option<NodeId>,
}

/// Builds the box tree of `doc`.
pub(crate) fn build_box_tree(doc: &Document, styles: &StyledDocument) -> BoxTree {
    let mut b = Builder {
        doc,
        styles,
        tree: BoxTree::default(),
        ordinals: list_ordinals(doc),
        anon_styles: HashMap::new(),
        overflow_propagated_from: None,
    };
    let Some(root) = styles.root_element().or_else(|| doc.document_element()) else {
        return b.tree;
    };
    b.propagate_overflow(root);
    let mut items = Vec::new();
    b.element(root, 0, &mut items);
    // The root box is always block-level and establishes a BFC (CSS
    // Display 3 §2.7 blockifies it).
    let root_box = items.first().map(|i| match *i {
        Item::Inline(id) | Item::Block(id) => id,
    });
    if let Some(rb) = root_box.and_then(|id| b.tree.get_mut(id)) {
        rb.level = Level::Block;
        if let BoxKind::Container { formatting, .. } = &mut rb.kind
            && *formatting == Formatting::Flow
        {
            *formatting = Formatting::FlowRoot;
        }
    }
    // `display: contents` on the root leaves several items; wrap them.
    if items.len() > 1 || items.iter().any(|i| matches!(i, Item::Inline(_))) {
        if let Some(style) = styles.style(root).cloned() {
            let anon_style = b.anon_style(&style, Display::BLOCK);
            let ids = b.fix_up_block_children(items, &anon_style);
            let inline = ids.iter().all(|i| b.is_inline(*i)) && !ids.is_empty();
            b.tree.root = b.push_box(LayoutBox {
                node: Some(root),
                pseudo: None,
                anonymous: true,
                style: anon_style,
                level: Level::Block,
                kind: BoxKind::Container {
                    formatting: Formatting::FlowRoot,
                    inline_children: inline,
                },
                parent: None,
                children: Vec::new(),
                marker: None,
                split: SplitEdges::WHOLE,
            });
            if let Some(r) = b.tree.root {
                b.set_children(r, ids);
            }
        }
    } else {
        b.tree.root = root_box;
    }
    b.tree
}

impl Builder<'_> {
    fn push_box(&mut self, b: LayoutBox) -> Option<BoxId> {
        self.tree.push(b)
    }

    fn set_children(&mut self, parent: BoxId, children: Vec<BoxId>) {
        for c in &children {
            if let Some(cb) = self.tree.get_mut(*c) {
                cb.parent = Some(parent);
            }
        }
        if let Some(p) = self.tree.get_mut(parent) {
            p.children = children;
        }
    }

    fn is_inline(&self, id: BoxId) -> bool {
        self.tree.get(id).is_some_and(|b| b.level == Level::Inline)
    }

    /// CSS Overflow 3 §3.3: the root's `overflow` applies to the viewport;
    /// if it is `visible` on both axes and the root is `<html>`, `<body>`'s
    /// is used instead. The element it came from then behaves as visible.
    fn propagate_overflow(&mut self, root: NodeId) {
        let visible = |s: &ComputedStyle| {
            matches!(s.overflow_x, Overflow::Visible) && matches!(s.overflow_y, Overflow::Visible)
        };
        let Some(rs) = self.styles.style(root) else {
            return;
        };
        if !visible(rs) {
            self.tree.viewport_overflow = (rs.overflow_x, rs.overflow_y);
            self.overflow_propagated_from = Some(root);
            return;
        }
        if !self.doc.is_html_element(root, "html") {
            return;
        }
        let body = self
            .doc
            .element_children(root)
            .find(|c| self.doc.is_html_element(*c, "body"));
        if let Some(body) = body
            && let Some(bs) = self.styles.style(body)
            && !visible(bs)
        {
            self.tree.viewport_overflow = (bs.overflow_x, bs.overflow_y);
            self.overflow_propagated_from = Some(body);
        }
    }

    /// The style used for `node`'s principal box.
    fn element_style(&self, node: NodeId) -> Option<Arc<ComputedStyle>> {
        let s = self.styles.style(node)?;
        if self.overflow_propagated_from == Some(node) {
            let mut c = ComputedStyle::clone(s);
            c.overflow_x = Overflow::Visible;
            c.overflow_y = Overflow::Visible;
            return Some(Arc::new(c));
        }
        Some(s.clone())
    }

    /// Generates the boxes for the children of `parent` (an element or a
    /// `display: contents` element) into `out`.
    fn children(&mut self, parent: NodeId, depth: usize, out: &mut Vec<Item>) {
        for child in self.doc.children(parent) {
            if self.tree.truncated {
                return;
            }
            match self.doc.data(child) {
                Some(NodeData::Text(text)) => {
                    if text.is_empty() {
                        continue;
                    }
                    let Some(style) = self.styles.style(child).cloned() else {
                        continue;
                    };
                    if let Some(id) = self.text_box(Some(child), None, style, text) {
                        out.push(Item::Inline(id));
                    }
                }
                Some(NodeData::Element(_)) => self.element(child, depth + 1, out),
                _ => {}
            }
        }
    }

    fn text_box(
        &mut self,
        node: Option<NodeId>,
        pseudo: Option<PseudoElement>,
        style: Arc<ComputedStyle>,
        text: &str,
    ) -> Option<BoxId> {
        self.push_box(LayoutBox {
            node,
            pseudo,
            anonymous: false,
            style,
            level: Level::Inline,
            kind: BoxKind::Text {
                text: Arc::from(text),
            },
            parent: None,
            children: Vec::new(),
            marker: None,
            split: SplitEdges::WHOLE,
        })
    }

    /// Generates the boxes of element `node` into `out`.
    fn element(&mut self, node: NodeId, depth: usize, out: &mut Vec<Item>) {
        if depth > MAX_BOX_DEPTH {
            self.tree.truncated = true;
            return;
        }
        let Some(style) = self.element_style(node) else {
            return;
        };
        let Some(el) = self.doc.element(node) else {
            return;
        };
        let replaced = replaced_kind(self.doc, node, el);
        match style.display {
            Display::None => return,
            Display::Contents => {
                // CSS Display 3 §2.5: replaced elements and form controls
                // with `display: contents` behave as `display: none`.
                if replaced.is_some() || is_html(el, "br") || is_html(el, "wbr") {
                    return;
                }
                self.pseudo(node, PseudoElement::Before, depth, out);
                self.children(node, depth, out);
                self.pseudo(node, PseudoElement::After, depth, out);
                return;
            }
            _ => {}
        }
        let (level, formatting, list_item) = match classify(style.display) {
            Some(c) => c,
            None => return,
        };
        if is_html(el, "br") {
            if let Some(id) = self.simple_box(node, style, BoxKind::LineBreak) {
                out.push(Item::Inline(id));
            }
            return;
        }
        if is_html(el, "wbr") {
            if let Some(id) = self.simple_box(node, style, BoxKind::WordBreakOpportunity) {
                out.push(Item::Inline(id));
            }
            return;
        }
        if let Some(kind) = replaced {
            let Some(id) = self.push_box(LayoutBox {
                node: Some(node),
                pseudo: None,
                anonymous: false,
                style,
                level,
                kind: BoxKind::Replaced(kind),
                parent: None,
                children: Vec::new(),
                marker: None,
                split: SplitEdges::WHOLE,
            }) else {
                return;
            };
            out.push(match level {
                Level::Block => Item::Block(id),
                Level::Inline => Item::Inline(id),
            });
            return;
        }
        let Some(id) = self.push_box(LayoutBox {
            node: Some(node),
            pseudo: None,
            anonymous: false,
            style: style.clone(),
            level,
            kind: BoxKind::Container {
                formatting,
                inline_children: true,
            },
            parent: None,
            children: Vec::new(),
            marker: None,
            split: SplitEdges::WHOLE,
        }) else {
            return;
        };
        let mut items = Vec::new();
        if list_item {
            // An inline list item gets an inside marker.
            let inside = matches!(style.list_style_position, ListStylePosition::Inside)
                || level == Level::Inline;
            if let Some(m) = self.marker(node, &style) {
                if inside {
                    items.push(Item::Inline(m));
                } else {
                    if let Some(mb) = self.tree.get_mut(m) {
                        mb.parent = Some(id);
                    }
                    if let Some(b) = self.tree.get_mut(id) {
                        b.marker = Some(m);
                    }
                }
            }
        }
        self.pseudo(node, PseudoElement::Before, depth, &mut items);
        self.children(node, depth, &mut items);
        self.pseudo(node, PseudoElement::After, depth, &mut items);
        self.finish_container(id, level, formatting, &style, items, out);
    }

    /// Attaches `items` to the container `id` (splitting it if it is an
    /// inline box with block-level contents) and emits it into `out`.
    fn finish_container(
        &mut self,
        id: BoxId,
        level: Level,
        formatting: Formatting,
        style: &Arc<ComputedStyle>,
        items: Vec<Item>,
        out: &mut Vec<Item>,
    ) {
        let is_inline_box = level == Level::Inline && formatting == Formatting::Flow;
        if is_inline_box {
            if items.iter().all(|i| matches!(i, Item::Inline(_))) {
                let ids = items.into_iter().map(item_id).collect();
                self.set_children(id, ids);
                out.push(Item::Inline(id));
            } else {
                self.split_inline(id, items, out);
            }
            return;
        }
        let ids = self.fix_up_block_children(items, style);
        let inline = !ids.is_empty() && ids.iter().all(|i| self.is_inline(*i));
        if let Some(b) = self.tree.get_mut(id)
            && let BoxKind::Container {
                inline_children, ..
            } = &mut b.kind
        {
            *inline_children = inline;
        }
        self.set_children(id, ids);
        out.push(match level {
            Level::Block => Item::Block(id),
            Level::Inline => Item::Inline(id),
        });
    }

    /// CSS 2.2 §9.2.1.1: an inline box containing a block-level box is
    /// broken around it; the pieces before and after go into anonymous
    /// block boxes (made by the parent's fix-up) and the block becomes a
    /// sibling of those.
    fn split_inline(&mut self, id: BoxId, items: Vec<Item>, out: &mut Vec<Item>) {
        let Some(template) = self.tree.get(id).cloned() else {
            return;
        };
        let mut groups: Vec<Result<Vec<BoxId>, BoxId>> = Vec::new();
        for item in items {
            match item {
                Item::Inline(c) => match groups.last_mut() {
                    Some(Ok(g)) => g.push(c),
                    _ => groups.push(Ok(vec![c])),
                },
                Item::Block(c) => groups.push(Err(c)),
            }
        }
        let n = groups.len();
        let mut first_piece = true;
        for (i, g) in groups.into_iter().enumerate() {
            match g {
                Ok(children) => {
                    let piece = if first_piece {
                        Some(id)
                    } else {
                        self.push_box(template.clone())
                    };
                    first_piece = false;
                    let Some(piece) = piece else { return };
                    if let Some(b) = self.tree.get_mut(piece) {
                        b.split = SplitEdges {
                            start: i == 0 && template.split.start,
                            end: i + 1 == n && template.split.end,
                        };
                    }
                    self.set_children(piece, children);
                    out.push(Item::Inline(piece));
                }
                Err(block) => out.push(Item::Block(block)),
            }
        }
    }

    /// CSS 2.2 §9.2.1.1: if a block container has both block-level and
    /// inline-level children, runs of inline-level children are wrapped
    /// in anonymous block boxes. Runs of collapsible white space only are
    /// dropped.
    fn fix_up_block_children(
        &mut self,
        items: Vec<Item>,
        parent_style: &Arc<ComputedStyle>,
    ) -> Vec<BoxId> {
        if !items.iter().any(|i| matches!(i, Item::Block(_))) {
            return items.into_iter().map(item_id).collect();
        }
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        for item in items {
            match item {
                Item::Inline(c) => run.push(c),
                Item::Block(c) => {
                    self.flush_run(&mut run, parent_style, &mut out);
                    out.push(c);
                }
            }
        }
        self.flush_run(&mut run, parent_style, &mut out);
        out
    }

    fn flush_run(
        &mut self,
        run: &mut Vec<BoxId>,
        parent_style: &Arc<ComputedStyle>,
        out: &mut Vec<BoxId>,
    ) {
        if run.is_empty() {
            return;
        }
        let children = std::mem::take(run);
        if children
            .iter()
            .all(|c| self.collapsible_whitespace_only(*c, 0))
        {
            return;
        }
        let style = self.anon_style(parent_style, Display::BLOCK);
        let Some(anon) = self.push_box(LayoutBox {
            node: None,
            pseudo: None,
            anonymous: true,
            style,
            level: Level::Block,
            kind: BoxKind::Container {
                formatting: Formatting::Flow,
                inline_children: true,
            },
            parent: None,
            children: Vec::new(),
            marker: None,
            split: SplitEdges::WHOLE,
        }) else {
            return;
        };
        self.set_children(anon, children);
        out.push(anon);
    }

    /// Whether the inline-level box `id` holds only white space that the
    /// white-space processing rules would collapse away (CSS Text 3
    /// §4.1.1), so it generates no anonymous block.
    fn collapsible_whitespace_only(&self, id: BoxId, depth: usize) -> bool {
        let Some(b) = self.tree.get(id) else {
            return true;
        };
        match &b.kind {
            BoxKind::Text { text } => {
                let ws = b.style.white_space;
                let collapses_newlines = matches!(ws, WhiteSpace::Normal | WhiteSpace::Nowrap);
                let collapses_spaces = collapses_newlines || ws == WhiteSpace::PreLine;
                collapses_spaces
                    && text.chars().all(|c| match c {
                        ' ' | '\t' | '\r' => true,
                        '\n' => collapses_newlines,
                        _ => false,
                    })
            }
            BoxKind::Container { .. } if b.is_inline_box() => {
                let s = &b.style;
                let decorated = s.padding.left.resolve(0.0) != 0.0
                    || s.padding.right.resolve(0.0) != 0.0
                    || s.border.left.width != 0.0
                    || s.border.right.width != 0.0
                    || s.margin.left.resolve(0.0).unwrap_or(0.0) != 0.0
                    || s.margin.right.resolve(0.0).unwrap_or(0.0) != 0.0;
                depth < MAX_BOX_DEPTH
                    && !decorated
                    && b.children
                        .iter()
                        .all(|c| self.collapsible_whitespace_only(*c, depth + 1))
            }
            _ => false,
        }
    }

    /// The style of an anonymous box inside a box styled `parent`: the
    /// inherited properties from the parent and initial values for the
    /// rest (CSS 2.2 §9.2.1.1).
    fn anon_style(&mut self, parent: &Arc<ComputedStyle>, display: Display) -> Arc<ComputedStyle> {
        let key = Arc::as_ptr(parent) as usize;
        if let Some(s) = self.anon_styles.get(&key)
            && s.display == display
        {
            return s.clone();
        }
        let s = Arc::new(inherit_style(parent, display));
        self.anon_styles.insert(key, s.clone());
        s
    }

    fn simple_box(
        &mut self,
        node: NodeId,
        style: Arc<ComputedStyle>,
        kind: BoxKind,
    ) -> Option<BoxId> {
        self.push_box(LayoutBox {
            node: Some(node),
            pseudo: None,
            anonymous: false,
            style,
            level: Level::Inline,
            kind,
            parent: None,
            children: Vec::new(),
            marker: None,
            split: SplitEdges::WHOLE,
        })
    }

    /// Generates a `::before` or `::after` box (CSS Generated Content 3
    /// §2).
    fn pseudo(&mut self, node: NodeId, which: PseudoElement, depth: usize, out: &mut Vec<Item>) {
        if depth >= MAX_BOX_DEPTH {
            self.tree.truncated = true;
            return;
        }
        let Some(style) = self.styles.pseudo_style(node, which).cloned() else {
            return;
        };
        let Content::Items(items) = &style.content else {
            return;
        };
        let Some((level, formatting, _)) = classify(style.display) else {
            return;
        };
        let ordinal = self.ordinals.get(&node).copied();
        let text = content_text(items, ordinal);
        let Some(id) = self.push_box(LayoutBox {
            node: Some(node),
            pseudo: Some(which),
            anonymous: false,
            style: style.clone(),
            level,
            kind: BoxKind::Container {
                formatting,
                inline_children: true,
            },
            parent: None,
            children: Vec::new(),
            marker: None,
            split: SplitEdges::WHOLE,
        }) else {
            return;
        };
        let mut kids = Vec::new();
        if !text.is_empty()
            && let Some(t) = self.text_box(Some(node), Some(which), style.clone(), &text)
        {
            kids.push(Item::Inline(t));
        }
        // M1 follow-up: `content: url()` images.
        self.finish_container(id, level, formatting, &style, kids, out);
    }

    /// Creates the `::marker` box of a list item (CSS Lists 3 §3).
    fn marker(&mut self, node: NodeId, li_style: &Arc<ComputedStyle>) -> Option<BoxId> {
        let style = match self.styles.pseudo_style(node, PseudoElement::Marker) {
            Some(s) => s.clone(),
            None => {
                // No `::marker` style (e.g. the cascade did not produce
                // one): inherit from the list item.
                let mut s = inherit_style(li_style, Display::INLINE);
                s.white_space = WhiteSpace::Pre;
                Arc::new(s)
            }
        };
        let ordinal = self.ordinals.get(&node).copied().unwrap_or(1);
        let text = match &style.content {
            Content::Items(items) => content_text(items, Some(ordinal)),
            Content::None => return None,
            Content::Normal => {
                // M1 follow-up: `list-style-image` markers.
                let lst = &li_style.list_style_type;
                if matches!(lst, ListStyleType::None) {
                    return None;
                }
                marker_text(lst, ordinal)
            }
        };
        if text.is_empty() {
            return None;
        }
        let id = self.push_box(LayoutBox {
            node: Some(node),
            pseudo: Some(PseudoElement::Marker),
            anonymous: false,
            style: style.clone(),
            level: Level::Inline,
            kind: BoxKind::Container {
                formatting: Formatting::Flow,
                inline_children: true,
            },
            parent: None,
            children: Vec::new(),
            marker: None,
            split: SplitEdges::WHOLE,
        })?;
        let t = self.text_box(Some(node), Some(PseudoElement::Marker), style, &text)?;
        self.set_children(id, vec![t]);
        Some(id)
    }
}

fn item_id(i: Item) -> BoxId {
    match i {
        Item::Inline(id) | Item::Block(id) => id,
    }
}

fn is_html(el: &ElementData, name: &str) -> bool {
    el.is_html(name)
}

/// Maps a computed `display` to (level, formatting, list-item), or `None`
/// for values that generate no box here.
fn classify(display: Display) -> Option<(Level, Formatting, bool)> {
    match display {
        Display::None | Display::Contents => None,
        Display::Pair {
            outside,
            inside,
            list_item,
        } => {
            let level = match outside {
                DisplayOutside::Block | DisplayOutside::RunIn => Level::Block,
                DisplayOutside::Inline => Level::Inline,
            };
            let formatting = match inside {
                // M1 follow-up: ruby layout; ruby is laid out as inline.
                DisplayInside::Flow | DisplayInside::Ruby => Formatting::Flow,
                DisplayInside::FlowRoot => Formatting::FlowRoot,
                DisplayInside::Table => Formatting::Table,
                DisplayInside::Flex => Formatting::Flex,
                DisplayInside::Grid => Formatting::Grid,
            };
            Some((level, formatting, list_item))
        }
        Display::Internal(internal) => match internal {
            // Columns hold no content (CSS 2.2 §17.2); without table
            // layout they generate nothing.
            DisplayInternal::TableColumn | DisplayInternal::TableColumnGroup => None,
            DisplayInternal::RubyBase
            | DisplayInternal::RubyText
            | DisplayInternal::RubyBaseContainer
            | DisplayInternal::RubyTextContainer => Some((Level::Inline, Formatting::Flow, false)),
            // M1 follow-up: anonymous table wrappers (CSS 2.2 §17.2.1);
            // table-internal boxes are block containers until then.
            other => Some((Level::Block, Formatting::TableInternal(other), false)),
        },
    }
}

/// A style with `parent`'s inherited properties and initial values for the
/// rest (CSS Cascade 5 §7.2).
pub(crate) fn inherit_style(parent: &ComputedStyle, display: Display) -> ComputedStyle {
    let mut s = ComputedStyle::initial();
    s.display = display;
    s.visibility = parent.visibility;
    s.color = parent.color;
    s.cursor = parent.cursor.clone();
    s.font_family = parent.font_family.clone();
    s.font_size = parent.font_size;
    s.font_weight = parent.font_weight;
    s.font_style = parent.font_style;
    s.font_stretch = parent.font_stretch;
    s.line_height = parent.line_height;
    s.text_align = parent.text_align;
    s.text_indent = parent.text_indent.clone();
    s.text_transform = parent.text_transform;
    s.text_decorations = parent.text_decorations.clone();
    s.white_space = parent.white_space;
    s.word_break = parent.word_break;
    s.overflow_wrap = parent.overflow_wrap;
    s.letter_spacing = parent.letter_spacing;
    s.word_spacing = parent.word_spacing;
    s.list_style_type = parent.list_style_type.clone();
    s.list_style_position = parent.list_style_position;
    s.list_style_image = parent.list_style_image.clone();
    s.border_collapse = parent.border_collapse;
    s.border_spacing = parent.border_spacing;
    s.caption_side = parent.caption_side;
    s.empty_cells = parent.empty_cells;
    s.custom_properties = parent.custom_properties.clone();
    s
}

/// The text of a `content` list (CSS Generated Content 3 §1). `ordinal`
/// is the element's `list-item` counter value.
fn content_text(items: &[ContentItem], ordinal: Option<i64>) -> String {
    let mut s = String::new();
    for item in items {
        match item {
            ContentItem::String(t) | ContentItem::Attr(t) => s.push_str(t),
            ContentItem::Counter { name, style } | ContentItem::Counters { name, style, .. } => {
                // M1 follow-up: CSS counters other than `list-item`
                // (counter-reset/increment scopes); they render as 0.
                let v = if name == "list-item" {
                    ordinal.unwrap_or(0)
                } else {
                    0
                };
                s.push_str(&crate::formatting::marker::format_counter(style, v));
            }
            // M1 follow-up: nested quote depth and the `quotes` property.
            ContentItem::OpenQuote => s.push('\u{201C}'),
            ContentItem::CloseQuote => s.push('\u{201D}'),
            ContentItem::NoOpenQuote | ContentItem::NoCloseQuote | ContentItem::Url(_) => {}
        }
        if s.len() > MAX_GENERATED_TEXT {
            break;
        }
    }
    s
}

/// The most bytes of generated text one pseudo-element produces.
const MAX_GENERATED_TEXT: usize = 64 * 1024;

/// HTML §4.4.8: the ordinal value of every `li`, computed in one pass.
/// Each element's `li` children are numbered in order: an `ol` starts at
/// its `start` attribute (or, when `reversed`, its number of `li`
/// children) and counts up (down when reversed); a `value` attribute
/// resets the count. Other parents count from 1.
fn list_ordinals(doc: &Document) -> HashMap<NodeId, i64> {
    let mut out = HashMap::new();
    for parent in doc.descendants(doc.root()) {
        let Some(el) = doc.element(parent) else {
            continue;
        };
        let mut lis = doc
            .element_children(parent)
            .filter(|c| doc.is_html_element(*c, "li"))
            .peekable();
        if lis.peek().is_none() {
            continue;
        }
        let is_ol = el.is_html("ol");
        let reversed = is_ol && el.has_attribute("reversed");
        let step: i64 = if reversed { -1 } else { 1 };
        let lis: Vec<NodeId> = lis.collect();
        let start = if is_ol {
            el.attribute("start").and_then(parse_integer)
        } else {
            None
        };
        let mut next = start.unwrap_or(if reversed {
            i64::try_from(lis.len()).unwrap_or(i64::MAX)
        } else {
            1
        });
        for li in lis {
            if let Some(v) = doc.attribute(li, "value").and_then(parse_integer) {
                next = v;
            }
            out.insert(li, next);
            next = next.saturating_add(step);
        }
    }
    out
}

/// HTML §2.3.4.1 "rules for parsing integers" (leading white space, an
/// optional sign, digits; trailing garbage ignored). Values are clamped
/// to ±2^31.
pub(crate) fn parse_integer(s: &str) -> Option<i64> {
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, s.get(1..)?),
        Some(b'+') => (false, s.get(1..)?),
        _ => (false, s),
    };
    let digits: &str = rest
        .get(..rest.bytes().take_while(u8::is_ascii_digit).count())
        .unwrap_or("");
    if digits.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for b in digits.bytes() {
        v = v.saturating_mul(10).saturating_add(i64::from(b - b'0'));
        if v > 1 << 31 {
            v = 1 << 31;
        }
    }
    Some(if neg { -v } else { v })
}

/// Whether `el` is a replaced element (or a form control laid out as one),
/// and which kind (HTML §15.4, §15.5).
fn replaced_kind(doc: &Document, node: NodeId, el: &ElementData) -> Option<ReplacedKind> {
    if el.namespace == Namespace::Svg && el.local_name == "svg" {
        // The outermost `<svg>` is replaced; nested ones are not reached
        // because SVG children generate no CSS boxes here.
        return Some(ReplacedKind::Embedded);
    }
    if el.namespace != Namespace::Html {
        return None;
    }
    let attr_u32 = |name: &str, default: u32| {
        el.attribute(name)
            .and_then(parse_integer)
            .filter(|v| *v > 0)
            .map_or(default, |v| u32::try_from(v.min(10_000)).unwrap_or(default))
    };
    let control = |kind, text: String| {
        Some(ReplacedKind::FormControl(FormControl {
            kind,
            text,
            checked: el.has_attribute("checked"),
            cols: 0,
            rows: 0,
        }))
    };
    match el.local_name.as_str() {
        "img" => Some(ReplacedKind::Image),
        "iframe" | "video" | "canvas" | "embed" | "object" => Some(ReplacedKind::Embedded),
        "input" => {
            let ty = el.attribute("type").unwrap_or("text").to_ascii_lowercase();
            let value = el.attribute("value").unwrap_or("").to_string();
            match ty.as_str() {
                "hidden" => None,
                "image" => Some(ReplacedKind::Image),
                "checkbox" => control(FormControlKind::Checkbox, String::new()),
                "radio" => control(FormControlKind::Radio, String::new()),
                "range" => control(FormControlKind::Range, String::new()),
                "color" => control(FormControlKind::Color, String::new()),
                "submit" | "reset" | "button" | "file" => {
                    let label = if el.has_attribute("value") && ty != "file" {
                        value
                    } else {
                        match ty.as_str() {
                            "submit" => "Submit".to_string(),
                            "reset" => "Reset".to_string(),
                            "file" => "Choose File".to_string(),
                            _ => String::new(),
                        }
                    };
                    control(FormControlKind::Button, label)
                }
                _ => {
                    let text = if ty == "password" {
                        "\u{2022}".repeat(value.chars().count().min(4096))
                    } else {
                        value
                    };
                    let mut c = FormControl {
                        kind: FormControlKind::TextField,
                        text,
                        checked: false,
                        cols: attr_u32("size", 20),
                        rows: 1,
                    };
                    c.text.truncate(floor_char_boundary(&c.text, 4096));
                    Some(ReplacedKind::FormControl(c))
                }
            }
        }
        "textarea" => {
            let mut text = doc.text_content(node).unwrap_or_default();
            text.truncate(floor_char_boundary(&text, 4096));
            Some(ReplacedKind::FormControl(FormControl {
                kind: FormControlKind::TextArea,
                text,
                checked: false,
                cols: attr_u32("cols", 20),
                rows: attr_u32("rows", 2),
            }))
        }
        "select" => {
            let size = attr_u32("size", 0);
            let multiple = el.has_attribute("multiple");
            let list_box = multiple || size > 1;
            let rows = if list_box {
                if size > 0 { size } else { 4 }
            } else {
                1
            };
            let (text, widest) = select_texts(doc, node);
            Some(ReplacedKind::FormControl(FormControl {
                kind: if list_box {
                    FormControlKind::ListBox
                } else {
                    FormControlKind::Select
                },
                text,
                checked: false,
                // For a select, `cols` is the widest option's length in
                // characters, which sizes the control.
                cols: widest,
                rows,
            }))
        }
        "progress" => control(FormControlKind::Progress, String::new()),
        "meter" => control(FormControlKind::Meter, String::new()),
        _ => None,
    }
}

/// The selected option's text and the widest option's length in chars.
fn select_texts(doc: &Document, select: NodeId) -> (String, u32) {
    let mut first: Option<String> = None;
    let mut selected: Option<String> = None;
    let mut widest = 0usize;
    for (n, d) in doc.descendants(select).enumerate() {
        if n > 10_000 {
            break;
        }
        if !doc.is_html_element(d, "option") {
            continue;
        }
        let text = collapse_ws(&doc.text_content(d).unwrap_or_default());
        widest = widest.max(text.chars().count());
        if selected.is_none() && doc.attribute(d, "selected").is_some() {
            selected = Some(text.clone());
        }
        if first.is_none() {
            first = Some(text);
        }
    }
    let mut text = selected.or(first).unwrap_or_default();
    text.truncate(floor_char_boundary(&text, 4096));
    (text, u32::try_from(widest.min(1000)).unwrap_or(0))
}

fn collapse_ws(s: &str) -> String {
    s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut i = max;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers() {
        assert_eq!(parse_integer("  12abc"), Some(12));
        assert_eq!(parse_integer("-3"), Some(-3));
        assert_eq!(parse_integer("+7"), Some(7));
        assert_eq!(parse_integer("x"), None);
        assert_eq!(parse_integer(""), None);
        assert_eq!(parse_integer("99999999999999999999"), Some(1 << 31));
    }
}
