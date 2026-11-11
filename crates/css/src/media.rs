//! Media queries ([Media Queries 4](https://drafts.csswg.org/mediaqueries-4/))
//! and their evaluation against a [`MediaContext`].

use crate::cursor::Cursor;
use crate::syntax::{BlockKind, ComponentValue, parse_component_values, split_commas, trim_ws};
use crate::values::{Length, LengthUnit};

/// The environment media queries are evaluated against.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaContext {
    /// The output medium.
    pub media_type: MediaType,
    /// Viewport width in CSS pixels.
    pub viewport_width: f32,
    /// Viewport height in CSS pixels.
    pub viewport_height: f32,
    /// The initial font size in CSS pixels, for `em`/`rem`/`ex`/`ch` in
    /// media queries (§1.3: relative units use the initial value).
    pub font_size: f32,
    /// Device pixels per CSS pixel.
    pub resolution_dppx: f32,
    /// The user's preferred color scheme.
    pub prefers_color_scheme: ColorScheme,
    /// Whether the user asked for reduced motion.
    pub prefers_reduced_motion: bool,
    /// Bits per color component of the output device (0 if monochrome).
    pub color_bits: u32,
    /// Whether the primary pointing device can hover.
    pub hover: bool,
    /// The accuracy of the primary pointing device.
    pub pointer: Pointer,
}

impl Default for MediaContext {
    fn default() -> Self {
        MediaContext {
            media_type: MediaType::Screen,
            viewport_width: 1280.0,
            viewport_height: 720.0,
            font_size: 16.0,
            resolution_dppx: 1.0,
            prefers_color_scheme: ColorScheme::Light,
            prefers_reduced_motion: false,
            color_bits: 8,
            hover: true,
            pointer: Pointer::Fine,
        }
    }
}

/// A color scheme preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorScheme {
    /// `light`
    Light,
    /// `dark`
    Dark,
}

/// Pointer accuracy for the `pointer` feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pointer {
    /// `none`
    None,
    /// `coarse`
    Coarse,
    /// `fine`
    Fine,
}

/// A media type (§2.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MediaType {
    /// `all`
    All,
    /// `screen`
    Screen,
    /// `print`
    Print,
    /// Any other identifier (including the deprecated CSS2 types); never
    /// matches.
    Other(String),
}

/// A media query list. An empty list matches everything.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaQueryList(pub Vec<MediaQuery>);

/// A single media query.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaQuery {
    /// `not` before the media type: negates the whole query.
    pub negated: bool,
    /// The media type (`all` when only a condition was given).
    pub media_type: MediaType,
    /// The condition after the type, or the whole query if no type.
    pub condition: Option<MediaCondition>,
}

impl MediaQuery {
    /// `not all`, which is what an unparseable query becomes (§3.2).
    pub fn not_all() -> Self {
        MediaQuery {
            negated: true,
            media_type: MediaType::All,
            condition: None,
        }
    }
}

/// A media condition (§3).
#[derive(Clone, Debug, PartialEq)]
pub enum MediaCondition {
    /// A media feature test.
    Feature(MediaFeature),
    /// `not <condition>`.
    Not(Box<MediaCondition>),
    /// Conditions joined with `and`.
    And(Vec<MediaCondition>),
    /// Conditions joined with `or`.
    Or(Vec<MediaCondition>),
    /// `<general-enclosed>`: syntax we do not understand (an unknown feature,
    /// a function, ...). Evaluates to "unknown".
    Unknown,
}

/// The media features we evaluate (§4 and Media Queries 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeatureName {
    /// `width`
    Width,
    /// `height`
    Height,
    /// `aspect-ratio`
    AspectRatio,
    /// `orientation`
    Orientation,
    /// `resolution`
    Resolution,
    /// `color`
    Color,
    /// `monochrome`
    Monochrome,
    /// `hover` and `any-hover`
    Hover,
    /// `pointer` and `any-pointer`
    Pointer,
    /// `prefers-color-scheme`
    PrefersColorScheme,
    /// `prefers-reduced-motion`
    PrefersReducedMotion,
    /// `device-width` (deprecated; evaluated as the viewport width)
    DeviceWidth,
    /// `device-height` (deprecated; evaluated as the viewport height)
    DeviceHeight,
    /// `grid`
    Grid,
}

impl FeatureName {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "width" => FeatureName::Width,
            "height" => FeatureName::Height,
            "aspect-ratio" => FeatureName::AspectRatio,
            "orientation" => FeatureName::Orientation,
            "resolution" => FeatureName::Resolution,
            "color" => FeatureName::Color,
            "monochrome" => FeatureName::Monochrome,
            "hover" | "any-hover" => FeatureName::Hover,
            "pointer" | "any-pointer" => FeatureName::Pointer,
            "prefers-color-scheme" => FeatureName::PrefersColorScheme,
            "prefers-reduced-motion" => FeatureName::PrefersReducedMotion,
            "device-width" => FeatureName::DeviceWidth,
            "device-height" => FeatureName::DeviceHeight,
            "grid" => FeatureName::Grid,
            _ => return None,
        })
    }

    /// Range features accept `min-`/`max-` and comparisons.
    fn is_range(self) -> bool {
        matches!(
            self,
            FeatureName::Width
                | FeatureName::Height
                | FeatureName::AspectRatio
                | FeatureName::Resolution
                | FeatureName::Color
                | FeatureName::Monochrome
                | FeatureName::DeviceWidth
                | FeatureName::DeviceHeight
        )
    }
}

/// A comparison in a range context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Comparison {
    /// `=`
    Equal,
    /// `<`
    LessThan,
    /// `<=`
    LessOrEqual,
    /// `>`
    GreaterThan,
    /// `>=`
    GreaterOrEqual,
}

impl Comparison {
    /// `a op b` becomes `b flip(op) a`.
    fn flip(self) -> Comparison {
        match self {
            Comparison::Equal => Comparison::Equal,
            Comparison::LessThan => Comparison::GreaterThan,
            Comparison::LessOrEqual => Comparison::GreaterOrEqual,
            Comparison::GreaterThan => Comparison::LessThan,
            Comparison::GreaterOrEqual => Comparison::LessOrEqual,
        }
    }

    fn test(self, a: f32, b: f32) -> bool {
        match self {
            Comparison::Equal => a == b,
            Comparison::LessThan => a < b,
            Comparison::LessOrEqual => a <= b,
            Comparison::GreaterThan => a > b,
            Comparison::GreaterOrEqual => a >= b,
        }
    }
}

/// A value in a media feature.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaValue {
    /// A number (also integers).
    Number(f32),
    /// A length.
    Length(Length),
    /// A ratio `a / b` (a lone number `a` is `a / 1`).
    Ratio(f32, f32),
    /// A resolution in dots per CSS pixel.
    Resolution(f32),
    /// A keyword, lowercased.
    Ident(String),
}

/// A media feature test (§4).
#[derive(Clone, Debug, PartialEq)]
pub enum MediaFeature {
    /// `(name)`: true if the feature is not zero/none.
    Boolean(FeatureName),
    /// `(name: value)` for discrete features.
    Equals(FeatureName, MediaValue),
    /// A range test, normalized so the feature is on the left of each
    /// comparison: `(400px <= width < 700px)` becomes
    /// `[(GreaterOrEqual, 400px), (LessThan, 700px)]`, and `(min-width: 5px)`
    /// becomes `[(GreaterOrEqual, 5px)]`.
    Range(FeatureName, Vec<(Comparison, MediaValue)>),
}

/// Parses a media query list from a string (e.g. a `media` attribute).
pub fn parse_media_query_list(input: &str) -> MediaQueryList {
    parse_media_query_list_tokens(&parse_component_values(input))
}

/// Parses a media query list from component values (§3.2). Each query that
/// fails to parse becomes `not all`.
pub(crate) fn parse_media_query_list_tokens(values: &[ComponentValue]) -> MediaQueryList {
    if trim_ws(values).is_empty() {
        return MediaQueryList(Vec::new());
    }
    MediaQueryList(
        split_commas(values)
            .into_iter()
            .map(|q| Cursor::parse_all(q, parse_media_query).unwrap_or_else(MediaQuery::not_all))
            .collect(),
    )
}

fn parse_media_query(c: &mut Cursor<'_>) -> Option<MediaQuery> {
    // `<media-condition>` alone?
    if let Some(cond) = c.attempt(|c| {
        let cond = parse_condition(c, true)?;
        c.is_exhausted().then_some(cond)
    }) {
        return Some(MediaQuery {
            negated: false,
            media_type: MediaType::All,
            condition: Some(cond),
        });
    }
    let mut negated = false;
    if c.eat_ident("not") {
        negated = true;
    } else {
        let _ = c.eat_ident("only");
    }
    let ty = c.ident()?.to_ascii_lowercase();
    let media_type = match ty.as_str() {
        "all" => MediaType::All,
        "screen" => MediaType::Screen,
        "print" => MediaType::Print,
        "only" | "not" | "and" | "or" | "layer" => return None,
        _ => MediaType::Other(ty),
    };
    let condition = if c.eat_ident("and") {
        Some(parse_condition(c, false)?)
    } else {
        None
    };
    Some(MediaQuery {
        negated,
        media_type,
        condition,
    })
}

/// `<media-condition>` (or `<media-condition-without-or>`).
fn parse_condition(c: &mut Cursor<'_>, allow_or: bool) -> Option<MediaCondition> {
    if c.eat_ident("not") {
        return Some(MediaCondition::Not(Box::new(parse_in_parens(c)?)));
    }
    let first = parse_in_parens(c)?;
    let mut items = vec![first];
    let mut joiner: Option<&str> = None;
    loop {
        let word = if c.eat_ident("and") {
            "and"
        } else if allow_or && c.eat_ident("or") {
            "or"
        } else {
            break;
        };
        if joiner.is_some_and(|j| j != word) {
            // Mixing `and` and `or` without parentheses is invalid.
            return None;
        }
        joiner = Some(word);
        items.push(parse_in_parens(c)?);
    }
    Some(match joiner {
        None => items.pop()?,
        Some("and") => MediaCondition::And(items),
        Some(_) => MediaCondition::Or(items),
    })
}

/// `<media-in-parens>`: `( condition )`, a feature, or general-enclosed.
fn parse_in_parens(c: &mut Cursor<'_>) -> Option<MediaCondition> {
    match c.next()? {
        ComponentValue::Block {
            kind: BlockKind::Parenthesis,
            contents,
        } => {
            if let Some(cond) = Cursor::parse_all(contents, |c| parse_condition(c, true)) {
                return Some(cond);
            }
            if let Some(f) = Cursor::parse_all(contents, parse_feature) {
                return Some(MediaCondition::Feature(f));
            }
            Some(MediaCondition::Unknown)
        }
        ComponentValue::Function { .. } => Some(MediaCondition::Unknown),
        _ => None,
    }
}

fn parse_feature(c: &mut Cursor<'_>) -> Option<MediaFeature> {
    // `name`, `name: value`, `name op value`.
    if let Some(f) = c.attempt(|c| {
        let raw = c.ident()?.to_ascii_lowercase();
        if c.is_exhausted() {
            return Some(MediaFeature::Boolean(FeatureName::from_name(&raw)?));
        }
        if matches!(c.peek(), Some(ComponentValue::Colon)) {
            c.next();
            let (prefix, base) = if let Some(b) = raw.strip_prefix("min-") {
                (Some(Comparison::GreaterOrEqual), b)
            } else if let Some(b) = raw.strip_prefix("max-") {
                (Some(Comparison::LessOrEqual), b)
            } else {
                (None, raw.as_str())
            };
            let name = FeatureName::from_name(base)?;
            let value = parse_value(c, name)?;
            return Some(match prefix {
                Some(op) if name.is_range() => MediaFeature::Range(name, vec![(op, value)]),
                Some(_) => return None,
                None if name.is_range() => {
                    MediaFeature::Range(name, vec![(Comparison::Equal, value)])
                }
                None => MediaFeature::Equals(name, value),
            });
        }
        let name = FeatureName::from_name(&raw)?;
        if !name.is_range() {
            return None;
        }
        let op = parse_comparison(c)?;
        let value = parse_value(c, name)?;
        Some(MediaFeature::Range(name, vec![(op, value)]))
    }) {
        return Some(f);
    }
    // `value op name [op value]`.
    let first = parse_any_value(c)?;
    let op1 = parse_comparison(c)?;
    let name = FeatureName::from_name(&c.ident()?.to_ascii_lowercase())?;
    if !name.is_range() {
        return None;
    }
    let first = coerce_value(first, name)?;
    let mut tests = vec![(op1.flip(), first)];
    if let Some(op2) = c.attempt(parse_comparison) {
        // Both comparisons must point the same way (§4.2).
        let same_dir = |a: Comparison, b: Comparison| {
            let lt = |x| matches!(x, Comparison::LessThan | Comparison::LessOrEqual);
            let gt = |x| matches!(x, Comparison::GreaterThan | Comparison::GreaterOrEqual);
            (lt(a) && lt(b)) || (gt(a) && gt(b))
        };
        if !same_dir(op1, op2) {
            return None;
        }
        let second = parse_value(c, name)?;
        tests.push((op2, second));
    }
    Some(MediaFeature::Range(name, tests))
}

fn parse_comparison(c: &mut Cursor<'_>) -> Option<Comparison> {
    c.attempt(|c| {
        let first = match c.next()? {
            ComponentValue::Delim(d @ ('<' | '>' | '=')) => *d,
            _ => return None,
        };
        // `<=` and `>=` must be adjacent.
        let eq = first != '='
            && c.attempt(|c| match c.next_including_ws()? {
                ComponentValue::Delim('=') => Some(()),
                _ => None,
            })
            .is_some();
        Some(match (first, eq) {
            ('<', false) => Comparison::LessThan,
            ('<', true) => Comparison::LessOrEqual,
            ('>', false) => Comparison::GreaterThan,
            ('>', true) => Comparison::GreaterOrEqual,
            _ => Comparison::Equal,
        })
    })
}

/// Any feature value, before we know the feature.
fn parse_any_value(c: &mut Cursor<'_>) -> Option<MediaValue> {
    c.attempt(|c| match c.next()? {
        ComponentValue::Number(n) => {
            let ratio = c.attempt(|c| {
                if !c.eat_delim('/') {
                    return None;
                }
                match c.next()? {
                    ComponentValue::Number(d) => Some(d.value),
                    _ => None,
                }
            });
            Some(match ratio {
                Some(d) => MediaValue::Ratio(n.value, d),
                None => MediaValue::Number(n.value),
            })
        }
        ComponentValue::Dimension(n, unit) => {
            let lower = unit.to_ascii_lowercase();
            if let Some(u) = LengthUnit::from_ident(&lower) {
                return Some(MediaValue::Length(Length {
                    value: n.value,
                    unit: u,
                }));
            }
            let dppx = match lower.as_str() {
                "dppx" | "x" => n.value,
                "dpi" => n.value / 96.0,
                "dpcm" => n.value * 2.54 / 96.0,
                _ => return None,
            };
            Some(MediaValue::Resolution(dppx))
        }
        ComponentValue::Ident(i) => Some(MediaValue::Ident(i.to_ascii_lowercase())),
        _ => None,
    })
}

/// Checks a value against the feature's value type.
fn coerce_value(v: MediaValue, name: FeatureName) -> Option<MediaValue> {
    use FeatureName as F;
    match (name, v) {
        (F::Width | F::Height | F::DeviceWidth | F::DeviceHeight, MediaValue::Length(l)) => {
            Some(MediaValue::Length(l))
        }
        (F::Width | F::Height | F::DeviceWidth | F::DeviceHeight, MediaValue::Number(0.0)) => {
            Some(MediaValue::Length(Length::px(0.0)))
        }
        (F::AspectRatio, MediaValue::Ratio(a, b)) => Some(MediaValue::Ratio(a, b)),
        (F::AspectRatio, MediaValue::Number(n)) => Some(MediaValue::Ratio(n, 1.0)),
        (F::Resolution, MediaValue::Resolution(r)) => Some(MediaValue::Resolution(r)),
        (F::Color | F::Monochrome | F::Grid, MediaValue::Number(n)) if n.fract() == 0.0 => {
            Some(MediaValue::Number(n))
        }
        (F::Orientation, MediaValue::Ident(i)) if i == "portrait" || i == "landscape" => {
            Some(MediaValue::Ident(i))
        }
        (F::PrefersColorScheme, MediaValue::Ident(i)) if i == "light" || i == "dark" => {
            Some(MediaValue::Ident(i))
        }
        (F::PrefersReducedMotion, MediaValue::Ident(i))
            if i == "no-preference" || i == "reduce" =>
        {
            Some(MediaValue::Ident(i))
        }
        (F::Hover, MediaValue::Ident(i)) if i == "none" || i == "hover" => {
            Some(MediaValue::Ident(i))
        }
        (F::Pointer, MediaValue::Ident(i)) if i == "none" || i == "coarse" || i == "fine" => {
            Some(MediaValue::Ident(i))
        }
        _ => None,
    }
}

fn parse_value(c: &mut Cursor<'_>, name: FeatureName) -> Option<MediaValue> {
    c.attempt(|c| coerce_value(parse_any_value(c)?, name))
}

/// Three-valued logic for media query evaluation (§3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kleene {
    True,
    False,
    Unknown,
}

impl Kleene {
    fn from_bool(b: bool) -> Self {
        if b { Kleene::True } else { Kleene::False }
    }

    fn not(self) -> Self {
        match self {
            Kleene::True => Kleene::False,
            Kleene::False => Kleene::True,
            Kleene::Unknown => Kleene::Unknown,
        }
    }
}

impl MediaQueryList {
    /// Whether the list matches: empty, or any query matches.
    pub fn matches(&self, ctx: &MediaContext) -> bool {
        self.0.is_empty() || self.0.iter().any(|q| q.matches(ctx))
    }
}

impl MediaQuery {
    /// Whether this query matches. An "unknown" result counts as false.
    pub fn matches(&self, ctx: &MediaContext) -> bool {
        let type_ok = match &self.media_type {
            MediaType::All => true,
            MediaType::Other(_) => false,
            t => *t == ctx.media_type,
        };
        let r = if !type_ok {
            Kleene::False
        } else {
            match &self.condition {
                None => Kleene::True,
                Some(c) => c.eval(ctx),
            }
        };
        let r = if self.negated { r.not() } else { r };
        r == Kleene::True
    }
}

impl MediaCondition {
    fn eval(&self, ctx: &MediaContext) -> Kleene {
        match self {
            MediaCondition::Feature(f) => Kleene::from_bool(f.eval(ctx)),
            MediaCondition::Not(c) => c.eval(ctx).not(),
            MediaCondition::And(v) => {
                let mut r = Kleene::True;
                for c in v {
                    match c.eval(ctx) {
                        Kleene::False => return Kleene::False,
                        Kleene::Unknown => r = Kleene::Unknown,
                        Kleene::True => {}
                    }
                }
                r
            }
            MediaCondition::Or(v) => {
                let mut r = Kleene::False;
                for c in v {
                    match c.eval(ctx) {
                        Kleene::True => return Kleene::True,
                        Kleene::Unknown => r = Kleene::Unknown,
                        Kleene::False => {}
                    }
                }
                r
            }
            MediaCondition::Unknown => Kleene::Unknown,
        }
    }
}

fn length_px(l: &Length, ctx: &MediaContext) -> f32 {
    if let Some(px) = l.to_px() {
        return px;
    }
    let em = ctx.font_size;
    let vw = ctx.viewport_width / 100.0;
    let vh = ctx.viewport_height / 100.0;
    l.value
        * match l.unit {
            LengthUnit::Em | LengthUnit::Rem | LengthUnit::Lh | LengthUnit::Rlh => em,
            LengthUnit::Ic | LengthUnit::Ric => em,
            LengthUnit::Ex | LengthUnit::Rex | LengthUnit::Ch | LengthUnit::Rch => em / 2.0,
            LengthUnit::Cap => em * 0.7,
            LengthUnit::Vw | LengthUnit::Vi => vw,
            LengthUnit::Vh | LengthUnit::Vb => vh,
            LengthUnit::Vmin => vw.min(vh),
            LengthUnit::Vmax => vw.max(vh),
            _ => 1.0,
        }
}

impl MediaFeature {
    fn eval(&self, ctx: &MediaContext) -> bool {
        let actual = |name: FeatureName| -> MediaValue {
            match name {
                FeatureName::Width | FeatureName::DeviceWidth => {
                    MediaValue::Length(Length::px(ctx.viewport_width))
                }
                FeatureName::Height | FeatureName::DeviceHeight => {
                    MediaValue::Length(Length::px(ctx.viewport_height))
                }
                FeatureName::AspectRatio => {
                    MediaValue::Ratio(ctx.viewport_width, ctx.viewport_height)
                }
                FeatureName::Orientation => MediaValue::Ident(
                    if ctx.viewport_height >= ctx.viewport_width {
                        "portrait"
                    } else {
                        "landscape"
                    }
                    .into(),
                ),
                FeatureName::Resolution => MediaValue::Resolution(ctx.resolution_dppx),
                FeatureName::Color => MediaValue::Number(ctx.color_bits as f32),
                FeatureName::Monochrome => {
                    MediaValue::Number(if ctx.color_bits == 0 { 1.0 } else { 0.0 })
                }
                FeatureName::Grid => MediaValue::Number(0.0),
                FeatureName::Hover => {
                    MediaValue::Ident(if ctx.hover { "hover" } else { "none" }.into())
                }
                FeatureName::Pointer => MediaValue::Ident(
                    match ctx.pointer {
                        Pointer::None => "none",
                        Pointer::Coarse => "coarse",
                        Pointer::Fine => "fine",
                    }
                    .into(),
                ),
                FeatureName::PrefersColorScheme => MediaValue::Ident(
                    match ctx.prefers_color_scheme {
                        ColorScheme::Light => "light",
                        ColorScheme::Dark => "dark",
                    }
                    .into(),
                ),
                FeatureName::PrefersReducedMotion => MediaValue::Ident(
                    if ctx.prefers_reduced_motion {
                        "reduce"
                    } else {
                        "no-preference"
                    }
                    .into(),
                ),
            }
        };
        let num = |v: &MediaValue| -> Option<f32> {
            match v {
                MediaValue::Number(n) | MediaValue::Resolution(n) => Some(*n),
                MediaValue::Length(l) => Some(length_px(l, ctx)),
                MediaValue::Ratio(a, b) => Some(if *b == 0.0 { f32::INFINITY } else { a / b }),
                MediaValue::Ident(_) => None,
            }
        };
        match self {
            MediaFeature::Boolean(name) => match actual(*name) {
                MediaValue::Ident(i) => i != "none" && i != "no-preference",
                v => num(&v).is_some_and(|n| n != 0.0),
            },
            MediaFeature::Equals(name, want) => actual(*name) == *want,
            MediaFeature::Range(name, tests) => {
                let a = num(&actual(*name));
                tests.iter().all(|(op, v)| match (a, num(v)) {
                    (Some(a), Some(b)) => op.test(a, b),
                    _ => false,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(q: &str, ctx: &MediaContext) -> bool {
        parse_media_query_list(q).matches(ctx)
    }

    #[test]
    fn media_types() {
        let screen = MediaContext::default();
        let print = MediaContext {
            media_type: MediaType::Print,
            ..MediaContext::default()
        };
        assert!(m("", &screen));
        assert!(m("all", &screen));
        assert!(m("screen", &screen));
        assert!(!m("print", &screen));
        assert!(m("print", &print));
        assert!(m("only screen", &screen));
        assert!(m("not print", &screen));
        assert!(!m("not screen", &screen));
        assert!(m("print, screen", &screen));
        assert!(!m("tv", &screen));
        assert!(m("not tv", &screen));
        assert!(m("SCREEN", &screen));
    }

    #[test]
    fn widths_and_ranges() {
        let ctx = MediaContext {
            viewport_width: 800.0,
            viewport_height: 600.0,
            ..MediaContext::default()
        };
        assert!(m("(min-width: 600px)", &ctx));
        assert!(!m("(min-width: 900px)", &ctx));
        assert!(m("(max-width: 50em)", &ctx));
        assert!(m(
            "screen and (min-width: 600px) and (max-width: 800px)",
            &ctx
        ));
        assert!(m("(width >= 800px)", &ctx));
        assert!(!m("(width > 800px)", &ctx));
        assert!(m("(400px <= width <= 800px)", &ctx));
        assert!(!m("(400px <= width < 800px)", &ctx));
        assert!(m("(600px < width)", &ctx));
        assert!(m("(height: 600px)", &ctx));
        assert!(m("(orientation: landscape)", &ctx));
        assert!(!m("(orientation: portrait)", &ctx));
        assert!(m("(min-aspect-ratio: 4/3)", &ctx));
        assert!(m("(aspect-ratio: 4 / 3)", &ctx));
        assert!(m("(width)", &ctx));
        assert!(m("(not (width < 100px))", &ctx));
        assert!(m("(width < 100px) or (height > 100px)", &ctx));
        assert!(!m("(400px >= width <= 800px)", &ctx));
    }

    #[test]
    fn preferences() {
        let dark = MediaContext {
            prefers_color_scheme: ColorScheme::Dark,
            ..MediaContext::default()
        };
        assert!(m("(prefers-color-scheme: dark)", &dark));
        assert!(!m("(prefers-color-scheme: light)", &dark));
        assert!(m("(prefers-reduced-motion: no-preference)", &dark));
        assert!(m("(hover: hover) and (pointer: fine)", &dark));
        assert!(m("(min-resolution: 1dppx)", &dark));
        assert!(m("(resolution: 96dpi)", &dark));
        assert!(m("(color)", &dark));
    }

    #[test]
    fn unknown_and_errors() {
        let ctx = MediaContext::default();
        // Unknown features are "unknown", so neither they nor their negation match.
        assert!(!m("(foo: bar)", &ctx));
        assert!(!m("not (foo: bar)", &ctx));
        assert!(m("(foo: bar) or (width > 0)", &ctx));
        // A malformed query becomes `not all`, other queries still count.
        let l = parse_media_query_list("screen and, print");
        assert_eq!(l.0[0], MediaQuery::not_all());
        assert!(!l.matches(&ctx));
        assert!(m("garbage!!, screen", &ctx));
        assert!(!m("(width > 0) and (height > 0) or (color)", &ctx));
        assert!(!m("only", &ctx));
        assert!(!m("(width: red)", &ctx));
    }
}
