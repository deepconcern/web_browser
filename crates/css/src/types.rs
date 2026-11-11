//! Typed property values for the M1 property set, with their parsers.
//!
//! Each property's grammar is cited from the spec that defines it. Parsers
//! take a [`Cursor`] and return `None` (restoring the cursor) on failure.

use crate::color::{Color, parse_color};
use crate::cursor::Cursor;
use crate::syntax::ComponentValue;
use crate::values::{
    Image, LengthPercentage, Range, parse_angle, parse_image_or_none, parse_integer,
    parse_length_percentage, parse_number, parse_percentage, parse_string, parse_url,
};

// ---------------------------------------------------------------------------
// Generic building blocks
// ---------------------------------------------------------------------------

/// `<length-percentage> | auto`.
#[derive(Clone, Debug, PartialEq)]
pub enum LengthPercentageOrAuto {
    /// `auto`
    Auto,
    /// A length or percentage.
    LengthPercentage(LengthPercentage),
}

pub(crate) fn lp(c: &mut Cursor<'_>) -> Option<LengthPercentage> {
    parse_length_percentage(c, Range::All, true)
}

pub(crate) fn lp_non_negative(c: &mut Cursor<'_>) -> Option<LengthPercentage> {
    parse_length_percentage(c, Range::NonNegative, true)
}

pub(crate) fn length_non_negative(c: &mut Cursor<'_>) -> Option<LengthPercentage> {
    parse_length_percentage(c, Range::NonNegative, false)
}

pub(crate) fn lp_or_auto(c: &mut Cursor<'_>) -> Option<LengthPercentageOrAuto> {
    if c.eat_ident("auto") {
        return Some(LengthPercentageOrAuto::Auto);
    }
    lp(c).map(LengthPercentageOrAuto::LengthPercentage)
}

pub(crate) fn non_negative_number(c: &mut Cursor<'_>) -> Option<f32> {
    parse_number(c, Range::NonNegative)
}

pub(crate) fn integer(c: &mut Cursor<'_>) -> Option<i32> {
    parse_integer(c)
}

pub(crate) fn color(c: &mut Cursor<'_>) -> Option<Color> {
    parse_color(c)
}

pub(crate) fn image(c: &mut Cursor<'_>) -> Option<Image> {
    parse_image_or_none(c)
}

// ---------------------------------------------------------------------------
// Box model (CSS Box 3, CSS Sizing 3, CSS Backgrounds 3 for borders)
// ---------------------------------------------------------------------------

/// A value of `width`, `height`, `min-width`, `min-height` (CSS Sizing 3 §3.1).
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

/// A value of `max-width` / `max-height` (CSS Sizing 3 §3.2).
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

/// The intrinsic sizing keywords shared by `Size` and `MaxSize`.
enum Intrinsic {
    Min,
    Max,
    Fit(Option<LengthPercentage>),
}

fn intrinsic(c: &mut Cursor<'_>) -> Option<Intrinsic> {
    c.attempt(|c| match c.next()? {
        ComponentValue::Ident(i) => {
            let lower = i.to_ascii_lowercase();
            let lower = lower.strip_prefix("-webkit-").unwrap_or(&lower);
            match lower {
                "min-content" => Some(Intrinsic::Min),
                "max-content" => Some(Intrinsic::Max),
                "fit-content" => Some(Intrinsic::Fit(None)),
                _ => None,
            }
        }
        ComponentValue::Function { name, arguments }
            if name.eq_ignore_ascii_case("fit-content") =>
        {
            Cursor::parse_all(arguments, lp_non_negative).map(|l| Intrinsic::Fit(Some(l)))
        }
        _ => None,
    })
}

pub(crate) fn size(c: &mut Cursor<'_>) -> Option<Size> {
    if c.eat_ident("auto") {
        return Some(Size::Auto);
    }
    if let Some(i) = intrinsic(c) {
        return Some(match i {
            Intrinsic::Min => Size::MinContent,
            Intrinsic::Max => Size::MaxContent,
            Intrinsic::Fit(l) => Size::FitContent(l),
        });
    }
    lp_non_negative(c).map(Size::LengthPercentage)
}

pub(crate) fn max_size(c: &mut Cursor<'_>) -> Option<MaxSize> {
    if c.eat_ident("none") {
        return Some(MaxSize::None);
    }
    if let Some(i) = intrinsic(c) {
        return Some(match i {
            Intrinsic::Min => MaxSize::MinContent,
            Intrinsic::Max => MaxSize::MaxContent,
            Intrinsic::Fit(l) => MaxSize::FitContent(l),
        });
    }
    lp_non_negative(c).map(MaxSize::LengthPercentage)
}

/// `<line-width>` (CSS Backgrounds 3 §3.3): `thin` is 1px, `medium` 3px,
/// `thick` 5px. Percentages are not allowed.
pub(crate) fn line_width(c: &mut Cursor<'_>) -> Option<LengthPercentage> {
    use crate::values::Length;
    let kw = c.attempt(|c| {
        let i = c.ident()?;
        if i.eq_ignore_ascii_case("thin") {
            Some(1.0)
        } else if i.eq_ignore_ascii_case("medium") {
            Some(3.0)
        } else if i.eq_ignore_ascii_case("thick") {
            Some(5.0)
        } else {
            None
        }
    });
    if let Some(px) = kw {
        return Some(LengthPercentage::Length(Length::px(px)));
    }
    length_non_negative(c)
}

keyword_enum! {
    /// `<line-style>` (CSS Backgrounds 3 §3.2).
    LineStyle {
        None = "none",
        Hidden = "hidden",
        Dotted = "dotted",
        Dashed = "dashed",
        Solid = "solid",
        Double = "double",
        Groove = "groove",
        Ridge = "ridge",
        Inset = "inset",
        Outset = "outset",
    }
}

keyword_enum! {
    /// `box-sizing` (CSS Sizing 3 §4.1).
    BoxSizing {
        ContentBox = "content-box",
        BorderBox = "border-box",
    }
}

/// One corner's radii (CSS Backgrounds 3 §5.1).
#[derive(Clone, Debug, PartialEq)]
pub struct CornerRadius {
    /// The horizontal radius.
    pub horizontal: LengthPercentage,
    /// The vertical radius.
    pub vertical: LengthPercentage,
}

pub(crate) fn corner_radius(c: &mut Cursor<'_>) -> Option<CornerRadius> {
    let horizontal = lp_non_negative(c)?;
    let vertical = c
        .attempt(lp_non_negative)
        .unwrap_or_else(|| horizontal.clone());
    Some(CornerRadius {
        horizontal,
        vertical,
    })
}

// ---------------------------------------------------------------------------
// Display and positioning (CSS Display 3, CSS Position 3, CSS 2.1 §9)
// ---------------------------------------------------------------------------

/// The outer display type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayOutside {
    /// `block`
    Block,
    /// `inline`
    Inline,
    /// `run-in`
    RunIn,
}

/// The inner display type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayInside {
    /// `flow`
    Flow,
    /// `flow-root`
    FlowRoot,
    /// `table`
    Table,
    /// `flex`
    Flex,
    /// `grid`
    Grid,
    /// `ruby`
    Ruby,
}

/// Layout-internal display types (CSS Display 3 §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisplayInternal {
    /// `table-row-group`
    TableRowGroup,
    /// `table-header-group`
    TableHeaderGroup,
    /// `table-footer-group`
    TableFooterGroup,
    /// `table-row`
    TableRow,
    /// `table-cell`
    TableCell,
    /// `table-column-group`
    TableColumnGroup,
    /// `table-column`
    TableColumn,
    /// `table-caption`
    TableCaption,
    /// `ruby-base`
    RubyBase,
    /// `ruby-text`
    RubyText,
    /// `ruby-base-container`
    RubyBaseContainer,
    /// `ruby-text-container`
    RubyTextContainer,
}

/// The `display` property (CSS Display 3 §2), with legacy one-keyword
/// values mapped to their two-keyword equivalents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Display {
    /// `none`
    None,
    /// `contents`
    Contents,
    /// An outer and inner display type, plus whether it is a list item.
    Pair {
        /// The outer type.
        outside: DisplayOutside,
        /// The inner type.
        inside: DisplayInside,
        /// `list-item` was given.
        list_item: bool,
    },
    /// A layout-internal type.
    Internal(DisplayInternal),
}

impl Display {
    /// `display: block`.
    pub const BLOCK: Display = Display::Pair {
        outside: DisplayOutside::Block,
        inside: DisplayInside::Flow,
        list_item: false,
    };
    /// `display: inline` (the initial value).
    pub const INLINE: Display = Display::Pair {
        outside: DisplayOutside::Inline,
        inside: DisplayInside::Flow,
        list_item: false,
    };
}

pub(crate) fn display(c: &mut Cursor<'_>) -> Option<Display> {
    let first = c.ident()?.to_ascii_lowercase();
    let pair = |outside, inside| {
        Some(Display::Pair {
            outside,
            inside,
            list_item: false,
        })
    };
    use DisplayInside as I;
    use DisplayInternal as N;
    use DisplayOutside as O;
    // Single keywords that cannot combine with anything.
    let single = match first.as_str() {
        "none" => Some(Display::None),
        "contents" => Some(Display::Contents),
        "inline-block" => pair(O::Inline, I::FlowRoot),
        "inline-table" => pair(O::Inline, I::Table),
        "inline-flex" | "-webkit-inline-box" | "-webkit-inline-flex" => pair(O::Inline, I::Flex),
        "inline-grid" => pair(O::Inline, I::Grid),
        "-webkit-box" | "-webkit-flex" => pair(O::Block, I::Flex),
        "table-row-group" => Some(Display::Internal(N::TableRowGroup)),
        "table-header-group" => Some(Display::Internal(N::TableHeaderGroup)),
        "table-footer-group" => Some(Display::Internal(N::TableFooterGroup)),
        "table-row" => Some(Display::Internal(N::TableRow)),
        "table-cell" => Some(Display::Internal(N::TableCell)),
        "table-column-group" => Some(Display::Internal(N::TableColumnGroup)),
        "table-column" => Some(Display::Internal(N::TableColumn)),
        "table-caption" => Some(Display::Internal(N::TableCaption)),
        "ruby-base" => Some(Display::Internal(N::RubyBase)),
        "ruby-text" => Some(Display::Internal(N::RubyText)),
        "ruby-base-container" => Some(Display::Internal(N::RubyBaseContainer)),
        "ruby-text-container" => Some(Display::Internal(N::RubyTextContainer)),
        _ => None,
    };
    if single.is_some() {
        return single;
    }
    // `<display-outside> || <display-inside>` and `<display-listitem>`.
    let mut outside = None;
    let mut inside = None;
    let mut list_item = false;
    let mut word = Some(first);
    while let Some(w) = word {
        match w.as_str() {
            "block" if outside.is_none() => outside = Some(O::Block),
            "inline" if outside.is_none() => outside = Some(O::Inline),
            "run-in" if outside.is_none() => outside = Some(O::RunIn),
            "flow" if inside.is_none() => inside = Some(I::Flow),
            "flow-root" if inside.is_none() => inside = Some(I::FlowRoot),
            "table" if inside.is_none() => inside = Some(I::Table),
            "flex" if inside.is_none() => inside = Some(I::Flex),
            "grid" if inside.is_none() => inside = Some(I::Grid),
            "ruby" if inside.is_none() => inside = Some(I::Ruby),
            "list-item" if !list_item => list_item = true,
            _ => return None,
        }
        word = c.attempt(|c| c.ident().map(str::to_ascii_lowercase));
    }
    if list_item && !matches!(inside, None | Some(I::Flow) | Some(I::FlowRoot)) {
        return None;
    }
    let inside_v = inside.unwrap_or(I::Flow);
    let outside_v = outside.unwrap_or(if inside_v == I::Ruby {
        O::Inline
    } else {
        O::Block
    });
    Some(Display::Pair {
        outside: outside_v,
        inside: inside_v,
        list_item,
    })
}

keyword_enum! {
    /// `position` (CSS Position 3 §2).
    Position {
        Static = "static",
        Relative = "relative",
        Absolute = "absolute",
        Fixed = "fixed",
        Sticky = "sticky",
    }
}

keyword_enum! {
    /// `float` (CSS 2.1 §9.5.1, CSS Logical 1).
    Float {
        None = "none",
        Left = "left",
        Right = "right",
        InlineStart = "inline-start",
        InlineEnd = "inline-end",
    }
}

keyword_enum! {
    /// `clear` (CSS 2.1 §9.5.2, CSS Logical 1).
    Clear {
        None = "none",
        Left = "left",
        Right = "right",
        Both = "both",
        InlineStart = "inline-start",
        InlineEnd = "inline-end",
    }
}

/// `z-index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ZIndex {
    /// `auto`
    Auto,
    /// An integer.
    Integer(i32),
}

pub(crate) fn z_index(c: &mut Cursor<'_>) -> Option<ZIndex> {
    if c.eat_ident("auto") {
        return Some(ZIndex::Auto);
    }
    integer(c).map(ZIndex::Integer)
}

keyword_enum! {
    /// `overflow-x` / `overflow-y` (CSS Overflow 3 §3).
    Overflow {
        Visible = "visible",
        Hidden = "hidden",
        Clip = "clip",
        Scroll = "scroll",
        Auto = "auto",
    }
}

pub(crate) fn overflow(c: &mut Cursor<'_>) -> Option<Overflow> {
    // `overlay` is a legacy alias of `auto` (CSS Overflow 3 §3).
    if c.eat_ident("overlay") {
        return Some(Overflow::Auto);
    }
    Overflow::parse(c)
}

keyword_enum! {
    /// `visibility` (CSS Display 3 §4).
    Visibility {
        Visible = "visible",
        Hidden = "hidden",
        Collapse = "collapse",
    }
}

/// `<alpha-value>` for `opacity`: a number or percentage, as a fraction.
/// Out-of-range values are kept; clamp at computed-value time.
pub(crate) fn alpha_value(c: &mut Cursor<'_>) -> Option<f32> {
    if let Some(p) = parse_percentage(c, Range::All) {
        return Some(p / 100.0);
    }
    parse_number(c, Range::All)
}

// ---------------------------------------------------------------------------
// Backgrounds (CSS Backgrounds 3 §2)
// ---------------------------------------------------------------------------

keyword_enum! {
    /// One axis of `<repeat-style>`.
    RepeatStyle {
        Repeat = "repeat",
        Space = "space",
        Round = "round",
        NoRepeat = "no-repeat",
    }
}

/// `<repeat-style>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BackgroundRepeat {
    /// Horizontal repetition.
    pub x: RepeatStyle,
    /// Vertical repetition.
    pub y: RepeatStyle,
}

pub(crate) fn background_repeat(c: &mut Cursor<'_>) -> Option<BackgroundRepeat> {
    if c.eat_ident("repeat-x") {
        return Some(BackgroundRepeat {
            x: RepeatStyle::Repeat,
            y: RepeatStyle::NoRepeat,
        });
    }
    if c.eat_ident("repeat-y") {
        return Some(BackgroundRepeat {
            x: RepeatStyle::NoRepeat,
            y: RepeatStyle::Repeat,
        });
    }
    let x = RepeatStyle::parse(c)?;
    let y = RepeatStyle::parse(c).unwrap_or(x);
    Some(BackgroundRepeat { x, y })
}

keyword_enum! {
    /// `background-attachment`.
    BackgroundAttachment {
        Scroll = "scroll",
        Fixed = "fixed",
        Local = "local",
    }
}

keyword_enum! {
    /// `<visual-box>` for `background-origin` / `background-clip`, plus `text`
    /// for `background-clip`.
    BackgroundBox {
        BorderBox = "border-box",
        PaddingBox = "padding-box",
        ContentBox = "content-box",
        Text = "text",
    }
}

pub(crate) fn visual_box(c: &mut Cursor<'_>) -> Option<BackgroundBox> {
    c.attempt(|c| BackgroundBox::parse(c).filter(|b| *b != BackgroundBox::Text))
}

pub(crate) fn clip_box(c: &mut Cursor<'_>) -> Option<BackgroundBox> {
    if c.eat_ident("-webkit-text") {
        return Some(BackgroundBox::Text);
    }
    BackgroundBox::parse(c)
}

/// Which edge a [`PositionComponent`] offset is measured from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PositionEdge {
    /// Left (x) or top (y).
    Start,
    /// Right (x) or bottom (y).
    End,
}

/// One axis of a `<position>`: an offset from an edge. `center` is
/// `Start` + 50%.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionComponent {
    /// The edge the offset is measured from.
    pub edge: PositionEdge,
    /// The offset.
    pub offset: LengthPercentage,
}

impl PositionComponent {
    fn start(offset: LengthPercentage) -> Self {
        PositionComponent {
            edge: PositionEdge::Start,
            offset,
        }
    }
    fn center() -> Self {
        Self::start(LengthPercentage::Percentage(50.0))
    }
}

/// A `<bg-position>` (CSS Backgrounds 3 §2.6).
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundPosition {
    /// Horizontal position.
    pub x: PositionComponent,
    /// Vertical position.
    pub y: PositionComponent,
}

#[derive(Clone, Debug)]
enum PosItem {
    Left,
    Right,
    Top,
    Bottom,
    Center,
    Offset(LengthPercentage),
}

fn pos_item(c: &mut Cursor<'_>) -> Option<PosItem> {
    if let Some(l) = lp(c) {
        return Some(PosItem::Offset(l));
    }
    c.attempt(|c| {
        let i = c.ident()?.to_ascii_lowercase();
        Some(match i.as_str() {
            "left" => PosItem::Left,
            "right" => PosItem::Right,
            "top" => PosItem::Top,
            "bottom" => PosItem::Bottom,
            "center" => PosItem::Center,
            _ => return None,
        })
    })
}

fn interpret_position(items: &[PosItem]) -> Option<BackgroundPosition> {
    use PosItem as P;
    let horiz = |p: &P| -> Option<PositionComponent> {
        Some(match p {
            P::Left => PositionComponent::start(LengthPercentage::Percentage(0.0)),
            P::Right => PositionComponent::start(LengthPercentage::Percentage(100.0)),
            P::Center => PositionComponent::center(),
            P::Offset(l) => PositionComponent::start(l.clone()),
            _ => return None,
        })
    };
    let vert = |p: &P| -> Option<PositionComponent> {
        Some(match p {
            P::Top => PositionComponent::start(LengthPercentage::Percentage(0.0)),
            P::Bottom => PositionComponent::start(LengthPercentage::Percentage(100.0)),
            P::Center => PositionComponent::center(),
            P::Offset(l) => PositionComponent::start(l.clone()),
            _ => return None,
        })
    };
    match items {
        [one] => match one {
            P::Top | P::Bottom => Some(BackgroundPosition {
                x: PositionComponent::center(),
                y: vert(one)?,
            }),
            _ => Some(BackgroundPosition {
                x: horiz(one)?,
                y: PositionComponent::center(),
            }),
        },
        [a, b] => {
            if let (Some(x), Some(y)) = (horiz(a), vert(b)) {
                return Some(BackgroundPosition { x, y });
            }
            // Keywords may come in either order (`top left`).
            let kw = |p: &P| !matches!(p, P::Offset(_));
            if kw(a) && kw(b) {
                return Some(BackgroundPosition {
                    x: horiz(b)?,
                    y: vert(a)?,
                });
            }
            None
        }
        _ => {
            // 3 or 4 values: two `keyword offset?` groups, `center` taking no
            // offset.
            let mut groups: Vec<(&P, Option<&LengthPercentage>)> = Vec::new();
            let mut i = 0;
            while let Some(item) = items.get(i) {
                if matches!(item, P::Offset(_)) {
                    return None;
                }
                let off = match items.get(i + 1) {
                    Some(P::Offset(l)) if !matches!(item, P::Center) => Some(l),
                    _ => None,
                };
                groups.push((item, off));
                i += if off.is_some() { 2 } else { 1 };
            }
            let [(k1, o1), (k2, o2)] = groups.as_slice() else {
                return None;
            };
            let is_v = |p: &P| matches!(p, P::Top | P::Bottom);
            let is_h = |p: &P| matches!(p, P::Left | P::Right);
            let ((hk, ho), (vk, vo)) = if is_h(k1) || is_v(k2) {
                ((*k1, *o1), (*k2, *o2))
            } else {
                ((*k2, *o2), (*k1, *o1))
            };
            if is_v(hk) || is_h(vk) {
                return None;
            }
            let comp = |k: &P, o: Option<&LengthPercentage>| -> PositionComponent {
                match (k, o) {
                    (P::Center, _) => PositionComponent::center(),
                    (P::Left | P::Top, Some(o)) => PositionComponent::start(o.clone()),
                    (P::Left | P::Top, None) => {
                        PositionComponent::start(LengthPercentage::Percentage(0.0))
                    }
                    (_, Some(o)) => PositionComponent {
                        edge: PositionEdge::End,
                        offset: o.clone(),
                    },
                    (_, None) => PositionComponent::start(LengthPercentage::Percentage(100.0)),
                }
            };
            Some(BackgroundPosition {
                x: comp(hk, ho),
                y: comp(vk, vo),
            })
        }
    }
}

pub(crate) fn background_position(c: &mut Cursor<'_>) -> Option<BackgroundPosition> {
    // Read up to four items, then use the longest prefix that forms a valid
    // position.
    let start = *c;
    let mut items = Vec::new();
    let mut ends = Vec::new();
    while items.len() < 4 {
        match pos_item(c) {
            Some(it) => {
                items.push(it);
                ends.push(*c);
            }
            None => break,
        }
    }
    for n in (1..=items.len()).rev() {
        if let (Some(slice), Some(end)) = (items.get(..n), ends.get(n - 1))
            && let Some(p) = interpret_position(slice)
        {
            *c = *end;
            return Some(p);
        }
    }
    *c = start;
    None
}

/// `<bg-size>` (CSS Backgrounds 3 §2.9).
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

pub(crate) fn background_size(c: &mut Cursor<'_>) -> Option<BackgroundSize> {
    if c.eat_ident("cover") {
        return Some(BackgroundSize::Cover);
    }
    if c.eat_ident("contain") {
        return Some(BackgroundSize::Contain);
    }
    let one = |c: &mut Cursor<'_>| -> Option<Option<LengthPercentage>> {
        if c.eat_ident("auto") {
            return Some(None);
        }
        lp_non_negative(c).map(Some)
    };
    let width = one(c)?;
    let height = c.attempt(one).unwrap_or(None);
    Some(BackgroundSize::Explicit { width, height })
}

/// Parses a comma-separated list of `item`.
pub(crate) fn comma_list<T>(
    c: &mut Cursor<'_>,
    mut item: impl FnMut(&mut Cursor<'_>) -> Option<T>,
) -> Option<Vec<T>> {
    let mut out = vec![item(c)?];
    while c.eat_comma() {
        out.push(item(c)?);
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Fonts (CSS Fonts 4)
// ---------------------------------------------------------------------------

keyword_enum! {
    /// Generic font families (CSS Fonts 4 §4.1.1).
    GenericFamily {
        Serif = "serif",
        SansSerif = "sans-serif",
        Monospace = "monospace",
        Cursive = "cursive",
        Fantasy = "fantasy",
        SystemUi = "system-ui",
        UiSerif = "ui-serif",
        UiSansSerif = "ui-sans-serif",
        UiMonospace = "ui-monospace",
        UiRounded = "ui-rounded",
        Math = "math",
        Emoji = "emoji",
        Fangsong = "fangsong",
    }
}

/// One entry of `font-family`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FontFamily {
    /// A family name (quoted, or unquoted identifiers joined by one space).
    Named(String),
    /// A generic family.
    Generic(GenericFamily),
}

pub(crate) fn font_family_list(c: &mut Cursor<'_>) -> Option<Vec<FontFamily>> {
    comma_list(c, font_family)
}

fn font_family(c: &mut Cursor<'_>) -> Option<FontFamily> {
    if let Some(s) = parse_string(c) {
        return Some(FontFamily::Named(s));
    }
    let first = c.ident()?;
    let mut words = vec![first];
    while let Some(w) = c.attempt(|c| c.ident()) {
        words.push(w);
    }
    if let [only] = words.as_slice() {
        if let Some(g) = GenericFamily::from_ident(only) {
            return Some(FontFamily::Generic(g));
        }
        // CSS-wide keywords and `default` are not valid family names.
        let lower = only.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "initial" | "inherit" | "unset" | "revert" | "revert-layer" | "default"
        ) {
            return None;
        }
    }
    Some(FontFamily::Named(words.join(" ")))
}

keyword_enum! {
    /// Absolute and relative `font-size` keywords (CSS Fonts 4 §2.5).
    FontSizeKeyword {
        XxSmall = "xx-small",
        XSmall = "x-small",
        Small = "small",
        Medium = "medium",
        Large = "large",
        XLarge = "x-large",
        XxLarge = "xx-large",
        XxxLarge = "xxx-large",
        Larger = "larger",
        Smaller = "smaller",
    }
}

/// `font-size`.
#[derive(Clone, Debug, PartialEq)]
pub enum FontSize {
    /// A keyword.
    Keyword(FontSizeKeyword),
    /// A non-negative length or percentage (of the parent's font size).
    LengthPercentage(LengthPercentage),
}

pub(crate) fn font_size(c: &mut Cursor<'_>) -> Option<FontSize> {
    if let Some(k) = FontSizeKeyword::parse(c) {
        return Some(FontSize::Keyword(k));
    }
    lp_non_negative(c).map(FontSize::LengthPercentage)
}

/// `font-weight` (CSS Fonts 4 §2.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontWeight {
    /// A number in 1..=1000 (`normal` is 400, `bold` is 700).
    Absolute(f32),
    /// `bolder`
    Bolder,
    /// `lighter`
    Lighter,
}

pub(crate) fn font_weight(c: &mut Cursor<'_>) -> Option<FontWeight> {
    if c.eat_ident("normal") {
        return Some(FontWeight::Absolute(400.0));
    }
    if c.eat_ident("bold") {
        return Some(FontWeight::Absolute(700.0));
    }
    if c.eat_ident("bolder") {
        return Some(FontWeight::Bolder);
    }
    if c.eat_ident("lighter") {
        return Some(FontWeight::Lighter);
    }
    c.attempt(|c| {
        let n = parse_number(c, Range::All)?;
        (1.0..=1000.0)
            .contains(&n)
            .then_some(FontWeight::Absolute(n))
    })
}

/// `font-style` (CSS Fonts 4 §2.4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontStyle {
    /// `normal`
    Normal,
    /// `italic`
    Italic,
    /// `oblique <angle>?`, in degrees (default 14).
    Oblique(f32),
}

pub(crate) fn font_style(c: &mut Cursor<'_>) -> Option<FontStyle> {
    if c.eat_ident("normal") {
        return Some(FontStyle::Normal);
    }
    if c.eat_ident("italic") {
        return Some(FontStyle::Italic);
    }
    if c.eat_ident("oblique") {
        let angle = c
            .attempt(|c| parse_angle(c, true).filter(|a| (-90.0..=90.0).contains(a)))
            .unwrap_or(14.0);
        return Some(FontStyle::Oblique(angle));
    }
    None
}

/// `font-stretch` / `font-width` keywords as percentages (CSS Fonts 4 §2.3).
pub(crate) fn font_stretch_keyword(c: &mut Cursor<'_>) -> Option<f32> {
    c.attempt(|c| {
        let i = c.ident()?.to_ascii_lowercase();
        Some(match i.as_str() {
            "ultra-condensed" => 50.0,
            "extra-condensed" => 62.5,
            "condensed" => 75.0,
            "semi-condensed" => 87.5,
            "normal" => 100.0,
            "semi-expanded" => 112.5,
            "expanded" => 125.0,
            "extra-expanded" => 150.0,
            "ultra-expanded" => 200.0,
            _ => return None,
        })
    })
}

/// `font-stretch`, as a percentage.
pub(crate) fn font_stretch(c: &mut Cursor<'_>) -> Option<f32> {
    font_stretch_keyword(c).or_else(|| parse_percentage(c, Range::NonNegative))
}

/// `line-height` (CSS Inline 3 §5.1 / CSS 2.1 §10.8.1).
#[derive(Clone, Debug, PartialEq)]
pub enum LineHeight {
    /// `normal`
    Normal,
    /// A non-negative number (multiplies the font size; inherited as the number).
    Number(f32),
    /// A non-negative length or percentage.
    LengthPercentage(LengthPercentage),
}

pub(crate) fn line_height(c: &mut Cursor<'_>) -> Option<LineHeight> {
    if c.eat_ident("normal") {
        return Some(LineHeight::Normal);
    }
    if let Some(n) = non_negative_number(c) {
        return Some(LineHeight::Number(n));
    }
    lp_non_negative(c).map(LineHeight::LengthPercentage)
}

// ---------------------------------------------------------------------------
// Text (CSS Text 3/4, CSS Text Decoration 3, CSS 2.1 §10.8)
// ---------------------------------------------------------------------------

keyword_enum! {
    /// `text-align` (CSS Text 3 §6.1).
    TextAlign {
        Start = "start",
        End = "end",
        Left = "left",
        Right = "right",
        Center = "center",
        Justify = "justify",
        MatchParent = "match-parent",
    }
}

pub(crate) fn text_align(c: &mut Cursor<'_>) -> Option<TextAlign> {
    // Legacy vendor values used by HTML's `align` attribute mapping.
    let legacy = c.attempt(|c| {
        let i = c.ident()?.to_ascii_lowercase();
        match i.as_str() {
            "-webkit-center" | "-moz-center" => Some(TextAlign::Center),
            "-webkit-left" | "-moz-left" => Some(TextAlign::Left),
            "-webkit-right" | "-moz-right" => Some(TextAlign::Right),
            _ => None,
        }
    });
    legacy.or_else(|| TextAlign::parse(c))
}

/// `text-decoration-line` (CSS Text Decoration 3 §2.1). All false is `none`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextDecorationLine {
    /// `underline`
    pub underline: bool,
    /// `overline`
    pub overline: bool,
    /// `line-through`
    pub line_through: bool,
    /// `blink`
    pub blink: bool,
}

pub(crate) fn text_decoration_line(c: &mut Cursor<'_>) -> Option<TextDecorationLine> {
    if c.eat_ident("none") {
        return Some(TextDecorationLine::default());
    }
    let mut v = TextDecorationLine::default();
    let mut any = false;
    loop {
        let ok = c.attempt(|c| {
            let i = c.ident()?.to_ascii_lowercase();
            let slot = match i.as_str() {
                "underline" => &mut v.underline,
                "overline" => &mut v.overline,
                "line-through" => &mut v.line_through,
                "blink" => &mut v.blink,
                _ => return None,
            };
            if *slot {
                return None;
            }
            *slot = true;
            Some(())
        });
        if ok.is_none() {
            break;
        }
        any = true;
    }
    any.then_some(v)
}

keyword_enum! {
    /// `text-decoration-style`.
    TextDecorationStyle {
        Solid = "solid",
        Double = "double",
        Dotted = "dotted",
        Dashed = "dashed",
        Wavy = "wavy",
    }
}

keyword_enum! {
    /// `text-transform` (CSS Text 3 §2.1). `full-width` and
    /// `full-size-kana` are not supported yet.
    TextTransform {
        None = "none",
        Capitalize = "capitalize",
        Uppercase = "uppercase",
        Lowercase = "lowercase",
    }
}

keyword_enum! {
    /// `white-space` (CSS Text 3 §3).
    WhiteSpace {
        Normal = "normal",
        Pre = "pre",
        Nowrap = "nowrap",
        PreWrap = "pre-wrap",
        PreLine = "pre-line",
        BreakSpaces = "break-spaces",
    }
}

keyword_enum! {
    /// `word-break` (CSS Text 3 §5.2).
    WordBreak {
        Normal = "normal",
        BreakAll = "break-all",
        KeepAll = "keep-all",
        BreakWord = "break-word",
    }
}

keyword_enum! {
    /// `overflow-wrap` / `word-wrap` (CSS Text 3 §5.5).
    OverflowWrap {
        Normal = "normal",
        Anywhere = "anywhere",
        BreakWord = "break-word",
    }
}

/// `letter-spacing` / `word-spacing` (CSS Text 4 §8).
#[derive(Clone, Debug, PartialEq)]
pub enum Spacing {
    /// `normal`
    Normal,
    /// A length or percentage.
    LengthPercentage(LengthPercentage),
}

pub(crate) fn spacing(c: &mut Cursor<'_>) -> Option<Spacing> {
    if c.eat_ident("normal") {
        return Some(Spacing::Normal);
    }
    lp(c).map(Spacing::LengthPercentage)
}

keyword_enum! {
    /// `vertical-align` keywords (CSS 2.1 §10.8.1).
    VerticalAlignKeyword {
        Baseline = "baseline",
        Sub = "sub",
        Super = "super",
        TextTop = "text-top",
        TextBottom = "text-bottom",
        Middle = "middle",
        Top = "top",
        Bottom = "bottom",
    }
}

/// `vertical-align`.
#[derive(Clone, Debug, PartialEq)]
pub enum VerticalAlign {
    /// A keyword.
    Keyword(VerticalAlignKeyword),
    /// A length or percentage (of `line-height`).
    LengthPercentage(LengthPercentage),
}

pub(crate) fn vertical_align(c: &mut Cursor<'_>) -> Option<VerticalAlign> {
    if let Some(k) = VerticalAlignKeyword::parse(c) {
        return Some(VerticalAlign::Keyword(k));
    }
    lp(c).map(VerticalAlign::LengthPercentage)
}

// ---------------------------------------------------------------------------
// Lists and generated content (CSS Lists 3, CSS Generated Content 3)
// ---------------------------------------------------------------------------

/// `list-style-type` (CSS Lists 3 §3.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ListStyleType {
    /// `none`
    None,
    /// A counter style name, ASCII-lowercased (e.g. `disc`, `decimal`,
    /// `lower-roman`). Unknown names fall back to `decimal` when used.
    Named(String),
    /// A string used as the marker.
    String(String),
}

pub(crate) fn list_style_type(c: &mut Cursor<'_>) -> Option<ListStyleType> {
    if c.eat_ident("none") {
        return Some(ListStyleType::None);
    }
    if let Some(s) = parse_string(c) {
        return Some(ListStyleType::String(s));
    }
    counter_style_name(c).map(ListStyleType::Named)
}

/// `<counter-style-name>`: a custom identifier, lowercased.
fn counter_style_name(c: &mut Cursor<'_>) -> Option<String> {
    c.attempt(|c| {
        let i = c.ident()?;
        let lower = i.to_ascii_lowercase();
        // `none` and CSS-wide keywords are excluded.
        (!matches!(
            lower.as_str(),
            "none" | "initial" | "inherit" | "unset" | "revert" | "revert-layer" | "default"
        ))
        .then_some(lower)
    })
}

keyword_enum! {
    /// `list-style-position`.
    ListStylePosition {
        Inside = "inside",
        Outside = "outside",
    }
}

/// One item of the `content` property (CSS Generated Content 3 §1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ContentItem {
    /// A string.
    String(String),
    /// `url(...)`, unresolved.
    Url(String),
    /// `attr(name)`: the attribute's value, or empty.
    Attr(String),
    /// `counter(name, style?)`.
    Counter {
        /// The counter name.
        name: String,
        /// The counter style (default `decimal`).
        style: ListStyleType,
    },
    /// `counters(name, separator, style?)`.
    Counters {
        /// The counter name.
        name: String,
        /// The separator string.
        separator: String,
        /// The counter style (default `decimal`).
        style: ListStyleType,
    },
    /// `open-quote`
    OpenQuote,
    /// `close-quote`
    CloseQuote,
    /// `no-open-quote`
    NoOpenQuote,
    /// `no-close-quote`
    NoCloseQuote,
}

/// The `content` property.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Content {
    /// `normal`
    Normal,
    /// `none`
    None,
    /// A list of items. Alternative text after `/` is dropped.
    Items(Vec<ContentItem>),
}

fn counter_args(args: &[ComponentValue], with_separator: bool) -> Option<ContentItem> {
    Cursor::parse_all(args, |c| {
        let name = c.ident()?.to_string();
        let separator = if with_separator {
            if !c.eat_comma() {
                return None;
            }
            Some(parse_string(c)?)
        } else {
            None
        };
        let style = if c.eat_comma() {
            list_style_type(c)?
        } else {
            ListStyleType::Named("decimal".into())
        };
        Some(match separator {
            Some(separator) => ContentItem::Counters {
                name,
                separator,
                style,
            },
            None => ContentItem::Counter { name, style },
        })
    })
}

fn content_item(c: &mut Cursor<'_>) -> Option<ContentItem> {
    if let Some(s) = parse_string(c) {
        return Some(ContentItem::String(s));
    }
    if let Some(u) = parse_url(c) {
        return Some(ContentItem::Url(u));
    }
    c.attempt(|c| match c.next()? {
        ComponentValue::Ident(i) => {
            let lower = i.to_ascii_lowercase();
            Some(match lower.as_str() {
                "open-quote" => ContentItem::OpenQuote,
                "close-quote" => ContentItem::CloseQuote,
                "no-open-quote" => ContentItem::NoOpenQuote,
                "no-close-quote" => ContentItem::NoCloseQuote,
                _ => return None,
            })
        }
        ComponentValue::Function { name, arguments } => {
            let lower = name.to_ascii_lowercase();
            match lower.as_str() {
                // M1 follow-up: attr() type and fallback arguments (CSS Values 5).
                "attr" => Cursor::parse_all(arguments, |c| c.ident().map(str::to_string))
                    .map(ContentItem::Attr),
                "counter" => counter_args(arguments, false),
                "counters" => counter_args(arguments, true),
                _ => None,
            }
        }
        _ => None,
    })
}

pub(crate) fn content(c: &mut Cursor<'_>) -> Option<Content> {
    if c.eat_ident("normal") {
        return Some(Content::Normal);
    }
    if c.eat_ident("none") {
        return Some(Content::None);
    }
    let mut items = vec![content_item(c)?];
    while let Some(i) = c.attempt(content_item) {
        items.push(i);
    }
    // `/ <alt text>`: accepted and dropped.
    if c.eat_delim('/') {
        let mut any = false;
        while c.attempt(|c| parse_string(c).map(|_| ())).is_some()
            || c.attempt(content_item).is_some()
        {
            any = true;
        }
        if !any {
            return None;
        }
    }
    Some(Content::Items(items))
}

/// `counter-reset` / `counter-increment`: `(name, value)` pairs; empty for
/// `none` (CSS Lists 3 §4).
pub(crate) fn counter_list(c: &mut Cursor<'_>, default: i32) -> Option<Vec<(String, i32)>> {
    if c.eat_ident("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    while let Some(name) = c.attempt(|c| {
        let n = c.ident()?;
        let lower = n.to_ascii_lowercase();
        (!matches!(
            lower.as_str(),
            "none" | "initial" | "inherit" | "unset" | "revert" | "revert-layer" | "default"
        ))
        .then(|| n.to_string())
    }) {
        let value = c.attempt(integer).unwrap_or(default);
        out.push((name, value));
    }
    (!out.is_empty()).then_some(out)
}

// ---------------------------------------------------------------------------
// Tables (CSS 2.1 §17)
// ---------------------------------------------------------------------------

keyword_enum! {
    /// `border-collapse`.
    BorderCollapse {
        Separate = "separate",
        Collapse = "collapse",
    }
}

/// `border-spacing`: horizontal and vertical lengths.
#[derive(Clone, Debug, PartialEq)]
pub struct BorderSpacing {
    /// Horizontal spacing.
    pub horizontal: LengthPercentage,
    /// Vertical spacing.
    pub vertical: LengthPercentage,
}

pub(crate) fn border_spacing(c: &mut Cursor<'_>) -> Option<BorderSpacing> {
    let horizontal = length_non_negative(c)?;
    let vertical = c
        .attempt(length_non_negative)
        .unwrap_or_else(|| horizontal.clone());
    Some(BorderSpacing {
        horizontal,
        vertical,
    })
}

keyword_enum! {
    /// `table-layout`.
    TableLayout {
        Auto = "auto",
        Fixed = "fixed",
    }
}

keyword_enum! {
    /// `caption-side`.
    CaptionSide {
        Top = "top",
        Bottom = "bottom",
    }
}

keyword_enum! {
    /// `empty-cells`.
    EmptyCells {
        Show = "show",
        Hide = "hide",
    }
}

// ---------------------------------------------------------------------------
// Flexbox and alignment (CSS Flexbox 1, CSS Box Alignment 3)
// ---------------------------------------------------------------------------

keyword_enum! {
    /// `flex-direction`.
    FlexDirection {
        Row = "row",
        RowReverse = "row-reverse",
        Column = "column",
        ColumnReverse = "column-reverse",
    }
}

keyword_enum! {
    /// `flex-wrap`.
    FlexWrap {
        Nowrap = "nowrap",
        Wrap = "wrap",
        WrapReverse = "wrap-reverse",
    }
}

/// `flex-basis` (CSS Flexbox 1 §7.3.3).
#[derive(Clone, Debug, PartialEq)]
pub enum FlexBasis {
    /// `content`
    Content,
    /// A `width`-like value (including `auto`).
    Size(Size),
}

pub(crate) fn flex_basis(c: &mut Cursor<'_>) -> Option<FlexBasis> {
    if c.eat_ident("content") {
        return Some(FlexBasis::Content);
    }
    size(c).map(FlexBasis::Size)
}

keyword_enum! {
    /// Alignment keywords for `justify-content`, `align-items`,
    /// `align-self` and `align-content` (CSS Box Alignment 3 §4). Each
    /// property accepts a subset. `safe`/`unsafe` prefixes are accepted
    /// and dropped, and `first baseline` is `Baseline`.
    AlignKeyword {
        Auto = "auto",
        Normal = "normal",
        Stretch = "stretch",
        Baseline = "baseline",
        LastBaseline = "last baseline",
        Center = "center",
        Start = "start",
        End = "end",
        FlexStart = "flex-start",
        FlexEnd = "flex-end",
        SelfStart = "self-start",
        SelfEnd = "self-end",
        Left = "left",
        Right = "right",
        SpaceBetween = "space-between",
        SpaceAround = "space-around",
        SpaceEvenly = "space-evenly",
    }
}

/// Which alignment property is being parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlignProp {
    JustifyContent,
    AlignContent,
    AlignItems,
    AlignSelf,
}

pub(crate) fn alignment(c: &mut Cursor<'_>, prop: AlignProp) -> Option<AlignKeyword> {
    use AlignKeyword as K;
    // Baselines: `baseline`, `first baseline`, `last baseline`.
    let baseline = c.attempt(|c| {
        if c.eat_ident("first") {
            c.eat_ident("baseline").then_some(K::Baseline)
        } else if c.eat_ident("last") {
            c.eat_ident("baseline").then_some(K::LastBaseline)
        } else {
            None
        }
    });
    let kw = match baseline {
        Some(k) => k,
        None => {
            let had_safety = c.eat_ident("safe") || c.eat_ident("unsafe");
            let k = c.attempt(|c| {
                let i = c.ident()?;
                if i.eq_ignore_ascii_case("last baseline") {
                    return None;
                }
                K::from_ident(i)
            })?;
            // Only positional keywords take `safe`/`unsafe`.
            let positional = matches!(
                k,
                K::Center
                    | K::Start
                    | K::End
                    | K::FlexStart
                    | K::FlexEnd
                    | K::SelfStart
                    | K::SelfEnd
                    | K::Left
                    | K::Right
            );
            if had_safety && !positional {
                return None;
            }
            k
        }
    };
    let ok = match prop {
        AlignProp::JustifyContent => matches!(
            kw,
            K::Normal
                | K::Stretch
                | K::Center
                | K::Start
                | K::End
                | K::FlexStart
                | K::FlexEnd
                | K::Left
                | K::Right
                | K::SpaceBetween
                | K::SpaceAround
                | K::SpaceEvenly
        ),
        AlignProp::AlignContent => matches!(
            kw,
            K::Normal
                | K::Stretch
                | K::Baseline
                | K::LastBaseline
                | K::Center
                | K::Start
                | K::End
                | K::FlexStart
                | K::FlexEnd
                | K::SpaceBetween
                | K::SpaceAround
                | K::SpaceEvenly
        ),
        AlignProp::AlignItems => matches!(
            kw,
            K::Normal
                | K::Stretch
                | K::Baseline
                | K::LastBaseline
                | K::Center
                | K::Start
                | K::End
                | K::FlexStart
                | K::FlexEnd
                | K::SelfStart
                | K::SelfEnd
        ),
        AlignProp::AlignSelf => matches!(
            kw,
            K::Auto
                | K::Normal
                | K::Stretch
                | K::Baseline
                | K::LastBaseline
                | K::Center
                | K::Start
                | K::End
                | K::FlexStart
                | K::FlexEnd
                | K::SelfStart
                | K::SelfEnd
        ),
    };
    ok.then_some(kw)
}

/// `row-gap` / `column-gap` (CSS Box Alignment 3 §8).
#[derive(Clone, Debug, PartialEq)]
pub enum Gap {
    /// `normal`
    Normal,
    /// A non-negative length or percentage.
    LengthPercentage(LengthPercentage),
}

pub(crate) fn gap(c: &mut Cursor<'_>) -> Option<Gap> {
    if c.eat_ident("normal") {
        return Some(Gap::Normal);
    }
    lp_non_negative(c).map(Gap::LengthPercentage)
}

// ---------------------------------------------------------------------------
// UI (CSS UI 4)
// ---------------------------------------------------------------------------

keyword_enum! {
    /// `cursor` keywords (CSS UI 4 §5.1).
    CursorKeyword {
        Auto = "auto",
        Default = "default",
        None = "none",
        ContextMenu = "context-menu",
        Help = "help",
        Pointer = "pointer",
        Progress = "progress",
        Wait = "wait",
        Cell = "cell",
        Crosshair = "crosshair",
        Text = "text",
        VerticalText = "vertical-text",
        Alias = "alias",
        Copy = "copy",
        Move = "move",
        NoDrop = "no-drop",
        NotAllowed = "not-allowed",
        Grab = "grab",
        Grabbing = "grabbing",
        EResize = "e-resize",
        NResize = "n-resize",
        NeResize = "ne-resize",
        NwResize = "nw-resize",
        SResize = "s-resize",
        SeResize = "se-resize",
        SwResize = "sw-resize",
        WResize = "w-resize",
        EwResize = "ew-resize",
        NsResize = "ns-resize",
        NeswResize = "nesw-resize",
        NwseResize = "nwse-resize",
        ColResize = "col-resize",
        RowResize = "row-resize",
        AllScroll = "all-scroll",
        ZoomIn = "zoom-in",
        ZoomOut = "zoom-out",
    }
}

/// A cursor image with an optional hotspot.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorImage {
    /// The image URL, unresolved.
    pub url: String,
    /// The hotspot `(x, y)`, if given.
    pub hotspot: Option<(f32, f32)>,
}

/// The `cursor` property.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorValue {
    /// Images to try in order.
    pub images: Vec<CursorImage>,
    /// The fallback keyword.
    pub keyword: CursorKeyword,
}

pub(crate) fn cursor(c: &mut Cursor<'_>) -> Option<CursorValue> {
    let mut images = Vec::new();
    while let Some(img) = c.attempt(|c| {
        let url = parse_url(c)?;
        let hotspot = c.attempt(|c| {
            let x = parse_number(c, Range::All)?;
            let y = parse_number(c, Range::All)?;
            Some((x, y))
        });
        c.eat_comma().then_some(CursorImage { url, hotspot })
    }) {
        images.push(img);
    }
    let keyword = CursorKeyword::parse(c)?;
    Some(CursorValue { images, keyword })
}
