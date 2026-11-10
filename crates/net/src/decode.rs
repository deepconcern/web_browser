//! Content-Encoding decoding ([RFC 9110 §8.4](https://www.rfc-editor.org/rfc/rfc9110#section-8.4)):
//! `gzip` (and `x-gzip`), `deflate`, `br` and `identity`.

use std::io::Read;

use crate::error::NetError;

/// The value sent in `Accept-Encoding`.
pub(crate) const ACCEPT_ENCODING: &str = "gzip, deflate, br";

/// Decodes `body` according to a `Content-Encoding` header value (a list
/// applied in order, so decoded in reverse). Unknown codings leave the body
/// as it is, which is what browsers do. The decoded size is capped at
/// `limit` bytes to defuse decompression bombs.
pub(crate) fn decode(
    body: Vec<u8>,
    content_encoding: &str,
    limit: u64,
) -> Result<Vec<u8>, NetError> {
    let codings: Vec<String> = content_encoding
        .split(',')
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty())
        .collect();
    let mut body = body;
    for coding in codings.iter().rev() {
        body = match coding.as_str() {
            "gzip" | "x-gzip" => read_limited(
                flate2::read::MultiGzDecoder::new(body.as_slice()),
                limit,
                "gzip",
            )?,
            "deflate" => inflate(&body, limit)?,
            "br" => read_limited(
                brotli_decompressor::Decompressor::new(body.as_slice(), 4096),
                limit,
                "br",
            )?,
            "identity" => body,
            // Unknown coding: stop and hand over what we have.
            _ => return Ok(body),
        };
    }
    Ok(body)
}

/// `deflate` is meant to be zlib-wrapped (RFC 1950), but some servers send
/// raw DEFLATE (RFC 1951); accept both.
fn inflate(body: &[u8], limit: u64) -> Result<Vec<u8>, NetError> {
    let looks_zlib = match body {
        [cmf, flg, ..] => cmf & 0x0F == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0,
        _ => false,
    };
    if looks_zlib {
        read_limited(flate2::read::ZlibDecoder::new(body), limit, "deflate")
    } else {
        read_limited(flate2::read::DeflateDecoder::new(body), limit, "deflate")
    }
}

fn read_limited<R: Read>(reader: R, limit: u64, what: &str) -> Result<Vec<u8>, NetError> {
    let mut out = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|e| NetError::Decode(format!("{what}: {e}")))?;
    if out.len() as u64 > limit {
        return Err(NetError::BodyTooLarge { limit });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn gzip_and_deflate() {
        let text = b"hello hello hello hello".to_vec();
        assert_eq!(decode(gzip(&text), "gzip", 1000).unwrap(), text);
        assert_eq!(decode(gzip(&text), "X-GZIP", 1000).unwrap(), text);

        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&text).unwrap();
        assert_eq!(decode(z.finish().unwrap(), "deflate", 1000).unwrap(), text);

        let mut raw =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        raw.write_all(&text).unwrap();
        assert_eq!(
            decode(raw.finish().unwrap(), "deflate", 1000).unwrap(),
            text
        );
    }

    #[test]
    fn brotli() {
        // A hand-built RFC 7932 stream: WBITS=16, one uncompressed
        // meta-block (MNIBBLES=4, MLEN-1=4) holding "hello", then an empty
        // last meta-block (ISLAST=1, ISLASTEMPTY=1).
        let compressed = [0x40, 0x00, 0x10, b'h', b'e', b'l', b'l', b'o', 0x03];
        assert_eq!(decode(compressed.to_vec(), "br", 1000).unwrap(), b"hello");
        assert!(matches!(
            decode(vec![0xff, 0xff, 0xff], "br", 1000),
            Err(NetError::Decode(_))
        ));
    }

    #[test]
    fn stacked_and_unknown() {
        let text = b"abc".to_vec();
        let twice = gzip(&gzip(&text));
        assert_eq!(decode(twice, "gzip, gzip", 1000).unwrap(), text);
        assert_eq!(decode(text.clone(), "identity", 1000).unwrap(), text);
        assert_eq!(decode(text.clone(), "", 1000).unwrap(), text);
        assert_eq!(decode(text.clone(), "zstd", 1000).unwrap(), text);
    }

    #[test]
    fn limits_and_corruption() {
        let big = vec![b'a'; 100_000];
        assert_eq!(
            decode(gzip(&big), "gzip", 1000),
            Err(NetError::BodyTooLarge { limit: 1000 })
        );
        assert!(matches!(
            decode(b"not gzip".to_vec(), "gzip", 1000),
            Err(NetError::Decode(_))
        ));
    }
}
