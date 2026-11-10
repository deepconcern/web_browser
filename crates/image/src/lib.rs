//! Image decoding for the browser.
//!
//! This crate turns the bytes of an image resource into an [`Image`]: a
//! width, a height and **premultiplied** RGBA8 pixels, ready for the painter
//! (tiny-skia wants premultiplied alpha). It supports PNG, JPEG, GIF (first
//! frame only) and WebP (first frame only for animations).
//!
//! The format is chosen by sniffing the bytes with the WHATWG MIME Sniffing
//! image patterns (see [`sniff`]), never by trusting a `Content-Type` header.
//!
//! Hostile input is the normal case here, so every entry point:
//! - checks the image's dimensions against [`Limits::max_dimension`] and the
//!   memory it would need against [`Limits::max_bytes`] *before* allocating
//!   the pixel buffer;
//! - returns an [`ImageError`] instead of panicking, including when a
//!   third-party decoder panics on malformed data.
//!
//! [`probe`] reads only the headers to get an image's size for layout.
//!
//! Color management: every image is assumed to be sRGB. Embedded ICC
//! profiles, PNG `gAMA`/`cHRM`/`sRGB` chunks and similar are ignored.
//! M1 follow-up: honor ICC profiles and PNG color chunks.
//!
//! The third-party decoders (`png`, `zune-jpeg`, `gif`, `image-webp`) are an
//! implementation detail: none of their types appear in this API.

mod exif;
mod formats;
mod pixels;
mod sniff;

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub use sniff::{ImageFormat, sniff};

/// A decoded image.
///
/// Pixels are stored row by row from the top-left corner, with no padding
/// between rows, as 8-bit **premultiplied** RGBA: each pixel is four bytes
/// `[r, g, b, a]` where `r`, `g` and `b` have already been multiplied by
/// `a / 255`. Color values are sRGB.
///
/// For JPEG images the EXIF orientation has already been applied, so
/// [`Image::width`] and [`Image::height`] are the dimensions as displayed.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Image {
    /// Makes an image from premultiplied RGBA8 pixels laid out as described
    /// on [`Image`].
    ///
    /// Returns `None` if `width` or `height` is zero, or if `pixels` is not
    /// exactly `width * height * 4` bytes long. The pixels are not checked to
    /// be validly premultiplied.
    #[must_use]
    pub fn from_premultiplied_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Option<Image> {
        let expected = rgba_len(width, height)?;
        (width > 0 && height > 0 && pixels.len() == expected).then_some(Image {
            width,
            height,
            pixels,
        })
    }

    /// The width in pixels. Always at least 1.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height in pixels. Always at least 1.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The premultiplied RGBA8 pixels, `width * height * 4` bytes, row by row
    /// from the top-left corner.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Consumes the image and returns its premultiplied RGBA8 pixel buffer.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
}

impl fmt::Debug for Image {
    // Leave out the pixel bytes: they are huge and unreadable.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pixels_len", &self.pixels.len())
            .finish()
    }
}

/// What [`probe`] learns from an image's headers without decoding its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageInfo {
    /// The sniffed format.
    pub format: ImageFormat,
    /// The width in pixels as displayed (after JPEG EXIF orientation).
    pub width: u32,
    /// The height in pixels as displayed (after JPEG EXIF orientation).
    pub height: u32,
}

/// Resource limits that protect against hostile images.
///
/// Both limits are checked from the image headers, before the pixel buffer
/// is allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    max_dimension: u32,
    max_bytes: usize,
}

impl Limits {
    /// The default largest width or height: 16384 pixels.
    pub const DEFAULT_MAX_DIMENSION: u32 = 16_384;
    /// The default memory budget for one decode: 256 MiB.
    pub const DEFAULT_MAX_BYTES: usize = 256 * 1024 * 1024;

    /// Makes limits with the given largest width or height in pixels and
    /// memory budget in bytes.
    #[must_use]
    pub const fn new(max_dimension: u32, max_bytes: usize) -> Limits {
        Limits {
            max_dimension,
            max_bytes,
        }
    }

    /// The largest width or height, in pixels, that will be decoded.
    #[must_use]
    pub const fn max_dimension(&self) -> u32 {
        self.max_dimension
    }

    /// The memory budget, in bytes, for the buffers one decode allocates
    /// (the final RGBA8 buffer plus any intermediate frame buffer). The
    /// budget is also passed on to the underlying decoders.
    #[must_use]
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Checks `width` x `height` against [`Limits::max_dimension`].
    fn check_dimensions(&self, width: u32, height: u32) -> Result<(), ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::ZeroSize);
        }
        if width > self.max_dimension || height > self.max_dimension {
            return Err(ImageError::DimensionsTooLarge {
                width,
                height,
                max: self.max_dimension,
            });
        }
        Ok(())
    }

    /// Checks that `buffers` RGBA8 buffers of `width` x `height` fit in the
    /// memory budget, and returns the size of one buffer.
    fn check_memory(&self, width: u32, height: u32, buffers: usize) -> Result<usize, ImageError> {
        let one = rgba_len(width, height).ok_or(ImageError::MemoryLimitExceeded {
            required: usize::MAX,
            limit: self.max_bytes,
        })?;
        let required = one.saturating_mul(buffers);
        if required > self.max_bytes {
            return Err(ImageError::MemoryLimitExceeded {
                required,
                limit: self.max_bytes,
            });
        }
        Ok(one)
    }
}

impl Default for Limits {
    fn default() -> Limits {
        Limits::new(Self::DEFAULT_MAX_DIMENSION, Self::DEFAULT_MAX_BYTES)
    }
}

/// Why an image could not be decoded or probed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImageError {
    /// The bytes match none of the WHATWG image type patterns.
    UnrecognizedFormat,
    /// The bytes are a known image format that this crate does not decode
    /// (BMP and ICO in M1).
    UnsupportedFormat(ImageFormat),
    /// The image has a zero width or height.
    ZeroSize,
    /// The image is wider or taller than [`Limits::max_dimension`].
    DimensionsTooLarge {
        /// The image's width in pixels.
        width: u32,
        /// The image's height in pixels.
        height: u32,
        /// The limit that was exceeded.
        max: u32,
    },
    /// Decoding would need more memory than [`Limits::max_bytes`].
    MemoryLimitExceeded {
        /// Bytes the decode would need (saturated at `usize::MAX`).
        required: usize,
        /// The configured limit.
        limit: usize,
    },
    /// The data is malformed or truncated, or uses a feature the decoder does
    /// not support.
    Malformed {
        /// The format being decoded.
        format: ImageFormat,
        /// A human-readable description from the decoder, for logs.
        message: String,
    },
}

impl ImageError {
    fn malformed(format: ImageFormat, message: impl fmt::Display) -> ImageError {
        ImageError::Malformed {
            format,
            message: message.to_string(),
        }
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::UnrecognizedFormat => f.write_str("unrecognized image format"),
            ImageError::UnsupportedFormat(format) => {
                write!(f, "unsupported image format: {}", format.mime_type())
            }
            ImageError::ZeroSize => f.write_str("image has zero width or height"),
            ImageError::DimensionsTooLarge { width, height, max } => {
                write!(f, "image is {width}x{height}, larger than {max} per side")
            }
            ImageError::MemoryLimitExceeded { required, limit } => write!(
                f,
                "image needs {required} bytes, more than the limit of {limit}"
            ),
            ImageError::Malformed { format, message } => {
                write!(f, "malformed {} image: {message}", format.mime_type())
            }
        }
    }
}

impl std::error::Error for ImageError {}

/// Decodes an image with the default [`Limits`].
///
/// The format is sniffed from the bytes (see [`sniff`]). GIF and animated
/// WebP yield their first frame; JPEG has its EXIF orientation applied.
///
/// # Errors
///
/// Returns an [`ImageError`] if the format is unrecognized or unsupported,
/// the image breaks the limits, or the data is malformed.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    decode_with_limits(bytes, &Limits::default())
}

/// Decodes an image, enforcing `limits`. See [`decode`].
///
/// # Errors
///
/// Returns an [`ImageError`] if the format is unrecognized or unsupported,
/// the image breaks the limits, or the data is malformed.
pub fn decode_with_limits(bytes: &[u8], limits: &Limits) -> Result<Image, ImageError> {
    let format = sniff(bytes).ok_or(ImageError::UnrecognizedFormat)?;
    guard(format, || match format {
        ImageFormat::Png => formats::png::decode(bytes, limits),
        ImageFormat::Jpeg => formats::jpeg::decode(bytes, limits),
        ImageFormat::Gif => formats::gif::decode(bytes, limits),
        ImageFormat::WebP => formats::webp::decode(bytes, limits),
        ImageFormat::Bmp | ImageFormat::Icon => Err(ImageError::UnsupportedFormat(format)),
    })
}

/// Reads an image's format and displayed size from its headers, without
/// decoding the pixels. Meant for layout, which needs the intrinsic size
/// before (or without) the pixels.
///
/// The size is the one [`decode`] would produce: JPEG EXIF orientation is
/// applied, and a GIF's size covers its first frame. Limits are not checked,
/// so a probe can succeed for an image that [`decode`] rejects.
///
/// # Errors
///
/// Returns an [`ImageError`] if the format is unrecognized or unsupported,
/// the size is zero, or the headers are malformed or truncated.
pub fn probe(bytes: &[u8]) -> Result<ImageInfo, ImageError> {
    let format = sniff(bytes).ok_or(ImageError::UnrecognizedFormat)?;
    let (width, height) = guard(format, || match format {
        ImageFormat::Png => formats::png::probe(bytes),
        ImageFormat::Jpeg => formats::jpeg::probe(bytes),
        ImageFormat::Gif => formats::gif::probe(bytes),
        ImageFormat::WebP => formats::webp::probe(bytes),
        ImageFormat::Bmp | ImageFormat::Icon => Err(ImageError::UnsupportedFormat(format)),
    })?;
    if width == 0 || height == 0 {
        return Err(ImageError::ZeroSize);
    }
    Ok(ImageInfo {
        format,
        width,
        height,
    })
}

/// Runs a third-party decoder, turning a panic inside it into an error.
///
/// Our own code never panics on web input (clippy enforces it), but the
/// borrowed decoders make no such promise. This is defense in depth for
/// builds that unwind; with `panic = "abort"` it changes nothing.
fn guard<T>(
    format: ImageFormat,
    run: impl FnOnce() -> Result<T, ImageError>,
) -> Result<T, ImageError> {
    catch_unwind(AssertUnwindSafe(run))
        .unwrap_or_else(|_| Err(ImageError::malformed(format, "decoder panicked")))
}

/// The byte length of a `width` x `height` RGBA8 buffer, if it fits in a
/// `usize`.
fn rgba_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}
