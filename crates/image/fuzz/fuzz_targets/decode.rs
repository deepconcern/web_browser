//! Fuzzes sniffing, probing and decoding of arbitrary bytes.
//!
//! Run from `crates/image`: `cargo +nightly fuzz run decode`. Seeding the
//! corpus with `tests/fixtures/*` helps it reach past the headers.

#![no_main]

use libfuzzer_sys::fuzz_target;
use wb_image::{ImageError, Limits, decode_with_limits, probe};

/// The crate catches a third-party decoder's panic and reports it as
/// `DecoderPanicked`; for fuzzing that is the bug we are looking for, so
/// turn it back into a crash with a reproducer.
fn no_decoder_panic<T>(result: &Result<T, ImageError>, what: &str) {
    if let Err(e @ ImageError::DecoderPanicked { .. }) = result {
        panic!("{what}: {e}");
    }
}

fuzz_target!(|data: &[u8]| {
    // Small limits keep each run fast and memory-bounded.
    let limits = Limits::new(2048, 16 * 1024 * 1024);
    let decoded = decode_with_limits(data, &limits);
    no_decoder_panic(&decoded, "decode");

    // Probe every input, not only the ones that decode.
    let probed = probe(data);
    no_decoder_panic(&probed, "probe");

    if let Ok(img) = &decoded {
        let len = img.width() as usize * img.height() as usize * 4;
        assert_eq!(img.pixels().len(), len);
        // Premultiplied: no color channel exceeds alpha.
        for px in img.pixels().chunks_exact(4) {
            assert!(px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]);
        }
        // Whatever decodes must probe to the same size.
        let info = probed.expect("decodable image must probe");
        assert_eq!((info.width, info.height), (img.width(), img.height()));
    }
});
