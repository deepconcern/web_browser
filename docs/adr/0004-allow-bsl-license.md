# ADR-0004: Allow the Boost Software License (BSL-1.0)

- **Status:** Accepted
- **Date:** 2026-11-11
- **Issue:** [#14](https://github.com/deepconcern/web_browser/issues/14) (M1 shell)

## Context

The shell needs system clipboard access on X11, Windows and macOS. Today it
only has Wayland support (`smithay-clipboard`, pulled in by `egui-winit`). The
usual cross-platform crate is `arboard`, which depends on `clipboard-win` and
`error-code`. Those two are licensed `BSL-1.0`, which is not on the allowlist.

The Boost Software License 1.0 is OSI-approved, FSF-free and GPL-compatible.
It is shorter and slightly more permissive than MIT: the notice must be kept
in source distributions, but not in binary-only distributions. It was missing
only because no dependency planned in #1 used it.

It is unrelated to the Business Source License (`BUSL-1.1`), which is
source-available but not open source and stays disallowed.

## Decision

Add `BSL-1.0` to the general license allowlist in `deny.toml`. We accept it as
a license, not just for one crate, since it is no more restrictive than MIT,
which is already allowed.

## Consequences

- The shell can use `arboard` for clipboard support on every desktop platform.
- Any future dependency under BSL-1.0 passes `cargo deny` without further
  review, the same as other allowlisted licenses.
