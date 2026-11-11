//! GIF via the `gif` crate (GIF89a specification), first frame only.
//!
//! The first frame is drawn onto a transparent canvas the size of the
//! logical screen (GIF89a §18). The spec requires frames to fit inside the
//! logical screen but does not say what to do when they don't; we grow the
//! canvas to cover the first frame, so a GIF with a zero-sized or too-small
//! logical screen still shows its image. M1 follow-up: animation.

use std::num::NonZeroU64;

use ::gif::{ColorOutput, DecodeOptions, Decoder, MemoryLimit};

use crate::pixels::premultiply;
use crate::{Image, ImageError, ImageFormat, Limits};

const FORMAT: ImageFormat = ImageFormat::Gif;

fn malformed(e: impl std::fmt::Display) -> ImageError {
    ImageError::malformed(FORMAT, e)
}

/// The first frame's placement on the canvas.
struct Placement {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    canvas_width: u32,
    canvas_height: u32,
}

/// Reads the headers up to the first frame's image descriptor (no pixel
/// data is decoded).
fn first_frame(
    bytes: &[u8],
    memory_limit: usize,
) -> Result<(Decoder<&[u8]>, Placement), ImageError> {
    let mut options = DecodeOptions::new();
    options.set_color_output(ColorOutput::RGBA);
    let limit = u64::try_from(memory_limit)
        .ok()
        .and_then(NonZeroU64::new)
        .unwrap_or(NonZeroU64::MIN);
    options.set_memory_limit(MemoryLimit::Bytes(limit));
    let mut decoder = options.read_info(bytes).map_err(malformed)?;
    let screen_width = u32::from(decoder.width());
    let screen_height = u32::from(decoder.height());
    let frame = decoder
        .next_frame_info()
        .map_err(malformed)?
        .ok_or_else(|| malformed("no image data"))?;
    let left = u32::from(frame.left);
    let top = u32::from(frame.top);
    let width = u32::from(frame.width);
    let height = u32::from(frame.height);
    let placement = Placement {
        left,
        top,
        width,
        height,
        // u16 + u16 always fits in u32.
        canvas_width: screen_width.max(left + width),
        canvas_height: screen_height.max(top + height),
    };
    Ok((decoder, placement))
}

pub(crate) fn probe(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    let (_, p) = first_frame(bytes, usize::MAX)?;
    Ok((p.canvas_width, p.canvas_height))
}

pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<Image, ImageError> {
    let (mut decoder, p) = first_frame(bytes, limits.max_bytes())?;
    limits.check_dimensions(p.canvas_width, p.canvas_height)?;
    let fills_canvas =
        p.left == 0 && p.top == 0 && p.width == p.canvas_width && p.height == p.canvas_height;

    // To produce RGBA, the gif crate first decodes a frame's palette
    // indices into its own buffer, 1 byte per frame pixel.
    let frame_pixels = crate::pixel_count(p.width, p.height);

    if fills_canvas {
        // Common case: decode straight into the output buffer.
        let (rgba_len, _) = limits.check_memory(p.width, p.height, frame_pixels)?;
        let mut canvas = vec![0u8; rgba_len];
        decoder.read_into_buffer(&mut canvas).map_err(malformed)?;
        premultiply(&mut canvas);
        return Image::from_premultiplied_rgba8(p.canvas_width, p.canvas_height, canvas)
            .ok_or(ImageError::ZeroSize);
    }

    // The canvas plus a separate RGBA buffer for the frame.
    let frame_len = frame_pixels.saturating_mul(4);
    let (canvas_len, _) = limits.check_memory(
        p.canvas_width,
        p.canvas_height,
        frame_len.saturating_add(frame_pixels),
    )?;
    let mut canvas = vec![0u8; canvas_len];
    if frame_len > 0 {
        let mut frame = vec![0u8; frame_len];
        decoder.read_into_buffer(&mut frame).map_err(malformed)?;
        blit(&mut canvas, &frame, &p);
    }
    premultiply(&mut canvas);
    Image::from_premultiplied_rgba8(p.canvas_width, p.canvas_height, canvas)
        .ok_or(ImageError::ZeroSize)
}

/// Copies the frame's rows into the canvas. The canvas was sized to contain
/// the frame and starts fully transparent, so a plain copy gives the same
/// result as compositing.
fn blit(canvas: &mut [u8], frame: &[u8], p: &Placement) {
    let (Ok(width), Ok(canvas_width), Ok(left), Ok(top)) = (
        usize::try_from(p.width),
        usize::try_from(p.canvas_width),
        usize::try_from(p.left),
        usize::try_from(p.top),
    ) else {
        return;
    };
    let row_len = width * 4;
    if row_len == 0 {
        return;
    }
    for (y, row) in frame.chunks_exact(row_len).enumerate() {
        let start = ((top + y) * canvas_width + left) * 4;
        if let Some(dst) = canvas.get_mut(start..start + row_len) {
            dst.copy_from_slice(row);
        }
    }
}
