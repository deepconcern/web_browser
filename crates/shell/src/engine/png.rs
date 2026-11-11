//! PNG encoding of rendered pages (for tests and reference rendering).

use std::fmt;

use wb_paint::Raster;

/// Why a raster could not be encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PngError(String);

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PNG encoding failed: {}", self.0)
    }
}

impl std::error::Error for PngError {}

/// Converts premultiplied RGBA8 pixels to straight (unpremultiplied)
/// RGBA8, as PNG and most image tools expect.
#[must_use]
pub fn unpremultiply(pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len());
    for px in pixels.chunks_exact(4) {
        if let [r, g, b, a] = *px {
            if a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let un = |c: u8| {
                    let v = (u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a);
                    u8::try_from(v.min(255)).unwrap_or(255)
                };
                out.extend_from_slice(&[un(r), un(g), un(b), a]);
            }
        }
    }
    out
}

/// Encodes a raster as an 8-bit RGBA PNG (straight alpha).
///
/// # Errors
///
/// Only if the encoder fails, which it should not for a valid raster.
pub fn encode_png(raster: &Raster) -> Result<Vec<u8>, PngError> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, raster.width(), raster.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| PngError(e.to_string()))?;
        writer
            .write_image_data(&unpremultiply(raster.pixels()))
            .map_err(|e| PngError(e.to_string()))?;
        writer.finish().map_err(|e| PngError(e.to_string()))?;
    }
    Ok(out)
}
