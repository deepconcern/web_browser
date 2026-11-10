//! Filter list text: comments, headers, metadata and `!#if` directives.

use std::time::Duration;

/// Metadata from a list's leading comment block (`! Title: ...`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListMetadata {
    /// `! Title:`.
    pub title: Option<String>,
    /// `! Homepage:`.
    pub homepage: Option<String>,
    /// `! Version:`.
    pub version: Option<String>,
    /// `! Last modified:` (kept as text).
    pub last_modified: Option<String>,
    /// `! Expires:` (for example `4 days (update frequency)` or `12 hours`):
    /// how long the list stays fresh before it should be downloaded again.
    pub expires: Option<Duration>,
}

/// What happened to each line of a list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseStats {
    /// Network filters accepted (blocking and exceptions, `$badfilter` lines
    /// excluded).
    pub network: usize,
    /// Cosmetic filters accepted (hiding and exceptions).
    pub cosmetic: usize,
    /// `$badfilter` lines.
    pub badfilters: usize,
    /// Comment, header, metadata and directive lines.
    pub comments: usize,
    /// Lines skipped because a `!#if` condition was false.
    pub skipped_conditional: usize,
    /// Network filters dropped for an option or form this engine does not
    /// support (`$popup`, `$redirect=`, `$csp=`, entity domains, ...).
    pub unsupported_network: usize,
    /// Cosmetic filters dropped for syntax this engine does not support
    /// (procedural filters, scriptlets, HTML filters, snippets, entities).
    pub unsupported_cosmetic: usize,
    /// `/regex/` filters that failed to compile.
    pub invalid_regex: usize,
    /// Malformed lines.
    pub invalid: usize,
}

impl ParseStats {
    /// Lines that were filters in some syntax but were dropped.
    pub fn dropped(&self) -> usize {
        self.unsupported_network + self.unsupported_cosmetic + self.invalid_regex + self.invalid
    }
}

/// The result of adding one list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListReport {
    /// The list's metadata.
    pub metadata: ListMetadata,
    /// Per-line counters.
    pub stats: ParseStats,
}

/// Records a `! Key: value` metadata comment (first occurrence wins).
pub(crate) fn read_metadata(comment: &str, meta: &mut ListMetadata) {
    let Some((key, value)) = comment.split_once(':') else {
        return;
    };
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let slot = match key.trim().to_ascii_lowercase().as_str() {
        "title" => &mut meta.title,
        "homepage" => &mut meta.homepage,
        "version" => &mut meta.version,
        "last modified" | "last-modified" | "updated" => &mut meta.last_modified,
        "expires" => {
            if meta.expires.is_none() {
                meta.expires = parse_expires(value);
            }
            return;
        }
        _ => return,
    };
    if slot.is_none() {
        *slot = Some(value.to_owned());
    }
}

/// Parses `4 days`, `1 day (update frequency)`, `12 hours`, `6h`, `2d`.
fn parse_expires(value: &str) -> Option<Duration> {
    let digits_end = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let n: u64 = value.get(..digits_end)?.parse().ok()?;
    let unit: String = value
        .get(digits_end..)?
        .trim_start()
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect::<String>()
        .to_ascii_lowercase();
    let secs = match unit.as_str() {
        "d" | "day" | "days" => n.checked_mul(86_400)?,
        "h" | "hour" | "hours" => n.checked_mul(3_600)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

/// One `!#if` frame.
#[derive(Debug, Clone, Copy)]
struct Frame {
    cond: bool,
    in_else: bool,
}

/// Tracks uBO-style `!#if` / `!#else` / `!#endif` preprocessor directives.
///
/// Every condition token (`env_firefox`, `cap_html_filtering`, `ext_ubol`,
/// ...) is false for this browser: we are none of those environments and
/// claim none of those capabilities, so `!#if !env_safari` blocks are kept and
/// `!#if env_chromium` blocks are skipped. Malformed expressions are false.
#[derive(Debug, Default)]
pub(crate) struct Conditions {
    stack: Vec<Frame>,
}

/// Maximum `!#if` nesting and expression depth, so hostile input cannot
/// exhaust memory or the stack.
const MAX_DEPTH: usize = 64;

impl Conditions {
    /// Whether lines are currently being read.
    pub(crate) fn active(&self) -> bool {
        self.stack.iter().all(|f| f.cond != f.in_else)
    }

    /// Handles a line that starts with `!#`. Returns false if it is not a
    /// directive (then it is a plain comment).
    pub(crate) fn directive(&mut self, rest: &str) -> bool {
        let (word, arg) = match rest.split_once(char::is_whitespace) {
            Some((w, a)) => (w, a.trim()),
            None => (rest, ""),
        };
        match word {
            "if" => {
                let cond = self.stack.len() < MAX_DEPTH && eval(arg).unwrap_or(false);
                self.stack.push(Frame {
                    cond,
                    in_else: false,
                });
            }
            "else" => {
                if let Some(top) = self.stack.last_mut() {
                    top.in_else = !top.in_else;
                }
            }
            "endif" => {
                self.stack.pop();
            }
            // Includes are resolved by whoever downloads the list; here
            // they are ignored.
            "include" => {}
            _ => return false,
        }
        true
    }
}

/// Evaluates a directive expression; `None` if malformed.
fn eval(expr: &str) -> Option<bool> {
    let mut p = ExprParser {
        s: expr.as_bytes(),
        i: 0,
        depth: 0,
    };
    let v = p.or()?;
    p.skip_ws();
    (p.i == p.s.len()).then_some(v)
}

struct ExprParser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl ExprParser<'_> {
    fn skip_ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn eat(&mut self, tok: &[u8]) -> bool {
        self.skip_ws();
        if self.s.get(self.i..).is_some_and(|r| r.starts_with(tok)) {
            self.i += tok.len();
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Option<()> {
        self.depth += 1;
        (self.depth <= MAX_DEPTH).then_some(())
    }

    fn or(&mut self) -> Option<bool> {
        self.enter()?;
        let mut v = self.and()?;
        while self.eat(b"||") {
            v = self.and()? || v;
        }
        self.depth -= 1;
        Some(v)
    }

    fn and(&mut self) -> Option<bool> {
        let mut v = self.unary()?;
        while self.eat(b"&&") {
            v = self.unary()? && v;
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<bool> {
        self.enter()?;
        let v = if self.eat(b"!") {
            !self.unary()?
        } else if self.eat(b"(") {
            let v = self.or()?;
            if !self.eat(b")") {
                return None;
            }
            v
        } else {
            self.skip_ws();
            let start = self.i;
            while self
                .s
                .get(self.i)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                self.i += 1;
            }
            if self.i == start {
                return None;
            }
            // Every environment / capability token is false here.
            false
        };
        self.depth -= 1;
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires() {
        assert_eq!(
            parse_expires("4 days (update frequency)"),
            Some(Duration::from_secs(4 * 86_400))
        );
        assert_eq!(parse_expires("1 day"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_expires("12 hours"), Some(Duration::from_secs(43_200)));
        assert_eq!(parse_expires("6h"), Some(Duration::from_secs(21_600)));
        assert_eq!(parse_expires("soon"), None);
        assert_eq!(parse_expires("3 weeks"), None);
        assert_eq!(parse_expires("99999999999999999999 days"), None);
    }

    #[test]
    fn metadata() {
        let mut m = ListMetadata::default();
        read_metadata(" Title: EasyList", &mut m);
        read_metadata(" Title: Other", &mut m);
        read_metadata(" Expires: 4 days", &mut m);
        read_metadata(" Homepage: https://easylist.to/", &mut m);
        read_metadata(" Version: 202611100000", &mut m);
        read_metadata(" Last modified: 10 Nov 2026", &mut m);
        read_metadata(" just a comment", &mut m);
        assert_eq!(m.title.as_deref(), Some("EasyList"));
        assert_eq!(m.homepage.as_deref(), Some("https://easylist.to/"));
        assert_eq!(m.version.as_deref(), Some("202611100000"));
        assert_eq!(m.last_modified.as_deref(), Some("10 Nov 2026"));
        assert_eq!(m.expires, Some(Duration::from_secs(4 * 86_400)));
    }

    #[test]
    fn expressions() {
        assert_eq!(eval("env_firefox"), Some(false));
        assert_eq!(eval("!env_safari"), Some(true));
        assert_eq!(eval("!env_a && !env_b"), Some(true));
        assert_eq!(eval("env_a || !(env_b || env_c)"), Some(true));
        assert_eq!(eval("(env_a"), None);
        assert_eq!(eval(""), None);
        assert_eq!(eval("a b"), None);
        let deep = "(".repeat(10_000) + "a" + &")".repeat(10_000);
        assert_eq!(eval(&deep), None);
        let deep = "!".repeat(10_000) + "a";
        assert_eq!(eval(&deep), None);
    }

    #[test]
    fn nesting() {
        let mut c = Conditions::default();
        assert!(c.active());
        assert!(c.directive("if !env_x"));
        assert!(c.active());
        assert!(c.directive("if env_chromium"));
        assert!(!c.active());
        assert!(c.directive("else"));
        assert!(c.active());
        assert!(c.directive("endif"));
        assert!(c.directive("endif"));
        assert!(c.directive("endif")); // unbalanced: ignored
        assert!(c.active());
        assert!(!c.directive("########"));
    }
}
