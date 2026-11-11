# Borrowed components

Every third-party component the browser depends on, and whether we plan to
replace it with our own. Update this file in the same PR that adds or removes a
dependency. The end goal (ADR-0001) is small independent libraries plus our own
code.

| Component | Used by | License | Origin | Plan |
|---|---|---|---|---|
| `serde_json` | `xtask` (tooling only) | MIT/Apache-2.0 | Independent | Keep (not shipped in the browser) |
| `png` | `wb-image`, `wb-shell` (encoding rendered pages for tests and reference rendering) | MIT/Apache-2.0 (deps MIT/Apache-2.0/Zlib) | image-rs project | Keep for M1. Chosen over the `image` crate: same decoders, smaller tree |
| `zune-jpeg`, `zune-core` | `wb-image` | MIT/Apache-2.0/Zlib | Independent (zune-image) | Keep for M1 |
| `gif` | `wb-image` | MIT/Apache-2.0 (dep `weezl` MIT/Apache-2.0) | image-rs project | Keep for M1 |
| `image-webp` | `wb-image` | MIT/Apache-2.0 (deps MIT/Apache-2.0) | image-rs project | Keep for M1 |
| `regex` | `wb-blocker` (`/regex/` network filters) | MIT/Apache-2.0 (deps: `regex-automata`, `regex-syntax` MIT/Apache-2.0; `aho-corasick`, `memchr` Unlicense OR MIT) | Independent (rust-lang) | Keep |
| `fontdb` | `wb-text`, `wb-shell` (finding installed fonts for the egui UI, which has its bundled fonts off) | MIT | Independent | Keep for now; replace with our own font directory scan + fontconfig config reader later |
| `harfrust` | `wb-text` | MIT | HarfBuzz project (Rust port of HarfBuzz) | Keep (text shaping); pinned to 0.12 to share `read-fonts` 0.41 with `swash` |
| `swash` | `wb-text` | Apache-2.0/MIT | Independent | Keep (glyph scaling and rasterization) |
| `unicode-linebreak` | `wb-text` | Apache-2.0 | Independent | Keep (UAX #14 data and pair table) |
| `unicode-script` | `wb-text` | MIT/Apache-2.0 | Independent | Keep (Script property data) |
| `unicode-bidi` | `wb-text` | MIT/Apache-2.0 | Servo project, standalone crate | Temporary scaffolding; replace with our own UAX #9 when full bidi lands |
| `rusqlite` (with `libsqlite3-sys`, `bundled`) | `wb-storage` | MIT (bundled SQLite C library: public domain) | Independent | Keep |
| `tempfile` | `wb-storage`, `wb-shell` (dev-dependency, tests only) | MIT/Apache-2.0 | Independent | Keep (not shipped in the browser) |
| `url` | `wb-url` | MIT/Apache-2.0 | Servo project, standalone crate | Keep for M1; replace with our own WHATWG URL parser later |
| Public Suffix List data (`crates/url/data/public_suffix_list.dat`, fetched 2026-11-10 from github.com/publicsuffix/list, sha256 `7bd9c547…6da4`) | `wb-url` | MPL-2.0 | Mozilla / PSL project (data, not code) | Keep; refresh periodically. Lookup algorithm is our own |
| `hyper`, `hyper-util`, `http`, `http-body-util`, `tower-service` | `wb-net` | MIT (`http`: MIT/Apache-2.0) | Independent (hyperium) | Keep |
| `tokio`, `bytes` | `wb-net` | MIT | Independent | Keep (runtime is private to `wb-net`) |
| `rustls` (`ring` provider), `tokio-rustls`, `rustls-platform-verifier` | `wb-net` | Apache-2.0/ISC/MIT (`ring`: Apache-2.0 AND ISC) | Independent | Keep. No `aws-lc-rs`, no `webpki-roots` |
| `flate2` (miniz_oxide backend) | `wb-net` | MIT/Apache-2.0 | Independent | Keep |
| `brotli-decompressor` | `wb-net` | BSD-3-Clause/MIT | Dropbox | Keep |
| `cssparser` 0.38 (default features off; pulls `dtoa-short`, `dtoa`, `itoa`, `smallvec`) | `wb-css` (tokenizer only, private) | MPL-2.0 (deps MIT/Apache-2.0, MPL-2.0) | Servo | Temporary scaffolding: replace with our own CSS Syntax 3 tokenizer |
| `encoding_rs` | `wb-html` (decoding and charset labels), `wb-shell` (stylesheet decoding, CSS Syntax 3 §3.2) | (Apache-2.0 OR MIT) AND BSD-3-Clause | Independent crate (written for Gecko, standalone; implements the WHATWG Encoding Standard) | Keep for now; replace only if we write our own Encoding Standard decoders |
| WHATWG `entities.json` (data) | `wb-html` (named character reference table, generated into `src/entities.rs`) | CC BY 4.0 (HTML Living Standard) | WHATWG specification data | Keep (spec data, not code) |
| WHATWG HTML §15 Rendering UA stylesheet (adapted) | `wb-style` (`src/ua/html.css`, `src/ua/quirks.css`) | CC BY 4.0 (HTML Living Standard) | WHATWG specification text | Keep (spec data, not code); attribution in the file header |
| `tiny-skia` 0.12 (features `std`, `simd`; pulls `tiny-skia-path`, `arrayref`, `arrayvec`, `bytemuck`, `cfg-if`, `log`, `strict-num`) | `wb-paint` (rasterizer, private) | BSD-3-Clause (deps: `arrayref` BSD-2-Clause, `strict-num` MIT, others MIT/Apache-2.0 or Zlib/Apache-2.0/MIT) | Independent (linebender; Rust port of a subset of Skia's raster backend, not Chromium code) | Keep for M1 (ADR-0002); a `vello` GPU backend may come later, with tiny-skia kept as the reference renderer |
| `winit` 0.30 (features `x11`, `wayland`, `wayland-dlopen`, `wayland-csd-adwaita-notitle`, `rwh_06`) | `wb-shell` (windows and input) | Apache-2.0 (deps MIT/Apache-2.0, MIT, ISC, Zlib) | Independent (rust-windowing) | Keep |
| `wgpu` 30 (features `vulkan`, `metal`, `dx12`, `gles`, `wgsl`; pulls `wgpu-core`, `wgpu-hal`, `wgpu-types`, `naga`) | `wb-shell` (presenting the UI and page frames) | MIT/Apache-2.0 | Independent (gfx-rs) | Keep |
| `egui`, `egui-winit`, `egui-wgpu` 0.37 (default features off: no bundled fonts, which are OFL/UFL; `egui-winit` with `smithay-clipboard`, MIT) | `wb-shell` (browser chrome: tabs, toolbar, settings, first-run screen) | MIT/Apache-2.0 | Independent (emilk) | Keep for M1; the page itself is rendered by our engine, egui only draws the chrome |
| `pollster` 0.4 | `wb-shell` (blocking on wgpu adapter and device setup) | MIT/Apache-2.0 | Independent | Keep |

Planned for M1, per the spec: `url`, `hyper`, `rustls`, `cssparser` (Servo-origin,
temporary), `fontdb`, `rustybuzz`/`harfrust`, `swash`, `image`, `rusqlite`,
`winit`, `wgpu`, `egui`. Each is added here when it lands.
