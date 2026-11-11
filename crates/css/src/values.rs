//! Basic value types: numbers, lengths, percentages, `calc()`, angles and
//! images ([CSS Values 4](https://drafts.csswg.org/css-values-4/),
//! [CSS Images 3](https://drafts.csswg.org/css-images-3/)).

use crate::cursor::Cursor;
use crate::syntax::{BlockKind, ComponentValue, Numeric};

keyword_enum! {
    /// A length unit (CSS Values 4 §6). The small/large/dynamic viewport
    /// units (`svw`, `lvh`, `dvh`, ...) are parsed as the plain viewport units
    /// because M1 has no dynamic browser UI.
    LengthUnit {
        Px = "px",
        Cm = "cm",
        Mm = "mm",
        Q = "q",
        In = "in",
        Pt = "pt",
        Pc = "pc",
        Em = "em",
        Rem = "rem",
        Ex = "ex",
        Rex = "rex",
        Cap = "cap",
        Ch = "ch",
        Rch = "rch",
        Ic = "ic",
        Ric = "ric",
        Lh = "lh",
        Rlh = "rlh",
        Vw = "vw",
        Vh = "vh",
        Vi = "vi",
        Vb = "vb",
        Vmin = "vmin",
        Vmax = "vmax",
    }
}

impl LengthUnit {
    fn from_unit(unit: &str) -> Option<Self> {
        if let Some(u) = Self::from_ident(unit) {
            return Some(u);
        }
        // §6.1.2.1: the s*, l* and d* viewport variants.
        let lower = unit.to_ascii_lowercase();
        let base = lower
            .strip_prefix('s')
            .or_else(|| lower.strip_prefix('l'))
            .or_else(|| lower.strip_prefix('d'))?;
        match base {
            "vw" | "vh" | "vi" | "vb" | "vmin" | "vmax" => Self::from_ident(base),
            _ => None,
        }
    }

    /// For absolute units, how many CSS pixels one unit is (§6.2).
    pub fn absolute_px(self) -> Option<f32> {
        Some(match self {
            LengthUnit::Px => 1.0,
            LengthUnit::Cm => 96.0 / 2.54,
            LengthUnit::Mm => 96.0 / 25.4,
            LengthUnit::Q => 96.0 / 101.6,
            LengthUnit::In => 96.0,
            LengthUnit::Pt => 96.0 / 72.0,
            LengthUnit::Pc => 16.0,
            _ => return None,
        })
    }
}

/// A `<length>`: a number and a unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Length {
    /// The number.
    pub value: f32,
    /// The unit.
    pub unit: LengthUnit,
}

impl Length {
    /// A length in CSS pixels.
    pub fn px(value: f32) -> Self {
        Length {
            value,
            unit: LengthUnit::Px,
        }
    }

    /// The length in CSS pixels if the unit is absolute.
    pub fn to_px(&self) -> Option<f32> {
        self.unit.absolute_px().map(|f| f * self.value)
    }
}

/// A `<length-percentage>`. Percentages are in percent units (`50%` is
/// `Percentage(50.0)`).
#[derive(Clone, Debug, PartialEq)]
pub enum LengthPercentage {
    /// A plain length.
    Length(Length),
    /// A percentage, in percent.
    Percentage(f32),
    /// A `calc()`-like expression whose type is length, percentage or both.
    Calc(Box<CalcNode>),
}

impl LengthPercentage {
    /// Zero pixels.
    pub fn zero() -> Self {
        LengthPercentage::Length(Length::px(0.0))
    }

    /// Resolves to pixels, given a way to resolve lengths and the basis that
    /// 100% refers to.
    pub fn resolve(&self, resolve_length: &dyn Fn(&Length) -> f32, percent_basis: f32) -> f32 {
        match self {
            LengthPercentage::Length(l) => resolve_length(l),
            LengthPercentage::Percentage(p) => p / 100.0 * percent_basis,
            LengthPercentage::Calc(c) => c.evaluate(resolve_length, percent_basis),
        }
    }
}

/// A math-function expression tree (CSS Values 4 §10), kept unresolved so
/// relative units and percentages can be resolved at computed/used-value time.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcNode {
    /// A plain number.
    Number(f32),
    /// A length.
    Length(Length),
    /// A percentage, in percent.
    Percentage(f32),
    /// The sum of the children.
    Sum(Vec<CalcNode>),
    /// The child negated (from `a - b`).
    Negate(Box<CalcNode>),
    /// The product of the children.
    Product(Vec<CalcNode>),
    /// One divided by the child (from `a / b`); the child is a number.
    Invert(Box<CalcNode>),
    /// `min(...)`.
    Min(Vec<CalcNode>),
    /// `max(...)`.
    Max(Vec<CalcNode>),
    /// `clamp(min, value, max)`.
    Clamp(Box<CalcNode>, Box<CalcNode>, Box<CalcNode>),
}

impl CalcNode {
    /// Evaluates the expression. `resolve_length` turns a length into pixels
    /// and percentages are taken of `percent_basis`. Per CSS Values 4 §10.9 a
    /// NaN result is returned as 0.
    pub fn evaluate(&self, resolve_length: &dyn Fn(&Length) -> f32, percent_basis: f32) -> f32 {
        let v = self.eval(resolve_length, percent_basis);
        if v.is_nan() { 0.0 } else { v }
    }

    fn eval(&self, rl: &dyn Fn(&Length) -> f32, basis: f32) -> f32 {
        match self {
            CalcNode::Number(n) => *n,
            CalcNode::Length(l) => rl(l),
            CalcNode::Percentage(p) => p / 100.0 * basis,
            CalcNode::Sum(v) => v.iter().map(|n| n.eval(rl, basis)).sum(),
            CalcNode::Negate(n) => -n.eval(rl, basis),
            CalcNode::Product(v) => v.iter().map(|n| n.eval(rl, basis)).product(),
            CalcNode::Invert(n) => 1.0 / n.eval(rl, basis),
            CalcNode::Min(v) => v
                .iter()
                .map(|n| n.eval(rl, basis))
                .fold(f32::INFINITY, f32::min),
            CalcNode::Max(v) => v
                .iter()
                .map(|n| n.eval(rl, basis))
                .fold(f32::NEG_INFINITY, f32::max),
            CalcNode::Clamp(min, val, max) => {
                let min = min.eval(rl, basis);
                let val = val.eval(rl, basis);
                let max = max.eval(rl, basis);
                // clamp(MIN, VAL, MAX) = max(MIN, min(VAL, MAX)).
                min.max(val.min(max))
            }
        }
    }

    /// The expression's type (§10.7), or `None` if it is not well-typed.
    fn calc_type(&self) -> Option<CalcType> {
        match self {
            CalcNode::Number(_) => Some(CalcType::Number),
            CalcNode::Length(_) => Some(CalcType::Length),
            CalcNode::Percentage(_) => Some(CalcType::Percentage),
            CalcNode::Negate(n) => n.calc_type(),
            CalcNode::Invert(n) => (n.calc_type()? == CalcType::Number).then_some(CalcType::Number),
            CalcNode::Sum(v) | CalcNode::Min(v) | CalcNode::Max(v) => {
                let mut t: Option<CalcType> = None;
                for n in v {
                    let nt = n.calc_type()?;
                    t = Some(match t {
                        None => nt,
                        Some(prev) => prev.add(nt)?,
                    });
                }
                t
            }
            CalcNode::Clamp(a, b, c) => a.calc_type()?.add(b.calc_type()?)?.add(c.calc_type()?),
            CalcNode::Product(v) => {
                let mut t = CalcType::Number;
                for n in v {
                    let nt = n.calc_type()?;
                    t = match (t, nt) {
                        (CalcType::Number, x) | (x, CalcType::Number) => x,
                        _ => return None,
                    };
                }
                Some(t)
            }
        }
    }

    /// The value if this tree has no lengths or percentages.
    fn constant(&self) -> Option<f32> {
        (self.calc_type()? == CalcType::Number).then(|| self.evaluate(&|_| 0.0, 0.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalcType {
    Number,
    Length,
    Percentage,
    LengthPercentage,
}

impl CalcType {
    fn add(self, other: CalcType) -> Option<CalcType> {
        use CalcType as T;
        Some(match (self, other) {
            (a, b) if a == b => a,
            (T::Number, _) | (_, T::Number) => return None,
            _ => T::LengthPercentage,
        })
    }
}

/// Which values a numeric property accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Range {
    All,
    NonNegative,
}

impl Range {
    fn ok(self, v: f32) -> bool {
        v.is_finite() && (self == Range::All || v >= 0.0)
    }
}

/// Whether `name` is a math function we parse.
fn is_math_function(name: &str) -> bool {
    ["calc", "min", "max", "clamp", "-webkit-calc"]
        .iter()
        .any(|f| name.eq_ignore_ascii_case(f))
}

/// Parses a math function (`calc()`, `min()`, `max()`, `clamp()`).
fn parse_math_function(c: &mut Cursor<'_>) -> Option<CalcNode> {
    c.attempt(|c| match c.next()? {
        ComponentValue::Function { name, arguments } if is_math_function(name) => {
            math_function_body(name, arguments)
        }
        _ => None,
    })
}

fn math_function_body(name: &str, arguments: &[ComponentValue]) -> Option<CalcNode> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "calc" | "-webkit-calc" => Cursor::parse_all(arguments, parse_sum),
        "min" | "max" | "clamp" => {
            let mut args = Vec::new();
            for part in crate::syntax::split_commas(arguments) {
                args.push(Cursor::parse_all(part, parse_sum)?);
            }
            match lower.as_str() {
                "min" if !args.is_empty() => Some(CalcNode::Min(args)),
                "max" if !args.is_empty() => Some(CalcNode::Max(args)),
                "clamp" => {
                    let mut it = args.into_iter();
                    let (a, b, c) = (it.next()?, it.next()?, it.next()?);
                    it.next()
                        .is_none()
                        .then(|| CalcNode::Clamp(Box::new(a), Box::new(b), Box::new(c)))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// `<calc-sum> = <calc-product> [ [ '+' | '-' ] <calc-product> ]*`, where the
/// operator must be surrounded by whitespace (§10.1).
fn parse_sum(c: &mut Cursor<'_>) -> Option<CalcNode> {
    let first = parse_product(c)?;
    let mut terms = vec![first];
    loop {
        let op = c.attempt(|c| {
            if !c.next_including_ws()?.is_whitespace() {
                return None;
            }
            let op = match c.next()? {
                ComponentValue::Delim('+') => '+',
                ComponentValue::Delim('-') => '-',
                _ => return None,
            };
            c.next_including_ws()?.is_whitespace().then_some(op)
        });
        let Some(op) = op else { break };
        let term = parse_product(c)?;
        terms.push(if op == '-' {
            CalcNode::Negate(Box::new(term))
        } else {
            term
        });
    }
    c.skip_ws();
    Some(if terms.len() == 1 {
        terms.pop()?
    } else {
        CalcNode::Sum(terms)
    })
}

/// `<calc-product> = <calc-value> [ [ '*' | '/' ] <calc-value> ]*`.
fn parse_product(c: &mut Cursor<'_>) -> Option<CalcNode> {
    let first = parse_calc_value(c)?;
    let mut factors = vec![first];
    loop {
        let op = c.attempt(|c| match c.next()? {
            ComponentValue::Delim('*') => Some('*'),
            ComponentValue::Delim('/') => Some('/'),
            _ => None,
        });
        let Some(op) = op else { break };
        let v = parse_calc_value(c)?;
        factors.push(if op == '/' {
            CalcNode::Invert(Box::new(v))
        } else {
            v
        });
    }
    Some(if factors.len() == 1 {
        factors.pop()?
    } else {
        CalcNode::Product(factors)
    })
}

fn parse_calc_value(c: &mut Cursor<'_>) -> Option<CalcNode> {
    match c.next()? {
        ComponentValue::Number(n) => Some(CalcNode::Number(n.value)),
        ComponentValue::Percentage(n) => Some(CalcNode::Percentage(n.value)),
        ComponentValue::Dimension(n, unit) => Some(CalcNode::Length(Length {
            value: n.value,
            unit: LengthUnit::from_unit(unit)?,
        })),
        ComponentValue::Block {
            kind: BlockKind::Parenthesis,
            contents,
        } => Cursor::parse_all(contents, parse_sum),
        ComponentValue::Function { name, arguments } if is_math_function(name) => {
            math_function_body(name, arguments)
        }
        ComponentValue::Ident(i) => {
            // §10.7.1 numeric constants.
            let lower = i.to_ascii_lowercase();
            Some(CalcNode::Number(match lower.as_str() {
                "e" => std::f32::consts::E,
                "pi" => std::f32::consts::PI,
                "infinity" => f32::INFINITY,
                "-infinity" => f32::NEG_INFINITY,
                "nan" => f32::NAN,
                _ => return None,
            }))
        }
        _ => None,
    }
}

/// Parses a `<number>` (including a `calc()` that reduces to a number).
pub(crate) fn parse_number(c: &mut Cursor<'_>, range: Range) -> Option<f32> {
    c.attempt(|c| {
        let v = match c.peek()? {
            ComponentValue::Number(n) => {
                c.next();
                n.value
            }
            _ => {
                let v = parse_math_function(c)?.constant()?;
                // Math functions clamp rather than reject (§10.12).
                return Some(if range == Range::NonNegative {
                    v.max(0.0)
                } else {
                    v
                });
            }
        };
        range.ok(v).then_some(v)
    })
}

/// Parses an `<integer>` (including a `calc()` that reduces to a number,
/// rounded to the nearest integer per §10.9).
pub(crate) fn parse_integer(c: &mut Cursor<'_>) -> Option<i32> {
    c.attempt(|c| match c.peek()? {
        ComponentValue::Number(Numeric {
            int_value: Some(i), ..
        }) => {
            c.next();
            Some(*i)
        }
        ComponentValue::Function { .. } => {
            let v = parse_math_function(c)?.constant()?;
            // §10.9: round to the nearest integer, ties toward +infinity.
            // `as` saturates, which is the clamping we want.
            Some((v + 0.5).floor() as i32)
        }
        _ => None,
    })
}

/// Parses a `<percentage>`, in percent.
pub(crate) fn parse_percentage(c: &mut Cursor<'_>, range: Range) -> Option<f32> {
    c.attempt(|c| match c.peek()? {
        ComponentValue::Percentage(n) => {
            c.next();
            range.ok(n.value).then_some(n.value)
        }
        ComponentValue::Function { .. } => {
            // A math function whose type is <percentage> (§10.7): evaluate it
            // in percent and clamp to the allowed range (§10.12).
            let node = parse_math_function(c)?;
            if node.calc_type()? != CalcType::Percentage {
                return None;
            }
            let v = node.evaluate(&|_| 0.0, 100.0);
            Some(if range == Range::NonNegative {
                v.max(0.0)
            } else {
                v
            })
        }
        _ => None,
    })
}

/// Parses a `<length-percentage>` (or a `<length>` when `allow_percent` is
/// false). A unitless zero is accepted as `0px`.
pub(crate) fn parse_length_percentage(
    c: &mut Cursor<'_>,
    range: Range,
    allow_percent: bool,
) -> Option<LengthPercentage> {
    c.attempt(|c| match c.peek()? {
        ComponentValue::Dimension(n, unit) => {
            let unit = LengthUnit::from_unit(unit)?;
            c.next();
            range
                .ok(n.value)
                .then_some(LengthPercentage::Length(Length {
                    value: n.value,
                    unit,
                }))
        }
        ComponentValue::Number(n) if n.value == 0.0 => {
            c.next();
            Some(LengthPercentage::zero())
        }
        ComponentValue::Percentage(n) if allow_percent => {
            c.next();
            range
                .ok(n.value)
                .then_some(LengthPercentage::Percentage(n.value))
        }
        ComponentValue::Function { .. } => {
            let node = parse_math_function(c)?;
            let ty = node.calc_type()?;
            let ok = match ty {
                CalcType::Length => true,
                CalcType::Percentage | CalcType::LengthPercentage => allow_percent,
                CalcType::Number => false,
            };
            if !ok {
                return None;
            }
            // Range checks on math functions happen at computed time.
            Some(match node {
                CalcNode::Length(l) => LengthPercentage::Length(l),
                CalcNode::Percentage(p) => LengthPercentage::Percentage(p),
                other => LengthPercentage::Calc(Box::new(other)),
            })
        }
        _ => None,
    })
}

/// Parses an `<angle>` and returns degrees. A unitless zero is accepted only
/// when `allow_zero` is true.
pub(crate) fn parse_angle(c: &mut Cursor<'_>, allow_zero: bool) -> Option<f32> {
    c.attempt(|c| match c.next()? {
        ComponentValue::Dimension(n, unit) => angle_to_degrees(n.value, unit),
        ComponentValue::Number(n) if allow_zero && n.value == 0.0 => Some(0.0),
        _ => None,
    })
}

pub(crate) fn angle_to_degrees(value: f32, unit: &str) -> Option<f32> {
    let unit = unit.to_ascii_lowercase();
    Some(match unit.as_str() {
        "deg" => value,
        "grad" => value * 0.9,
        "rad" => value.to_degrees(),
        "turn" => value * 360.0,
        _ => return None,
    })
}

/// Parses a `<string>`.
pub(crate) fn parse_string(c: &mut Cursor<'_>) -> Option<String> {
    c.attempt(|c| match c.next()? {
        ComponentValue::String(s) => Some(s.clone()),
        _ => None,
    })
}

/// Parses a `<url>`: `url(foo)` or `url("foo")` (also `src()`), returning
/// the URL text unresolved.
pub(crate) fn parse_url(c: &mut Cursor<'_>) -> Option<String> {
    c.attempt(|c| match c.next()? {
        ComponentValue::Url(u) => Some(u.clone()),
        ComponentValue::Function { name, arguments }
            if name.eq_ignore_ascii_case("url") || name.eq_ignore_ascii_case("src") =>
        {
            // URL modifiers are not supported; only a lone string.
            Cursor::parse_all(arguments, parse_string)
        }
        _ => None,
    })
}

/// An `<image>` (CSS Images 3 §3).
#[derive(Clone, Debug, PartialEq)]
pub enum Image {
    /// `none`.
    None,
    /// `url(...)`, the URL unresolved.
    Url(String),
    /// A gradient function (`linear-gradient()`, `radial-gradient()`,
    /// `conic-gradient()` and their `repeating-` forms). The arguments are
    /// kept unparsed for now; painters treat it as `none` until gradients are
    /// supported.
    // M1 follow-up: parse gradient arguments into a typed form.
    Gradient {
        /// The function name, lowercased.
        function: String,
        /// The unparsed arguments.
        arguments: Vec<ComponentValue>,
    },
}

/// Parses an `<image>` or `none`.
pub(crate) fn parse_image_or_none(c: &mut Cursor<'_>) -> Option<Image> {
    if c.eat_ident("none") {
        return Some(Image::None);
    }
    parse_image(c)
}

/// Parses an `<image>` (not `none`).
pub(crate) fn parse_image(c: &mut Cursor<'_>) -> Option<Image> {
    if let Some(u) = parse_url(c) {
        return Some(Image::Url(u));
    }
    c.attempt(|c| match c.next()? {
        ComponentValue::Function { name, arguments } => {
            let lower = name.to_ascii_lowercase();
            let mut base = lower.as_str();
            base = base.strip_prefix("-webkit-").unwrap_or(base);
            base = base.strip_prefix("repeating-").unwrap_or(base);
            matches!(
                base,
                "linear-gradient" | "radial-gradient" | "conic-gradient"
            )
            .then(|| Image::Gradient {
                function: lower.clone(),
                arguments: arguments.clone(),
            })
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::parse_component_values;

    fn lp(s: &str) -> Option<LengthPercentage> {
        let v = parse_component_values(s);
        Cursor::parse_all(&v, |c| parse_length_percentage(c, Range::All, true))
    }

    #[test]
    fn lengths() {
        assert_eq!(lp("10px"), Some(LengthPercentage::Length(Length::px(10.0))));
        assert_eq!(lp("0"), Some(LengthPercentage::zero()));
        assert_eq!(lp("5"), None);
        assert_eq!(lp("50%"), Some(LengthPercentage::Percentage(50.0)));
        assert!(matches!(
            lp("2SVH"),
            Some(LengthPercentage::Length(Length {
                unit: LengthUnit::Vh,
                ..
            }))
        ));
        assert_eq!(lp("3foo"), None);
    }

    #[test]
    fn calc_tree_and_eval() {
        let v = lp("calc(100% - 2 * 10px)").unwrap();
        let LengthPercentage::Calc(node) = &v else {
            panic!("{v:?}")
        };
        let r = node.evaluate(&|l| l.to_px().unwrap_or(0.0), 200.0);
        assert_eq!(r, 180.0);
        assert_eq!(
            lp("calc(10px)"),
            Some(LengthPercentage::Length(Length::px(10.0)))
        );
        assert_eq!(lp("calc(1px+2px)"), None);
        assert_eq!(lp("calc(1px * 2px)"), None);
        assert_eq!(lp("calc(1px + 2)"), None);
        assert_eq!(lp("calc(2)"), None);
        let v = lp("clamp(10px, 50%, 100px)").unwrap();
        assert_eq!(v.resolve(&|l| l.value, 1000.0), 100.0);
        let v = lp("min(10px, 5%, (3px))").unwrap();
        assert_eq!(v.resolve(&|l| l.value, 100.0), 3.0);
        let v = lp("max(1em / 2, 4px)").unwrap();
        assert_eq!(
            v.resolve(&|l| l.to_px().unwrap_or(l.value * 16.0), 0.0),
            8.0
        );
    }

    #[test]
    fn numbers() {
        let v = parse_component_values("calc(1 / 4 + pi * 0)");
        assert_eq!(
            Cursor::parse_all(&v, |c| parse_number(c, Range::All)),
            Some(0.25)
        );
        let v = parse_component_values("calc(2.6)");
        assert_eq!(Cursor::parse_all(&v, parse_integer), Some(3));
        let v = parse_component_values("-1");
        assert_eq!(
            Cursor::parse_all(&v, |c| parse_number(c, Range::NonNegative)),
            None
        );
    }

    #[test]
    fn integer_rounding_ties_toward_positive_infinity() {
        for (src, want) in [
            ("calc(-1.5)", -1),
            ("calc(1.5)", 2),
            ("calc(-2.5)", -2),
            ("calc(-1.6)", -2),
            ("calc(-1.4)", -1),
        ] {
            let v = parse_component_values(src);
            assert_eq!(Cursor::parse_all(&v, parse_integer), Some(want), "{src}");
        }
    }

    #[test]
    fn percentage_math_functions() {
        let pct = |s: &str, r| {
            let v = parse_component_values(s);
            Cursor::parse_all(&v, |c| parse_percentage(c, r))
        };
        assert_eq!(pct("calc(50%)", Range::All), Some(50.0));
        assert_eq!(pct("calc(100% + 20%)", Range::All), Some(120.0));
        assert_eq!(pct("calc(50% * 2)", Range::All), Some(100.0));
        assert_eq!(pct("calc(-10%)", Range::NonNegative), Some(0.0));
        assert_eq!(pct("calc(10px)", Range::All), None);
        assert_eq!(pct("calc(10% + 1px)", Range::All), None);
        assert_eq!(pct("-1%", Range::NonNegative), None);
    }

    #[test]
    fn images() {
        let v = parse_component_values("url( \"a b.png\" )");
        assert_eq!(
            Cursor::parse_all(&v, parse_image_or_none),
            Some(Image::Url("a b.png".into()))
        );
        let v = parse_component_values("repeating-linear-gradient(red, blue)");
        assert!(matches!(
            Cursor::parse_all(&v, parse_image_or_none),
            Some(Image::Gradient { .. })
        ));
    }
}
