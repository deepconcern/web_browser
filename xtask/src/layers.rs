//! Enforces the crate layering from ADR-0001.
//!
//! Every `wb-*` crate declares `[package.metadata.layer] level = N`. A crate may
//! only depend on workspace crates with a strictly lower level.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::Result;

const PREFIX: &str = "wb-";

pub fn run(root: &Path) -> Result<()> {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cannot parse cargo metadata: {e}"))?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata has no packages")?;

    let violations = check(packages)?;
    if violations.is_empty() {
        println!("layers: ok");
        Ok(())
    } else {
        for v in &violations {
            eprintln!("layers: {v}");
        }
        Err(format!("{} layering violation(s)", violations.len()))
    }
}

/// Returns a list of human-readable violations.
fn check(packages: &[Value]) -> Result<Vec<String>> {
    let mut levels = BTreeMap::new();
    let mut violations = Vec::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !name.starts_with(PREFIX) {
            continue;
        }
        match package
            .pointer("/metadata/layer/level")
            .and_then(Value::as_u64)
        {
            Some(level) => {
                levels.insert(name.to_string(), level);
            }
            None => violations.push(format!("{name} has no [package.metadata.layer] level")),
        }
    }
    for package in packages {
        let name = package
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(&level) = levels.get(name) else {
            continue;
        };
        let deps = package
            .get("dependencies")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for dep in deps {
            let dep_name = dep.get("name").and_then(Value::as_str).unwrap_or_default();
            if let Some(&dep_level) = levels.get(dep_name)
                && dep_level >= level
            {
                violations.push(format!(
                    "{name} (layer {level}) depends on {dep_name} (layer {dep_level}); \
                     crates may only depend on lower layers"
                ));
            }
        }
    }
    Ok(violations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(name: &str, level: Option<u64>, deps: &[&str]) -> Value {
        let metadata = match level {
            Some(level) => json!({ "layer": { "level": level } }),
            None => Value::Null,
        };
        let deps: Vec<Value> = deps.iter().map(|d| json!({ "name": d })).collect();
        json!({ "name": name, "metadata": metadata, "dependencies": deps })
    }

    #[test]
    fn lower_layer_dependency_is_allowed() {
        let packages = [
            package("wb-a", Some(0), &[]),
            package("wb-b", Some(1), &["wb-a"]),
        ];
        assert!(check(&packages).unwrap().is_empty());
    }

    #[test]
    fn same_or_higher_layer_dependency_is_rejected() {
        let packages = [
            package("wb-a", Some(1), &["wb-b"]),
            package("wb-b", Some(1), &["wb-c"]),
            package("wb-c", Some(2), &[]),
        ];
        assert_eq!(check(&packages).unwrap().len(), 2);
    }

    #[test]
    fn missing_level_is_rejected() {
        let packages = [package("wb-a", None, &[])];
        assert_eq!(check(&packages).unwrap().len(), 1);
    }

    #[test]
    fn external_crates_are_ignored() {
        let packages = [
            package("wb-a", Some(0), &["serde_json"]),
            package("xtask", None, &[]),
        ];
        assert!(check(&packages).unwrap().is_empty());
    }
}
