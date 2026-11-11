//! Perceptual comparison of rendered pages with their reference images.
//!
//! Two renderings of the same page on two machines (or after an unrelated
//! change) differ in glyph antialiasing and subpixel positions, so an
//! exact comparison is too strict. A comparison passes when both of these
//! hold:
//!
//! 1. **Few mismatched pixels.** A pixel mismatches when any of its color
//!    channels (composited over white) differs by more than
//!    [`Thresholds::pixel_tolerance`]. At most
//!    [`Thresholds::max_mismatch_ratio`] of all pixels may mismatch. This
//!    catches broad changes: a shifted layout, a different background.
//! 2. **No block changed much.** The image is cut into
//!    [`Thresholds::block`]-pixel squares, and the mean luminance of each
//!    square is compared. No square may differ by more than
//!    [`Thresholds::max_block_delta`] (on a 0–255 scale). This catches
//!    small but real changes that the ratio would dilute: a missing word,
//!    a lost bullet, an image that didn't load.
//!
//! Images of different sizes never match.

use std::io::Cursor;

use crate::Result;

/// An 8-bit straight-alpha RGBA image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, row by row.
    pub rgba: Vec<u8>,
}

/// Largest reference image accepted, in pixels per side.
const MAX_SIDE: u32 = 16_384;

impl Image {
    /// The pixel at `(x, y)` composited over white, as RGB.
    fn rgb_over_white(&self, x: u32, y: u32) -> [u8; 3] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        match self.rgba.get(i..i + 4) {
            Some(&[r, g, b, a]) => {
                let over = |c: u8| {
                    let v = (u32::from(c) * u32::from(a) + 255 * (255 - u32::from(a)) + 127) / 255;
                    u8::try_from(v).unwrap_or(255)
                };
                [over(r), over(g), over(b)]
            }
            _ => [255, 255, 255],
        }
    }
}

/// How close two images must be to count as the same rendering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    /// Largest per-channel difference (0–255) that still counts as equal.
    pub pixel_tolerance: u8,
    /// Largest share of mismatched pixels (0–1).
    pub max_mismatch_ratio: f64,
    /// Side of the luminance blocks, in pixels.
    pub block: u32,
    /// Largest mean-luminance difference of any block (0–255).
    pub max_block_delta: f64,
}

impl Default for Thresholds {
    fn default() -> Thresholds {
        Thresholds {
            pixel_tolerance: 40,
            max_mismatch_ratio: 0.005,
            block: 16,
            max_block_delta: 12.0,
        }
    }
}

/// The result of comparing two images.
#[derive(Clone, Debug, PartialEq)]
pub struct Diff {
    /// Whether the images have different sizes (nothing else is measured).
    pub size_mismatch: bool,
    /// Pixels whose channels differ by more than the tolerance.
    pub mismatched: u64,
    /// All pixels compared.
    pub total: u64,
    /// The largest block mean-luminance difference.
    pub worst_block: f64,
    /// The top-left pixel of that block.
    pub worst_block_at: (u32, u32),
    /// The block side used, in pixels.
    pub block: u32,
}

impl Diff {
    /// Share of mismatched pixels (0–1).
    pub fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.mismatched as f64 / self.total as f64
        }
    }

    /// Whether the comparison is within `t`.
    pub fn passes(&self, t: &Thresholds) -> bool {
        !self.size_mismatch
            && self.ratio() <= t.max_mismatch_ratio
            && self.worst_block <= t.max_block_delta
    }

    /// A one-line summary for logs.
    pub fn summary(&self) -> String {
        if self.size_mismatch {
            return "image sizes differ".to_string();
        }
        format!(
            "{:.3}% pixels differ ({} of {}), worst {}px block differs by {:.1} at ({}, {})",
            self.ratio() * 100.0,
            self.mismatched,
            self.total,
            self.block,
            self.worst_block,
            self.worst_block_at.0,
            self.worst_block_at.1
        )
    }
}

/// Rec. 709 luma of an RGB triple, 0–255.
fn luma([r, g, b]: [u8; 3]) -> f64 {
    0.2126 * f64::from(r) + 0.7152 * f64::from(g) + 0.0722 * f64::from(b)
}

/// Compares `actual` with `expected` (see the module docs).
pub fn compare(actual: &Image, expected: &Image, t: &Thresholds) -> Diff {
    if actual.width != expected.width || actual.height != expected.height {
        return Diff {
            size_mismatch: true,
            mismatched: 0,
            total: 0,
            worst_block: f64::INFINITY,
            worst_block_at: (0, 0),
            block: t.block,
        };
    }
    let (w, h) = (actual.width, actual.height);
    let block = t.block.max(1);
    let mut mismatched = 0u64;
    let mut worst_block = 0.0f64;
    let mut worst_block_at = (0, 0);
    for by in (0..h).step_by(block as usize) {
        for bx in (0..w).step_by(block as usize) {
            let (mut sum_a, mut sum_e, mut n) = (0.0, 0.0, 0u32);
            for y in by..(by + block).min(h) {
                for x in bx..(bx + block).min(w) {
                    let pa = actual.rgb_over_white(x, y);
                    let pe = expected.rgb_over_white(x, y);
                    let far = pa
                        .iter()
                        .zip(pe.iter())
                        .any(|(a, e)| a.abs_diff(*e) > t.pixel_tolerance);
                    if far {
                        mismatched += 1;
                    }
                    sum_a += luma(pa);
                    sum_e += luma(pe);
                    n += 1;
                }
            }
            if n > 0 {
                let delta = (sum_a - sum_e).abs() / f64::from(n);
                if delta > worst_block {
                    worst_block = delta;
                    worst_block_at = (bx, by);
                }
            }
        }
    }
    Diff {
        size_mismatch: false,
        mismatched,
        total: u64::from(w) * u64::from(h),
        worst_block,
        worst_block_at,
        block,
    }
}

/// A picture of the differences for review: the expected image faded to
/// light gray, with mismatched pixels in red. Uses the smaller of the two
/// sizes when they differ.
pub fn diff_image(actual: &Image, expected: &Image, t: &Thresholds) -> Image {
    let w = actual.width.min(expected.width);
    let h = actual.height.min(expected.height);
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            let pa = actual.rgb_over_white(x, y);
            let pe = expected.rgb_over_white(x, y);
            let far = pa
                .iter()
                .zip(pe.iter())
                .any(|(a, e)| a.abs_diff(*e) > t.pixel_tolerance);
            if far {
                rgba.extend_from_slice(&[230, 0, 0, 255]);
            } else {
                // Map luma to 192..=255 so the page stays recognizable.
                let l = luma(pe) / 255.0 * 63.0 + 192.0;
                let l = l.round().clamp(0.0, 255.0) as u8;
                rgba.extend_from_slice(&[l, l, l, 255]);
            }
        }
    }
    Image {
        width: w,
        height: h,
        rgba,
    }
}

/// Decodes an 8-bit RGB or RGBA PNG.
pub fn decode_png(bytes: &[u8]) -> Result<Image> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("invalid PNG: {e}"))?;
    let (color, depth) = reader.output_color_type();
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!("PNG is too large ({width}x{height})"));
    }
    if depth != png::BitDepth::Eight {
        return Err("PNG must be 8 bits per channel".to_string());
    }
    let size = reader
        .output_buffer_size()
        .ok_or("PNG is too large to decode")?;
    let mut buf = vec![0; size];
    let frame = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("invalid PNG: {e}"))?;
    let data = buf.get(..frame.buffer_size()).unwrap_or_default();
    let rgba = match color {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => rgb_to_rgba(data),
        other => return Err(format!("PNG must be RGB or RGBA, not {other:?}")),
    };
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("PNG has an unexpected row layout".to_string());
    }
    Ok(Image {
        width,
        height,
        rgba,
    })
}

fn rgb_to_rgba(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 3 * 4);
    for p in data.chunks_exact(3) {
        out.extend_from_slice(p);
        out.push(255);
    }
    out
}

/// Encodes an image as a well-compressed 8-bit RGBA PNG.
pub fn encode_png(image: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, image.width, image.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::High);
        let mut writer = enc
            .write_header()
            .map_err(|e| format!("PNG encoding failed: {e}"))?;
        writer
            .write_image_data(&image.rgba)
            .map_err(|e| format!("PNG encoding failed: {e}"))?;
        writer
            .finish()
            .map_err(|e| format!("PNG encoding failed: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            rgba: rgba.repeat((w * h) as usize),
        }
    }

    fn set(img: &mut Image, x: u32, y: u32, rgba: [u8; 4]) {
        let i = ((y * img.width + x) * 4) as usize;
        img.rgba[i..i + 4].copy_from_slice(&rgba);
    }

    #[test]
    fn identical_images_pass() {
        let a = solid(64, 48, [10, 20, 30, 255]);
        let d = compare(&a, &a, &Thresholds::default());
        assert!(d.passes(&Thresholds::default()));
        assert_eq!((d.mismatched, d.total), (0, 64 * 48));
        assert_eq!(d.worst_block, 0.0);
    }

    #[test]
    fn size_mismatch_fails() {
        let d = compare(
            &solid(10, 10, [0; 4]),
            &solid(10, 11, [0; 4]),
            &Thresholds::default(),
        );
        assert!(d.size_mismatch);
        assert!(!d.passes(&Thresholds::default()));
    }

    #[test]
    fn small_antialiasing_noise_passes() {
        let a = solid(100, 100, [255, 255, 255, 255]);
        let mut b = a.clone();
        // Slight shifts everywhere stay under the tolerance.
        for y in 0..100 {
            for x in 0..100 {
                if (x + y) % 3 == 0 {
                    set(&mut b, x, y, [235, 235, 235, 255]);
                }
            }
        }
        // A handful of strongly different pixels, spread out.
        for i in 0..10 {
            set(&mut b, i * 9, i * 9, [0, 0, 0, 255]);
        }
        let d = compare(&b, &a, &Thresholds::default());
        assert_eq!(d.mismatched, 10);
        assert!(d.passes(&Thresholds::default()), "{}", d.summary());
    }

    #[test]
    fn a_missing_word_fails_on_the_block_metric() {
        let a = solid(400, 400, [255, 255, 255, 255]);
        let mut b = a.clone();
        // A dark 12x10 "word" inside one block: far below the ratio limit.
        for y in 2..12 {
            for x in 2..14 {
                set(&mut b, x, y, [0, 0, 0, 255]);
            }
        }
        let t = Thresholds::default();
        let d = compare(&b, &a, &t);
        assert!(d.ratio() < t.max_mismatch_ratio);
        assert!(d.worst_block > t.max_block_delta);
        assert_eq!(d.worst_block_at, (0, 0));
        assert!(!d.passes(&t));
    }

    #[test]
    fn a_broad_change_fails_on_the_ratio() {
        let a = solid(100, 100, [255, 255, 255, 255]);
        let b = solid(100, 100, [200, 200, 255, 255]);
        let t = Thresholds::default();
        let d = compare(&b, &a, &t);
        assert_eq!(d.mismatched, 100 * 100);
        assert!(!d.passes(&t));
    }

    #[test]
    fn transparency_is_composited_over_white() {
        let a = solid(8, 8, [0, 0, 0, 0]);
        let b = solid(8, 8, [255, 255, 255, 255]);
        assert!(compare(&a, &b, &Thresholds::default()).passes(&Thresholds::default()));
    }

    #[test]
    fn png_round_trip() {
        let mut a = solid(5, 3, [1, 2, 3, 255]);
        set(&mut a, 4, 2, [200, 100, 50, 128]);
        let png = encode_png(&a).unwrap();
        assert_eq!(decode_png(&png).unwrap(), a);
        assert!(decode_png(b"not a png").is_err());
    }

    #[test]
    fn diff_image_marks_mismatches() {
        let a = solid(4, 4, [255, 255, 255, 255]);
        let mut b = a.clone();
        set(&mut b, 1, 1, [0, 0, 0, 255]);
        let d = diff_image(&b, &a, &Thresholds::default());
        assert_eq!(&d.rgba[(5 * 4)..(5 * 4 + 4)], &[230, 0, 0, 255]);
        assert_eq!(&d.rgba[0..4], &[255, 255, 255, 255]);
    }
}
