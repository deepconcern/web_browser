//! End-to-end tests: encode small images (in-test or from `tests/fixtures`)
//! and check what `decode` and `probe` return.

// Tests may unwrap, panic and index freely (AGENTS.md); clippy only knows
// that inside `#[test]` functions, not in this file's helpers.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::borrow::Cow;

use wb_image::{
    Image, ImageError, ImageFormat, ImageInfo, Limits, decode, decode_with_limits, probe, sniff,
};

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn pixel(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * img.width() + x) * 4) as usize;
    img.pixels()[at..at + 4].try_into().unwrap()
}

fn assert_close(got: [u8; 4], want: [u8; 4], tolerance: u8, what: &str) {
    for (g, w) in got.iter().zip(want) {
        assert!(
            g.abs_diff(w) <= tolerance,
            "{what}: got {got:?}, want {want:?} (±{tolerance})"
        );
    }
}

// ---------------------------------------------------------------- PNG

fn encode_png(
    width: u32,
    height: u32,
    color: png::ColorType,
    depth: png::BitDepth,
    data: &[u8],
    palette: Option<(&[u8], &[u8])>,
) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(color);
        enc.set_depth(depth);
        if let Some((plte, trns)) = palette {
            enc.set_palette(plte.to_vec());
            enc.set_trns(trns.to_vec());
        }
        let mut w = enc.write_header().unwrap();
        w.write_image_data(data).unwrap();
        w.finish().unwrap();
    }
    out
}

#[test]
fn png_rgba_is_premultiplied() {
    let data = [
        255, 0, 0, 255, // opaque red
        0, 255, 0, 128, // half green
        10, 20, 30, 0, // transparent: color must be zeroed
        200, 100, 50, 51, // 20%
    ];
    let bytes = encode_png(
        2,
        2,
        png::ColorType::Rgba,
        png::BitDepth::Eight,
        &data,
        None,
    );
    assert_eq!(sniff(&bytes), Some(ImageFormat::Png));
    let img = decode(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (2, 2));
    assert_eq!(
        img.pixels(),
        &[255, 0, 0, 255, 0, 128, 0, 128, 0, 0, 0, 0, 40, 20, 10, 51]
    );
}

#[test]
fn png_rgb_gray_and_gray_alpha() {
    let rgb = encode_png(
        2,
        1,
        png::ColorType::Rgb,
        png::BitDepth::Eight,
        &[1, 2, 3, 4, 5, 6],
        None,
    );
    assert_eq!(
        decode(&rgb).unwrap().pixels(),
        &[1, 2, 3, 255, 4, 5, 6, 255]
    );

    let gray = encode_png(
        2,
        1,
        png::ColorType::Grayscale,
        png::BitDepth::Eight,
        &[7, 200],
        None,
    );
    assert_eq!(
        decode(&gray).unwrap().pixels(),
        &[7, 7, 7, 255, 200, 200, 200, 255]
    );

    let ga = encode_png(
        1,
        1,
        png::ColorType::GrayscaleAlpha,
        png::BitDepth::Eight,
        &[200, 128],
        None,
    );
    assert_eq!(decode(&ga).unwrap().pixels(), &[100, 100, 100, 128]);
}

#[test]
fn png_low_and_high_bit_depths() {
    // 1-bit gray, 8 pixels in one byte: 1010_0000.
    let one_bit = encode_png(
        3,
        1,
        png::ColorType::Grayscale,
        png::BitDepth::One,
        &[0b1010_0000],
        None,
    );
    assert_eq!(
        decode(&one_bit).unwrap().pixels(),
        &[255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255]
    );

    // 16-bit RGBA: the high byte of each sample is kept.
    let sixteen = encode_png(
        1,
        1,
        png::ColorType::Rgba,
        png::BitDepth::Sixteen,
        &[0xFF, 0xFF, 0x80, 0x00, 0x00, 0x00, 0xFF, 0xFF],
        None,
    );
    assert_eq!(decode(&sixteen).unwrap().pixels(), &[255, 128, 0, 255]);
}

#[test]
fn png_palette_with_transparency() {
    let plte = [255, 0, 0, 0, 0, 255, 0, 255, 0];
    let trns = [255, 0, 102]; // entry 1 transparent, entry 2 40%
    let bytes = encode_png(
        3,
        1,
        png::ColorType::Indexed,
        png::BitDepth::Eight,
        &[0, 1, 2],
        Some((&plte, &trns)),
    );
    assert_eq!(
        decode(&bytes).unwrap().pixels(),
        &[255, 0, 0, 255, 0, 0, 0, 0, 0, 102, 0, 102]
    );
}

#[test]
fn png_probe_reads_only_the_header() {
    let bytes = encode_png(
        5,
        3,
        png::ColorType::Rgb,
        png::BitDepth::Eight,
        &[0; 45],
        None,
    );
    // The signature and IHDR are enough.
    let info = probe(&bytes[..33]).unwrap();
    assert_eq!(
        info,
        ImageInfo {
            format: ImageFormat::Png,
            width: 5,
            height: 3
        }
    );
    // But a full decode of the same prefix fails cleanly.
    assert!(matches!(
        decode(&bytes[..33]),
        Err(ImageError::Malformed {
            format: ImageFormat::Png,
            ..
        })
    ));
}

/// CRC-32 (ISO 3309, as used by PNG), for hand-built chunks.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A PNG signature and IHDR claiming the given size, with no image data.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut chunk = b"IHDR".to_vec();
    chunk.extend_from_slice(&width.to_be_bytes());
    chunk.extend_from_slice(&height.to_be_bytes());
    chunk.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    let mut out = b"\x89PNG\r\n\x1A\n".to_vec();
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&crc32(&chunk).to_be_bytes());
    out
}

#[test]
fn png_dimension_limit() {
    let bytes = png_header(20_000, 1);
    assert_eq!(probe(&bytes).unwrap().width, 20_000);
    assert_eq!(
        decode(&bytes),
        Err(ImageError::DimensionsTooLarge {
            width: 20_000,
            height: 1,
            max: 16_384
        })
    );
}

#[test]
fn png_memory_limit_checked_before_allocating() {
    // 16384 x 16384 RGBA is 1 GiB: within the dimension limit, over the
    // default memory budget. Must fail fast, without allocating.
    let bytes = png_header(16_384, 16_384);
    assert_eq!(
        decode(&bytes),
        Err(ImageError::MemoryLimitExceeded {
            // Output plus the decoder's rows and inflate state.
            required: (1 << 30) + 4 * (16_384 * 8 + 1) + 128 * 1024,
            limit: Limits::DEFAULT_MAX_BYTES
        })
    );

    let small = encode_png(
        8,
        8,
        png::ColorType::Rgb,
        png::BitDepth::Eight,
        &[0; 192],
        None,
    );
    assert_eq!(
        decode_with_limits(&small, &Limits::new(16_384, 100)),
        Err(ImageError::MemoryLimitExceeded {
            required: SMALL_PNG_BUDGET,
            limit: 100
        })
    );
    assert_eq!(
        decode_with_limits(&small, &Limits::new(4, 1 << 20)),
        Err(ImageError::DimensionsTooLarge {
            width: 8,
            height: 8,
            max: 4
        })
    );
    assert!(decode_with_limits(&small, &Limits::new(8, SMALL_PNG_BUDGET)).is_ok());
    assert!(decode_with_limits(&small, &Limits::new(8, SMALL_PNG_BUDGET - 1)).is_err());
}

/// An 8x8 PNG: 256 bytes of output, four 65-byte rows and 128 KiB of
/// inflate state.
const SMALL_PNG_BUDGET: usize = 256 + 4 * 65 + 128 * 1024;

#[test]
fn png_zero_size_is_an_error() {
    assert!(decode(&png_header(0, 5)).is_err());
    assert!(probe(&png_header(0, 5)).is_err());
}

// ---------------------------------------------------------------- JPEG

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// Checks the 32x16 "quadrants" layout: red, green on top; blue, white
/// below. Samples block centers, away from edges where JPEG rings.
fn assert_quadrants(img: &Image, tolerance: u8, what: &str) {
    assert_eq!((img.width(), img.height()), (32, 16), "{what}");
    assert_close(pixel(img, 8, 4), RED, tolerance, what);
    assert_close(pixel(img, 24, 4), GREEN, tolerance, what);
    assert_close(pixel(img, 8, 12), BLUE, tolerance, what);
    assert_close(pixel(img, 24, 12), WHITE, tolerance, what);
}

#[test]
fn jpeg_baseline_progressive_and_subsampled() {
    for (name, tolerance) in [
        ("quadrants.jpg", 8),
        ("quadrants-progressive.jpg", 8),
        ("quadrants-420.jpg", 12),
    ] {
        let bytes = fixture(name);
        assert_eq!(sniff(&bytes), Some(ImageFormat::Jpeg));
        let img = decode(&bytes).unwrap();
        assert_quadrants(&img, tolerance, name);
        assert!(img.pixels().chunks(4).all(|p| p[3] == 255), "{name}");
        assert_eq!(probe(&bytes).unwrap().width, 32);
    }
}

#[test]
fn jpeg_gray_and_cmyk() {
    let gray = decode(&fixture("gray.jpg")).unwrap();
    assert_eq!((gray.width(), gray.height()), (8, 8));
    assert_close(pixel(&gray, 4, 4), [128, 128, 128, 255], 2, "gray");

    let cmyk = decode(&fixture("quadrants-cmyk.jpg")).unwrap();
    assert_quadrants(&cmyk, 16, "cmyk");
}

/// Inserts an APP1 EXIF segment holding `tiff` right after the SOI marker.
fn with_exif(jpeg: &[u8], tiff: &[u8]) -> Vec<u8> {
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(tiff);
    let len = u16::try_from(payload.len() + 2).unwrap();
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// A TIFF structure with just the Orientation tag.
fn orientation_tiff(value: u16, big_endian: bool) -> Vec<u8> {
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
    let mut t = if big_endian { b"MM" } else { b"II" }.to_vec();
    t.extend_from_slice(&u16b(42));
    t.extend_from_slice(&u32b(8));
    t.extend_from_slice(&u16b(1));
    t.extend_from_slice(&u16b(0x0112));
    t.extend_from_slice(&u16b(3));
    t.extend_from_slice(&u32b(1));
    t.extend_from_slice(&u16b(value));
    t.extend_from_slice(&[0, 0]);
    t.extend_from_slice(&u32b(0));
    t
}

#[test]
fn jpeg_exif_orientation() {
    let base = fixture("quadrants.jpg");
    let upright = decode(&base).unwrap();
    let (w, h) = (upright.width(), upright.height());

    // For each orientation: displayed size, and which stored pixel shows up
    // at displayed (dx, dy). Written as an inverse map, independently of
    // the decoder's forward map.
    type Inverse = fn(u32, u32, u32, u32) -> (u32, u32);
    let cases: [(u16, bool, Inverse); 8] = [
        (1, false, |dx, dy, _, _| (dx, dy)),
        (2, false, |dx, dy, w, _| (w - 1 - dx, dy)),
        (3, false, |dx, dy, w, h| (w - 1 - dx, h - 1 - dy)),
        (4, false, |dx, dy, _, h| (dx, h - 1 - dy)),
        (5, true, |dx, dy, _, _| (dy, dx)),
        (6, true, |dx, dy, _, h| (dy, h - 1 - dx)),
        (7, true, |dx, dy, w, h| (w - 1 - dy, h - 1 - dx)),
        (8, true, |dx, dy, w, _| (w - 1 - dy, dx)),
    ];
    for big_endian in [false, true] {
        for (value, swaps, inverse) in cases {
            let bytes = with_exif(&base, &orientation_tiff(value, big_endian));
            let img = decode(&bytes).unwrap();
            let want = if swaps { (h, w) } else { (w, h) };
            assert_eq!((img.width(), img.height()), want, "orientation {value}");
            let info = probe(&bytes).unwrap();
            assert_eq!((info.width, info.height), want, "probe {value}");
            for dy in 0..img.height() {
                for dx in 0..img.width() {
                    let (sx, sy) = inverse(dx, dy, w, h);
                    assert_eq!(
                        pixel(&img, dx, dy),
                        pixel(&upright, sx, sy),
                        "orientation {value} at ({dx}, {dy})"
                    );
                }
            }
        }
    }

    // Spot check orientation 6 (rotate 90 degrees clockwise) by color: the
    // stored left column (red over blue) becomes the top row (blue, red).
    let rotated = decode(&with_exif(&base, &orientation_tiff(6, true))).unwrap();
    assert_close(pixel(&rotated, 4, 8), BLUE, 8, "rot90 top-left");
    assert_close(pixel(&rotated, 12, 8), RED, 8, "rot90 top-right");
    assert_close(pixel(&rotated, 12, 24), GREEN, 8, "rot90 bottom-right");
}

#[test]
fn jpeg_orientation_needs_memory_for_two_buffers() {
    let base = fixture("quadrants.jpg");
    let one_buffer = 32 * 16 * 4;
    let rows = 3 * 32 * 256; // zune-jpeg's row buffers
    let upright = one_buffer + rows;
    assert!(decode_with_limits(&base, &Limits::new(64, upright)).is_ok());
    let rotated = with_exif(&base, &orientation_tiff(6, false));
    assert_eq!(
        decode_with_limits(&rotated, &Limits::new(64, upright)),
        Err(ImageError::MemoryLimitExceeded {
            required: 2 * one_buffer + rows,
            limit: upright
        })
    );
    assert!(decode_with_limits(&rotated, &Limits::new(64, 2 * one_buffer + rows)).is_ok());
    assert_eq!(
        decode_with_limits(&base, &Limits::new(16, 1 << 20)),
        Err(ImageError::DimensionsTooLarge {
            width: 32,
            height: 16,
            max: 16
        })
    );
}

/// Rewrites the frame size in a JPEG's SOF segment.
fn with_jpeg_size(jpeg: &[u8], width: u16, height: u16) -> Vec<u8> {
    let mut out = jpeg.to_vec();
    let sof = out
        .windows(2)
        .position(|w| w[0] == 0xFF && matches!(w[1], 0xC0..=0xC2))
        .unwrap();
    // FF Cn, Lf (2), P (1), Y (2), X (2).
    out[sof + 5..sof + 7].copy_from_slice(&height.to_be_bytes());
    out[sof + 7..sof + 9].copy_from_slice(&width.to_be_bytes());
    out
}

#[test]
fn jpeg_budget_covers_decoder_buffers() {
    // 32x16, 4:4:4, three components. Output 2048 bytes; row buffers
    // 256 bytes per component per 32-padded column = 24576.
    let baseline = fixture("quadrants.jpg");
    assert_eq!(
        decode_with_limits(&baseline, &Limits::new(64, 26_623)),
        Err(ImageError::MemoryLimitExceeded {
            required: 26_624,
            limit: 26_623
        })
    );
    assert!(decode_with_limits(&baseline, &Limits::new(64, 26_624)).is_ok());

    // Progressive also keeps every coefficient: 3 components x 32x16
    // samples x 2 bytes = 3072.
    let progressive = fixture("quadrants-progressive.jpg");
    assert_eq!(
        decode_with_limits(&progressive, &Limits::new(64, 29_695)),
        Err(ImageError::MemoryLimitExceeded {
            required: 29_696,
            limit: 29_695
        })
    );
    assert!(decode_with_limits(&progressive, &Limits::new(64, 29_696)).is_ok());

    // A hostile header: 4096x4096 progressive 4:4:4. The 64 MiB output
    // fits in 128 MiB, but the 96 MiB of coefficients do not, so this must
    // be refused before decoding starts.
    let huge = with_jpeg_size(&progressive, 4096, 4096);
    assert_eq!(
        decode_with_limits(&huge, &Limits::new(16_384, 128 << 20)),
        Err(ImageError::MemoryLimitExceeded {
            required: (64 << 20) + (96 << 20) + 3 * 4096 * 256,
            limit: 128 << 20
        })
    );
}

// ---------------------------------------------------------------- GIF

/// One GIF frame: left, top, width, height, palette indices, transparent
/// index.
type GifFrame<'a> = (u16, u16, u16, u16, &'a [u8], Option<u8>);

/// Encodes a GIF with a global palette of red, green, blue, black, and the
/// given frames.
fn encode_gif(screen: (u16, u16), frames: &[GifFrame<'_>]) -> Vec<u8> {
    let palette = [255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
    let mut out = Vec::new();
    {
        let mut enc = gif::Encoder::new(&mut out, screen.0, screen.1, &palette).unwrap();
        for &(left, top, width, height, indices, transparent) in frames {
            let frame = gif::Frame {
                left,
                top,
                width,
                height,
                transparent,
                buffer: Cow::Borrowed(indices),
                ..gif::Frame::default()
            };
            enc.write_frame(&frame).unwrap();
        }
    }
    out
}

#[test]
fn gif_first_frame_with_transparency() {
    let bytes = encode_gif(
        (2, 2),
        &[
            (0, 0, 2, 2, &[0, 1, 2, 3], Some(3)),
            // A second frame must be ignored.
            (0, 0, 2, 2, &[2, 2, 2, 2], None),
        ],
    );
    assert_eq!(sniff(&bytes), Some(ImageFormat::Gif));
    let img = decode(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (2, 2));
    assert_eq!(
        img.pixels(),
        &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 0, 0]
    );
    assert_eq!(probe(&bytes).unwrap().width, 2);
}

#[test]
fn gif_frame_offset_on_larger_screen() {
    let bytes = encode_gif((4, 3), &[(1, 1, 2, 1, &[1, 2], None)]);
    let img = decode(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (4, 3));
    for y in 0..3 {
        for x in 0..4 {
            let want = match (x, y) {
                (1, 1) => GREEN,
                (2, 1) => BLUE,
                _ => [0, 0, 0, 0],
            };
            assert_eq!(pixel(&img, x, y), want, "({x}, {y})");
        }
    }
}

#[test]
fn gif_canvas_grows_to_cover_first_frame() {
    // A zero-sized logical screen: the canvas becomes the frame's extent.
    let bytes = encode_gif((0, 0), &[(1, 0, 1, 2, &[0, 2], None)]);
    let info = probe(&bytes).unwrap();
    assert_eq!((info.width, info.height), (2, 2));
    let img = decode(&bytes).unwrap();
    assert_eq!(pixel(&img, 0, 0), [0, 0, 0, 0]);
    assert_eq!(pixel(&img, 1, 0), RED);
    assert_eq!(pixel(&img, 1, 1), BLUE);
}

#[test]
fn gif_without_frames_is_an_error() {
    let bytes = encode_gif((2, 2), &[]);
    assert!(matches!(
        decode(&bytes),
        Err(ImageError::Malformed {
            format: ImageFormat::Gif,
            ..
        })
    ));
}

#[test]
fn gif_limits() {
    let bytes = encode_gif((300, 2), &[(0, 0, 1, 1, &[0], None)]);
    assert!(matches!(
        decode_with_limits(&bytes, &Limits::new(256, 1 << 20)),
        Err(ImageError::DimensionsTooLarge { width: 300, .. })
    ));
    // Canvas 2400 bytes + frame 4 bytes + the decoder's 1-byte-per-pixel
    // index buffer for the frame.
    assert!(matches!(
        decode_with_limits(&bytes, &Limits::new(1024, 2404)),
        Err(ImageError::MemoryLimitExceeded { required: 2405, .. })
    ));
    assert!(decode_with_limits(&bytes, &Limits::new(1024, 2405)).is_ok());

    // A frame that fills the canvas: output plus the index buffer.
    let full = encode_gif((4, 4), &[(0, 0, 4, 4, &[0; 16], None)]);
    assert!(matches!(
        decode_with_limits(&full, &Limits::new(1024, 79)),
        Err(ImageError::MemoryLimitExceeded { required: 80, .. })
    ));
    assert!(decode_with_limits(&full, &Limits::new(1024, 80)).is_ok());
}

// ---------------------------------------------------------------- WebP

fn encode_webp(width: u32, height: u32, color: image_webp::ColorType, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out)
        .encode(data, width, height, color)
        .unwrap();
    out
}

#[test]
fn webp_lossless() {
    let rgba = encode_webp(
        2,
        1,
        image_webp::ColorType::Rgba8,
        &[255, 0, 0, 255, 200, 100, 50, 128],
    );
    assert_eq!(sniff(&rgba), Some(ImageFormat::WebP));
    assert_eq!(
        decode(&rgba).unwrap().pixels(),
        &[255, 0, 0, 255, 100, 50, 25, 128]
    );

    let rgb = encode_webp(2, 1, image_webp::ColorType::Rgb8, &[1, 2, 3, 4, 5, 6]);
    assert_eq!(
        decode(&rgb).unwrap().pixels(),
        &[1, 2, 3, 255, 4, 5, 6, 255]
    );
    assert_eq!(
        probe(&rgb).unwrap(),
        ImageInfo {
            format: ImageFormat::WebP,
            width: 2,
            height: 1
        }
    );
}

#[test]
fn webp_lossy_and_alpha() {
    let lossy = decode(&fixture("quadrants-lossy.webp")).unwrap();
    assert_quadrants(&lossy, 24, "lossy webp");

    let alpha = decode(&fixture("alpha-lossy.webp")).unwrap();
    assert_eq!((alpha.width(), alpha.height()), (16, 16));
    assert_close(pixel(&alpha, 8, 3), RED, 24, "opaque half");
    assert_eq!(pixel(&alpha, 8, 12)[3], 0, "transparent half alpha");
    // Premultiplied: a transparent pixel has no color.
    assert_eq!(pixel(&alpha, 8, 12), [0, 0, 0, 0]);
}

#[test]
fn webp_animated_first_frame() {
    let bytes = fixture("animated.webp");
    let img = decode(&bytes).unwrap();
    assert_eq!((img.width(), img.height()), (16, 16));
    assert_eq!(pixel(&img, 8, 8), RED);
    assert_eq!(probe(&bytes).unwrap().width, 16);
}

#[test]
fn webp_limits() {
    // Lossless RGB: image-webp decodes into a full RGBA intermediate
    // (256 bytes) before our 256-byte output, and its transform side
    // images get 1 byte per pixel (64).
    let rgb = encode_webp(8, 8, image_webp::ColorType::Rgb8, &[9; 192]);
    assert_eq!(
        decode_with_limits(&rgb, &Limits::new(16_384, 575)),
        Err(ImageError::MemoryLimitExceeded {
            required: 576,
            limit: 575
        })
    );
    assert!(decode_with_limits(&rgb, &Limits::new(8, 576)).is_ok());

    // Lossless RGBA decodes straight into the output.
    let rgba = encode_webp(8, 8, image_webp::ColorType::Rgba8, &[9; 256]);
    assert_eq!(
        decode_with_limits(&rgba, &Limits::new(16_384, 319)),
        Err(ImageError::MemoryLimitExceeded {
            required: 320,
            limit: 319
        })
    );
    assert!(decode_with_limits(&rgba, &Limits::new(8, 320)).is_ok());
}

#[test]
fn webp_budget_covers_decoder_buffers() {
    // Lossy 32x16 RGB: output 2048 + YUV planes for 32x16 macroblocks
    // (32*16*3/2 = 768).
    let lossy = fixture("quadrants-lossy.webp");
    assert_eq!(
        decode_with_limits(&lossy, &Limits::new(64, 2815)),
        Err(ImageError::MemoryLimitExceeded {
            required: 2816,
            limit: 2815
        })
    );
    assert!(decode_with_limits(&lossy, &Limits::new(64, 2816)).is_ok());

    // Animated 16x16: output 1024 + canvas 1024 + frame 1024 + per-frame
    // working storage (YUV 384 + alpha 6 bytes per pixel = 1536).
    let animated = fixture("animated.webp");
    assert_eq!(
        decode_with_limits(&animated, &Limits::new(64, 4991)),
        Err(ImageError::MemoryLimitExceeded {
            required: 4992,
            limit: 4991
        })
    );
    assert!(decode_with_limits(&animated, &Limits::new(64, 4992)).is_ok());
}

// ---------------------------------------------------------------- Common

#[test]
fn unknown_and_unsupported_formats() {
    assert_eq!(decode(b""), Err(ImageError::UnrecognizedFormat));
    assert_eq!(probe(b"<svg/>"), Err(ImageError::UnrecognizedFormat));
    assert_eq!(
        decode(b"BM\0\0\0\0"),
        Err(ImageError::UnsupportedFormat(ImageFormat::Bmp))
    );
    assert_eq!(
        probe(b"\0\0\x01\0\x01\0"),
        Err(ImageError::UnsupportedFormat(ImageFormat::Icon))
    );
}

#[test]
fn format_comes_from_bytes_not_name() {
    // A PNG is a PNG whatever it claims to be; there is no way to pass a
    // Content-Type, by design.
    let bytes = encode_png(
        1,
        1,
        png::ColorType::Rgb,
        png::BitDepth::Eight,
        &[1, 2, 3],
        None,
    );
    assert_eq!(probe(&bytes).unwrap().format, ImageFormat::Png);
}

#[test]
fn image_constructor_validates() {
    assert!(Image::from_premultiplied_rgba8(1, 1, vec![0; 4]).is_some());
    assert!(Image::from_premultiplied_rgba8(1, 1, vec![0; 3]).is_none());
    assert!(Image::from_premultiplied_rgba8(0, 1, vec![]).is_none());
    let img = Image::from_premultiplied_rgba8(2, 1, vec![1; 8]).unwrap();
    assert_eq!(
        format!("{img:?}"),
        "Image { width: 2, height: 1, pixels_len: 8 }"
    );
    assert_eq!(img.into_pixels(), vec![1; 8]);
}

#[test]
fn errors_display() {
    let e = ImageError::DimensionsTooLarge {
        width: 1,
        height: 2,
        max: 0,
    };
    assert_eq!(e.to_string(), "image is 1x2, larger than 0 per side");
    assert_eq!(
        ImageError::UnsupportedFormat(ImageFormat::Bmp).to_string(),
        "unsupported image format: image/bmp"
    );
}

/// A tiny deterministic fuzz pass over every format: truncations and byte
/// flips must give an error or an image, never a panic (even one the crate
/// would catch: that would mean a decoder bug worth knowing about).
#[test]
fn corrupted_inputs_do_not_panic() {
    let mut inputs = vec![
        fixture("quadrants.jpg"),
        fixture("quadrants-progressive.jpg"),
        fixture("quadrants-lossy.webp"),
        fixture("alpha-lossy.webp"),
        fixture("animated.webp"),
        encode_png(
            4,
            4,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            &[77; 64],
            None,
        ),
        encode_gif((4, 4), &[(1, 1, 2, 2, &[0, 1, 2, 3], Some(1))]),
        encode_webp(4, 4, image_webp::ColorType::Rgba8, &[200; 64]),
    ];
    inputs.push(with_exif(&inputs[0], &orientation_tiff(7, true)));

    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let check = |bytes: &[u8]| {
        for result in [decode(bytes).map(|_| ()), probe(bytes).map(|_| ())] {
            assert!(
                !matches!(result, Err(ImageError::DecoderPanicked { .. })),
                "input {bytes:?}"
            );
        }
    };
    for input in &inputs {
        for len in 0..input.len() {
            check(&input[..len]);
        }
        for _ in 0..300 {
            let mut bytes = input.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() as usize) % bytes.len();
                bytes[at] = next() as u8;
            }
            check(&bytes);
        }
    }
}
