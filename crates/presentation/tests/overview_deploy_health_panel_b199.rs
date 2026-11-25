//! CXA-B181.2 — presentation guardrails for the above-the-fold DEPLOY
//! HEALTH panel (`#ov-deploy` + its forensics/rollback sub-renders,
//! `crates/presentation/src/web/js/core.js` renderActive overview branch).
//!
//! Panel-list source: CXA-B181.1's enumerated fold set
//! (`crates/application/tests/overview_fold_guardrails_b181.rs`, panel 4).
//! Convention: the no-harness `include_str!` discipline of
//! `worklog_panel_terminal_state_b131.rs` — pin the bytes the hub serves.
//! No server, no port, no invented types.
//!
//! Depth-bar contract asserted here: a failing deploy renders WHY (summary,
//! failure bundle, container logs), a rollback renders its outcome and
//! timestamp (attribution), and the healthy state stays explicable (summary +
//! when).

// Test scratch-parsing of shipped JS/HTML bytes: an `expect` here fails the
// test loudly, which is exactly what a guardrail should do (repo convention).
#![allow(clippy::expect_used, clippy::unwrap_used)]

/// The dashboard shell the router serves verbatim.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The overview render path — owner of the deploy health panel.
const CORE_JS: &str = include_str!("../src/web/js/core.js");

/// The overview branch of renderActive(), from its guard to the changelog —
/// bounded so later branches don't leak into these pins.
fn overview_branch() -> &'static str {
    let start = CORE_JS
        .find("function renderActive(){")
        .expect("core.js must define renderActive()");
    let end = CORE_JS[start..]
        .find("ov-changelog")
        .map(|i| start + i)
        .expect("renderActive's overview branch must paint the changelog");
    &CORE_JS[start..end]
}

/// AC(depth 6): the deploy state is never a silent idle — a failure paints
/// "Deployment failed" with the deploy's own summary, and the forensic
/// sub-render surfaces the compose stderr tail plus per-container logs, with
/// secrets masked before they hit the DOM.
#[test]
fn a_failed_deploy_says_why_with_forensics() {
    assert!(
        CORE_JS.contains("Deployment ${dp.ok?'healthy':'failed'}"),
        "the panel headline must flip between healthy and failed — never a \
         stateless green dot"
    );
    assert!(
        // Pinned at core.js:805 — the row renders inside the same flex block.
        CORE_JS.contains("${esc(dp.summary)}"),
        "the deploy row must carry the deploy record's own summary — the WHY"
    );
    assert!(
        CORE_JS.contains("compose failed before any container started"),
        "even a bundle with no logs must explain itself — 'compose failed \
         before any container started', not a blank forensics block"
    );
    assert!(
        CORE_JS.contains("${esc(maskSecrets(b.stderr_tail||\"\"))}"),
        "the failure bundle's stderr must be secret-masked before render — \
         attribution that leaks credentials is a worse failure"
    );
    assert!(
        CORE_JS.contains("container · ${esc(c.service)}"),
        "per-container log tails must name their service — the WHY at the \
         right grain"
    );
}

/// AC(depth 3): the deploy row carries its timestamp — when the state was
/// observed, the attribution half of the depth bar.
#[test]
fn the_deploy_row_stamps_when_it_was_observed() {
    assert!(
        // Pinned on CORE_JS (core.js:806): the dp row is painted in the same
        // renderActive overview branch, after the changelog boundary the
        // helper cuts at, so pin against the whole file, not the branch slice.
        CORE_JS.contains("${esc((dp.at||\"\").slice(0,16).replace(\"T\",\" \"))}"),
        "the deploy row must render dp.at — a health verdict without a when \
         is unattributable"
    );
    // And a rollback outcome renders as its own attributed row.
    assert!(
        CORE_JS.contains("function rollbackOutcomeHtml(rb)"),
        "the rollback outcome renderer must exist next to the deploy panel"
    );
    assert!(
        CORE_JS.contains("${esc((rb.at||\"\").slice(0,16).replace(\"T\",\" \"))}"),
        "a rollback row must stamp when it happened"
    );
    assert!(
        CORE_JS.contains("${esc(rb.summary||rb.reason||\"\")}",),
        "a rollback row must carry its summary or reason — never a bare \
         'Rolled back' with no why"
    );
}

/// AC(depth 6, inverse): a failed rollback must not paint as success — the
/// outcome badge is sign-explicit (green "Rolled back" vs red "Rollback
/// failed"), so the panel never silently idles a botched recovery.
#[test]
fn a_failed_rollback_never_paints_as_success() {
    let fn_at = CORE_JS
        .find("function rollbackOutcomeHtml(rb)")
        .expect("rollbackOutcomeHtml must exist");
    let body = &CORE_JS[fn_at..];
    assert!(
        body.contains("${ok?\"Rolled back\":\"Rollback failed\"}"),
        "the badge text must flip on rb.ok — a failed recovery must be named"
    );
    assert!(
        body.contains("ok?\"var(--green)\":\"var(--red)\""),
        "the badge colour must flip with the verdict too"
    );
}

/// AC(fold seam, panel 4): the deploy panel is a dedicated slot of the
/// overview view, and the zero state is explicit ("") only when the project
/// genuinely has no deploy record.
#[test]
fn the_deploy_slot_is_anchored_in_the_overview_view() {
    let slot = INDEX_HTML
        .find("id=\"ov-deploy\"")
        .expect("#ov-deploy slot must exist");
    let view = INDEX_HTML
        .find("id=\"view-overview\"")
        .expect("#view-overview must exist");
    assert!(
        view < slot,
        "the deploy panel must live inside the Overview view — CXA-B181.1's \
         panel 4"
    );
    // The pre-state branch clears the slot; the loaded branch fills it.
    assert!(
        overview_branch().contains("ov-deploy\").innerHTML=''"),
        "the pre-state branch must clear the slot so a previous project's \
         deploy verdict never leaks across a project switch"
    );
}
