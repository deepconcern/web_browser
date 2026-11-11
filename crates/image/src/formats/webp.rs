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
    let (rgba_len, remaining) =
        limits.check_memory(width, height, working_bytes(&mut decoder, width, height))?;
    // image-webp applies this only to metadata chunks, which we don't read;
    // pass what is left so it can never exceed the budget either way.
    decoder.set_memory_limit(remaining);

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

/// Bytes image-webp 0.2 allocates besides our output, at the worst moment,
/// from what the chunk headers say (`set_memory_limit` doesn't cover
/// these). Per pixel of the canvas:
///
/// - lossless (VP8L): the predictor, color and Huffman-meta side images are
///   at most 1/16 of the image each at 4 bytes per pixel, so under 1 byte;
///   without alpha the image is decoded into a 4-byte RGBA intermediate
///   first.
/// - lossy (VP8): Y, U and V planes for whole 16x16 macroblocks (1.5 bytes
///   per padded pixel). An alpha plane (ALPH) can be lossless-compressed:
///   a 4-byte RGBA decode plus a 1-byte copy plus side images, 6 bytes.
/// - animated: a 4-byte canvas, a 4-byte frame buffer, and the worse of
///   the two frame kinds above (frames are no larger than the canvas).
fn working_bytes<R: std::io::BufRead + std::io::Seek>(
    decoder: &mut WebPDecoder<R>,
    width: u32,
    height: u32,
) -> usize {
    let pixels = crate::pixel_count(width, height);
    let per_pixel = |n: usize| pixels.saturating_mul(n);
    let yuv =
        crate::pixel_count(width.div_ceil(16), height.div_ceil(16)).saturating_mul(16 * 16 * 3) / 2;
    if decoder.is_animated() {
        per_pixel(4 + 4 + 6).saturating_add(yuv)
    } else if decoder.is_lossy() {
        let alpha = if decoder.has_alpha() { per_pixel(6) } else { 0 };
        yuv.saturating_add(alpha)
    } else if decoder.has_alpha() {
        per_pixel(1)
    } else {
        per_pixel(1 + 4)
    }
}
