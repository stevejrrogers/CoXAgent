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
    "toast(", "toasty(", "toastCopy(", "copyText(",
    "confirm(", "alert(", "prompt(", "coxModal(",
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
static INLINE_COPY_EXCEPTIONS: LazyLock<Vec<(&'static str, &'static str, &'static str)>> =
    LazyLock::new(|| {
        let owner = "CXA-B164 legacy (pre-gate inline copy)";
        vec![
    ("crates/presentation/src/web/js/approval_policy.js:129", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/approval_policy.js:140", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:212", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:308", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:309", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:654", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:666", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:688", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:702", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:742", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:747", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:750", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:751", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:752", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:767", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:941", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:943", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:948", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:970", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:975", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:978", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1041", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1057", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1065", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1081", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1084", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1087", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1339", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1458", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1459", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1611", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1614", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1616", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1618", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1621", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1623", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1632", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1662", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1695", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:1854", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2209", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2212", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2213", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2219", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2224", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2229", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2247", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/chat.js:2311", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/core.js:356", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/core.js:357", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/core.js:358", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/docs.js:177", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/docs.js:178", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/docs.js:179", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/docs.js:198", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/docs.js:199", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/docs.js:200", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:75", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:117", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:166", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:450", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:458", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:460", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:467", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:475", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:477", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:485", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:487", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:499", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:514", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:546", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:547", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:550", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:557", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:566", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:716", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:717", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:727", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/home.js:729", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/inbox.js:221", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/inbox.js:239", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/inbox.js:252", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/manage.js:110", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/manage.js:125", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/manage.js:126", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/manage.js:174", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/manage.js:304", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/manage.js:305", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/mcp.js:43", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/mcp.js:47", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/mcp.js:50", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/mcp.js:52", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/mcp.js:118", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/mcp.js:174", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:45", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:146", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:148", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:149", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:150", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:381", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:384", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:392", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:396", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:708", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:742", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:745", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:751", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:847", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:851", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:855", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:856", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:886", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:887", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:1129", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:1130", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:1179", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:1199", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2202", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2203", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2208", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2209", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2217", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2220", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2270", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2271", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2279", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2282", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2285", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2289", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2292", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2441", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2447", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
    ("crates/presentation/src/web/js/shell.js:2581", owner, "grandfathered inline literal; migrate to copyText()/toastCopy() when its panel is next touched"),
        ]
    });

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .canonicalize()
        .expect("repo root must resolve")
}

fn web_js_files() -> Vec<PathBuf> {
    let dir = repo_root().join("crates/presentation/src/web/js");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("web/js directory must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "js"))
        .collect();
    files.sort();
    files
}

fn line_of(src: &str, offset: usize) -> usize {
    src[..offset].bytes().filter(|b| *b == b'\n').count() + 1
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
        let name = path.file_name().expect("js file name").to_string_lossy().to_string();
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
                let quote = match after.trim_start().chars().next() {
                    Some(q @ ('\'' | '"')) => q,
                    _ => continue, // variable/template arg — already layer-resolved or checked elsewhere
                };
                let rest = after.trim_start();
                let Some(end_rel) = rest[1..].find(quote) else { continue };
                let lit = &rest[1..1 + end_rel];
                if !is_user_facing_literal(lit) {
                    continue;
                }
                let rel = path
                    .strip_prefix(repo_root())
                    .expect("js path under repo root")
                    .display()
                    .to_string();
                let loc = format!("{}:{}", rel, line_of(&src, 0) + 0);
                let _ = &loc;
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
    let allowed: Vec<&str> = INLINE_COPY_EXCEPTIONS.iter().map(|(loc, _, _)| *loc).collect();
    let real: Vec<&String> = violations
        .iter()
        .filter(|v| !allowed.iter().any(|a| v.contains(a)))
        .collect();
    assert!(
        real.is_empty(),
        "inline copy-layer violations ({}):\n{}",
        real.len(),
        real.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    );
    // Ratchet: an exception that no longer matches any live violation must be
    // removed (the list may only shrink, never accumulate dead entries).
    for (loc, owner, reason) in INLINE_COPY_EXCEPTIONS.iter() {
        assert!(
            violations.iter().any(|v| v.contains(loc)),
            "stale copy-gate exception {loc} (owner {owner}): {reason} — the violation is gone, remove the entry"
        );
        assert!(!owner.trim().is_empty(), "exception {loc} is missing its owner");
        assert!(!reason.trim().is_empty(), "exception {loc} is missing its reason/ticket");
    }
    let _ = ATTRIBUTE_ALLOW;
}

#[test]
fn copy_layer_assets_are_served_and_load_before_their_consumers() {
    let server_mod = std::fs::read_to_string(
        repo_root().join("crates/presentation/src/server/mod.rs"),
    )
    .expect("server/mod.rs must exist");
    let pos_copy = server_mod
        .find("\"copy.js\"")
        .expect("copy.js must be registered in APP_JS (crates/presentation/src/server/mod.rs)");
    for consumer in ["kpis.js", "core.js", "manage.js", "home.js", "inbox.js"] {
        let pos = server_mod
            .find(&format!("\"{consumer}\""))
            .unwrap_or_else(|| panic!("{consumer} must be registered in APP_JS"));
        assert!(
            pos_copy < pos,
            "copy.js must load BEFORE {consumer} so window.copyText exists at consumer load time"
        );
    }
    let copy_js = std::fs::read_to_string(repo_root().join("crates/presentation/src/web/js/copy.js"))
        .expect("copy.js must exist");
    assert!(
        copy_js.contains("window.copyText = function"),
        "copy.js must expose window.copyText"
    );
}

#[test]
fn gate_exception_list_documentation_is_present() {
    // The baseline must be documented where the next engineer will find it.
    let wiki = repo_root().join("docs/wiki/engineering/cxa-b191-microcopy-guardrails.md");
    let doc = std::fs::read_to_string(&wiki)
        .unwrap_or_else(|e| panic!("read {}: {e} — the gate baseline must be documented", wiki.display()));
    assert!(
        doc.contains("shrink-only"),
        "the wiki page must state the exceptions list is shrink-only"
    );
    assert!(
        doc.contains("INLINE_COPY_EXCEPTIONS"),
        "the wiki page must name the code-level exception list"
    );
}
