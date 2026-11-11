# Borrowed components

Every third-party component the browser depends on, and whether we plan to
replace it with our own. Update this file in the same PR that adds or removes a
dependency. The end goal (ADR-0001) is small independent libraries plus our own
code.

| Component | Used by | License | Origin | Plan |
|---|---|---|---|---|
| `serde_json` | `xtask` (tooling only) | MIT/Apache-2.0 | Independent | Keep (not shipped in the browser) |
| `png` | `wb-image` | MIT/Apache-2.0 (deps MIT/Apache-2.0/Zlib) | image-rs project | Keep for M1. Chosen over the `image` crate: same decoders, smaller tree |
| `zune-jpeg`, `zune-core` | `wb-image` | MIT/Apache-2.0/Zlib | Independent (zune-image) | Keep for M1 |
| `gif` | `wb-image` | MIT/Apache-2.0 (dep `weezl` MIT/Apache-2.0) | image-rs project | Keep for M1 |
| `image-webp` | `wb-image` | MIT/Apache-2.0 (deps MIT/Apache-2.0) | image-rs project | Keep for M1 |
| `fontdb` | `wb-text` | MIT | Independent | Keep for now; replace with our own font directory scan + fontconfig config reader later |
| `harfrust` | `wb-text` | MIT | HarfBuzz project (Rust port of HarfBuzz) | Keep (text shaping); pinned to 0.12 to share `read-fonts` 0.41 with `swash` |
| `swash` | `wb-text` | Apache-2.0/MIT | Independent | Keep (glyph scaling and rasterization) |
| `unicode-linebreak` | `wb-text` | Apache-2.0 | Independent | Keep (UAX #14 data and pair table) |
| `unicode-script` | `wb-text` | MIT/Apache-2.0 | Independent | Keep (Script property data) |
| `unicode-bidi` | `wb-text` | MIT/Apache-2.0 | Servo project, standalone crate | Temporary scaffolding; replace with our own UAX #9 when full bidi lands |
| `rusqlite` (with `libsqlite3-sys`, `bundled`) | `wb-storage` | MIT (bundled SQLite C library: public domain) | Independent | Keep |
| `tempfile` | `wb-storage` (dev-dependency, tests only) | MIT/Apache-2.0 | Independent | Keep (not shipped in the browser) |
| `url` | `wb-url` | MIT/Apache-2.0 | Servo project, standalone crate | Keep for M1; replace with our own WHATWG URL parser later |
| Public Suffix List data (`crates/url/data/public_suffix_list.dat`, fetched 2026-11-10 from github.com/publicsuffix/list, sha256 `7bd9c547…6da4`) | `wb-url` | MPL-2.0 | Mozilla / PSL project (data, not code) | Keep; refresh periodically. Lookup algorithm is our own |
| `hyper`, `hyper-util`, `http`, `http-body-util`, `tower-service` | `wb-net` | MIT (`http`: MIT/Apache-2.0) | Independent (hyperium) | Keep |
| `tokio`, `bytes` | `wb-net` | MIT | Independent | Keep (runtime is private to `wb-net`) |
| `rustls` (`ring` provider), `tokio-rustls`, `rustls-platform-verifier` | `wb-net` | Apache-2.0/ISC/MIT (`ring`: Apache-2.0 AND ISC) | Independent | Keep. No `aws-lc-rs`, no `webpki-roots` |
| `flate2` (miniz_oxide backend) | `wb-net` | MIT/Apache-2.0 | Independent | Keep |
| `brotli-decompressor` | `wb-net` | BSD-3-Clause/MIT | Dropbox | Keep |
| `cssparser` 0.38 (default features off; pulls `dtoa-short`, `dtoa`, `itoa`, `smallvec`) | `wb-css` (tokenizer only, private) | MPL-2.0 (deps MIT/Apache-2.0, MPL-2.0) | Servo | Temporary scaffolding: replace with our own CSS Syntax 3 tokenizer |
| `encoding_rs` | `wb-html` (decoding and charset labels) | (Apache-2.0 OR MIT) AND BSD-3-Clause | Independent crate (written for Gecko, standalone; implements the WHATWG Encoding Standard) | Keep for now; replace only if we write our own Encoding Standard decoders |
| WHATWG `entities.json` (data) | `wb-html` (named character reference table, generated into `src/entities.rs`) | CC BY 4.0 (HTML Living Standard) | WHATWG specification data | Keep (spec data, not code) |

Planned for M1, per the spec: `url`, `hyper`, `rustls`, `cssparser` (Servo-origin,
temporary), `fontdb`, `rustybuzz`/`harfrust`, `swash`, `image`, `rusqlite`,
`winit`, `wgpu`, `egui`. Each is added here when it lands.
