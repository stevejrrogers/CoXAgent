//! CXA-F001 AC2 — the engine must resolve a role's system prompt as
//! `local prompts/<role>.md` → embedded fallback, so a per-project override
//! really changes what EVERY agent run receives.
//!
//! The embedded resolver (`crates/application/src/prompts.rs::resolve_prompt`)
//! and the `run_*` use cases that opt in are not enough: AC2 governs every
//! whole-agent run. Any agent spawned with its `system_prompt` built straight
//! from an embedded constant (`system_prompt: ...::system_prompt(prompts::ROLE)`)
//! ignores a project-local `prompts/<role>.md` override — the feature silently
//! does nothing for that run, while looking enabled everywhere else.
//!
//! Unlike `prompts_gate.rs` (which checks the resolver exists and that SOME use
//! case calls it), this gate asserts the OVERRIDE actually WINS at every one of
//! the dozens of agent spawn points across the use-case and presentation tower.
//! It is RED precisely where a spawn point still hardcodes the embedded
//! constant instead of routing through `resolve_prompt`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::manual_strip)]

use std::path::{Path, PathBuf};

/// Concatenate every Rust source under a directory tree (the same walk the
/// other gates use), keyed by path relative to the root.
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

/// Find every whole-agent spawn whose `system_prompt` is hardcoded to an
/// embedded constant — i.e. a `system_prompt:` field whose initializer is
/// `<...>::system_prompt(...)` directly, rather than `resolve_prompt(...)`.
///
/// Returns one entry per violation: the file and the role constant or a short
/// snippet, so a failing assertion points straight at the seam to close.
fn hardcoded_system_prompt_sites() -> Vec<(String, String)> {
    let mut sites = Vec::new();
    for (rel_root, display) in [
        ("../application/src/use_cases", "use_cases/"),
        ("../presentation/src/server", "presentation/"),
    ] {
        for (rel, text) in sources_under(rel_root) {
            let mut idx = 0;
            while let Some(rel_i) = text[idx..].find("system_prompt:") {
                let at = idx + rel_i + "system_prompt:".len();
                // Skip the prompt function *definitions* in prompts.rs-like
                // role prompts.rs is under application/src (not under
                // use_cases/ or presentation), so definitions never appear
                // here; only AgentRequest field assignments do.
                let tail = &text[at..];
                // Walk past whitespace / `&` / optional newline to the call's
                // callee identifier.
                let mut look = at;
                for ch in tail.chars() {
                    if ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n' || ch == '&' {
                        look += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                let after = &text[look..];
                // The callee of the `system_prompt:` initializer may be fully
                // qualified: `resolve_prompt(...)`, `crate::prompts::system_prompt(...)`,
                // `prompts::resolve_prompt(...)`, etc. Trim the module path to
                // the final callee identifier before deciding.
                let mut callee = after;
                for sep in [
                    "crate::prompts::",
                    "prompts::",
                    "coxagent_application::prompts::",
                ] {
                    if let Some(rest) = callee.strip_prefix(sep) {
                        callee = rest;
                        break;
                    }
                }
                // Good: routed through the local-first resolver.
                if callee.starts_with("resolve_prompt") {
                    idx = look;
                    continue;
                }
                // Bypass: hardcoded embedded constant.
                if callee.starts_with("system_prompt") {
                    let const_tail = &callee["system_prompt".len()..];
                    let open = const_tail.find('(').expect("system_prompt(");
                    let args = &const_tail[open + 1..];
                    let role = args
                        .trim_start()
                        .trim_start_matches("crate::prompts::")
                        .trim_start_matches("prompts::")
                        .trim_start_matches("coxagent_application::prompts::")
                        .chars()
                        .take_while(|c| c.is_ascii_uppercase() || *c == '_')
                        .collect::<String>();
                    sites.push((format!("{display}{rel}"), role));
                    idx = look;
                    continue;
                }
                // Not a call we recognise — keep scanning forward.
                idx = at;
            }
        }
    }
    sites
}

#[test]
fn every_agent_spawn_routes_its_system_prompt_through_the_local_first_resolver() {
    let bypasses = hardcoded_system_prompt_sites();

    assert!(
        bypasses.is_empty(),
        "CXA-F001 AC2: {} whole-agent spawn(s) build their system prompt from a \
         hardcoded embedded constant and IGNORE a project-local `prompts/<role>.md` \
         override. A workspace that edits `prompts/dev.md` / `prompts/sa.md` / ... \
         would see no effect on these runs — the per-project override is only \
         partial. Route each through `resolve_prompt(..., &system_prompt(embedded))`:\n{}",
        bypasses.len(),
        bypasses
            .iter()
            .map(|(f, r)| format!("  - {f}  (role {r})"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}
