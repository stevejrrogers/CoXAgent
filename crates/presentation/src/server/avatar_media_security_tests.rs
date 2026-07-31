// Split from server/mod.rs — avatar/media upload hardening tests.
#![allow(clippy::wildcard_imports)]
use super::*;

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
