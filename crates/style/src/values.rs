//! Specified-to-computed value conversion (CSS Values 4 §6, §10).

use std::sync::Arc;

use wb_css::{
    CalcNode, Color, Content, ContentItem, CursorImage, CursorValue, FontSizeKeyword, Image,
    Length, LengthUnit, Rgba,
};

use crate::computed::{
    BackgroundSize, CalcExpr, FlexBasis, Gap, LengthPercentage, LengthPercentageOrAuto, MaxSize,
    PositionComponent, Size, VerticalAlign,
};

/// Computed lengths are clamped to this magnitude so later arithmetic in
/// layout stays finite (CSS Values 4 §10.12 lets UAs clamp).
pub(crate) const MAX_PX: f32 = 33_554_428.0;

/// Makes a pixel value finite and bounded.
pub(crate) fn sanitize(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-MAX_PX, MAX_PX)
    }
}

/// What relative lengths are resolved against.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LengthContext {
    /// `em` basis.
    pub(crate) font_size: f32,
    /// `rem` basis.
    pub(crate) root_font_size: f32,
    /// `lh` basis in pixels.
    pub(crate) line_height: f32,
    /// `rlh` basis in pixels.
    pub(crate) root_line_height: f32,
    pub(crate) viewport_width: f32,
    pub(crate) viewport_height: f32,
}

impl LengthContext {
    /// A length in CSS pixels. Font-relative units that need font metrics
    /// use the fallbacks of CSS Values 4 §6.1.1 (`ex` and `ch` are 0.5em,
    /// `ic` is 1em); `cap` uses 0.7em.
    pub(crate) fn px(&self, l: &Length) -> f32 {
        let v = l.value;
        let em = self.font_size;
        let rem = self.root_font_size;
        let vw = self.viewport_width / 100.0;
        let vh = self.viewport_height / 100.0;
        let px = match l.unit {
            LengthUnit::Em => v * em,
            LengthUnit::Rem => v * rem,
            LengthUnit::Ex | LengthUnit::Ch => v * em * 0.5,
            LengthUnit::Rex | LengthUnit::Rch => v * rem * 0.5,
            LengthUnit::Cap => v * em * 0.7,
            LengthUnit::Ic => v * em,
            LengthUnit::Ric => v * rem,
            LengthUnit::Lh => v * self.line_height,
            LengthUnit::Rlh => v * self.root_line_height,
            LengthUnit::Vw | LengthUnit::Vi => v * vw,
            LengthUnit::Vh | LengthUnit::Vb => v * vh,
            LengthUnit::Vmin => v * vw.min(vh),
            LengthUnit::Vmax => v * vw.max(vh),
            other => v * other.absolute_px().unwrap_or(1.0),
        };
        sanitize(px)
    }

    /// Converts a specified `<length-percentage>`.
    pub(crate) fn lp(&self, v: &wb_css::LengthPercentage) -> LengthPercentage {
        match v {
            wb_css::LengthPercentage::Length(l) => LengthPercentage::Length(self.px(l)),
            wb_css::LengthPercentage::Percentage(p) => LengthPercentage::Percentage(sanitize(*p)),
            wb_css::LengthPercentage::Calc(node) => {
                let expr = self.calc(node, 0);
                if expr.has_percentage() {
                    LengthPercentage::Calc(Arc::new(expr))
                } else {
                    LengthPercentage::Length(sanitize(expr.resolve(0.0)))
                }
            }
        }
    }

    /// Like [`Self::lp`] but clamps plain lengths to be non-negative (for
    /// properties that do not accept negative values).
    pub(crate) fn lp_non_negative(&self, v: &wb_css::LengthPercentage) -> LengthPercentage {
        match self.lp(v) {
            LengthPercentage::Length(px) => LengthPercentage::Length(px.max(0.0)),
            other => other,
        }
    }

    /// Resolves a `<length-percentage>` against a known basis to pixels.
    pub(crate) fn lp_px(&self, v: &wb_css::LengthPercentage, basis: f32) -> f32 {
        sanitize(self.lp(v).resolve(basis))
    }

    fn calc(&self, node: &CalcNode, depth: u32) -> CalcExpr {
        // `wb-css` bounds nesting at 64; this guards the recursion anyway.
        if depth > 128 {
            return CalcExpr::Number(0.0);
        }
        let d = depth + 1;
        let list = |v: &[CalcNode]| v.iter().map(|n| self.calc(n, d)).collect();
        match node {
            CalcNode::Number(n) => CalcExpr::Number(*n),
            CalcNode::Length(l) => CalcExpr::Px(self.px(l)),
            CalcNode::Percentage(p) => CalcExpr::Percentage(*p),
            CalcNode::Sum(v) => CalcExpr::Sum(list(v)),
            CalcNode::Negate(n) => CalcExpr::Negate(Box::new(self.calc(n, d))),
            CalcNode::Product(v) => CalcExpr::Product(list(v)),
            CalcNode::Invert(n) => CalcExpr::Invert(Box::new(self.calc(n, d))),
            CalcNode::Min(v) => CalcExpr::Min(list(v)),
            CalcNode::Max(v) => CalcExpr::Max(list(v)),
            CalcNode::Clamp(a, b, c) => CalcExpr::Clamp(
                Box::new(self.calc(a, d)),
                Box::new(self.calc(b, d)),
                Box::new(self.calc(c, d)),
            ),
        }
    }

    pub(crate) fn lp_or_auto(&self, v: &wb_css::LengthPercentageOrAuto) -> LengthPercentageOrAuto {
        match v {
            wb_css::LengthPercentageOrAuto::Auto => LengthPercentageOrAuto::Auto,
            wb_css::LengthPercentageOrAuto::LengthPercentage(lp) => {
                LengthPercentageOrAuto::LengthPercentage(self.lp(lp))
            }
        }
    }

    pub(crate) fn size(&self, v: &wb_css::Size) -> Size {
        match v {
            wb_css::Size::Auto => Size::Auto,
            wb_css::Size::LengthPercentage(lp) => Size::LengthPercentage(self.lp_non_negative(lp)),
            wb_css::Size::MinContent => Size::MinContent,
            wb_css::Size::MaxContent => Size::MaxContent,
            wb_css::Size::FitContent(lp) => {
                Size::FitContent(lp.as_ref().map(|l| self.lp_non_negative(l)))
            }
        }
    }

    pub(crate) fn max_size(&self, v: &wb_css::MaxSize) -> MaxSize {
        match v {
            wb_css::MaxSize::None => MaxSize::None,
            wb_css::MaxSize::LengthPercentage(lp) => {
                MaxSize::LengthPercentage(self.lp_non_negative(lp))
            }
            wb_css::MaxSize::MinContent => MaxSize::MinContent,
            wb_css::MaxSize::MaxContent => MaxSize::MaxContent,
            wb_css::MaxSize::FitContent(lp) => {
                MaxSize::FitContent(lp.as_ref().map(|l| self.lp_non_negative(l)))
            }
        }
    }

    pub(crate) fn flex_basis(&self, v: &wb_css::FlexBasis) -> FlexBasis {
        match v {
            wb_css::FlexBasis::Content => FlexBasis::Content,
            wb_css::FlexBasis::Size(s) => FlexBasis::Size(self.size(s)),
        }
    }

    pub(crate) fn gap(&self, v: &wb_css::Gap) -> Gap {
        match v {
            wb_css::Gap::Normal => Gap::Normal,
            wb_css::Gap::LengthPercentage(lp) => Gap::LengthPercentage(self.lp_non_negative(lp)),
        }
    }

    pub(crate) fn vertical_align(&self, v: &wb_css::VerticalAlign) -> VerticalAlign {
        match v {
            wb_css::VerticalAlign::Keyword(k) => VerticalAlign::Keyword(*k),
            wb_css::VerticalAlign::LengthPercentage(lp) => {
                VerticalAlign::LengthPercentage(self.lp(lp))
            }
        }
    }

    pub(crate) fn position_component(&self, v: &wb_css::PositionComponent) -> PositionComponent {
        PositionComponent {
            edge: v.edge,
            offset: self.lp(&v.offset),
        }
    }

    pub(crate) fn background_size(&self, v: &wb_css::BackgroundSize) -> BackgroundSize {
        match v {
            wb_css::BackgroundSize::Cover => BackgroundSize::Cover,
            wb_css::BackgroundSize::Contain => BackgroundSize::Contain,
            wb_css::BackgroundSize::Explicit { width, height } => BackgroundSize::Explicit {
                width: width.as_ref().map(|l| self.lp_non_negative(l)),
                height: height.as_ref().map(|l| self.lp_non_negative(l)),
            },
        }
    }
}

/// The `font-size` keyword scale (CSS Fonts 4 §2.5, table of scaling
/// factors), relative to `medium`.
pub(crate) fn font_size_keyword(k: FontSizeKeyword, medium: f32, parent: f32) -> f32 {
    use FontSizeKeyword as K;
    match k {
        K::XxSmall => medium * 3.0 / 5.0,
        K::XSmall => medium * 3.0 / 4.0,
        K::Small => medium * 8.0 / 9.0,
        K::Medium => medium,
        K::Large => medium * 6.0 / 5.0,
        K::XLarge => medium * 3.0 / 2.0,
        K::XxLarge => medium * 2.0,
        K::XxxLarge => medium * 3.0,
        // §2.5: a ratio of 1.2 between adjacent sizes.
        K::Larger => parent * 1.2,
        K::Smaller => parent / 1.2,
    }
}

/// `bolder` / `lighter` (CSS Fonts 4 §2.2, relative weight table).
pub(crate) fn bolder(parent: f32) -> f32 {
    if parent < 350.0 {
        400.0
    } else if parent < 550.0 {
        700.0
    } else if parent < 900.0 {
        900.0
    } else {
        parent
    }
}

/// See [`bolder`].
pub(crate) fn lighter(parent: f32) -> f32 {
    if parent < 100.0 {
        parent
    } else if parent < 550.0 {
        100.0
    } else if parent < 750.0 {
        400.0
    } else {
        700.0
    }
}

/// Resolves a specified color; `current` is the element's `color`.
pub(crate) fn color(c: Color, current: Rgba) -> Rgba {
    match c {
        Color::CurrentColor => current,
        Color::Rgba(r) => r,
        // M1 follow-up: `color-scheme`; system colors use the light palette.
        Color::System(s) => s.to_rgba(false),
    }
}

/// Resolves `url` against `base` (CSS Values 4 §4.5.1: the computed value is
/// the absolute URL). Unresolvable URLs are kept as written.
pub(crate) fn resolve_url(url: &str, base: Option<&wb_url::Url>) -> String {
    match base.map(|b| b.join(url)) {
        Some(Ok(u)) => u.as_str().to_string(),
        _ => url.to_string(),
    }
}

pub(crate) fn image(i: &Image, base: Option<&wb_url::Url>) -> Image {
    match i {
        Image::Url(u) => Image::Url(resolve_url(u, base)),
        other => other.clone(),
    }
}

pub(crate) fn cursor(c: &CursorValue, base: Option<&wb_url::Url>) -> CursorValue {
    CursorValue {
        images: c
            .images
            .iter()
            .map(|i| CursorImage {
                url: resolve_url(&i.url, base),
                hotspot: i.hotspot,
            })
            .collect(),
        keyword: c.keyword,
    }
}

/// Computes `content`: `attr()` becomes a string from `attr_lookup`, and
/// `url()`s are resolved.
pub(crate) fn content(
    c: &Content,
    base: Option<&wb_url::Url>,
    attr_lookup: &dyn Fn(&str) -> String,
) -> Content {
    match c {
        Content::Items(items) => Content::Items(
            items
                .iter()
                .map(|item| match item {
                    ContentItem::Attr(name) => ContentItem::String(attr_lookup(name)),
                    ContentItem::Url(u) => ContentItem::Url(resolve_url(u, base)),
                    other => other.clone(),
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx() -> LengthContext {
        LengthContext {
            font_size: 20.0,
            root_font_size: 10.0,
            line_height: 30.0,
            root_line_height: 12.0,
            viewport_width: 1000.0,
            viewport_height: 500.0,
        }
    }

    fn len(value: f32, unit: LengthUnit) -> Length {
        Length { value, unit }
    }

    #[test]
    fn units() {
        let c = cx();
        assert_eq!(c.px(&len(2.0, LengthUnit::Em)), 40.0);
        assert_eq!(c.px(&len(2.0, LengthUnit::Rem)), 20.0);
        assert_eq!(c.px(&len(10.0, LengthUnit::Vw)), 100.0);
        assert_eq!(c.px(&len(10.0, LengthUnit::Vh)), 50.0);
        assert_eq!(c.px(&len(10.0, LengthUnit::Vmin)), 50.0);
        assert_eq!(c.px(&len(10.0, LengthUnit::Vmax)), 100.0);
        assert_eq!(c.px(&len(1.0, LengthUnit::In)), 96.0);
        assert_eq!(c.px(&len(12.0, LengthUnit::Pt)), 16.0);
        assert_eq!(c.px(&len(1.0, LengthUnit::Lh)), 30.0);
        assert_eq!(c.px(&len(2.0, LengthUnit::Ex)), 20.0);
        assert_eq!(c.px(&len(f32::INFINITY, LengthUnit::Px)), MAX_PX);
        assert_eq!(c.px(&len(f32::NAN, LengthUnit::Px)), 0.0);
    }

    #[test]
    fn weights() {
        assert_eq!(bolder(400.0), 700.0);
        assert_eq!(bolder(700.0), 900.0);
        assert_eq!(bolder(950.0), 950.0);
        assert_eq!(bolder(100.0), 400.0);
        assert_eq!(lighter(400.0), 100.0);
        assert_eq!(lighter(700.0), 400.0);
        assert_eq!(lighter(900.0), 700.0);
        assert_eq!(lighter(50.0), 50.0);
    }

    #[test]
    fn urls() {
        let base = wb_url::Url::parse("https://example.com/css/a.css").ok();
        assert_eq!(
            resolve_url("../img/x.png", base.as_ref()),
            "https://example.com/img/x.png"
        );
        assert_eq!(resolve_url("x.png", None), "x.png");
    }
}
