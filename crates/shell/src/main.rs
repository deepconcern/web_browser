//! The browser application entry point: `web_browser [URL or file]`.

use wb_shell::omnibox::{self, Input};
use wb_url::Url;

/// The URL to open from the first command-line argument: an existing
/// local file becomes a `file:` URL; anything else is read like URL bar
/// input (searches are ignored: no engine is chosen at startup).
fn start_url(arg: Option<String>) -> Option<Url> {
    let arg = arg?;
    if let Ok(path) = std::path::Path::new(&arg).canonicalize()
        && path.is_file()
    {
        let s = path
            .to_string_lossy()
            .replace('%', "%25")
            .replace(' ', "%20")
            .replace('#', "%23")
            .replace('?', "%3F");
        return Url::parse(&format!("file://{s}")).ok();
    }
    match omnibox::classify(&arg, None) {
        Input::Url(u) => Some(u),
        _ => None,
    }
}

fn main() {
    let url = start_url(std::env::args().nth(1));
    if let Err(e) = wb_shell::app::run(url) {
        eprintln!("web_browser: {e}");
        std::process::exit(1);
    }
}
