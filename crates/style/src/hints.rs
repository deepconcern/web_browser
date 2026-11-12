//! Presentational hints (HTML §15.3 "non-CSS presentational hints").
//!
//! Attributes such as `bgcolor`, `width` and `align`, and the `<font>`
//! element, map to CSS declarations. They are author-level declarations
//! with zero specificity that lose to every author rule (CSS Cascade 5
//! §6.4.4); the cascade puts them below all author style rules.

use wb_css::{
    BorderSpacing, CaptionSide, Clear, Color, Declaration, DeclarationKind, Float, FontSize,
    FontSizeKeyword, Image, Length, LengthPercentageOrAuto, LineStyle, ListStyleType, Longhand,
    LonghandId, PropertyId, Rgba, Size, TextAlign, VerticalAlign, VerticalAlignKeyword, WhiteSpace,
    parse_color_str, parse_component_values, parse_property_value,
};
use wb_dom::{Document, ElementData, Namespace, NodeId};

type Lp = wb_css::LengthPercentage;

/// A parsed dimension value (HTML §2.3.4.4 "rules for parsing dimension
/// values").
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Dimension {
    Length(f32),
    Percentage(f32),
}

/// HTML §2.3.4.4.
pub(crate) fn parse_dimension(input: &str) -> Option<Dimension> {
    let s = input.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut value: f64 = 0.0;
    while let Some(&b) = bytes.get(i) {
        if !b.is_ascii_digit() {
            break;
        }
        value = (value * 10.0 + f64::from(b - b'0')).min(1e9);
        i += 1;
    }
    if i == 0 {
        return None;
    }
    if bytes.get(i) == Some(&b'.') {
        let mut divisor = 1.0f64;
        let mut j = i + 1;
        while let Some(&b) = bytes.get(j) {
            if !b.is_ascii_digit() {
                break;
            }
            divisor *= 10.0;
            value += f64::from(b - b'0') / divisor;
            j += 1;
        }
        i = j;
    }
    #[allow(clippy::cast_possible_truncation)]
    let v = value as f32;
    if bytes.get(i) == Some(&b'%') {
        Some(Dimension::Percentage(v))
    } else {
        Some(Dimension::Length(v))
    }
}

/// HTML §2.3.4.5 "rules for parsing non-zero dimension values".
fn parse_nonzero_dimension(input: &str) -> Option<Dimension> {
    match parse_dimension(input)? {
        Dimension::Length(v) | Dimension::Percentage(v) if v == 0.0 => None,
        d => Some(d),
    }
}

/// HTML §2.3.4.1 "rules for parsing non-negative integers", saturating.
pub(crate) fn parse_non_negative_integer(input: &str) -> Option<i32> {
    let s = input.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let s = s.strip_prefix('+').unwrap_or(s);
    let mut value: i32 = 0;
    let mut any = false;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            break;
        }
        any = true;
        value = value.saturating_mul(10).saturating_add(i32::from(b - b'0'));
    }
    any.then_some(value)
}

/// HTML §2.3.6 "rules for parsing a legacy color value".
pub(crate) fn parse_legacy_color(input: &str) -> Option<Rgba> {
    if input.is_empty() {
        return None;
    }
    let input = input.trim_matches(|c: char| c.is_ascii_whitespace());
    if input.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if input.bytes().all(|b| b.is_ascii_alphabetic())
        && let Some(Color::Rgba(c)) = parse_color_str(input)
    {
        return Some(c);
    }
    let chars: Vec<char> = input.chars().collect();
    if let ['#', r, g, b] = chars.as_slice()
        && let (Some(r), Some(g), Some(b)) = (r.to_digit(16), g.to_digit(16), b.to_digit(16))
    {
        let x = |v: u32| u8::try_from(v * 17).unwrap_or(255);
        return Some(Rgba::rgb(x(r), x(g), x(b)));
    }
    // Code points above U+FFFF become "00"; truncate to 128 code points.
    let mut s: Vec<char> = Vec::new();
    for c in chars {
        if u32::from(c) > 0xFFFF {
            s.push('0');
            s.push('0');
        } else {
            s.push(c);
        }
    }
    s.truncate(128);
    if s.first() == Some(&'#') {
        s.remove(0);
    }
    for c in &mut s {
        if !c.is_ascii_hexdigit() {
            *c = '0';
        }
    }
    while s.is_empty() || !s.len().is_multiple_of(3) {
        s.push('0');
    }
    let len = s.len() / 3;
    let mut parts: Vec<Vec<char>> = s.chunks(len).map(<[char]>::to_vec).collect();
    if len > 8 {
        for p in &mut parts {
            let drop = p.len() - 8;
            p.drain(..drop);
        }
    }
    while parts.iter().all(|p| p.len() > 2 && p.first() == Some(&'0')) {
        for p in &mut parts {
            p.remove(0);
        }
    }
    for p in &mut parts {
        p.truncate(2);
    }
    let channel = |p: Option<&Vec<char>>| -> u8 {
        p.map_or(0, |p| {
            p.iter()
                .fold(0u32, |acc, c| acc * 16 + c.to_digit(16).unwrap_or(0))
                .min(255) as u8
        })
    };
    Some(Rgba::rgb(
        channel(parts.first()),
        channel(parts.get(1)),
        channel(parts.get(2)),
    ))
}

/// HTML §15.3.4 "rules for parsing a legacy font size".
fn parse_legacy_font_size(input: &str) -> Option<FontSizeKeyword> {
    let s = input.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (mode, rest) = match s.as_bytes().first() {
        Some(b'+') => (1, s.get(1..)?),
        Some(b'-') => (-1, s.get(1..)?),
        _ => (0, s),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let mut value: i64 = digits.chars().take(6).collect::<String>().parse().ok()?;
    if mode == 1 {
        value += 3;
    } else if mode == -1 {
        value = 3 - value;
    }
    use FontSizeKeyword as K;
    Some(match value.clamp(1, 7) {
        1 => K::XSmall,
        2 => K::Small,
        3 => K::Medium,
        4 => K::Large,
        5 => K::XLarge,
        6 => K::XxLarge,
        _ => K::XxxLarge,
    })
}

struct Hints {
    out: Vec<Declaration>,
}

impl Hints {
    fn set(&mut self, l: Longhand) {
        self.out.push(Declaration {
            kind: DeclarationKind::Longhand(l),
            important: false,
        });
    }

    fn color(&mut self, el: &ElementData, attr: &str, make: fn(Color) -> Longhand) {
        if let Some(c) = el.attribute(attr).and_then(parse_legacy_color) {
            self.set(make(Color::Rgba(c)));
        }
    }

    fn background(&mut self, el: &ElementData) {
        self.color(el, "bgcolor", Longhand::BackgroundColor);
        if let Some(url) = el.attribute("background") {
            let url = url.trim();
            if !url.is_empty() {
                self.set(Longhand::BackgroundImage(vec![Image::Url(url.to_string())]));
            }
        }
    }

    /// "maps to the dimension property" (HTML §15.3.1).
    fn dimension(&mut self, value: Option<&str>, nonzero: bool, make: fn(Size) -> Longhand) {
        let Some(v) = value else {
            return;
        };
        let d = if nonzero {
            parse_nonzero_dimension(v)
        } else {
            parse_dimension(v)
        };
        if let Some(d) = d {
            self.set(make(Size::LengthPercentage(dim_lp(d))));
        }
    }

    /// "maps to the pixel length property".
    fn pixel_sides(&mut self, value: Option<&str>, ids: &[fn(LengthPercentageOrAuto) -> Longhand]) {
        if let Some(px) = value.and_then(parse_dimension) {
            let lp = dim_lp(px);
            for make in ids {
                make_set(
                    self,
                    *make,
                    LengthPercentageOrAuto::LengthPercentage(lp.clone()),
                );
            }
        }
    }

    fn text_align(&mut self, el: &ElementData, allow_middle: bool) {
        let Some(v) = el.attribute("align") else {
            return;
        };
        let v = v.trim();
        let a = if v.eq_ignore_ascii_case("left") {
            TextAlign::Left
        } else if v.eq_ignore_ascii_case("right") {
            TextAlign::Right
        } else if v.eq_ignore_ascii_case("center")
            || (allow_middle && v.eq_ignore_ascii_case("middle"))
        {
            // M1 follow-up: the `-webkit-center`-like alignment of block
            // children (HTML §15.3.3).
            TextAlign::Center
        } else if v.eq_ignore_ascii_case("justify") {
            TextAlign::Justify
        } else {
            return;
        };
        self.set(Longhand::TextAlign(a));
    }

    fn valign(&mut self, el: &ElementData) {
        let Some(v) = el.attribute("valign") else {
            return;
        };
        let v = v.trim();
        use VerticalAlignKeyword as K;
        let k = if v.eq_ignore_ascii_case("top") {
            K::Top
        } else if v.eq_ignore_ascii_case("middle") {
            K::Middle
        } else if v.eq_ignore_ascii_case("bottom") {
            K::Bottom
        } else if v.eq_ignore_ascii_case("baseline") {
            K::Baseline
        } else {
            return;
        };
        self.set(Longhand::VerticalAlign(VerticalAlign::Keyword(k)));
    }

    fn border_all(&mut self, width: f32, style: LineStyle) {
        let w = Lp::Length(Length::px(width));
        self.set(Longhand::BorderTopWidth(w.clone()));
        self.set(Longhand::BorderRightWidth(w.clone()));
        self.set(Longhand::BorderBottomWidth(w.clone()));
        self.set(Longhand::BorderLeftWidth(w));
        self.set(Longhand::BorderTopStyle(style));
        self.set(Longhand::BorderRightStyle(style));
        self.set(Longhand::BorderBottomStyle(style));
        self.set(Longhand::BorderLeftStyle(style));
    }
}

fn make_set(
    h: &mut Hints,
    make: fn(LengthPercentageOrAuto) -> Longhand,
    v: LengthPercentageOrAuto,
) {
    h.set(make(v));
}

fn dim_lp(d: Dimension) -> Lp {
    match d {
        Dimension::Length(v) => Lp::Length(Length::px(v)),
        Dimension::Percentage(p) => Lp::Percentage(p),
    }
}

fn px_lp(v: f32) -> Lp {
    Lp::Length(Length::px(v))
}

/// The table an element belongs to, for cell and row hints: the nearest
/// `table` within three levels (`td` → `tr` → section → `table`).
fn owning_table(doc: &Document, node: NodeId) -> Option<&ElementData> {
    let mut cur = doc.parent(node);
    for _ in 0..3 {
        let p = cur?;
        let el = doc.element(p)?;
        if el.is_html("table") {
            return Some(el);
        }
        cur = doc.parent(p);
    }
    None
}

/// The `body` element's link color hints apply to every `:link` element
/// (HTML §15.3.4).
pub(crate) fn body_link_color(doc: &Document) -> Option<Rgba> {
    let body = doc.html_child_of_document_element("body")?;
    doc.element(body)?
        .attribute("link")
        .and_then(parse_legacy_color)
}

/// The presentational hints of an element, as declarations in the order
/// they apply. `link_color` is [`body_link_color`] for the document.
pub(crate) fn presentational_hints(
    doc: &Document,
    node: NodeId,
    el: &ElementData,
    link_color: Option<Rgba>,
) -> Vec<Declaration> {
    let mut h = Hints { out: Vec::new() };
    if el.namespace != Namespace::Html {
        return h.out;
    }
    let quirks = doc.quirks_mode() == wb_dom::QuirksMode::Quirks;
    let name = el.local_name.as_str();
    match name {
        "body" => {
            h.background(el);
            h.color(el, "text", Longhand::Color);
            let attr = |a: &str, b: &str| el.attribute(a).or_else(|| el.attribute(b));
            h.pixel_sides(attr("marginheight", "topmargin"), &[Longhand::MarginTop]);
            h.pixel_sides(
                attr("marginheight", "bottommargin"),
                &[Longhand::MarginBottom],
            );
            h.pixel_sides(attr("marginwidth", "leftmargin"), &[Longhand::MarginLeft]);
            h.pixel_sides(attr("marginwidth", "rightmargin"), &[Longhand::MarginRight]);
        }
        "a" | "area" => {
            if let Some(c) = link_color
                && el.has_attribute("href")
            {
                h.set(Longhand::Color(Color::Rgba(c)));
            }
        }
        "font" => {
            h.color(el, "color", Longhand::Color);
            if let Some(face) = el.attribute("face") {
                let values = parse_component_values(face);
                if let Some(ls) =
                    parse_property_value(PropertyId::Longhand(LonghandId::FontFamily), &values)
                {
                    for l in ls {
                        h.set(l);
                    }
                }
            }
            if let Some(k) = el.attribute("size").and_then(parse_legacy_font_size) {
                h.set(Longhand::FontSize(FontSize::Keyword(k)));
            }
        }
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => h.text_align(el, false),
        "div" | "caption" => {
            if name == "caption" {
                match el.attribute("align").map(str::trim) {
                    Some(v) if v.eq_ignore_ascii_case("top") => {
                        h.set(Longhand::CaptionSide(CaptionSide::Top));
                    }
                    Some(v) if v.eq_ignore_ascii_case("bottom") => {
                        h.set(Longhand::CaptionSide(CaptionSide::Bottom));
                    }
                    _ => h.text_align(el, true),
                }
            } else {
                h.text_align(el, true);
            }
        }
        "table" => {
            h.background(el);
            h.dimension(el.attribute("width"), true, Longhand::Width);
            h.dimension(el.attribute("height"), false, Longhand::Height);
            if let Some(px) = el
                .attribute("cellspacing")
                .and_then(parse_non_negative_integer)
            {
                #[allow(clippy::cast_precision_loss)]
                let v = px_lp(px as f32);
                h.set(Longhand::BorderSpacing(BorderSpacing {
                    horizontal: v.clone(),
                    vertical: v,
                }));
            }
            if let Some(border) = el.attribute("border") {
                // An unparsable value counts as 1 (HTML §15.3.8).
                let w = parse_non_negative_integer(border).unwrap_or(1);
                if w > 0 {
                    #[allow(clippy::cast_precision_loss)]
                    h.border_all(w as f32, LineStyle::Outset);
                }
            }
            match el.attribute("align").map(str::trim) {
                Some(v) if v.eq_ignore_ascii_case("left") => h.set(Longhand::Float(Float::Left)),
                Some(v) if v.eq_ignore_ascii_case("right") => h.set(Longhand::Float(Float::Right)),
                Some(v) if v.eq_ignore_ascii_case("center") => {
                    h.set(Longhand::MarginLeft(LengthPercentageOrAuto::Auto));
                    h.set(Longhand::MarginRight(LengthPercentageOrAuto::Auto));
                }
                _ => {}
            }
        }
        "thead" | "tbody" | "tfoot" | "tr" => {
            h.background(el);
            h.text_align(el, true);
            h.valign(el);
            if name == "tr" {
                h.dimension(el.attribute("height"), false, Longhand::Height);
            }
        }
        "td" | "th" => {
            h.background(el);
            h.text_align(el, true);
            h.valign(el);
            h.dimension(el.attribute("width"), true, Longhand::Width);
            h.dimension(el.attribute("height"), true, Longhand::Height);
            if el.has_attribute("nowrap") {
                h.set(Longhand::WhiteSpace(WhiteSpace::Nowrap));
            }
            if let Some(table) = owning_table(doc, node) {
                if let Some(p) = table
                    .attribute("cellpadding")
                    .and_then(parse_non_negative_integer)
                {
                    #[allow(clippy::cast_precision_loss)]
                    let v = px_lp(p as f32);
                    h.set(Longhand::PaddingTop(v.clone()));
                    h.set(Longhand::PaddingRight(v.clone()));
                    h.set(Longhand::PaddingBottom(v.clone()));
                    h.set(Longhand::PaddingLeft(v));
                }
                if let Some(b) = table.attribute("border")
                    && parse_non_negative_integer(b).unwrap_or(1) > 0
                {
                    h.border_all(1.0, LineStyle::Inset);
                }
            }
        }
        "col" | "colgroup" => {
            h.dimension(el.attribute("width"), true, Longhand::Width);
        }
        "img" | "object" | "embed" | "iframe" | "video" | "canvas" | "input" => {
            let is_image_input = name == "input"
                && el
                    .attribute("type")
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("image"));
            if name != "input" || is_image_input {
                h.dimension(el.attribute("width"), false, Longhand::Width);
                h.dimension(el.attribute("height"), false, Longhand::Height);
            }
            if matches!(name, "img" | "object" | "embed" | "iframe") || is_image_input {
                h.pixel_sides(
                    el.attribute("hspace"),
                    &[Longhand::MarginLeft, Longhand::MarginRight],
                );
                h.pixel_sides(
                    el.attribute("vspace"),
                    &[Longhand::MarginTop, Longhand::MarginBottom],
                );
                image_align(&mut h, el, quirks && name == "img");
            }
            if (matches!(name, "img" | "object") || is_image_input)
                && let Some(b) = el.attribute("border").and_then(parse_non_negative_integer)
            {
                #[allow(clippy::cast_precision_loss)]
                h.border_all(b as f32, LineStyle::Solid);
            }
            if name == "iframe"
                && el
                    .attribute("frameborder")
                    .is_some_and(|f| f.trim() == "0" || f.trim().eq_ignore_ascii_case("no"))
            {
                h.border_all(0.0, LineStyle::None);
            }
        }
        "hr" => hr_hints(&mut h, el),
        "ul" | "ol" | "li" => list_hints(&mut h, el, name),
        "br" => {
            let clear = match el.attribute("clear").map(str::trim) {
                Some(v) if v.eq_ignore_ascii_case("left") => Some(Clear::Left),
                Some(v) if v.eq_ignore_ascii_case("right") => Some(Clear::Right),
                Some(v) if v.eq_ignore_ascii_case("all") || v.eq_ignore_ascii_case("both") => {
                    Some(Clear::Both)
                }
                _ => None,
            };
            if let Some(c) = clear {
                h.set(Longhand::Clear(c));
            }
        }
        "pre" | "listing" | "xmp" => {
            if el.has_attribute("wrap") {
                h.set(Longhand::WhiteSpace(WhiteSpace::PreWrap));
            }
        }
        "marquee" => {
            h.color(el, "bgcolor", Longhand::BackgroundColor);
            h.dimension(el.attribute("width"), false, Longhand::Width);
            h.dimension(el.attribute("height"), false, Longhand::Height);
        }
        _ => {}
    }
    h.out
}

fn image_align(h: &mut Hints, el: &ElementData, quirks_img: bool) {
    let Some(v) = el.attribute("align").map(str::trim) else {
        return;
    };
    let lower = v.to_ascii_lowercase();
    use VerticalAlignKeyword as K;
    let va = match lower.as_str() {
        "left" => {
            h.set(Longhand::Float(Float::Left));
            if quirks_img {
                h.set(Longhand::MarginRight(
                    LengthPercentageOrAuto::LengthPercentage(px_lp(3.0)),
                ));
            }
            return;
        }
        "right" => {
            h.set(Longhand::Float(Float::Right));
            if quirks_img {
                h.set(Longhand::MarginLeft(
                    LengthPercentageOrAuto::LengthPercentage(px_lp(3.0)),
                ));
            }
            return;
        }
        "top" => K::Top,
        "middle" | "absmiddle" | "abscenter" => K::Middle,
        "bottom" | "baseline" => K::Baseline,
        "texttop" => K::TextTop,
        "absbottom" => K::Bottom,
        _ => return,
    };
    h.set(Longhand::VerticalAlign(VerticalAlign::Keyword(va)));
}

fn hr_hints(h: &mut Hints, el: &ElementData) {
    match el.attribute("align").map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case("left") => {
            h.set(Longhand::MarginLeft(
                LengthPercentageOrAuto::LengthPercentage(px_lp(0.0)),
            ));
            h.set(Longhand::MarginRight(LengthPercentageOrAuto::Auto));
        }
        Some(v) if v.eq_ignore_ascii_case("right") => {
            h.set(Longhand::MarginLeft(LengthPercentageOrAuto::Auto));
            h.set(Longhand::MarginRight(
                LengthPercentageOrAuto::LengthPercentage(px_lp(0.0)),
            ));
        }
        Some(v) if v.eq_ignore_ascii_case("center") => {
            h.set(Longhand::MarginLeft(LengthPercentageOrAuto::Auto));
            h.set(Longhand::MarginRight(LengthPercentageOrAuto::Auto));
        }
        _ => {}
    }
    let color = el.attribute("color").and_then(parse_legacy_color);
    if let Some(c) = color {
        let c = Color::Rgba(c);
        h.set(Longhand::BorderTopColor(c));
        h.set(Longhand::BorderRightColor(c));
        h.set(Longhand::BorderBottomColor(c));
        h.set(Longhand::BorderLeftColor(c));
        h.set(Longhand::BackgroundColor(c));
    }
    if el.has_attribute("noshade") || color.is_some() {
        for l in [
            Longhand::BorderTopStyle(LineStyle::Solid),
            Longhand::BorderRightStyle(LineStyle::Solid),
            Longhand::BorderBottomStyle(LineStyle::Solid),
            Longhand::BorderLeftStyle(LineStyle::Solid),
        ] {
            h.set(l);
        }
        if color.is_none() {
            let gray = Color::Rgba(Rgba::rgb(128, 128, 128));
            h.set(Longhand::BorderTopColor(gray));
            h.set(Longhand::BorderRightColor(gray));
            h.set(Longhand::BorderBottomColor(gray));
            h.set(Longhand::BorderLeftColor(gray));
            h.set(Longhand::BackgroundColor(gray));
        }
    }
    if let Some(size) = el.attribute("size").and_then(parse_non_negative_integer)
        && size >= 1
    {
        if size == 1 {
            h.set(Longhand::BorderBottomWidth(px_lp(0.0)));
        } else {
            #[allow(clippy::cast_precision_loss)]
            let v = (size - 2).max(0) as f32;
            h.set(Longhand::Height(Size::LengthPercentage(px_lp(v))));
        }
    }
    h.dimension(el.attribute("width"), true, Longhand::Width);
}

fn list_hints(h: &mut Hints, el: &ElementData, name: &str) {
    if let Some(t) = el.attribute("type") {
        let t = t.trim();
        let ol_style = match t {
            "1" => Some("decimal"),
            "a" => Some("lower-alpha"),
            "A" => Some("upper-alpha"),
            "i" => Some("lower-roman"),
            "I" => Some("upper-roman"),
            _ => None,
        };
        let ul_style = ["none", "disc", "circle", "square"]
            .into_iter()
            .find(|k| t.eq_ignore_ascii_case(k));
        let style = match name {
            "ol" => ol_style,
            "ul" => ul_style,
            _ => ol_style.or(ul_style),
        };
        match style {
            Some("none") => h.set(Longhand::ListStyleType(ListStyleType::None)),
            Some(s) => h.set(Longhand::ListStyleType(ListStyleType::Named(s.to_string()))),
            None => {}
        }
    }
    if name == "ol"
        && !el.has_attribute("reversed")
        && let Some(start) = el.attribute("start").and_then(parse_signed_integer)
    {
        // The list-item counter starts one below the first value (CSS
        // Lists 3 §4.4). M1 follow-up: `reversed` and `li value`
        // (`counter-set`), which layout reads from the attributes.
        h.set(Longhand::CounterReset(vec![(
            "list-item".to_string(),
            start.saturating_sub(1),
        )]));
    }
}

/// HTML §2.3.4.1 "rules for parsing integers", saturating.
fn parse_signed_integer(input: &str) -> Option<i32> {
    let s = input.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let v = parse_non_negative_integer_strict(s)?;
    Some(if neg { v.saturating_neg() } else { v })
}

fn parse_non_negative_integer_strict(s: &str) -> Option<i32> {
    if !s.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    parse_non_negative_integer(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_colors() {
        assert_eq!(parse_legacy_color("#ff6600"), Some(Rgba::rgb(255, 102, 0)));
        assert_eq!(
            parse_legacy_color("#f6f6ef"),
            Some(Rgba::rgb(0xf6, 0xf6, 0xef))
        );
        assert_eq!(parse_legacy_color("ff6600"), Some(Rgba::rgb(255, 102, 0)));
        assert_eq!(
            parse_legacy_color("#abc"),
            Some(Rgba::rgb(0xaa, 0xbb, 0xcc))
        );
        assert_eq!(parse_legacy_color("red"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_legacy_color(" Red "), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(parse_legacy_color("transparent"), None);
        assert_eq!(parse_legacy_color(""), None);
        // The classic "chucknorris" is red.
        assert_eq!(
            parse_legacy_color("chucknorris"),
            Some(Rgba::rgb(0xc0, 0, 0))
        );
        assert_eq!(
            parse_legacy_color("#12345"),
            Some(Rgba::rgb(0x12, 0x34, 0x50))
        );
        assert_eq!(
            parse_legacy_color("#123456789abc"),
            Some(Rgba::rgb(0x12, 0x56, 0x9a))
        );
        let long = "1".repeat(10_000);
        assert!(parse_legacy_color(&long).is_some());
    }

    #[test]
    fn dimensions() {
        assert_eq!(parse_dimension("85%"), Some(Dimension::Percentage(85.0)));
        assert_eq!(parse_dimension(" 18"), Some(Dimension::Length(18.0)));
        assert_eq!(parse_dimension("1.5px"), Some(Dimension::Length(1.5)));
        assert_eq!(parse_dimension("x"), None);
        assert_eq!(parse_nonzero_dimension("0"), None);
        assert_eq!(
            parse_dimension("99999999999999"),
            Some(Dimension::Length(1e9))
        );
        assert_eq!(parse_non_negative_integer("+12abc"), Some(12));
        assert_eq!(parse_non_negative_integer("-1"), None);
        assert_eq!(parse_non_negative_integer("99999999999"), Some(i32::MAX));
        assert_eq!(parse_signed_integer("-5"), Some(-5));
    }

    #[test]
    fn legacy_font_sizes() {
        assert_eq!(parse_legacy_font_size("1"), Some(FontSizeKeyword::XSmall));
        assert_eq!(parse_legacy_font_size("+1"), Some(FontSizeKeyword::Large));
        assert_eq!(parse_legacy_font_size("-1"), Some(FontSizeKeyword::Small));
        assert_eq!(parse_legacy_font_size("9"), Some(FontSizeKeyword::XxxLarge));
        assert_eq!(parse_legacy_font_size("-9"), Some(FontSizeKeyword::XSmall));
        assert_eq!(parse_legacy_font_size("x"), None);
    }
}
