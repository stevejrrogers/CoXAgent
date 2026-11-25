//! CXA-B181.2 — presentation guardrails for the above-the-fold drain banner
//! (`#ov-drain`, `drainBanner()` in
//! `crates/presentation/src/web/js/shell.js`).
//!
//! Panel-list source: CXA-B181.1's enumerated fold set
//! (`crates/application/tests/overview_fold_guardrails_b181.rs`, panel 2).
//! Convention: the no-harness `include_str!` discipline of
//! `worklog_panel_terminal_state_b131.rs` — pin the bytes the hub serves.
//! No server, no port, no invented types.
//!
//! Depth-bar contract asserted here: a paused/holding state must say WHY
//! (attribution: the PR count it is waiting on), name who/what it touches
//! (all agents, all users), and ship the way out (the merge-sweep action and
//! its Review drill-down) — never a silent hold.

// Test scratch-parsing of shipped JS/HTML bytes: an `expect` here fails the
// test loudly, which is exactly what a guardrail should do (repo convention).
#![allow(clippy::expect_used, clippy::unwrap_used)]

/// The dashboard shell the router serves verbatim.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The render path that paints `#ov-drain` on the Overview view.
const SHELL_JS: &str = include_str!("../src/web/js/shell.js");

/// AC(depth 6): the hold is never silent — the banner states WHY agents are
/// paused, with the count it is waiting on and the oldest-first merge rule.
#[test]
fn the_hold_names_its_cause_its_count_and_its_rule() {
    let banner = SHELL_JS
        .find("CLEAN-BASE DRAIN")
        .expect("drainBanner must paint the CLEAN-BASE DRAIN notice");
    let body = &SHELL_JS[banner..banner + 900];
    assert!(
        body.contains("refactor waiting on a clean merge of"),
        "the banner must say WHY work is paused — a refactor waiting on the \
         merge queue, not a bare 'paused' pill"
    );
    assert!(
        body.contains("${DRAIN_CACHE.n} PR"),
        "the banner must attribute the hold to a live count: the number of \
         PRs blocking the merge (why NOW)"
    );
    assert!(
        body.contains("only fix conflicts &amp; merge (oldest first)"),
        "the banner must name the standing rule so the pause is explicable"
    );
}

/// AC(depth 6): the hold says WHO it touches — every agent of every user —
/// so the blast radius is stated, not implied.
#[test]
fn the_hold_states_who_it_touches() {
    assert!(
        SHELL_JS.contains("All agents for all users PAUSE new work"),
        "the banner must name its blast radius (all agents, all users) — \
         attribution depth bar"
    );
}

/// AC(depth 4+7): the paused state ships its inverse and its drill-down — the
/// one-click merge sweep (the way out) and a link into the Review queue that
/// owns the blocking list.
#[test]
fn the_hold_ships_its_way_out_and_its_drill_down() {
    assert!(
        SHELL_JS.contains("onclick=\"mergeSweep()\""),
        "the banner must carry the merge-sweep action — the inverse of the hold"
    );
    assert!(
        SHELL_JS.contains("Open Review to merge green PRs"),
        "the banner must drill down into the queue that owns the blocking PRs"
    );
    // The sweep itself reports its outcome with counts, never silently.
    assert!(
        SHELL_JS.contains("Merged ${m} PR ✓") && SHELL_JS.contains("No PRs merged"),
        "the sweep must toast its outcome — merged count or the zero-state \
         'no PRs merged', never a silent no-op"
    );
}

/// AC(fold seam, panel 2): the drain banner is a dedicated slot of the
/// overview view, present in the shipped shell next to the fold's headline.
#[test]
fn the_drain_slot_is_anchored_in_the_overview_view() {
    let slot = INDEX_HTML
        .find("id=\"ov-drain\"")
        .expect("#ov-drain slot must exist");
    let view = INDEX_HTML
        .find("id=\"view-overview\"")
        .expect("#view-overview must exist");
    assert!(
        view < slot,
        "the drain banner must live inside the Overview view — CXA-B181.1's \
         panel 2"
    );
    // And the Overview nav path repaints it, so a stale banner can't idle.
    assert!(
        SHELL_JS.contains("if(CUR===\"overview\")drainBanner(\"ov-drain\");"),
        "nav('overview') must repaint the banner — a hold notice that is not \
         refreshed is a silent idle"
    );
}
