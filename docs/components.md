# Borrowed components

Every third-party component the browser depends on, and whether we plan to
replace it with our own. Update this file in the same PR that adds or removes a
dependency. The end goal (ADR-0001) is small independent libraries plus our own
code.

| Component | Used by | License | Origin | Plan |
|---|---|---|---|---|
| `serde_json` | `xtask` (tooling only) | MIT/Apache-2.0 | Independent | Keep (not shipped in the browser) |
| `fontdb` | `wb-text` | MIT | Independent | Keep for now; replace with our own font directory scan + fontconfig config reader later |
| `harfrust` | `wb-text` | MIT | HarfBuzz project (Rust port of HarfBuzz) | Keep (text shaping); pinned to 0.12 to share `read-fonts` 0.41 with `swash` |
| `swash` | `wb-text` | Apache-2.0/MIT | Independent | Keep (glyph scaling and rasterization) |
| `unicode-linebreak` | `wb-text` | Apache-2.0 | Independent | Keep (UAX #14 data and pair table) |
| `unicode-script` | `wb-text` | MIT/Apache-2.0 | Independent | Keep (Script property data) |
| `unicode-bidi` | `wb-text` | MIT/Apache-2.0 | Servo project, standalone crate | Temporary scaffolding; replace with our own UAX #9 when full bidi lands |
| `cssparser` 0.38 (default features off; pulls `dtoa-short`, `dtoa`, `itoa`, `smallvec`) | `wb-css` (tokenizer only, private) | MPL-2.0 (deps MIT/Apache-2.0, MPL-2.0) | Servo | Temporary scaffolding: replace with our own CSS Syntax 3 tokenizer |
| `encoding_rs` | `wb-html` (decoding and charset labels) | (Apache-2.0 OR MIT) AND BSD-3-Clause | Independent crate (written for Gecko, standalone; implements the WHATWG Encoding Standard) | Keep for now; replace only if we write our own Encoding Standard decoders |
| WHATWG `entities.json` (data) | `wb-html` (named character reference table, generated into `src/entities.rs`) | CC BY 4.0 (HTML Living Standard) | WHATWG specification data | Keep (spec data, not code) |

Planned for M1, per the spec: `url`, `hyper`, `rustls`, `cssparser` (Servo-origin,
temporary), `fontdb`, `rustybuzz`/`harfrust`, `swash`, `image`, `rusqlite`,
`winit`, `wgpu`, `egui`. Each is added here when it lands.
