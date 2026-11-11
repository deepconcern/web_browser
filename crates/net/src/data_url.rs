//! `data:` URLs: the Fetch Standard's
//! [data: URL processor](https://fetch.spec.whatwg.org/#data-url-processor),
//! with the MIME Sniffing Standard's
//! [MIME type parser](https://mimesniff.spec.whatwg.org/#parse-a-mime-type)
//! and Infra's [forgiving-base64 decode](https://infra.spec.whatwg.org/#forgiving-base64-decode).

use wb_url::Url;

use crate::error::NetError;
use crate::types::{is_token_byte, isomorphic_decode};

/// The result of processing a `data:` URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DataUrl {
    /// The serialized MIME type.
    pub(crate) mime_type: String,
    /// The decoded body.
    pub(crate) body: Vec<u8>,
}

fn is_http_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | ' ')
}

/// Processes a `data:` URL, refusing a body over `limit` bytes.
///
/// Decoding never yields more bytes than the URL holds, so the work is
/// linear in the URL's length, and a plain (non-base64) body stops decoding
/// as soon as it passes the limit.
pub(crate) fn process(url: &Url, limit: u64) -> Result<DataUrl, NetError> {
    let max = usize::try_from(limit).unwrap_or(usize::MAX);
    let too_large = NetError::BodyTooLarge { limit };
    if url.scheme() != "data" {
        return Err(NetError::InvalidDataUrl);
    }
    // Steps 2-3: serialize without fragment, drop the leading "data:".
    let serialized = url.without_fragment();
    let input = serialized
        .as_str()
        .get("data:".len()..)
        .ok_or(NetError::InvalidDataUrl)?;
    // Steps 5-9.
    let (mime_raw, encoded_body) = input.split_once(',').ok_or(NetError::InvalidDataUrl)?;
    let mut mime_type = mime_raw
        .trim_matches(|c: char| c.is_ascii_whitespace())
        .to_owned();
    let base64 = strip_base64_suffix(&mime_type);
    // Step 10. A base64 body shrinks when decoded, so only a plain body is
    // cut off here.
    let percent_max = if base64.is_some() { usize::MAX } else { max };
    let mut body =
        percent_decode_max(encoded_body.as_bytes(), percent_max).ok_or(too_large.clone())?;
    // Step 11: a trailing ";base64" (with optional spaces before "base64").
    if let Some(stripped) = base64 {
        body =
            forgiving_base64_decode(&isomorphic_decode(&body)).ok_or(NetError::InvalidDataUrl)?;
        mime_type = stripped;
    }
    if body.len() > max {
        return Err(too_large);
    }
    // Step 12.
    if mime_type.starts_with(';') {
        mime_type.insert_str(0, "text/plain");
    }
    // Steps 13-14.
    let mime_type =
        parse_mime_type(&mime_type).unwrap_or_else(|| "text/plain;charset=US-ASCII".to_owned());
    Ok(DataUrl { mime_type, body })
}

/// If `mime` ends with `;`, spaces, then `base64` (case-insensitive), returns
/// the MIME type with that suffix removed (steps 11.4-11.6).
fn strip_base64_suffix(mime: &str) -> Option<String> {
    let len = mime.len();
    if len < 6 {
        return None;
    }
    let tail = mime.get(len - 6..)?;
    if !tail.eq_ignore_ascii_case("base64") {
        return None;
    }
    let rest = mime.get(..len - 6)?.trim_end_matches(' ');
    let rest = rest.strip_suffix(';')?;
    Some(rest.to_owned())
}

/// [Percent-decode](https://url.spec.whatwg.org/#percent-decode) a byte sequence.
#[cfg(test)]
pub(crate) fn percent_decode(input: &[u8]) -> Vec<u8> {
    percent_decode_max(input, usize::MAX).unwrap_or_default()
}

/// Percent-decodes, giving up (`None`) once the output passes `max` bytes.
fn percent_decode_max(input: &[u8], max: usize) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len().min(max.saturating_add(1)));
    let mut i = 0;
    while let Some(&b) = input.get(i) {
        if out.len() > max {
            return None;
        }
        if b == b'%' {
            let hi = input.get(i + 1).and_then(|c| (*c as char).to_digit(16));
            let lo = input.get(i + 2).and_then(|c| (*c as char).to_digit(16));
            if let (Some(hi), Some(lo)) = (hi, lo) {
                // hi and lo are < 16, so this fits in a byte.
                out.push(u8::try_from(hi * 16 + lo).unwrap_or(0));
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    (out.len() <= max).then_some(out)
}

/// Infra's forgiving-base64 decode.
pub(crate) fn forgiving_base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = input
        .bytes()
        .filter(|b| !matches!(b, b'\t' | b'\n' | b'\x0C' | b'\r' | b' '))
        .collect();
    if data.len().is_multiple_of(4) {
        if data.ends_with(b"==") {
            data.truncate(data.len() - 2);
        } else if data.ends_with(b"=") {
            data.truncate(data.len() - 1);
        }
    }
    if data.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for b in data {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buffer = ((buffer << 6) | u32::from(v)) & 0x00FF_FFFF;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xFF) as u8);
        }
    }
    // Leftover bits (4 or 2) are discarded, as the spec requires.
    Some(out)
}

fn is_quoted_string_token_char(c: char) -> bool {
    c == '\t' || (' '..='~').contains(&c) || ('\u{80}'..='\u{FF}').contains(&c)
}

fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_token_byte)
}

/// [Parse a MIME type](https://mimesniff.spec.whatwg.org/#parse-a-mime-type)
/// and return its [serialization](https://mimesniff.spec.whatwg.org/#serialize-a-mime-type),
/// or `None` on failure.
pub(crate) fn parse_mime_type(input: &str) -> Option<String> {
    let input = input.trim_matches(is_http_whitespace);
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0usize;
    let collect = |pos: &mut usize, stop: &dyn Fn(char) -> bool| -> String {
        let mut s = String::new();
        while let Some(&c) = chars.get(*pos) {
            if stop(c) {
                break;
            }
            s.push(c);
            *pos += 1;
        }
        s
    };
    let ty = collect(&mut pos, &|c| c == '/');
    if !is_token(&ty) || pos >= chars.len() {
        return None;
    }
    pos += 1;
    let subtype = collect(&mut pos, &|c| c == ';');
    let subtype = subtype.trim_end_matches(is_http_whitespace).to_owned();
    if !is_token(&subtype) {
        return None;
    }
    let mut out = format!(
        "{}/{}",
        ty.to_ascii_lowercase(),
        subtype.to_ascii_lowercase()
    );
    let mut names: Vec<String> = Vec::new();
    while pos < chars.len() {
        pos += 1; // Skip ';'.
        while chars.get(pos).is_some_and(|c| is_http_whitespace(*c)) {
            pos += 1;
        }
        let name = collect(&mut pos, &|c| c == ';' || c == '=').to_ascii_lowercase();
        match chars.get(pos) {
            None => break,
            Some(';') => continue,
            Some(_) => pos += 1, // Skip '='.
        }
        if pos >= chars.len() {
            break;
        }
        let value = if chars.get(pos) == Some(&'"') {
            let v = collect_quoted_string(&chars, &mut pos);
            collect(&mut pos, &|c| c == ';');
            v
        } else {
            let v = collect(&mut pos, &|c| c == ';');
            let v = v.trim_end_matches(is_http_whitespace).to_owned();
            if v.is_empty() {
                continue;
            }
            v
        };
        if is_token(&name)
            && value.chars().all(is_quoted_string_token_char)
            && !names.contains(&name)
        {
            out.push(';');
            out.push_str(&name);
            out.push('=');
            if is_token(&value) {
                out.push_str(&value);
            } else {
                out.push('"');
                for c in value.chars() {
                    if c == '"' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
            }
            names.push(name);
        }
    }
    Some(out)
}

/// [Collect an HTTP quoted string](https://fetch.spec.whatwg.org/#collect-an-http-quoted-string)
/// with *extract-value* set.
fn collect_quoted_string(chars: &[char], pos: &mut usize) -> String {
    let mut value = String::new();
    *pos += 1; // Skip the opening quote.
    loop {
        while let Some(&c) = chars.get(*pos) {
            if c == '"' || c == '\\' {
                break;
            }
            value.push(c);
            *pos += 1;
        }
        let Some(&q) = chars.get(*pos) else { break };
        *pos += 1;
        if q == '\\' {
            match chars.get(*pos) {
                None => {
                    value.push('\\');
                    break;
                }
                Some(&c) => {
                    value.push(c);
                    *pos += 1;
                }
            }
        } else {
            break;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<(String, Vec<u8>)> {
        process(&Url::parse(s).unwrap(), u64::MAX)
            .ok()
            .map(|d| (d.mime_type, d.body))
    }

    #[test]
    fn data_urls() {
        // Cases from WPT fetch/data-urls/resources/data-urls.json.
        assert_eq!(
            p("data://test/,X"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:,X"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(p("data:"), None);
        assert_eq!(p("data:text/html"), None);
        assert_eq!(
            p("data:,"),
            Some(("text/plain;charset=US-ASCII".into(), vec![]))
        );
        assert_eq!(
            p("data:,X#X"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:,%FF"),
            Some(("text/plain;charset=US-ASCII".into(), vec![0xFF]))
        );
        assert_eq!(
            p("data:text/plain,X"),
            Some(("text/plain".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/plain ,X"),
            Some(("text/plain".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/plain%20,X"),
            Some(("text/plain%20".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/plain\u{0C},X"),
            Some(("text/plain%0c".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/plain%0C,X"),
            Some(("text/plain%0c".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:;x=x;charset=x,X"),
            Some(("text/plain;x=x;charset=x".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:;x=x,X"),
            Some(("text/plain;x=x".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/html;charset=gbk,X"),
            Some(("text/html;charset=gbk".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:IMAGE/gif,%C2%B1"),
            Some(("image/gif".into(), vec![0xC2, 0xB1]))
        );
        assert_eq!(
            p("data:IMAGE/gif;hi=x,%C2%B1"),
            Some(("image/gif;hi=x".into(), vec![0xC2, 0xB1]))
        );
        assert_eq!(
            p("data:IMAGE/gif;CHARSET=x,%C2%B1"),
            Some(("image/gif;charset=x".into(), vec![0xC2, 0xB1]))
        );
        assert_eq!(
            p("data: ,%FF"),
            Some(("text/plain;charset=US-ASCII".into(), vec![0xFF]))
        );
        assert_eq!(
            p("data:%20,%FF"),
            Some(("text/plain;charset=US-ASCII".into(), vec![0xFF]))
        );
        assert_eq!(
            p("data:x/x;base64;base64,WA"),
            Some(("x/x".into(), b"X".to_vec()))
        );
    }

    #[test]
    fn base64_data_urls() {
        assert_eq!(
            p("data:;base64,WA"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:;base64,WA=="),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(p("data:;base64,WA="), None);
        assert_eq!(
            p("data:;base64,W A"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:;base64,W%20A"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:;base64,W%0CA"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(p("data:;base64,WA%"), None);
        assert_eq!(p("data:;base64,WA%3D"), None, "WA= has a misplaced pad");
        assert_eq!(
            p("data:;base64,WA%3D%3D"),
            Some(("text/plain;charset=US-ASCII".into(), b"X".to_vec()))
        );
        assert_eq!(p("data:;base64,%3D"), None);
        assert_eq!(
            p("data:text/plain;base64 ,X"),
            None,
            "one base64 character is invalid"
        );
        assert_eq!(
            p("data:text/plain;base64 ,WA"),
            Some(("text/plain".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/plain;  base64,WA"),
            Some(("text/plain".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/plain;BASE64,WA"),
            Some(("text/plain".into(), b"X".to_vec()))
        );
        assert_eq!(
            p("data:text/plain;base64,aGVsbG8="),
            Some(("text/plain".into(), b"hello".to_vec()))
        );
    }

    #[test]
    fn mime_types() {
        assert_eq!(
            parse_mime_type("text/html;charset=\"utf-8\"").as_deref(),
            Some("text/html;charset=utf-8")
        );
        assert_eq!(
            parse_mime_type("text/html;charset=\"a b\"").as_deref(),
            Some("text/html;charset=\"a b\"")
        );
        assert_eq!(
            parse_mime_type("text/html;x=\"a\\\"b\"").as_deref(),
            Some("text/html;x=\"a\\\"b\"")
        );
        assert_eq!(
            parse_mime_type("text/html;a=1;a=2").as_deref(),
            Some("text/html;a=1")
        );
        assert_eq!(
            parse_mime_type("text/html;=x;b").as_deref(),
            Some("text/html")
        );
        assert_eq!(
            parse_mime_type("text/html;b=").as_deref(),
            Some("text/html")
        );
        assert_eq!(parse_mime_type(" TEXT/HTML ").as_deref(), Some("text/html"));
        assert_eq!(parse_mime_type("text"), None);
        assert_eq!(parse_mime_type("/html"), None);
        assert_eq!(parse_mime_type("te xt/html"), None);
        assert_eq!(parse_mime_type("text/"), None);
        assert_eq!(
            parse_mime_type("text/html;x=\"unterminated").as_deref(),
            Some("text/html;x=unterminated")
        );
    }

    #[test]
    fn forgiving_base64() {
        assert_eq!(forgiving_base64_decode(""), Some(vec![]));
        assert_eq!(forgiving_base64_decode("YQ"), Some(b"a".to_vec()));
        assert_eq!(forgiving_base64_decode("YWI"), Some(b"ab".to_vec()));
        assert_eq!(forgiving_base64_decode("YWJj"), Some(b"abc".to_vec()));
        assert_eq!(forgiving_base64_decode("Y"), None);
        assert_eq!(forgiving_base64_decode("YQ==="), None);
        assert_eq!(forgiving_base64_decode("Y=Q="), None);
        assert_eq!(forgiving_base64_decode(" Y W J j "), Some(b"abc".to_vec()));
    }

    #[test]
    fn percent() {
        assert_eq!(percent_decode(b"%41%4a%zz%4"), b"AJ%zz%4".to_vec());
    }

    #[test]
    fn size_limit() {
        let url = |s: &str| Url::parse(s).unwrap();
        assert_eq!(process(&url("data:,abcd"), 4).unwrap().body, b"abcd");
        assert_eq!(
            process(&url("data:,abcde"), 4),
            Err(NetError::BodyTooLarge { limit: 4 })
        );
        // %41%41 is 6 URL bytes but 2 body bytes: the limit is on the body.
        assert_eq!(process(&url("data:,%41%41"), 2).unwrap().body, b"AA");
        assert_eq!(process(&url("data:;base64,YWJj"), 3).unwrap().body, b"abc");
        assert_eq!(
            process(&url("data:;base64,YWJjZA=="), 3),
            Err(NetError::BodyTooLarge { limit: 3 })
        );
        assert_eq!(process(&url("data:x"), 3), Err(NetError::InvalidDataUrl));
        assert_eq!(
            process(&url("https://a.test/"), 3),
            Err(NetError::InvalidDataUrl)
        );
    }
}
