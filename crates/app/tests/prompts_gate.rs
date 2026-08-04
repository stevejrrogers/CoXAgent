//! CXA-F001 — Editable Prompt System with Per-Project Override.
//!
//! `coxagent init` must scaffold the embedded default prompts into a
//! project-local `prompts/` directory, and the engine must resolve each role's
//! system prompt as `local prompts/<role>.md` → embedded fallback, so a project
//! that never touches `prompts/` behaves exactly as before (zero regression).
//! The dashboard Settings screen edits those files live and saves them to
//! `prompts/<role>.md`; onboarding prompts live in their own
//! `prompts/onboarding/` files and are loaded only during onboard; and a
//! guardian guarantees no agent role ever runs without a discoverable prompt
//! source.
//!
//! PLAN.md §3j defines the canonical layout:
//!
//! ```text
//! prompts/
//! ├── _base.md        # shared: evidence, no raw state edits, output format
//! ├── ba.md  po.md  sm.md  sa.md  pd.md  dev.md  test.md  docs.md
//! ├── onboard/        # po_interview.md, sa_archaeology.md
//! └── discussion.md   # thread rules: opinion → evidence → proposal
//! ```
//!
//! This gate is in the style of `health_gate.rs` / `hexagonal_gate.rs`: it
//! asserts on the production source, because a MISSING capability — a role
//! without a prompt source, an init that never scaffolds, a resolver the engine
//! never consults — breaks nothing the compiler or any runtime test can see.
//! Every claim below targets a seam, and the suite is RED where the seam is not
//! yet closed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_borrow,
    clippy::useless_format
)]

use std::path::{Path, PathBuf};

/// Onboarding prompts are separate files, loaded only during the onboard flow.
const PLAN_ONBOARD_FILES: &[&str] = &["onboard/po_interview.md", "onboard/sa_archaeology.md"];

/// Every file the `coxagent init` scaffold must materialise, exactly as the
/// plan lays it out.
const ALL_PLAN_FILES: &[&str] = &[
    "_base.md",
    "ba.md",
    "po.md",
    "sm.md",
    "sa.md",
    "pd.md",
    "dev.md",
    "test.md",
    "docs.md",
    "onboard/po_interview.md",
    "onboard/sa_archaeology.md",
    "discussion.md",
];

/// Embedding a role's system prompt must be impossible without a discoverable
/// source. These are the constants that must be covered by the default-file
/// manifest; any NEW role added to a `run_*` use case must extend this list
/// (and the manifest), or the gate fails.
const EMBEDDED_ROLE_CONSTANTS: &[&str] = &["PO", "SM", "BA", "SA", "PD", "DEV", "TEST", "DOCS"];

fn file(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let p: PathBuf = root.join(rel);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|_| panic!("expected to read {} for the gate", p.display()))
}

/// The application layer's embedded-prompts source.
fn prompts_source() -> String {
    file("../application/src/prompts.rs")
}

/// All sources under a directory tree, keyed by path relative to the given
/// root, in the same walk the other gates use.
fn sources_under(rel_root: &str) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_root);
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p: PathBuf = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned();
                if let Ok(text) = std::fs::read_to_string(&p) {
                    out.push((rel, text));
                }
            }
        }
    }
    out.sort();
    out
}

/// Concatenate every production source under a directory.
fn blobs_under(rel_root: &str) -> String {
    sources_under(rel_root)
        .into_iter()
        .map(|(_, t)| t)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The use-case layer — the `run_*` handlers that dispatch whole agents.
///
/// Whether they reach for the embedded constant or the local-override resolver
/// is exactly what AC2 must pin down.
fn use_case_blobs() -> String {
    blobs_under("../application/src/use_cases")
}

/// Extract every role brought up as a whole agent via `system_prompt(...)`.
///
/// Captures `prompts::ROLE` referenced inside a `system_prompt(` call in the
/// use-case layer and the presentation server. Mirrors `health_gate.rs`: we
/// scan text, not types, because a MISSING call site is exactly what we must
/// not rely on the compiler to notice.
fn agent_role_callsites() -> Vec<String> {
    let mut all = blobs_under("../application/src/use_cases");
    all.push_str(&blobs_under("../presentation/src/server"));

    let marker = "system_prompt(";
    let mut roles = Vec::new();
    let mut start = 0;
    while let Some(rel) = all[start..].find(marker) {
        let rel = start + rel + marker.len();
        let tail = &all[rel..];
        let after = tail.trim_start();
        if let Some(rest) = after.strip_prefix("prompts::") {
            // A role constant token: [A-Z_]+ but ignore composed partials that
            // are not whole-agent roles.
            let token: String = rest
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || *c == '_')
                .collect();
            if !token.is_empty() {
                roles.push(token);
            }
        }
        start = rel;
    }
    roles
}

// ---------------------------------------------------------------------------
// AC 1 — `coxagent init` scaffolds the full `prompts/` tree.
// ---------------------------------------------------------------------------

#[test]
fn coxagent_init_scaffolds_every_embedded_prompt_file() {
    let init_lib = file("../app/src/lib.rs");
    let onboard = file("../app/src/onboard.rs");
    let scaffold = format!("{init_lib}\n{onboard}");

    for f in ALL_PLAN_FILES {
        // The scaffold must write a file whose path reads `prompts/<f>`, either
        // as a literal in the production source or through a documented
        // template constant.
        let literal = format!("prompts/{f}");
        let plan_literal = literal
            .replace("prompts/", "\"prompts/\"")
            .replace(".md", ".md\"");
        assert!(
            scaffold.contains(&literal) || scaffold.contains(&plan_literal),
            "CXA-F001 AC1: `coxagent init` must scaffold project-local \
             `prompts/{f}` from the embedded default; no scaffold write for it \
             exists in {}/src/lib.rs or {}/src/onboard.rs",
            env!("CARGO_MANIFEST_DIR"),
            env!("CARGO_MANIFEST_DIR"),
        );
    }
}

// ---------------------------------------------------------------------------
// AC 2 — resolve local `prompts/<role>.md`, fall back to embedded, no-regression.
// ---------------------------------------------------------------------------

#[test]
fn engine_routes_every_agent_role_through_the_local_first_resolver() {
    let src = prompts_source();
    let use_cases = use_case_blobs();

    // The resolver must exist as a symbol that reads a `prompts/` path.
    assert!(
        src.contains("fn resolve_role_prompt") || src.contains("fn resolve_prompt"),
        "CXA-F001 AC2: the engine must resolve a role's system prompt as \
         local `prompts/<role>.md` first, then the embedded default. No \
         resolver function is defined in crates/application/src/prompts.rs."
    );
    assert!(
        src.contains("prompts/"),
        "CXA-F001 AC2: the resolver must read a project-local `prompts/` path \
         before falling back; no such IO seam exists."
    );

    // The engine must ACTUALLY use it. AC2 is about behaviour, not a dead
    // declaration: every whole-agent `run_*` use case must route its role
    // prompt through the local-first resolver, so a project-local
    // `prompts/dev.md` really changes what DEV receives. A resolv function
    // that nothing calls adds the file but a project never sees an override
    // (and the zero-regression fallback is just the embedded constant again).
    assert!(
        use_cases.contains("resolve_prompt"),
        "CXA-F001 AC2: `resolve_prompt` exists but the engine never consults it. \
         Every `run_*` use case still builds its role prompt from the embedded \
         constant (system_prompt(prompts::ROLE)) and ignores the project-local \
         `prompts/<role>.md` override. Wire the local-first resolver into the \
         agent dispatch layer (run_sa, run_pd, run_ba, run_test, run_docs, \
         run_discussion, run_chat_reply, run_reviews, ...) so an override \
         takes effect."
    );
}

// ---------------------------------------------------------------------------
// AC 3 — dashboard Settings edits and saves a prompt to `prompts/<role>.md`.
// ---------------------------------------------------------------------------

#[test]
fn dashboard_settings_edits_and_saves_prompts_to_file() {
    let server = blobs_under("../presentation/src/server");
    let web_js = blobs_under_js("../presentation/src/web/js");
    let web_html = file("../presentation/src/web/index.html");
    let web = format!("{web_js}\n{web_html}");

    // The server must persist an edited prompt to a project-local
    // `prompts/<role>.md` — the plan's "có file thì override".
    assert!(
        server.contains("prompts/"),
        "CXA-F001 AC3: the dashboard Settings screen must save edits to a \
         project-local `prompts/<role>.md`; no server handler writes a \
         `prompts/` path yet."
    );

    // The Settings view must expose a live prompt editor that saves the file.
    assert!(
        web.contains("prompts/") || web.contains("savePrompt") || web.contains("promptEditor"),
        "CXA-F001 AC3: the browser Settings view must render a live prompt editor \
         that saves `prompts/<role>.md`; no such editor exists in the web shell."
    );
}

// ---------------------------------------------------------------------------
// AC 4 — onboarding prompts are separate files, loaded only during onboard.
// ---------------------------------------------------------------------------

#[test]
fn onboarding_prompts_are_separate_files_loaded_only_during_onboard() {
    let src = prompts_source();

    // The embedded defaults must ship the two onboarding prompts as distinct
    // files under `onboard/`, separate from the standard per-role sections.
    for f in PLAN_ONBOARD_FILES {
        let literal = format!("{f}");
        let base = f.rsplit('/').next().unwrap();
        assert!(
            src.contains(&literal) || src.contains(base),
            "CXA-F001 AC4: the embedded defaults must include onboarding prompt \
             `{f}`, split into `prompts/onboarding/`; it is not embedded."
        );
    }

    // These onboarding prompts are loaded ONLY by the onboard flow. If any
    // standard-cycle `system_prompt(...)` role call overlaps an onboarding
    // prompt name, the split is not real — onboarding content would leak into
    // ordinary agent cycles.
    let onboard_bases: Vec<&str> = PLAN_ONBOARD_FILES
        .iter()
        .map(|f| f.rsplit('/').next().unwrap())
        .collect();
    for role in agent_role_callsites() {
        assert!(
            !onboard_bases.contains(&role.as_str()),
            "CXA-F001 AC4: onboarding prompt `{role}` must not be brought up as a \
             standard-cycle `system_prompt` role; it may only load during onboard \
             (crates/app/src/onboard.rs)."
        );
    }
}

// ---------------------------------------------------------------------------
// AC 5 — every agent role has a discoverable prompt source (gate).
// ---------------------------------------------------------------------------

#[test]
fn every_agent_role_has_a_discoverable_prompt_source() {
    let src = prompts_source();

    // The embedded defaults must register a manifest of the per-role prompt
    // files so the engine can discover a source for every role.
    let has_manifest = src.contains("PROMPT_DEFAULT_FILES")
        || src.contains("DEFAULT_PROMPT_FILES")
        || src.contains("PROMPT_FILES");
    assert!(
        has_manifest,
        "CXA-F001 AC5: the embedded defaults must declare a manifest of prompt \
         source files (`ba.md`, `po.md`, ...); no such manifest exists, so no \
         role can be proven to have a discoverable prompt source."
    );

    // Every whole-agent role constant must map to a `<role>.md` entry in that
    // manifest. Adding a NEW role to a run_* use case without giving it a
    // prompt source — and listing it here and in the manifest — is exactly the
    // gap this gate exists to close, across `run_sa`, `run_pd`, `run_ba`,
    // `run_test`, `run_docs`, etc.
    for role in EMBEDDED_ROLE_CONSTANTS {
        let role_file = format!("{}.md", role.to_ascii_lowercase());
        assert!(
            src.contains(&role_file),
            "CXA-F001 AC5: role `{role}` is brought up as an agent but has no \
             discoverable prompt source: no `{role_file}` appears in the embedded \
             prompt manifest."
        );
    }
}

// Internal helpers -----------------------------------------------------------

fn blobs_under_js(rel_root: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_root);
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let p: PathBuf = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "js") {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    out.push(text);
                }
            }
        }
    }
    out.join("\n")
}
