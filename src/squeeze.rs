// SPDX-License-Identifier: EUPL-1.2
//! Response middleware for the web listener: gzip for JSON/HTML (the dashboard
//! polls over LTE, where every byte is paid for) and a weak ETag so a poll whose
//! answer hasn't changed costs a `304 Not Modified` and no body.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Below this, gzip's own overhead isn't worth it.
const MIN_GZIP: usize = 512;
/// Largest body we buffer to compress (the page is ~150 KB).
const MAX_BODY: usize = 4 << 20;

pub async fn layer(req: Request, next: Next) -> Response {
    let gzip_ok = req.headers().get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(accepts_gzip);
    let if_none_match = req.headers().get(header::IF_NONE_MATCH).cloned();
    let is_get = req.method() == Method::GET;
    let is_api = req.uri().path().starts_with("/api/");
    let res = next.run(req).await;
    if !is_get || res.status() != StatusCode::OK || res.headers().contains_key(header::CONTENT_ENCODING) {
        return res;
    }
    let ct = res.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !(ct.starts_with("application/json") || ct.starts_with("text/html")) {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let h = &mut parts.headers;
    h.insert(header::VARY, HeaderValue::from_static("Accept-Encoding, Cookie"));
    if is_api {
        // the browser may keep it but must ask again each time (and gets a 304 if unchanged)
        h.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("private, no-cache"));
        let tag = etag(&bytes);
        if if_none_match.as_ref().and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == tag)) {
            let mut r = StatusCode::NOT_MODIFIED.into_response();
            for k in [header::ETAG, header::VARY, header::CACHE_CONTROL] {
                if let Some(v) = h.get(&k) {
                    r.headers_mut().insert(k, v.clone());
                }
            }
            r.headers_mut().insert(header::ETAG, HeaderValue::from_str(&tag).expect("ascii"));
            return r;
        }
        h.insert(header::ETAG, HeaderValue::from_str(&tag).expect("ascii"));
    }
    if gzip_ok && bytes.len() >= MIN_GZIP {
        let z = gzip(&bytes);
        h.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        h.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::from(z));
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Weak validator of the uncompressed body: CRC-32 + length.
pub fn etag(body: &[u8]) -> String {
    format!("W/\"{:08x}-{:x}\"", crate::blob::crc32(body), body.len())
}

/// `Accept-Encoding` lists gzip (or *) without q=0.
pub fn accepts_gzip(v: &str) -> bool {
    v.split(',').any(|part| {
        let mut it = part.split(';');
        let name = it.next().unwrap_or("").trim();
        let q0 =
            it.any(|p| matches!(p.trim().strip_prefix("q="), Some(q) if q.trim().parse::<f32>().ok() == Some(0.0)));
        (name.eq_ignore_ascii_case("gzip") || name == "*") && !q0
    })
}

/// RFC 1952 gzip member around a raw deflate stream.
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    out.extend(miniz_oxide::deflate::compress_to_vec(data, 6));
    out.extend_from_slice(&crate::blob::crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_roundtrip_and_crc() {
        assert_eq!(crate::blob::crc32(b"123456789"), 0xCBF4_3926); // the standard check value
        let data = br#"{"services":[{"name":"dongie","state":"active"},{"name":"lnsd","state":"active"}]}"#.repeat(40);
        let z = gzip(&data);
        assert!(z.len() < data.len() / 5);
        assert_eq!(&z[..3], &[0x1f, 0x8b, 8]);
        let raw = miniz_oxide::inflate::decompress_to_vec(&z[10..z.len() - 8]).unwrap();
        assert_eq!(raw, data);
        let n = z.len();
        assert_eq!(u32::from_le_bytes(z[n - 8..n - 4].try_into().unwrap()), crate::blob::crc32(&data));
        assert_eq!(u32::from_le_bytes(z[n - 4..].try_into().unwrap()) as usize, data.len());
    }

    #[test]
    fn accept_encoding() {
        assert!(accepts_gzip("gzip, deflate, br, zstd"));
        assert!(accepts_gzip("br;q=1.0, gzip;q=0.8"));
        assert!(accepts_gzip("*"));
        assert!(!accepts_gzip("gzip;q=0"));
        assert!(!accepts_gzip("br, deflate"));
        assert!(!accepts_gzip(""));
    }

    #[test]
    fn etag_changes_with_content() {
        assert_eq!(etag(b"abc"), etag(b"abc"));
        assert_ne!(etag(b"abc"), etag(b"abd"));
        assert!(etag(b"").starts_with("W/\""));
    }
}
