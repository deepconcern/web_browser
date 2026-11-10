//! CSS parsing for the browser: stylesheets, selectors, media queries and
//! typed property values.
//!
//! `cssparser` (Servo-origin, temporary scaffolding per ADR-0001) is used
//! only as a tokenizer. Its types never appear in this API: tokens are
//! converted into our own [`ComponentValue`] tree, and every grammar above
//! that (rules, error recovery, selectors, media queries, values) is
//! implemented here from the specs.
//!
//! # Entry points
//!
//! - [`parse_stylesheet`] for `<style>` contents and fetched sheets.
//!   `@import` URLs come back in [`Stylesheet::imports`] for the caller to
//!   fetch.
//! - [`parse_style_attribute`] for `style=""`.
//! - [`parse_selector_list`] for `querySelector`-style APIs.
//! - [`parse_media_query_list`] for `media=""` attributes; evaluate with
//!   [`MediaQueryList::matches`] and a [`MediaContext`].
//! - [`substitute_variables`] and [`parse_property_value`] for `var()`
//!   resolution in `wb-style`.
//!
//! Selector matching, the cascade and computed values live in `wb-style`.
#![warn(missing_docs)]

#[macro_use]
mod macros;
mod color;
mod cursor;
mod media;
mod properties;
#[cfg(test)]
mod property_tests;
mod selector;
mod shorthands;
#[cfg(test)]
mod stress_tests;
mod stylesheet;
mod syntax;
mod types;
mod values;
mod variables;

pub use color::{Color, Rgba, SystemColor, parse_color_str};
pub use media::{
    ColorScheme, Comparison, FeatureName, MediaCondition, MediaContext, MediaFeature, MediaQuery,
    MediaQueryList, MediaType, MediaValue, Pointer, parse_media_query_list,
};
pub use properties::{
    CssWideKeyword, CustomValue, Declaration, DeclarationBlock, DeclarationKind, Longhand,
    LonghandId, PropertyId, ShorthandId, contains_var, parse_property_value,
};
pub use selector::{
    AttributeCase, AttributeOperator, AttributeSelector, Combinator, Compound, Nth, PseudoClass,
    PseudoElement, Selector, SelectorList, SelectorParseError, SimpleSelector, Specificity,
    parse_selector_list,
};
pub use stylesheet::{
    CssRule, ImportRule, LayerBlockRule, MediaRule, StyleRule, Stylesheet, SupportsRule,
    charset_from_bytes, parse_style_attribute, parse_stylesheet,
};
pub use syntax::{BlockKind, ComponentValue, Numeric, parse_component_values, serialize};
pub use types::{
    AlignKeyword, BackgroundAttachment, BackgroundBox, BackgroundPosition, BackgroundRepeat,
    BackgroundSize, BorderCollapse, BorderSpacing, BoxSizing, CaptionSide, Clear, Content,
    ContentItem, CornerRadius, CursorImage, CursorKeyword, CursorValue, Display, DisplayInside,
    DisplayInternal, DisplayOutside, EmptyCells, FlexBasis, FlexDirection, FlexWrap, Float,
    FontFamily, FontSize, FontSizeKeyword, FontStyle, FontWeight, Gap, GenericFamily,
    LengthPercentageOrAuto, LineHeight, LineStyle, ListStylePosition, ListStyleType, MaxSize,
    Overflow, OverflowWrap, Position, PositionComponent, PositionEdge, RepeatStyle, Size, Spacing,
    TableLayout, TextAlign, TextDecorationLine, TextDecorationStyle, TextTransform, VerticalAlign,
    VerticalAlignKeyword, Visibility, WhiteSpace, WordBreak, ZIndex,
};
pub use values::{CalcNode, Image, Length, LengthPercentage, LengthUnit};
pub use variables::substitute_variables;
