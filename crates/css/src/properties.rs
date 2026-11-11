//! Properties and declarations.
//!
//! Every supported longhand has a variant in [`LonghandId`] and a typed value
//! in [`Longhand`]. Shorthands are expanded into longhands at parse time
//! (CSS Cascade 4 §1.2). Declarations that reference `var()` are kept as
//! token lists ([`DeclarationKind::WithVariables`]) for `wb-style` to
//! substitute at computed-value time and then re-parse with
//! [`parse_property_value`].

use crate::color::Color;
use crate::cursor::Cursor;
use crate::syntax::{ComponentValue, RawDeclaration};
use crate::types::*;
use crate::values::{Image, LengthPercentage};
use crate::variables::{is_declaration_value, var_references_valid};

macro_rules! longhands {
    ($(
        $(#[$doc:meta])*
        $variant:ident($ty:ty) = $name:literal, inherited: $inh:literal, parse: $parse:expr;
    )+) => {
        /// Identifies a supported longhand property.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum LonghandId {
            $(
                #[doc = concat!("`", $name, "`")]
                $variant,
            )+
        }

        impl LonghandId {
            /// Every supported longhand.
            pub const ALL: &'static [LonghandId] = &[$(LonghandId::$variant,)+];

            /// The property name.
            pub fn name(self) -> &'static str {
                match self {
                    $(LonghandId::$variant => $name,)+
                }
            }

            /// Looks up a longhand by its lowercase name (no aliases).
            fn from_lowercase_name(name: &str) -> Option<Self> {
                Some(match name {
                    $($name => LonghandId::$variant,)+
                    _ => return None,
                })
            }

            /// Whether the property is inherited by default.
            pub fn is_inherited(self) -> bool {
                match self {
                    $(LonghandId::$variant => $inh,)+
                }
            }
        }

        /// A longhand property with a typed specified value.
        #[derive(Clone, Debug, PartialEq)]
        pub enum Longhand {
            $(
                #[doc = concat!("`", $name, "`")]
                $(#[$doc])*
                $variant($ty),
            )+
        }

        impl Longhand {
            /// Which property this is.
            pub fn id(&self) -> LonghandId {
                match self {
                    $(Longhand::$variant(_) => LonghandId::$variant,)+
                }
            }
        }

        fn parse_longhand(id: LonghandId, c: &mut Cursor<'_>) -> Option<Longhand> {
            match id {
                $(LonghandId::$variant => {
                    let f: fn(&mut Cursor<'_>) -> Option<$ty> = $parse;
                    f(c).map(Longhand::$variant)
                })+
            }
        }
    };
}

longhands! {
    MarginTop(LengthPercentageOrAuto) = "margin-top", inherited: false, parse: lp_or_auto;
    MarginRight(LengthPercentageOrAuto) = "margin-right", inherited: false, parse: lp_or_auto;
    MarginBottom(LengthPercentageOrAuto) = "margin-bottom", inherited: false, parse: lp_or_auto;
    MarginLeft(LengthPercentageOrAuto) = "margin-left", inherited: false, parse: lp_or_auto;
    PaddingTop(LengthPercentage) = "padding-top", inherited: false, parse: lp_non_negative;
    PaddingRight(LengthPercentage) = "padding-right", inherited: false, parse: lp_non_negative;
    PaddingBottom(LengthPercentage) = "padding-bottom", inherited: false, parse: lp_non_negative;
    PaddingLeft(LengthPercentage) = "padding-left", inherited: false, parse: lp_non_negative;
    /// The width in a length (keywords are converted: thin 1px, medium 3px, thick 5px).
    BorderTopWidth(LengthPercentage) = "border-top-width", inherited: false, parse: line_width;
    /// See `BorderTopWidth`.
    BorderRightWidth(LengthPercentage) = "border-right-width", inherited: false, parse: line_width;
    /// See `BorderTopWidth`.
    BorderBottomWidth(LengthPercentage) = "border-bottom-width", inherited: false, parse: line_width;
    /// See `BorderTopWidth`.
    BorderLeftWidth(LengthPercentage) = "border-left-width", inherited: false, parse: line_width;
    BorderTopStyle(LineStyle) = "border-top-style", inherited: false, parse: LineStyle::parse;
    BorderRightStyle(LineStyle) = "border-right-style", inherited: false, parse: LineStyle::parse;
    BorderBottomStyle(LineStyle) = "border-bottom-style", inherited: false, parse: LineStyle::parse;
    BorderLeftStyle(LineStyle) = "border-left-style", inherited: false, parse: LineStyle::parse;
    BorderTopColor(Color) = "border-top-color", inherited: false, parse: color;
    BorderRightColor(Color) = "border-right-color", inherited: false, parse: color;
    BorderBottomColor(Color) = "border-bottom-color", inherited: false, parse: color;
    BorderLeftColor(Color) = "border-left-color", inherited: false, parse: color;
    BorderTopLeftRadius(CornerRadius) = "border-top-left-radius", inherited: false, parse: corner_radius;
    BorderTopRightRadius(CornerRadius) = "border-top-right-radius", inherited: false, parse: corner_radius;
    BorderBottomRightRadius(CornerRadius) = "border-bottom-right-radius", inherited: false, parse: corner_radius;
    BorderBottomLeftRadius(CornerRadius) = "border-bottom-left-radius", inherited: false, parse: corner_radius;
    Width(Size) = "width", inherited: false, parse: size;
    Height(Size) = "height", inherited: false, parse: size;
    MinWidth(Size) = "min-width", inherited: false, parse: size;
    MinHeight(Size) = "min-height", inherited: false, parse: size;
    MaxWidth(MaxSize) = "max-width", inherited: false, parse: max_size;
    MaxHeight(MaxSize) = "max-height", inherited: false, parse: max_size;
    BoxSizing(BoxSizing) = "box-sizing", inherited: false, parse: BoxSizing::parse;
    Display(Display) = "display", inherited: false, parse: display;
    Position(Position) = "position", inherited: false, parse: Position::parse;
    Top(LengthPercentageOrAuto) = "top", inherited: false, parse: lp_or_auto;
    Right(LengthPercentageOrAuto) = "right", inherited: false, parse: lp_or_auto;
    Bottom(LengthPercentageOrAuto) = "bottom", inherited: false, parse: lp_or_auto;
    Left(LengthPercentageOrAuto) = "left", inherited: false, parse: lp_or_auto;
    Float(Float) = "float", inherited: false, parse: Float::parse;
    Clear(Clear) = "clear", inherited: false, parse: Clear::parse;
    ZIndex(ZIndex) = "z-index", inherited: false, parse: z_index;
    OverflowX(Overflow) = "overflow-x", inherited: false, parse: overflow;
    OverflowY(Overflow) = "overflow-y", inherited: false, parse: overflow;
    Visibility(Visibility) = "visibility", inherited: true, parse: Visibility::parse;
    /// As a fraction; not clamped (clamp to 0..=1 at computed-value time).
    Opacity(f32) = "opacity", inherited: false, parse: alpha_value;
    Color(Color) = "color", inherited: true, parse: color;
    BackgroundColor(Color) = "background-color", inherited: false, parse: color;
    /// One entry per background layer.
    BackgroundImage(Vec<Image>) = "background-image", inherited: false, parse: |c| comma_list(c, image);
    /// One entry per background layer.
    BackgroundRepeat(Vec<BackgroundRepeat>) = "background-repeat", inherited: false, parse: |c| comma_list(c, background_repeat);
    /// One entry per background layer.
    BackgroundPosition(Vec<BackgroundPosition>) = "background-position", inherited: false, parse: |c| comma_list(c, background_position);
    /// One entry per background layer.
    BackgroundSize(Vec<BackgroundSize>) = "background-size", inherited: false, parse: |c| comma_list(c, background_size);
    /// One entry per background layer.
    BackgroundAttachment(Vec<BackgroundAttachment>) = "background-attachment", inherited: false, parse: |c| comma_list(c, BackgroundAttachment::parse);
    /// One entry per background layer.
    BackgroundOrigin(Vec<BackgroundBox>) = "background-origin", inherited: false, parse: |c| comma_list(c, visual_box);
    /// One entry per background layer.
    BackgroundClip(Vec<BackgroundBox>) = "background-clip", inherited: false, parse: |c| comma_list(c, clip_box);
    FontFamily(Vec<FontFamily>) = "font-family", inherited: true, parse: font_family_list;
    FontSize(FontSize) = "font-size", inherited: true, parse: font_size;
    FontWeight(FontWeight) = "font-weight", inherited: true, parse: font_weight;
    FontStyle(FontStyle) = "font-style", inherited: true, parse: font_style;
    /// As a percentage of the normal width (`condensed` is 75).
    FontStretch(f32) = "font-stretch", inherited: true, parse: font_stretch;
    LineHeight(LineHeight) = "line-height", inherited: true, parse: line_height;
    TextAlign(TextAlign) = "text-align", inherited: true, parse: text_align;
    TextDecorationLine(TextDecorationLine) = "text-decoration-line", inherited: false, parse: text_decoration_line;
    TextDecorationStyle(TextDecorationStyle) = "text-decoration-style", inherited: false, parse: TextDecorationStyle::parse;
    TextDecorationColor(Color) = "text-decoration-color", inherited: false, parse: color;
    TextTransform(TextTransform) = "text-transform", inherited: true, parse: TextTransform::parse;
    // M1 follow-up: `hanging` and `each-line` keywords.
    TextIndent(LengthPercentage) = "text-indent", inherited: true, parse: lp;
    WhiteSpace(WhiteSpace) = "white-space", inherited: true, parse: WhiteSpace::parse;
    WordBreak(WordBreak) = "word-break", inherited: true, parse: WordBreak::parse;
    OverflowWrap(OverflowWrap) = "overflow-wrap", inherited: true, parse: OverflowWrap::parse;
    LetterSpacing(Spacing) = "letter-spacing", inherited: true, parse: spacing;
    WordSpacing(Spacing) = "word-spacing", inherited: true, parse: spacing;
    VerticalAlign(VerticalAlign) = "vertical-align", inherited: false, parse: vertical_align;
    ListStyleType(ListStyleType) = "list-style-type", inherited: true, parse: list_style_type;
    ListStylePosition(ListStylePosition) = "list-style-position", inherited: true, parse: ListStylePosition::parse;
    ListStyleImage(Image) = "list-style-image", inherited: true, parse: image;
    BorderCollapse(BorderCollapse) = "border-collapse", inherited: true, parse: BorderCollapse::parse;
    BorderSpacing(BorderSpacing) = "border-spacing", inherited: true, parse: border_spacing;
    TableLayout(TableLayout) = "table-layout", inherited: false, parse: TableLayout::parse;
    CaptionSide(CaptionSide) = "caption-side", inherited: true, parse: CaptionSide::parse;
    EmptyCells(EmptyCells) = "empty-cells", inherited: true, parse: EmptyCells::parse;
    FlexDirection(FlexDirection) = "flex-direction", inherited: false, parse: FlexDirection::parse;
    FlexWrap(FlexWrap) = "flex-wrap", inherited: false, parse: FlexWrap::parse;
    FlexGrow(f32) = "flex-grow", inherited: false, parse: non_negative_number;
    FlexShrink(f32) = "flex-shrink", inherited: false, parse: non_negative_number;
    FlexBasis(FlexBasis) = "flex-basis", inherited: false, parse: flex_basis;
    JustifyContent(AlignKeyword) = "justify-content", inherited: false, parse: |c| alignment(c, AlignProp::JustifyContent);
    AlignItems(AlignKeyword) = "align-items", inherited: false, parse: |c| alignment(c, AlignProp::AlignItems);
    AlignSelf(AlignKeyword) = "align-self", inherited: false, parse: |c| alignment(c, AlignProp::AlignSelf);
    AlignContent(AlignKeyword) = "align-content", inherited: false, parse: |c| alignment(c, AlignProp::AlignContent);
    Order(i32) = "order", inherited: false, parse: integer;
    RowGap(Gap) = "row-gap", inherited: false, parse: gap;
    ColumnGap(Gap) = "column-gap", inherited: false, parse: gap;
    Cursor(CursorValue) = "cursor", inherited: true, parse: cursor;
    Content(Content) = "content", inherited: false, parse: content;
    /// `(name, value)` pairs; empty for `none`.
    CounterReset(Vec<(String, i32)>) = "counter-reset", inherited: false, parse: |c| counter_list(c, 0);
    /// `(name, value)` pairs; empty for `none`.
    CounterIncrement(Vec<(String, i32)>) = "counter-increment", inherited: false, parse: |c| counter_list(c, 1);
}

/// A shorthand property, expanded into longhands when parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShorthandId {
    /// `margin`
    Margin,
    /// `padding`
    Padding,
    /// `border`
    Border,
    /// `border-top`
    BorderTop,
    /// `border-right`
    BorderRight,
    /// `border-bottom`
    BorderBottom,
    /// `border-left`
    BorderLeft,
    /// `border-width`
    BorderWidth,
    /// `border-style`
    BorderStyle,
    /// `border-color`
    BorderColor,
    /// `border-radius`
    BorderRadius,
    /// `inset`
    Inset,
    /// `overflow`
    Overflow,
    /// `background`
    Background,
    /// `font`
    Font,
    /// `text-decoration`
    TextDecoration,
    /// `list-style`
    ListStyle,
    /// `flex-flow`
    FlexFlow,
    /// `flex`
    Flex,
    /// `gap`
    Gap,
}

impl ShorthandId {
    /// The property name.
    pub fn name(self) -> &'static str {
        use ShorthandId as S;
        match self {
            S::Margin => "margin",
            S::Padding => "padding",
            S::Border => "border",
            S::BorderTop => "border-top",
            S::BorderRight => "border-right",
            S::BorderBottom => "border-bottom",
            S::BorderLeft => "border-left",
            S::BorderWidth => "border-width",
            S::BorderStyle => "border-style",
            S::BorderColor => "border-color",
            S::BorderRadius => "border-radius",
            S::Inset => "inset",
            S::Overflow => "overflow",
            S::Background => "background",
            S::Font => "font",
            S::TextDecoration => "text-decoration",
            S::ListStyle => "list-style",
            S::FlexFlow => "flex-flow",
            S::Flex => "flex",
            S::Gap => "gap",
        }
    }

    fn from_lowercase_name(name: &str) -> Option<Self> {
        use ShorthandId as S;
        Some(match name {
            "margin" => S::Margin,
            "padding" => S::Padding,
            "border" => S::Border,
            "border-top" => S::BorderTop,
            "border-right" => S::BorderRight,
            "border-bottom" => S::BorderBottom,
            "border-left" => S::BorderLeft,
            "border-width" => S::BorderWidth,
            "border-style" => S::BorderStyle,
            "border-color" => S::BorderColor,
            "border-radius" => S::BorderRadius,
            "inset" => S::Inset,
            "overflow" => S::Overflow,
            "background" => S::Background,
            "font" => S::Font,
            "text-decoration" => S::TextDecoration,
            "list-style" => S::ListStyle,
            "flex-flow" => S::FlexFlow,
            "flex" => S::Flex,
            "gap" => S::Gap,
            _ => return None,
        })
    }

    /// The longhands this shorthand sets (and resets).
    pub fn longhands(self) -> &'static [LonghandId] {
        use LonghandId as L;
        use ShorthandId as S;
        match self {
            S::Margin => &[L::MarginTop, L::MarginRight, L::MarginBottom, L::MarginLeft],
            S::Padding => &[
                L::PaddingTop,
                L::PaddingRight,
                L::PaddingBottom,
                L::PaddingLeft,
            ],
            S::Border => &[
                L::BorderTopWidth,
                L::BorderRightWidth,
                L::BorderBottomWidth,
                L::BorderLeftWidth,
                L::BorderTopStyle,
                L::BorderRightStyle,
                L::BorderBottomStyle,
                L::BorderLeftStyle,
                L::BorderTopColor,
                L::BorderRightColor,
                L::BorderBottomColor,
                L::BorderLeftColor,
            ],
            S::BorderTop => &[L::BorderTopWidth, L::BorderTopStyle, L::BorderTopColor],
            S::BorderRight => &[
                L::BorderRightWidth,
                L::BorderRightStyle,
                L::BorderRightColor,
            ],
            S::BorderBottom => &[
                L::BorderBottomWidth,
                L::BorderBottomStyle,
                L::BorderBottomColor,
            ],
            S::BorderLeft => &[L::BorderLeftWidth, L::BorderLeftStyle, L::BorderLeftColor],
            S::BorderWidth => &[
                L::BorderTopWidth,
                L::BorderRightWidth,
                L::BorderBottomWidth,
                L::BorderLeftWidth,
            ],
            S::BorderStyle => &[
                L::BorderTopStyle,
                L::BorderRightStyle,
                L::BorderBottomStyle,
                L::BorderLeftStyle,
            ],
            S::BorderColor => &[
                L::BorderTopColor,
                L::BorderRightColor,
                L::BorderBottomColor,
                L::BorderLeftColor,
            ],
            S::BorderRadius => &[
                L::BorderTopLeftRadius,
                L::BorderTopRightRadius,
                L::BorderBottomRightRadius,
                L::BorderBottomLeftRadius,
            ],
            S::Inset => &[L::Top, L::Right, L::Bottom, L::Left],
            S::Overflow => &[L::OverflowX, L::OverflowY],
            S::Background => &[
                L::BackgroundColor,
                L::BackgroundImage,
                L::BackgroundRepeat,
                L::BackgroundPosition,
                L::BackgroundSize,
                L::BackgroundAttachment,
                L::BackgroundOrigin,
                L::BackgroundClip,
            ],
            S::Font => &[
                L::FontStyle,
                L::FontWeight,
                L::FontStretch,
                L::FontSize,
                L::LineHeight,
                L::FontFamily,
            ],
            S::TextDecoration => &[
                L::TextDecorationLine,
                L::TextDecorationStyle,
                L::TextDecorationColor,
            ],
            S::ListStyle => &[L::ListStyleType, L::ListStylePosition, L::ListStyleImage],
            S::FlexFlow => &[L::FlexDirection, L::FlexWrap],
            S::Flex => &[L::FlexGrow, L::FlexShrink, L::FlexBasis],
            S::Gap => &[L::RowGap, L::ColumnGap],
        }
    }
}

/// A known property: a longhand or a shorthand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PropertyId {
    /// A longhand.
    Longhand(LonghandId),
    /// A shorthand.
    Shorthand(ShorthandId),
}

impl PropertyId {
    /// Looks up a property by name (ASCII case-insensitive), including legacy
    /// aliases such as `word-wrap` and `font-width`.
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let lower = match lower.as_str() {
            "word-wrap" => "overflow-wrap",
            "font-width" => "font-stretch",
            "grid-gap" => "gap",
            "grid-row-gap" => "row-gap",
            "grid-column-gap" => "column-gap",
            other => other,
        };
        if let Some(l) = LonghandId::from_lowercase_name(lower) {
            return Some(PropertyId::Longhand(l));
        }
        ShorthandId::from_lowercase_name(lower).map(PropertyId::Shorthand)
    }

    /// The property name.
    pub fn name(self) -> &'static str {
        match self {
            PropertyId::Longhand(l) => l.name(),
            PropertyId::Shorthand(s) => s.name(),
        }
    }

    /// The longhands this property sets.
    pub fn longhands(self) -> Vec<LonghandId> {
        match self {
            PropertyId::Longhand(l) => vec![l],
            PropertyId::Shorthand(s) => s.longhands().to_vec(),
        }
    }
}

keyword_enum! {
    /// A CSS-wide keyword (CSS Cascade 4 §7.3).
    CssWideKeyword {
        Initial = "initial",
        Inherit = "inherit",
        Unset = "unset",
        Revert = "revert",
        RevertLayer = "revert-layer",
    }
}

/// The value of a custom property (`--name`).
#[derive(Clone, Debug, PartialEq)]
pub enum CustomValue {
    /// The value's component values, trimmed of surrounding whitespace. It
    /// may itself contain `var()` references.
    Tokens(Vec<ComponentValue>),
    /// A CSS-wide keyword.
    CssWide(CssWideKeyword),
}

/// What a declaration sets.
#[derive(Clone, Debug, PartialEq)]
pub enum DeclarationKind {
    /// A longhand with a typed value.
    Longhand(Longhand),
    /// A longhand set to a CSS-wide keyword.
    CssWide(LonghandId, CssWideKeyword),
    /// A custom property.
    Custom {
        /// The name, including the leading `--` (case-sensitive).
        name: String,
        /// The value.
        value: CustomValue,
    },
    /// A known property whose value contains `var()`. It applies to all of
    /// `property.longhands()`; after substitution, re-parse it with
    /// [`parse_property_value`]. If that fails the declaration is "invalid at
    /// computed-value time" (CSS Variables 1 §3.1).
    WithVariables {
        /// The property as written (a longhand or shorthand).
        property: PropertyId,
        /// The unsubstituted value.
        value: Vec<ComponentValue>,
    },
    /// An unsupported property, kept so nothing is lost; ignore it when
    /// styling.
    Unknown {
        /// The name as written.
        name: String,
        /// The raw value.
        value: Vec<ComponentValue>,
    },
}

/// One declaration of a declaration block.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// What it sets.
    pub kind: DeclarationKind,
    /// `!important`.
    pub important: bool,
}

/// An ordered list of declarations, as written. A shorthand contributes one
/// declaration per longhand, in place.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclarationBlock {
    /// The declarations in source order.
    pub declarations: Vec<Declaration>,
}

/// Whether the value contains a `var()` (or `env()`) reference anywhere.
pub fn contains_var(values: &[ComponentValue]) -> bool {
    values.iter().any(|v| match v {
        ComponentValue::Function { name, arguments } => {
            name.eq_ignore_ascii_case("var")
                || name.eq_ignore_ascii_case("env")
                || contains_var(arguments)
        }
        ComponentValue::Block { contents, .. } => contains_var(contents),
        _ => false,
    })
}

/// Parses a declaration into zero or more declarations. Invalid values yield
/// nothing (the declaration is dropped, CSS Syntax 3 §5.4.6 / CSS Cascade 4).
pub(crate) fn parse_declaration(raw: &RawDeclaration<'_>, out: &mut Vec<Declaration>) {
    let important = raw.important;
    let mut push = |kind| out.push(Declaration { kind, important });
    if raw.name.starts_with("--") {
        // The value must be a `<declaration-value>` with well-formed `var()`
        // references, or the declaration is invalid (CSS Variables 1 §2, §3).
        if !is_declaration_value(raw.value, false) || !var_references_valid(raw.value) {
            return;
        }
        let value = match css_wide(raw.value) {
            Some(k) => CustomValue::CssWide(k),
            None => CustomValue::Tokens(raw.value.to_vec()),
        };
        push(DeclarationKind::Custom {
            name: raw.name.to_string(),
            value,
        });
        return;
    }
    let Some(property) = PropertyId::from_name(raw.name) else {
        push(DeclarationKind::Unknown {
            name: raw.name.to_string(),
            value: raw.value.to_vec(),
        });
        return;
    };
    if raw.value.is_empty() {
        return;
    }
    if let Some(k) = css_wide(raw.value) {
        for l in property.longhands() {
            push(DeclarationKind::CssWide(l, k));
        }
        return;
    }
    if contains_var(raw.value) {
        // Malformed `var()` syntax invalidates the declaration at parse time.
        if !is_declaration_value(raw.value, false) || !var_references_valid(raw.value) {
            return;
        }
        push(DeclarationKind::WithVariables {
            property,
            value: raw.value.to_vec(),
        });
        return;
    }
    if let Some(longhands) = parse_property_value(property, raw.value) {
        for l in longhands {
            push(DeclarationKind::Longhand(l));
        }
    }
}

fn css_wide(value: &[ComponentValue]) -> Option<CssWideKeyword> {
    match value {
        [ComponentValue::Ident(i)] => CssWideKeyword::from_ident(i),
        _ => None,
    }
}

/// Parses a property value into typed longhands (a shorthand expands to all
/// of its longhands). Returns `None` if the value is invalid for the property.
///
/// This is also the re-parse step after `var()` substitution. It does not
/// handle CSS-wide keywords or `var()`; callers check for those first.
pub fn parse_property_value(
    property: PropertyId,
    value: &[ComponentValue],
) -> Option<Vec<Longhand>> {
    match property {
        PropertyId::Longhand(id) => {
            Cursor::parse_all(value, |c| parse_longhand(id, c)).map(|l| vec![l])
        }
        PropertyId::Shorthand(s) => Cursor::parse_all(value, |c| crate::shorthands::parse(s, c)),
    }
}
