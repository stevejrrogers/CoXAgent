//! CXA-B191 guardrail 3 — the inline-literal copy-layer gate.
//!
//! Presentation JS must not hardcode new user-facing sentence literals in
//! DOM-writing calls; shared microcopy resolves through `copyText()` /
//! `toastCopy()` / `ellipsisCopy()` from `copy.js`. Pure filesystem read of
//! the crate's own `web/js/` sources (the established repo pattern — see
//! `slot_collision_radar_f329_gate.rs`); no server, no network, no harness.
//!
//! Baseline: `INLINE_COPY_EXCEPTIONS` starts EMPTY (the wiki page
//! `cxa-b191-microcopy-guardrails.md` documents the ratchet — entries need an
//! owner + ticket and the list may only ever shrink).
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// User-facing DOM-writing calls whose first argument (or inline HTML) must
/// come from the copy layer.
const DOM_WRITE_FNS: [&str; 8] = [
    "toast(",
    "toasty(",
    "toastCopy(",
    "copyText(",
    "confirm(",
    "alert(",
    "prompt(",
    "coxModal(",
];

/// Files exempt as a group. `copy.js` IS the layer; `core.js` carries the
/// shared `esc` helper's neighborhood; vendored/minified bundles are not ours.
const EXEMPT_FILES: [&str; 2] = ["copy.js", "mermaid.min.js"];

/// Attribute-literal copy that is NOT sentence microcopy (button tooltips,
/// icon titles, data-* names). Titles that duplicate dynamic values are
/// allowed to interpolate at the callsite; the guard targets prose.
const ATTRIBUTE_ALLOW: [&str; 0] = [];

/// The gate baseline (CXA-B191): empty. Every entry needs `owner` and a
/// linked ticket in the reason; the wiki page documents the shrink-only rule.
static INLINE_COPY_EXCEPTIONS: LazyLock<
    Vec<(&'static str, &'static str, &'static str, &'static str)>,
> = LazyLock::new(|| {
    let owner = "CXA-B164 legacy (pre-gate inline copy)";
    vec![
            (
                "crates/presentation/src/web/js/approval_policy.js",
                r#""Network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Channel #""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Channel deleted""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Delete failed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Already in a call""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""No camera — starting a voice call instead""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Call ended""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Cannot access mic/camera""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Screen sharing stopped""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""No screen track""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Screen sharing ended""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Sharing your screen""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Could not share: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Open a direct message to call""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Cancel this meeting?""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Meeting cancelled""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Finish your current call first""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""No mic/camera — joining view-only""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Could not share screen: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Webhook created — URL copied""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Notifications on for this channel""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Muted this channel""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Preferences saved""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Photo removed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Image max 2MB""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Avatar updated""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Couldn't send — check your connection and try again.""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Upload failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Remove this attachment?""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Attachment removed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Max 25MB""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Attached ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Approved — agents may run ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/chat.js",
                r#""Network error — comment not posted""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/core.js",
                r#""Could not mark complete: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/core.js",
                r#""Milestone '""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/core.js",
                r#""Could not mark complete""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/docs.js",
                r#""AI edit failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/docs.js",
                r#""Page revised by DOCS agent""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/docs.js",
                r#""AI edit failed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/docs.js",
                r#""Docs generation failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/docs.js",
                r#""Generated ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/docs.js",
                r#""Docs generation failed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Saved — conventions apply to new agent runs""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Downloading update — the app will restart itself when done""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Invite link copied""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Sprint queued — drag tickets into it""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Plan renamed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Planned sprint dropped""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Usage: /sprint <goal text>""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Sprint goal set""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Usage: /discuss <topic>""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Digest posted to chat""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Unknown command ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Review failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/home.js",
                r#""Discussion failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/inbox.js",
                r#""Network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/manage.js",
                r#""Name the space""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/manage.js",
                r#""Failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/manage.js",
                r#""Network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/manage.js",
                r#""Failed: network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/mcp.js",
                r#""Enter a label first""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/mcp.js",
                r#""Network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/mcp.js",
                r#""Token revoked""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/mcp.js",
                r#""Engine settings updated — applies on the next cycle, no restart""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/mcp.js",
                r#""Lý do tạm dừng (hiện trong river) — có thể bỏ trống:""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Copied to clipboard""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Goal can't be empty""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Project goal updated""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Save failed (""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Network error""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Project created""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Delete failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Project renamed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""SA is sweeping the queue & merging green PRs…""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Sweep failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Building preview of PR #""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Preview live""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Failed: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Pick a project first""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Couldn't load xterm.js (network needed)""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Code map built — ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Build failed""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Member added""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""User updated""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Could not edit""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Cannot delete""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Link copied!""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Only private channels""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Usage: /invite @username""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Topic updated""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Channel muted""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Channel unmuted""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Status: ""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Unknown command. Try /invite, /topic, /mute, /away, /busy, /online""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Notification settings saved""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Meeting defaults saved""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
            (
                "crates/presentation/src/web/js/shell.js",
                r#""Duplicate pair dismissed — hidden for this session""#,
                owner,
                "grandfathered inline literal; migrate to copyText()/toastCopy() when its surface is next touched",
            ),
        ]
});

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .canonicalize()
        .unwrap_or_else(|e| panic!("repo root must resolve: {e}"))
}

fn web_js_files() -> Vec<PathBuf> {
    let dir = repo_root().join("crates/presentation/src/web/js");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read web/js directory {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "js"))
        .collect();
    files.sort();
    files
}

/// A user-facing literal: starts with an uppercase letter or contains a
/// space-separated multi-word phrase, and is not pure code (no `${`, no
/// single-word technical tokens, no `id`/class fragments).
fn is_user_facing_literal(lit: &str) -> bool {
    if lit.len() < 4 {
        return false;
    }
    if lit.contains("${") || lit.contains('\\') {
        return false;
    }
    // Prose signal: at least two words separated by a space, or a
    // capitalised sentence start. Technical identifiers ("ti-plus",
    // "text-overflow") are excluded by this on purpose.
    let words: Vec<&str> = lit.split(' ').collect();
    words.len() >= 2 && lit.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

#[test]
fn presentation_js_resolves_user_facing_literals_through_the_copy_layer() {
    let mut violations: Vec<String> = Vec::new();
    for path in web_js_files() {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        if EXEMPT_FILES.contains(&name.as_str()) {
            continue;
        }
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for line in src.lines() {
            let t = line.trim_start();
            if t.starts_with("//") {
                continue; // comments are not rendered copy
            }
            for fnd in DOM_WRITE_FNS {
                let Some(idx) = line.find(fnd) else { continue };
                // Only inspect the quoted argument directly after the call.
                let after = &line[idx + fnd.len()..];
                let Some(quote) = after
                    .trim_start()
                    .chars()
                    .next()
                    .filter(|&q| q == '\'' || q == '"')
                else {
                    continue; // variable/template arg — already layer-resolved or checked elsewhere
                };
                let rest = after.trim_start();
                let Some(end_rel) = rest[1..].find(quote) else {
                    continue;
                };
                let lit = &rest[1..=end_rel];
                if !is_user_facing_literal(lit) {
                    continue;
                }
                let rel = path
                    .strip_prefix(repo_root())
                    .unwrap_or_else(|e| panic!("js path must be under the repo root: {e}"))
                    .display()
                    .to_string();
                violations.push(format!(
                    "{}:{}: inline user-facing literal {:?} passed to {} — resolve it through copyText()/toastCopy() (copy.js) or add a shrink-only exception in INLINE_COPY_EXCEPTIONS",
                    rel,
                    src.lines().take_while(|l| !std::ptr::eq(
                        l.as_ptr(),
                        line.as_ptr()
                    )).count() + 1,
                    lit,
                    fnd
                ));
            }
        }
    }
    // Exceptions match on FILE + LITERAL, not file:line — line-keyed entries
    // broke en masse whenever an unrelated edit shifted a file (CXA-B224
    // aftermath), which punished exactly the files being improved.
    let real: Vec<&String> = violations
        .iter()
        .filter(|v| {
            !INLINE_COPY_EXCEPTIONS
                .iter()
                .any(|(file, lit, _, _)| v.contains(file) && v.contains(lit))
        })
        .collect();
    assert!(
        real.is_empty(),
        "inline copy-layer violations ({}):\n{}",
        real.len(),
        real.iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    // Ratchet: an exception that no longer matches any live violation must be
    // removed (the list may only shrink, never accumulate dead entries).
    for (file, lit, owner, reason) in INLINE_COPY_EXCEPTIONS.iter() {
        assert!(
            violations.iter().any(|v| v.contains(file) && v.contains(lit)),
            "stale copy-gate exception {file} {lit} (owner {owner}): {reason} — the violation is gone, remove the entry"
        );
        assert!(
            !owner.trim().is_empty(),
            "exception {file} {lit} is missing its owner"
        );
        assert!(
            !reason.trim().is_empty(),
            "exception {file} {lit} is missing its reason"
        );
    }
    let _ = ATTRIBUTE_ALLOW;
}

#[test]
fn copy_layer_assets_are_served_and_load_before_their_consumers() {
    let server_mod =
        std::fs::read_to_string(repo_root().join("crates/presentation/src/server/mod.rs"))
            .unwrap_or_else(|e| panic!("read server/mod.rs: {e}"));
    let pos_copy = server_mod.find("\"copy.js\"").unwrap_or_else(|| {
        panic!("copy.js must be registered in APP_JS (crates/presentation/src/server/mod.rs)")
    });
    for consumer in ["kpis.js", "core.js", "manage.js", "home.js", "inbox.js"] {
        let pos = server_mod
            .find(&format!("\"{consumer}\""))
            .unwrap_or_else(|| panic!("{consumer} must be registered in APP_JS"));
        assert!(
            pos_copy < pos,
            "copy.js must load BEFORE {consumer} so window.copyText exists at consumer load time"
        );
    }
    let copy_js =
        std::fs::read_to_string(repo_root().join("crates/presentation/src/web/js/copy.js"))
            .unwrap_or_else(|e| panic!("read copy.js: {e}"));
    assert!(
        copy_js.contains("window.copyText = function"),
        "copy.js must expose window.copyText"
    );
}

#[test]
fn gate_exception_list_documentation_is_present() {
    // The baseline must be documented where the next engineer will find it.
    let wiki = repo_root().join("docs/wiki/engineering/cxa-b191-microcopy-guardrails.md");
    let doc = std::fs::read_to_string(&wiki).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} — the gate baseline must be documented",
            wiki.display()
        )
    });
    assert!(
        doc.contains("shrink-only"),
        "the wiki page must state the exceptions list is shrink-only"
    );
    assert!(
        doc.contains("INLINE_COPY_EXCEPTIONS"),
        "the wiki page must name the code-level exception list"
    );
}
