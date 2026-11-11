//! Determining the character encoding (HTML §13.2.3.2) and decoding.
//!
//! `encoding_rs` provides the WHATWG Encoding Standard's labels and decoders;
//! it stays private to this module.

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};

/// How sure the parser is about the encoding (HTML §13.2.3.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Confidence {
    /// The encoding was a guess (from the prescan or the default), and a
    /// `meta` declaration later in the document may change it.
    Tentative,
    /// The encoding came from a byte order mark or the transport layer.
    Certain,
}

/// The result of the encoding sniffing algorithm.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SniffedEncoding {
    /// The encoding's name as the Encoding Standard spells it, e.g. `UTF-8`
    /// or `windows-1252`.
    pub name: &'static str,
    /// The confidence.
    pub confidence: Confidence,
}

/// Whether `label` names an encoding ("get an encoding", Encoding §4.2).
pub(crate) fn is_known_label(label: &str) -> bool {
    Encoding::for_label(label.as_bytes()).is_some()
}

/// The encoding for a label, by its canonical name.
pub(crate) fn lookup(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.as_bytes())
}

/// BOM sniffing (Encoding §4.1 "BOM sniff").
fn bom_sniff(bytes: &[u8]) -> Option<&'static Encoding> {
    match bytes {
        [0xEF, 0xBB, 0xBF, ..] => Some(UTF_8),
        [0xFE, 0xFF, ..] => Some(UTF_16BE),
        [0xFF, 0xFE, ..] => Some(UTF_16LE),
        _ => None,
    }
}

/// The encoding sniffing algorithm (§13.2.3.2) for a resource with these
/// bytes and, optionally, a `charset` parameter from its `Content-Type`.
/// Without any declaration it returns windows-1252, the spec's suggested
/// default for locales without a specific one.
#[must_use]
pub fn sniff_encoding(bytes: &[u8], transport_charset: Option<&str>) -> SniffedEncoding {
    let (encoding, confidence) = sniff(bytes, transport_charset);
    SniffedEncoding {
        name: encoding.name(),
        confidence,
    }
}

pub(crate) fn sniff(
    bytes: &[u8],
    transport_charset: Option<&str>,
) -> (&'static Encoding, Confidence) {
    if let Some(encoding) = bom_sniff(bytes) {
        return (encoding, Confidence::Certain);
    }
    if let Some(encoding) = transport_charset.and_then(lookup) {
        return (encoding, Confidence::Certain);
    }
    // Prescan the first 1024 bytes, as the spec encourages.
    let head = bytes.get(..bytes.len().min(1024)).unwrap_or(bytes);
    if let Some(encoding) = prescan(head) {
        return (encoding, Confidence::Tentative);
    }
    (WINDOWS_1252, Confidence::Tentative)
}

/// Decodes `bytes` with `encoding` (Encoding §4.1 "decode", which also
/// honours a byte order mark).
pub(crate) fn decode(bytes: &[u8], encoding: &'static Encoding) -> String {
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Whether `encoding` is UTF-16BE or UTF-16LE.
pub(crate) fn is_utf16(encoding: &'static Encoding) -> bool {
    encoding == UTF_16BE || encoding == UTF_16LE
}

/// Adjusts an encoding found in the document per §13.2.3.2: UTF-16 becomes
/// UTF-8 and x-user-defined becomes windows-1252.
pub(crate) fn adjust_declared(encoding: &'static Encoding) -> &'static Encoding {
    if is_utf16(encoding) {
        UTF_8
    } else if encoding == X_USER_DEFINED {
        WINDOWS_1252
    } else {
        encoding
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// "Prescan a byte stream to determine its encoding" (§13.2.3.2).
fn prescan(bytes: &[u8]) -> Option<&'static Encoding> {
    match bytes {
        [0x3C, 0x00, 0x3F, 0x00, 0x78, 0x00, ..] => return Some(UTF_16LE),
        [0x00, 0x3C, 0x00, 0x3F, 0x00, 0x78, ..] => return Some(UTF_16BE),
        _ => {}
    }
    let mut pos = 0;
    while pos < bytes.len() {
        let rest = bytes.get(pos..).unwrap_or_default();
        if rest.starts_with(b"<!--") {
            // Skip to the end of the comment: a '>' preceded by "--" (the
            // dashes may be those of "<!--").
            let mut end = pos + 2;
            loop {
                match bytes.get(end..end + 3) {
                    Some(b"-->") => {
                        pos = end + 3;
                        break;
                    }
                    Some(_) => end += 1,
                    None => return get_xml_encoding(bytes),
                }
            }
            continue;
        }
        if rest.len() >= 6
            && rest
                .get(..5)
                .is_some_and(|s| s.eq_ignore_ascii_case(b"<meta"))
            && rest.get(5).is_some_and(|&b| is_space(b) || b == b'/')
        {
            pos += 5;
            match prescan_meta(bytes, &mut pos) {
                MetaResult::Found(encoding) => return Some(encoding),
                MetaResult::OutOfBytes => return get_xml_encoding(bytes),
                MetaResult::NotFound => {}
            }
            pos += 1;
            continue;
        }
        let is_tag = match rest {
            [b'<', b'/', c, ..] | [b'<', c, ..] => c.is_ascii_alphabetic(),
            _ => false,
        };
        if is_tag {
            pos += 1;
            while bytes.get(pos).is_some_and(|&b| !is_space(b) && b != b'>') {
                pos += 1;
            }
            if pos >= bytes.len() {
                return get_xml_encoding(bytes);
            }
            loop {
                match get_attribute(bytes, &mut pos) {
                    Attr::Found(..) => {}
                    Attr::None => break,
                    Attr::OutOfBytes => return get_xml_encoding(bytes),
                }
            }
            pos += 1;
            continue;
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"</") || rest.starts_with(b"<?") {
            match bytes
                .get(pos + 1..)
                .and_then(|r| r.iter().position(|&b| b == b'>'))
            {
                Some(offset) => pos += 1 + offset + 1,
                None => return get_xml_encoding(bytes),
            }
            continue;
        }
        pos += 1;
    }
    get_xml_encoding(bytes)
}

enum MetaResult {
    Found(&'static Encoding),
    NotFound,
    OutOfBytes,
}

/// The `<meta` branch of the prescan; `pos` points at the space or slash
/// after `<meta`.
fn prescan_meta(bytes: &[u8], pos: &mut usize) -> MetaResult {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let mut got_pragma = false;
    let mut need_pragma: Option<bool> = None;
    let mut charset: Option<Option<&'static Encoding>> = None;
    loop {
        match get_attribute(bytes, pos) {
            Attr::OutOfBytes => return MetaResult::OutOfBytes,
            Attr::None => break,
            Attr::Found(name, value) => {
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name.clone());
                match name.as_slice() {
                    b"http-equiv" => {
                        if value == b"content-type" {
                            got_pragma = true;
                        }
                    }
                    b"content" => {
                        let text = String::from_utf8_lossy(&value);
                        if let Some(label) = extract_from_meta_content(&text)
                            && charset.is_none()
                        {
                            charset = Some(lookup(&label));
                            need_pragma = Some(true);
                        }
                    }
                    b"charset" => {
                        charset = Some(lookup(&String::from_utf8_lossy(&value)));
                        need_pragma = Some(false);
                    }
                    _ => {}
                }
            }
        }
    }
    match need_pragma {
        None => MetaResult::NotFound,
        Some(true) if !got_pragma => MetaResult::NotFound,
        _ => match charset.flatten() {
            Some(encoding) => MetaResult::Found(adjust_declared(encoding)),
            None => MetaResult::NotFound,
        },
    }
}

enum Attr {
    Found(Vec<u8>, Vec<u8>),
    None,
    OutOfBytes,
}

/// "Get an attribute" from the prescan algorithm.
fn get_attribute(bytes: &[u8], pos: &mut usize) -> Attr {
    let at = |p: usize| bytes.get(p).copied();
    while at(*pos).is_some_and(|b| is_space(b) || b == b'/') {
        *pos += 1;
    }
    match at(*pos) {
        None => return Attr::OutOfBytes,
        Some(b'>') => return Attr::None,
        Some(_) => {}
    }
    let mut name = Vec::new();
    let mut value = Vec::new();
    // Name.
    loop {
        match at(*pos) {
            None => return Attr::OutOfBytes,
            Some(b'=') if !name.is_empty() => {
                *pos += 1;
                break;
            }
            Some(b) if is_space(b) => {
                // Spaces, then maybe '='.
                while at(*pos).is_some_and(is_space) {
                    *pos += 1;
                }
                match at(*pos) {
                    None => return Attr::OutOfBytes,
                    Some(b'=') => {
                        *pos += 1;
                        break;
                    }
                    Some(_) => return Attr::Found(name, value),
                }
            }
            Some(b'/' | b'>') => return Attr::Found(name, value),
            Some(b) => {
                name.push(b.to_ascii_lowercase());
                *pos += 1;
            }
        }
    }
    // Value.
    while at(*pos).is_some_and(is_space) {
        *pos += 1;
    }
    match at(*pos) {
        None => Attr::OutOfBytes,
        Some(quote @ (b'"' | b'\'')) => {
            *pos += 1;
            loop {
                match at(*pos) {
                    None => return Attr::OutOfBytes,
                    Some(b) if b == quote => {
                        *pos += 1;
                        return Attr::Found(name, value);
                    }
                    Some(b) => {
                        value.push(b.to_ascii_lowercase());
                        *pos += 1;
                    }
                }
            }
        }
        Some(b'>') => Attr::Found(name, value),
        Some(_) => loop {
            match at(*pos) {
                None => return Attr::OutOfBytes,
                Some(b) if is_space(b) || b == b'>' => return Attr::Found(name, value),
                Some(b) => {
                    value.push(b.to_ascii_lowercase());
                    *pos += 1;
                }
            }
        },
    }
}

/// "Get an XML encoding" (§13.2.3.2), used when the prescan runs out of
/// bytes.
fn get_xml_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    if !bytes.starts_with(b"<?xml") {
        return None;
    }
    let end = bytes.iter().position(|&b| b == b'>')?;
    let declaration = bytes.get(..end)?;
    let start = declaration.windows(8).position(|w| w == b"encoding")? + 8;
    let mut pos = start;
    let at = |p: usize| declaration.get(p).copied();
    while at(pos).is_some_and(|b| b <= 0x20) {
        pos += 1;
    }
    if at(pos) != Some(b'=') {
        return None;
    }
    pos += 1;
    while at(pos).is_some_and(|b| b <= 0x20) {
        pos += 1;
    }
    let quote = at(pos).filter(|&b| b == b'"' || b == b'\'')?;
    pos += 1;
    let rest = bytes.get(pos..)?;
    let len = rest.iter().position(|&b| b == quote)?;
    let label = rest.get(..len)?;
    if label.iter().any(|&b| b <= 0x20) {
        return None;
    }
    Encoding::for_label(label).map(adjust_declared)
}

/// The algorithm for extracting a character encoding from a meta element
/// (HTML §2.6.4 / §4.2.5): returns the label, if any.
pub(crate) fn extract_from_meta_content(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut pos = 0;
    loop {
        let found = bytes
            .get(pos..)?
            .windows(7)
            .position(|w| w.eq_ignore_ascii_case(b"charset"))?;
        pos += found + 7;
        while bytes.get(pos).is_some_and(|&b| is_space(b)) {
            pos += 1;
        }
        if bytes.get(pos) != Some(&b'=') {
            continue;
        }
        pos += 1;
        while bytes.get(pos).is_some_and(|&b| is_space(b)) {
            pos += 1;
        }
        let rest = bytes.get(pos..)?;
        let label = match rest.first()? {
            quote @ (b'"' | b'\'') => {
                let inner = rest.get(1..)?;
                let end = inner.iter().position(|b| b == quote)?;
                inner.get(..end)?
            }
            _ => {
                let end = rest
                    .iter()
                    .position(|&b| is_space(b) || b == b';')
                    .unwrap_or(rest.len());
                rest.get(..end)?
            }
        };
        let label = String::from_utf8_lossy(label).into_owned();
        return lookup(&label).map(|_| label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_wins() {
        let sniffed = sniff_encoding(b"\xEF\xBB\xBF<meta charset=latin2>", Some("koi8-r"));
        assert_eq!(sniffed.name, "UTF-8");
        assert_eq!(sniffed.confidence, Confidence::Certain);
        assert_eq!(sniff_encoding(b"\xFF\xFEa\0", None).name, "UTF-16LE");
    }

    #[test]
    fn transport_charset_beats_meta() {
        let sniffed = sniff_encoding(b"<meta charset=latin2>", Some("koi8-r"));
        assert_eq!(sniffed.name, "KOI8-R");
        assert_eq!(sniffed.confidence, Confidence::Certain);
    }

    #[test]
    fn prescan_finds_meta_charset() {
        let html = b"<!doctype html><!-- <meta charset=koi8-r> --><html><head>\
                     <meta name=x content='y'><meta charset=\"ISO-8859-2\">";
        let sniffed = sniff_encoding(html, None);
        assert_eq!(sniffed.name, "ISO-8859-2");
        assert_eq!(sniffed.confidence, Confidence::Tentative);
    }

    #[test]
    fn prescan_finds_http_equiv() {
        let html = b"<meta http-equiv=Content-Type content='text/html; charset=shift_jis'>";
        assert_eq!(sniff_encoding(html, None).name, "Shift_JIS");
        // Without the pragma the content attribute does not count.
        let html = b"<meta content='text/html; charset=shift_jis'>";
        assert_eq!(sniff_encoding(html, None).name, "windows-1252");
    }

    #[test]
    fn utf16_declarations_become_utf8() {
        assert_eq!(sniff_encoding(b"<meta charset=utf-16>", None).name, "UTF-8");
        assert_eq!(
            sniff_encoding(b"<?xml version='1.0' encoding='utf-16'?>", None).name,
            "UTF-8"
        );
    }

    #[test]
    fn extracts_charset_from_content() {
        assert_eq!(
            extract_from_meta_content("text/html; charset=\"utf-8\"").as_deref(),
            Some("utf-8")
        );
        assert_eq!(
            extract_from_meta_content("text/html;charset = big5 ;x").as_deref(),
            Some("big5")
        );
        assert_eq!(extract_from_meta_content("text/html; charset='utf-8"), None);
        assert_eq!(extract_from_meta_content("text/html"), None);
    }

    #[test]
    fn prescan_does_not_panic_on_truncated_input() {
        for input in [
            &b"<meta"[..],
            b"<meta charset=",
            b"<meta charset='utf",
            b"<!--",
            b"<a b",
            b"<?xml encoding",
            b"<",
        ] {
            let _ = sniff_encoding(input, None);
        }
    }
}
