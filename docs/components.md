# Borrowed components

Every third-party component the browser depends on, and whether we plan to
replace it with our own. Update this file in the same PR that adds or removes a
dependency. The end goal (ADR-0001) is small independent libraries plus our own
code.

| Component | Used by | License | Origin | Plan |
|---|---|---|---|---|
| `serde_json` | `xtask` (tooling only) | MIT/Apache-2.0 | Independent | Keep (not shipped in the browser) |
| `cssparser` 0.38 (default features off; pulls `dtoa-short`, `dtoa`, `itoa`, `smallvec`) | `wb-css` (tokenizer only, private) | MPL-2.0 (deps MIT/Apache-2.0, MPL-2.0) | Servo | Temporary scaffolding: replace with our own CSS Syntax 3 tokenizer |

Planned for M1, per the spec: `url`, `hyper`, `rustls`, `cssparser` (Servo-origin,
temporary), `fontdb`, `rustybuzz`/`harfrust`, `swash`, `image`, `rusqlite`,
`winit`, `wgpu`, `egui`. Each is added here when it lands.
