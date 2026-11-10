//! Fetches conformance corpora at the commits pinned in `conformance/corpora.json`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::{Result, read_json};

#[derive(Debug)]
pub struct Corpus {
    pub name: String,
    pub repo: String,
    pub commit: String,
    pub enabled: bool,
    pub sparse: Vec<String>,
}

pub fn corpora_dir(root: &Path) -> PathBuf {
    root.join("target").join("corpora")
}

pub fn load(root: &Path) -> Result<Vec<Corpus>> {
    let json = read_json(&root.join("conformance").join("corpora.json"))?;
    let entries = json
        .get("corpora")
        .and_then(Value::as_array)
        .ok_or("corpora.json has no \"corpora\" array")?;
    entries
        .iter()
        .map(|entry| {
            let field = |key: &str| {
                entry
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| format!("corpus entry is missing \"{key}\""))
            };
            Ok(Corpus {
                name: field("name")?,
                repo: field("repo")?,
                commit: field("commit")?,
                enabled: entry
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                sparse: entry
                    .get("sparse")
                    .and_then(Value::as_array)
                    .map(|paths| {
                        paths
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

pub fn fetch(root: &Path) -> Result<()> {
    for corpus in load(root)? {
        if !corpus.enabled {
            println!("{}: disabled, skipping", corpus.name);
            continue;
        }
        let dir = corpora_dir(root).join(&corpus.name);
        if current_commit(&dir).as_deref() == Some(corpus.commit.as_str()) {
            println!("{}: already at {}", corpus.name, corpus.commit);
            continue;
        }
        println!(
            "{}: fetching {} at {}",
            corpus.name, corpus.repo, corpus.commit
        );
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        git(&dir, &["init", "--quiet"])?;
        git(&dir, &["remote", "add", "origin", &corpus.repo])?;
        if !corpus.sparse.is_empty() {
            let mut args = vec!["sparse-checkout", "set", "--no-cone"];
            args.extend(corpus.sparse.iter().map(String::as_str));
            git(&dir, &args)?;
        }
        let mut fetch = vec!["fetch", "--quiet", "--depth", "1"];
        if !corpus.sparse.is_empty() {
            // Only download the blobs the sparse checkout needs (WPT is huge).
            fetch.push("--filter=blob:none");
        }
        fetch.extend(["origin", corpus.commit.as_str()]);
        git(&dir, &fetch)?;
        git(&dir, &["checkout", "--quiet", "FETCH_HEAD"])?;
    }
    Ok(())
}

fn current_commit(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {} failed in {}",
            args.join(" "),
            dir.display()
        ))
    }
}
