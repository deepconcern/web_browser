# ADR-0002: Paint backend

- **Status:** Accepted
- **Date:** 2026-11-11
- **Issue:** [#3](https://github.com/deepconcern/web_browser/issues/3) (M1, Q5), sub-issue #11

## Context

`wb-paint` turns a display list into pixels. M1 needs rectangles, borders with
elliptical corners, glyph masks, scaled images, clips (rectangular and rounded)
and opacity groups, at any device scale factor, and the result must be shown in
the `egui` shell. The reference rendering tests (issue #3 §B.2 item 13) also need
a headless renderer whose output is deterministic, so screenshots can be compared
in CI on machines without a GPU.

The options:

- **`tiny-skia`** (BSD-3-Clause, linebender). A CPU rasterizer: a Rust port of a
  subset of Skia's raster backend. Paths with antialiasing, fill rules, image
  patterns with nearest/bilinear/bicubic filtering, coverage masks, blend modes.
  Pure Rust, small dependency tree (`arrayref`, `arrayvec`, `bytemuck`,
  `cfg-if`, `log`, `strict-num`, `tiny-skia-path`), runs anywhere, and gives the
  same pixels on every machine.
- **`vello`** (Apache-2.0/MIT, linebender). A GPU compute renderer on `wgpu`.
  Fast on large, complex scenes, but it needs a GPU (or a software adapter) and
  its output varies slightly by driver. Still pre-1.0 with API churn.
- **Skia itself** (`skia-safe`) is C++ from the Chromium project: excluded by
  AGENTS.md, and a large native build.
- **WebRender** is Servo/Gecko engine-scale code: excluded by ADR-0001.
- **`raqote`** (BSD-3) is a CPU rasterizer with less development activity and
  fewer users than tiny-skia; **`femtovg`/`lyon`** are GPU tessellation
  approaches with the same determinism concerns as vello.
- **Our own rasterizer** is a lot of work (edge building, antialiased scan
  conversion, image sampling) that M1 doesn't need to own yet.

## Decision

1. M1 paints with **`tiny-skia` 0.12** on the CPU, into a premultiplied RGBA8
   buffer that the shell uploads to an `egui` texture each time the page changes.
   Default features are off; we enable `std` and `simd` only (no `png-format`:
   encoding PNGs is not paint's job; the reference tests can bring their own
   encoder).
2. tiny-skia is **private to `wb-paint`**. The public API is our own display
   list (`wb_paint::DisplayList`) in CSS px, a `Raster` of bytes, and our
   own geometry types. Most geometry (rounded rectangles, border regions, rect
   clips) is computed in our code as convex polygons; tiny-skia is used for
   antialiased polygon fills, image patterns, masks and layer compositing.
   This keeps the backend swappable.
3. Later, a **GPU backend with `vello`** may be added behind the same display
   list for speed. **tiny-skia stays the reference renderer**: the reference
   rendering tests always use it, and the GPU backend is checked against it.

## Consequences

- Rendering is deterministic and testable pixel by pixel on any CI machine.
- Full repaints of a large HiDPI window cost CPU time. `wb_paint::repaint`
  redraws only a dirty rectangle, and culls items outside it, to keep scrolling
  and selection updates cheap. Very heavy pages may be slow until the GPU backend
  exists.
- tiny-skia contains `unsafe` (for `bytemuck` casts in its SIMD types). Our
  `forbid(unsafe_code)` covers our crates only, as with other dependencies.
- Things tiny-skia lacks that CSS will want later (blur for `box-shadow` and
  `filter`, color glyphs) will be built on top of it in our code or wait for the
  GPU backend.
- `docs/components.md` records tiny-skia under "Keep for M1; vello later, with
  tiny-skia as the reference".
