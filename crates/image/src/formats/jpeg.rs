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
    // Reorienting needs a second buffer.
    let buffers = if orientation == Orientation::Normal {
        1
    } else {
        2
    };
    let rgba_len = limits.check_memory(width, height, buffers)?;
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
