# ADR-0003: Allow the NCSA license

- **Status:** Accepted
- **Date:** 2026-11-11
- **Issue:** [#3](https://github.com/deepconcern/web_browser/issues/3) (M1), raised on PRs #19 and #20

## Context

The M1 image and content blocker crates have `cargo-fuzz` targets. Those use
`libfuzzer-sys`, the standard Rust binding to LLVM's libFuzzer, whose license is
`(MIT OR Apache-2.0) AND NCSA`. The NCSA part covers the bundled libFuzzer C++
sources and can't be avoided.

The University of Illinois/NCSA Open Source License is OSI-approved and
FSF-free. Its terms are close to BSD-3-Clause: keep the copyright notice and
disclaimer, and don't use the authors' names to promote derived work. It is
compatible with everything already on the allowlist. It was missing only because
no dependency planned in #1 used it.

## Decision

Add `NCSA` to the general license allowlist in `deny.toml`. We accept it as a
license, not just for one crate, since it is no more restrictive than
BSD-3-Clause or Zlib, which are already allowed.

## Consequences

- Fuzz targets can use `libfuzzer-sys`.
- Any future dependency under NCSA passes `cargo deny` without further review,
  the same as other allowlisted licenses. Its notice must still be included
  wherever we ship it, like any BSD-style license.
