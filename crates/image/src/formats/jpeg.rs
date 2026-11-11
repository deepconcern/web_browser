//! JPEG via `zune-jpeg` (ITU-T T.81 / JFIF), with EXIF orientation applied.
//!
//! zune-jpeg converts YCbCr, grayscale, CMYK and YCCK input to RGB for us
//! (we ask for RGB rather than RGBA because zune-jpeg 0.5.15 mishandles
//! CMYK to RGBA). Embedded ICC profiles are ignored (sRGB is assumed).

use ::zune_core::bytestream::ZCursor;
use ::zune_core::colorspace::ColorSpace;
use ::zune_core::options::DecoderOptions;
use ::zune_jpeg::JpegDecoder;

use crate::exif::{self, Orientation};
use crate::pixels::{apply_orientation, expand_to_rgba};
use crate::{Image, ImageError, ImageFormat, Limits};

const FORMAT: ImageFormat = ImageFormat::Jpeg;

fn malformed(e: impl std::fmt::Display) -> ImageError {
    ImageError::malformed(FORMAT, e)
}

fn options() -> DecoderOptions {
    // JPEG sizes are 16-bit. Let zune-jpeg accept them all; `Limits` is
    // checked by us so the error is the same for every format.
    let max = usize::from(u16::MAX);
    DecoderOptions::default()
        .set_max_width(max)
        .set_max_height(max)
        .set_strict_mode(false)
        .jpeg_set_out_colorspace(ColorSpace::RGB)
}

/// Decodes the headers (everything before the first scan) and returns the
/// stored size and the EXIF orientation.
fn headers(
    decoder: &mut JpegDecoder<ZCursor<&[u8]>>,
) -> Result<(u32, u32, Orientation), ImageError> {
    decoder.decode_headers().map_err(malformed)?;
    let info = decoder
        .info()
        .ok_or_else(|| malformed("headers not decoded"))?;
    let orientation = info
        .exif_data
        .as_deref()
        .map_or(Orientation::Normal, exif::orientation);
    Ok((u32::from(info.width), u32::from(info.height), orientation))
}

pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options());
    let (width, height, orientation) = headers(&mut decoder)?;
    Ok(if orientation.swaps_dimensions() {
        (height, width)
    } else {
        (width, height)
    })
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, ImageError> {
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options());
    let (width, height, orientation) = headers(&mut decoder)?;
    limits.check_dimensions(width, height)?;
    let rgba_len = limits
        .check_memory(
            width,
            height,
            working_bytes(bytes, width, height, orientation),
        )?
        .0;
    let pixel_count = rgba_len / 4;
    let rgb_len = pixel_count.saturating_mul(3);
    if decoder.output_buffer_size() != Some(rgb_len) {
        return Err(malformed("unexpected output size"));
    }

    // One allocation: decode RGB into the front, then widen in place.
    let mut buf = vec![0u8; rgba_len];
    let decoded = buf
        .get_mut(..rgb_len)
        .ok_or_else(|| malformed("unexpected output size"))?;
    decoder.decode_into(decoded).map_err(malformed)?;
    // JPEG has no alpha, so opaque pixels are already premultiplied.
    if !expand_to_rgba(&mut buf, pixel_count, 3) {
        return Err(malformed("unexpected output size"));
    }
    let (buf, width, height) = apply_orientation(buf, width, height, orientation)
        .ok_or_else(|| malformed("unexpected output size"))?;
    Image::from_premultiplied_rgba8(width, height, buf).ok_or(ImageError::ZeroSize)
}

/// Bytes the decode needs besides the RGBA output, at the worst moment.
///
/// - zune-jpeg keeps a few rows of samples per component (raw coefficients,
///   upsampling input and output): at most 256 bytes per component per
///   column, columns padded to the largest MCU width (32).
/// - A progressive JPEG, or a sequential one whose first scan doesn't
///   carry every component, has its whole coefficient plane kept for every
///   component (2 bytes per sample) until the last scan.
/// - Reorienting copies the output into a second buffer, after the
///   coefficients are freed.
fn working_bytes(bytes: &[u8], width: u32, height: u32, orientation: Orientation) -> usize {
    let layout = frame_layout(bytes);
    let components = layout.as_ref().map_or(MAX_COMPONENTS, |l| l.sampling.len());
    let rows = components
        .saturating_mul(pad(width, 32))
        .saturating_mul(256);
    let coefficients = match &layout {
        Some(l) if !l.keeps_all_coefficients() => 0,
        Some(l) => l.coefficient_bytes(),
        // Our parse failed where zune-jpeg's succeeded: assume the worst,
        // every component at full resolution.
        None => MAX_COMPONENTS
            .saturating_mul(pad(width, 32))
            .saturating_mul(pad(height, 32))
            .saturating_mul(2),
    };
    let reorient = if orientation == Orientation::Normal {
        0
    } else {
        crate::pixel_count(width, height).saturating_mul(4)
    };
    rows.saturating_add(coefficients.max(reorient))
}

/// zune-jpeg decodes at most four components.
const MAX_COMPONENTS: usize = 4;

/// `n` rounded up to a multiple of `to`, as a `usize`.
fn pad(n: u32, to: u32) -> usize {
    usize::try_from(u64::from(n).div_ceil(u64::from(to)) * u64::from(to)).unwrap_or(usize::MAX)
}

/// What the memory preflight needs from the frame header and first scan
/// header.
struct FrameLayout {
    progressive: bool,
    width: u32,
    height: u32,
    /// (horizontal, vertical) sampling factor per component.
    sampling: Vec<(u8, u8)>,
    /// Components in the first scan.
    first_scan_components: usize,
}

impl FrameLayout {
    fn keeps_all_coefficients(&self) -> bool {
        self.progressive || self.first_scan_components != self.sampling.len()
    }

    /// Bytes of coefficient planes: each component is stored as whole MCUs
    /// (T.81 A.2.4), `8 * Hi` by `8 * Vi` samples per MCU, at 2 bytes each.
    fn coefficient_bytes(&self) -> usize {
        // A single-component scan is not interleaved: its MCU is one 8x8
        // block regardless of the sampling factors (T.81 A.2.2).
        if let [_] = self.sampling.as_slice() {
            return pad(self.width, 8)
                .saturating_mul(pad(self.height, 8))
                .saturating_mul(2);
        }
        let h_max = self.sampling.iter().map(|s| s.0).max().unwrap_or(1).max(1);
        let v_max = self.sampling.iter().map(|s| s.1).max().unwrap_or(1).max(1);
        let mcus_x = pad(self.width, 8 * u32::from(h_max)) / (8 * usize::from(h_max));
        let mcus_y = pad(self.height, 8 * u32::from(v_max)) / (8 * usize::from(v_max));
        let mcus = mcus_x.saturating_mul(mcus_y);
        self.sampling
            .iter()
            .map(|&(h, v)| {
                mcus.saturating_mul(64 * usize::from(h) * usize::from(v))
                    .saturating_mul(2)
            })
            .fold(0usize, usize::saturating_add)
    }
}

/// Walks the marker segments up to the first SOS (ITU-T T.81 B.1-B.2) and
/// reads the frame header (B.2.2) and the first scan header (B.2.3).
/// Returns `None` if the structure is not what it expects.
fn frame_layout(bytes: &[u8]) -> Option<FrameLayout> {
    let mut frame: Option<FrameLayout> = None;
    let mut pos = 2; // after SOI
    loop {
        // B.1.1.2: a marker is 0xFF then a code; extra 0xFF are fill bytes.
        if *bytes.get(pos)? != 0xFF {
            return None;
        }
        while *bytes.get(pos)? == 0xFF {
            pos += 1;
        }
        let code = *bytes.get(pos)?;
        pos += 1;
        // Markers without a length: TEM, RSTm, SOI, EOI (B.1.1.4 / Table B.1).
        if code == 0x01 || (0xD0..=0xD9).contains(&code) {
            continue;
        }
        let len = usize::from(u16::from_be_bytes([*bytes.get(pos)?, *bytes.get(pos + 1)?]));
        let segment = bytes.get(pos + 2..pos.checked_add(len)?)?;
        match code {
            // SOFn, excluding DHT (C4), JPG (C8) and DAC (CC).
            0xC0..=0xCF if !matches!(code, 0xC4 | 0xC8 | 0xCC) => {
                let height = u16::from_be_bytes([*segment.get(1)?, *segment.get(2)?]);
                let width = u16::from_be_bytes([*segment.get(3)?, *segment.get(4)?]);
                let count = usize::from(*segment.get(5)?);
                let sampling = (0..count)
                    .map(|i| {
                        let hv = *segment.get(6 + i * 3 + 1)?;
                        Some((hv >> 4, hv & 0x0F))
                    })
                    .collect::<Option<Vec<_>>>()?;
                frame = Some(FrameLayout {
                    // SOF2, SOF6, SOF10, SOF14 are progressive.
                    progressive: matches!(code, 0xC2 | 0xC6 | 0xCA | 0xCE),
                    width: u32::from(width),
                    height: u32::from(height),
                    sampling,
                    first_scan_components: 0,
                });
            }
            // SOS: the frame header must have come first.
            0xDA => {
                let mut frame = frame?;
                frame.first_scan_components = usize::from(*segment.first()?);
                return Some(frame);
            }
            _ => {}
        }
        pos = pos.checked_add(len)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(progressive: bool, sampling: &[(u8, u8)], first_scan: usize) -> FrameLayout {
        FrameLayout {
            progressive,
            width: 100,
            height: 50,
            sampling: sampling.to_vec(),
            first_scan_components: first_scan,
        }
    }

    #[test]
    fn coefficient_planes() {
        // 4:4:4: 13x7 MCUs of 8x8, three components.
        assert_eq!(
            layout(true, &[(1, 1); 3], 3).coefficient_bytes(),
            3 * 13 * 7 * 64 * 2
        );
        // 4:2:0: 7x4 MCUs of 16x16; luma has 4 blocks per MCU, chroma 1.
        assert_eq!(
            layout(true, &[(2, 2), (1, 1), (1, 1)], 3).coefficient_bytes(),
            7 * 4 * 64 * 2 * (4 + 1 + 1)
        );
        // Grayscale ignores its sampling factors.
        assert_eq!(layout(true, &[(2, 2)], 1).coefficient_bytes(), 104 * 56 * 2);
    }

    #[test]
    fn which_jpegs_keep_coefficients() {
        assert!(layout(true, &[(1, 1); 3], 3).keeps_all_coefficients());
        assert!(!layout(false, &[(1, 1); 3], 3).keeps_all_coefficients());
        // Sequential, but one component per scan.
        assert!(layout(false, &[(1, 1); 3], 1).keeps_all_coefficients());
    }

    #[test]
    fn layout_parser_is_total() {
        let mut jpeg = b"\xFF\xD8\xFF\xFF\xC2\x00\x11\x08\x00\x10\x00\x20\x03".to_vec();
        jpeg.extend_from_slice(b"\x01\x22\x00\x02\x11\x01\x03\x11\x01");
        jpeg.extend_from_slice(b"\xFF\xDA\x00\x08\x01\x01\x00\x00\x3F\x00");
        let l = frame_layout(&jpeg).unwrap();
        assert!(l.progressive);
        assert_eq!((l.width, l.height), (32, 16));
        assert_eq!(l.sampling, vec![(2, 2), (1, 1), (1, 1)]);
        assert_eq!(l.first_scan_components, 1);
        for len in 0..jpeg.len() {
            assert!(frame_layout(&jpeg[..len]).is_none());
        }
        // SOS before any SOF.
        assert!(frame_layout(b"\xFF\xD8\xFF\xDA\x00\x03\x01").is_none());
    }
}
