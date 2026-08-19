// Split from server/mod.rs — avatar/media upload hardening tests.
#![allow(clippy::wildcard_imports)]
use super::*;
use axum::extract::FromRequest;

const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
const SVG_BODY: &[u8] =
    b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";

#[test]
fn a_genuine_png_is_recognized_regardless_of_claimed_content_type() {
    assert_eq!(sniff_avatar_image(PNG_MAGIC), Some(("png", "image/png")));
}

#[test]
fn a_genuine_jpeg_is_recognized() {
    assert_eq!(sniff_avatar_image(JPEG_MAGIC), Some(("jpg", "image/jpeg")));
}

#[test]
fn a_genuine_gif_is_recognized() {
    assert_eq!(
        sniff_avatar_image(b"GIF89a...."),
        Some(("gif", "image/gif"))
    );
}

#[test]
fn a_genuine_webp_is_recognized() {
    let mut riff = b"RIFF".to_vec();
    riff.extend_from_slice(&[0, 0, 0, 0]);
    riff.extend_from_slice(b"WEBP");
    assert_eq!(sniff_avatar_image(&riff), Some(("webp", "image/webp")));
}

/// The exact repro from the ticket: an SVG document with an embedded
/// `<script>`, regardless of what multipart Content-Type accompanies it,
/// must never be accepted as an avatar.
#[test]
fn an_svg_document_is_rejected_even_though_it_could_claim_image_svg() {
    assert_eq!(sniff_avatar_image(SVG_BODY), None);
}

#[test]
fn arbitrary_non_image_bytes_are_rejected() {
    assert_eq!(sniff_avatar_image(b"<html><body>hi</body></html>"), None);
    assert_eq!(sniff_avatar_image(b"not an image"), None);
    assert_eq!(sniff_avatar_image(b""), None);
}

/// Regression guard for the sink shared by every upload path (avatar,
/// chat attachment, project attachment): a `.svg`/`.html`/`.xml` name must
/// never be rendered inline, since `mime_of` derives Content-Type from
/// the filename alone and would otherwise let a browser execute it on
/// direct navigation.
#[test]
fn script_capable_extensions_are_flagged_for_forced_download() {
    assert!(is_active_content_ext("evil.svg"));
    assert!(is_active_content_ext("evil.HTML"));
    assert!(is_active_content_ext("evil.xhtml"));
    assert!(is_active_content_ext("evil.xml"));
    assert!(!is_active_content_ext("photo.png"));
    assert!(!is_active_content_ext("photo.jpg"));
    assert!(!is_active_content_ext("report.pdf"));
    assert!(!is_active_content_ext("noext"));
}

#[test]
fn syschat_media_forces_download_for_svg_but_not_png() {
    let mut svg_resp = ([(header::CONTENT_TYPE, mime_of("x.svg"))], "body").into_response();
    force_download_if_active_content("x.svg", &mut svg_resp);
    assert_eq!(
        svg_resp
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok()),
        Some("attachment")
    );

    let mut png_resp = ([(header::CONTENT_TYPE, mime_of("x.png"))], "body").into_response();
    force_download_if_active_content("x.png", &mut png_resp);
    assert!(png_resp
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .is_none());
}

// --- End-to-end regression: drives the real handlers, not just the helpers
// they call. Proves the ticket's curl repro (spoofed Content-Type upload,
// then a direct fetch of whatever got stored) is dead through the actual
// `/api/profile/avatar` and `/api/chat/media/:file` request path.

struct NoopAudit;

#[async_trait::async_trait]
impl AuditPort for NoopAudit {
    async fn record(&self, _entry: AuditRecord) {}
    async fn recent(
        &self,
        _limit: usize,
    ) -> Result<Vec<AuditRecord>, coxagent_application::PortError> {
        Ok(Vec::new())
    }
}

/// Auth-disabled (open-mode) `AppState` backed by a scratch hub dir, so
/// `principal_name` resolves every request as `"operator"` without any RBAC
/// wiring — mirrors how these endpoints actually run when no accounts are
/// configured.
async fn test_app_state(hub_dir: &std::path::Path) -> AppState {
    build_state(
        Vec::new(),
        Arc::new(NoopAudit),
        HubExtras {
            hub_dir: Some(hub_dir.to_path_buf()),
            ..Default::default()
        },
    )
    .await
}

/// A one-field `multipart/form-data` POST body, the same shape a browser's
/// `<input type=file>` sends — `claimed_content_type` is attacker-controlled,
/// exactly like the ticket's `curl -F "file=@evil.svg;type=image/svg"`.
fn multipart_avatar_request(
    filename: &str,
    claimed_content_type: &str,
    bytes: &[u8],
) -> Request<axum::body::Body> {
    const BOUNDARY: &str = "coxb059testboundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
             Content-Type: {claimed_content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    Request::builder()
        .method("POST")
        .uri("/api/profile/avatar")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(axum::body::Body::from(body))
        .expect("well-formed multipart request")
}

/// The ticket's exact repro, through the live endpoint: an SVG document
/// claiming `Content-Type: image/svg` must be refused. Before the fix (which
/// trusted a client-supplied `image/*`-prefixed Content-Type) this same
/// request stored the payload and answered 200 with a URL — this test would
/// fail without `sniff_avatar_image` gating the upload.
#[tokio::test]
async fn profile_avatar_ep_rejects_a_content_type_spoofed_svg_upload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = test_app_state(tmp.path()).await;
    let multipart = axum::extract::Multipart::from_request(
        multipart_avatar_request("evil.svg", "image/svg", SVG_BODY),
        &(),
    )
    .await
    .expect("well-formed multipart body parses");

    let resp = profile_avatar_ep(State(app), axum::http::HeaderMap::new(), multipart).await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Control for the test above: a genuine PNG upload is still accepted, so
/// the rejection is the byte sniff at work — not a broken test harness that
/// would reject everything.
#[tokio::test]
async fn profile_avatar_ep_accepts_a_genuine_png_upload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = test_app_state(tmp.path()).await;
    let multipart = axum::extract::Multipart::from_request(
        multipart_avatar_request("photo.png", "image/png", PNG_MAGIC),
        &(),
    )
    .await
    .expect("well-formed multipart body parses");

    let resp = profile_avatar_ep(State(app), axum::http::HeaderMap::new(), multipart).await;

    assert_eq!(resp.status(), StatusCode::OK);
}

/// The other half of the ticket's repro: fetching a stored `.svg` back
/// through the real `/api/chat/media/:file` handler must force a download
/// rather than let a direct navigation render (and execute) it. Seeds the
/// file straight through `StoragePort`, since the upload gate above already
/// covers avatars — this proves the serving side is safe independent of how
/// the file got there (defense in depth for any other/legacy upload path).
#[tokio::test]
async fn syschat_media_ep_forces_download_for_a_stored_svg() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let app = test_app_state(tmp.path()).await;
    app.storage
        .put("chat/evil.svg", SVG_BODY, "image/svg+xml")
        .await
        .expect("seed a stored file");

    let resp = syschat_media_ep(State(app), Path("evil.svg".to_owned())).await;

    assert_eq!(
        resp.headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok()),
        Some("attachment"),
        "a browser opening this URL directly must download, not execute, the SVG"
    );
}
