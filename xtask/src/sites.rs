//! Reference rendering of the M1 target sites (issue #3 §B.2 item 13).
//!
//! The pages themselves are not committed (they are third-party content).
//! `conformance/sites/manifest.json` pins every site: the page URL, the
//! viewport it is rendered at, and every resource the engine fetched when
//! it was snapshotted, each with the archived source it was downloaded
//! from and its SHA-256. `cargo xtask snapshots` (see [`crate::snapshots`])
//! downloads them into `target/snapshots/<site>/<sha256>`.
//!
//! Rendering never touches the network: every resource is served from the
//! snapshot by a [`LocalFetcher`] under its original URL, so relative
//! links resolve as they did on the live site and anything not in the
//! snapshot is a 404.
//!
//! `cargo xtask sites` renders each site and compares it with the reviewed
//! reference image in `conformance/sites/reference/<site>.png` (see
//! [`crate::imgdiff`] for the metric). `--update` rewrites the references
//! instead; review them by hand before committing. The result per site is
//! recorded in `conformance/sites/status.json` and becomes the "Target
//! sites" row of `docs/scorecard.md`.
//!
//! `cargo test -p xtask` runs the same comparison for every site whose
//! snapshot and reference are present, and skips the rest with a message,
//! so a plain `cargo test` stays offline.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use wb_shell::engine::{Engine, LocalFetcher, NoObserver, RenderOptions, unpremultiply};
use wb_text::FontContext;
use wb_url::Url;

use crate::imgdiff::{self, Image, Thresholds};
use crate::sha256::hex_digest;
use crate::{Result, read_json};

/// How many rounds of late image loads (images that styles only refer to
/// once laid out) a render waits for.
const MAX_IMAGE_ROUNDS: usize = 4;

/// Largest viewport side accepted from the manifest, in CSS px.
const MAX_VIEWPORT: u32 = 4096;

/// One pinned resource of a site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// The URL the page asked for (and the snapshot serves it under).
    pub url: String,
    /// Where the bytes were downloaded from (an archived copy).
    pub source: String,
    /// The `Content-Type` it was served with.
    pub content_type: String,
    /// SHA-256 of the body, lowercase hex.
    pub sha256: String,
}

/// One target site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    /// Short name; also the snapshot directory and reference file name.
    pub name: String,
    /// The page URL.
    pub url: String,
    /// The archive timestamp (`YYYYMMDDhhmmss`) the snapshot is pinned to.
    pub timestamp: String,
    /// Viewport width in CSS px.
    pub width: u32,
    /// Viewport height in CSS px.
    pub height: u32,
    /// Every resource fetched while rendering, document first. Empty
    /// until `cargo xtask snapshots --discover` has run.
    pub resources: Vec<Resource>,
}

/// The contents of `conformance/sites/manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// A comment kept at the top of the file.
    pub comment: String,
    /// The archive URL template, with `{timestamp}` and `{url}`.
    pub archive: String,
    /// The target sites.
    pub sites: Vec<Site>,
}

pub fn sites_dir(root: &Path) -> PathBuf {
    root.join("conformance").join("sites")
}

pub fn manifest_path(root: &Path) -> PathBuf {
    sites_dir(root).join("manifest.json")
}

pub fn snapshots_dir(root: &Path) -> PathBuf {
    root.join("target").join("snapshots")
}

fn reference_path(root: &Path, site: &str) -> PathBuf {
    sites_dir(root)
        .join("reference")
        .join(format!("{site}.png"))
}

fn status_path(root: &Path) -> PathBuf {
    sites_dir(root).join("status.json")
}

/// Where renderings and diff images for review go.
fn output_dir(root: &Path) -> PathBuf {
    root.join("target").join("reftests")
}

/// A site name must be safe to use as a file name.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Manifest {
    pub fn load(root: &Path) -> Result<Manifest> {
        Manifest::from_json(&read_json(&manifest_path(root))?)
    }

    pub fn from_json(json: &Value) -> Result<Manifest> {
        let text = |v: &Value, key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("manifest entry is missing \"{key}\""))
        };
        let number = |v: &Value, key: &str| {
            v.get(key)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| (1..=MAX_VIEWPORT).contains(n))
                .ok_or_else(|| format!("manifest \"{key}\" must be 1..={MAX_VIEWPORT}"))
        };
        let mut sites = Vec::new();
        for s in json
            .get("sites")
            .and_then(Value::as_array)
            .ok_or("manifest has no \"sites\" array")?
        {
            let name = text(s, "name")?;
            if !valid_name(&name) {
                return Err(format!("invalid site name {name:?}"));
            }
            let mut resources = Vec::new();
            for r in s
                .get("resources")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let resource = Resource {
                    url: text(r, "url")?,
                    source: text(r, "source")?,
                    content_type: text(r, "content_type")?,
                    sha256: text(r, "sha256")?,
                };
                if !valid_sha256(&resource.sha256) {
                    return Err(format!("{name}: bad sha256 for {}", resource.url));
                }
                resources.push(resource);
            }
            sites.push(Site {
                url: text(s, "url")?,
                timestamp: text(s, "timestamp")?,
                width: number(s, "width")?,
                height: number(s, "height")?,
                name,
                resources,
            });
        }
        Ok(Manifest {
            comment: json
                .get("_comment")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            archive: text(json, "archive")?,
            sites,
        })
    }

    pub fn to_json(&self) -> Value {
        let sites: Vec<Value> = self
            .sites
            .iter()
            .map(|s| {
                let resources: Vec<Value> = s
                    .resources
                    .iter()
                    .map(|r| {
                        json!({
                            "url": r.url,
                            "source": r.source,
                            "content_type": r.content_type,
                            "sha256": r.sha256,
                        })
                    })
                    .collect();
                json!({
                    "name": s.name,
                    "url": s.url,
                    "timestamp": s.timestamp,
                    "width": s.width,
                    "height": s.height,
                    "resources": resources,
                })
            })
            .collect();
        json!({
            "_comment": self.comment,
            "archive": self.archive,
            "sites": sites,
        })
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        write_json(&manifest_path(root), &self.to_json())
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("cannot serialize {}: {e}", path.display()))?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Whether a site's snapshot can be rendered.
pub enum Snapshot {
    /// Every resource is present and verified.
    Ready(LocalFetcher),
    /// Something is missing; the reason says what to run.
    Missing(String),
}

/// Loads a site's snapshot from `dir` (`target/snapshots`), verifying
/// every file against its pinned hash.
///
/// # Errors
///
/// A file whose hash doesn't match (a corrupt or tampered snapshot).
pub fn load_snapshot(dir: &Path, site: &Site) -> Result<Snapshot> {
    if site.resources.is_empty() {
        return Ok(Snapshot::Missing(
            "not pinned yet (run `cargo xtask snapshots --discover`)".to_string(),
        ));
    }
    let mut fetcher = LocalFetcher::new();
    for r in &site.resources {
        let path = dir.join(&site.name).join(&r.sha256);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                return Ok(Snapshot::Missing(
                    "snapshot not downloaded (run `cargo xtask snapshots`)".to_string(),
                ));
            }
        };
        if hex_digest(&bytes) != r.sha256 {
            return Err(format!(
                "{}: {} does not match its pinned sha256; delete it and run `cargo xtask snapshots`",
                site.name,
                path.display()
            ));
        }
        fetcher.insert(&r.url, &r.content_type, bytes);
    }
    Ok(Snapshot::Ready(fetcher))
}

/// Loads `url` with `engine` and renders the first `width` × `height`
/// CSS px at scale 1, after letting late images load.
pub fn render(engine: &Engine, url: &str, width: u32, height: u32) -> Result<Image> {
    let url = Url::parse(url).map_err(|e| format!("invalid URL {url}: {e:?}"))?;
    let mut page = engine
        .load(&url)
        .map_err(|e| format!("cannot load {url:?}: {e}"))?;
    let options = RenderOptions::new(width as f32, height as f32);
    let mut raster = page
        .render(&options)
        .map_err(|e| format!("cannot render {url:?}: {e}"))?;
    for _ in 0..MAX_IMAGE_ROUNDS {
        if !page.has_pending_images() || !engine.load_pending_images(&mut page, &NoObserver) {
            break;
        }
        raster = page
            .render(&options)
            .map_err(|e| format!("cannot render {url:?}: {e}"))?;
    }
    Ok(Image {
        width: raster.width(),
        height: raster.height(),
        rgba: unpremultiply(raster.pixels()),
    })
}

/// The result of checking one site.
#[derive(Debug)]
pub enum Outcome {
    /// Matches its reference.
    Pass(imgdiff::Diff),
    /// Differs from its reference.
    Fail(imgdiff::Diff),
    /// Rendered, but no reference is committed yet.
    NoReference,
    /// Not checked: the snapshot is missing.
    Skipped(String),
}

impl Outcome {
    /// The value recorded in `status.json`, if the site was checked.
    fn status(&self) -> Option<&'static str> {
        match self {
            Outcome::Pass(_) => Some("pass"),
            Outcome::Fail(_) => Some("fail"),
            Outcome::NoReference => Some("no-reference"),
            Outcome::Skipped(_) => None,
        }
    }
}

/// Renders `site` from its snapshot and compares it with its reference.
/// When `actual_out` is given, the rendering (and, on failure, a diff
/// image) is written there for review.
pub fn check_site(
    root: &Path,
    fonts: &Arc<FontContext>,
    site: &Site,
    actual_out: Option<&Path>,
) -> Result<Outcome> {
    let fetcher = match load_snapshot(&snapshots_dir(root), site)? {
        Snapshot::Ready(f) => f,
        Snapshot::Missing(reason) => return Ok(Outcome::Skipped(reason)),
    };
    let engine = Engine::new(Arc::new(fetcher), Arc::clone(fonts));
    let actual = render(&engine, &site.url, site.width, site.height)?;
    let write = |suffix: &str, image: &Image| -> Result<()> {
        if let Some(dir) = actual_out {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let path = dir.join(format!("{}{suffix}.png", site.name));
            std::fs::write(&path, imgdiff::encode_png(image)?)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        Ok(())
    };
    write("", &actual)?;
    let reference = reference_path(root, &site.name);
    let expected = match std::fs::read(&reference) {
        Ok(bytes) => {
            imgdiff::decode_png(&bytes).map_err(|e| format!("{}: {e}", reference.display()))?
        }
        Err(_) => return Ok(Outcome::NoReference),
    };
    let t = Thresholds::default();
    let diff = imgdiff::compare(&actual, &expected, &t);
    if diff.passes(&t) {
        Ok(Outcome::Pass(diff))
    } else {
        write("-diff", &imgdiff::diff_image(&actual, &expected, &t))?;
        Ok(Outcome::Fail(diff))
    }
}

/// `cargo xtask sites [--update]`.
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let update = match args {
        [] => false,
        [flag] if flag == "--update" => true,
        _ => return Err(format!("unknown sites options: {}", args.join(" "))),
    };
    let manifest = Manifest::load(root)?;
    let fonts = Engine::system_fonts();
    let out = output_dir(root);
    let mut status = read_status(root)?;
    let mut failures = 0;
    for site in &manifest.sites {
        if update {
            match load_snapshot(&snapshots_dir(root), site)? {
                Snapshot::Missing(reason) => println!("{}: skipped, {reason}", site.name),
                Snapshot::Ready(fetcher) => {
                    let engine = Engine::new(Arc::new(fetcher), Arc::clone(&fonts));
                    let image = render(&engine, &site.url, site.width, site.height)?;
                    let path = reference_path(root, &site.name);
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                    }
                    std::fs::write(&path, imgdiff::encode_png(&image)?)
                        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    println!(
                        "{}: wrote {} (review it before committing)",
                        site.name,
                        path.display()
                    );
                    status.insert(site.name.clone(), "pass".to_string());
                }
            }
            continue;
        }
        let outcome = check_site(root, &fonts, site, Some(&out))?;
        match &outcome {
            Outcome::Pass(d) => println!("{}: pass ({})", site.name, d.summary()),
            Outcome::Fail(d) => {
                failures += 1;
                println!(
                    "{}: FAIL ({}); see {}",
                    site.name,
                    d.summary(),
                    out.join(format!("{}-diff.png", site.name)).display()
                );
            }
            Outcome::NoReference => println!(
                "{}: no reference yet; review {} and run `cargo xtask sites --update`",
                site.name,
                out.join(format!("{}.png", site.name)).display()
            ),
            Outcome::Skipped(reason) => println!("{}: skipped, {reason}", site.name),
        }
        if let Some(s) = outcome.status() {
            status.insert(site.name.clone(), s.to_string());
        }
    }
    // Forget sites that are no longer in the manifest.
    status.retain(|name, _| manifest.sites.iter().any(|s| &s.name == name));
    write_status(root, &status)?;
    update_scorecard_row(root, &manifest, &status)?;
    if failures > 0 {
        Err(format!(
            "{failures} target site(s) differ from their references"
        ))
    } else {
        Ok(())
    }
}

fn read_status(root: &Path) -> Result<BTreeMap<String, String>> {
    let path = status_path(root);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let json = read_json(&path)?;
    Ok(json
        .get("sites")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default())
}

fn write_status(root: &Path, status: &BTreeMap<String, String>) -> Result<()> {
    let sites: Map<String, Value> = status
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    write_json(
        &status_path(root),
        &json!({
            "_comment": "Written by `cargo xtask sites`: the last result per target site (pass, fail or no-reference). Feeds the \"Target sites\" row of docs/scorecard.md.",
            "sites": sites,
        }),
    )
}

/// The start of the scorecard row this module owns.
const ROW_PREFIX: &str = "| Target sites";

fn render_row(manifest: &Manifest, status: &BTreeMap<String, String>) -> String {
    let total = manifest.sites.len() as u64;
    let passed = manifest
        .sites
        .iter()
        .filter(|s| status.get(&s.name).map(String::as_str) == Some("pass"))
        .count() as u64;
    let rate = if total == 0 {
        0.0
    } else {
        passed as f64 * 100.0 / total as f64
    };
    format!(
        "{ROW_PREFIX} (reference rendering, `cargo xtask sites`) | {passed} | {total} | {rate:.1}% |"
    )
}

/// The "Target sites" scorecard row, from the committed manifest and
/// status (used by `cargo xtask conformance`).
pub fn scorecard_row(root: &Path) -> Result<String> {
    Ok(render_row(&Manifest::load(root)?, &read_status(root)?))
}

/// A note under the scorecard table listing each site's state.
pub fn scorecard_note(root: &Path) -> Result<String> {
    Ok(scorecard_note_from(
        &Manifest::load(root)?,
        &read_status(root)?,
    ))
}

/// Rewrites the "Target sites" row and note of `docs/scorecard.md` in
/// place, leaving the conformance rows alone.
fn update_scorecard_row(
    root: &Path,
    manifest: &Manifest,
    status: &BTreeMap<String, String>,
) -> Result<()> {
    let path = root.join("docs").join("scorecard.md");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let row = render_row(manifest, status);
    if !text.lines().any(|l| l.starts_with(ROW_PREFIX)) {
        return Err(format!(
            "{} has no target sites row; run `cargo xtask conformance` first",
            path.display()
        ));
    }
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with(ROW_PREFIX) {
            out.push_str(&row);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    // The per-site note is the part after the marker line.
    let note = scorecard_note_from(manifest, status);
    let out = match out.find(NOTE_MARKER) {
        Some(i) => format!("{}{note}", out.get(..i).unwrap_or_default()),
        None => out,
    };
    std::fs::write(&path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The first words of [`scorecard_note`], used to find it again.
const NOTE_MARKER: &str = "\nTarget sites are rendered from";

fn scorecard_note_from(manifest: &Manifest, status: &BTreeMap<String, String>) -> String {
    let mut out = String::from(NOTE_MARKER);
    out.push_str(
        " pinned offline snapshots (`conformance/sites/manifest.json`, \
         downloaded by `cargo xtask snapshots`) and compared with reviewed reference images:",
    );
    for site in &manifest.sites {
        let state = if site.resources.is_empty() {
            "not pinned yet"
        } else {
            match status.get(&site.name).map(String::as_str) {
                Some("pass") => "pass",
                Some("fail") => "fail",
                Some("no-reference") => "no reference yet",
                _ => "not checked yet",
            }
        };
        let _ = write!(out, "\n- `{}` ({}): {state}", site.name, site.url);
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_root() -> PathBuf {
        crate::workspace_root()
    }

    /// The reference test proper: every target site with a snapshot and a
    /// reference must match it. Sites without either are skipped with a
    /// message, so this passes offline.
    #[test]
    fn target_sites_match_references() {
        let root = workspace_root();
        let manifest = Manifest::load(&root).unwrap();
        let fonts = Engine::system_fonts();
        let mut failures = Vec::new();
        for site in &manifest.sites {
            match check_site(&root, &fonts, site, Some(&output_dir(&root))).unwrap() {
                Outcome::Pass(_) => {}
                Outcome::Fail(d) => failures.push(format!("{}: {}", site.name, d.summary())),
                Outcome::NoReference => eprintln!(
                    "skipping {}: no reference image yet (run `cargo xtask sites --update` and review it)",
                    site.name
                ),
                Outcome::Skipped(reason) => eprintln!("skipping {}: {reason}", site.name),
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn committed_manifest_is_valid_and_canonical() {
        let root = workspace_root();
        let json = read_json(&manifest_path(&root)).unwrap();
        let manifest = Manifest::from_json(&json).unwrap();
        assert_eq!(
            manifest.to_json(),
            json,
            "manifest is not in canonical form"
        );
        let names: Vec<_> = manifest.sites.iter().map(|s| &s.name).collect();
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(names, unique, "duplicate site names");
        assert!(manifest.archive.contains("{timestamp}") && manifest.archive.contains("{url}"));
    }

    #[test]
    fn scorecard_row_is_current() {
        let root = workspace_root();
        let scorecard = std::fs::read_to_string(root.join("docs/scorecard.md")).unwrap();
        let row = scorecard_row(&root).unwrap();
        assert!(
            scorecard.lines().any(|l| l == row),
            "docs/scorecard.md lacks {row:?}"
        );
        assert!(scorecard.contains(&scorecard_note(&root).unwrap()));
    }

    #[test]
    fn manifest_rejects_bad_input() {
        let bad_name = json!({"archive": "a", "sites": [
            {"name": "../x", "url": "https://a.test/", "timestamp": "1", "width": 10, "height": 10}
        ]});
        assert!(Manifest::from_json(&bad_name).is_err());
        let huge = json!({"archive": "a", "sites": [
            {"name": "x", "url": "https://a.test/", "timestamp": "1", "width": 100000, "height": 10}
        ]});
        assert!(Manifest::from_json(&huge).is_err());
        let bad_hash = json!({"archive": "a", "sites": [
            {"name": "x", "url": "https://a.test/", "timestamp": "1", "width": 10, "height": 10,
             "resources": [{"url": "u", "source": "s", "content_type": "t", "sha256": "../etc"}]}
        ]});
        assert!(Manifest::from_json(&bad_hash).is_err());
    }

    /// Writes `files` (url, content type, body) as a snapshot of `name`
    /// under `dir` and returns the site.
    fn make_snapshot(dir: &Path, name: &str, files: &[(&str, &str, &[u8])]) -> Site {
        std::fs::create_dir_all(dir.join(name)).unwrap();
        let resources = files
            .iter()
            .map(|(url, ct, body)| {
                let sha256 = hex_digest(body);
                std::fs::write(dir.join(name).join(&sha256), body).unwrap();
                Resource {
                    url: (*url).to_string(),
                    source: format!("https://archive.test/{url}"),
                    content_type: (*ct).to_string(),
                    sha256,
                }
            })
            .collect();
        Site {
            name: name.to_string(),
            url: files[0].0.to_string(),
            timestamp: "20261101000000".to_string(),
            width: 64,
            height: 48,
            resources,
        }
    }

    fn render_snapshot(dir: &Path, site: &Site) -> Image {
        let Snapshot::Ready(fetcher) = load_snapshot(dir, site).unwrap() else {
            panic!("snapshot should be ready");
        };
        let engine = Engine::new(Arc::new(fetcher), Arc::new(FontContext::new()));
        render(&engine, &site.url, site.width, site.height).unwrap()
    }

    #[test]
    fn snapshot_pipeline_renders_offline_and_compares() {
        let dir = tempfile::tempdir().unwrap();
        let page = b"<!doctype html><link rel=stylesheet href=s.css><div id=a></div>";
        let css = b"body{margin:0} #a{width:32px;height:24px;background:#0a0}";
        let site = make_snapshot(
            dir.path(),
            "synthetic",
            &[
                ("https://site.test/", "text/html", page),
                ("https://site.test/s.css", "text/css", css),
            ],
        );
        let a = render_snapshot(dir.path(), &site);
        assert_eq!((a.width, a.height), (64, 48));
        // The stylesheet came from the snapshot: the box is green.
        assert_eq!(&a.rgba[0..4], &[0, 0xaa, 0, 255]);
        assert_eq!(&a.rgba[(63 * 4)..(64 * 4)], &[255, 255, 255, 255]);
        let t = Thresholds::default();
        assert!(imgdiff::compare(&a, &a, &t).passes(&t));

        // A different stylesheet renders differently and fails.
        let css2 = b"body{margin:0} #a{width:32px;height:24px;background:#a00}";
        let site2 = make_snapshot(
            dir.path(),
            "synthetic2",
            &[
                ("https://site.test/", "text/html", page),
                ("https://site.test/s.css", "text/css", css2),
            ],
        );
        let b = render_snapshot(dir.path(), &site2);
        assert!(!imgdiff::compare(&b, &a, &t).passes(&t));

        // Round trip through a reference PNG.
        let decoded = imgdiff::decode_png(&imgdiff::encode_png(&a).unwrap()).unwrap();
        assert!(imgdiff::compare(&a, &decoded, &t).passes(&t));
    }

    #[test]
    fn snapshot_states() {
        let dir = tempfile::tempdir().unwrap();
        let mut site = make_snapshot(
            dir.path(),
            "s",
            &[("https://site.test/", "text/html", b"<p>x")],
        );
        assert!(matches!(
            load_snapshot(dir.path(), &site).unwrap(),
            Snapshot::Ready(_)
        ));
        // Corrupted file: an error, not a silent pass.
        std::fs::write(dir.path().join("s").join(&site.resources[0].sha256), "y").unwrap();
        assert!(load_snapshot(dir.path(), &site).is_err());
        // Missing file: skipped.
        std::fs::remove_file(dir.path().join("s").join(&site.resources[0].sha256)).unwrap();
        assert!(matches!(
            load_snapshot(dir.path(), &site).unwrap(),
            Snapshot::Missing(_)
        ));
        // Not pinned: skipped.
        site.resources.clear();
        assert!(matches!(
            load_snapshot(dir.path(), &site).unwrap(),
            Snapshot::Missing(_)
        ));
    }

    #[test]
    fn snapshot_never_reaches_the_network() {
        let dir = tempfile::tempdir().unwrap();
        // The image is not in the snapshot: it must 404 locally, and the
        // page still renders.
        let site = make_snapshot(
            dir.path(),
            "net",
            &[(
                "https://site.test/",
                "text/html",
                b"<body style=margin:0><img src=https://elsewhere.test/a.png width=8 height=8>",
            )],
        );
        let image = render_snapshot(dir.path(), &site);
        assert_eq!((image.width, image.height), (64, 48));
    }
}
