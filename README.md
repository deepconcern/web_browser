# web_browser

A web browser built entirely on free and open source software, with its own
engine. It does not reuse WebKit, Blink/Chromium or Gecko.

> **Status:** M0 (foundations). There is nothing to browse yet. See the
> [architecture spec](https://github.com/deepconcern/web_browser/issues/1) and
> [ADR-0001](docs/adr/0001-architecture.md).

## Principles

1. **An independent engine.** No code from WebKit, Blink/Chromium or Gecko, so
   the web has another engine nobody controls.
2. **Free/open source dependencies only.**
3. **The user is in control.** Content blocking at full strength, guaranteed.
   Extensions will keep blocking `webRequest`.
4. **No telemetry.** The only automatic network traffic is update checks and
   filter-list downloads, and both can be turned off.
5. **No AI in core, ever.** Extensions may add it for users who want it.
6. **Private by default**, with an opt-in hardened mode.
7. **Lean.** No bundled services, promotions or lock-in.

## Building

Requires the Rust toolchain pinned in `rust-toolchain.toml` (installed
automatically by `rustup`).

```sh
cargo build
cargo run -p wb-shell
```

## Checks

Every PR must pass the same gate as CI:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask layers
cargo deny check                 # cargo install cargo-deny
cargo xtask fetch-corpora
cargo xtask conformance --check  # regenerates docs/scorecard.md
```

## Releasing

Releases are built by `.github/workflows/release.yml` for x86_64 Linux.

1. Bump `version` under `[workspace.package]` in `Cargo.toml` (e.g. to
   `0.1.0`), run `cargo build` to update `Cargo.lock`, and merge that to
   `main`.
2. Tag the merge commit and push the tag:
   ```sh
   git tag -a v0.1.0 -m "web_browser 0.1.0"
   git push origin v0.1.0
   ```
3. The workflow checks that the tag matches the workspace version, runs the
   CI gate, builds `web_browser` in release mode and publishes a GitHub
   Release with `web_browser-<version>-x86_64-linux.tar.gz` and `SHA256SUMS`.
   A version with a suffix (e.g. `0.1.0-beta.1`) becomes a pre-release.

To try it without releasing, run the workflow by hand from the Actions tab
(`workflow_dispatch`): it uploads the tarball and checksums as a workflow
artifact instead.

## Layout

| Path | What |
|---|---|
| `crates/` | One crate per subsystem (`wb-html`, `wb-css`, `wb-layout`, ...). |
| `xtask/` | Project automation (`cargo xtask ...`). |
| `conformance/` | Pinned test corpora and the pass-count baseline. |
| `docs/adr/` | Architecture Decision Records. |
| `docs/components.md` | Borrowed components and plans to replace them. |
| `docs/scorecard.md` | Conformance pass rates (generated). |
| `AGENTS.md` | Rules for contributors, human or AI. Read it first. |

## License

[MPL-2.0](LICENSE).
