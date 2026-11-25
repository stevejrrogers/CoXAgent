//! CXA-B181.2 — presentation guardrails for the above-the-fold ALERTS strip
//! (`#ov-alerts`, `alertsHtml()` in
//! `crates/presentation/src/web/js/core.js`).
//!
//! Panel-list source: CXA-B181.1's enumerated fold set
//! (`crates/application/tests/overview_fold_guardrails_b181.rs`, panel 5).
//! Convention: the no-harness `include_str!` discipline of
//! `worklog_panel_terminal_state_b131.rs` — pin the bytes the hub serves.
//! No server, no port, no invented types.
//!
//! Depth-bar contract asserted here: every alert carries a severity colour,
//! a headline and a WHY line naming its cause and its consequence; a cleared
//! strip renders empty (signal, not noise); and the loop-paused alert names
//! the pause.

// Test scratch-parsing of shipped JS/HTML bytes: an `expect` here fails the
// test loudly, which is exactly what a guardrail should do (repo convention).
#![allow(clippy::expect_used, clippy::unwrap_used)]

/// The overview render path — owner of `alertsHtml`.
const CORE_JS: &str = include_str!("../src/web/js/core.js");

/// AC(depth 6): every alert row is a WHY row — a severity colour, a headline
/// naming the condition, and a detail line naming cause + consequence. Never
/// a bare "something is wrong" pill.
#[test]
fn every_alert_row_carries_severity_headline_and_why() {
    let fn_at = CORE_JS
        .find("function alertsHtml(s,m,spend)")
        .expect("core.js must define alertsHtml()");
    let body = &CORE_JS[fn_at..];
    assert!(
        body.contains("[\"${c}\"]") || body.contains("var(--${c})"),
        "every alert must carry a severity colour (red/amber) via the \
         colour variable"
    );
    assert!(
        body.contains("<div style=\"font-size:13px;font-weight:600\">${t}</div><div style=\"font-size:12px;color:var(--muted)\">${d}</div>"),
        "every alert must render headline + WHY detail line together — the \
         detail is the attribution"
    );
}

/// AC(depth 6): each alert family explains itself — deploy failure says WHAT
/// failed, budget says the numbers and the consequence, reverted work says
/// what it costs next cycle.
#[test]
fn each_alert_family_states_its_consequence() {
    let fn_at = CORE_JS
        .find("function alertsHtml(s,m,spend)")
        .expect("core.js must define alertsHtml()");
    let body = &CORE_JS[fn_at..];
    assert!(
        body.contains("Last deployment failed"),
        "deploy failure must be surfaced as an alert with the deploy's own \
         summary as its WHY"
    );
    assert!(
        body.contains("loop paused"),
        "the budget-cap alert must say the loop pauses — consequence, not \
         just the number"
    );
    assert!(
        body.contains("planning weights them down next cycle"),
        "confirmed reverts must say what happens next cycle — the CXC-F047 \
         attribution"
    );
    assert!(
        body.contains("decide in the Inbox"),
        "pending reverts must name where the human decides"
    );
}

/// AC(depth 5): a healthy project renders an EMPTY alerts strip — "no alert"
/// must read as "nothing needs you", not as a hidden panel or a placeholder
/// ghost. Silence here is the correct signal, so the strip must render "".
#[test]
fn a_healthy_project_renders_an_empty_alerts_strip() {
    assert!(
        CORE_JS.contains("if(!al.length)return \"\";"),
        "no alerts must render an empty strip — alerting that fires on health \
         is noise"
    );
    // And the pre-state branch clears the slot instead of leaving a ghost.
    assert!(
        CORE_JS.contains("ov-alerts\").innerHTML=''"),
        "the pre-state branch must clear the alerts slot so a previous \
         project's red banner never leaks across a project switch"
    );
}

/// AC(depth 3): the strip is count-bearing where it matters — the bug alert
/// carries the open-bug count in its headline, not just a colour.
#[test]
fn the_bug_alert_names_its_count() {
    let fn_at = CORE_JS
        .find("function alertsHtml(s,m,spend)")
        .expect("core.js must define alertsHtml()");
    let body = &CORE_JS[fn_at..];
    assert!(
        // The source interpolates via JS string concat: m.openBugs+" open bugs"
        body.contains("m.openBugs+\" open bugs\""),
        "the bug alert must carry the live open-bug count in its headline — \
         counts alongside the signal"
    );
    assert!(
        body.contains("m.openBugs>=5"),
        "the bug alert must have an explicit threshold so 'alerting' is a \
         defined state, not a mood"
    );
}
