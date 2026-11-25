//! CXA-B181.2 — presentation guardrails for the above-the-fold working strip
//! (`#ov-working`, `renderOvWorking()` in
//! `crates/presentation/src/web/js/core.js`).
//!
//! Panel-list source: CXA-B181.1's enumerated fold set
//! (`crates/application/tests/overview_fold_guardrails_b181.rs`, panel 3).
//! Convention: the no-harness `include_str!` discipline of
//! `worklog_panel_terminal_state_b131.rs` — pin the bytes the hub serves.
//! No server, no port, no invented types.
//!
//! Depth-bar contract asserted here: the strip names WHO is working and on
//! WHICH ticket (attribution), every chip drills into that agent's live log,
//! and a failed liveness probe degrades to an empty strip instead of a
//! permanently stale one.

// Test scratch-parsing of shipped JS/HTML bytes: an `expect` here fails the
// test loudly, which is exactly what a guardrail should do (repo convention).
#![allow(clippy::expect_used, clippy::unwrap_used)]

/// The dashboard shell the router serves verbatim.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The overview render path — owner of `renderOvWorking`.
const CORE_JS: &str = include_str!("../src/web/js/core.js");

/// AC(depth 3): the strip is a count-bearing list — one chip per working
/// agent, each chip carrying BOTH the agent role and its ticket, so the fold
/// says who is moving and on what (attribution).
#[test]
fn each_working_chip_names_the_agent_and_its_ticket() {
    let fn_at = CORE_JS
        .find("function renderOvWorking()")
        .expect("core.js must define renderOvWorking()");
    let body = &CORE_JS[fn_at..];
    assert!(
        body.contains("<b>${esc(w.role)}</b>"),
        "every chip must name the agent role — an anonymous 'working' dot has \
         no attribution"
    );
    assert!(
        body.contains("<span class=\"tk\">${esc(w.ticket)}</span>"),
        "every chip must name the ticket the agent holds — who AND what"
    );
    assert!(
        body.contains("working now"),
        "the chip must label the state so a parked browser still reads it"
    );
}

/// AC(depth 4): the chips are a window onto the live logs — each one opens
/// the agent's log drawer (drill-down), with the agent named in the tooltip.
#[test]
fn every_working_chip_drills_into_the_live_log() {
    let fn_at = CORE_JS
        .find("function renderOvWorking()")
        .expect("core.js must define renderOvWorking()");
    let body = &CORE_JS[fn_at..];
    assert!(
        body.contains("onclick=\"openAgent('${esc(w.role)}','${esc(w.who)}')\""),
        "every chip must be clickable into openAgent() — a working indicator \
         without a path to the work is a dead end"
    );
    assert!(
        body.contains("title=\"open ${esc(w.role)}'s live log\""),
        "the drill-down must name its destination in the tooltip"
    );
}

/// AC(depth 5+6): a failed probe must not freeze stale chips — the fetches
/// degrade to empty lists and an empty strip renders; and a strip with no
/// working agents renders empty rather than pretending activity. Also, the
/// strip re-probes on a gentle cadence (freshness window) so "working now"
/// cannot silently idle.
#[test]
fn a_failed_probe_degrades_to_empty_and_never_freezes_stale_chips() {
    let fn_at = CORE_JS
        .find("function renderOvWorking()")
        .expect("core.js must define renderOvWorking()");
    let body = &CORE_JS[fn_at..];
    assert!(
        body.contains(".catch(()=>[])"),
        "both probes must degrade to empty lists on failure — a dead endpoint \
         must empty the strip, not leave yesterday's chips up"
    );
    assert!(
        body.contains("if(!chips.length){setHTML(el,\"\");return;}"),
        "no working agents must render an EMPTY strip — never a stale or \
         fake 'working' row"
    );
    assert!(
        body.contains("now-OV_WORK_LAST<4000"),
        "the strip must rate-limit its own repaints (freshness window) so \
         'working now' is always near-live"
    );
    // Ground-truth fusion: a freshly-growing live log counts as working even
    // when the claim registry is stale (CXA-F386) — attribution from two
    // sources, not one cached claim.
    assert!(
        body.contains("age_secs") && body.contains("120"),
        "liveness must fuse the log-freshness signal (age under 120s) with \
         the worker registry — the CXA-F386 ground truth"
    );
}

/// AC(fold seam, panel 3): the strip is a dedicated slot of the overview
/// view, present in the shipped shell.
#[test]
fn the_working_strip_slot_is_anchored_in_the_overview_view() {
    let slot = INDEX_HTML
        .find("id=\"ov-working\"")
        .expect("#ov-working slot must exist");
    let view = INDEX_HTML
        .find("id=\"view-overview\"")
        .expect("#view-overview must exist");
    assert!(
        view < slot,
        "the working strip must live inside the Overview view — CXA-B181.1's \
         panel 3"
    );
    // And the overview render path engages it (repaint on every nav).
    assert!(
        CORE_JS.contains("renderOvWorking();"),
        "renderActive()'s overview branch must call renderOvWorking() — an \
         unengaged strip is a silent idle"
    );
}
