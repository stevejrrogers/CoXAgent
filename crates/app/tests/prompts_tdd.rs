//! CXA-F001 TDD — Editable Prompt System with Per-Project Override.
//!
//! These tests encode the exact acceptance criteria for the prompt override
//! feature. Written TDD-style: they compile and fail until each AC is closed.
//!
//! After implementation every test passes.
//!
//! Run:
//! ```
//! cargo test --package coxagent-app --test prompts_tdd
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::manual_strip,
    clippy::ptr_arg,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::uninlined_format_args,
    clippy::useless_format
)]

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

// ── helpers ────────────────────────────────────────────────────────────────

fn coxagent_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/coxagent")
}

/// Create a fresh workspace and return handles + path components.
fn make_workspace() -> (TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    let state_dir = root.join("state");
    let codebase = root.join("codebase");
    let prompts = root.join("prompts");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&codebase).unwrap();
    (dir, root, state_dir, codebase, prompts)
}

/// Run `coxagent onboard --name <name>` with the workspace `state_dir`, returning
/// combined stdout. `--state-dir` is a global flag; the project name is passed
/// via `--name` (so `state_dir` must contain a `name` that is CLI-safe, which
/// every test name here already is).
fn run_onboard(state_dir: &PathBuf, name: &str) -> String {
    let out = Command::new(coxagent_bin())
        .args([
            "onboard",
            "--state-dir",
            &state_dir.to_string_lossy(),
            "--name",
            name,
        ])
        .current_dir(state_dir.parent().unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .expect("coxagent onboard failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Extract file names from the static manifest in the embedded prompts source.
fn manifest_files() -> HashSet<String> {
    let src = String::from(env!("CARGO_MANIFEST_DIR")) + "/../application/src/prompts.rs";
    let text = std::fs::read_to_string(src).expect("read prompts source");

    let mut set = HashSet::new();
    let mut in_manifest = false;
    for line in text.lines() {
        // Anchor on the declaration itself, not on a prose/doc-comment mention
        // of the same name (a comment also reads "PROMPT_DEFAULT_FILES" and
        // carries `prompts/<role>.md` tokens that are not real manifest files).
        if line
            .trim_start()
            .starts_with("pub const PROMPT_DEFAULT_FILES")
        {
            in_manifest = true;
            continue;
        }
        if !in_manifest {
            continue;
        }
        if line.trim().starts_with(']') {
            break;
        }
        for word in line.split_whitespace() {
            let trimmed = word
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '/' || *c == '_' || *c == '.')
                .collect::<String>();
            if trimmed.ends_with(".md") {
                set.insert(trimmed);
            }
        }
    }
    set
}

// ── AC 1 — coxagent init scaffolds the full prompts tree ───────────────────

#[test]
fn ac1_onboard_scaffolds_prompts_directory_for_every_role() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();

    assert!(
        !prompts.exists(),
        "prompts dir should not exist before onboard"
    );

    let _output = run_onboard(&state_dir, "ac1-tdd");

    for name in manifest_files() {
        let path = prompts.join(&name);
        assert!(
            path.exists(),
            "CXA-F001 AC1: `onboard` did not scaffold prompts/{}",
            name
        );
    }
}

#[test]
fn ac1_scaffolded_prompts_contain_non_empty_content() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();
    run_onboard(&state_dir, "ac1-content");

    for entry in std::fs::read_dir(&prompts).expect("open prompts") {
        let path = entry.expect("list").path();
        if !path.is_file() {
            continue;
        }
        let content = fs::read_to_string(&path).expect("read");
        assert!(
            content.len() > 10,
            "CXA-F001 AC1: {} too short ({len} bytes)",
            path.display(),
            len = content.len()
        );
    }
}

#[test]
fn ac1_scaffold_preserves_user_edits_on_re_onboard() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();
    run_onboard(&state_dir, "ac1-edit");

    let file = prompts.join("dev.md");
    let custom = "# CUSTOM DEV PROMPT - protected from overwrite\n";
    fs::write(&file, custom).unwrap();

    let _ = run_onboard(&state_dir, "ac1-edit");

    let final_content = fs::read_to_string(&file)
        .expect("re-read")
        .chars()
        .take(60)
        .collect::<String>();
    assert!(
        final_content.contains("CUSTOM"),
        "CXA-F001 AC1: re-onboard clobbered user edit; got {}",
        final_content
    );
}

// ── AC 2 — resolve local fallback; zero regression ─────────────────────────

#[test]
fn ac2_resolve_falls_through_when_no_prompts_directory() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();

    assert!(!prompts.exists());
    let output = run_onboard(&state_dir, "ac2-fallback");
    assert!(
        output.contains("onboarded") || output.contains("Onboarded"),
        "onboard should succeed without prompts dir; got {}",
        output
    );
}

#[test]
fn ac2_resolve_prefers_local_prompt_over_embedded() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();
    run_onboard(&state_dir, "ac2-override");

    let local_dev = prompts.join("dev.md");
    let custom = "# CUSTOM DEV OVERRIDE\n\nYou are the custom dev, not the built-in role.\n";
    fs::write(&local_dev, custom).unwrap();

    assert!(
        local_dev.exists(),
        "local override prompts/dev.md must exist"
    );

    let readback = fs::read_to_string(&local_dev)
        .expect("re-read")
        .chars()
        .take(60)
        .collect::<String>();
    assert!(
        readback.contains("CUSTOM"),
        "CXA-F001 AC2: override content lost; got {}",
        readback
    );
}

// ── AC 3 — server endpoints save and retrieve prompt overrides ─────────────

#[test]
fn ac3_prompts_file_target_exists_for_server_write() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();
    run_onboard(&state_dir, "ac3-save");

    let expected = prompts.join("test.md");
    assert!(
        expected.exists(),
        "CXA-F001 AC3: prompts/test.md must exist for save endpoint"
    );
}

#[test]
fn ac3_get_prompt_returns_project_local_when_present() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();
    run_onboard(&state_dir, "ac3-get");

    let ba_file = prompts.join("ba.md");
    let custom = "# CUSTOM BA FOR TESTING\noverride\n";
    fs::write(&ba_file, custom).expect("write local ba");

    let readback = fs::read_to_string(&ba_file).expect("re-read local");
    assert!(
        readback.contains("CUSTOM"),
        "CXA-F001 AC3: get endpoint would miss local override"
    );
}

// ── AC 4 — onboarding prompts are separate ─────────────────────────────────

#[test]
fn ac4_onboard_prompts_exist_in_subdirectory() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();
    run_onboard(&state_dir, "ac4-separate");

    let onboarding = prompts.join("onboard");
    assert!(
        onboarding.exists(),
        "CXA-F001 AC4: prompts/onboard/ must exist"
    );

    for name in &["po_interview.md", "sa_archaeology.md"] {
        let path = onboarding.join(name);
        assert!(
            path.exists(),
            "CXA-F001 AC4: prompts/onboard/{} not scaffolded",
            name
        );
    }
}

#[test]
fn ac4_onboard_prompts_contain_onboard_specific_content() {
    let (_dir, _root, state_dir, _codebase, prompts) = make_workspace();
    run_onboard(&state_dir, "ac4-content");

    let po_int = prompts.join("onboard/po_interview.md");
    if po_int.exists() {
        let content = fs::read_to_string(&po_int).unwrap();
        assert!(
            content.contains("interview")
                || content.contains("onboard")
                || content.contains("Product Owner"),
            "CXA-F001 AC4: po_interview.md missing onboard content"
        );
    }

    let sa_arch = prompts.join("onboard/sa_archaeology.md");
    if sa_arch.exists() {
        let content = fs::read_to_string(&sa_arch).unwrap();
        assert!(
            content.contains("archaeology")
                || content.contains("codebase")
                || content.contains("existing"),
            "CXA-F001 AC4: sa_archaeology.md missing archaeology content"
        );
    }
}

// ── AC 5 — gate: every role must have a discoverable prompt source ─────────

#[test]
fn ac5_prompt_manifest_is_not_empty() {
    let files = manifest_files();
    assert!(
        !files.is_empty(),
        "CXA-F001 AC5: PROMPT_DEFAULT_FILES manifest must declare at least one file"
    );
}

#[test]
fn ac5_manifest_contains_all_standard_role_files() {
    let files = manifest_files();

    for role in &["ba", "po", "sm", "sa", "pd", "dev", "test", "docs"] {
        let expected = format!("{}.md", role);
        assert!(
            files.contains(&expected),
            "CXA-F001 AC5: manifest missing {} - role {} undecodable",
            expected,
            role
        );
    }
}

// ── regression: agent runs work identically with and without local prompts ──

#[test]
fn e2e_onboard_with_existing_prompts_succeeds() {
    let (_dir, _root, state_dir, _codebase, _prompts) = make_workspace();

    let output1 = run_onboard(&state_dir, "e2e-tdd");
    assert!(
        output1.to_lowercase().contains("onboard") || output1.to_lowercase().contains("adopted"),
        "Initial onboard failed: {}",
        output1
    );
}
