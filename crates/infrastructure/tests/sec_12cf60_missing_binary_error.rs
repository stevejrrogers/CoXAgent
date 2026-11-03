//! SEC-12CF60 regression gate.
//!
//! Three sweep runs died with `backend failure: spawn opencode: No such file
//! or directory (os error 2)` because the scanning CLI was not resolvable on
//! the hub host's `PATH`. A bare io::Error names neither the missing
//! executable nor the fix, so retries were copy-pasted over an unexplained
//! crash.
//!
//! This test drives the REAL adapter spawn path (`AgentEnginePort::run` →
//! `proc::agent_command` → `proc::spawn_confined` → the error mapping under
//! review) with a `PATH` that cannot resolve anything — the exact "binary not
//! installed where the hub ran" condition behind the ticket — and asserts the
//! returned `PortError::Backend`:
//!   1. still attributes the failure to the engine (`spawn opencode:`),
//!   2. NAMES the missing executable + its fix (install hint),
//!   3. no longer ships the bare `No such file or directory (os error 2)`
//!      payload as the whole story — the exact opacity that caused the ticket.
//!
//! Without the SEC-12CF60 mapping (the old `format!("spawn opencode: {e}")`),
//! assertions 2 and 3 FAIL — this is the proof the bug is dead.
#![cfg(unix)]

use coxagent_application::ports::outbound::{AgentEnginePort, AgentRequest};
use coxagent_application::PortError;
use coxagent_infrastructure::engine::opencode::OpencodeEngine;

fn request(work_dir: &std::path::Path) -> AgentRequest {
    AgentRequest {
        role: coxagent_domain::Role::DevFeature,
        system_prompt: "s".into(),
        task_prompt: "t".into(),
        work_dir: work_dir.to_path_buf(),
        // Generous budget: we exercise the production spawn path and ENOENT
        // fails fast anyway — the timeout is never the subject here.
        timeout: std::time::Duration::from_secs(120),
        escalation_level: 0,
        label: Some("SEC-12CF60-regression".into()),
    }
}

/// Run one engine call with a `PATH` guaranteed to resolve nothing, restoring
/// the original `PATH` afterwards. Safe here: this test binary contains exactly
/// one test, so there is no cross-test race on the process environment.
async fn run_with_dead_path(engine: OpencodeEngine, work_dir: &std::path::Path) -> PortError {
    let saved = std::env::var_os("PATH");
    // A directory that exists but is empty: every exec lookup — the `nice`
    // wrapper AND the engine binary under it — misses, which is precisely the
    // NotFound spawn error the ticket's mapper must translate.
    let empty = std::env::temp_dir().join(format!("cox-sec12cf60-empty-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).expect("mkdir empty PATH dir");
    std::env::set_var("PATH", &empty);

    let outcome = engine.run(request(work_dir)).await;

    std::env::set_var("PATH", saved.unwrap_or_default());
    let _ = std::fs::remove_dir_all(&empty);

    outcome.expect_err("an unresolvable binary must fail the run")
}

#[tokio::test]
async fn missing_scanner_cli_names_itself_and_its_fix_not_a_bare_enoent() {
    let dir = tempfile::tempdir().expect("work dir");

    let engine =
        OpencodeEngine::new("test/model").with_binary("/nonexistent/cox-sec-12cf60-no-such-cli");

    let err = run_with_dead_path(engine, dir.path()).await;

    let msg = match &err {
        PortError::Backend(m) => m.as_str(),
        other => panic!("expected PortError::Backend, got: {other:?}"),
    };

    assert!(
        msg.starts_with("spawn opencode:"),
        "must still attribute the failure to the engine: {msg}"
    );
    assert!(
        msg.contains("`opencode` not found on PATH"),
        "SEC-12CF60: the error must NAME the missing executable: {msg}"
    );
    assert!(
        msg.contains("install") || msg.contains("Install") || msg.contains("curl"),
        "SEC-12CF60: the error must carry an actionable install hint: {msg}"
    );
    assert!(
        !msg.trim_end().ends_with("(os error 2)")
            && !msg.ends_with("directory (os error 2)\n"),
        "the bare ENOENT payload is exactly what made three sweeps \
         undiagnosable — it must not survive as the whole story: {msg}"
    );
}
