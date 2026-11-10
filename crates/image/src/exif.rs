//! Reading the orientation from EXIF metadata.
//!
//! EXIF data is a TIFF structure (TIFF 6.0 §2 "TIFF Structure"; CIPA DC-008
//! Exif 2.32 §4.5). We read only tag 0x0112 Orientation from the 0th IFD
//! (Exif 2.32 §4.6.4 A, "Orientation"); everything else is ignored.

/// The EXIF Orientation tag value: how the stored pixels must be transformed
/// for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Orientation {
    /// 1: no transform.
    Normal,
    /// 2: mirror left to right.
    FlipHorizontal,
    /// 3: rotate 180 degrees.
    Rotate180,
    /// 4: mirror top to bottom.
    FlipVertical,
    /// 5: mirror along the top-left to bottom-right diagonal.
    Transpose,
    /// 6: rotate 90 degrees clockwise.
    Rotate90,
    /// 7: mirror along the top-right to bottom-left diagonal.
    Transverse,
    /// 8: rotate 90 degrees counter-clockwise.
    Rotate270,
}

impl Orientation {
    /// Maps a tag value to an orientation. Values outside 1 to 8 are
    /// reserved; treat them as "no transform".
    fn from_tag(value: u16) -> Orientation {
        match value {
            2 => Orientation::FlipHorizontal,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipVertical,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => Orientation::Normal,
        }
    }

    /// Whether displaying with this orientation swaps width and height.
    pub(crate) fn swaps_dimensions(self) -> bool {
        matches!(
            self,
            Orientation::Transpose
                | Orientation::Rotate90
                | Orientation::Transverse
                | Orientation::Rotate270
        )
    }
}

/// Tag 0x0112, Orientation.
const ORIENTATION_TAG: u16 = 0x0112;
/// TIFF field type 3, SHORT (16-bit unsigned).
const TYPE_SHORT: u16 = 3;

/// Reads the orientation from a TIFF-structured EXIF payload (the bytes
/// after the APP1 `Exif\0\0` identifier). Missing, malformed or truncated
/// data yields [`Orientation::Normal`].
pub(crate) fn orientation(tiff: &[u8]) -> Orientation {
    read_orientation(tiff).map_or(Orientation::Normal, Orientation::from_tag)
}

fn read_orientation(tiff: &[u8]) -> Option<u16> {
    // TIFF 6.0 §2 "Image File Header": byte order, 42, offset of the 0th IFD.
    let big_endian = match tiff.get(0..2)? {
        b"II" => false,
        b"MM" => true,
        _ => return None,
    };
    let reader = Reader { tiff, big_endian };
    if reader.u16_at(2)? != 42 {
        return None;
    }
    let ifd = usize::try_from(reader.u32_at(4)?).ok()?;
    // "Image File Directory": a 2-byte entry count, then 12-byte entries of
    // tag, type, count and value-or-offset.
    let count = usize::from(reader.u16_at(ifd)?);
    (0..count).find_map(|i| {
        let entry = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
        if reader.u16_at(entry)? != ORIENTATION_TAG {
            return None;
        }
        if reader.u16_at(entry + 2)? != TYPE_SHORT || reader.u32_at(entry + 4)? != 1 {
            return Some(1);
        }
        // A single SHORT is stored left-justified in the 4-byte value field.
        reader.u16_at(entry + 8)
    })
}

struct Reader<'a> {
    tiff: &'a [u8],
    big_endian: bool,
}

impl Reader<'_> {
    fn u16_at(&self, at: usize) -> Option<u16> {
        let bytes: [u8; 2] = self.tiff.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.big_endian {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        let bytes: [u8; 4] = self.tiff.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    }
}

/// Builds a minimal TIFF/EXIF payload holding one Orientation entry, for
/// tests in this crate.
#[cfg(test)]
pub(crate) fn tiff_with_orientation(value: u16, big_endian: bool) -> Vec<u8> {
    let u16b = |v: u16| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let u32b = |v: u32| {
        if big_endian {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let mut t = Vec::new();
    t.extend_from_slice(if big_endian { b"MM" } else { b"II" });
    t.extend_from_slice(&u16b(42));
    t.extend_from_slice(&u32b(8));
    // IFD0 with two entries: an unrelated tag first, then Orientation.
    t.extend_from_slice(&u16b(2));
    t.extend_from_slice(&u16b(0x010F)); // Make, ASCII, count 4, inline.
    t.extend_from_slice(&u16b(2));
    t.extend_from_slice(&u32b(4));
    t.extend_from_slice(b"abc\0");
    t.extend_from_slice(&u16b(ORIENTATION_TAG));
    t.extend_from_slice(&u16b(TYPE_SHORT));
    t.extend_from_slice(&u32b(1));
    t.extend_from_slice(&u16b(value));
    t.extend_from_slice(&[0, 0]);
    t.extend_from_slice(&u32b(0)); // No next IFD.
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_byte_orders() {
        for be in [false, true] {
            for v in 1..=8 {
                let o = orientation(&tiff_with_orientation(v, be));
                assert_eq!(o, Orientation::from_tag(v));
            }
        }
        assert_eq!(
            orientation(&tiff_with_orientation(6, true)),
            Orientation::Rotate90
        );
        assert_eq!(
            orientation(&tiff_with_orientation(8, false)),
            Orientation::Rotate270
        );
    }

    #[test]
    fn reserved_values_are_normal() {
        for v in [0, 9, 0xFFFF] {
            assert_eq!(
                orientation(&tiff_with_orientation(v, false)),
                Orientation::Normal
            );
        }
    }

    #[test]
    fn malformed_is_normal() {
        let good = tiff_with_orientation(6, true);
        for len in 0..good.len() {
            // Truncations that cut off the value must not panic; ones that
            // keep it may still find it.
            let _ = orientation(&good[..len]);
        }
        assert_eq!(orientation(&good[..20]), Orientation::Normal);
        assert_eq!(orientation(b"XX\0\x2a\0\0\0\x08"), Orientation::Normal);
        let mut bad_magic = good.clone();
        bad_magic[3] = 43;
        assert_eq!(orientation(&bad_magic), Orientation::Normal);
        let mut far_ifd = good;
        far_ifd[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(orientation(&far_ifd), Orientation::Normal);
    }

    #[test]
    fn wrong_type_is_normal() {
        let mut t = tiff_with_orientation(6, false);
        // Orientation entry starts at 8 + 2 + 12; its type is 2 bytes in.
        t[22 + 2] = 4; // LONG instead of SHORT.
        assert_eq!(orientation(&t), Orientation::Normal);
    }
}
