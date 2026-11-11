//! A small `Content-Type` parser (MIME Sniffing §4.4 "parse a MIME type"),
//! enough to get the essence and the `charset` parameter.

/// A parsed `Content-Type` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MimeType {
    /// `type/subtype`, ASCII-lowercased.
    pub(crate) essence: String,
    /// The first `charset` parameter's value, if any.
    pub(crate) charset: Option<String>,
}

/// HTTP whitespace (Fetch §2.2): tab, LF, CR and space.
fn is_http_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

/// An HTTP token code point (Fetch §2.2, RFC 9110 `tchar`).
fn is_token(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
}

/// Parses `input` per MIME Sniffing §4.4. Returns `None` for an invalid
/// value. Parameters other than `charset` are skipped.
pub(crate) fn parse(input: &str) -> Option<MimeType> {
    let input = input.trim_matches(is_http_ws);
    let (ty, rest) = input.split_once('/')?;
    if ty.is_empty() || !ty.chars().all(is_token) {
        return None;
    }
    let (sub, params) = match rest.find(';') {
        Some(i) => (rest.get(..i)?, rest.get(i + 1..).unwrap_or("")),
        None => (rest, ""),
    };
    let sub = sub.trim_end_matches(is_http_ws);
    if sub.is_empty() || !sub.chars().all(is_token) {
        return None;
    }
    let essence = format!("{}/{}", ty.to_ascii_lowercase(), sub.to_ascii_lowercase());
    Some(MimeType {
        essence,
        charset: charset_param(params),
    })
}

/// Finds the first valid `charset` parameter in the text after the first
/// `;` (MIME Sniffing §4.4 steps 11–12, simplified: quoted values may
/// contain `;` and backslash escapes).
fn charset_param(mut s: &str) -> Option<String> {
    loop {
        s = s.trim_start_matches(is_http_ws);
        if s.is_empty() {
            return None;
        }
        let name_end = s.find([';', '=']).unwrap_or(s.len());
        let name = s.get(..name_end)?.to_ascii_lowercase();
        let after = s.get(name_end..)?;
        if !after.starts_with('=') {
            s = after.get(1..).unwrap_or("");
            continue;
        }
        let after = after.get(1..)?;
        let (value, rest) = if let Some(quoted) = after.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = quoted.char_indices();
            let mut end = quoted.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, n)) = chars.next() {
                            out.push(n);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    _ => out.push(c),
                }
            }
            let rest = quoted.get(end..).unwrap_or("");
            let rest = rest.find(';').and_then(|i| rest.get(i + 1..)).unwrap_or("");
            (out, rest)
        } else {
            let end = after.find(';').unwrap_or(after.len());
            let v = after.get(..end)?.trim_end_matches(is_http_ws).to_string();
            (v, after.get(end + 1..).unwrap_or(""))
        };
        if name == "charset" && !value.is_empty() && name.chars().all(is_token) {
            return Some(value);
        }
        s = rest;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn essence_and_charset() {
        let m = parse("Text/HTML; Charset=\"Shift_JIS\"").unwrap();
        assert_eq!(m.essence, "text/html");
        assert_eq!(m.charset.as_deref(), Some("Shift_JIS"));
        let m = parse("text/css;foo=bar;charset=utf-8 ").unwrap();
        assert_eq!(m.charset.as_deref(), Some("utf-8"));
        let m = parse("text/plain;charset=\"a\\\"b\";x=y").unwrap();
        assert_eq!(m.charset.as_deref(), Some("a\"b"));
        assert_eq!(parse("image/png").unwrap().charset, None);
    }

    #[test]
    fn invalid() {
        assert_eq!(parse("text"), None);
        assert_eq!(parse("/html"), None);
        assert_eq!(parse("text/"), None);
        assert_eq!(parse("te xt/html"), None);
        assert_eq!(parse(""), None);
    }
}
