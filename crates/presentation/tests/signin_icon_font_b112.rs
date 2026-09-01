//! CXA-B112 regression — the primary "Sign in" button rendered without its
//! icon in deployed containers (text-only CTA).
//!
//! The root cause was a chain, and every link is pinned here on the bytes the
//! hub actually serves — the same no-harness discipline as
//! `dashboard_file_pickers.rs` and `preflight_f239_tdd.rs`: no server, no
//! port, no invented types.
//!
//!   1. `index.html` loaded the Tabler icon webfont from `cdn.jsdelivr.net`.
//!   2. Deployed containers have no CDN egress (verified from inside the
//!      deployed workspace container), so that stylesheet never arrived.
//!   3. Tabler icon characters exist only as CSS `content:` rules — with the
//!      stylesheet gone, NO `.ti-*` rule exists, and a glyph with no rule and
//!      no text fallback paints zero ink. The sign-in CTA's `ti-login` glyph
//!      was the visible casualty; the logo tile next to it is plain local CSS
//!      and still painted, which is what fooled visual QA into thinking the
//!      webfont had loaded.
//!
//! The fix vendors the font like Mermaid and xterm. Each test below fails on
//! the pre-fix tree and passes on the fixed one.

/// The dashboard shell the router serves verbatim — where the icon stylesheet
/// is referenced.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The hub's router source — where every `/assets/...` route is registered
/// and the vendored assets are embedded.
const ROUTER: &str = include_str!("../src/server/mod.rs");

/// The vendored Tabler stylesheet the router serves at
/// `/assets/tabler-icons.min.css` (pinned upstream v3.24.0, MIT).
const TABLER_CSS: &str = include_str!("../src/web/tabler-icons.min.css");

/// The vendored woff2 the stylesheet resolves, served at
/// `/assets/fonts/tabler-icons.woff2`.
const TABLER_WOFF2: &[u8] = include_bytes!("../src/web/fonts/tabler-icons.woff2");

/// AC1: the dashboard's own UI must never depend on a third-party CDN — that
/// dependency is exactly what erased every icon in air-gapped deploys. The
/// stylesheet must come from same-origin `/assets/`, and the CSP must not
/// re-allowlist the CDN (defense in depth: a reintroduced CDN link would be
/// blocked, not silently half-working).
#[test]
fn dashboard_never_loads_its_icon_font_from_a_cdn() {
    assert!(
        !INDEX_HTML.contains("cdn.jsdelivr.net"),
        "index.html must not reference cdn.jsdelivr.net — deployed containers \
         cannot reach it, and every icon glyph silently vanished (CXA-B112)"
    );
    assert!(
        INDEX_HTML.contains("href=\"/assets/tabler-icons.min.css\""),
        "index.html must load the icon stylesheet from the vendored \
         same-origin asset"
    );
    assert!(
        !ROUTER.contains("cdn.jsdelivr.net"),
        "the dashboard CSP must not allowlist cdn.jsdelivr.net for \
         style/font any more — nothing loads from it"
    );
}

/// AC2: the local chain must actually be wired — the router serves the
/// vendored stylesheet and the woff2 it resolves, and the font is EMBEDDED in
/// the binary (include_bytes!), not a runtime file dependency that would
/// break the single-binary deploy shape.
#[test]
fn router_serves_the_vendored_stylesheet_and_font() {
    assert!(
        ROUTER.contains("\"/assets/tabler-icons.min.css\""),
        "the router must serve the vendored Tabler stylesheet"
    );
    assert!(
        ROUTER.contains("\"/assets/fonts/tabler-icons.woff2\""),
        "the router must serve the vendored Tabler woff2"
    );
    assert!(
        ROUTER.contains("include_bytes!(\"../web/fonts/tabler-icons.woff2\")"),
        "the font must be embedded via include_bytes! like the other \
         vendored assets (mermaid/xterm), not read from disk at runtime"
    );
}

/// AC3: the served stylesheet must carry the exact glyph the sign-in button
/// needs — `ti-login` — and resolve its font from the locally served path.
/// This is the assertion that dies if the vendored copy is ever replaced by a
/// subset missing the CTA's icon.
#[test]
fn vendored_stylesheet_declares_the_sign_in_glyph() {
    assert!(
        TABLER_CSS.contains(".ti-login:before{content:\"\\eba7\"}"),
        "the vendored stylesheet must declare the ti-login glyph the \
         sign-in CTA renders"
    );
    assert!(
        TABLER_CSS.contains("url(\"./fonts/tabler-icons.woff2"),
        "the stylesheet must resolve its font from the locally served \
         relative path, not an absolute CDN URL"
    );
    assert!(
        TABLER_CSS.contains(".ti{font-family:\"tabler-icons\""),
        "the stylesheet must hook the .ti class onto the tabler-icons family"
    );
}

/// AC4: the embedded font must be a real woff2 binary of the full icon set —
/// a truncated download or an accidentally committed placeholder would
/// re-create the same zero-ink symptom with all the wiring above in place.
#[test]
fn embedded_font_is_a_real_woff2_binary() {
    assert_eq!(
        TABLER_WOFF2.get(..4),
        Some(b"wOF2".as_slice()),
        "the embedded font must carry the woff2 magic bytes"
    );
    assert!(
        TABLER_WOFF2.len() > 100_000,
        "the embedded font must be the full icon set, not a stub \
         (got {} bytes)",
        TABLER_WOFF2.len()
    );
}
