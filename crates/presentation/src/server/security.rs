// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! HTTP security middleware and active-content protection: response headers,
//! same-origin WebSocket guard, TURN credential signing, and
//! content-type sniffing for XSS-via-download prevention.

use super::*;

/// Baseline security headers on every response: no MIME sniffing, no framing
/// (clickjacking), same-origin referrers.
pub(super) async fn security_headers_mw(
    req: Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "X-Content-Type-Options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        "X-Frame-Options",
        axum::http::HeaderValue::from_static("DENY"),
    );
    h.insert(
        "Referrer-Policy",
        axum::http::HeaderValue::from_static("same-origin"),
    );
    resp
}

/// Same-origin guard: allow when there is no `Origin` (non-browser client) or
/// when its host matches the request `Host`. Blocks browser sockets opened from
/// a different site even if the session cookie were somehow attached.
pub(super) fn origin_ok(headers: &axum::http::HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let origin_host = origin.split("://").nth(1).unwrap_or(origin);
    match headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
        Some(host) => origin_host == host,
        None => false,
    }
}

/// HMAC-SHA1 (coturn's long-term-credential scheme).
pub(super) fn hmac_sha1(key: &[u8], msg: &[u8]) -> Vec<u8> {
    use hmac::Mac;
    let Ok(mut mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(key) else {
        return Vec::new();
    };
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

/// Standard Base64 (for the TURN credential).
pub(super) fn base64_std(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(A[(b0 >> 2) as usize] as char);
        out.push(A[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// True for extensions whose MIME type a browser will execute as script if
/// the file is opened via direct/top-level navigation (SVG documents, HTML,
/// XML). `mime_of` derives Content-Type from the filename alone, so this
/// covers files stored through ANY upload path (avatar, chat attachment,
/// project attachment) — not just the one that first surfaced the bug.
pub(super) fn is_active_content_ext(name: &str) -> bool {
    matches!(
        name.rsplit('.').next().map(str::to_lowercase).as_deref(),
        Some("svg" | "html" | "htm" | "xhtml" | "xml")
    )
}

/// Force a download instead of inline rendering for [`is_active_content_ext`]
/// files, so "open in new tab" / direct navigation can't execute embedded
/// script — the browser downloads the file rather than parsing it as a
/// top-level document.
pub(super) fn force_download_if_active_content(file: &str, resp: &mut axum::response::Response) {
    if is_active_content_ext(file) {
        resp.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            axum::http::HeaderValue::from_static("attachment"),
        );
    }
}
