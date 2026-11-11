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
| `encoding_rs` | `wb-html` (decoding and charset labels) | (Apache-2.0 OR MIT) AND BSD-3-Clause | Independent crate (written for Gecko, standalone; implements the WHATWG Encoding Standard) | Keep for now; replace only if we write our own Encoding Standard decoders |
| WHATWG `entities.json` (data) | `wb-html` (named character reference table, generated into `src/entities.rs`) | CC BY 4.0 (HTML Living Standard) | WHATWG specification data | Keep (spec data, not code) |

Planned for M1, per the spec: `url`, `hyper`, `rustls`, `cssparser` (Servo-origin,
temporary), `fontdb`, `rustybuzz`/`harfrust`, `swash`, `image`, `rusqlite`,
`winit`, `wgpu`, `egui`. Each is added here when it lands.
