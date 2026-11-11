//! PNG via the `png` crate (W3C PNG Specification, Third Edition).
//!
//! For APNG files this decodes the default image (the `IDAT` image), which
//! is what a non-animating decoder shows. M1 follow-up: APNG animation.

use std::io::Cursor;

use ::png::{BitDepth, ColorType, Decoder, Limits as PngLimits, Transformations};

use crate::pixels::{expand_to_rgba, premultiply};
use crate::{Image, ImageError, ImageFormat, Limits};

const FORMAT: ImageFormat = ImageFormat::Png;

fn malformed(e: impl std::fmt::Display) -> ImageError {
    ImageError::malformed(FORMAT, e)
}

pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    let info = decoder.read_header_info().map_err(malformed)?;
    Ok((info.width, info.height))
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, ImageError> {
    let mut decoder = Decoder::new_with_limits(
        Cursor::new(bytes),
        PngLimits {
            bytes: limits.max_bytes(),
        },
    );
    // Check the header before the decoder allocates anything sized by it.
    let (width, height) = {
        let info = decoder.read_header_info().map_err(malformed)?;
        (info.width, info.height)
    };
    limits.check_dimensions(width, height)?;
    // Our output buffer is the only image-sized allocation: the decoder
    // writes into it and otherwise works on rows. Its working storage is
    // the inflate window and tables, and a few rows (current, previous,
    // output line); the crate checks only some of that against its own
    // limit, so we budget it here and hand the decoder the rest.
    let working = working_bytes(width);
    let (rgba_len, remaining) = limits.check_memory(width, height, working)?;
    decoder.set_limits(PngLimits {
        bytes: working.saturating_add(remaining),
    });

    // Palette and low-bit-depth images expand to 8-bit gray or RGB, tRNS
    // becomes an alpha channel, and 16-bit samples are reduced to 8 bits.
    decoder.set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(malformed)?;

    let (color, depth) = reader.output_color_type();
    let channels = match color {
        ColorType::Grayscale => 1,
        ColorType::GrayscaleAlpha => 2,
        ColorType::Rgb => 3,
        ColorType::Rgba => 4,
        ColorType::Indexed => return Err(malformed("palette was not expanded")),
    };
    if depth != BitDepth::Eight {
        return Err(malformed("samples were not reduced to 8 bits"));
    }
    let out_len = reader
        .output_buffer_size()
        .ok_or_else(|| malformed("image too large"))?;
    let pixel_count = rgba_len / 4;
    if out_len != pixel_count.saturating_mul(channels) {
        return Err(malformed("unexpected output size"));
    }

    // One allocation: decode into the front, then widen to RGBA in place.
    let mut buf = vec![0u8; rgba_len];
    let decoded = buf
        .get_mut(..out_len)
        .ok_or_else(|| malformed("unexpected output size"))?;
    let frame = reader.next_frame(decoded).map_err(malformed)?;
    if frame.width != width || frame.height != height {
        return Err(malformed("first frame size differs from the header"));
    }
    if !expand_to_rgba(&mut buf, pixel_count, channels) {
        return Err(malformed("unexpected output size"));
    }
    premultiply(&mut buf);
    Image::from_premultiplied_rgba8(width, height, buf).ok_or(ImageError::ZeroSize)
}

/// The decoder's working storage for an image `width` pixels wide: four
/// rows at the widest PNG row (16-bit RGBA, 8 bytes per pixel, plus the
/// filter byte) and 128 KiB for the inflate window and tables.
pub(crate) fn working_bytes(width: u32) -> usize {
    let row = crate::pixel_count(width, 8).saturating_add(1);
    row.saturating_mul(4).saturating_add(128 * 1024)
}
