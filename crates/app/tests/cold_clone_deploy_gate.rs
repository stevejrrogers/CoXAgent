//! CI workflow wiring guard for GATE-590C70: `.github/workflows/
//! cold-clone-deploy.yml` must stay a real cold-clone deploy gate.
//!
//! The workflow's whole reason to exist is that it compiles what
//! `deploy/Dockerfile` ships — with NO rust-cache — on every PR. Any of these
//! regressions makes it report green while guaranteeing nothing:
//!
//! 1. job gated (`if:` / `continue-on-error`) or removed;
//! 2. trigger moved off pull_request (it exists to protect PRs);
//! 3. a cache action sneaks in (Swatinem/rust-cache or actions/cache) — the
//!    "cold" half dies and the job degrades into ci.yml's `check` twin;
//! 4. the package set stops being derived from `deploy/Dockerfile`
//!    (--release --locked parity lost — the CXA-B006/B008 class of bug again).
//!
//! The check-run display name is documented in the workflow header so branch
//! protection can require it by name; a rename without updating the comment
//! would dangle a required check (the F286 lesson), hence the name pin below.

use std::path::Path;

const WORKFLOW: &str = ".github/workflows/cold-clone-deploy.yml";
const DOCKERFILE: &str = "deploy/Dockerfile";
const JOB: &str = "cold-clone";

/// Required status check name, pinned exactly as GitHub matches it.
const DISPLAY_NAME: &str = "cold-clone deploy build+test (no cache, deploy/Dockerfile package set)";

fn repo_root() -> std::path::PathBuf {
    // crate is crates/app → root is two up
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|e| panic!("repo root resolves: {e}"))
}

fn read_repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    match std::fs::read_to_string(&path) {
        Ok(src) => src,
        Err(e) => panic!(
            "cannot read `{rel}` from {}: {e} — the gate has nothing to guard",
            path.display()
        ),
    }
}

// --- YAML slice helpers (same conventions as ci_availability_gate.rs) -------

fn job_block<'a>(src: &'a str, job: &str) -> Option<&'a str> {
    let lines: Vec<&str> = src.lines().collect();
    let start = lines.iter().position(|l| *l == format!("  {job}:"))?;
    let rest = &lines[start + 1..];
    let end = rest
        .iter()
        .position(|l| l.starts_with("  ") && !l.starts_with("   ") && l.ends_with(':'))
        .unwrap_or(rest.len());
    Some(rest[..end].join("\n").leak())
}

/// Job-level disable: an `if:` gate or continue-on-error at four spaces.
fn job_gate(block: &str) -> Option<String> {
    block
        .lines()
        .find(|l| l.starts_with("    if:") || l.starts_with("    continue-on-error:"))
        .map(|l| l.trim().to_owned())
}

fn job_display_name(block: &str) -> Option<String> {
    block
        .lines()
        .find_map(|l| l.strip_prefix("    name:").map(str::trim))
        .map(ToOwned::to_owned)
}

fn top_level_key_lines<'a>(src: &'a str, key: &str) -> impl Iterator<Item = &'a str> + 'a {
    let marker: String = format!("{key}:");
    src.lines()
        .skip_while(move |l| l != &marker)
        .skip(1)
        .take_while(|l| l.starts_with(' ') || l.is_empty() || l.starts_with('#'))
}

// --- The five failure modes --------------------------------------------------

fn why_not_wired(src: &str) -> Result<(), String> {
    let block = job_block(src, JOB).ok_or_else(|| format!("no `{JOB}` job in the workflow"))?;

    if let Some(gate) = job_gate(block) {
        return Err(format!(
            "`{JOB}` is disabled by a job-level `{gate}` — the cold-clone \
             deploy gate never runs"
        ));
    }

    let pr_trigger =
        top_level_key_lines(src, "on").any(|l| l.trim_start().starts_with("pull_request"));
    if !pr_trigger {
        return Err(
            "the workflow no longer triggers on pull_request — it exists to \
             catch deploy-builder breakage BEFORE the merge, not after"
                .to_owned(),
        );
    }

    let lowered = block.to_lowercase();
    for cache in ["swatinem", "actions/cache"] {
        if lowered.contains(cache) {
            return Err(format!(
                "`{JOB}` regained a cache step ({cache}) — the cold half of \
                 the gate dies and broken-from-clean builds pass unverified"
            ));
        }
    }
    if !lowered.contains("--locked") {
        return Err("`{JOB}` dropped the --locked flag".replace("{JOB}", JOB));
    }
    if !block.contains("cargo test") {
        return Err(format!(
            "`{JOB}` stopped running tests — compile alone shipped CXA-B008"
        ));
    }
    if job_display_name(block) != Some(DISPLAY_NAME.to_owned()) {
        return Err(format!(
            "`{JOB}`'s check-run display name drifted from `{DISPLAY_NAME}` — \
             branch protection pins the exact string"
        ));
    }
    let comments: String = src
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if !comments.contains(DISPLAY_NAME) {
        return Err(format!(
            "the workflow header no longer documents the required check \
             `{DISPLAY_NAME}` — update the comment with any rename"
        ));
    }
    Ok(())
}

/// Deploy-parity rules over both files together: the Dockerfile keeps its
/// locked release builder line, and the workflow derives its package list
/// from that very line instead of hardcoding a divergent copy.
fn why_parity_lost(workflow_src: &str, dockerfile_src: &str) -> Result<(), String> {
    let build_line = dockerfile_src
        .lines()
        .find(|l| l.contains("cargo build --release --locked"))
        .ok_or_else(|| {
            "deploy/Dockerfile carries no `cargo build --release --locked …` \
             builder command — the deploy pipeline and this gate have lost \
             their single source of truth"
                .to_owned()
        })?
        .trim();

    let packages: String = build_line
        .split_once("cargo build --release --locked")
        .map(|(_, rest)| {
            rest.split_whitespace()
                .filter(|t| !t.eq_ignore_ascii_case("\\"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();
    if packages.is_empty() {
        return Err("the Dockerfile builder names no packages".to_owned());
    }

    let wf_block = job_block(workflow_src, JOB).unwrap_or("");
    if !(wf_block.contains("grep -m1") && wf_block.contains("cargo build --release --locked")) {
        return Err("the workflow no longer reads its package set out of \
             deploy/Dockerfile's builder command — hardcoding it invites drift"
            .to_owned());
    }

    let _ = build_line;
    let _ = packages;

    Ok(())
}

// --- Positive control ---------------------------------------------------------

fn wired_workflow() -> String {
    std::fs::read_to_string(repo_root().join(WORKFLOW))
        .unwrap_or_else(|e| panic!("real workflow exists at {WORKFLOW}: {e}"))
}

#[test]
fn the_real_workflow_satisfies_every_cold_clone_rule() {
    let src = wired_workflow();
    why_not_wired(&src).unwrap_or_else(|why| panic!("cold-clone gate unwired: {why}"));

    let dockerfile = read_repo_file(DOCKERFILE);
    why_parity_lost(&src, &dockerfile).unwrap_or_else(|why| panic!("deploy parity lost: {why}"));
}

#[test]
fn a_gated_or_removed_job_is_caught() {
    let src = wired_workflow().replace(
        "jobs:\n  cold-clone:",
        "jobs:\n  cold-clone:\n    if: false",
    );
    let why = match why_not_wired(&src) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("disabled by a job-level"), "unhelpful: {why}");

    let missing = wired_workflow()
        .split("jobs:")
        .next()
        .unwrap_or("")
        .to_owned();
    let why = match why_not_wired(&missing) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("no `cold-clone` job"), "unhelpful: {why}");
}

#[test]
fn losing_the_pr_trigger_is_caught() {
    let src = wired_workflow().replace(
        "on:\n  pull_request:\n    types: [opened, synchronize]\n\nenv:",
        "on:\n  workflow_dispatch:\n\nenv:",
    );
    let why = match why_not_wired(&src) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("pull_request"), "unhelpful: {why}");
}

#[test]
fn a_cache_step_creeping_back_in_is_caught() {
    let rust_cache = wired_workflow().replace(
        "      - uses: dtolnay/rust-toolchain@stable",
        "      - uses: Swatinem/rust-cache@v2\n      - uses: dtolnay/rust-toolchain@stable",
    );
    let why = match why_not_wired(&rust_cache) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("cache"), "unhelpful: {why}");

    let generic = wired_workflow().replace(
        "      - uses: dtolnay/rust-toolchain@stable",
        "      - uses: actions/cache@v4\n        with:\n          path: target\n      - uses: dtolnay/rust-toolchain@stable",
    );
    let why = match why_not_wired(&generic) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("cache"), "unhelpful: {why}");
}

#[test]
fn dropping_locked_or_tests_or_the_name_pin_is_caught() {
    let unlocked = wired_workflow()
        .lines()
        .filter(|l| !l.contains("--locked"))
        .collect::<Vec<_>>()
        .join("\n");
    let why = match why_not_wired(&unlocked) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("--locked"), "unhelpful: {why}");

    let testless = wired_workflow().replace(
        "      - name: Test the deploy package set, cold (--locked)\n        run: cargo test --locked $DEPLOY_PACKAGES",
        "",
    );
    let why = match why_not_wired(&testless) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("stopped running tests"), "unhelpful: {why}");

    let renamed = wired_workflow().replace(DISPLAY_NAME, "cold clone build");
    let why = match why_not_wired(&renamed) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("display name drifted"), "unhelpful: {why}");

    let undocumented = wired_workflow()
        .lines()
        .filter(|l| !l.contains(DISPLAY_NAME) || !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let why = match why_not_wired(&undocumented) {
        Ok(()) => panic!("expected why_not_wired to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("no longer documents"), "unhelpful: {why}");
}

#[test]
fn a_dockerfile_without_a_builder_command_is_caught() {
    let empty = "# shell image only\nFROM scratch\n";
    let why = match why_parity_lost(&wired_workflow(), empty) {
        Ok(()) => panic!("expected why_parity_lost to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("single source of truth"), "unhelpful: {why}");

    let no_packages = "RUN cargo build --release --locked\nENTRYPOINT [\"coxagent\"]\n";
    let why = match why_parity_lost(&wired_workflow(), no_packages) {
        Ok(()) => panic!("expected why_parity_lost to reject the mutated workflow"),
        Err(why) => why,
    };
    assert!(why.contains("names no packages"), "unhelpful: {why}");

    let no_grep = wired_workflow().replace(
        "cargo build --release --locked",
        "cargo build --frozen --locked",
    );
    // The grep line is gone, so the Dockerfile must still carry a valid
    // builder command — otherwise the right error fires for the wrong reason.
    let ok_dockerfile =
        "RUN cargo build --release --locked -p coxagent-app\nENTRYPOINT [\"coxagent\"]\n";
    match why_parity_lost(&no_grep, ok_dockerfile) {
        Ok(()) => panic!("a workflow without the Dockerfile grep passed parity"),
        Err(why) => assert!(why.contains("package set"), "unhelpful: {why}"),
    }
}
