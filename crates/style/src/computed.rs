//! Computed values ([CSS Cascade 5 §4.4], [CSS Values 4]).
//!
//! A [`ComputedStyle`] holds the computed value of every supported longhand
//! for one element or pseudo-element. Lengths are resolved to CSS pixels
//! (`em`, `rem`, `ex`, `ch`, viewport units and the absolute units), colors to
//! [`Rgba`], and `url()`s to absolute URLs when a base URL is known.
//! Percentages that depend on layout stay unresolved as
//! [`LengthPercentage::Percentage`] or [`LengthPercentage::Calc`]; layout
//! resolves them with [`LengthPercentage::resolve`].
//!
//! Keyword types that need no resolution are re-exported from `wb-css`.
//!
//! [CSS Cascade 5 §4.4]: https://drafts.csswg.org/css-cascade-5/#computed
//! [CSS Values 4]: https://drafts.csswg.org/css-values-4/

use std::collections::HashMap;
use std::sync::Arc;

use wb_css::{
    AlignKeyword, BackgroundAttachment, BackgroundBox, BorderCollapse, BoxSizing, CaptionSide,
    Clear, ComponentValue, Content, CursorKeyword, CursorValue, Display, EmptyCells, FlexDirection,
    FlexWrap, Float, FontFamily, FontStyle, GenericFamily, Image, LineStyle, ListStylePosition,
    ListStyleType, Overflow, OverflowWrap, Position, PositionEdge, RepeatStyle, Rgba, TableLayout,
    TextAlign, TextDecorationLine, TextDecorationStyle, TextTransform, VerticalAlignKeyword,
    Visibility, WhiteSpace, WordBreak, ZIndex,
};

/// The initial font size in CSS pixels (`font-size: medium`).
pub const DEFAULT_FONT_SIZE: f32 = 16.0;

/// A computed math expression over pixels and percentages (CSS Values 4
/// §10.10): every length is already in CSS pixels, but percentages are kept
/// so layout can resolve them against the right basis.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcExpr {
    /// A length in CSS pixels.
    Px(f32),
    /// A percentage, in percent (`50%` is `50.0`).
    Percentage(f32),
    /// A plain number.
    Number(f32),
    /// The sum of the children.
    Sum(Vec<CalcExpr>),
    /// The child negated.
    Negate(Box<CalcExpr>),
    /// The product of the children.
    Product(Vec<CalcExpr>),
    /// One divided by the child.
    Invert(Box<CalcExpr>),
    /// `min(...)`.
    Min(Vec<CalcExpr>),
    /// `max(...)`.
    Max(Vec<CalcExpr>),
    /// `clamp(min, value, max)`.
    Clamp(Box<CalcExpr>, Box<CalcExpr>, Box<CalcExpr>),
}

/// Expression nesting is already bounded by `wb-css` (64 blocks); this is a
/// second guard so evaluation can never overflow the stack.
const MAX_CALC_DEPTH: u32 = 128;

impl CalcExpr {
    /// Evaluates the expression with `percent_basis` as the value of 100%.
    /// NaN results become 0 (CSS Values 4 §10.9).
    pub fn resolve(&self, percent_basis: f32) -> f32 {
        let v = self.eval(percent_basis, 0);
        if v.is_nan() { 0.0 } else { v }
    }

    fn eval(&self, basis: f32, depth: u32) -> f32 {
        if depth > MAX_CALC_DEPTH {
            return 0.0;
        }
        let d = depth.saturating_add(1);
        match self {
            CalcExpr::Px(v) | CalcExpr::Number(v) => *v,
            CalcExpr::Percentage(p) => p / 100.0 * basis,
            CalcExpr::Sum(v) => v.iter().map(|n| n.eval(basis, d)).sum(),
            CalcExpr::Negate(n) => -n.eval(basis, d),
            CalcExpr::Product(v) => v.iter().map(|n| n.eval(basis, d)).product(),
            CalcExpr::Invert(n) => 1.0 / n.eval(basis, d),
            CalcExpr::Min(v) => v
                .iter()
                .map(|n| n.eval(basis, d))
                .fold(f32::INFINITY, f32::min),
            CalcExpr::Max(v) => v
                .iter()
                .map(|n| n.eval(basis, d))
                .fold(f32::NEG_INFINITY, f32::max),
            CalcExpr::Clamp(min, val, max) => {
                let (min, val, max) = (min.eval(basis, d), val.eval(basis, d), max.eval(basis, d));
                min.max(val.min(max))
            }
        }
    }

    /// Whether the expression refers to a percentage anywhere.
    pub fn has_percentage(&self) -> bool {
        self.has_percentage_at(0)
    }

    fn has_percentage_at(&self, depth: u32) -> bool {
        if depth > MAX_CALC_DEPTH {
            return false;
        }
        let d = depth.saturating_add(1);
        match self {
            CalcExpr::Percentage(_) => true,
            CalcExpr::Px(_) | CalcExpr::Number(_) => false,
            CalcExpr::Sum(v) | CalcExpr::Product(v) | CalcExpr::Min(v) | CalcExpr::Max(v) => {
                v.iter().any(|n| n.has_percentage_at(d))
            }
            CalcExpr::Negate(n) | CalcExpr::Invert(n) => n.has_percentage_at(d),
            CalcExpr::Clamp(a, b, c) => {
                a.has_percentage_at(d) || b.has_percentage_at(d) || c.has_percentage_at(d)
            }
        }
    }
}

/// A computed `<length-percentage>`.
#[derive(Clone, Debug, PartialEq)]
pub enum LengthPercentage {
    /// A length in CSS pixels.
    Length(f32),
    /// A percentage, in percent (`50%` is `50.0`).
    Percentage(f32),
    /// A math expression that mixes lengths and percentages. Expressions
    /// without percentages are always folded into [`LengthPercentage::Length`].
    Calc(Arc<CalcExpr>),
}

impl LengthPercentage {
    /// Zero pixels.
    pub const ZERO: LengthPercentage = LengthPercentage::Length(0.0);

    /// Resolves to pixels with `percent_basis` as the value of 100%.
    pub fn resolve(&self, percent_basis: f32) -> f32 {
        match self {
            LengthPercentage::Length(px) => *px,
            LengthPercentage::Percentage(p) => p / 100.0 * percent_basis,
            LengthPercentage::Calc(c) => c.resolve(percent_basis),
        }
    }

    /// The value in pixels if it does not depend on a percentage basis.
    pub fn to_px(&self) -> Option<f32> {
        match self {
            LengthPercentage::Length(px) => Some(*px),
            _ => None,
        }
    }

    /// Whether the value depends on a percentage basis.
    pub fn has_percentage(&self) -> bool {
        !matches!(self, LengthPercentage::Length(_))
    }
}

/// A computed `<length-percentage> | auto`.
#[derive(Clone, Debug, PartialEq)]
pub enum LengthPercentageOrAuto {
    /// `auto`
    Auto,
    /// A length or percentage.
    LengthPercentage(LengthPercentage),
}

impl LengthPercentageOrAuto {
    /// Zero pixels.
    pub const ZERO: LengthPercentageOrAuto =
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::ZERO);

    /// Whether this is `auto`.
    pub fn is_auto(&self) -> bool {
        matches!(self, LengthPercentageOrAuto::Auto)
    }

    /// Resolves to pixels, or `None` for `auto`.
    pub fn resolve(&self, percent_basis: f32) -> Option<f32> {
        match self {
            LengthPercentageOrAuto::Auto => None,
            LengthPercentageOrAuto::LengthPercentage(lp) => Some(lp.resolve(percent_basis)),
        }
    }
}

/// A computed `width` / `height` / `min-width` / `min-height` /
/// `flex-basis` size (CSS Sizing 3 §3.1).
#[derive(Clone, Debug, PartialEq)]
pub enum Size {
    /// `auto`
    Auto,
    /// A non-negative length or percentage.
    LengthPercentage(LengthPercentage),
    /// `min-content`
    MinContent,
    /// `max-content`
    MaxContent,
    /// `fit-content` or `fit-content(<length-percentage>)`.
    FitContent(Option<LengthPercentage>),
}

/// A computed `max-width` / `max-height` (CSS Sizing 3 §3.2).
#[derive(Clone, Debug, PartialEq)]
pub enum MaxSize {
    /// `none`
    None,
    /// A non-negative length or percentage.
    LengthPercentage(LengthPercentage),
    /// `min-content`
    MinContent,
    /// `max-content`
    MaxContent,
    /// `fit-content` or `fit-content(<length-percentage>)`.
    FitContent(Option<LengthPercentage>),
}

/// A computed `flex-basis`.
#[derive(Clone, Debug, PartialEq)]
pub enum FlexBasis {
    /// `content`
    Content,
    /// A `width`-like value (including `auto`).
    Size(Size),
}

/// A computed `line-height` (CSS Inline 3 §5.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    /// `normal`: layout picks a value from the font metrics (about 1.2).
    Normal,
    /// A number: multiplies the element's own font size, and is inherited
    /// as the number.
    Number(f32),
    /// A length in CSS pixels (percentages are resolved against the font
    /// size at computed-value time).
    Length(f32),
}

impl LineHeight {
    /// The line height in pixels for `font_size`, with `normal` as
    /// `normal_factor` times the font size.
    pub fn to_px(self, font_size: f32, normal_factor: f32) -> f32 {
        match self {
            LineHeight::Normal => font_size * normal_factor,
            LineHeight::Number(n) => font_size * n,
            LineHeight::Length(px) => px,
        }
    }
}

/// A computed `vertical-align`.
#[derive(Clone, Debug, PartialEq)]
pub enum VerticalAlign {
    /// A keyword.
    Keyword(VerticalAlignKeyword),
    /// A length, or a percentage of the element's own `line-height`.
    LengthPercentage(LengthPercentage),
}

/// A computed gap (`row-gap` / `column-gap`).
#[derive(Clone, Debug, PartialEq)]
pub enum Gap {
    /// `normal` (0 in flex layout).
    Normal,
    /// A non-negative length or percentage.
    LengthPercentage(LengthPercentage),
}

/// One computed `border-*-*-radius`.
#[derive(Clone, Debug, PartialEq)]
pub struct CornerRadius {
    /// The horizontal radius (percentages refer to the border box width).
    pub horizontal: LengthPercentage,
    /// The vertical radius (percentages refer to the border box height).
    pub vertical: LengthPercentage,
}

impl CornerRadius {
    /// A square zero radius.
    pub const ZERO: CornerRadius = CornerRadius {
        horizontal: LengthPercentage::ZERO,
        vertical: LengthPercentage::ZERO,
    };
}

/// A value for each of the four sides of a box.
#[derive(Clone, Debug, PartialEq)]
pub struct Sides<T> {
    /// The top side.
    pub top: T,
    /// The right side.
    pub right: T,
    /// The bottom side.
    pub bottom: T,
    /// The left side.
    pub left: T,
}

impl<T: Clone> Sides<T> {
    /// The same value on every side.
    pub fn all(value: T) -> Self {
        Sides {
            top: value.clone(),
            right: value.clone(),
            bottom: value.clone(),
            left: value,
        }
    }
}

/// A value for each of the four corners of a box.
#[derive(Clone, Debug, PartialEq)]
pub struct Corners<T> {
    /// The top-left corner.
    pub top_left: T,
    /// The top-right corner.
    pub top_right: T,
    /// The bottom-right corner.
    pub bottom_right: T,
    /// The bottom-left corner.
    pub bottom_left: T,
}

impl<T: Clone> Corners<T> {
    /// The same value on every corner.
    pub fn all(value: T) -> Self {
        Corners {
            top_left: value.clone(),
            top_right: value.clone(),
            bottom_right: value.clone(),
            bottom_left: value,
        }
    }
}

/// One computed border side (CSS Backgrounds 3 §3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderSide {
    /// The width in CSS pixels. It is 0 when `style` is `none` or `hidden`
    /// (§3.3: the computed width is then zero).
    pub width: f32,
    /// The line style.
    pub style: LineStyle,
    /// The color, with `currentcolor` resolved.
    pub color: Rgba,
}

impl BorderSide {
    /// Whether the side draws anything.
    pub fn is_visible(&self) -> bool {
        self.width > 0.0 && !matches!(self.style, LineStyle::None | LineStyle::Hidden)
    }
}

/// A computed `<bg-position>` component: an offset from an edge.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionComponent {
    /// The edge the offset is measured from.
    pub edge: PositionEdge,
    /// The offset (percentages refer to the positioning area minus the
    /// image size, CSS Backgrounds 3 §2.6).
    pub offset: LengthPercentage,
}

/// A computed `background-size` value.
#[derive(Clone, Debug, PartialEq)]
pub enum BackgroundSize {
    /// `cover`
    Cover,
    /// `contain`
    Contain,
    /// Explicit width and height; `None` means `auto`.
    Explicit {
        /// The width, or `auto`.
        width: Option<LengthPercentage>,
        /// The height, or `auto`.
        height: Option<LengthPercentage>,
    },
}

/// One background layer, with every `background-*` list already repeated
/// to the number of `background-image` values (CSS Backgrounds 3 §2.2).
/// The first layer is painted on top.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundLayer {
    /// The image; `url()`s are absolute when the base URL was known.
    pub image: Image,
    /// Horizontal repetition.
    pub repeat_x: RepeatStyle,
    /// Vertical repetition.
    pub repeat_y: RepeatStyle,
    /// Horizontal position.
    pub position_x: PositionComponent,
    /// Vertical position.
    pub position_y: PositionComponent,
    /// The size.
    pub size: BackgroundSize,
    /// The attachment.
    pub attachment: BackgroundAttachment,
    /// The positioning area.
    pub origin: BackgroundBox,
    /// The painting area.
    pub clip: BackgroundBox,
}

impl BackgroundLayer {
    /// The initial layer: no image, `repeat`, `0% 0%`, `auto`, `scroll`,
    /// `padding-box` origin and `border-box` clip.
    pub fn initial() -> Self {
        BackgroundLayer {
            image: Image::None,
            repeat_x: RepeatStyle::Repeat,
            repeat_y: RepeatStyle::Repeat,
            position_x: PositionComponent {
                edge: PositionEdge::Start,
                offset: LengthPercentage::Percentage(0.0),
            },
            position_y: PositionComponent {
                edge: PositionEdge::Start,
                offset: LengthPercentage::Percentage(0.0),
            },
            size: BackgroundSize::Explicit {
                width: None,
                height: None,
            },
            attachment: BackgroundAttachment::Scroll,
            origin: BackgroundBox::PaddingBox,
            clip: BackgroundBox::BorderBox,
        }
    }
}

/// A text decoration in effect on an element's text: its own
/// `text-decoration-*` values or one propagated from an ancestor (CSS Text
/// Decoration 3 §2.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextDecoration {
    /// The lines to draw.
    pub line: TextDecorationLine,
    /// The line style.
    pub style: TextDecorationStyle,
    /// The color, with `currentcolor` resolved against the decorating box.
    pub color: Rgba,
}

/// The computed values of every supported property for one element or
/// pseudo-element.
///
/// Styles are shared between elements through [`Arc`] when they are known to
/// be identical, so compare with [`Arc::ptr_eq`] only as a fast path.
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedStyle {
    // --- Box generation and positioning ---
    /// `display`, after blockification (CSS Display 3 §2.7): the root,
    /// floats, absolutely positioned boxes and flex/grid items have a
    /// block-level display. `display` on `::marker` is not used.
    pub display: Display,
    /// `position`.
    pub position: Position,
    /// `float`, mapped to `Left`/`Right`/`None` (the logical values resolve
    /// against the left-to-right direction). It is `None` for absolutely
    /// positioned boxes (CSS 2.1 §9.7).
    pub float: Float,
    /// `clear`, mapped to `Left`/`Right`/`Both`/`None`.
    pub clear: Clear,
    /// `top`, `right`, `bottom`, `left`.
    pub inset: Sides<LengthPercentageOrAuto>,
    /// `z-index`.
    pub z_index: ZIndex,

    // --- Box model ---
    /// `margin-*`.
    pub margin: Sides<LengthPercentageOrAuto>,
    /// `padding-*`.
    pub padding: Sides<LengthPercentage>,
    /// `border-*-width`, `border-*-style`, `border-*-color`.
    pub border: Sides<BorderSide>,
    /// `border-*-radius`.
    pub border_radius: Corners<CornerRadius>,
    /// `width`.
    pub width: Size,
    /// `height`.
    pub height: Size,
    /// `min-width`.
    pub min_width: Size,
    /// `min-height`.
    pub min_height: Size,
    /// `max-width`.
    pub max_width: MaxSize,
    /// `max-height`.
    pub max_height: MaxSize,
    /// `box-sizing`.
    pub box_sizing: BoxSizing,
    /// `overflow-x`, after the CSS Overflow 3 §3.1 fix-up (`visible`/`clip`
    /// become `auto`/`hidden` when the other axis scrolls).
    pub overflow_x: Overflow,
    /// `overflow-y`, after the same fix-up.
    pub overflow_y: Overflow,

    // --- Visual ---
    /// `visibility`.
    pub visibility: Visibility,
    /// `opacity`, clamped to 0..=1.
    pub opacity: f32,
    /// `color`.
    pub color: Rgba,
    /// `background-color`.
    pub background_color: Rgba,
    /// The background layers (at least one).
    pub background_layers: Arc<[BackgroundLayer]>,
    /// `cursor`; image URLs are absolute when the base URL was known.
    pub cursor: CursorValue,

    // --- Fonts ---
    /// `font-family`, in preference order (never empty).
    pub font_family: Arc<[FontFamily]>,
    /// `font-size` in CSS pixels.
    pub font_size: f32,
    /// `font-weight`, 1..=1000 (`bolder`/`lighter` resolved, CSS Fonts 4
    /// §2.2).
    pub font_weight: f32,
    /// `font-style`.
    pub font_style: FontStyle,
    /// `font-stretch`, as a percentage of the normal width.
    pub font_stretch: f32,
    /// `line-height`.
    pub line_height: LineHeight,

    // --- Text ---
    /// `text-align`. `match-parent` is resolved to the parent's value;
    /// `start`/`end` are kept (direction is left-to-right in M1).
    pub text_align: TextAlign,
    /// `text-indent`.
    pub text_indent: LengthPercentage,
    /// `text-transform`.
    pub text_transform: TextTransform,
    /// `text-decoration-line`.
    pub text_decoration_line: TextDecorationLine,
    /// `text-decoration-style`.
    pub text_decoration_style: TextDecorationStyle,
    /// `text-decoration-color`, `currentcolor` resolved.
    pub text_decoration_color: Rgba,
    /// The decorations to draw on this box's text: those propagated from
    /// ancestors plus its own, outermost first (CSS Text Decoration 3
    /// §2.1). Not propagated into floats, absolutely positioned boxes and
    /// atomic inlines.
    pub text_decorations: Arc<[TextDecoration]>,
    /// `white-space`.
    pub white_space: WhiteSpace,
    /// `word-break`.
    pub word_break: WordBreak,
    /// `overflow-wrap`.
    pub overflow_wrap: OverflowWrap,
    /// `letter-spacing` in CSS pixels (`normal` is 0; percentages are of
    /// the font size, CSS Text 4 §8.2).
    pub letter_spacing: f32,
    /// `word-spacing` in CSS pixels (`normal` is 0; percentages are of the
    /// font size).
    pub word_spacing: f32,
    /// `vertical-align`.
    pub vertical_align: VerticalAlign,

    // --- Lists and generated content ---
    /// `list-style-type`.
    pub list_style_type: ListStyleType,
    /// `list-style-position`.
    pub list_style_position: ListStylePosition,
    /// `list-style-image`.
    pub list_style_image: Image,
    /// `content`. On `::before`/`::after`, `normal` computes to `none`.
    /// `attr()` is replaced by the attribute's value as a string and
    /// `url()`s are absolute when the base URL was known.
    pub content: Content,
    /// `counter-reset` as `(name, value)` pairs.
    pub counter_reset: Arc<[(String, i32)]>,
    /// `counter-increment` as `(name, value)` pairs.
    pub counter_increment: Arc<[(String, i32)]>,

    // --- Tables ---
    /// `border-collapse`.
    pub border_collapse: BorderCollapse,
    /// `border-spacing` as `(horizontal, vertical)` CSS pixels.
    pub border_spacing: (f32, f32),
    /// `table-layout`.
    pub table_layout: TableLayout,
    /// `caption-side`.
    pub caption_side: CaptionSide,
    /// `empty-cells`.
    pub empty_cells: EmptyCells,

    // --- Flexbox and alignment ---
    /// `flex-direction`.
    pub flex_direction: FlexDirection,
    /// `flex-wrap`.
    pub flex_wrap: FlexWrap,
    /// `flex-grow`.
    pub flex_grow: f32,
    /// `flex-shrink`.
    pub flex_shrink: f32,
    /// `flex-basis`.
    pub flex_basis: FlexBasis,
    /// `order`.
    pub order: i32,
    /// `justify-content`.
    pub justify_content: AlignKeyword,
    /// `align-items`.
    pub align_items: AlignKeyword,
    /// `align-self` (`auto` is kept; it means the parent's `align-items`).
    pub align_self: AlignKeyword,
    /// `align-content`.
    pub align_content: AlignKeyword,
    /// `row-gap`.
    pub row_gap: Gap,
    /// `column-gap`.
    pub column_gap: Gap,

    // --- Custom properties ---
    /// Computed custom properties (`--*`), with `var()` already substituted.
    /// Shared with the parent when the element sets none.
    pub custom_properties: Arc<HashMap<String, Vec<ComponentValue>>>,
}

impl ComputedStyle {
    /// The initial values of every property (CSS Cascade 5 §7.1), as used for
    /// the root element's parent. `font-family` starts as `serif` and
    /// `color` as black, like common browsers.
    pub fn initial() -> Self {
        let black = Rgba::rgb(0, 0, 0);
        let no_border = BorderSide {
            width: 0.0,
            style: LineStyle::None,
            color: black,
        };
        ComputedStyle {
            display: Display::INLINE,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            inset: Sides::all(LengthPercentageOrAuto::Auto),
            z_index: ZIndex::Auto,
            margin: Sides::all(LengthPercentageOrAuto::ZERO),
            padding: Sides::all(LengthPercentage::ZERO),
            border: Sides::all(no_border),
            border_radius: Corners::all(CornerRadius::ZERO),
            width: Size::Auto,
            height: Size::Auto,
            min_width: Size::Auto,
            min_height: Size::Auto,
            max_width: MaxSize::None,
            max_height: MaxSize::None,
            box_sizing: BoxSizing::ContentBox,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            visibility: Visibility::Visible,
            opacity: 1.0,
            color: black,
            background_color: Rgba::TRANSPARENT,
            background_layers: Arc::from(vec![BackgroundLayer::initial()]),
            cursor: CursorValue {
                images: Vec::new(),
                keyword: CursorKeyword::Auto,
            },
            font_family: Arc::from(vec![FontFamily::Generic(GenericFamily::Serif)]),
            font_size: DEFAULT_FONT_SIZE,
            font_weight: 400.0,
            font_style: FontStyle::Normal,
            font_stretch: 100.0,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Start,
            text_indent: LengthPercentage::ZERO,
            text_transform: TextTransform::None,
            text_decoration_line: TextDecorationLine::default(),
            text_decoration_style: TextDecorationStyle::Solid,
            text_decoration_color: black,
            text_decorations: Arc::from(Vec::new()),
            white_space: WhiteSpace::Normal,
            word_break: WordBreak::Normal,
            overflow_wrap: OverflowWrap::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            vertical_align: VerticalAlign::Keyword(VerticalAlignKeyword::Baseline),
            list_style_type: ListStyleType::Named("disc".to_string()),
            list_style_position: ListStylePosition::Outside,
            list_style_image: Image::None,
            content: Content::Normal,
            counter_reset: Arc::from(Vec::new()),
            counter_increment: Arc::from(Vec::new()),
            border_collapse: BorderCollapse::Separate,
            border_spacing: (0.0, 0.0),
            table_layout: TableLayout::Auto,
            caption_side: CaptionSide::Top,
            empty_cells: EmptyCells::Show,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Nowrap,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: FlexBasis::Size(Size::Auto),
            order: 0,
            justify_content: AlignKeyword::Normal,
            align_items: AlignKeyword::Normal,
            align_self: AlignKeyword::Auto,
            align_content: AlignKeyword::Normal,
            row_gap: Gap::Normal,
            column_gap: Gap::Normal,
            custom_properties: Arc::new(HashMap::new()),
        }
    }

    /// Whether `display` is `none`: the element and its descendants
    /// generate no boxes.
    pub fn is_display_none(&self) -> bool {
        matches!(self.display, Display::None)
    }

    /// Whether the box is absolutely positioned (`absolute` or `fixed`).
    pub fn is_absolutely_positioned(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed)
    }

    /// Whether the box floats.
    pub fn is_floating(&self) -> bool {
        !matches!(self.float, Float::None)
    }

    /// Whether the box is out of flow (floated or absolutely positioned).
    pub fn is_out_of_flow(&self) -> bool {
        self.is_floating() || self.is_absolutely_positioned()
    }

    /// Whether this box creates a stacking context (CSS 2.1 Appendix E,
    /// CSS Position 3, CSS Color 4 for `opacity`).
    pub fn creates_stacking_context(&self) -> bool {
        let z = matches!(self.z_index, ZIndex::Integer(_));
        (z && !matches!(self.position, Position::Static))
            || matches!(self.position, Position::Fixed | Position::Sticky)
            || self.opacity < 1.0
    }

    /// The computed value of a custom property, without the leading `--`
    /// stripped (pass `"--name"`).
    pub fn custom_property(&self, name: &str) -> Option<&[ComponentValue]> {
        self.custom_properties.get(name).map(Vec::as_slice)
    }

    /// The line height in pixels, with `normal` taken as 1.2 times the
    /// font size (layout should use the font's metrics when it has them).
    pub fn line_height_px(&self) -> f32 {
        self.line_height.to_px(self.font_size, 1.2)
    }
}

impl Default for ComputedStyle {
    fn default() -> Self {
        ComputedStyle::initial()
    }
}
