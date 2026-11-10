//! Shorthand parsing and expansion.

use crate::color::Color;
use crate::cursor::Cursor;
use crate::properties::{Longhand as L, ShorthandId};
use crate::syntax::split_commas;
use crate::types::*;
use crate::values::{Image, Length, LengthPercentage, Range, parse_number};

/// Parses the value of shorthand `s`; the caller checks that all input was
/// consumed.
pub(crate) fn parse(s: ShorthandId, c: &mut Cursor<'_>) -> Option<Vec<L>> {
    use ShorthandId as S;
    match s {
        S::Margin => {
            let [t, r, b, l] = four(c, lp_or_auto)?;
            Some(vec![
                L::MarginTop(t),
                L::MarginRight(r),
                L::MarginBottom(b),
                L::MarginLeft(l),
            ])
        }
        S::Padding => {
            let [t, r, b, l] = four(c, lp_non_negative)?;
            Some(vec![
                L::PaddingTop(t),
                L::PaddingRight(r),
                L::PaddingBottom(b),
                L::PaddingLeft(l),
            ])
        }
        S::Inset => {
            let [t, r, b, l] = four(c, lp_or_auto)?;
            Some(vec![L::Top(t), L::Right(r), L::Bottom(b), L::Left(l)])
        }
        S::BorderWidth => {
            let [t, r, b, l] = four(c, line_width)?;
            Some(vec![
                L::BorderTopWidth(t),
                L::BorderRightWidth(r),
                L::BorderBottomWidth(b),
                L::BorderLeftWidth(l),
            ])
        }
        S::BorderStyle => {
            let [t, r, b, l] = four(c, LineStyle::parse)?;
            Some(vec![
                L::BorderTopStyle(t),
                L::BorderRightStyle(r),
                L::BorderBottomStyle(b),
                L::BorderLeftStyle(l),
            ])
        }
        S::BorderColor => {
            let [t, r, b, l] = four(c, color)?;
            Some(vec![
                L::BorderTopColor(t),
                L::BorderRightColor(r),
                L::BorderBottomColor(b),
                L::BorderLeftColor(l),
            ])
        }
        S::Border | S::BorderTop | S::BorderRight | S::BorderBottom | S::BorderLeft => {
            let (w, st, col) = border_side(c)?;
            let sides: &[usize] = match s {
                S::BorderTop => &[0],
                S::BorderRight => &[1],
                S::BorderBottom => &[2],
                S::BorderLeft => &[3],
                _ => &[0, 1, 2, 3],
            };
            let mut out = Vec::new();
            for side in sides {
                out.extend(border_side_longhands(*side, w.clone(), st, col));
            }
            Some(out)
        }
        S::BorderRadius => border_radius(c),
        S::Overflow => {
            let x = overflow(c)?;
            let y = c.attempt(overflow).unwrap_or(x);
            Some(vec![L::OverflowX(x), L::OverflowY(y)])
        }
        S::Gap => {
            let row = gap(c)?;
            let col = c.attempt(gap).unwrap_or_else(|| row.clone());
            Some(vec![L::RowGap(row), L::ColumnGap(col)])
        }
        S::FlexFlow => {
            let mut dir = None;
            let mut wrap = None;
            for _ in 0..2 {
                if dir.is_none()
                    && let Some(d) = FlexDirection::parse(c)
                {
                    dir = Some(d);
                    continue;
                }
                if wrap.is_none()
                    && let Some(w) = FlexWrap::parse(c)
                {
                    wrap = Some(w);
                    continue;
                }
                break;
            }
            if dir.is_none() && wrap.is_none() {
                return None;
            }
            Some(vec![
                L::FlexDirection(dir.unwrap_or(FlexDirection::Row)),
                L::FlexWrap(wrap.unwrap_or(FlexWrap::Nowrap)),
            ])
        }
        S::Flex => flex(c),
        S::TextDecoration => text_decoration(c),
        S::ListStyle => list_style(c),
        S::Font => font(c),
        S::Background => background(c),
    }
}

/// The 1-to-4 value box expansion (top, right, bottom, left).
fn four<T: Clone>(
    c: &mut Cursor<'_>,
    mut item: impl FnMut(&mut Cursor<'_>) -> Option<T>,
) -> Option<[T; 4]> {
    let top = item(c)?;
    let right = c.attempt(&mut item);
    let bottom = right.as_ref().and_then(|_| c.attempt(&mut item));
    let left = bottom.as_ref().and_then(|_| c.attempt(&mut item));
    let right = right.unwrap_or_else(|| top.clone());
    let bottom = bottom.unwrap_or_else(|| top.clone());
    let left = left.unwrap_or_else(|| right.clone());
    Some([top, right, bottom, left])
}

/// `<line-width> || <line-style> || <color>` (CSS Backgrounds 3 §3.6).
fn border_side(c: &mut Cursor<'_>) -> Option<(LengthPercentage, LineStyle, Color)> {
    let mut w = None;
    let mut s = None;
    let mut col = None;
    loop {
        if w.is_none()
            && let Some(v) = c.attempt(line_width)
        {
            w = Some(v);
            continue;
        }
        if s.is_none()
            && let Some(v) = LineStyle::parse(c)
        {
            s = Some(v);
            continue;
        }
        if col.is_none()
            && let Some(v) = c.attempt(color)
        {
            col = Some(v);
            continue;
        }
        break;
    }
    if w.is_none() && s.is_none() && col.is_none() {
        return None;
    }
    Some((
        w.unwrap_or(LengthPercentage::Length(Length::px(3.0))),
        s.unwrap_or(LineStyle::None),
        col.unwrap_or(Color::CurrentColor),
    ))
}

fn border_side_longhands(side: usize, w: LengthPercentage, s: LineStyle, c: Color) -> [L; 3] {
    match side {
        0 => [
            L::BorderTopWidth(w),
            L::BorderTopStyle(s),
            L::BorderTopColor(c),
        ],
        1 => [
            L::BorderRightWidth(w),
            L::BorderRightStyle(s),
            L::BorderRightColor(c),
        ],
        2 => [
            L::BorderBottomWidth(w),
            L::BorderBottomStyle(s),
            L::BorderBottomColor(c),
        ],
        _ => [
            L::BorderLeftWidth(w),
            L::BorderLeftStyle(s),
            L::BorderLeftColor(c),
        ],
    }
}

/// `<length-percentage>{1,4} [ / <length-percentage>{1,4} ]?`.
fn border_radius(c: &mut Cursor<'_>) -> Option<Vec<L>> {
    let [h_tl, h_tr, h_br, h_bl] = four(c, lp_non_negative)?;
    let [v_tl, v_tr, v_br, v_bl] = if c.eat_delim('/') {
        four(c, lp_non_negative)?
    } else {
        [h_tl.clone(), h_tr.clone(), h_br.clone(), h_bl.clone()]
    };
    let cr = |horizontal, vertical| CornerRadius {
        horizontal,
        vertical,
    };
    Some(vec![
        L::BorderTopLeftRadius(cr(h_tl, v_tl)),
        L::BorderTopRightRadius(cr(h_tr, v_tr)),
        L::BorderBottomRightRadius(cr(h_br, v_br)),
        L::BorderBottomLeftRadius(cr(h_bl, v_bl)),
    ])
}

/// `none | [ <flex-grow> <flex-shrink>? || <flex-basis> ]` (CSS Flexbox 1
/// §7.1.1). An omitted basis is `0%`, matching browsers.
fn flex(c: &mut Cursor<'_>) -> Option<Vec<L>> {
    let auto = || FlexBasis::Size(Size::Auto);
    if c.eat_ident("none") {
        return Some(vec![
            L::FlexGrow(0.0),
            L::FlexShrink(0.0),
            L::FlexBasis(auto()),
        ]);
    }
    let mut grow = None;
    let mut shrink = None;
    let mut basis = None;
    loop {
        if grow.is_none()
            && let Some(g) = c.attempt(|c| parse_number(c, Range::NonNegative))
        {
            grow = Some(g);
            shrink = c.attempt(|c| parse_number(c, Range::NonNegative));
            continue;
        }
        if basis.is_none()
            && let Some(b) = c.attempt(flex_basis)
        {
            basis = Some(b);
            continue;
        }
        break;
    }
    if grow.is_none() && basis.is_none() {
        return None;
    }
    Some(vec![
        L::FlexGrow(grow.unwrap_or(1.0)),
        L::FlexShrink(shrink.unwrap_or(1.0)),
        L::FlexBasis(basis.unwrap_or(FlexBasis::Size(Size::LengthPercentage(
            LengthPercentage::Percentage(0.0),
        )))),
    ])
}

/// `<line> || <style> || <color>` (CSS Text Decoration 3 §2.4).
fn text_decoration(c: &mut Cursor<'_>) -> Option<Vec<L>> {
    let mut line = None;
    let mut style = None;
    let mut col = None;
    loop {
        if line.is_none()
            && let Some(v) = c.attempt(text_decoration_line)
        {
            line = Some(v);
            continue;
        }
        if style.is_none()
            && let Some(v) = TextDecorationStyle::parse(c)
        {
            style = Some(v);
            continue;
        }
        if col.is_none()
            && let Some(v) = c.attempt(color)
        {
            col = Some(v);
            continue;
        }
        break;
    }
    if line.is_none() && style.is_none() && col.is_none() {
        return None;
    }
    Some(vec![
        L::TextDecorationLine(line.unwrap_or_default()),
        L::TextDecorationStyle(style.unwrap_or(TextDecorationStyle::Solid)),
        L::TextDecorationColor(col.unwrap_or(Color::CurrentColor)),
    ])
}

/// `<type> || <position> || <image>` (CSS Lists 3 §3.4), with the `none`
/// disambiguation rule.
fn list_style(c: &mut Cursor<'_>) -> Option<Vec<L>> {
    let mut ty = None;
    let mut pos = None;
    let mut img = None;
    let mut nones = 0;
    loop {
        if c.eat_ident("none") {
            nones += 1;
            continue;
        }
        if pos.is_none()
            && let Some(v) = ListStylePosition::parse(c)
        {
            pos = Some(v);
            continue;
        }
        if img.is_none()
            && let Some(v) = c.attempt(crate::values::parse_image)
        {
            img = Some(v);
            continue;
        }
        if ty.is_none()
            && let Some(v) = c.attempt(list_style_type)
        {
            ty = Some(v);
            continue;
        }
        break;
    }
    // Each `none` fills type or image, whichever is still unset.
    for _ in 0..nones {
        if ty.is_none() && img.is_some() {
            ty = Some(ListStyleType::None);
        } else if img.is_none() {
            img = Some(Image::None);
        } else if ty.is_none() {
            ty = Some(ListStyleType::None);
        } else {
            return None;
        }
    }
    if nones == 1 && ty.is_none() {
        // `list-style: none` sets both.
        ty = Some(ListStyleType::None);
    }
    if ty.is_none() && pos.is_none() && img.is_none() {
        return None;
    }
    Some(vec![
        L::ListStyleType(ty.unwrap_or_else(|| ListStyleType::Named("disc".into()))),
        L::ListStylePosition(pos.unwrap_or(ListStylePosition::Outside)),
        L::ListStyleImage(img.unwrap_or(Image::None)),
    ])
}

/// `[ <font-style> || <font-variant-css2> || <font-weight> ||
/// <font-width-css3> ]? <font-size> [ / <line-height> ]? <font-family>`
/// (CSS Fonts 4 §2.7).
// M1 follow-up: system font keywords (`caption`, `menu`, ...) and resetting
// font-variant and the other sub-properties.
fn font(c: &mut Cursor<'_>) -> Option<Vec<L>> {
    let mut style = None;
    let mut weight = None;
    let mut stretch = None;
    let mut variant = false;
    let mut count = 0;
    while count < 4 {
        count += 1;
        if c.eat_ident("normal") {
            continue;
        }
        if style.is_none()
            && let Some(v) = c.attempt(font_style)
        {
            style = Some(v);
            continue;
        }
        if !variant && c.eat_ident("small-caps") {
            variant = true;
            continue;
        }
        if weight.is_none()
            && let Some(v) = c.attempt(font_weight)
        {
            weight = Some(v);
            continue;
        }
        if stretch.is_none()
            && let Some(v) = c.attempt(font_stretch_keyword)
        {
            stretch = Some(v);
            continue;
        }
        break;
    }
    let size = font_size(c)?;
    let lh = if c.eat_delim('/') {
        line_height(c)?
    } else {
        LineHeight::Normal
    };
    let family = font_family_list(c)?;
    Some(vec![
        L::FontStyle(style.unwrap_or(FontStyle::Normal)),
        L::FontWeight(weight.unwrap_or(FontWeight::Absolute(400.0))),
        L::FontStretch(stretch.unwrap_or(100.0)),
        L::FontSize(size),
        L::LineHeight(lh),
        L::FontFamily(family),
    ])
}

struct BgLayer {
    image: Option<Image>,
    position: Option<BackgroundPosition>,
    size: Option<BackgroundSize>,
    repeat: Option<BackgroundRepeat>,
    attachment: Option<BackgroundAttachment>,
    origin: Option<BackgroundBox>,
    clip: Option<BackgroundBox>,
    color: Option<Color>,
}

fn bg_layer(c: &mut Cursor<'_>, is_final: bool) -> Option<BgLayer> {
    let mut l = BgLayer {
        image: None,
        position: None,
        size: None,
        repeat: None,
        attachment: None,
        origin: None,
        clip: None,
        color: None,
    };
    let mut any = false;
    loop {
        if l.image.is_none()
            && let Some(v) = c.attempt(crate::values::parse_image_or_none)
        {
            l.image = Some(v);
        } else if l.position.is_none()
            && let Some(v) = c.attempt(background_position)
        {
            l.position = Some(v);
            if c.eat_delim('/') {
                l.size = Some(background_size(c)?);
            }
        } else if l.repeat.is_none()
            && let Some(v) = c.attempt(background_repeat)
        {
            l.repeat = Some(v);
        } else if l.attachment.is_none()
            && let Some(v) = BackgroundAttachment::parse(c)
        {
            l.attachment = Some(v);
        } else if l.origin.is_none()
            && let Some(v) = c.attempt(visual_box)
        {
            l.origin = Some(v);
        } else if l.origin.is_some()
            && l.clip.is_none()
            && let Some(v) = c.attempt(clip_box)
        {
            l.clip = Some(v);
        } else if is_final
            && l.color.is_none()
            && let Some(v) = c.attempt(color)
        {
            l.color = Some(v);
        } else {
            break;
        }
        any = true;
    }
    // One box value sets both origin and clip.
    if l.clip.is_none() {
        l.clip = l.origin;
    }
    (any && c.is_exhausted()).then_some(l)
}

/// `<bg-layer>#? , <final-bg-layer>` (CSS Backgrounds 3 §2.11).
fn background(c: &mut Cursor<'_>) -> Option<Vec<L>> {
    let parts = split_commas(c.rest());
    let n = parts.len();
    let mut layers = Vec::with_capacity(n);
    for (i, part) in parts.into_iter().enumerate() {
        let mut lc = Cursor::new(part);
        layers.push(bg_layer(&mut lc, i + 1 == n)?);
    }
    // Consume everything: the layers were parsed from `rest()`.
    while c.next_including_ws().is_some() {}
    let initial_pos = || BackgroundPosition {
        x: PositionComponent {
            edge: PositionEdge::Start,
            offset: LengthPercentage::Percentage(0.0),
        },
        y: PositionComponent {
            edge: PositionEdge::Start,
            offset: LengthPercentage::Percentage(0.0),
        },
    };
    let bg_color = layers
        .last()
        .and_then(|l| l.color)
        .unwrap_or(Color::TRANSPARENT);
    let mut images = Vec::new();
    let mut positions = Vec::new();
    let mut sizes = Vec::new();
    let mut repeats = Vec::new();
    let mut attachments = Vec::new();
    let mut origins = Vec::new();
    let mut clips = Vec::new();
    for l in layers {
        images.push(l.image.unwrap_or(Image::None));
        positions.push(l.position.unwrap_or_else(initial_pos));
        sizes.push(l.size.unwrap_or(BackgroundSize::Explicit {
            width: None,
            height: None,
        }));
        repeats.push(l.repeat.unwrap_or(BackgroundRepeat {
            x: RepeatStyle::Repeat,
            y: RepeatStyle::Repeat,
        }));
        attachments.push(l.attachment.unwrap_or(BackgroundAttachment::Scroll));
        origins.push(l.origin.unwrap_or(BackgroundBox::PaddingBox));
        clips.push(l.clip.unwrap_or(BackgroundBox::BorderBox));
    }
    Some(vec![
        L::BackgroundColor(bg_color),
        L::BackgroundImage(images),
        L::BackgroundRepeat(repeats),
        L::BackgroundPosition(positions),
        L::BackgroundSize(sizes),
        L::BackgroundAttachment(attachments),
        L::BackgroundOrigin(origins),
        L::BackgroundClip(clips),
    ])
}
