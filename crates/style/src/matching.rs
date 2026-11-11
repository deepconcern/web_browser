//! Selector matching ([Selectors 4](https://drafts.csswg.org/selectors-4/)).
//!
//! Complex selectors are matched right to left. Each step reports not just
//! failure but how far the failure reaches, so that a failed descendant
//! search does not get retried from every ancestor (which would make some
//! selectors quadratic or worse in the tree depth).

use std::collections::HashMap;

use wb_css::{
    AttributeCase, AttributeOperator, AttributeSelector, Combinator, Compound, PseudoClass,
    Selector, SelectorList, SimpleSelector,
};
use wb_dom::{Document, ElementData, Namespace, NodeData, NodeId};

use crate::ElementState;

/// Nested selector lists (`:is(:not(:is(...)))`) deeper than this never
/// match. `wb-css` already bounds nesting at 64 blocks.
const MAX_NESTING: u32 = 96;

/// Position data for one element, computed once per styling pass so that
/// structural pseudo-classes are O(1).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NodeInfo {
    /// 1-based position among element siblings.
    index: u32,
    /// Number of element siblings, including itself.
    count: u32,
    /// 1-based position among element siblings of the same type.
    type_index: u32,
    /// Number of element siblings of the same type.
    type_count: u32,
    flags: u8,
}

const HOVER: u8 = 1;
const ACTIVE: u8 = 2;
const FOCUS: u8 = 4;
const FOCUS_WITHIN: u8 = 8;

/// Per-document matching state.
#[derive(Debug)]
pub(crate) struct MatchContext<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) quirks: bool,
    info: Vec<NodeInfo>,
    focus_visible: bool,
}

impl<'a> MatchContext<'a> {
    pub(crate) fn new(doc: &'a Document, state: &ElementState) -> Self {
        let quirks = doc.quirks_mode() == wb_dom::QuirksMode::Quirks;
        let mut info = vec![NodeInfo::default(); doc.node_count()];
        // One pass over every parent's children: O(n) overall.
        let mut parents = vec![doc.root()];
        let mut type_counts: HashMap<(Namespace, &str), u32> = HashMap::new();
        while let Some(parent) = parents.pop() {
            let mut count = 0u32;
            type_counts.clear();
            for child in doc.children(parent) {
                parents.push(child);
                if let Some(el) = doc.element(child) {
                    count = count.saturating_add(1);
                    let t = type_counts
                        .entry((el.namespace, el.local_name.as_str()))
                        .or_insert(0);
                    *t = t.saturating_add(1);
                    if let Some(i) = info.get_mut(child.index()) {
                        i.index = count;
                        i.type_index = *t;
                    }
                }
            }
            for child in doc.children(parent) {
                if let Some(el) = doc.element(child) {
                    let tc = type_counts
                        .get(&(el.namespace, el.local_name.as_str()))
                        .copied()
                        .unwrap_or(1);
                    if let Some(i) = info.get_mut(child.index()) {
                        i.count = count;
                        i.type_count = tc;
                    }
                }
            }
        }
        let mut cx = MatchContext {
            doc,
            quirks,
            info,
            focus_visible: state.focus_visible,
        };
        cx.flag_chain(state.hover, HOVER, HOVER);
        cx.flag_chain(state.active, ACTIVE, ACTIVE);
        cx.flag_chain(state.focus, FOCUS | FOCUS_WITHIN, FOCUS_WITHIN);
        cx
    }

    /// Sets `own` on `node` and `ancestors` on its ancestors.
    fn flag_chain(&mut self, node: Option<NodeId>, own: u8, ancestors: u8) {
        let Some(node) = node else {
            return;
        };
        if !self.doc.is_element(node) {
            return;
        }
        if let Some(i) = self.info.get_mut(node.index()) {
            i.flags |= own;
        }
        for a in self.doc.ancestors(node) {
            if let Some(i) = self.info.get_mut(a.index()) {
                i.flags |= ancestors;
            }
        }
    }

    fn info(&self, node: NodeId) -> NodeInfo {
        self.info.get(node.index()).copied().unwrap_or_default()
    }

    fn parent_element(&self, node: NodeId) -> Option<NodeId> {
        self.doc.parent(node).filter(|&p| self.doc.is_element(p))
    }

    fn prev_element_sibling(&self, node: NodeId) -> Option<NodeId> {
        let mut cur = self.doc.previous_sibling(node);
        while let Some(n) = cur {
            if self.doc.is_element(n) {
                return Some(n);
            }
            cur = self.doc.previous_sibling(n);
        }
        None
    }

    /// Whether `selector` matches `element` (ignoring its pseudo-element).
    pub(crate) fn matches(&self, selector: &Selector, element: NodeId) -> bool {
        matches!(
            self.match_from(&selector.compounds, 0, element, 0),
            Outcome::Matched
        )
    }

    fn matches_list(&self, list: &SelectorList, element: NodeId, depth: u32) -> bool {
        if depth > MAX_NESTING {
            return false;
        }
        list.0.iter().any(|s| {
            // A pseudo-element inside :is()/:not() never matches an element.
            s.pseudo_element.is_none()
                && s.compounds.len() <= crate::rules::MAX_COMPOUNDS
                && matches!(
                    self.match_from(&s.compounds, 0, element, depth),
                    Outcome::Matched
                )
        })
    }

    /// Matches `compounds[i..]` with `compounds[i]` at `element`.
    fn match_from(&self, compounds: &[Compound], i: usize, element: NodeId, depth: u32) -> Outcome {
        let Some(compound) = compounds.get(i) else {
            return Outcome::Matched;
        };
        if !self.matches_compound(compound, element, depth) {
            return Outcome::RetrySibling;
        }
        let next = i + 1;
        if next >= compounds.len() {
            return Outcome::Matched;
        }
        match compound.combinator {
            None => Outcome::Matched,
            Some(Combinator::Child) => match self.parent_element(element) {
                None => Outcome::Fail,
                Some(p) => match self.match_from(compounds, next, p, depth) {
                    Outcome::RetrySibling => Outcome::RetryAncestor,
                    o => o,
                },
            },
            Some(Combinator::Descendant) => {
                let mut cur = self.parent_element(element);
                while let Some(a) = cur {
                    match self.match_from(compounds, next, a, depth) {
                        Outcome::Matched => return Outcome::Matched,
                        Outcome::Fail => return Outcome::Fail,
                        Outcome::RetryAncestor | Outcome::RetrySibling => {}
                    }
                    cur = self.parent_element(a);
                }
                Outcome::Fail
            }
            Some(Combinator::NextSibling) => match self.prev_element_sibling(element) {
                None => Outcome::RetryAncestor,
                Some(s) => self.match_from(compounds, next, s, depth),
            },
            Some(Combinator::SubsequentSibling) => {
                let mut cur = self.prev_element_sibling(element);
                while let Some(s) = cur {
                    match self.match_from(compounds, next, s, depth) {
                        Outcome::RetrySibling => {}
                        o => return o,
                    }
                    cur = self.prev_element_sibling(s);
                }
                Outcome::RetryAncestor
            }
        }
    }

    fn matches_compound(&self, compound: &Compound, element: NodeId, depth: u32) -> bool {
        let Some(el) = self.doc.element(element) else {
            return false;
        };
        compound
            .selectors
            .iter()
            .all(|s| self.matches_simple(s, element, el, depth))
    }

    fn matches_simple(
        &self,
        s: &SimpleSelector,
        node: NodeId,
        el: &ElementData,
        depth: u32,
    ) -> bool {
        match s {
            SimpleSelector::Universal => true,
            SimpleSelector::Type {
                name,
                lowercase_name,
            } => {
                if el.namespace == Namespace::Html {
                    el.local_name == *lowercase_name
                } else {
                    el.local_name == *name
                }
            }
            SimpleSelector::Id(id) => el.id().is_some_and(|v| self.eq_quirky(v, id)),
            SimpleSelector::Class(c) => el.classes().any(|v| self.eq_quirky(v, c)),
            SimpleSelector::Attribute(a) => matches_attribute(a, el),
            SimpleSelector::PseudoClass(pc) => self.matches_pseudo_class(pc, node, el, depth),
        }
    }

    /// Ids and classes match case-insensitively in quirks mode (§6.6, §6.7).
    fn eq_quirky(&self, a: &str, b: &str) -> bool {
        if self.quirks {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    }

    fn matches_pseudo_class(
        &self,
        pc: &PseudoClass,
        node: NodeId,
        el: &ElementData,
        depth: u32,
    ) -> bool {
        let d = depth.saturating_add(1);
        let info = self.info(node);
        match pc {
            PseudoClass::Not(list) => depth <= MAX_NESTING && !self.matches_list(list, node, d),
            PseudoClass::Is(list) | PseudoClass::Where(list) => self.matches_list(list, node, d),
            PseudoClass::Root => matches!(
                self.doc.parent(node).and_then(|p| self.doc.data(p)),
                Some(NodeData::Document { .. })
            ),
            PseudoClass::Empty => self.doc.children(node).all(|c| match self.doc.data(c) {
                Some(NodeData::Element(_)) => false,
                Some(NodeData::Text(t)) => t.is_empty(),
                _ => true,
            }),
            PseudoClass::FirstChild => info.index == 1,
            PseudoClass::LastChild => info.index == info.count,
            PseudoClass::OnlyChild => info.count == 1,
            PseudoClass::FirstOfType => info.type_index == 1,
            PseudoClass::LastOfType => info.type_index == info.type_count,
            PseudoClass::OnlyOfType => info.type_count == 1,
            PseudoClass::NthChild(nth, None) => nth.matches(to_i32(info.index)),
            PseudoClass::NthLastChild(nth, None) => nth.matches(to_i32(
                info.count.saturating_sub(info.index).saturating_add(1),
            )),
            PseudoClass::NthChild(nth, Some(of)) => {
                // M1 follow-up: cache `of S` positions; this scan is linear in
                // the number of siblings.
                if !self.matches_list(of, node, d) {
                    return false;
                }
                let mut pos = 1i32;
                let mut cur = self.prev_element_sibling(node);
                while let Some(s) = cur {
                    if self.matches_list(of, s, d) {
                        pos = pos.saturating_add(1);
                    }
                    cur = self.prev_element_sibling(s);
                }
                nth.matches(pos)
            }
            PseudoClass::NthLastChild(nth, Some(of)) => {
                if !self.matches_list(of, node, d) {
                    return false;
                }
                let mut pos = 1i32;
                let mut cur = self.next_element_sibling(node);
                while let Some(s) = cur {
                    if self.matches_list(of, s, d) {
                        pos = pos.saturating_add(1);
                    }
                    cur = self.next_element_sibling(s);
                }
                nth.matches(pos)
            }
            PseudoClass::NthOfType(nth) => nth.matches(to_i32(info.type_index)),
            PseudoClass::NthLastOfType(nth) => nth.matches(to_i32(
                info.type_count
                    .saturating_sub(info.type_index)
                    .saturating_add(1),
            )),
            // Every link counts as unvisited, so history cannot leak.
            PseudoClass::Link | PseudoClass::AnyLink => is_link(el),
            PseudoClass::Visited => false,
            PseudoClass::Hover => info.flags & HOVER != 0,
            PseudoClass::Active => info.flags & ACTIVE != 0,
            PseudoClass::Focus => info.flags & FOCUS != 0,
            PseudoClass::FocusWithin => info.flags & FOCUS_WITHIN != 0,
            PseudoClass::FocusVisible => self.focus_visible && info.flags & FOCUS != 0,
            PseudoClass::Checked => is_checked(el),
            PseudoClass::Disabled => is_form_control(el) && self.is_disabled(node, el),
            PseudoClass::Enabled => is_form_control(el) && !self.is_disabled(node, el),
        }
    }

    fn next_element_sibling(&self, node: NodeId) -> Option<NodeId> {
        let mut cur = self.doc.next_sibling(node);
        while let Some(n) = cur {
            if self.doc.is_element(n) {
                return Some(n);
            }
            cur = self.doc.next_sibling(n);
        }
        None
    }

    /// HTML §4.16.3 "actually disabled", approximated without form-associated
    /// custom elements.
    fn is_disabled(&self, node: NodeId, el: &ElementData) -> bool {
        if el.has_attribute("disabled") {
            return true;
        }
        if el.is_html("option") {
            return self
                .parent_element(node)
                .and_then(|p| self.doc.element(p))
                .is_some_and(|p| p.is_html("optgroup") && p.has_attribute("disabled"));
        }
        if el.is_html("optgroup") {
            return false;
        }
        // A descendant of a disabled fieldset, unless inside its first legend.
        let mut child = node;
        for a in self.doc.ancestors(node) {
            let Some(ae) = self.doc.element(a) else {
                break;
            };
            if ae.is_html("fieldset") && ae.has_attribute("disabled") {
                let first_legend = self
                    .doc
                    .element_children(a)
                    .find(|&c| self.doc.is_html_element(c, "legend"));
                if first_legend != Some(child) {
                    return true;
                }
            }
            child = a;
        }
        false
    }
}

fn to_i32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// How far a failed match reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Matched,
    /// Try an earlier sibling (or, failing that, an ancestor).
    RetrySibling,
    /// Siblings cannot help; an outer descendant combinator may try a
    /// higher ancestor.
    RetryAncestor,
    /// No other candidate can match.
    Fail,
}

/// `:link` / `:any-link`: `a` and `area` elements with an `href` (HTML
/// §4.16.3).
pub(crate) fn is_link(el: &ElementData) -> bool {
    (el.is_html("a") || el.is_html("area")) && el.has_attribute("href")
}

fn is_checked(el: &ElementData) -> bool {
    if el.is_html("input") {
        let ty = el.attribute("type").unwrap_or("");
        return (ty.eq_ignore_ascii_case("checkbox") || ty.eq_ignore_ascii_case("radio"))
            && el.has_attribute("checked");
    }
    el.is_html("option") && el.has_attribute("selected")
}

fn is_form_control(el: &ElementData) -> bool {
    el.namespace == Namespace::Html
        && matches!(
            el.local_name.as_str(),
            "button" | "input" | "select" | "textarea" | "optgroup" | "option" | "fieldset"
        )
}

/// Attributes whose values match case-insensitively on HTML elements when
/// no `i`/`s` flag is given (HTML §4.16.2 "Case-sensitivity of selectors").
const CASE_INSENSITIVE_ATTRIBUTES: &[&str] = &[
    "accept",
    "accept-charset",
    "align",
    "alink",
    "axis",
    "bgcolor",
    "charset",
    "checked",
    "clear",
    "codetype",
    "color",
    "compact",
    "declare",
    "defer",
    "dir",
    "direction",
    "disabled",
    "enctype",
    "face",
    "frame",
    "hreflang",
    "http-equiv",
    "lang",
    "language",
    "link",
    "media",
    "method",
    "multiple",
    "nohref",
    "noresize",
    "noshade",
    "nowrap",
    "readonly",
    "rel",
    "rev",
    "rules",
    "scope",
    "scrolling",
    "selected",
    "shape",
    "target",
    "text",
    "type",
    "valign",
    "valuetype",
    "vlink",
];

fn matches_attribute(a: &AttributeSelector, el: &ElementData) -> bool {
    let html = el.namespace == Namespace::Html;
    let wanted = if html { &a.lowercase_name } else { &a.name };
    let Some(attr) = el
        .attributes
        .iter()
        .find(|at| at.namespace.is_none() && at.local_name == *wanted)
    else {
        return false;
    };
    let Some((op, value)) = &a.operation else {
        return true;
    };
    let insensitive = match a.case {
        AttributeCase::Insensitive => true,
        AttributeCase::Sensitive => false,
        AttributeCase::Default => {
            html && CASE_INSENSITIVE_ATTRIBUTES.contains(&a.lowercase_name.as_str())
        }
    };
    let actual = attr.value.as_str();
    let eq = |x: &str, y: &str| {
        if insensitive {
            x.eq_ignore_ascii_case(y)
        } else {
            x == y
        }
    };
    match op {
        AttributeOperator::Equals => eq(actual, value),
        AttributeOperator::Includes => {
            !value.is_empty()
                && !value.contains(|c: char| c.is_ascii_whitespace())
                && actual.split_ascii_whitespace().any(|w| eq(w, value))
        }
        AttributeOperator::DashMatch => {
            eq(actual, value)
                || (actual.len() > value.len()
                    && actual.as_bytes().get(value.len()) == Some(&b'-')
                    && actual.get(..value.len()).is_some_and(|p| eq(p, value)))
        }
        AttributeOperator::Prefix => {
            !value.is_empty() && actual.get(..value.len()).is_some_and(|p| eq(p, value))
        }
        AttributeOperator::Suffix => {
            !value.is_empty()
                && actual
                    .len()
                    .checked_sub(value.len())
                    .and_then(|start| actual.get(start..))
                    .is_some_and(|p| eq(p, value))
        }
        AttributeOperator::Substring => {
            if value.is_empty() {
                false
            } else if insensitive {
                actual
                    .to_ascii_lowercase()
                    .contains(&value.to_ascii_lowercase())
            } else {
                actual.contains(value.as_str())
            }
        }
    }
}
