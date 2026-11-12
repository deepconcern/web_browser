//! Selector matching, the cascade and computed values.
//!
//! # Overview
//!
//! 1. Build a [`Stylist`] from the document's stylesheets ([`SheetInput`],
//!    already fetched by the caller, `@import`s included) and a
//!    [`MediaContext`]. The UA stylesheet (HTML §15 "Rendering") is built
//!    in. A stylist only depends on the sheets and the media context, so
//!    keep it while neither changes.
//! 2. Call [`Stylist::style_document`] with the [`Document`] and
//!    [`StyleOptions`] (the base URL and the [`ElementState`] for `:hover`,
//!    `:focus` and `:active`). It returns a [`StyledDocument`] with a
//!    [`ComputedStyle`] for every element (and, through its parent, every
//!    text node), plus `::before`, `::after` and `::marker` styles.
//!
//! [`style_document`] does both steps at once.
//!
//! Use [`document_style_sources`] to find the `<style>` and
//! `<link rel=stylesheet>` elements of a document in tree order.
//!
//! # Spec coverage
//!
//! - Selectors 4 matching with specificity; `:link` matches every link and
//!   `:visited` never matches (privacy).
//! - CSS Cascade 5: origins, `!important`, the `style` attribute, cascade
//!   layers, `initial`/`inherit`/`unset`/`revert`/`revert-layer`.
//! - CSS Variables 1: custom properties and `var()`, with cycle detection.
//! - CSS Values 4 computed values (see [`computed`]).
//! - HTML §15: the UA stylesheet (including the quirks-mode additions) and
//!   presentational hints (`bgcolor`, `width`, `align`, `<font>`, ...).
#![warn(missing_docs)]

mod bloom;
mod cascade;
pub mod computed;
mod hints;
mod matching;
mod rules;
mod sources;
mod ua;
mod values;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use computed::{
    BackgroundLayer, BackgroundSize, BorderSide, CalcExpr, ComputedStyle, CornerRadius, Corners,
    DEFAULT_FONT_SIZE, FlexBasis, Gap, LengthPercentage, LengthPercentageOrAuto, LineHeight,
    MaxSize, PositionComponent, Sides, Size, TextDecoration, VerticalAlign,
};
pub use sources::{StyleSource, StyleSourceKind, document_style_sources};
// Keyword and value types used by computed values, re-exported so that
// layout and paint can name them without depending on `wb-css` directly.
pub use wb_css::{
    AlignKeyword, BackgroundAttachment, BackgroundBox, BorderCollapse, BoxSizing, CaptionSide,
    Clear, ColorScheme, ComponentValue, Content, ContentItem, CursorImage, CursorKeyword,
    CursorValue, Display, DisplayInside, DisplayInternal, DisplayOutside, EmptyCells,
    FlexDirection, FlexWrap, Float, FontFamily, FontStyle, GenericFamily, Image, LineStyle,
    ListStylePosition, ListStyleType, MediaContext, MediaQueryList, MediaType, Overflow,
    OverflowWrap, Position, PositionEdge, PseudoElement, RepeatStyle, Rgba, Stylesheet,
    TableLayout, TextAlign, TextDecorationLine, TextDecorationStyle, TextTransform,
    VerticalAlignKeyword, Visibility, WhiteSpace, WordBreak, ZIndex,
};
use wb_dom::{Document, NodeId};

/// A cascade origin (CSS Cascade 5 §6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// The user agent: the built-in stylesheet, plus any extra UA sheets.
    UserAgent,
    /// The user's own stylesheets.
    User,
    /// The page's stylesheets, `style` attributes and presentational hints.
    Author,
}

/// A stylesheet to apply, already parsed and with its imports fetched.
#[derive(Clone, Debug)]
pub struct SheetInput {
    /// The parsed stylesheet.
    pub sheet: Arc<Stylesheet>,
    /// Its origin.
    pub origin: Origin,
    /// The `media` attribute (or `@import` media list) the sheet is
    /// conditioned on; an empty list means all media.
    pub media: MediaQueryList,
    /// The sheet's URL (or the document's base URL for `<style>`), used to
    /// resolve `url()`s. `None` leaves them as written.
    pub base_url: Option<wb_url::Url>,
    /// The fetched `@import`ed sheets, one entry per [`Stylesheet::imports`]
    /// rule in the same order (`None` when the fetch failed or was skipped).
    /// Missing trailing entries count as `None`. Each import's own `media`
    /// and `layer` come from its rule; leave [`SheetInput::media`] empty on
    /// these. Nesting deeper than 16 levels is ignored.
    pub imports: Vec<Option<SheetInput>>,
}

impl SheetInput {
    /// An author stylesheet for all media, with no base URL and no imports.
    pub fn author(sheet: Stylesheet) -> Self {
        SheetInput {
            sheet: Arc::new(sheet),
            origin: Origin::Author,
            media: MediaQueryList::default(),
            base_url: None,
            imports: Vec::new(),
        }
    }
}

/// Dynamic element state for the user-action pseudo-classes (Selectors 4
/// §9). The hovered and active elements match together with their
/// ancestors; the focused element matches `:focus` and its ancestors
/// `:focus-within`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ElementState {
    /// The element under the pointer.
    pub hover: Option<NodeId>,
    /// The element being activated (e.g. mouse button held down).
    pub active: Option<NodeId>,
    /// The focused element.
    pub focus: Option<NodeId>,
    /// Whether the focus should be visibly indicated (`:focus-visible`),
    /// e.g. after keyboard navigation.
    pub focus_visible: bool,
}

/// Per-document inputs for [`Stylist::style_document`].
#[derive(Clone, Debug, Default)]
pub struct StyleOptions {
    /// The document's base URL, for `url()`s in `style` attributes and
    /// presentational hints (`background=""`).
    pub base_url: Option<wb_url::Url>,
    /// `:hover`, `:active` and `:focus` state.
    pub state: ElementState,
}

/// Compiled stylesheets for one media context: rules flattened from
/// `@media`/`@supports`/`@layer`/`@import`, bucketed by their rightmost id,
/// class or tag for fast matching.
#[derive(Debug)]
pub struct Stylist {
    pub(crate) rules: rules::RuleSet,
    pub(crate) media: MediaContext,
}

impl Stylist {
    /// Compiles the built-in UA stylesheet and `sheets` (in the order given,
    /// which should be document order) for `media`.
    pub fn new(sheets: &[SheetInput], media: &MediaContext) -> Stylist {
        Stylist {
            rules: rules::RuleSet::build(sheets, media),
            media: media.clone(),
        }
    }

    /// The media context the stylist was built for.
    pub fn media(&self) -> &MediaContext {
        &self.media
    }

    /// The number of selectors compiled (one per selector of each rule).
    pub fn selector_count(&self) -> usize {
        self.rules.selector_count()
    }

    /// Computes the style of every element of `doc` (CSS Cascade 5).
    pub fn style_document(&self, doc: &Document, options: &StyleOptions) -> StyledDocument {
        cascade::style_document(self, doc, options)
    }
}

/// Styles `doc` with the UA stylesheet plus `sheets`, for `media`.
pub fn style_document(
    doc: &Document,
    sheets: &[SheetInput],
    media: &MediaContext,
    options: &StyleOptions,
) -> StyledDocument {
    Stylist::new(sheets, media).style_document(doc, options)
}

/// Styles generated for an element's pseudo-elements.
#[derive(Clone, Debug, Default)]
pub(crate) struct PseudoStyles {
    pub(crate) before: Option<Arc<ComputedStyle>>,
    pub(crate) after: Option<Arc<ComputedStyle>>,
    pub(crate) marker: Option<Arc<ComputedStyle>>,
}

/// The result of styling a document.
#[derive(Debug)]
pub struct StyledDocument {
    /// Indexed by `NodeId::index()`; `None` for nodes that are not elements
    /// in the document tree.
    pub(crate) styles: Vec<Option<Arc<ComputedStyle>>>,
    pub(crate) pseudos: Vec<Option<Box<PseudoStyles>>>,
    pub(crate) parents: Vec<Option<NodeId>>,
    pub(crate) root: Option<NodeId>,
    pub(crate) initial: Arc<ComputedStyle>,
}

impl StyledDocument {
    /// The computed style of an element in the document tree, or of a text
    /// node (its parent element's style, which is what its text inherits).
    /// `None` for other nodes and nodes outside the document tree (e.g.
    /// template contents).
    pub fn style(&self, node: NodeId) -> Option<&Arc<ComputedStyle>> {
        let i = node.index();
        if let Some(Some(s)) = self.styles.get(i) {
            return Some(s);
        }
        let parent = self.parents.get(i).copied().flatten()?;
        self.styles.get(parent.index())?.as_ref()
    }

    /// The style of an element's pseudo-element. `::before` and `::after`
    /// are present only when they generate a box (`content` is not `none`
    /// and `display` is not `none`); `::marker` only on list items. Other
    /// pseudo-elements return `None` in M1.
    pub fn pseudo_style(&self, node: NodeId, pseudo: PseudoElement) -> Option<&Arc<ComputedStyle>> {
        let p = self.pseudos.get(node.index())?.as_deref()?;
        match pseudo {
            PseudoElement::Before => p.before.as_ref(),
            PseudoElement::After => p.after.as_ref(),
            PseudoElement::Marker => p.marker.as_ref(),
            _ => None,
        }
    }

    /// The root element (`<html>`), if the document has one.
    pub fn root_element(&self) -> Option<NodeId> {
        self.root
    }

    /// The root element's style, or the initial style when there is no root
    /// element (the style of the initial containing block).
    pub fn root_style(&self) -> &Arc<ComputedStyle> {
        self.root
            .and_then(|r| self.styles.get(r.index()))
            .and_then(Option::as_ref)
            .unwrap_or(&self.initial)
    }

    /// The number of distinct style objects (a measure of style sharing).
    pub fn distinct_style_count(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        for s in self.styles.iter().flatten() {
            seen.insert(Arc::as_ptr(s) as usize);
        }
        seen.len()
    }
}
