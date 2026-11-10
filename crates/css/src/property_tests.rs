//! Tests for longhand and shorthand value parsing.

use crate::color::{Color, Rgba};
use crate::properties::{Longhand as L, PropertyId, parse_property_value};
use crate::syntax::parse_component_values;
use crate::types::*;
use crate::values::{Image, Length, LengthPercentage as LP, LengthUnit};

fn p(name: &str, value: &str) -> Option<Vec<L>> {
    let id = PropertyId::from_name(name).unwrap_or_else(|| panic!("{name}"));
    parse_property_value(id, &parse_component_values(value))
}

fn one(name: &str, value: &str) -> L {
    let mut v = p(name, value).unwrap_or_else(|| panic!("{name}: {value}"));
    assert_eq!(v.len(), 1);
    v.remove(0)
}

fn px(v: f32) -> LP {
    LP::Length(Length::px(v))
}

fn invalid(name: &str, value: &str) {
    assert!(
        p(name, value).is_none(),
        "{name}: {value} should be invalid"
    );
}

#[test]
fn box_model() {
    assert_eq!(
        p("margin", "1px 2px").unwrap(),
        vec![
            L::MarginTop(LengthPercentageOrAuto::LengthPercentage(px(1.0))),
            L::MarginRight(LengthPercentageOrAuto::LengthPercentage(px(2.0))),
            L::MarginBottom(LengthPercentageOrAuto::LengthPercentage(px(1.0))),
            L::MarginLeft(LengthPercentageOrAuto::LengthPercentage(px(2.0))),
        ]
    );
    let v = p("padding", "1px 2px 3px").unwrap();
    assert_eq!(v[3], L::PaddingLeft(px(2.0)));
    invalid("padding", "-1px");
    invalid("margin", "1px 2px 3px 4px 5px");
    assert_eq!(
        one("margin-left", "-5%"),
        L::MarginLeft(LengthPercentageOrAuto::LengthPercentage(LP::Percentage(
            -5.0
        )))
    );
    assert_eq!(one("width", "auto"), L::Width(Size::Auto));
    assert_eq!(
        one("width", "fit-content(10px)"),
        L::Width(Size::FitContent(Some(px(10.0))))
    );
    assert_eq!(one("max-height", "none"), L::MaxHeight(MaxSize::None));
    invalid("max-width", "auto");
    invalid("width", "-1px");
    assert_eq!(
        one("box-sizing", "Border-Box"),
        L::BoxSizing(BoxSizing::BorderBox)
    );
    assert_eq!(one("border-top-width", "thick"), L::BorderTopWidth(px(5.0)));
    invalid("border-top-width", "10%");
    let b = p("border", "1px solid red").unwrap();
    assert_eq!(b.len(), 12);
    assert!(b.contains(&L::BorderLeftStyle(LineStyle::Solid)));
    assert!(b.contains(&L::BorderBottomColor(Color::Rgba(Rgba::rgb(255, 0, 0)))));
    let b = p("border-top", "dashed").unwrap();
    assert_eq!(b[0], L::BorderTopWidth(px(3.0)));
    assert_eq!(b[2], L::BorderTopColor(Color::CurrentColor));
    invalid("border", "solid solid");
    let s = p("border-style", "solid none").unwrap();
    assert_eq!(s[1], L::BorderRightStyle(LineStyle::None));
    let c = p("border-color", "red green blue").unwrap();
    assert_eq!(c[3], L::BorderLeftColor(Color::Rgba(Rgba::rgb(0, 128, 0))));
    let r = p("border-radius", "10px 5% / 20px").unwrap();
    assert_eq!(
        r[1],
        L::BorderTopRightRadius(CornerRadius {
            horizontal: LP::Percentage(5.0),
            vertical: px(20.0)
        })
    );
    assert_eq!(
        one("border-top-left-radius", "3px 4px"),
        L::BorderTopLeftRadius(CornerRadius {
            horizontal: px(3.0),
            vertical: px(4.0)
        })
    );
}

#[test]
fn display_and_position() {
    assert_eq!(one("display", "block"), L::Display(Display::BLOCK));
    assert_eq!(
        one("display", "inline-block"),
        L::Display(Display::Pair {
            outside: DisplayOutside::Inline,
            inside: DisplayInside::FlowRoot,
            list_item: false
        })
    );
    assert_eq!(
        one("display", "list-item"),
        L::Display(Display::Pair {
            outside: DisplayOutside::Block,
            inside: DisplayInside::Flow,
            list_item: true
        })
    );
    assert_eq!(
        one("display", "inline flex"),
        L::Display(Display::Pair {
            outside: DisplayOutside::Inline,
            inside: DisplayInside::Flex,
            list_item: false
        })
    );
    assert_eq!(
        one("display", "table-cell"),
        L::Display(Display::Internal(DisplayInternal::TableCell))
    );
    assert_eq!(one("display", "none"), L::Display(Display::None));
    invalid("display", "block inline");
    invalid("display", "flex list-item");
    assert_eq!(one("position", "sticky"), L::Position(Position::Sticky));
    assert_eq!(
        p("inset", "0 auto").unwrap()[1],
        L::Right(LengthPercentageOrAuto::Auto)
    );
    assert_eq!(one("float", "left"), L::Float(Float::Left));
    assert_eq!(one("clear", "both"), L::Clear(Clear::Both));
    assert_eq!(one("z-index", "-3"), L::ZIndex(ZIndex::Integer(-3)));
    invalid("z-index", "1.5");
    assert_eq!(
        p("overflow", "hidden auto").unwrap(),
        vec![L::OverflowX(Overflow::Hidden), L::OverflowY(Overflow::Auto)]
    );
    assert_eq!(
        one("visibility", "collapse"),
        L::Visibility(Visibility::Collapse)
    );
    assert_eq!(one("opacity", "50%"), L::Opacity(0.5));
    assert_eq!(one("opacity", "1.5"), L::Opacity(1.5));
}

#[test]
fn backgrounds() {
    let v = p(
        "background",
        "url(a.png) no-repeat right 10px top / cover, #fff",
    )
    .unwrap();
    assert_eq!(
        v[0],
        L::BackgroundColor(Color::Rgba(Rgba::rgb(255, 255, 255)))
    );
    assert_eq!(
        v[1],
        L::BackgroundImage(vec![Image::Url("a.png".into()), Image::None])
    );
    let L::BackgroundPosition(pos) = &v[3] else {
        panic!()
    };
    assert_eq!(
        pos[0].x,
        PositionComponent {
            edge: PositionEdge::End,
            offset: px(10.0)
        }
    );
    assert_eq!(pos[0].y.offset, LP::Percentage(0.0));
    let L::BackgroundSize(sz) = &v[4] else {
        panic!()
    };
    assert_eq!(sz[0], BackgroundSize::Cover);
    invalid("background", "red, url(a.png)");
    let v = p("background", "none").unwrap();
    assert_eq!(v[0], L::BackgroundColor(Color::TRANSPARENT));
    let v = p("background", "padding-box content-box blue").unwrap();
    assert_eq!(v[6], L::BackgroundOrigin(vec![BackgroundBox::PaddingBox]));
    assert_eq!(v[7], L::BackgroundClip(vec![BackgroundBox::ContentBox]));
    let L::BackgroundPosition(pos) = one("background-position", "center bottom") else {
        panic!()
    };
    assert_eq!(pos[0].x.offset, LP::Percentage(50.0));
    assert_eq!(pos[0].y.offset, LP::Percentage(100.0));
    let L::BackgroundPosition(pos) = one("background-position", "top left, 10px 20%") else {
        panic!()
    };
    assert_eq!(pos[0].x.offset, LP::Percentage(0.0));
    assert_eq!(pos[1].y.offset, LP::Percentage(20.0));
    invalid("background-position", "left right");
    invalid("background-position", "50% left");
    assert_eq!(
        one("background-repeat", "repeat-x"),
        L::BackgroundRepeat(vec![BackgroundRepeat {
            x: RepeatStyle::Repeat,
            y: RepeatStyle::NoRepeat
        }])
    );
    assert_eq!(
        one("background-size", "auto 50%"),
        L::BackgroundSize(vec![BackgroundSize::Explicit {
            width: None,
            height: Some(LP::Percentage(50.0))
        }])
    );
    assert!(
        matches!(&one("background-image", "linear-gradient(red, blue)"), L::BackgroundImage(v) if matches!(v[0], Image::Gradient { .. }))
    );
}

#[test]
fn fonts() {
    let v = p(
        "font",
        "italic bold 12px/1.5 \"Helvetica Neue\", Arial, sans-serif",
    )
    .unwrap();
    assert_eq!(v[0], L::FontStyle(FontStyle::Italic));
    assert_eq!(v[1], L::FontWeight(FontWeight::Absolute(700.0)));
    assert_eq!(v[3], L::FontSize(FontSize::LengthPercentage(px(12.0))));
    assert_eq!(v[4], L::LineHeight(LineHeight::Number(1.5)));
    assert_eq!(
        v[5],
        L::FontFamily(vec![
            FontFamily::Named("Helvetica Neue".into()),
            FontFamily::Named("Arial".into()),
            FontFamily::Generic(GenericFamily::SansSerif)
        ])
    );
    let v = p("font", "small-caps 300 condensed small Times New Roman").unwrap();
    assert_eq!(v[2], L::FontStretch(75.0));
    assert_eq!(
        v[5],
        L::FontFamily(vec![FontFamily::Named("Times New Roman".into())])
    );
    invalid("font", "12px");
    invalid("font", "bold serif");
    assert_eq!(
        one("font-weight", "550"),
        L::FontWeight(FontWeight::Absolute(550.0))
    );
    invalid("font-weight", "0");
    assert_eq!(
        one("font-style", "oblique 10deg"),
        L::FontStyle(FontStyle::Oblique(10.0))
    );
    assert_eq!(
        one("font-size", "x-large"),
        L::FontSize(FontSize::Keyword(FontSizeKeyword::XLarge))
    );
    assert_eq!(one("font-stretch", "120%"), L::FontStretch(120.0));
    assert_eq!(
        one("line-height", "normal"),
        L::LineHeight(LineHeight::Normal)
    );
    invalid("font-family", "inherit, serif");
}

#[test]
fn text() {
    assert_eq!(one("text-align", "center"), L::TextAlign(TextAlign::Center));
    assert_eq!(
        one("text-align", "-webkit-center"),
        L::TextAlign(TextAlign::Center)
    );
    let v = p("text-decoration", "underline overline wavy red").unwrap();
    assert_eq!(
        v[0],
        L::TextDecorationLine(TextDecorationLine {
            underline: true,
            overline: true,
            ..Default::default()
        })
    );
    assert_eq!(v[1], L::TextDecorationStyle(TextDecorationStyle::Wavy));
    assert_eq!(
        p("text-decoration", "none").unwrap()[0],
        L::TextDecorationLine(TextDecorationLine::default())
    );
    invalid("text-decoration-line", "underline underline");
    assert_eq!(
        one("text-transform", "uppercase"),
        L::TextTransform(TextTransform::Uppercase)
    );
    assert_eq!(
        one("text-indent", "2em"),
        L::TextIndent(LP::Length(Length {
            value: 2.0,
            unit: LengthUnit::Em
        }))
    );
    assert_eq!(
        one("white-space", "pre-wrap"),
        L::WhiteSpace(WhiteSpace::PreWrap)
    );
    assert_eq!(
        one("word-break", "break-all"),
        L::WordBreak(WordBreak::BreakAll)
    );
    assert_eq!(
        one("word-wrap", "break-word"),
        L::OverflowWrap(OverflowWrap::BreakWord)
    );
    assert_eq!(
        one("letter-spacing", "normal"),
        L::LetterSpacing(Spacing::Normal)
    );
    assert_eq!(
        one("word-spacing", "-1px"),
        L::WordSpacing(Spacing::LengthPercentage(px(-1.0)))
    );
    assert_eq!(
        one("vertical-align", "middle"),
        L::VerticalAlign(VerticalAlign::Keyword(VerticalAlignKeyword::Middle))
    );
    assert_eq!(
        one("vertical-align", "10%"),
        L::VerticalAlign(VerticalAlign::LengthPercentage(LP::Percentage(10.0)))
    );
}

#[test]
fn lists_and_content() {
    let v = p("list-style", "none").unwrap();
    assert_eq!(v[0], L::ListStyleType(ListStyleType::None));
    assert_eq!(v[2], L::ListStyleImage(Image::None));
    let v = p("list-style", "square inside").unwrap();
    assert_eq!(
        v[0],
        L::ListStyleType(ListStyleType::Named("square".into()))
    );
    assert_eq!(v[1], L::ListStylePosition(ListStylePosition::Inside));
    let v = p("list-style", "none url(x.png)").unwrap();
    assert_eq!(v[0], L::ListStyleType(ListStyleType::None));
    assert_eq!(v[2], L::ListStyleImage(Image::Url("x.png".into())));
    invalid("list-style", "none none none");
    assert_eq!(
        one("list-style-type", "'-'"),
        L::ListStyleType(ListStyleType::String("-".into()))
    );
    assert_eq!(
        one(
            "content",
            "\"[\" attr(title) counter(item, lower-roman) counters(c, '.') open-quote / \"alt\""
        ),
        L::Content(Content::Items(vec![
            ContentItem::String("[".into()),
            ContentItem::Attr("title".into()),
            ContentItem::Counter {
                name: "item".into(),
                style: ListStyleType::Named("lower-roman".into())
            },
            ContentItem::Counters {
                name: "c".into(),
                separator: ".".into(),
                style: ListStyleType::Named("decimal".into())
            },
            ContentItem::OpenQuote,
        ]))
    );
    assert_eq!(one("content", "none"), L::Content(Content::None));
    invalid("content", "foo");
    assert_eq!(
        one("counter-reset", "a b 3"),
        L::CounterReset(vec![("a".into(), 0), ("b".into(), 3)])
    );
    assert_eq!(
        one("counter-increment", "a"),
        L::CounterIncrement(vec![("a".into(), 1)])
    );
    assert_eq!(
        one("counter-increment", "none"),
        L::CounterIncrement(vec![])
    );
}

#[test]
fn tables() {
    assert_eq!(
        one("border-collapse", "collapse"),
        L::BorderCollapse(BorderCollapse::Collapse)
    );
    assert_eq!(
        one("border-spacing", "2px"),
        L::BorderSpacing(BorderSpacing {
            horizontal: px(2.0),
            vertical: px(2.0)
        })
    );
    invalid("border-spacing", "10%");
    assert_eq!(
        one("table-layout", "fixed"),
        L::TableLayout(TableLayout::Fixed)
    );
    assert_eq!(
        one("caption-side", "bottom"),
        L::CaptionSide(CaptionSide::Bottom)
    );
    assert_eq!(one("empty-cells", "hide"), L::EmptyCells(EmptyCells::Hide));
}

#[test]
fn flexbox() {
    let zero_pct = FlexBasis::Size(Size::LengthPercentage(LP::Percentage(0.0)));
    let auto = FlexBasis::Size(Size::Auto);
    assert_eq!(
        p("flex", "1").unwrap(),
        vec![
            L::FlexGrow(1.0),
            L::FlexShrink(1.0),
            L::FlexBasis(zero_pct.clone())
        ]
    );
    assert_eq!(
        p("flex", "none").unwrap(),
        vec![
            L::FlexGrow(0.0),
            L::FlexShrink(0.0),
            L::FlexBasis(auto.clone())
        ]
    );
    assert_eq!(
        p("flex", "auto").unwrap(),
        vec![
            L::FlexGrow(1.0),
            L::FlexShrink(1.0),
            L::FlexBasis(auto.clone())
        ]
    );
    assert_eq!(
        p("flex", "2 3 10px").unwrap()[2],
        L::FlexBasis(FlexBasis::Size(Size::LengthPercentage(px(10.0))))
    );
    assert_eq!(
        p("flex", "0 0 0").unwrap()[2],
        L::FlexBasis(FlexBasis::Size(Size::LengthPercentage(px(0.0))))
    );
    assert_eq!(p("flex", "auto 2").unwrap()[0], L::FlexGrow(2.0));
    invalid("flex", "1 2 3 4");
    assert_eq!(
        p("flex-flow", "column wrap").unwrap(),
        vec![
            L::FlexDirection(FlexDirection::Column),
            L::FlexWrap(FlexWrap::Wrap)
        ]
    );
    assert_eq!(
        one("justify-content", "space-between"),
        L::JustifyContent(AlignKeyword::SpaceBetween)
    );
    assert_eq!(
        one("justify-content", "safe center"),
        L::JustifyContent(AlignKeyword::Center)
    );
    invalid("justify-content", "baseline");
    assert_eq!(
        one("align-items", "last baseline"),
        L::AlignItems(AlignKeyword::LastBaseline)
    );
    assert_eq!(one("align-self", "auto"), L::AlignSelf(AlignKeyword::Auto));
    invalid("align-items", "auto");
    assert_eq!(
        one("align-content", "space-around"),
        L::AlignContent(AlignKeyword::SpaceAround)
    );
    assert_eq!(one("order", "-1"), L::Order(-1));
    assert_eq!(
        p("gap", "1px 2%").unwrap(),
        vec![
            L::RowGap(Gap::LengthPercentage(px(1.0))),
            L::ColumnGap(Gap::LengthPercentage(LP::Percentage(2.0)))
        ]
    );
    assert_eq!(one("flex-shrink", "0"), L::FlexShrink(0.0));
    invalid("flex-grow", "-1");
}

#[test]
fn ui() {
    assert_eq!(
        one("cursor", "url(a.cur) 1 2, url(b.png), pointer"),
        L::Cursor(CursorValue {
            images: vec![
                CursorImage {
                    url: "a.cur".into(),
                    hotspot: Some((1.0, 2.0))
                },
                CursorImage {
                    url: "b.png".into(),
                    hotspot: None
                }
            ],
            keyword: CursorKeyword::Pointer
        })
    );
    invalid("cursor", "url(a.cur)");
    assert_eq!(
        one("color", "hsl(0 100% 50%)"),
        L::Color(Color::Rgba(Rgba::rgb(255, 0, 0)))
    );
}

#[test]
fn calc_in_properties() {
    assert!(matches!(
        one("width", "calc(100% - 10px)"),
        L::Width(Size::LengthPercentage(LP::Calc(_)))
    ));
    assert_eq!(one("z-index", "calc(2 * 3)"), L::ZIndex(ZIndex::Integer(6)));
    assert_eq!(one("opacity", "calc(1 / 2)"), L::Opacity(0.5));
    invalid("width", "calc(10)");
}

#[test]
fn every_longhand_name_round_trips() {
    for id in crate::properties::LonghandId::ALL {
        assert_eq!(
            PropertyId::from_name(id.name()),
            Some(PropertyId::Longhand(*id))
        );
    }
}
