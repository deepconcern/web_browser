# AGENTS.md

Read this file first. It applies to every contributor, human or AI agent.

## The goal

A browser built only on free/open source software, with an engine that does not
come from WebKit, Blink/Chromium or Gecko. The architecture is in
[issue #1](https://github.com/deepconcern/web_browser/issues/1) and
[ADR-0001](docs/adr/0001-architecture.md).

## Workflow

1. Every feature starts as a GitHub issue: questions, then a spec.
2. Implementation waits until the spec has the `approved` label from the maintainer.
3. Work happens on a branch. The PR links its issue (`Closes #N` or `Part of #N`).
4. The maintainer reviews and merges. Never push to `main`, and never merge your own PR.
5. A significant design decision gets an ADR in `docs/adr/` (copy `0000-template.md`).

## Hard rules

- **Clean room.** You may study other engines' *behavior*: how a page renders,
  what a test expects. You may not copy, translate or paraphrase their *source
  code*. That covers WebKit, Blink/Chromium, Gecko, Servo, Ladybird and the rest.
  Implement from the specs (WHATWG, W3C, IETF, ECMA-262) and cite the spec
  section in comments where it helps.
- **Dependencies.** Only licenses allowed in `deny.toml`. Nothing from WebKit,
  Blink/Chromium, Gecko, V8, JavaScriptCore or SpiderMonkey. From Servo, only
  small standalone crates (e.g. `cssparser`, `selectors`) as temporary
  scaffolding. No Stylo, WebRender, or Servo layout/script. Record every
  borrowed component in `docs/components.md`.
- **Own the seams.** Borrowed code stays private to the crate that wraps it.
  Other crates use our API, never the borrowed crate's types.
- **No panics on web input.** Production code may not `unwrap`, `expect`,
  `panic!`, `todo!`, `unimplemented!`, `unreachable!` or index slices unchecked.
  Clippy enforces this. Handle the error or recover the way the spec says.
  Tests may do all of these.
- **No `unsafe`.** It's forbidden workspace-wide. An exception needs an ADR and
  an entry in the crate's own lint settings.
- **No telemetry.** Code may not make a network request the user didn't cause,
  except the ones listed in the network policy (update checks and filter-list
  downloads, both can be turned off).
- **No AI features in core.**
- **Layers.** Every crate declares `[package.metadata.layer] level` and may only
  depend on lower layers. `cargo xtask layers` enforces this.

| Level | Crates |
|---|---|
| 0 | `wb-url`, `wb-ipc` |
| 1 | `wb-dom`, `wb-css`, `wb-text`, `wb-image`, `wb-storage`, `wb-blocker` |
| 2 | `wb-html`, `wb-net`, `wb-js` |
| 3 | `wb-style` |
| 4 | `wb-layout` |
| 5 | `wb-paint` |
| 6 | `wb-shell` |

Changing a crate's level is an architecture change and needs an ADR.

## The gate

Before saying a change is done, run everything in the README's "Checks"
section. A PR may not lower any conformance pass count. If counts go up, run
`cargo xtask conformance --update-baseline` and commit both
`conformance/baseline.json` and `docs/scorecard.md`.

## Working in parallel

- Split work along crate boundaries: one area per PR.
- Each parallel agent uses its own `CARGO_TARGET_DIR` to avoid lock contention.
- Keep edits to shared files (`Cargo.toml`, `AGENTS.md`, `deny.toml`) small and
  surgical, and expect to rebase.
