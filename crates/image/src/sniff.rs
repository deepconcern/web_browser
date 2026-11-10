//! Image format sniffing.
//!
//! Implements "matching an image type pattern" from the WHATWG MIME
//! Sniffing Standard, §6.1 (Matching an image type pattern):
//! <https://mimesniff.spec.whatwg.org/#matching-an-image-type-pattern>.
//!
//! "Sniffing in an image context" (§8.2) uses the result of this algorithm
//! whenever it is defined, falling back to the supplied MIME type only when
//! no pattern matches. None of the formats we decode can be identified
//! without a pattern, so [`sniff`] ignores `Content-Type` altogether.

/// An image format that the WHATWG image type patterns can identify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFormat {
    /// PNG (`image/png`).
    Png,
    /// JPEG (`image/jpeg`).
    Jpeg,
    /// GIF, 87a or 89a (`image/gif`).
    Gif,
    /// WebP (`image/webp`).
    WebP,
    /// Windows bitmap (`image/bmp`). Recognized but not decoded in M1.
    Bmp,
    /// Windows icon or cursor (`image/x-icon`). Recognized but not decoded
    /// in M1.
    Icon,
}

impl ImageFormat {
    /// The MIME type the WHATWG image type pattern table gives this format.
    #[must_use]
    pub fn mime_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Icon => "image/x-icon",
        }
    }

    /// Whether [`crate::decode`] can decode this format.
    #[must_use]
    pub fn is_supported(self) -> bool {
        matches!(
            self,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
        )
    }
}

/// One row of the image type pattern table: pattern, mask, result.
/// No image pattern has leading bytes to ignore.
struct Pattern {
    pattern: &'static [u8],
    mask: &'static [u8],
    format: ImageFormat,
}

/// The table from §6.1, in the spec's order.
const PATTERNS: &[Pattern] = &[
    // A Windows Icon signature.
    Pattern {
        pattern: b"\x00\x00\x01\x00",
        mask: b"\xFF\xFF\xFF\xFF",
        format: ImageFormat::Icon,
    },
    // A Windows Cursor signature.
    Pattern {
        pattern: b"\x00\x00\x02\x00",
        mask: b"\xFF\xFF\xFF\xFF",
        format: ImageFormat::Icon,
    },
    // The string "BM", a BMP signature.
    Pattern {
        pattern: b"BM",
        mask: b"\xFF\xFF",
        format: ImageFormat::Bmp,
    },
    // The string "GIF87a", a GIF signature.
    Pattern {
        pattern: b"GIF87a",
        mask: b"\xFF\xFF\xFF\xFF\xFF\xFF",
        format: ImageFormat::Gif,
    },
    // The string "GIF89a", a GIF signature.
    Pattern {
        pattern: b"GIF89a",
        mask: b"\xFF\xFF\xFF\xFF\xFF\xFF",
        format: ImageFormat::Gif,
    },
    // "RIFF" followed by four bytes followed by "WEBPVP".
    Pattern {
        pattern: b"RIFF\x00\x00\x00\x00WEBPVP",
        mask: b"\xFF\xFF\xFF\xFF\x00\x00\x00\x00\xFF\xFF\xFF\xFF\xFF\xFF",
        format: ImageFormat::WebP,
    },
    // An error-checking byte followed by "PNG" followed by CR LF SUB LF,
    // the PNG signature.
    Pattern {
        pattern: b"\x89PNG\r\n\x1A\n",
        mask: b"\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF",
        format: ImageFormat::Png,
    },
    // The JPEG Start of Image marker followed by the indicator byte of
    // another marker.
    Pattern {
        pattern: b"\xFF\xD8\xFF",
        mask: b"\xFF\xFF\xFF",
        format: ImageFormat::Jpeg,
    },
];

/// Identifies an image format from the first bytes of a resource.
///
/// Runs the WHATWG "image type pattern matching algorithm" (MIME Sniffing
/// §6.1) over the resource header. Returns `None` when no pattern matches,
/// which the spec calls "undefined". Bytes past the first 1445 (the
/// resource header length, §5.2) are never looked at.
#[must_use]
pub fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    let header = bytes.get(..1445).unwrap_or(bytes);
    PATTERNS
        .iter()
        .find(|p| pattern_matches(header, p))
        .map(|p| p.format)
}

/// The "pattern matching algorithm" (§4.1 / §6) with an empty ignored set.
fn pattern_matches(input: &[u8], p: &Pattern) -> bool {
    // Step 2: the input must be at least as long as the pattern.
    let Some(prefix) = input.get(..p.pattern.len()) else {
        return false;
    };
    // Steps 5-6: every masked input byte must equal the pattern byte.
    prefix
        .iter()
        .zip(p.mask)
        .zip(p.pattern)
        .all(|((byte, mask), want)| byte & mask == *want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_well_formed() {
        for p in PATTERNS {
            assert_eq!(p.pattern.len(), p.mask.len());
            // Every pattern byte must survive its own mask.
            for (b, m) in p.pattern.iter().zip(p.mask) {
                assert_eq!(b & m, *b);
            }
        }
    }

    #[test]
    fn sniffs_each_signature() {
        let cases: &[(&[u8], ImageFormat)] = &[
            (b"\x00\x00\x01\x00rest", ImageFormat::Icon),
            (b"\x00\x00\x02\x00", ImageFormat::Icon),
            (b"BMxxxx", ImageFormat::Bmp),
            (b"GIF87a...", ImageFormat::Gif),
            (b"GIF89a", ImageFormat::Gif),
            (b"RIFF\x12\x34\x56\x78WEBPVP8 ", ImageFormat::WebP),
            (b"RIFF\xFF\xFF\xFF\xFFWEBPVP8L", ImageFormat::WebP),
            (b"RIFF\0\0\0\0WEBPVP8X", ImageFormat::WebP),
            (b"\x89PNG\r\n\x1A\n\0\0\0\rIHDR", ImageFormat::Png),
            (b"\xFF\xD8\xFF\xE0", ImageFormat::Jpeg),
            (b"\xFF\xD8\xFF", ImageFormat::Jpeg),
        ];
        for (bytes, want) in cases {
            assert_eq!(sniff(bytes), Some(*want), "{bytes:?}");
        }
    }

    #[test]
    fn rejects_near_misses() {
        let cases: &[&[u8]] = &[
            b"",
            b"B",
            b"GIF88a",
            b"GIF89",
            b"RIFF\0\0\0\0WAVEfmt ",
            b"RIFF\0\0\0\0WEBPV",
            b"\x89PNG\r\n\x1A",
            b"\x89PNG\n\r\x1A\n",
            b"\xFF\xD8",
            b"\xFF\xD8\xFE",
            b"\x00\x00\x03\x00",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ];
        for bytes in cases {
            assert_eq!(sniff(bytes), None, "{bytes:?}");
        }
    }

    #[test]
    fn mime_types() {
        assert_eq!(ImageFormat::Png.mime_type(), "image/png");
        assert_eq!(ImageFormat::Icon.mime_type(), "image/x-icon");
        assert!(ImageFormat::WebP.is_supported());
        assert!(!ImageFormat::Bmp.is_supported());
    }
}
