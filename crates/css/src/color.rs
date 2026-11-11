//! Colors ([CSS Color 4](https://drafts.csswg.org/css-color-4/)).

use crate::cursor::Cursor;
use crate::syntax::{ComponentValue, split_commas};
use crate::values::{Range, angle_to_degrees, parse_number, parse_percentage};

/// An sRGB color with 8-bit channels and 8-bit alpha (255 is opaque).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgba {
    /// Red, 0-255.
    pub red: u8,
    /// Green, 0-255.
    pub green: u8,
    /// Blue, 0-255.
    pub blue: u8,
    /// Alpha, 0 (transparent) to 255 (opaque).
    pub alpha: u8,
}

impl Rgba {
    /// An opaque color.
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Rgba {
            red,
            green,
            blue,
            alpha: 255,
        }
    }

    /// Fully transparent black.
    pub const TRANSPARENT: Rgba = Rgba {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 0,
    };

    /// From channels in 0..=1 (clipped to the sRGB gamut).
    fn from_unit(r: f32, g: f32, b: f32, a: f32) -> Self {
        Rgba {
            red: unit_to_u8(r),
            green: unit_to_u8(g),
            blue: unit_to_u8(b),
            alpha: unit_to_u8(a),
        }
    }
}

fn unit_to_u8(v: f32) -> u8 {
    if v.is_nan() {
        return 0;
    }
    // `as` saturates; the clamp keeps rounding well-defined.
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A specified `<color>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// `currentcolor`: the element's `color` value.
    CurrentColor,
    /// A concrete color (named, hex, functional, or `transparent`).
    Rgba(Rgba),
    /// A system color keyword; resolve with [`SystemColor::to_rgba`].
    System(SystemColor),
}

impl Color {
    /// `transparent`.
    pub const TRANSPARENT: Color = Color::Rgba(Rgba::TRANSPARENT);
}

keyword_enum! {
    /// A system color (CSS Color 4 §6.2), including the deprecated ones,
    /// which map to their replacements in [`SystemColor::to_rgba`].
    SystemColor {
        AccentColor = "accentcolor",
        AccentColorText = "accentcolortext",
        ActiveText = "activetext",
        ButtonBorder = "buttonborder",
        ButtonFace = "buttonface",
        ButtonText = "buttontext",
        Canvas = "canvas",
        CanvasText = "canvastext",
        Field = "field",
        FieldText = "fieldtext",
        GrayText = "graytext",
        Highlight = "highlight",
        HighlightText = "highlighttext",
        LinkText = "linktext",
        Mark = "mark",
        MarkText = "marktext",
        SelectedItem = "selecteditem",
        SelectedItemText = "selecteditemtext",
        VisitedText = "visitedtext",
        ActiveBorder = "activeborder",
        ActiveCaption = "activecaption",
        AppWorkspace = "appworkspace",
        Background = "background",
        ButtonHighlight = "buttonhighlight",
        ButtonShadow = "buttonshadow",
        CaptionText = "captiontext",
        InactiveBorder = "inactiveborder",
        InactiveCaption = "inactivecaption",
        InactiveCaptionText = "inactivecaptiontext",
        InfoBackground = "infobackground",
        InfoText = "infotext",
        Menu = "menu",
        MenuText = "menutext",
        Scrollbar = "scrollbar",
        ThreeDDarkShadow = "threeddarkshadow",
        ThreeDFace = "threedface",
        ThreeDHighlight = "threedhighlight",
        ThreeDLightShadow = "threedlightshadow",
        ThreeDShadow = "threedshadow",
        Window = "window",
        WindowFrame = "windowframe",
        WindowText = "windowtext",
    }
}

impl SystemColor {
    /// A fallback value for the light or dark color scheme. The spec leaves
    /// these to the platform; these are common defaults.
    pub fn to_rgba(self, dark: bool) -> Rgba {
        use SystemColor as S;
        let (light, dark_v) = match self {
            S::Canvas | S::Field | S::Window | S::Background | S::InfoBackground => {
                (Rgba::rgb(255, 255, 255), Rgba::rgb(18, 18, 18))
            }
            S::CanvasText
            | S::FieldText
            | S::ButtonText
            | S::WindowText
            | S::CaptionText
            | S::InfoText
            | S::MenuText
            | S::InactiveCaptionText => (Rgba::rgb(0, 0, 0), Rgba::rgb(255, 255, 255)),
            S::LinkText => (Rgba::rgb(0, 0, 238), Rgba::rgb(158, 158, 255)),
            S::VisitedText => (Rgba::rgb(85, 26, 139), Rgba::rgb(208, 173, 240)),
            S::ActiveText => (Rgba::rgb(238, 0, 0), Rgba::rgb(255, 102, 102)),
            S::ButtonFace
            | S::ButtonHighlight
            | S::ButtonShadow
            | S::ThreeDFace
            | S::ThreeDHighlight
            | S::ThreeDLightShadow
            | S::Menu
            | S::Scrollbar
            | S::AppWorkspace
            | S::ActiveCaption
            | S::InactiveCaption => (Rgba::rgb(240, 240, 240), Rgba::rgb(59, 59, 59)),
            S::ButtonBorder
            | S::ActiveBorder
            | S::InactiveBorder
            | S::ThreeDDarkShadow
            | S::ThreeDShadow
            | S::WindowFrame => (Rgba::rgb(118, 118, 118), Rgba::rgb(133, 133, 133)),
            S::GrayText => (Rgba::rgb(109, 109, 109), Rgba::rgb(168, 168, 168)),
            S::Highlight | S::SelectedItem | S::AccentColor => {
                (Rgba::rgb(0, 117, 255), Rgba::rgb(153, 200, 255))
            }
            S::HighlightText | S::SelectedItemText | S::AccentColorText => {
                (Rgba::rgb(255, 255, 255), Rgba::rgb(0, 0, 0))
            }
            S::Mark => (Rgba::rgb(255, 255, 0), Rgba::rgb(255, 255, 0)),
            S::MarkText => (Rgba::rgb(0, 0, 0), Rgba::rgb(0, 0, 0)),
        };
        if dark { dark_v } else { light }
    }
}

/// Parses a `<color>`.
pub(crate) fn parse_color(c: &mut Cursor<'_>) -> Option<Color> {
    c.attempt(|c| match c.next()? {
        ComponentValue::Ident(name) => {
            if name.eq_ignore_ascii_case("currentcolor") {
                Some(Color::CurrentColor)
            } else if name.eq_ignore_ascii_case("transparent") {
                Some(Color::TRANSPARENT)
            } else if let Some(s) = SystemColor::from_ident(name) {
                Some(Color::System(s))
            } else {
                named_color(name).map(Color::Rgba)
            }
        }
        ComponentValue::Hash { value, .. } => parse_hex(value).map(Color::Rgba),
        ComponentValue::Function { name, arguments } => {
            parse_color_function(name, arguments).map(Color::Rgba)
        }
        _ => None,
    })
}

/// Parses a color from a string (for presentational attributes and tests).
pub fn parse_color_str(input: &str) -> Option<Color> {
    let v = crate::syntax::parse_component_values(input);
    Cursor::parse_all(&v, parse_color)
}

/// §5.2 hex colors: 3, 4, 6 or 8 hex digits.
fn parse_hex(s: &str) -> Option<Rgba> {
    let digits: Option<Vec<u8>> = s
        .chars()
        .map(|ch| ch.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect();
    let d = digits?;
    let pair = |i: usize| -> Option<u8> { Some(d.get(i)? * 16 + d.get(i + 1)?) };
    let single = |i: usize| -> Option<u8> { Some(d.get(i)? * 17) };
    match d.len() {
        3 => Some(Rgba::rgb(single(0)?, single(1)?, single(2)?)),
        4 => Some(Rgba {
            red: single(0)?,
            green: single(1)?,
            blue: single(2)?,
            alpha: single(3)?,
        }),
        6 => Some(Rgba::rgb(pair(0)?, pair(2)?, pair(4)?)),
        8 => Some(Rgba {
            red: pair(0)?,
            green: pair(2)?,
            blue: pair(4)?,
            alpha: pair(6)?,
        }),
        _ => None,
    }
}

/// A channel in a functional color: a number, a percentage, or `none`.
#[derive(Clone, Copy, Debug)]
enum Channel {
    Number(f32),
    Percent(f32),
    None,
}

impl Channel {
    /// The value, where 100% maps to `percent_ref`; `none` is 0.
    fn resolve(self, percent_ref: f32) -> f32 {
        match self {
            Channel::Number(n) => n,
            Channel::Percent(p) => p / 100.0 * percent_ref,
            Channel::None => 0.0,
        }
    }
}

fn parse_channel(c: &mut Cursor<'_>, allow_none: bool) -> Option<Channel> {
    if allow_none && c.eat_ident("none") {
        return Some(Channel::None);
    }
    if let Some(p) = parse_percentage(c, Range::All) {
        return Some(Channel::Percent(p));
    }
    parse_number(c, Range::All).map(Channel::Number)
}

/// A hue: a number (degrees) or an angle; `none` is 0.
fn parse_hue(c: &mut Cursor<'_>, allow_none: bool) -> Option<f32> {
    if allow_none && c.eat_ident("none") {
        return Some(0.0);
    }
    if let Some(n) = parse_number(c, Range::All) {
        return Some(n);
    }
    c.attempt(|c| match c.next()? {
        ComponentValue::Dimension(n, unit) => angle_to_degrees(n.value, unit),
        _ => None,
    })
}

/// The alpha channel, 0..=1.
fn alpha_of(ch: Channel) -> f32 {
    ch.resolve(1.0).clamp(0.0, 1.0)
}

fn parse_color_function(name: &str, args: &[ComponentValue]) -> Option<Rgba> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "rgb" | "rgba" => parse_rgb(args),
        "hsl" | "hsla" => parse_hsl(args),
        "hwb" => parse_hwb(args),
        "lab" | "lch" | "oklab" | "oklch" => parse_lab_like(&lower, args),
        "color" => parse_color_space(args),
        // M1 follow-up: color-mix(), relative color syntax, light-dark().
        _ => None,
    }
}

/// Splits modern space-separated arguments: three channels then an optional
/// `/ alpha`.
fn parse_modern<'a, T>(
    args: &'a [ComponentValue],
    mut first: impl FnMut(&mut Cursor<'a>) -> Option<T>,
    mut channel: impl FnMut(&mut Cursor<'a>, usize) -> Option<T>,
) -> Option<(T, T, T, Channel)> {
    Cursor::parse_all(args, |c| {
        let a = first(c)?;
        let b = channel(c, 1)?;
        let cc = channel(c, 2)?;
        let alpha = if c.eat_delim('/') {
            parse_channel(c, true)?
        } else {
            Channel::Number(1.0)
        };
        Some((a, b, cc, alpha))
    })
}

/// Legacy comma-separated arguments: 3 or 4 items.
fn legacy_parts(args: &[ComponentValue]) -> Option<Vec<&[ComponentValue]>> {
    if !args.iter().any(|v| matches!(v, ComponentValue::Comma)) {
        return None;
    }
    let parts = split_commas(args);
    (parts.len() == 3 || parts.len() == 4).then_some(parts)
}

/// §5.1 `rgb()` / `rgba()`, legacy and modern syntax.
fn parse_rgb(args: &[ComponentValue]) -> Option<Rgba> {
    if let Some(parts) = legacy_parts(args) {
        // Legacy: all numbers or all percentages; no `none`.
        let mut chans = Vec::new();
        for p in parts.iter().take(3) {
            chans.push(Cursor::parse_all(p, |c| parse_channel(c, false))?);
        }
        let all_pct = chans.iter().all(|c| matches!(c, Channel::Percent(_)));
        let all_num = chans.iter().all(|c| matches!(c, Channel::Number(_)));
        if !all_pct && !all_num {
            return None;
        }
        let alpha = match parts.get(3) {
            Some(p) => Cursor::parse_all(p, |c| parse_channel(c, false))?,
            None => Channel::Number(1.0),
        };
        let v: Vec<f32> = chans.iter().map(|c| c.resolve(255.0) / 255.0).collect();
        return Some(Rgba::from_unit(
            *v.first()?,
            *v.get(1)?,
            *v.get(2)?,
            alpha_of(alpha),
        ));
    }
    let (r, g, b, a) = parse_modern(
        args,
        |c| parse_channel(c, true),
        |c, _| parse_channel(c, true),
    )?;
    Some(Rgba::from_unit(
        r.resolve(255.0) / 255.0,
        g.resolve(255.0) / 255.0,
        b.resolve(255.0) / 255.0,
        alpha_of(a),
    ))
}

/// §7.1 HSL to sRGB; saturation and lightness in 0..=1.
fn hsl_to_rgb(hue: f32, sat: f32, light: f32) -> (f32, f32, f32) {
    let hue = hue.rem_euclid(360.0);
    let f = |n: f32| {
        let k = (n + hue / 30.0).rem_euclid(12.0);
        let a = sat * light.min(1.0 - light);
        light - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    (f(0.0), f(8.0), f(4.0))
}

/// §7 `hsl()` / `hsla()`.
fn parse_hsl(args: &[ComponentValue]) -> Option<Rgba> {
    let (h, s, l, a) = if let Some(parts) = legacy_parts(args) {
        let h = Cursor::parse_all(parts.first()?, |c| parse_hue(c, false))?;
        let pct = |p: &[ComponentValue]| Cursor::parse_all(p, |c| parse_percentage(c, Range::All));
        let s = pct(parts.get(1)?)?;
        let l = pct(parts.get(2)?)?;
        let a = match parts.get(3) {
            Some(p) => Cursor::parse_all(p, |c| parse_channel(c, false))?,
            None => Channel::Number(1.0),
        };
        (h, s, l, a)
    } else {
        let (h, s, l, a) = parse_modern(
            args,
            |c| parse_hue(c, true).map(Channel::Number),
            |c, _| parse_channel(c, true),
        )?;
        (h.resolve(1.0), s.resolve(100.0), l.resolve(100.0), a)
    };
    let s = (s / 100.0).clamp(0.0, 1.0);
    let l = (l / 100.0).clamp(0.0, 1.0);
    let (r, g, b) = hsl_to_rgb(h, s, l);
    Some(Rgba::from_unit(r, g, b, alpha_of(a)))
}

/// §8 `hwb()`.
fn parse_hwb(args: &[ComponentValue]) -> Option<Rgba> {
    let (h, w, b, a) = parse_modern(
        args,
        |c| parse_hue(c, true).map(Channel::Number),
        |c, _| parse_channel(c, true),
    )?;
    let h = h.resolve(1.0);
    // §8: whiteness and blackness are clamped to [0%, 100%] first.
    let w = (w.resolve(100.0) / 100.0).clamp(0.0, 1.0);
    let bl = (b.resolve(100.0) / 100.0).clamp(0.0, 1.0);
    let sum = w + bl;
    if sum >= 1.0 {
        let gray = w / sum;
        return Some(Rgba::from_unit(gray, gray, gray, alpha_of(a)));
    }
    let (r, g, bb) = hsl_to_rgb(h, 1.0, 0.5);
    let f = |x: f32| x * (1.0 - w - bl) + w;
    Some(Rgba::from_unit(f(r), f(g), f(bb), alpha_of(a)))
}

/// sRGB transfer function (linear to gamma-encoded), sign-preserving.
fn srgb_encode(c: f32) -> f32 {
    let abs = c.abs();
    if abs <= 0.003_130_8 {
        12.92 * c
    } else {
        c.signum() * (1.055 * abs.powf(1.0 / 2.4) - 0.055)
    }
}

/// sRGB transfer function (gamma-encoded to linear), sign-preserving.
fn srgb_decode(c: f32) -> f32 {
    let abs = c.abs();
    if abs <= 0.04045 {
        c / 12.92
    } else {
        c.signum() * ((abs + 0.055) / 1.055).powf(2.4)
    }
}

type Vec3 = [f32; 3];
type Mat3 = [[f32; 3]; 3];

fn mul(m: &Mat3, v: Vec3) -> Vec3 {
    let [r0, r1, r2] = m;
    let dot = |r: &[f32; 3]| r[0] * v[0] + r[1] * v[1] + r[2] * v[2];
    [dot(r0), dot(r1), dot(r2)]
}

// Matrices from CSS Color 4 §18 ("Sample code for color conversions"),
// rounded to f32 precision.
const XYZ_D65_TO_LINEAR_SRGB: Mat3 = [
    [3.240_97, -1.537_383_2, -0.498_610_76],
    [-0.969_243_65, 1.875_967_5, 0.041_555_06],
    [0.055_630_08, -0.203_976_96, 1.056_971_5],
];
const LINEAR_P3_TO_XYZ_D65: Mat3 = [
    [0.486_570_95, 0.265_667_7, 0.198_217_29],
    [0.228_974_56, 0.691_738_5, 0.079_286_91],
    [0.0, 0.045_113_38, 1.043_944_4],
];
const D50_TO_D65: Mat3 = [
    [0.955_473_4, -0.023_098_455, 0.063_259_24],
    [-0.028_369_71, 1.009_995_4, 0.021_041_441],
    [0.012_314_015, -0.020_507_65, 1.330_365_9],
];
/// The D50 white point, as XYZ.
const D50_WHITE: Vec3 = [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];

fn linear_srgb_to_rgba(v: Vec3, alpha: f32) -> Rgba {
    Rgba::from_unit(
        srgb_encode(v[0]),
        srgb_encode(v[1]),
        srgb_encode(v[2]),
        alpha,
    )
}

/// CIE Lab (D50) to XYZ D50 (§9.3 / §18).
fn lab_to_xyz_d50(l: f32, a: f32, b: f32) -> Vec3 {
    const KAPPA: f32 = 24389.0 / 27.0;
    const EPSILON: f32 = 216.0 / 24389.0;
    let f1 = (l + 16.0) / 116.0;
    let f0 = a / 500.0 + f1;
    let f2 = f1 - b / 200.0;
    let x = if f0.powi(3) > EPSILON {
        f0.powi(3)
    } else {
        (116.0 * f0 - 16.0) / KAPPA
    };
    let y = if l > KAPPA * EPSILON {
        ((l + 16.0) / 116.0).powi(3)
    } else {
        l / KAPPA
    };
    let z = if f2.powi(3) > EPSILON {
        f2.powi(3)
    } else {
        (116.0 * f2 - 16.0) / KAPPA
    };
    [x * D50_WHITE[0], y * D50_WHITE[1], z * D50_WHITE[2]]
}

/// OKLab to linear sRGB (Ottosson's published matrices, as used in CSS
/// Color 4 §9.2).
fn oklab_to_linear_srgb(l: f32, a: f32, b: f32) -> Vec3 {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_.powi(3), m_.powi(3), s_.powi(3));
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
}

/// §9 `lab()`, `lch()`, `oklab()`, `oklch()`. Out-of-gamut results are
/// clipped to sRGB.
// M1 follow-up: CSS gamut mapping (§13.2) instead of clipping.
fn parse_lab_like(kind: &str, args: &[ComponentValue]) -> Option<Rgba> {
    let polar = kind.ends_with("ch");
    let (l, x, y, a) = parse_modern(
        args,
        |c| parse_channel(c, true),
        |c, i| {
            if polar && i == 2 {
                parse_hue(c, true).map(Channel::Number)
            } else {
                parse_channel(c, true)
            }
        },
    )?;
    let ok = kind.starts_with("ok");
    // Percent references: L, then a/b (or chroma).
    let (l_ref, ab_ref, c_ref) = if ok {
        (1.0, 0.4, 0.4)
    } else {
        (100.0, 125.0, 150.0)
    };
    let l = l.resolve(l_ref).max(0.0);
    let (a_axis, b_axis) = if polar {
        let chroma = x.resolve(c_ref).max(0.0);
        let hue = y.resolve(1.0).to_radians();
        (chroma * hue.cos(), chroma * hue.sin())
    } else {
        (x.resolve(ab_ref), y.resolve(ab_ref))
    };
    let alpha = alpha_of(a);
    if ok {
        Some(linear_srgb_to_rgba(
            oklab_to_linear_srgb(l, a_axis, b_axis),
            alpha,
        ))
    } else {
        let xyz65 = mul(&D50_TO_D65, lab_to_xyz_d50(l, a_axis, b_axis));
        Some(linear_srgb_to_rgba(
            mul(&XYZ_D65_TO_LINEAR_SRGB, xyz65),
            alpha,
        ))
    }
}

/// §10 `color()` with the sRGB, linear sRGB, Display P3 and XYZ spaces.
fn parse_color_space(args: &[ComponentValue]) -> Option<Rgba> {
    Cursor::parse_all(args, |c| {
        let space = c.ident()?.to_ascii_lowercase();
        let r = parse_channel(c, true)?.resolve(1.0);
        let g = parse_channel(c, true)?.resolve(1.0);
        let b = parse_channel(c, true)?.resolve(1.0);
        let alpha = if c.eat_delim('/') {
            alpha_of(parse_channel(c, true)?)
        } else {
            1.0
        };
        let linear = match space.as_str() {
            "srgb" => return Some(Rgba::from_unit(r, g, b, alpha)),
            "srgb-linear" => [r, g, b],
            "display-p3" => {
                let lin = [srgb_decode(r), srgb_decode(g), srgb_decode(b)];
                mul(&XYZ_D65_TO_LINEAR_SRGB, mul(&LINEAR_P3_TO_XYZ_D65, lin))
            }
            "xyz" | "xyz-d65" => mul(&XYZ_D65_TO_LINEAR_SRGB, [r, g, b]),
            "xyz-d50" => mul(&XYZ_D65_TO_LINEAR_SRGB, mul(&D50_TO_D65, [r, g, b])),
            // M1 follow-up: a98-rgb, prophoto-rgb, rec2020.
            _ => return None,
        };
        Some(linear_srgb_to_rgba(linear, alpha))
    })
}

/// §6.1 named colors.
fn named_color(name: &str) -> Option<Rgba> {
    let lower = name.to_ascii_lowercase();
    let hex: u32 = match lower.as_str() {
        "aliceblue" => 0xf0f8ff,
        "antiquewhite" => 0xfaebd7,
        "aqua" => 0x00ffff,
        "aquamarine" => 0x7fffd4,
        "azure" => 0xf0ffff,
        "beige" => 0xf5f5dc,
        "bisque" => 0xffe4c4,
        "black" => 0x000000,
        "blanchedalmond" => 0xffebcd,
        "blue" => 0x0000ff,
        "blueviolet" => 0x8a2be2,
        "brown" => 0xa52a2a,
        "burlywood" => 0xdeb887,
        "cadetblue" => 0x5f9ea0,
        "chartreuse" => 0x7fff00,
        "chocolate" => 0xd2691e,
        "coral" => 0xff7f50,
        "cornflowerblue" => 0x6495ed,
        "cornsilk" => 0xfff8dc,
        "crimson" => 0xdc143c,
        "cyan" => 0x00ffff,
        "darkblue" => 0x00008b,
        "darkcyan" => 0x008b8b,
        "darkgoldenrod" => 0xb8860b,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "darkgreen" => 0x006400,
        "darkkhaki" => 0xbdb76b,
        "darkmagenta" => 0x8b008b,
        "darkolivegreen" => 0x556b2f,
        "darkorange" => 0xff8c00,
        "darkorchid" => 0x9932cc,
        "darkred" => 0x8b0000,
        "darksalmon" => 0xe9967a,
        "darkseagreen" => 0x8fbc8f,
        "darkslateblue" => 0x483d8b,
        "darkslategray" | "darkslategrey" => 0x2f4f4f,
        "darkturquoise" => 0x00ced1,
        "darkviolet" => 0x9400d3,
        "deeppink" => 0xff1493,
        "deepskyblue" => 0x00bfff,
        "dimgray" | "dimgrey" => 0x696969,
        "dodgerblue" => 0x1e90ff,
        "firebrick" => 0xb22222,
        "floralwhite" => 0xfffaf0,
        "forestgreen" => 0x228b22,
        "fuchsia" => 0xff00ff,
        "gainsboro" => 0xdcdcdc,
        "ghostwhite" => 0xf8f8ff,
        "gold" => 0xffd700,
        "goldenrod" => 0xdaa520,
        "gray" | "grey" => 0x808080,
        "green" => 0x008000,
        "greenyellow" => 0xadff2f,
        "honeydew" => 0xf0fff0,
        "hotpink" => 0xff69b4,
        "indianred" => 0xcd5c5c,
        "indigo" => 0x4b0082,
        "ivory" => 0xfffff0,
        "khaki" => 0xf0e68c,
        "lavender" => 0xe6e6fa,
        "lavenderblush" => 0xfff0f5,
        "lawngreen" => 0x7cfc00,
        "lemonchiffon" => 0xfffacd,
        "lightblue" => 0xadd8e6,
        "lightcoral" => 0xf08080,
        "lightcyan" => 0xe0ffff,
        "lightgoldenrodyellow" => 0xfafad2,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "lightgreen" => 0x90ee90,
        "lightpink" => 0xffb6c1,
        "lightsalmon" => 0xffa07a,
        "lightseagreen" => 0x20b2aa,
        "lightskyblue" => 0x87cefa,
        "lightslategray" | "lightslategrey" => 0x778899,
        "lightsteelblue" => 0xb0c4de,
        "lightyellow" => 0xffffe0,
        "lime" => 0x00ff00,
        "limegreen" => 0x32cd32,
        "linen" => 0xfaf0e6,
        "magenta" => 0xff00ff,
        "maroon" => 0x800000,
        "mediumaquamarine" => 0x66cdaa,
        "mediumblue" => 0x0000cd,
        "mediumorchid" => 0xba55d3,
        "mediumpurple" => 0x9370db,
        "mediumseagreen" => 0x3cb371,
        "mediumslateblue" => 0x7b68ee,
        "mediumspringgreen" => 0x00fa9a,
        "mediumturquoise" => 0x48d1cc,
        "mediumvioletred" => 0xc71585,
        "midnightblue" => 0x191970,
        "mintcream" => 0xf5fffa,
        "mistyrose" => 0xffe4e1,
        "moccasin" => 0xffe4b5,
        "navajowhite" => 0xffdead,
        "navy" => 0x000080,
        "oldlace" => 0xfdf5e6,
        "olive" => 0x808000,
        "olivedrab" => 0x6b8e23,
        "orange" => 0xffa500,
        "orangered" => 0xff4500,
        "orchid" => 0xda70d6,
        "palegoldenrod" => 0xeee8aa,
        "palegreen" => 0x98fb98,
        "paleturquoise" => 0xafeeee,
        "palevioletred" => 0xdb7093,
        "papayawhip" => 0xffefd5,
        "peachpuff" => 0xffdab9,
        "peru" => 0xcd853f,
        "pink" => 0xffc0cb,
        "plum" => 0xdda0dd,
        "powderblue" => 0xb0e0e6,
        "purple" => 0x800080,
        "rebeccapurple" => 0x663399,
        "red" => 0xff0000,
        "rosybrown" => 0xbc8f8f,
        "royalblue" => 0x4169e1,
        "saddlebrown" => 0x8b4513,
        "salmon" => 0xfa8072,
        "sandybrown" => 0xf4a460,
        "seagreen" => 0x2e8b57,
        "seashell" => 0xfff5ee,
        "sienna" => 0xa0522d,
        "silver" => 0xc0c0c0,
        "skyblue" => 0x87ceeb,
        "slateblue" => 0x6a5acd,
        "slategray" | "slategrey" => 0x708090,
        "snow" => 0xfffafa,
        "springgreen" => 0x00ff7f,
        "steelblue" => 0x4682b4,
        "tan" => 0xd2b48c,
        "teal" => 0x008080,
        "thistle" => 0xd8bfd8,
        "tomato" => 0xff6347,
        "turquoise" => 0x40e0d0,
        "violet" => 0xee82ee,
        "wheat" => 0xf5deb3,
        "white" => 0xffffff,
        "whitesmoke" => 0xf5f5f5,
        "yellow" => 0xffff00,
        "yellowgreen" => 0x9acd32,
        _ => return None,
    };
    let [_, r, g, b] = hex.to_be_bytes();
    Some(Rgba::rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(s: &str) -> Option<Rgba> {
        match parse_color_str(s)? {
            Color::Rgba(c) => Some(c),
            other => panic!("{other:?}"),
        }
    }

    fn close(a: Rgba, b: Rgba) -> bool {
        let d = |x: u8, y: u8| (i16::from(x) - i16::from(y)).abs() <= 1;
        d(a.red, b.red) && d(a.green, b.green) && d(a.blue, b.blue) && d(a.alpha, b.alpha)
    }

    #[test]
    fn named_and_keywords() {
        assert_eq!(rgba("RebeccaPurple"), Some(Rgba::rgb(0x66, 0x33, 0x99)));
        assert_eq!(rgba("transparent"), Some(Rgba::TRANSPARENT));
        assert_eq!(parse_color_str("currentColor"), Some(Color::CurrentColor));
        assert_eq!(
            parse_color_str("Canvas"),
            Some(Color::System(SystemColor::Canvas))
        );
        assert_eq!(parse_color_str("notacolor"), None);
    }

    #[test]
    fn hex() {
        assert_eq!(rgba("#f00"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(rgba("#0f08").map(|c| c.alpha), Some(0x88));
        assert_eq!(rgba("#123456"), Some(Rgba::rgb(0x12, 0x34, 0x56)));
        assert_eq!(rgba("#12345678").map(|c| c.alpha), Some(0x78));
        assert_eq!(rgba("#12345"), None);
        assert_eq!(rgba("#ggg"), None);
    }

    #[test]
    fn rgb_functions() {
        assert_eq!(rgba("rgb(255, 0, 0)"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(rgba("rgba(100%, 0%, 0%, 0.5)").map(|c| c.alpha), Some(128));
        assert_eq!(rgba("rgb(255, 0%, 0)"), None);
        assert_eq!(rgba("rgb(255 128 0 / 50%)").map(|c| c.alpha), Some(128));
        assert_eq!(rgba("rgb(none 300 -5)"), Some(Rgba::rgb(0, 255, 0)));
        assert_eq!(rgba("rgba(1 2 3)"), Some(Rgba::rgb(1, 2, 3)));
        assert_eq!(rgba("rgb(1, 2 3)"), None);
        assert_eq!(rgba("rgb(calc(100 + 155) 0 0)"), Some(Rgba::rgb(255, 0, 0)));
    }

    #[test]
    fn hsl_hwb() {
        assert_eq!(rgba("hsl(120, 100%, 50%)"), Some(Rgba::rgb(0, 255, 0)));
        assert_eq!(rgba("hsl(0.5turn 100% 25%)"), Some(Rgba::rgb(0, 128, 128)));
        assert_eq!(rgba("hsla(240 100 50 / 0)").map(|c| c.alpha), Some(0));
        assert_eq!(rgba("hsl(120, 100, 50)"), None);
        assert_eq!(rgba("hwb(0 0% 0%)"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(rgba("hwb(0 60% 60%)"), Some(Rgba::rgb(128, 128, 128)));
    }

    #[test]
    fn hwb_clamps_whiteness_and_blackness() {
        assert_eq!(rgba("hwb(30 -50% 50%)"), Some(Rgba::rgb(128, 64, 0)));
        assert_eq!(rgba("hwb(0 0% 150%)"), Some(Rgba::rgb(0, 0, 0)));
    }

    #[test]
    fn percentage_calc_in_colors() {
        assert_eq!(rgba("rgb(calc(100%) 0% 0%)"), Some(Rgba::rgb(255, 0, 0)));
    }

    #[test]
    fn lab_family() {
        let red = Rgba::rgb(255, 0, 0);
        assert!(
            close(rgba("lab(54.29 80.81 69.89)").unwrap(), red),
            "{:?}",
            rgba("lab(54.29 80.81 69.89)")
        );
        assert!(close(rgba("lch(54.29 106.84 40.85)").unwrap(), red));
        assert!(close(rgba("oklab(0.628 0.2249 0.1258)").unwrap(), red));
        assert!(close(rgba("oklch(62.8% 0.2577 29.23)").unwrap(), red));
        assert!(close(rgba("color(srgb 1 0 0)").unwrap(), red));
        assert!(close(rgba("color(srgb-linear 1 0 0)").unwrap(), red));
        assert!(close(
            rgba("color(display-p3 0.9175 0.2003 0.1386)").unwrap(),
            red
        ));
        assert!(close(
            rgba("color(xyz 0.9505 1 1.089)").unwrap(),
            Rgba::rgb(255, 255, 255)
        ));
        assert_eq!(rgba("color(rec9999 1 0 0)"), None);
    }

    #[test]
    fn system_fallbacks() {
        assert_eq!(SystemColor::Canvas.to_rgba(false), Rgba::rgb(255, 255, 255));
        assert_eq!(
            SystemColor::CanvasText.to_rgba(true),
            Rgba::rgb(255, 255, 255)
        );
    }
}
