//! CXA-B181.2 — presentation guardrails for the above-the-fold ACTIVITY
//! timeline (`#ov-activity`, `actItem()` in
//! `crates/presentation/src/web/js/core.js` renderActive overview branch).
//!
//! Panel-list source: CXA-B181.1's enumerated fold set
//! (`crates/application/tests/overview_fold_guardrails_b181.rs`, panel 6).
//! Convention: the no-harness `include_str!` discipline of
//! `worklog_panel_terminal_state_b131.rs` — pin the bytes the hub serves.
//! No server, no port, no invented types.
//!
//! Depth-bar contract asserted here: every row is attribution (WHO did WHAT
//! on WHICH ticket, WHEN), rows are sorted newest-first, the list is a
//! bounded window with a click-through per ticket, and the empty state says
//! what would fill it.

// Test scratch-parsing of shipped JS/HTML bytes: an `expect` here fails the
// test loudly, which is exactly what a guardrail should do (repo convention).
#![allow(clippy::expect_used, clippy::unwrap_used)]

/// The dashboard shell the router serves verbatim.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The overview render path — owner of the activity timeline.
const CORE_JS: &str = include_str!("../src/web/js/core.js");

/// AC(depth 6): every activity row is full attribution — the agent (coloured
/// by identity), the action verb, the ticket chip, and a relative timestamp
/// with the absolute time in its tooltip.
#[test]
fn every_activity_row_is_full_attribution() {
    let fn_at = CORE_JS
        .find("function actItem(a)")
        .expect("core.js must define actItem()");
    let body = &CORE_JS[fn_at..];
    assert!(
        body.contains("class=\"tl-who\" style=\"color:${col}\">${esc(a.agent)}"),
        "each row must name the agent, colour-keyed by identity"
    );
    assert!(
        body.contains("class=\"tl-act\">${esc(a.action)}"),
        "each row must name the action verb — WHO without WHAT is gossip"
    );
    assert!(
        body.contains("onclick=\"showTicket('${esc(a.ticket)}')\""),
        "each row's ticket chip must click through to the ticket"
    );
    assert!(
        body.contains("title=\"${esc(a.at||'')}\">${esc(relTime(a.at))}"),
        "each row must show a relative time with the absolute timestamp in \
         the tooltip — when, twice over"
    );
}

/// AC(depth 1+2): the timeline is a sorted, bounded window onto a longer
/// stream — the overview branch reverses the activity feed (newest first)
/// and caps it at 7 rows, a list of 7 does not paginate (depth bar: past
/// ~50 rows engages paging; this panel's window is the cap itself).
#[test]
fn the_timeline_is_newest_first_and_capped() {
    let branch_at = CORE_JS
        .find("function renderActive(){")
        .expect("core.js must define renderActive()");
    let body = &CORE_JS[branch_at..];
    assert!(
        body.contains("[...(s.activity||[])].reverse().slice(0,7)"),
        "the overview must render the newest rows first, capped at 7 — the \
         reverse+slice IS the sort+window"
    );
}

/// AC(depth 5): the empty state is explanatory — 'no activity yet' names what
/// would fill it, and the pre-state branch carries the longer variant
/// ('activity appears as agents work') instead of a blank area.
#[test]
fn the_empty_states_say_what_would_fill_the_timeline() {
    assert!(
        CORE_JS.contains("'<div class=\"empty\">no activity yet</div>'"),
        "the loaded-but-empty branch must render the explicit empty chip"
    );
    assert!(
        CORE_JS.contains("activity appears as agents work"),
        "the pre-state branch must carry the explanatory empty state — what \
         fills the panel (agents working), never a blank div"
    );
}

/// AC(fold seam, panel 6): the activity slot is a dedicated slot of the
/// overview view, present in the shipped shell below the fold seam.
#[test]
fn the_activity_slot_is_anchored_in_the_overview_view() {
    let slot = INDEX_HTML
        .find("id=\"ov-activity\"")
        .expect("#ov-activity slot must exist");
    let view = INDEX_HTML
        .find("id=\"view-overview\"")
        .expect("#view-overview must exist");
    assert!(
        view < slot,
        "the activity timeline must live inside the Overview view — \
         CXA-B181.1's panel 6"
    );
}
