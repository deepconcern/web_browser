//! WebP via `image-webp` (RFC 9649), lossy and lossless, with alpha.
//!
//! Animated WebP yields its first frame, composited on the canvas as the
//! container format specifies. M1 follow-up: animation. ICC profiles and
//! EXIF orientation in WebP are ignored.

use std::io::Cursor;

use ::image_webp::WebPDecoder;

use crate::pixels::{expand_to_rgba, premultiply};
use crate::{Image, ImageError, ImageFormat, Limits};

const FORMAT: ImageFormat = ImageFormat::WebP;

fn malformed(e: impl std::fmt::Display) -> ImageError {
    ImageError::malformed(FORMAT, e)
}

pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    let decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(malformed)?;
    Ok(decoder.dimensions())
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, ImageError> {
    // Construction parses the RIFF chunk headers only.
    let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(malformed)?;
    let (width, height) = decoder.dimensions();
    limits.check_dimensions(width, height)?;
    let rgba_len = limits.check_memory(width, height, 1)?;
    decoder.set_memory_limit(limits.max_bytes());

    let channels = if decoder.has_alpha() { 4 } else { 3 };
    let pixel_count = rgba_len / 4;
    let out_len = decoder
        .output_buffer_size()
        .ok_or_else(|| malformed("image too large"))?;
    if out_len != pixel_count.saturating_mul(channels) {
        return Err(malformed("unexpected output size"));
    }

    // One allocation: decode into the front, then widen to RGBA in place.
    let mut buf = vec![0u8; rgba_len];
    let decoded = buf
        .get_mut(..out_len)
        .ok_or_else(|| malformed("unexpected output size"))?;
    decoder.read_image(decoded).map_err(malformed)?;
    if !expand_to_rgba(&mut buf, pixel_count, channels) {
        return Err(malformed("unexpected output size"));
    }
    premultiply(&mut buf);
    Image::from_premultiplied_rgba8(width, height, buf).ok_or(ImageError::ZeroSize)
}
