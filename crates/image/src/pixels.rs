//! Pixel-buffer helpers shared by the format decoders: widening to RGBA,
//! premultiplying, and applying EXIF orientation.

use crate::exif::Orientation;

/// Widens `pixel_count` packed pixels of `channels` bytes each (1 = gray,
/// 2 = gray + alpha, 3 = RGB, 4 = RGBA) at the start of `buf` into RGBA8,
/// in place.
///
/// `buf` must hold at least `pixel_count * 4` bytes. Returns `false` if it
/// does not or `channels` is not 1 to 4.
pub(crate) fn expand_to_rgba(buf: &mut [u8], pixel_count: usize, channels: usize) -> bool {
    if !(1..=4).contains(&channels) || pixel_count.checked_mul(4).is_none_or(|n| n > buf.len()) {
        return false;
    }
    if channels == 4 {
        return true;
    }
    // Walk backwards: pixel i is read from [i*c, i*c+c) and written to
    // [i*4, i*4+4). Since c <= 4, a write never clobbers a pixel that is
    // still to be read (all of those lie below i*c <= i*4).
    for i in (0..pixel_count).rev() {
        let src = i * channels;
        let rgba = match buf.get(src..src + channels) {
            Some(&[g]) => [g, g, g, 255],
            Some(&[g, a]) => [g, g, g, a],
            Some(&[r, g, b]) => [r, g, b, 255],
            _ => return false,
        };
        let dst = i * 4;
        match buf.get_mut(dst..dst + 4) {
            Some(out) => out.copy_from_slice(&rgba),
            None => return false,
        }
    }
    true
}

/// Converts straight-alpha RGBA8 pixels to premultiplied alpha, in place.
pub(crate) fn premultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        if let [r, g, b, a] = px {
            match *a {
                255 => {}
                0 => {
                    *r = 0;
                    *g = 0;
                    *b = 0;
                }
                alpha => {
                    *r = mul_div_255(*r, alpha);
                    *g = mul_div_255(*g, alpha);
                    *b = mul_div_255(*b, alpha);
                }
            }
        }
    }
}

/// `round(c * a / 255)`, exact for all 8-bit inputs.
fn mul_div_255(c: u8, a: u8) -> u8 {
    let t = u32::from(c) * u32::from(a) + 128;
    // (t + (t >> 8)) >> 8 is the exact rounded quotient by 255 for t < 65536.
    let q = (t + (t >> 8)) >> 8;
    u8::try_from(q).unwrap_or(u8::MAX)
}

/// Applies an EXIF orientation to a `width` x `height` RGBA8 buffer.
///
/// Returns the new buffer and its (possibly swapped) dimensions, or `None`
/// if `rgba` has the wrong length. The caller must have budgeted for a
/// second buffer of the same size.
pub(crate) fn apply_orientation(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    orientation: Orientation,
) -> Option<(Vec<u8>, u32, u32)> {
    let w = usize::try_from(width).ok()?;
    let h = usize::try_from(height).ok()?;
    if w.checked_mul(h)?.checked_mul(4)? != rgba.len() {
        return None;
    }
    if orientation == Orientation::Normal {
        return Some((rgba, width, height));
    }
    let (out_w, out_h) = if orientation.swaps_dimensions() {
        (height, width)
    } else {
        (width, height)
    };
    let out_stride = if orientation.swaps_dimensions() { h } else { w };
    let mut out = vec![0u8; rgba.len()];
    // CIPA DC-008 (Exif 2.32) §4.6.4 tag 0x0112: each value names where the
    // stored 0th row and 0th column end up when displayed. The match maps a
    // stored pixel (x, y) to its displayed position (dx, dy).
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        let x = i % w;
        let y = i / w;
        let (dx, dy) = match orientation {
            Orientation::Normal => (x, y),
            Orientation::FlipHorizontal => (w - 1 - x, y),
            Orientation::Rotate180 => (w - 1 - x, h - 1 - y),
            Orientation::FlipVertical => (x, h - 1 - y),
            Orientation::Transpose => (y, x),
            Orientation::Rotate90 => (h - 1 - y, x),
            Orientation::Transverse => (h - 1 - y, w - 1 - x),
            Orientation::Rotate270 => (y, w - 1 - x),
        };
        let at = (dy * out_stride + dx) * 4;
        out.get_mut(at..at + 4)?.copy_from_slice(px);
    }
    Some((out, out_w, out_h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_div_255_is_exact() {
        for c in 0..=255u8 {
            for a in 0..=255u8 {
                let want = (f64::from(c) * f64::from(a) / 255.0).round() as u8;
                assert_eq!(mul_div_255(c, a), want, "c={c} a={a}");
            }
        }
    }

    #[test]
    fn premultiplies() {
        let mut px = vec![200, 100, 50, 128, 1, 2, 3, 0, 9, 8, 7, 255];
        premultiply(&mut px);
        assert_eq!(px, vec![100, 50, 25, 128, 0, 0, 0, 0, 9, 8, 7, 255]);
    }

    #[test]
    fn expands_every_channel_count() {
        let mut g = vec![10, 20, 0, 0, 0, 0, 0, 0];
        assert!(expand_to_rgba(&mut g, 2, 1));
        assert_eq!(g, vec![10, 10, 10, 255, 20, 20, 20, 255]);

        let mut ga = vec![10, 1, 20, 2, 0, 0, 0, 0];
        assert!(expand_to_rgba(&mut ga, 2, 2));
        assert_eq!(ga, vec![10, 10, 10, 1, 20, 20, 20, 2]);

        let mut rgb = vec![1, 2, 3, 4, 5, 6, 0, 0];
        assert!(expand_to_rgba(&mut rgb, 2, 3));
        assert_eq!(rgb, vec![1, 2, 3, 255, 4, 5, 6, 255]);

        let mut rgba = vec![1, 2, 3, 4];
        assert!(expand_to_rgba(&mut rgba, 1, 4));
        assert_eq!(rgba, vec![1, 2, 3, 4]);
    }

    #[test]
    fn expand_rejects_bad_input() {
        assert!(!expand_to_rgba(&mut [0; 7], 2, 3));
        assert!(!expand_to_rgba(&mut [0; 8], 2, 5));
        assert!(!expand_to_rgba(&mut [0; 8], 2, 0));
        assert!(!expand_to_rgba(&mut [0; 8], usize::MAX, 1));
    }

    /// A 3x2 image whose pixel (x, y) has red = 10*x + y.
    fn sample() -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                v.extend_from_slice(&[10 * x + y, 0, 0, 255]);
            }
        }
        v
    }

    fn reds(buf: &[u8]) -> Vec<u8> {
        buf.chunks(4).map(|p| p[0]).collect()
    }

    #[test]
    fn orientations() {
        // Stored:      00 10 20
        //              01 11 21
        let cases = [
            (Orientation::Normal, 3, 2, vec![0, 10, 20, 1, 11, 21]),
            (
                Orientation::FlipHorizontal,
                3,
                2,
                vec![20, 10, 0, 21, 11, 1],
            ),
            (Orientation::Rotate180, 3, 2, vec![21, 11, 1, 20, 10, 0]),
            (Orientation::FlipVertical, 3, 2, vec![1, 11, 21, 0, 10, 20]),
            (Orientation::Transpose, 2, 3, vec![0, 1, 10, 11, 20, 21]),
            // Rotated 90 degrees clockwise: the stored left column becomes
            // the top row, read from bottom to top.
            (Orientation::Rotate90, 2, 3, vec![1, 0, 11, 10, 21, 20]),
            (Orientation::Transverse, 2, 3, vec![21, 20, 11, 10, 1, 0]),
            (Orientation::Rotate270, 2, 3, vec![20, 21, 10, 11, 0, 1]),
        ];
        for (o, w, h, want) in cases {
            let (out, ow, oh) = apply_orientation(sample(), 3, 2, o).unwrap();
            assert_eq!((ow, oh), (w, h), "{o:?}");
            assert_eq!(reds(&out), want, "{o:?}");
        }
    }

    #[test]
    fn orientation_rejects_wrong_length() {
        assert!(apply_orientation(vec![0; 8], 3, 2, Orientation::Rotate90).is_none());
    }
}
