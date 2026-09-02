//! CXA-B151 — a fixture boot beside a live hub must keep its metrics admin
//! listener.
//!
//! The ticket, verbatim: "Every e2e fixture boot on a dev host that also runs
//! the live hub (which owns 127.0.0.1:9010) logs: 'ERROR metrics admin
//! listener could not bind 127.0.0.1:9010 (Address already in use (os error
//! 48)) — set COXAGENT_METRICS_PORT to a free port; the hub continues without
//! it'. ... expected (hermetic-suite policy from the F326/F327 decisions):
//! the fixture scripts should pin COXAGENT_METRICS_PORT so a co-resident live
//! hub cannot silently strip metrics from the suite's server."
//!
//! WHERE THE SUBJECTS LIVE (this tree): the sourced guard's free-port pick
//! (`e2e/fixture-guard.sh` → `pick_free_loopback_port`) and the two fixture
//! boot scripts (`e2e/run-server.sh`, `e2e/run-server-auth.sh`). The Rust
//! side (`crates/presentation/src/server/metrics_admin.rs`,
//! `DEFAULT_METRICS_PORT = 9010`) is the documented fail-open posture and is
//! deliberately NOT changed — the fix is the pin, exactly as the ticket
//! states, and pinning the shared default itself would be the bug, not the
//! fix (9010 is the port the live hub owns).
//!
//! GUARD STYLE: repo-source scans with pure predicates, unit-tested on
//! synthetic scripts both ways — the established convention of this tree's
//! acceptance gates for surfaces that have no executable Rust seam
//! (`e2e_fixture_hardening_f315_tdd.rs`). No server, no harness, no port.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

const GUARD: &str = "e2e/fixture-guard.sh";
const OPEN_SCRIPT: &str = "e2e/run-server.sh";
const AUTH_SCRIPT: &str = "e2e/run-server-auth.sh";
/// The shared default the live hub owns (`metrics_admin.rs`
/// DEFAULT_METRICS_PORT). Pinning THIS value reproduces the bug.
const SHARED_DEFAULT: &str = "9010";

// --- repo-source scan helpers (the f315 gate's convention) --------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The script with whole-line `#` comments dropped, so prose in a header can
/// never satisfy a code predicate. These scripts only comment whole lines.
fn shell_code(src: &str) -> String {
    src.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

// --- predicates ----------------------------------------------------------------

/// The child server must be handed a `COXAGENT_METRICS_PORT`, and it must not
/// be the shared default: a boot without the pin loses the metrics admin
/// listener to whatever owns 9010 (the live hub, on every dev host), and the
/// hub continues without it by design — a silent loss. Pure over script text.
fn metrics_port_pinned(src: &str) -> Result<(), String> {
    let code = shell_code(src);
    let pinned = code.contains("COXAGENT_METRICS_PORT=");
    let pinned_to_default = code.contains(&format!("COXAGENT_METRICS_PORT={SHARED_DEFAULT}"))
        || code.contains(&format!("COXAGENT_METRICS_PORT=\"{SHARED_DEFAULT}\""));
    match (pinned, pinned_to_default) {
        (true, false) => Ok(()),
        (true, true) => Err(format!(
            "script pins COXAGENT_METRICS_PORT to the shared default {SHARED_DEFAULT} — \
             exactly the port a co-resident live hub owns; the pin must be a free port"
        )),
        (false, _) => Err(format!(
            "script never sets COXAGENT_METRICS_PORT — the child boots with the shared \
             default 127.0.0.1:{SHARED_DEFAULT} and silently loses its metrics admin \
             listener whenever the live hub owns that port"
        )),
    }
}

/// The pin must be collision-free: the port comes from the kernel (bind port
/// 0 on loopback, read the assignment, release), not from a hardcoded
/// literal — a hardcoded pin just moves the squatter flake to a new number.
/// `listen(0` is the kernel-assigned-port marker. Pure over the guard text.
fn kernel_assigned_pick(src: &str) -> Result<(), String> {
    let code = shell_code(src);
    let binds_ephemeral = code.contains("listen(0");
    let on_loopback = code.contains("127.0.0.1");
    if !(binds_ephemeral && on_loopback) {
        return Err(format!(
            "the free-port pick is not kernel-assigned (binds_ephemeral={binds_ephemeral} \
             loopback={on_loopback}) — a hardcoded port re-creates the squatter flake on a \
             new number; bind port 0 on 127.0.0.1 and use the assigned port"
        ));
    }
    Ok(())
}

/// One pick, shared by both suites: a private copy per script drifts the way
/// duplicated shell helpers always do. Pure over script text.
fn pin_routes_through_the_shared_helper(src: &str) -> Result<(), String> {
    let code = shell_code(src);
    if !code.contains("pick_free_loopback_port") {
        return Err(
            "script does not route the metrics port through the shared \
             pick_free_loopback_port helper — duplicated picks drift"
                .to_owned(),
        );
    }
    Ok(())
}

/// A failed pick must abort the boot: `export VAR="$(helper)"` masks the
/// helper's exit status under set -e on some /bin/sh (verified on macOS sh),
/// so the boot would continue with an EMPTY port and silently fall back to
/// the shared default — the exact bug this ticket fixes, resurrected. Pick
/// with a plain assignment (its failure aborts), export afterwards. Pure
/// over script text.
fn pick_failure_aborts_the_boot(src: &str) -> Result<(), String> {
    let masked = shell_code(src).lines().any(|l| {
        l.contains("export") && l.contains("COXAGENT_METRICS_PORT=") && l.contains("$(")
    });
    if masked {
        return Err(
            "export COXAGENT_METRICS_PORT=\"$(…)\" masks the pick's failure under set -e \
             on some /bin/sh — the boot continues with an EMPTY port and silently falls \
             back to the shared default; assign with a plain statement, export after"
                .to_owned(),
        );
    }
    Ok(())
}

// --- AC: the pin exists, in both fixture boot scripts ---------------------------

#[test]
fn ac_both_fixture_scripts_pin_the_metrics_admin_port() {
    for script in [OPEN_SCRIPT, AUTH_SCRIPT] {
        metrics_port_pinned(&read(script)).unwrap_or_else(|why| panic!("{script}: {why}"));
        pin_routes_through_the_shared_helper(&read(script))
            .unwrap_or_else(|why| panic!("{script}: {why}"));
        pick_failure_aborts_the_boot(&read(script))
            .unwrap_or_else(|why| panic!("{script}: {why}"));
    }
}

#[test]
fn ac_the_shared_pick_is_kernel_assigned_on_loopback() {
    kernel_assigned_pick(&read(GUARD)).unwrap_or_else(|why| panic!("{GUARD}: {why}"));
    let code = shell_code(&read(GUARD));
    assert!(
        code.contains("pick_free_loopback_port() {"),
        "{GUARD} does not define the shared pick_free_loopback_port helper"
    );
}

// --- synthetic both-ways: the predicates explain their verdicts ------------------

#[test]
fn ac_the_pin_predicate_rejects_a_script_that_never_sets_the_port() {
    // The exact shape both scripts shipped in before CXA-B151.
    let before = "PORT=4517\nCOXAGENT_PORT=\"$PORT\" \"$BIN\" serve\n";
    let why = metrics_port_pinned(before).unwrap_err();
    assert!(why.contains("never sets COXAGENT_METRICS_PORT"), "unhelpful: {why}");
}

#[test]
fn ac_the_pin_predicate_rejects_pinning_the_shared_default() {
    let useless = "export COXAGENT_METRICS_PORT=\"9010\"\n";
    let why = metrics_port_pinned(useless).unwrap_err();
    assert!(why.contains("shared default 9010"), "unhelpful: {why}");
}

#[test]
fn ac_the_pin_predicate_accepts_the_pinned_boot_line() {
    let pinned = "METRICS_PORT=\"$(pick_free_loopback_port)\"\n\
                  COXAGENT_PORT=\"$PORT\" COXAGENT_METRICS_PORT=\"$METRICS_PORT\" \
                  \"$BIN\" serve\n";
    metrics_port_pinned(pinned).unwrap_or_else(|why| panic!("pinned boot line rejected: {why}"));
}

#[test]
fn ac_the_pick_predicate_rejects_a_hardcoded_port() {
    let hardcoded = "node -e 's.listen(4321,\"127.0.0.1\",...)'\n";
    let why = kernel_assigned_pick(hardcoded).unwrap_err();
    assert!(why.contains("not kernel-assigned"), "unhelpful: {why}");
}

#[test]
fn ac_the_abort_predicate_rejects_the_failure_masking_export_form() {
    let masked = "export COXAGENT_METRICS_PORT=\"$(pick_free_loopback_port)\"\n";
    let why = pick_failure_aborts_the_boot(masked).unwrap_err();
    assert!(why.contains("masks the pick's failure"), "unhelpful: {why}");
}

#[test]
fn ac_the_abort_predicate_accepts_pick_then_export() {
    let two_step = "COXAGENT_METRICS_PORT=\"$(pick_free_loopback_port)\"\n\
                    export COXAGENT_METRICS_PORT\n";
    pick_failure_aborts_the_boot(two_step)
        .unwrap_or_else(|why| panic!("two-step form rejected: {why}"));
}

#[test]
fn ac_the_pick_predicate_accepts_the_kernel_assigned_shape() {
    let real = "node -e 'const s=require(\"node:net\").createServer();\
                s.listen(0,\"127.0.0.1\",()=>{console.log(s.address().port);s.close()})'\n";
    kernel_assigned_pick(real).unwrap_or_else(|why| panic!("kernel pick rejected: {why}"));
}
