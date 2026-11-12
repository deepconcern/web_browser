//! The cascade ([CSS Cascade 5](https://drafts.csswg.org/css-cascade-5/))
//! and computed-value calculation for a whole document.
//!
//! For each element: collect matching selectors from the buckets, find the
//! winning declaration of every property by walking declarations from the
//! highest cascade priority down (which makes `revert` and `revert-layer`
//! simple), resolve custom properties and `var()`, then compute values in
//! dependency order (`font-size`, then `color`, then `line-height`, then the
//! rest).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use wb_css::{
    ComponentValue, Content, ContentItem, CssWideKeyword, CustomValue, Declaration,
    DeclarationBlock, DeclarationKind, Display, DisplayInside, DisplayOutside, Float, FontSize,
    FontWeight, Longhand, LonghandId, PropertyId, PseudoElement, Rgba, TextAlign,
    parse_property_value, parse_style_attribute, substitute_variables,
};
use wb_dom::{Document, ElementData, NodeData, NodeId};

use crate::bloom::{self, AncestorFilter, Kind};
use crate::computed::{
    BackgroundLayer, BackgroundSize, ComputedStyle, CornerRadius, LineHeight, PositionComponent,
    TextDecoration,
};
use crate::matching::MatchContext;
use crate::values::{self, LengthContext, sanitize};
use crate::{Origin, PseudoStyles, StyleOptions, StyledDocument, Stylist, hints};

/// Where a declaration sits in the cascade, for `revert-layer` and
/// `revert`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pos {
    origin: Origin,
    important: bool,
    /// The `style` attribute (element-attached, §6.3).
    attached: bool,
    /// Presentational hints (below every author layer).
    hint: bool,
    layer: u32,
}

#[derive(Clone, Copy, Debug)]
enum Skip {
    /// After `revert`: ignore this origin and higher ones.
    Origin(Origin),
    /// After `revert-layer`: ignore the rest of this layer.
    Layer(Pos),
}

impl Skip {
    fn excludes(self, pos: Pos) -> bool {
        match self {
            Skip::Origin(o) => pos.origin >= o,
            Skip::Layer(p) => p == pos,
        }
    }
}

#[derive(Clone, Debug)]
enum Winner<'a> {
    Value(&'a Longhand),
    Keyword(CssWideKeyword),
    Vars {
        property: PropertyId,
        value: &'a [ComponentValue],
    },
}

#[derive(Clone, Debug, Default)]
struct Slot<'a> {
    winner: Option<(Winner<'a>, Option<&'a wb_url::Url>)>,
    skip: Option<Skip>,
}

#[derive(Clone, Debug)]
enum CustomWinner<'a> {
    Tokens(&'a [ComponentValue]),
    Keyword(CssWideKeyword),
}

#[derive(Clone, Debug, Default)]
struct CustomSlot<'a> {
    winner: Option<CustomWinner<'a>>,
    skip: Option<Skip>,
}

/// What a value becomes after cascading, before computing.
#[derive(Debug)]
enum Resolved<'a> {
    Value(Cow<'a, Longhand>),
    Inherit,
    Initial,
}

/// The cascaded values of one element or pseudo-element.
#[derive(Debug)]
struct Cascaded<'a> {
    slots: Vec<Slot<'a>>,
    customs: HashMap<&'a str, CustomSlot<'a>>,
}

impl<'a> Cascaded<'a> {
    fn new() -> Self {
        Cascaded {
            slots: vec![Slot::default(); LonghandId::ALL.len()],
            customs: HashMap::new(),
        }
    }

    fn offer(&mut self, id: LonghandId, pos: Pos, w: Winner<'a>, base: Option<&'a wb_url::Url>) {
        let Some(slot) = self.slots.get_mut(id as usize) else {
            return;
        };
        if slot.winner.is_some() || slot.skip.is_some_and(|s| s.excludes(pos)) {
            return;
        }
        match w {
            Winner::Keyword(CssWideKeyword::Revert) => slot.skip = Some(Skip::Origin(pos.origin)),
            Winner::Keyword(CssWideKeyword::RevertLayer) => slot.skip = Some(Skip::Layer(pos)),
            w => slot.winner = Some((w, base)),
        }
    }

    fn offer_custom(&mut self, name: &'a str, pos: Pos, w: CustomWinner<'a>) {
        let slot = self.customs.entry(name).or_default();
        if slot.winner.is_some() || slot.skip.is_some_and(|s| s.excludes(pos)) {
            return;
        }
        match w {
            CustomWinner::Keyword(CssWideKeyword::Revert) => {
                slot.skip = Some(Skip::Origin(pos.origin));
            }
            CustomWinner::Keyword(CssWideKeyword::RevertLayer) => {
                slot.skip = Some(Skip::Layer(pos));
            }
            w => slot.winner = Some(w),
        }
    }

    /// Offers a block's declarations of the given importance, last first.
    fn offer_block(&mut self, decls: &'a [Declaration], pos: Pos, base: Option<&'a wb_url::Url>) {
        for d in decls.iter().rev() {
            if d.important != pos.important {
                continue;
            }
            match &d.kind {
                DeclarationKind::Longhand(l) => self.offer(l.id(), pos, Winner::Value(l), base),
                DeclarationKind::CssWide(id, k) => self.offer(*id, pos, Winner::Keyword(*k), base),
                DeclarationKind::WithVariables { property, value } => {
                    for id in property_longhands(*property) {
                        self.offer(
                            *id,
                            pos,
                            Winner::Vars {
                                property: *property,
                                value,
                            },
                            base,
                        );
                    }
                }
                DeclarationKind::Custom { name, value } => {
                    let w = match value {
                        CustomValue::Tokens(t) => CustomWinner::Tokens(t),
                        CustomValue::CssWide(k) => CustomWinner::Keyword(*k),
                    };
                    self.offer_custom(name, pos, w);
                }
                DeclarationKind::Unknown { .. } => {}
            }
        }
    }

    fn winner(&self, id: LonghandId) -> Option<&(Winner<'a>, Option<&'a wb_url::Url>)> {
        self.slots.get(id as usize).and_then(|s| s.winner.as_ref())
    }
}

fn property_longhands(p: PropertyId) -> &'static [LonghandId] {
    match p {
        PropertyId::Longhand(l) => LonghandId::ALL
            .iter()
            .position(|x| *x == l)
            .and_then(|i| LonghandId::ALL.get(i..=i))
            .unwrap_or(&[]),
        PropertyId::Shorthand(s) => s.longhands(),
    }
}

/// The sources of declarations for one element or pseudo-element.
struct Sources<'a> {
    /// Matched selector indices.
    rules: &'a [u32],
    style_attr: Option<&'a DeclarationBlock>,
    hints: &'a [Declaration],
}

/// Fixed per-document inputs.
struct Styler<'a> {
    stylist: &'a Stylist,
    doc: &'a Document,
    cx: MatchContext<'a>,
    base_url: Option<&'a wb_url::Url>,
    link_color: Option<Rgba>,
    initial: Arc<ComputedStyle>,
    root_font_size: f32,
    root_line_height: f32,
}

/// Matched selectors of one element, split by pseudo-element.
#[derive(Default)]
struct Matched {
    element: Vec<u32>,
    before: Vec<u32>,
    after: Vec<u32>,
    marker: Vec<u32>,
}

type ShareKey = (usize, u8, Vec<u32>);

pub(crate) fn style_document(
    stylist: &Stylist,
    doc: &Document,
    options: &StyleOptions,
) -> StyledDocument {
    let mut initial = ComputedStyle::initial();
    initial.font_size = sanitize(stylist.media.font_size.max(0.0));
    let initial = Arc::new(initial);
    let n = doc.node_count();
    let mut styler = Styler {
        stylist,
        doc,
        cx: MatchContext::new(doc, &options.state),
        base_url: options.base_url.as_ref(),
        link_color: hints::body_link_color(doc),
        root_font_size: initial.font_size,
        root_line_height: initial.line_height_px(),
        initial: initial.clone(),
    };
    let mut out = StyledDocument {
        styles: vec![None; n],
        pseudos: vec![None; n],
        parents: vec![None; n],
        root: doc.document_element(),
        initial,
    };
    let mut filter = AncestorFilter::new();
    let mut cache: HashMap<ShareKey, Arc<ComputedStyle>> = HashMap::new();

    enum Frame {
        Enter(NodeId),
        Exit(Vec<u32>),
    }
    // Iterative depth-first traversal: deep trees cannot overflow the stack.
    let mut stack: Vec<Frame> = doc.children(doc.root()).map(Frame::Enter).collect();
    stack.reverse();
    while let Some(frame) = stack.pop() {
        let node = match frame {
            Frame::Exit(hashes) => {
                for h in hashes {
                    filter.remove(h);
                }
                continue;
            }
            Frame::Enter(node) => node,
        };
        let parent = doc.parent(node);
        match doc.data(node) {
            Some(NodeData::Element(el)) => {
                let parent_style = parent
                    .and_then(|p| out.styles.get(p.index()))
                    .and_then(Option::as_ref)
                    .cloned();
                let is_root = parent_style.is_none();
                let parent_style = parent_style.unwrap_or_else(|| styler.initial.clone());
                let (style, pseudos) =
                    styler.style_element(node, el, &parent_style, is_root, &filter, &mut cache);
                if is_root {
                    styler.root_font_size = style.font_size;
                    styler.root_line_height = style.line_height_px();
                }
                if let Some(slot) = out.styles.get_mut(node.index()) {
                    *slot = Some(style);
                }
                if let Some(slot) = out.pseudos.get_mut(node.index()) {
                    *slot = pseudos;
                }
                let hashes = element_hashes(el);
                for &h in &hashes {
                    filter.insert(h);
                }
                stack.push(Frame::Exit(hashes));
                let first = stack.len();
                stack.extend(doc.children(node).map(Frame::Enter));
                if let Some(children) = stack.get_mut(first..) {
                    children.reverse();
                }
            }
            Some(NodeData::Text(_)) => {
                if let Some(slot) = out.parents.get_mut(node.index()) {
                    *slot = parent;
                }
            }
            _ => {}
        }
    }
    out
}

fn element_hashes(el: &ElementData) -> Vec<u32> {
    let mut v = vec![bloom::hash(Kind::Tag, &el.local_name)];
    if let Some(id) = el.id() {
        v.push(bloom::hash(Kind::Id, id));
    }
    for c in el.classes() {
        v.push(bloom::hash(Kind::Class, c));
    }
    v
}

fn content_has_attr(c: Option<&(Winner<'_>, Option<&wb_url::Url>)>) -> bool {
    match c {
        Some((Winner::Value(Longhand::Content(Content::Items(items))), _)) => {
            items.iter().any(|i| matches!(i, ContentItem::Attr(_)))
        }
        Some((Winner::Vars { .. }, _)) => true,
        _ => false,
    }
}

impl<'a> Styler<'a> {
    fn collect(&self, node: NodeId, el: &ElementData, filter: &AncestorFilter) -> Matched {
        let set = &self.stylist.rules;
        let mut m = Matched::default();
        let mut consider = |list: Option<&Vec<u32>>| {
            for &idx in list.into_iter().flatten() {
                let Some(rs) = set.selectors.get(idx as usize) else {
                    continue;
                };
                let Some(rule) = set.rules.get(rs.rule as usize) else {
                    continue;
                };
                if rule.quirks_only && !self.cx.quirks {
                    continue;
                }
                if rs
                    .ancestor_hashes
                    .iter()
                    .any(|&h| h != 0 && !filter.might_contain(h))
                {
                    continue;
                }
                if !self.cx.matches(&rs.selector, node) {
                    continue;
                }
                match rs.pseudo() {
                    None => m.element.push(idx),
                    Some(PseudoElement::Before) => m.before.push(idx),
                    Some(PseudoElement::After) => m.after.push(idx),
                    Some(PseudoElement::Marker) => m.marker.push(idx),
                    // M1 follow-up: ::first-line, ::first-letter, ...
                    Some(_) => {}
                }
            }
        };
        if let Some(id) = el.id() {
            consider(set.by_id.get(&id.to_ascii_lowercase()));
        }
        let mut seen: Vec<String> = Vec::new();
        for c in el.classes() {
            let lower = c.to_ascii_lowercase();
            if seen.contains(&lower) {
                continue;
            }
            consider(set.by_class.get(&lower));
            seen.push(lower);
        }
        consider(set.by_tag.get(&el.local_name.to_ascii_lowercase()));
        consider(Some(&set.universal));
        m
    }

    /// Sorts matched selectors in ascending cascade order and drops
    /// duplicate rules (a rule matched through several of its selectors
    /// keeps the most specific one).
    fn sort(&self, list: &mut Vec<u32>) {
        let set = &self.stylist.rules;
        let key = |idx: &u32| {
            set.selectors
                .get(*idx as usize)
                .map_or((Origin::UserAgent, 0, 0, 0), |s| {
                    let r = set.rules.get(s.rule as usize);
                    (
                        r.map_or(Origin::UserAgent, |r| r.origin),
                        r.map_or(0, |r| r.layer),
                        s.specificity,
                        s.order,
                    )
                })
        };
        list.sort_by_key(key);
        // Keep the last (highest-priority) selector of each rule.
        let mut seen = std::collections::HashSet::new();
        let mut kept: Vec<u32> = Vec::with_capacity(list.len());
        for &idx in list.iter().rev() {
            let rule = set.selectors.get(idx as usize).map(|s| s.rule);
            if seen.insert(rule) {
                kept.push(idx);
            }
        }
        kept.reverse();
        *list = kept;
    }

    fn cascade<'s>(&'s self, src: &Sources<'s>) -> Cascaded<'s> {
        let set = &self.stylist.rules;
        let mut c = Cascaded::new();
        let entries: Vec<(&'s crate::rules::RuleData, u32)> = src
            .rules
            .iter()
            .filter_map(|&idx| {
                let s = set.selectors.get(idx as usize)?;
                Some((set.rules.get(s.rule as usize)?, s.specificity))
            })
            .collect();
        // Important declarations: earlier layers win (§6.4.2), so order by
        // reversed layer, then specificity and order (kept from `sort`).
        let mut important: Vec<usize> = (0..entries.len()).collect();
        important.sort_by_key(|&i| {
            entries
                .get(i)
                .map(|(r, _)| (r.origin, std::cmp::Reverse(r.layer)))
        });
        let levels = [
            (Origin::UserAgent, true),
            (Origin::User, true),
            (Origin::Author, true),
            (Origin::Author, false),
            (Origin::User, false),
            (Origin::UserAgent, false),
        ];
        for (origin, important_level) in levels {
            let pos = |layer: u32| Pos {
                origin,
                important: important_level,
                attached: false,
                hint: false,
                layer,
            };
            if origin == Origin::Author
                && let Some(block) = src.style_attr
            {
                let p = Pos {
                    attached: true,
                    ..pos(u32::MAX)
                };
                c.offer_block(&block.declarations, p, self.base_url);
            }
            let order: Box<dyn Iterator<Item = usize>> = if important_level {
                Box::new(important.iter().rev().copied())
            } else {
                Box::new((0..entries.len()).rev())
            };
            for i in order {
                let Some((rule, _)) = entries.get(i) else {
                    continue;
                };
                if rule.origin != origin {
                    continue;
                }
                c.offer_block(
                    &rule.declarations.declarations,
                    pos(rule.layer),
                    rule.base_url.as_deref(),
                );
            }
            if origin == Origin::Author && !important_level {
                let p = Pos {
                    hint: true,
                    ..pos(0)
                };
                c.offer_block(src.hints, p, self.base_url);
            }
        }
        c
    }

    fn style_element(
        &self,
        node: NodeId,
        el: &ElementData,
        parent: &Arc<ComputedStyle>,
        is_root: bool,
        filter: &AncestorFilter,
        cache: &mut HashMap<ShareKey, Arc<ComputedStyle>>,
    ) -> (Arc<ComputedStyle>, Option<Box<PseudoStyles>>) {
        let mut m = self.collect(node, el, filter);
        self.sort(&mut m.element);
        let style_attr = el.attribute("style").map(parse_style_attribute);
        let hint_decls = hints::presentational_hints(self.doc, node, el, self.link_color);
        let shareable = !is_root && style_attr.is_none() && hint_decls.is_empty();
        let key: ShareKey = (Arc::as_ptr(parent) as usize, 0, m.element.clone());
        let style = match cache.get(&key).filter(|_| shareable) {
            Some(s) => s.clone(),
            None => {
                let src = Sources {
                    rules: &m.element,
                    style_attr: style_attr.as_ref(),
                    hints: &hint_decls,
                };
                let cascaded = self.cascade(&src);
                let attr = |name: &str| attr_value(el, name);
                let computed = Arc::new(self.compute(&cascaded, parent, is_root, None, &attr));
                if shareable && !content_has_attr(cascaded.winner(LonghandId::Content)) {
                    cache.insert(key, computed.clone());
                }
                computed
            }
        };

        let mut pseudos = PseudoStyles::default();
        let attr = |name: &str| attr_value(el, name);
        for (which, list) in [
            (PseudoElement::Before, &mut m.before),
            (PseudoElement::After, &mut m.after),
        ] {
            if list.is_empty() {
                continue;
            }
            self.sort(list);
            let src = Sources {
                rules: list,
                style_attr: None,
                hints: &[],
            };
            let cascaded = self.cascade(&src);
            if !might_generate(&cascaded, &style) {
                continue;
            }
            let ps = self.compute(&cascaded, &style, false, Some(which), &attr);
            if matches!(ps.content, Content::None | Content::Normal) || ps.is_display_none() {
                continue;
            }
            let ps = Some(Arc::new(ps));
            match which {
                PseudoElement::Before => pseudos.before = ps,
                _ => pseudos.after = ps,
            }
        }
        if matches!(
            style.display,
            Display::Pair {
                list_item: true,
                ..
            }
        ) {
            self.sort(&mut m.marker);
            let key: ShareKey = (Arc::as_ptr(&style) as usize, 1, m.marker.clone());
            pseudos.marker = Some(match cache.get(&key) {
                Some(s) => s.clone(),
                None => {
                    let src = Sources {
                        rules: &m.marker,
                        style_attr: None,
                        hints: &[],
                    };
                    let cascaded = self.cascade(&src);
                    let ms = Arc::new(self.compute(
                        &cascaded,
                        &style,
                        false,
                        Some(PseudoElement::Marker),
                        &attr,
                    ));
                    if !content_has_attr(cascaded.winner(LonghandId::Content)) {
                        cache.insert(key, ms.clone());
                    }
                    ms
                }
            });
        }
        let pseudos =
            (pseudos.before.is_some() || pseudos.after.is_some() || pseudos.marker.is_some())
                .then(|| Box::new(pseudos));
        (style, pseudos)
    }

    /// Computes a style from cascaded values (CSS Cascade 5 §4.4).
    fn compute(
        &self,
        c: &Cascaded<'_>,
        parent: &ComputedStyle,
        is_root: bool,
        pseudo: Option<PseudoElement>,
        attr: &dyn Fn(&str) -> String,
    ) -> ComputedStyle {
        let initial = &*self.initial;
        let customs = compute_customs(&parent.custom_properties, &c.customs);
        let mut s = inherit_base(parent, initial);
        for id in [
            LonghandId::BorderTopWidth,
            LonghandId::BorderRightWidth,
            LonghandId::BorderBottomWidth,
            LonghandId::BorderLeftWidth,
        ] {
            reset_border_width(id, &mut s);
        }
        s.custom_properties = customs.clone();

        // Resolve each longhand's cascaded value.
        let mut parsed_vars: HashMap<usize, Option<Result<Vec<Longhand>, CssWideKeyword>>> =
            HashMap::new();
        let mut resolved: Vec<(LonghandId, Resolved<'_>, Option<&wb_url::Url>)> = Vec::new();
        for &id in LonghandId::ALL {
            let Some((w, base)) = c.winner(id) else {
                continue;
            };
            let r = match w {
                Winner::Value(l) => Resolved::Value(Cow::Borrowed(*l)),
                Winner::Keyword(k) => keyword(*k, id),
                Winner::Vars { property, value } => {
                    let key = value.as_ptr() as usize;
                    let parsed = parsed_vars
                        .entry(key)
                        .or_insert_with(|| substitute_and_parse(*property, value, &customs));
                    match parsed {
                        Some(Ok(ls)) => match ls.iter().find(|l| l.id() == id) {
                            Some(l) => Resolved::Value(Cow::Owned(l.clone())),
                            None => keyword(CssWideKeyword::Unset, id),
                        },
                        Some(Err(k)) => keyword(*k, id),
                        None => keyword(CssWideKeyword::Unset, id),
                    }
                }
            };
            resolved.push((id, r, *base));
        }

        // font-size first: everything else may depend on it.
        let parent_lcx = LengthContext {
            font_size: parent.font_size,
            root_font_size: if is_root {
                initial.font_size
            } else {
                self.root_font_size
            },
            line_height: parent.line_height_px(),
            root_line_height: if is_root {
                initial.line_height_px()
            } else {
                self.root_line_height
            },
            viewport_width: self.stylist.media.viewport_width,
            viewport_height: self.stylist.media.viewport_height,
        };
        for (id, r, _) in &resolved {
            if *id != LonghandId::FontSize {
                continue;
            }
            s.font_size = match r {
                Resolved::Inherit => parent.font_size,
                Resolved::Initial => initial.font_size,
                Resolved::Value(l) => match l.as_ref() {
                    Longhand::FontSize(FontSize::Keyword(k)) => {
                        values::font_size_keyword(*k, initial.font_size, parent.font_size)
                    }
                    Longhand::FontSize(FontSize::LengthPercentage(lp)) => {
                        parent_lcx.lp_px(lp, parent.font_size)
                    }
                    _ => s.font_size,
                },
            };
            s.font_size = sanitize(s.font_size.max(0.0));
        }
        // color next: `currentcolor` in other properties refers to it.
        for (id, r, _) in &resolved {
            if *id != LonghandId::Color {
                continue;
            }
            s.color = match r {
                Resolved::Inherit => parent.color,
                Resolved::Initial => initial.color,
                Resolved::Value(l) => match l.as_ref() {
                    // `color: currentcolor` is `inherit` (CSS Color 4 §4.4).
                    Longhand::Color(col) => values::color(*col, parent.color),
                    _ => s.color,
                },
            };
        }
        let mut lcx = LengthContext {
            font_size: s.font_size,
            root_font_size: if is_root {
                s.font_size
            } else {
                self.root_font_size
            },
            line_height: parent.line_height_px(),
            ..parent_lcx
        };
        for (id, r, base) in &resolved {
            if *id != LonghandId::LineHeight {
                continue;
            }
            apply(&mut s, parent, &lcx, r, *base, attr, &mut BgLists::none());
        }
        lcx.line_height = s.line_height_px();
        if is_root {
            lcx.root_line_height = lcx.line_height;
        }

        // Colors that stay `currentcolor` (initial for borders and
        // text-decoration-color) are filled in at the end.
        let mut bg = BgLists::from_layers(&initial.background_layers);
        let mut bg_touched = false;
        let mut current = CurrentColor::default();
        for (id, r, base) in &resolved {
            use LonghandId as L;
            match id {
                L::FontSize | L::Color | L::LineHeight => continue,
                L::BackgroundImage
                | L::BackgroundRepeat
                | L::BackgroundPosition
                | L::BackgroundSize
                | L::BackgroundAttachment
                | L::BackgroundOrigin
                | L::BackgroundClip => bg_touched = true,
                L::BorderTopColor => current.top = is_current(r),
                L::BorderRightColor => current.right = is_current(r),
                L::BorderBottomColor => current.bottom = is_current(r),
                L::BorderLeftColor => current.left = is_current(r),
                L::TextDecorationColor => current.decoration = is_current(r),
                _ => {}
            }
            match r {
                Resolved::Inherit => {
                    copy_prop(*id, &mut s, parent);
                    bg.copy_from(*id, &BgLists::from_layers(&parent.background_layers));
                }
                Resolved::Initial => {
                    copy_prop(*id, &mut s, initial);
                    reset_border_width(*id, &mut s);
                    bg.copy_from(*id, &BgLists::from_layers(&initial.background_layers));
                }
                Resolved::Value(_) => apply(&mut s, parent, &lcx, r, *base, attr, &mut bg),
            }
        }
        if bg_touched {
            s.background_layers = bg.into_layers();
        }
        let color = s.color;
        for (flag, side) in [
            (current.top, &mut s.border.top),
            (current.right, &mut s.border.right),
            (current.bottom, &mut s.border.bottom),
            (current.left, &mut s.border.left),
        ] {
            if flag {
                side.color = color;
            }
        }
        if current.decoration {
            s.text_decoration_color = color;
        }

        fixups(&mut s, parent, is_root, pseudo);
        s
    }
}

#[derive(Debug)]
struct CurrentColor {
    top: bool,
    right: bool,
    bottom: bool,
    left: bool,
    decoration: bool,
}

impl Default for CurrentColor {
    fn default() -> Self {
        CurrentColor {
            top: true,
            right: true,
            bottom: true,
            left: true,
            decoration: true,
        }
    }
}

/// Whether a resolved color value is (or stays) `currentcolor`.
fn is_current(r: &Resolved<'_>) -> bool {
    match r {
        Resolved::Initial => true,
        Resolved::Inherit => false,
        Resolved::Value(l) => matches!(
            l.as_ref(),
            Longhand::BorderTopColor(wb_css::Color::CurrentColor)
                | Longhand::BorderRightColor(wb_css::Color::CurrentColor)
                | Longhand::BorderBottomColor(wb_css::Color::CurrentColor)
                | Longhand::BorderLeftColor(wb_css::Color::CurrentColor)
                | Longhand::TextDecorationColor(wb_css::Color::CurrentColor)
        ),
    }
}

/// `initial`, `inherit` and `unset` (CSS Cascade 5 §7.3). `revert` and
/// `revert-layer` only reach here when nothing is left to revert to, and
/// then behave like `unset`.
fn keyword(k: CssWideKeyword, id: LonghandId) -> Resolved<'static> {
    match k {
        CssWideKeyword::Initial => Resolved::Initial,
        CssWideKeyword::Inherit => Resolved::Inherit,
        _ => {
            if id.is_inherited() {
                Resolved::Inherit
            } else {
                Resolved::Initial
            }
        }
    }
}

/// Substitutes `var()` and re-parses (CSS Variables 1 §3.1). `None` means
/// invalid at computed-value time; `Some(Err(k))` means the value became a
/// CSS-wide keyword.
fn substitute_and_parse(
    property: PropertyId,
    value: &[ComponentValue],
    customs: &HashMap<String, Vec<ComponentValue>>,
) -> Option<Result<Vec<Longhand>, CssWideKeyword>> {
    let mut lookup = |name: &str| customs.get(name).cloned();
    let substituted = substitute_variables(value, &mut lookup)?;
    let meaningful: Vec<&ComponentValue> = substituted
        .iter()
        .filter(|v| !matches!(v, ComponentValue::Whitespace))
        .collect();
    if let [ComponentValue::Ident(i)] = meaningful.as_slice()
        && let Some(k) = CssWideKeyword::from_ident(i)
    {
        return Some(Err(k));
    }
    parse_property_value(property, &substituted).map(Ok)
}

/// Computes the custom properties (CSS Variables 1 §2, §3): inherited
/// values, overridden by this element's declarations with `var()` resolved
/// in dependency order. Properties in a reference cycle become
/// guaranteed-invalid (absent). Iterative, so long chains cannot overflow
/// the stack.
fn compute_customs(
    parent: &Arc<HashMap<String, Vec<ComponentValue>>>,
    slots: &HashMap<&str, CustomSlot<'_>>,
) -> Arc<HashMap<String, Vec<ComponentValue>>> {
    if slots.values().all(|s| s.winner.is_none()) {
        return parent.clone();
    }
    let mut map = (**parent).clone();
    let mut names: Vec<&str> = Vec::new();
    let mut raw: Vec<&[ComponentValue]> = Vec::new();
    for (name, slot) in slots {
        match &slot.winner {
            Some(CustomWinner::Tokens(t)) => {
                map.remove(*name);
                names.push(name);
                raw.push(t);
            }
            Some(CustomWinner::Keyword(CssWideKeyword::Initial)) => {
                map.remove(*name);
            }
            // inherit / unset (custom properties inherit): keep the parent's.
            _ => {}
        }
    }
    let index: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let deps: Vec<Vec<usize>> = raw
        .iter()
        .map(|v| {
            let mut refs = Vec::new();
            collect_var_refs(v, &mut refs, 0);
            refs.iter()
                .filter_map(|r| index.get(r.as_str()).copied())
                .collect()
        })
        .collect();
    let n = names.len();
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    let mut stack_pos = vec![usize::MAX; n];
    let mut cyclic = vec![false; n];
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for start in 0..n {
        if state.get(start) != Some(&0) {
            continue;
        }
        if let Some(st) = state.get_mut(start) {
            *st = 1;
        }
        if let Some(p) = stack_pos.get_mut(start) {
            *p = 0;
        }
        stack.push((start, 0));
        while let Some(&mut (v, ref mut k)) = stack.last_mut() {
            let next = deps.get(v).and_then(|d| d.get(*k)).copied();
            if let Some(w) = next {
                *k += 1;
                match state.get(w) {
                    Some(0) => {
                        if let Some(st) = state.get_mut(w) {
                            *st = 1;
                        }
                        if let Some(p) = stack_pos.get_mut(w) {
                            *p = stack.len();
                        }
                        stack.push((w, 0));
                    }
                    Some(1) => {
                        let from = stack_pos.get(w).copied().unwrap_or(0);
                        for &(member, _) in stack.get(from..).unwrap_or(&[]) {
                            if let Some(c) = cyclic.get_mut(member) {
                                *c = true;
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
            stack.pop();
            if let Some(st) = state.get_mut(v) {
                *st = 2;
            }
            let (Some(name), Some(value)) = (names.get(v), raw.get(v)) else {
                continue;
            };
            let result = if cyclic.get(v) == Some(&true) {
                None
            } else {
                let mut lookup = |r: &str| map.get(r).cloned();
                substitute_variables(value, &mut lookup)
            };
            match result {
                Some(r) => {
                    map.insert((*name).to_string(), r);
                }
                None => {
                    map.remove(*name);
                }
            }
        }
    }
    Arc::new(map)
}

/// The custom property names referenced by `var()`s in `values`.
fn collect_var_refs(values: &[ComponentValue], out: &mut Vec<String>, depth: u32) {
    if depth > 128 {
        return;
    }
    for v in values {
        match v {
            ComponentValue::Function { name, arguments } => {
                if name.eq_ignore_ascii_case("var")
                    && let Some(ComponentValue::Ident(i)) = arguments
                        .iter()
                        .find(|a| !matches!(a, ComponentValue::Whitespace))
                    && i.starts_with("--")
                {
                    out.push(i.clone());
                }
                collect_var_refs(arguments, out, depth + 1);
            }
            ComponentValue::Block { contents, .. } => collect_var_refs(contents, out, depth + 1),
            _ => {}
        }
    }
}

/// Whether `::before`/`::after` cascaded values can produce a box: only
/// when `content` is something other than `normal`/`none` (CSS Pseudo 4
/// §2.2). Inherited or `var()` values are computed to find out.
fn might_generate(c: &Cascaded<'_>, element: &ComputedStyle) -> bool {
    match c.winner(LonghandId::Content) {
        None => false,
        Some((Winner::Value(Longhand::Content(Content::Normal | Content::None)), _)) => false,
        Some((Winner::Keyword(CssWideKeyword::Inherit), _)) => {
            matches!(element.content, Content::Items(_))
        }
        Some((Winner::Keyword(_), _)) => false,
        Some(_) => true,
    }
}

fn attr_value(el: &ElementData, name: &str) -> String {
    let lookup = if el.namespace == wb_dom::Namespace::Html {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    };
    el.attribute(&lookup).unwrap_or("").to_string()
}

/// A new style with the inherited properties taken from `parent` and the
/// rest initial (CSS Cascade 5 §7.2).
fn inherit_base(parent: &ComputedStyle, initial: &ComputedStyle) -> ComputedStyle {
    let mut s = initial.clone();
    for &id in LonghandId::ALL {
        if id.is_inherited() {
            copy_prop(id, &mut s, parent);
        }
    }
    s.custom_properties = parent.custom_properties.clone();
    s
}

/// Copies one property's computed value from `src` to `dst`.
fn copy_prop(id: LonghandId, dst: &mut ComputedStyle, src: &ComputedStyle) {
    use LonghandId as L;
    macro_rules! c {
        ($($f:ident).+) => {
            dst.$($f).+ = src.$($f).+.clone()
        };
    }
    match id {
        L::MarginTop => c!(margin.top),
        L::MarginRight => c!(margin.right),
        L::MarginBottom => c!(margin.bottom),
        L::MarginLeft => c!(margin.left),
        L::PaddingTop => c!(padding.top),
        L::PaddingRight => c!(padding.right),
        L::PaddingBottom => c!(padding.bottom),
        L::PaddingLeft => c!(padding.left),
        L::BorderTopWidth => c!(border.top.width),
        L::BorderRightWidth => c!(border.right.width),
        L::BorderBottomWidth => c!(border.bottom.width),
        L::BorderLeftWidth => c!(border.left.width),
        L::BorderTopStyle => c!(border.top.style),
        L::BorderRightStyle => c!(border.right.style),
        L::BorderBottomStyle => c!(border.bottom.style),
        L::BorderLeftStyle => c!(border.left.style),
        L::BorderTopColor => c!(border.top.color),
        L::BorderRightColor => c!(border.right.color),
        L::BorderBottomColor => c!(border.bottom.color),
        L::BorderLeftColor => c!(border.left.color),
        L::BorderTopLeftRadius => c!(border_radius.top_left),
        L::BorderTopRightRadius => c!(border_radius.top_right),
        L::BorderBottomRightRadius => c!(border_radius.bottom_right),
        L::BorderBottomLeftRadius => c!(border_radius.bottom_left),
        L::Width => c!(width),
        L::Height => c!(height),
        L::MinWidth => c!(min_width),
        L::MinHeight => c!(min_height),
        L::MaxWidth => c!(max_width),
        L::MaxHeight => c!(max_height),
        L::BoxSizing => c!(box_sizing),
        L::Display => c!(display),
        L::Position => c!(position),
        L::Top => c!(inset.top),
        L::Right => c!(inset.right),
        L::Bottom => c!(inset.bottom),
        L::Left => c!(inset.left),
        L::Float => c!(float),
        L::Clear => c!(clear),
        L::ZIndex => c!(z_index),
        L::OverflowX => c!(overflow_x),
        L::OverflowY => c!(overflow_y),
        L::Visibility => c!(visibility),
        L::Opacity => c!(opacity),
        L::Color => c!(color),
        L::BackgroundColor => c!(background_color),
        // Background lists are copied through `BgLists`.
        L::BackgroundImage
        | L::BackgroundRepeat
        | L::BackgroundPosition
        | L::BackgroundSize
        | L::BackgroundAttachment
        | L::BackgroundOrigin
        | L::BackgroundClip => {}
        L::FontFamily => c!(font_family),
        L::FontSize => c!(font_size),
        L::FontWeight => c!(font_weight),
        L::FontStyle => c!(font_style),
        L::FontStretch => c!(font_stretch),
        L::LineHeight => c!(line_height),
        L::TextAlign => c!(text_align),
        L::TextDecorationLine => c!(text_decoration_line),
        L::TextDecorationStyle => c!(text_decoration_style),
        L::TextDecorationColor => c!(text_decoration_color),
        L::TextTransform => c!(text_transform),
        L::TextIndent => c!(text_indent),
        L::WhiteSpace => c!(white_space),
        L::WordBreak => c!(word_break),
        L::OverflowWrap => c!(overflow_wrap),
        L::LetterSpacing => c!(letter_spacing),
        L::WordSpacing => c!(word_spacing),
        L::VerticalAlign => c!(vertical_align),
        L::ListStyleType => c!(list_style_type),
        L::ListStylePosition => c!(list_style_position),
        L::ListStyleImage => c!(list_style_image),
        L::BorderCollapse => c!(border_collapse),
        L::BorderSpacing => c!(border_spacing),
        L::TableLayout => c!(table_layout),
        L::CaptionSide => c!(caption_side),
        L::EmptyCells => c!(empty_cells),
        L::FlexDirection => c!(flex_direction),
        L::FlexWrap => c!(flex_wrap),
        L::FlexGrow => c!(flex_grow),
        L::FlexShrink => c!(flex_shrink),
        L::FlexBasis => c!(flex_basis),
        L::JustifyContent => c!(justify_content),
        L::AlignItems => c!(align_items),
        L::AlignSelf => c!(align_self),
        L::AlignContent => c!(align_content),
        L::Order => c!(order),
        L::RowGap => c!(row_gap),
        L::ColumnGap => c!(column_gap),
        L::Cursor => c!(cursor),
        L::Content => c!(content),
        L::CounterReset => c!(counter_reset),
        L::CounterIncrement => c!(counter_increment),
    }
}

/// The background longhands as separate lists, before they are combined
/// into layers.
#[derive(Clone, Debug)]
struct BgLists {
    images: Vec<wb_css::Image>,
    repeat: Vec<(wb_css::RepeatStyle, wb_css::RepeatStyle)>,
    position: Vec<(PositionComponent, PositionComponent)>,
    size: Vec<BackgroundSize>,
    attachment: Vec<wb_css::BackgroundAttachment>,
    origin: Vec<wb_css::BackgroundBox>,
    clip: Vec<wb_css::BackgroundBox>,
}

impl BgLists {
    fn none() -> Self {
        BgLists {
            images: Vec::new(),
            repeat: Vec::new(),
            position: Vec::new(),
            size: Vec::new(),
            attachment: Vec::new(),
            origin: Vec::new(),
            clip: Vec::new(),
        }
    }

    fn from_layers(layers: &[BackgroundLayer]) -> Self {
        BgLists {
            images: layers.iter().map(|l| l.image.clone()).collect(),
            repeat: layers.iter().map(|l| (l.repeat_x, l.repeat_y)).collect(),
            position: layers
                .iter()
                .map(|l| (l.position_x.clone(), l.position_y.clone()))
                .collect(),
            size: layers.iter().map(|l| l.size.clone()).collect(),
            attachment: layers.iter().map(|l| l.attachment).collect(),
            origin: layers.iter().map(|l| l.origin).collect(),
            clip: layers.iter().map(|l| l.clip).collect(),
        }
    }

    fn copy_from(&mut self, id: LonghandId, src: &BgLists) {
        use LonghandId as L;
        match id {
            L::BackgroundImage => self.images = src.images.clone(),
            L::BackgroundRepeat => self.repeat = src.repeat.clone(),
            L::BackgroundPosition => self.position = src.position.clone(),
            L::BackgroundSize => self.size = src.size.clone(),
            L::BackgroundAttachment => self.attachment = src.attachment.clone(),
            L::BackgroundOrigin => self.origin = src.origin.clone(),
            L::BackgroundClip => self.clip = src.clip.clone(),
            _ => {}
        }
    }

    /// One layer per image, other lists repeated to match (CSS Backgrounds
    /// 3 §2.2).
    fn into_layers(self) -> Arc<[BackgroundLayer]> {
        let init = BackgroundLayer::initial();
        let n = self.images.len().max(1);
        let layers: Vec<BackgroundLayer> = (0..n)
            .map(|i| {
                let image = self.images.get(i).cloned().unwrap_or(wb_css::Image::None);
                let (repeat_x, repeat_y) =
                    cycle(&self.repeat, i).unwrap_or((init.repeat_x, init.repeat_y));
                let (position_x, position_y) = cycle(&self.position, i)
                    .unwrap_or((init.position_x.clone(), init.position_y.clone()));
                BackgroundLayer {
                    image,
                    repeat_x,
                    repeat_y,
                    position_x,
                    position_y,
                    size: cycle(&self.size, i).unwrap_or(init.size.clone()),
                    attachment: cycle(&self.attachment, i).unwrap_or(init.attachment),
                    origin: cycle(&self.origin, i).unwrap_or(init.origin),
                    clip: cycle(&self.clip, i).unwrap_or(init.clip),
                }
            })
            .collect();
        Arc::from(layers)
    }
}

/// The initial border width is `medium` (3px, CSS Backgrounds 3 §3.3); it
/// only computes to 0 when the style is `none`, which `fixups` handles.
fn reset_border_width(id: LonghandId, s: &mut ComputedStyle) {
    const MEDIUM: f32 = 3.0;
    match id {
        LonghandId::BorderTopWidth => s.border.top.width = MEDIUM,
        LonghandId::BorderRightWidth => s.border.right.width = MEDIUM,
        LonghandId::BorderBottomWidth => s.border.bottom.width = MEDIUM,
        LonghandId::BorderLeftWidth => s.border.left.width = MEDIUM,
        _ => {}
    }
}

/// Element `i` of `v` repeated cyclically.
fn cycle<T: Clone>(v: &[T], i: usize) -> Option<T> {
    if v.is_empty() {
        None
    } else {
        v.get(i % v.len()).cloned()
    }
}

/// Computes one specified value into `s`.
#[allow(clippy::too_many_arguments)]
fn apply(
    s: &mut ComputedStyle,
    parent: &ComputedStyle,
    lcx: &LengthContext,
    r: &Resolved<'_>,
    base: Option<&wb_url::Url>,
    attr: &dyn Fn(&str) -> String,
    bg: &mut BgLists,
) {
    let l = match r {
        Resolved::Value(l) => l.as_ref(),
        Resolved::Inherit | Resolved::Initial => return,
    };
    let border_width = |v: &wb_css::LengthPercentage| lcx.lp_px(v, 0.0).max(0.0);
    let radius = |r: &wb_css::CornerRadius| CornerRadius {
        horizontal: lcx.lp_non_negative(&r.horizontal),
        vertical: lcx.lp_non_negative(&r.vertical),
    };
    let current = s.color;
    use Longhand as H;
    match l {
        H::MarginTop(v) => s.margin.top = lcx.lp_or_auto(v),
        H::MarginRight(v) => s.margin.right = lcx.lp_or_auto(v),
        H::MarginBottom(v) => s.margin.bottom = lcx.lp_or_auto(v),
        H::MarginLeft(v) => s.margin.left = lcx.lp_or_auto(v),
        H::PaddingTop(v) => s.padding.top = lcx.lp_non_negative(v),
        H::PaddingRight(v) => s.padding.right = lcx.lp_non_negative(v),
        H::PaddingBottom(v) => s.padding.bottom = lcx.lp_non_negative(v),
        H::PaddingLeft(v) => s.padding.left = lcx.lp_non_negative(v),
        H::BorderTopWidth(v) => s.border.top.width = border_width(v),
        H::BorderRightWidth(v) => s.border.right.width = border_width(v),
        H::BorderBottomWidth(v) => s.border.bottom.width = border_width(v),
        H::BorderLeftWidth(v) => s.border.left.width = border_width(v),
        H::BorderTopStyle(v) => s.border.top.style = *v,
        H::BorderRightStyle(v) => s.border.right.style = *v,
        H::BorderBottomStyle(v) => s.border.bottom.style = *v,
        H::BorderLeftStyle(v) => s.border.left.style = *v,
        H::BorderTopColor(v) => s.border.top.color = values::color(*v, current),
        H::BorderRightColor(v) => s.border.right.color = values::color(*v, current),
        H::BorderBottomColor(v) => s.border.bottom.color = values::color(*v, current),
        H::BorderLeftColor(v) => s.border.left.color = values::color(*v, current),
        H::BorderTopLeftRadius(v) => s.border_radius.top_left = radius(v),
        H::BorderTopRightRadius(v) => s.border_radius.top_right = radius(v),
        H::BorderBottomRightRadius(v) => s.border_radius.bottom_right = radius(v),
        H::BorderBottomLeftRadius(v) => s.border_radius.bottom_left = radius(v),
        H::Width(v) => s.width = lcx.size(v),
        H::Height(v) => s.height = lcx.size(v),
        H::MinWidth(v) => s.min_width = lcx.size(v),
        H::MinHeight(v) => s.min_height = lcx.size(v),
        H::MaxWidth(v) => s.max_width = lcx.max_size(v),
        H::MaxHeight(v) => s.max_height = lcx.max_size(v),
        H::BoxSizing(v) => s.box_sizing = *v,
        H::Display(v) => s.display = *v,
        H::Position(v) => s.position = *v,
        H::Top(v) => s.inset.top = lcx.lp_or_auto(v),
        H::Right(v) => s.inset.right = lcx.lp_or_auto(v),
        H::Bottom(v) => s.inset.bottom = lcx.lp_or_auto(v),
        H::Left(v) => s.inset.left = lcx.lp_or_auto(v),
        H::Float(v) => s.float = *v,
        H::Clear(v) => s.clear = *v,
        H::ZIndex(v) => s.z_index = *v,
        H::OverflowX(v) => s.overflow_x = *v,
        H::OverflowY(v) => s.overflow_y = *v,
        H::Visibility(v) => s.visibility = *v,
        H::Opacity(v) => s.opacity = if v.is_nan() { 1.0 } else { v.clamp(0.0, 1.0) },
        H::Color(v) => s.color = values::color(*v, parent.color),
        H::BackgroundColor(v) => s.background_color = values::color(*v, current),
        H::BackgroundImage(v) => bg.images = v.iter().map(|i| values::image(i, base)).collect(),
        H::BackgroundRepeat(v) => bg.repeat = v.iter().map(|r| (r.x, r.y)).collect(),
        H::BackgroundPosition(v) => {
            bg.position = v
                .iter()
                .map(|p| (lcx.position_component(&p.x), lcx.position_component(&p.y)))
                .collect();
        }
        H::BackgroundSize(v) => bg.size = v.iter().map(|b| lcx.background_size(b)).collect(),
        H::BackgroundAttachment(v) => bg.attachment = v.clone(),
        H::BackgroundOrigin(v) => bg.origin = v.clone(),
        H::BackgroundClip(v) => bg.clip = v.clone(),
        H::FontFamily(v) => {
            if !v.is_empty() {
                s.font_family = Arc::from(v.clone());
            }
        }
        H::FontSize(_) => {}
        H::FontWeight(v) => {
            s.font_weight = match v {
                FontWeight::Absolute(w) => w.clamp(1.0, 1000.0),
                FontWeight::Bolder => values::bolder(parent.font_weight),
                FontWeight::Lighter => values::lighter(parent.font_weight),
            };
        }
        H::FontStyle(v) => s.font_style = *v,
        H::FontStretch(v) => s.font_stretch = sanitize(v.max(0.0)),
        H::LineHeight(v) => {
            s.line_height = match v {
                wb_css::LineHeight::Normal => LineHeight::Normal,
                wb_css::LineHeight::Number(n) => LineHeight::Number(sanitize(n.max(0.0))),
                wb_css::LineHeight::LengthPercentage(lp) => {
                    LineHeight::Length(lcx.lp_px(lp, s.font_size).max(0.0))
                }
            };
        }
        H::TextAlign(v) => {
            s.text_align = match v {
                TextAlign::MatchParent => parent.text_align,
                other => *other,
            };
        }
        H::TextDecorationLine(v) => s.text_decoration_line = *v,
        H::TextDecorationStyle(v) => s.text_decoration_style = *v,
        H::TextDecorationColor(v) => s.text_decoration_color = values::color(*v, current),
        H::TextTransform(v) => s.text_transform = *v,
        H::TextIndent(v) => s.text_indent = lcx.lp(v),
        H::WhiteSpace(v) => s.white_space = *v,
        H::WordBreak(v) => s.word_break = *v,
        H::OverflowWrap(v) => s.overflow_wrap = *v,
        H::LetterSpacing(v) => s.letter_spacing = spacing(v, lcx, s.font_size),
        H::WordSpacing(v) => s.word_spacing = spacing(v, lcx, s.font_size),
        H::VerticalAlign(v) => s.vertical_align = lcx.vertical_align(v),
        H::ListStyleType(v) => s.list_style_type = v.clone(),
        H::ListStylePosition(v) => s.list_style_position = *v,
        H::ListStyleImage(v) => s.list_style_image = values::image(v, base),
        H::BorderCollapse(v) => s.border_collapse = *v,
        H::BorderSpacing(v) => {
            s.border_spacing = (
                lcx.lp_px(&v.horizontal, 0.0).max(0.0),
                lcx.lp_px(&v.vertical, 0.0).max(0.0),
            );
        }
        H::TableLayout(v) => s.table_layout = *v,
        H::CaptionSide(v) => s.caption_side = *v,
        H::EmptyCells(v) => s.empty_cells = *v,
        H::FlexDirection(v) => s.flex_direction = *v,
        H::FlexWrap(v) => s.flex_wrap = *v,
        H::FlexGrow(v) => s.flex_grow = sanitize(v.max(0.0)),
        H::FlexShrink(v) => s.flex_shrink = sanitize(v.max(0.0)),
        H::FlexBasis(v) => s.flex_basis = lcx.flex_basis(v),
        H::JustifyContent(v) => s.justify_content = *v,
        H::AlignItems(v) => s.align_items = *v,
        H::AlignSelf(v) => s.align_self = *v,
        H::AlignContent(v) => s.align_content = *v,
        H::Order(v) => s.order = *v,
        H::RowGap(v) => s.row_gap = lcx.gap(v),
        H::ColumnGap(v) => s.column_gap = lcx.gap(v),
        H::Cursor(v) => s.cursor = values::cursor(v, base),
        H::Content(v) => s.content = values::content(v, base, attr),
        H::CounterReset(v) => s.counter_reset = Arc::from(v.clone()),
        H::CounterIncrement(v) => s.counter_increment = Arc::from(v.clone()),
    }
}

fn spacing(v: &wb_css::Spacing, lcx: &LengthContext, font_size: f32) -> f32 {
    match v {
        wb_css::Spacing::Normal => 0.0,
        wb_css::Spacing::LengthPercentage(lp) => lcx.lp_px(lp, font_size),
    }
}

/// Blockification (CSS Display 3 §2.7).
fn blockify(d: Display) -> Display {
    match d {
        Display::Pair {
            outside: DisplayOutside::Inline | DisplayOutside::RunIn,
            inside,
            list_item,
        } => Display::Pair {
            outside: DisplayOutside::Block,
            inside,
            list_item,
        },
        Display::Internal(_) => Display::BLOCK,
        other => other,
    }
}

/// Fix-ups that depend on several properties or on the parent.
fn fixups(
    s: &mut ComputedStyle,
    parent: &ComputedStyle,
    is_root: bool,
    pseudo: Option<PseudoElement>,
) {
    use wb_css::{Clear, LineStyle, Overflow, Position};
    // Logical float/clear values, for a left-to-right inline direction.
    s.float = match s.float {
        Float::InlineStart => Float::Left,
        Float::InlineEnd => Float::Right,
        f => f,
    };
    s.clear = match s.clear {
        Clear::InlineStart => Clear::Left,
        Clear::InlineEnd => Clear::Right,
        c => c,
    };
    // CSS 2.1 §9.7.
    let absolute = matches!(s.position, Position::Absolute | Position::Fixed);
    if absolute {
        s.float = Float::None;
    }
    let parent_is_flex_or_grid = pseudo != Some(PseudoElement::Marker)
        && !is_root
        && matches!(
            parent.display,
            Display::Pair {
                inside: DisplayInside::Flex | DisplayInside::Grid,
                ..
            }
        );
    if is_root && matches!(s.display, Display::Contents) {
        s.display = Display::BLOCK;
    }
    if is_root || absolute || !matches!(s.float, Float::None) || parent_is_flex_or_grid {
        s.display = blockify(s.display);
    }
    // CSS Backgrounds 3 §3.3: no style means no width.
    for side in [
        &mut s.border.top,
        &mut s.border.right,
        &mut s.border.bottom,
        &mut s.border.left,
    ] {
        if matches!(side.style, LineStyle::None | LineStyle::Hidden) {
            side.width = 0.0;
        }
    }
    // CSS Overflow 3 §3.1.
    let scrolls = |o: Overflow| !matches!(o, Overflow::Visible | Overflow::Clip);
    if scrolls(s.overflow_x) || scrolls(s.overflow_y) {
        let fix = |o: Overflow| match o {
            Overflow::Visible => Overflow::Auto,
            Overflow::Clip => Overflow::Hidden,
            o => o,
        };
        s.overflow_x = fix(s.overflow_x);
        s.overflow_y = fix(s.overflow_y);
    }
    // CSS Pseudo 4 §2.2: `normal` computes to `none` on ::before/::after.
    if matches!(pseudo, Some(PseudoElement::Before | PseudoElement::After))
        && matches!(s.content, Content::Normal)
    {
        s.content = Content::None;
    }
    // CSS Text Decoration 3 §2.1: decorations propagate to in-flow
    // descendants, but not into out-of-flow boxes or atomic inlines.
    let atomic_inline = matches!(
        s.display,
        Display::Pair {
            outside: DisplayOutside::Inline,
            inside: DisplayInside::FlowRoot
                | DisplayInside::Table
                | DisplayInside::Flex
                | DisplayInside::Grid,
            ..
        }
    );
    let blocked = absolute || !matches!(s.float, Float::None) || atomic_inline;
    let line = s.text_decoration_line;
    let own = line.underline || line.overline || line.line_through;
    if blocked || own {
        let mut list: Vec<TextDecoration> = if blocked {
            Vec::new()
        } else {
            parent.text_decorations.to_vec()
        };
        if own {
            list.push(TextDecoration {
                line,
                style: s.text_decoration_style,
                color: s.text_decoration_color,
            });
        }
        s.text_decorations = Arc::from(list);
    } else {
        s.text_decorations = parent.text_decorations.clone();
    }
}
