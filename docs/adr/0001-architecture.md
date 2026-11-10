# ADR-0001: Browser architecture and project principles

- **Status:** Accepted
- **Date:** 2026-11-10
- **Issue:** [#1](https://github.com/deepconcern/web_browser/issues/1) (the full spec; this ADR records the decisions)

## Context

Most of the browser market runs on Chromium, and the rest on Gecko or WebKit.
Whoever maintains an engine controls the extension model (see Manifest V3),
default privacy, telemetry and bundled features. We want a browser whose whole
stack is free software, with an engine independent of all three.

## Decision

1. **Hybrid engine strategy.** We own the architecture and every subsystem
   boundary. We may start on independent FOSS components behind our own APIs
   and replace them over time. The end goal is small independent libraries plus
   our own code.
2. **Rust**, with `unsafe` forbidden by default and no panics on web input.
3. **Crates by subsystem, in layers** (see `AGENTS.md`), enforced by
   `cargo xtask layers`.
4. **Process model:** a privileged browser process plus sandboxed per-site
   content processes (seccomp-bpf, namespaces, Landlock on Linux). The MVP runs
   in one process behind a message-passing boundary. Process isolation (M3) must
   land before JavaScript is on by default in any release.
5. **Dependencies:** licenses limited to the allowlist in `deny.toml`. No code
   from WebKit, Blink/Chromium, Gecko or their JS engines. From Servo, only small
   standalone crates (e.g. `cssparser`, `selectors`) as temporary scaffolding:
   no Stylo, WebRender, or Servo layout/script. HTML parsing is ours from the start.
6. **Clean-room rule:** other engines may be studied for behavior, never copied.
7. **Network policy:** no telemetry. The only automatic requests are update
   checks and filter-list downloads, both can be turned off, and a test enforces
   zero unprompted connections.
8. **No AI in core, ever.** Extensions may add it.
9. **Privacy defaults:** partitioned storage, third-party cookies blocked,
   content blocking on, HTTPS-only, no search deals. Plus an opt-in hardened
   mode with anti-fingerprinting.
10. **Extensions:** a built-in content blocker first. Later, a WebExtensions
    subset that permanently guarantees blocking `webRequest`.
11. **Platforms:** Linux first, then macOS and Windows. Mobile is out of scope for now.
12. **License:** MPL-2.0.
13. **UI:** `winit` + `wgpu` + `egui` for the MVP shell.
14. **Correctness:** html5lib-tests and web-platform-tests run in CI at pinned
    commits. A PR that lowers a pass count fails. Results are published in
    `docs/scorecard.md`.

## Consequences

- We can render real pages sooner by borrowing parts, and the seams keep each
  borrowed part replaceable.
- The conformance gate lets AI-assisted contributions move quickly without
  silently regressing.
- Anti-panic and no-`unsafe` rules make some code more verbose, which is the
  price of parsing hostile input safely.
- The single-process MVP is a temporary, documented risk, retired by M3.
