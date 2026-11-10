//! Project automation, run as `cargo xtask <command>`.
//!
//! Commands:
//! - `layers`: check that every crate only depends on crates in lower layers.
//! - `fetch-corpora`: fetch the conformance corpora at their pinned commits.
//! - `conformance [--check [--base <baseline.json>] | --update-baseline]`: run the
//!   conformance suites, regenerate `docs/scorecard.md`, and compare against the
//!   committed baseline (and optionally the target branch's baseline).

mod conformance;
mod corpora;
mod layers;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = workspace_root();
    let result = match args.first().map(String::as_str) {
        Some("layers") => layers::run(&root),
        Some("fetch-corpora") => corpora::fetch(&root),
        Some("conformance") => conformance::run(&root, args.get(1..).unwrap_or_default()),
        _ => Err(usage()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> String {
    "usage: cargo xtask <layers | fetch-corpora | \
     conformance [--check [--base <baseline.json>] | --update-baseline]>"
        .to_string()
}

/// The workspace root is the parent of this crate's directory.
fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map_or_else(|| manifest_dir.to_path_buf(), Path::to_path_buf)
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid JSON in {}: {e}", path.display()))
}
