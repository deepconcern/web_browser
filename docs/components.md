# Borrowed components

Every third-party component the browser depends on, and whether we plan to
replace it with our own. Update this file in the same PR that adds or removes a
dependency. The end goal (ADR-0001) is small independent libraries plus our own
code.

| Component | Used by | License | Origin | Plan |
|---|---|---|---|---|
| `serde_json` | `xtask` (tooling only) | MIT/Apache-2.0 | Independent | Keep (not shipped in the browser) |
| `regex` | `wb-blocker` (`/regex/` network filters) | MIT/Apache-2.0 (deps: `regex-automata`, `regex-syntax` MIT/Apache-2.0; `aho-corasick`, `memchr` Unlicense OR MIT) | Independent (rust-lang) | Keep |

Planned for M1, per the spec: `url`, `hyper`, `rustls`, `cssparser` (Servo-origin,
temporary), `fontdb`, `rustybuzz`/`harfrust`, `swash`, `image`, `rusqlite`,
`winit`, `wgpu`, `egui`. Each is added here when it lands.
