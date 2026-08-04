//! CXA-F003 — TL Project Activation & Walking Skeleton: TDD acceptance tests.
//!
//! Each test encodes exactly one acceptance criterion and is intended to FAIL
//! until the walking skeleton lands:
//!   1. `state/project_context.md` carries the product definition (user,
//!      must-haves, success criteria, conventions) written by BA.
//!   2. The service answers `GET /health` with 200 within 5s of launch.
//!   3. `docker compose up -d --build` builds from source and the TEST agent's
//!      `/health` curl smoke test passes.
//!   4. The walking skeleton is tracked as the first FEAT ticket (`…-F001`),
//!      and any baseline bugs are filed as BUG tickets.
//!   5. A user-facing doc page (README.md or docs/*.md) describes what the
//!      service is, how to run it, and what `/health` returns.
//!
//! Red phase only — no implementation lives here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use coxagent_application::ports::outbound::AuditPort;
use coxagent_application::state::ProjectState;
use coxagent_domain::TicketType;
use coxagent_presentation::{serve_full, HubExtras};

/// In-process service boot port. Unique in this crate (`deploy_smoke` binds
/// 8101, `pr_review_role_gate` 47_711) so parallel tests never race it.
const HEALTH_PORT: u16 = 47_721;

/// The published host port the repo's docker-compose.yml maps (8101 → 4000).
const HOST_PORT: u16 = 8101;
const READY_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn state_dir() -> PathBuf {
    repo_root().join("state")
}

// ── AC1: project_context.md carries the product definition ───────────────

#[test]
fn project_context_md_carries_the_product_definition() {
    let path = state_dir().join("project_context.md");
    let body = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "state/project_context.md is missing ({e}) — BA has not written the \
             product definition in the first productive cycle"
        )
    });
    let lower = body.to_lowercase();

    assert!(lower.contains("user"), "missing: who the user is");
    assert!(lower.contains("must"), "missing: must-haves");
    assert!(
        lower.contains("success criteria"),
        "missing: success criteria"
    );
    assert!(
        lower.contains("convention"),
        "missing: conventions to respect"
    );
}

// ── AC2: the service answers /health within 5s of launch ─────────────────

#[tokio::test]
async fn service_answers_health_within_five_seconds_of_launch() {
    let dir = tempfile::tempdir().expect("temp hub dir");
    let audit: Arc<dyn AuditPort> = Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    let extras = HubExtras {
        hub_dir: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    // Launch the existing service (the hub's own axum server) in-process.
    tokio::spawn(serve_full(vec![], HEALTH_PORT, audit, extras));

    let client = reqwest::Client::new();
    let health_url = format!("http://127.0.0.1:{HEALTH_PORT}/health");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut last: Option<u16> = None;
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client
            .get(&health_url)
            .timeout(Duration::from_millis(500))
            .send()
            .await
        {
            let code = resp.status().as_u16();
            last = Some(code);
            if code == 200 {
                return; // AC met: /health answered 200 within 5s of launch.
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Disambiguate the failure: /api/health is the existing liveness probe, so
    // it answering proves the service launched — a failure here is therefore the
    // missing /health route, not a boot failure.
    let api_ok = client
        .get(format!("http://127.0.0.1:{HEALTH_PORT}/api/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .map(|r| r.status().as_u16() == 200)
        .unwrap_or(false);
    panic!(
        "service did not answer 200 on /health within 5s of launch \
         (last /health status: {last:?}; /api/health booted: {api_ok})"
    );
}

// ── AC3: docker-compose builds from source; /health smoke passes ─────────

struct ComposeStack {
    root: PathBuf,
}

impl ComposeStack {
    fn claim(root: PathBuf) -> Self {
        let stack = Self { root };
        stack.down();
        stack
    }
    fn down(&self) {
        let _ = compose(&self.root, &["down", "-v"]);
    }
}

impl Drop for ComposeStack {
    fn drop(&mut self) {
        self.down();
    }
}

fn compose(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .arg("compose")
        .args(args)
        .current_dir(root)
        .env("COXAGENT_ADMIN_PASSWORD", "ci-smoke")
        .output()
        .unwrap_or_else(|e| panic!("`docker compose {}` failed to spawn: {e}", args.join(" ")))
}

#[tokio::test]
#[ignore = "builds a release image and binds host port 8101; run with --ignored"]
async fn docker_compose_builds_and_health_smoke_passes() {
    let root = repo_root();
    let stack = ComposeStack::claim(root.clone());

    // The exact bring-up command docker-compose.yml documents.
    let up = compose(&root, &["up", "-d", "--build"]);
    assert!(
        up.status.success(),
        "`docker compose up -d --build` failed:\n{}",
        String::from_utf8_lossy(&up.stderr)
    );

    // TEST agent's curl smoke test: poll /health on the published host port.
    let client = reqwest::Client::new();
    let url = format!("http://localhost:{HOST_PORT}/health");
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let mut last: Option<u16> = None;
    while std::time::Instant::now() < deadline {
        if let Ok(resp) = client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            let code = resp.status().as_u16();
            last = Some(code);
            if code == 200 {
                return; // TEST agent smoke test passed.
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let logs = compose(&stack.root, &["logs", "--no-color", "--tail", "50"]);
    panic!(
        "TEST agent /health smoke test failed: never got 200 on :{HOST_PORT}/health (last: {last:?})\n{}",
        String::from_utf8_lossy(&logs.stdout)
    );
}

// ── AC4: walking skeleton is the first FEAT ticket; baseline bugs filed ───

/// Numeric sequence of a feature ticket id (`CXA-F003` → 3).
fn feat_seq(id: &str) -> u32 {
    id.rsplit_once('-')
        .and_then(|(_, suffix)| suffix.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()))
        .unwrap_or(u32::MAX)
}

#[test]
fn walking_skeleton_is_first_feat_ticket_and_baseline_bugs_filed() {
    let path = state_dir().join("state.json");
    let body = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("project state not activated: state/state.json is missing ({e})")
    });
    let state: ProjectState =
        serde_json::from_str(&body).expect("state/state.json is not a valid ProjectState");

    // The walking skeleton is tracked as the first FEAT ticket (id …-F001).
    let first_feat = state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Feature)
        .min_by_key(|t| feat_seq(t.id().as_str()))
        .expect("no FEAT ticket in the backlog — the walking skeleton is not tracked");
    assert!(
        first_feat
            .title()
            .to_lowercase()
            .contains("walking skeleton"),
        "first FEAT ticket is '{}', expected the walking skeleton",
        first_feat.title()
    );
    assert!(
        first_feat.id().as_str().ends_with("F001"),
        "first FEAT ticket is {}, expected …-F001 (the walking skeleton)",
        first_feat.id()
    );

    // Baseline bugs (if any from the brownfield scan) are filed as BUG tickets,
    // each triaged with a title — not a raw, untitled scan dump.
    for t in state
        .tickets
        .iter()
        .filter(|t| t.ticket_type() == TicketType::Bug)
    {
        assert!(
            !t.title().trim().is_empty(),
            "BUG ticket {} has an empty title — not a filed/triaged baseline bug",
            t.id()
        );
    }
}

// ── AC5: user-facing docs describe the service, how to run it, /health ────

/// True when a doc page covers all three AC5 descriptors.
fn doc_covers_ac(body: &str) -> bool {
    let lower = body.to_lowercase();
    let has_what = body.lines().any(|l| l.starts_with("# "));
    let has_run = lower.contains("docker compose")
        || lower.contains("coxagent serve")
        || lower.contains("coxagent hub")
        || lower.contains("cargo run")
        || lower.contains("cargo build");
    let has_health = lower.contains("/health")
        && (lower.contains("\"status\"")
            || lower.contains("status: ok")
            || lower.contains("status=ok")
            || lower.contains("200"));
    has_what && has_run && has_health
}

#[test]
fn user_facing_docs_describe_service_run_and_health_endpoint() {
    // README.md is the primary user-facing page; a DOCS-produced guide under
    // docs/ is equally acceptable.
    let readme = repo_root().join("README.md");
    if doc_covers_ac(&fs::read_to_string(&readme).unwrap_or_default()) {
        return;
    }
    let docs = repo_root().join("docs");
    if let Ok(entries) = fs::read_dir(&docs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && doc_covers_ac(&fs::read_to_string(&path).unwrap_or_default())
            {
                return;
            }
        }
    }
    panic!(
        "no user-facing doc (README.md or docs/*.md) describes what the service is, \
         how to run it, AND what /health returns"
    );
}
