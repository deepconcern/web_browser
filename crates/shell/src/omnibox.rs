//! The URL bar: deciding whether input is a URL or a search, and the
//! search engines offered.
//!
//! The engine list is short and has no commercial deals behind it (issue
//! #3). None is preselected: until the user picks one, typing a search
//! asks them to choose.

use wb_ipc::Url;

/// A search engine the user can choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchEngine {
    /// The stable identifier stored in the profile.
    pub id: &'static str,
    /// The display name.
    pub name: &'static str,
    /// The query URL; `{q}` is replaced by the percent-encoded terms.
    template: &'static str,
}

/// The engines offered, in display order.
pub const SEARCH_ENGINES: [SearchEngine; 4] = [
    SearchEngine {
        id: "duckduckgo",
        name: "DuckDuckGo",
        template: "https://duckduckgo.com/html/?q={q}",
    },
    SearchEngine {
        id: "startpage",
        name: "Startpage",
        template: "https://www.startpage.com/do/search?q={q}",
    },
    SearchEngine {
        id: "mojeek",
        name: "Mojeek",
        template: "https://www.mojeek.com/search?q={q}",
    },
    SearchEngine {
        id: "wikipedia",
        name: "Wikipedia",
        template: "https://en.wikipedia.org/w/index.php?search={q}",
    },
];

impl SearchEngine {
    /// The engine with identifier `id`.
    #[must_use]
    pub fn by_id(id: &str) -> Option<SearchEngine> {
        SEARCH_ENGINES.iter().copied().find(|e| e.id == id)
    }

    /// The URL searching for `terms`.
    #[must_use]
    pub fn search_url(&self, terms: &str) -> Option<Url> {
        let q = crate::forms::urlencode(terms.trim());
        Url::parse(&self.template.replace("{q}", &q)).ok()
    }
}

/// What the user typed into the URL bar means.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// Navigate to this URL.
    Url(Url),
    /// Search for these terms with the chosen engine.
    Search(Url),
    /// A search, but no engine is chosen yet: ask the user to pick one.
    NeedsSearchEngine(String),
    /// Nothing to do (empty input).
    Empty,
}

/// Schemes the user can type directly.
const TYPED_SCHEMES: [&str; 5] = ["http", "https", "about", "data", "file"];

/// Classifies URL bar input.
///
/// 1. Input with a scheme the browser loads (`https:`, `http:`, `about:`,
///    `data:`, `file:`) is a URL.
/// 2. Input with no spaces whose host part has a dot, is `localhost`, or
///    is an IP address (optionally with port, path, query) is a URL with
///    `https://` prepended (the network layer is HTTPS-only anyway).
/// 3. Anything else (spaces, a single word, `?` prefix) is a search.
#[must_use]
pub fn classify(input: &str, engine: Option<SearchEngine>) -> Input {
    let text = input.trim();
    if text.is_empty() {
        return Input::Empty;
    }
    let search = |terms: &str| match engine.and_then(|e| e.search_url(terms)) {
        Some(u) => Input::Search(u),
        None => Input::NeedsSearchEngine(terms.to_string()),
    };
    if let Some(terms) = text.strip_prefix('?') {
        return search(terms);
    }
    if let Some((scheme, _)) = text.split_once(':')
        && TYPED_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str())
        && !text.contains(' ')
        && let Ok(u) = Url::parse(text)
    {
        return Input::Url(u);
    }
    if !text.contains(char::is_whitespace) && looks_like_host(text) {
        if let Ok(u) = Url::parse(&format!("https://{text}")) {
            return Input::Url(u);
        }
    }
    search(text)
}

/// Whether `text` starts with something that reads as a host name.
fn looks_like_host(text: &str) -> bool {
    let end = text.find(['/', '?', '#']).unwrap_or(text.len());
    let authority = text.get(..end).unwrap_or(text);
    if authority.contains('@') {
        return false;
    }
    if authority.starts_with('[') {
        return authority.contains(']');
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        Some(_) => return false,
        None => authority,
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host.starts_with('.') || host.ends_with("..") || !host.contains('.') {
        return false;
    }
    let labels_ok = host
        .trim_end_matches('.')
        .split('.')
        .all(|l| !l.is_empty() && l.chars().all(|c| c.is_alphanumeric() || c == '-'));
    // The last label must not be all digits unless the whole host is an
    // IPv4 address ("3.14" is a search; "10.0.0.1" a host).
    let last = host.trim_end_matches('.').rsplit('.').next().unwrap_or("");
    let ipv4 = host.split('.').count() == 4
        && host
            .split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    labels_ok && (ipv4 || !last.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ddg() -> Option<SearchEngine> {
        SearchEngine::by_id("duckduckgo")
    }

    fn url_of(i: Input) -> String {
        match i {
            Input::Url(u) | Input::Search(u) => u.as_str().to_string(),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn urls() {
        for (input, want) in [
            ("example.com", "https://example.com/"),
            ("  example.com/a?b#c ", "https://example.com/a?b#c"),
            ("https://example.com", "https://example.com/"),
            ("HTTP://Example.com/x", "http://example.com/x"),
            ("localhost:8080/x", "https://localhost:8080/x"),
            ("10.0.0.1", "https://10.0.0.1/"),
            ("[::1]:80/", "https://[::1]:80/"),
            ("about:blank", "about:blank"),
            ("data:text/plain,hi", "data:text/plain,hi"),
            ("sub.domain.co.uk.", "https://sub.domain.co.uk./"),
            ("bücher.de", "https://xn--bcher-kva.de/"),
        ] {
            let got = classify(input, ddg());
            assert!(matches!(got, Input::Url(_)), "{input}: {got:?}");
            assert_eq!(url_of(got), want, "{input}");
        }
    }

    #[test]
    fn searches() {
        for (input, q) in [
            ("rust language", "rust+language"),
            ("hello", "hello"),
            ("3.14", "3.14"),
            ("what is 1+1?", "what+is+1%2B1%3F"),
            ("?example.com", "example.com"),
            ("javascript:alert(1)", "javascript%3Aalert%281%29"),
            ("user@example.com", "user%40example.com"),
            ("C:\\path", "C%3A%5Cpath"),
        ] {
            let got = classify(input, ddg());
            assert!(matches!(got, Input::Search(_)), "{input}: {got:?}");
            assert_eq!(url_of(got), format!("https://duckduckgo.com/html/?q={q}"));
        }
    }

    #[test]
    fn no_engine_chosen() {
        assert_eq!(
            classify("two words", None),
            Input::NeedsSearchEngine("two words".into())
        );
        assert!(matches!(classify("example.org", None), Input::Url(_)));
        assert_eq!(classify("   ", None), Input::Empty);
    }

    #[test]
    fn engines() {
        assert_eq!(SEARCH_ENGINES.len(), 4);
        for e in SEARCH_ENGINES {
            let u = e.search_url("a b&c").unwrap();
            assert_eq!(u.scheme(), "https");
            assert!(u.as_str().contains("a+b%26c"), "{u:?}");
            assert_eq!(SearchEngine::by_id(e.id), Some(e));
        }
        assert_eq!(SearchEngine::by_id("google"), None);
    }
}
