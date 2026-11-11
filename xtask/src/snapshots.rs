//! `cargo xtask snapshots`: downloads the pinned target-site snapshots.
//!
//! - `cargo xtask snapshots` downloads every resource listed in
//!   `conformance/sites/manifest.json` from its pinned archived source into
//!   `target/snapshots/<site>/<sha256>`, and checks each hash. Files that
//!   are already present and correct are kept. The pages are never
//!   committed: they are third-party content.
//! - `cargo xtask snapshots --discover [site...]` (re)pins sites: it loads
//!   each page with our own engine through the web archive at the site's
//!   pinned timestamp, records every resource the engine asks for
//!   (document, stylesheets and their imports, images), and rewrites the
//!   site's resource list in the manifest. Run it only to add a site or
//!   move a pin; the reference image must then be reviewed again.
//!
//! Pages come from the web archive in "original bytes" mode (the archive
//! URL template in the manifest), so a pin keeps meaning the same bytes
//! even after the live site changes. Downloads use `curl` (which honours
//! the usual proxy settings), the same way `fetch-corpora` uses `git`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use wb_net::{Headers, HttpVersion, NetError, Request, Response};
use wb_shell::engine::{Engine, Fetcher, LocalFetcher};

use crate::Result;
use crate::sha256::hex_digest;
use crate::sites::{Manifest, Resource, Site, snapshots_dir};

/// Most resources pinned per site.
const MAX_RESOURCES: usize = 1000;
/// Largest resource downloaded, in bytes.
const MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Per-download time limit, in seconds.
const TIMEOUT_SECS: &str = "120";
/// A generic client name: the tooling sends nothing that identifies the user.
const USER_AGENT: &str = "web_browser-snapshots/0.1";

pub fn run(root: &Path, args: &[String]) -> Result<()> {
    match args.split_first() {
        None => download(root),
        Some((flag, names)) if flag == "--discover" => discover(root, names),
        Some(_) => Err(format!("unknown snapshots options: {}", args.join(" "))),
    }
}

/// Downloads every pinned resource that is missing or wrong.
fn download(root: &Path) -> Result<()> {
    let manifest = Manifest::load(root)?;
    let tmp = tmp_dir(root)?;
    let mut failures = 0;
    for site in &manifest.sites {
        if site.resources.is_empty() {
            println!(
                "{}: not pinned yet; run `cargo xtask snapshots --discover {}`",
                site.name, site.name
            );
            continue;
        }
        let dir = site_dir(root, site)?;
        let mut fetched = 0;
        for r in &site.resources {
            let path = dir.join(&r.sha256);
            if std::fs::read(&path).is_ok_and(|b| hex_digest(&b) == r.sha256) {
                continue;
            }
            match curl(&r.source, &tmp) {
                Ok(d) if d.status == 200 && hex_digest(&d.body) == r.sha256 => {
                    std::fs::write(&path, &d.body)
                        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    fetched += 1;
                }
                Ok(d) => {
                    failures += 1;
                    eprintln!(
                        "{}: {} returned status {} with sha256 {} (pinned {})",
                        site.name,
                        r.source,
                        d.status,
                        hex_digest(&d.body),
                        r.sha256
                    );
                }
                Err(e) => {
                    failures += 1;
                    eprintln!("{}: {}: {e}", site.name, r.source);
                }
            }
        }
        println!(
            "{}: {} resources ({} downloaded)",
            site.name,
            site.resources.len(),
            fetched
        );
    }
    if failures > 0 {
        Err(format!("{failures} resource(s) could not be downloaded"))
    } else {
        Ok(())
    }
}

/// Re-pins the named sites (all sites when `names` is empty).
fn discover(root: &Path, names: &[String]) -> Result<()> {
    let mut manifest = Manifest::load(root)?;
    if let Some(unknown) = names
        .iter()
        .find(|n| !manifest.sites.iter().any(|s| &s.name == *n))
    {
        return Err(format!("no site named {unknown:?} in the manifest"));
    }
    let tmp = tmp_dir(root)?;
    let fonts = Engine::system_fonts();
    let archive = manifest.archive.clone();
    for site in &mut manifest.sites {
        if !names.is_empty() && !names.contains(&site.name) {
            continue;
        }
        println!(
            "{}: discovering {} at {}",
            site.name, site.url, site.timestamp
        );
        let tmp = tmp.clone();
        let download: DownloadFn = Box::new(move |url| curl(url, &tmp));
        site.resources = record_site(site, &archive, &site_dir(root, site)?, download, &fonts)?;
        println!("{}: pinned {} resources", site.name, site.resources.len());
        // Save after every site so a later failure keeps earlier pins.
        manifest_save(root, site)?;
    }
    Ok(())
}

/// Downloads a URL (curl, or a fake in tests).
type DownloadFn = Box<dyn Fn(&str) -> Result<Download> + Send + Sync>;

/// Renders `site` through the archive and returns every resource the
/// engine fetched, document first, with the bodies saved in `dir`.
fn record_site(
    site: &Site,
    archive: &str,
    dir: &Path,
    download: DownloadFn,
    fonts: &Arc<wb_text::FontContext>,
) -> Result<Vec<Resource>> {
    let recorder = Arc::new(Recorder {
        archive: archive.to_string(),
        timestamp: site.timestamp.clone(),
        download,
        dir: dir.to_path_buf(),
        records: Mutex::new(Vec::new()),
        seen: Mutex::new(HashSet::new()),
        local: LocalFetcher::new(),
    });
    let engine = Engine::new(Arc::clone(&recorder) as Arc<dyn Fetcher>, Arc::clone(fonts));
    crate::sites::render(&engine, &site.url, site.width, site.height)?;
    let records = std::mem::take(
        &mut *recorder
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    if records.first().map(|r| r.url.as_str()) != Some(site.url.as_str()) {
        return Err(format!(
            "{}: the document itself could not be fetched from the archive",
            site.name
        ));
    }
    Ok(records)
}

/// Saves `site` into the manifest on disk, leaving the other sites alone.
fn manifest_save(root: &Path, site: &Site) -> Result<()> {
    let mut on_disk = Manifest::load(root)?;
    for s in &mut on_disk.sites {
        if s.name == site.name {
            s.resources.clone_from(&site.resources);
        }
    }
    on_disk.save(root)
}

fn site_dir(root: &Path, site: &Site) -> Result<PathBuf> {
    let dir = snapshots_dir(root).join(&site.name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn tmp_dir(root: &Path) -> Result<PathBuf> {
    let dir = snapshots_dir(root).join(".download");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Fills the archive URL template for `url` at `timestamp`.
fn archive_url(template: &str, timestamp: &str, url: &str) -> String {
    template
        .replace("{timestamp}", timestamp)
        .replace("{url}", url)
}

/// A finished download.
struct Download {
    status: u16,
    /// The URL after redirects (the archive's exact capture).
    final_url: String,
    content_type: String,
    body: Vec<u8>,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Downloads `url` with curl, following redirects.
fn curl(url: &str, tmp: &Path) -> Result<Download> {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file = tmp.join(format!("{}-{n}", std::process::id()));
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--location", "--compressed"])
        .args(["--max-redirs", "10", "--max-time", TIMEOUT_SECS])
        .args(["--max-filesize", &MAX_BYTES.to_string()])
        .args(["--proto", "=https", "--proto-redir", "=https"])
        .args(["--user-agent", USER_AGENT])
        .args([
            "--write-out",
            "%{http_code}\n%{url_effective}\n%{content_type}",
        ])
        .arg("--output")
        .arg(&file)
        .arg("--")
        .arg(url)
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    let body = std::fs::read(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    if !output.status.success() {
        return Err(format!(
            "curl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let meta = String::from_utf8_lossy(&output.stdout);
    let mut lines = meta.lines();
    let status = lines
        .next()
        .and_then(|s| s.trim().parse().ok())
        .ok_or("curl reported no status")?;
    let final_url = lines.next().unwrap_or(url).trim().to_string();
    let content_type = lines.next().unwrap_or_default().trim().to_string();
    Ok(Download {
        status,
        final_url,
        content_type,
        body,
    })
}

/// A fetcher that downloads through the archive and records every
/// successful response as a pinned resource.
struct Recorder {
    archive: String,
    timestamp: String,
    download: DownloadFn,
    /// The site's snapshot directory; bodies are saved there as they come.
    dir: PathBuf,
    records: Mutex<Vec<Resource>>,
    seen: Mutex<HashSet<String>>,
    /// Handles `data:` and `about:blank`.
    local: LocalFetcher,
}

impl Recorder {
    fn record(&self, url: &str, d: &Download) {
        let sha256 = hex_digest(&d.body);
        let path = self.dir.join(&sha256);
        if let Err(e) = std::fs::write(&path, &d.body) {
            eprintln!("cannot write {}: {e}", path.display());
            return;
        }
        let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
        records.push(Resource {
            url: url.to_string(),
            source: d.final_url.clone(),
            content_type: d.content_type.clone(),
            sha256,
        });
    }
}

fn response(url: wb_url::Url, status: u16, content_type: &str, body: Vec<u8>) -> Response {
    let mut headers = Headers::new();
    if !content_type.is_empty() {
        headers.set("content-type", content_type);
    }
    Response {
        url_list: vec![url.clone()],
        url,
        status,
        headers,
        body,
        version: HttpVersion::None,
        from_cache: false,
    }
}

impl Fetcher for Recorder {
    fn fetch(&self, request: Request) -> std::result::Result<Response, NetError> {
        let url = request.url.without_fragment();
        if !matches!(url.scheme(), "http" | "https") {
            return self.local.fetch(request);
        }
        let key = url.as_str().to_string();
        let first_time = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone());
        let full = self
            .records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
            >= MAX_RESOURCES;
        if !first_time || full {
            // Already fetched (the engine caches by URL), or over the cap.
            return Ok(response(url, 404, "", Vec::new()));
        }
        let source = archive_url(&self.archive, &self.timestamp, &key);
        match (self.download)(&source) {
            Ok(d) => {
                println!("  {} {key}", d.status);
                if d.status == 200 {
                    self.record(&key, &d);
                }
                Ok(response(url, d.status, &d.content_type, d.body))
            }
            Err(e) => {
                eprintln!("  error {key}: {e}");
                Err(NetError::Connect(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sites::{Snapshot, load_snapshot, render};

    /// Discovery through a fake archive records the document, its
    /// stylesheet and the stylesheet's image, and the result renders
    /// offline exactly like the live load did.
    #[test]
    fn discovery_records_what_the_engine_fetches() {
        let dir = tempfile::tempdir().unwrap();
        let fake: DownloadFn = Box::new(|source: &str| {
            let url = source
                .strip_prefix("https://archive.test/20261101000000/")
                .unwrap_or_default();
            let (status, ct, body): (u16, &str, Vec<u8>) = match url {
                "https://site.test/" => (
                    200,
                    "text/html; charset=utf-8",
                    b"<link rel=stylesheet href=/a.css><p class=x>hi<img src=missing.png>".to_vec(),
                ),
                "https://site.test/a.css" => {
                    (200, "text/css", b"body{margin:0;background:#00f}".to_vec())
                }
                _ => (404, "text/html", b"gone".to_vec()),
            };
            Ok(Download {
                status,
                final_url: format!("https://archive.test/20261101123456/{url}"),
                content_type: ct.to_string(),
                body,
            })
        });
        let mut site = Site {
            name: "fake".to_string(),
            url: "https://site.test/".to_string(),
            timestamp: "20261101000000".to_string(),
            width: 32,
            height: 16,
            resources: Vec::new(),
        };
        let fonts = Arc::new(wb_text::FontContext::new());
        let site_dir = dir.path().join("fake");
        std::fs::create_dir_all(&site_dir).unwrap();
        let records = record_site(
            &site,
            "https://archive.test/{timestamp}/{url}",
            &site_dir,
            fake,
            &fonts,
        )
        .unwrap();
        let urls: Vec<_> = records.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(urls, ["https://site.test/", "https://site.test/a.css"]);
        assert_eq!(
            records[1].source,
            "https://archive.test/20261101123456/https://site.test/a.css"
        );
        assert_eq!(records[0].content_type, "text/html; charset=utf-8");

        site.resources = records;
        let Snapshot::Ready(fetcher) = load_snapshot(dir.path(), &site).unwrap() else {
            panic!("snapshot should be complete");
        };
        let engine = Engine::new(Arc::new(fetcher), fonts);
        let image = render(&engine, &site.url, site.width, site.height).unwrap();
        assert_eq!(&image.rgba[0..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn archive_url_template() {
        assert_eq!(
            archive_url(
                "https://web.archive.org/web/{timestamp}id_/{url}",
                "20261101000000",
                "https://news.ycombinator.com/item?id=1"
            ),
            "https://web.archive.org/web/20261101000000id_/https://news.ycombinator.com/item?id=1"
        );
    }
}
