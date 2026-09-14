//! CXA-B131 regression — the Work log panel (`#agent-transcript`) reached no
//! terminal state unless the agent drawer was opened.
//!
//! bf9eb0cc (CXA-B128) fixed the drawer flow: `openAgent()` paints the
//! terminal empty state and starts the SSE stream. But the panel is embedded
//! in the page DOM on EVERY view, and the shipped HTML was a bare
//! `loading…` placeholder that only the drawer path ever replaced. Visiting
//! Transcripts & alerts (/#activity) and never opening the drawer left the
//! panel on `loading…` forever — no stream engaged, no error, no empty state
//! (the healthy closed-panel case fires none).
//!
//! The same no-harness discipline as `signin_icon_font_b112.rs`: these tests
//! pin the bytes the hub actually serves and the JS wiring that normalises
//! the panel — no server, no port, no invented types.

/// The dashboard shell the router serves verbatim.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

/// The activity render path — where `nav('activity')` lands.
const CORE_JS: &str = include_str!("../src/web/js/core.js");

/// The agent-log panel code — owner of every Work log terminal state.
const SHELL_JS: &str = include_str!("../src/web/js/shell.js");

/// The panel's shipped bytes: from its id marker to the next overlay —
/// bounded, because later views legitimately still use `loading…`.
fn panel_bytes() -> &'static str {
    let after = INDEX_HTML
        .split("id=\"agent-transcript\"")
        .nth(1)
        .unwrap_or_else(|| panic!("index.html must contain the #agent-transcript Work log panel"));
    after
        .split("<div class=\"ov\"")
        .next()
        .unwrap_or_else(|| panic!("the Work log panel modal is followed by another overlay"))
}

/// AC1: the panel must ship in a terminal state — never the indefinite
/// `loading…` placeholder that nothing replaces until a drawer opens.
#[test]
fn worklog_panel_ships_a_terminal_state_not_loading() {
    let panel = panel_bytes();
    assert!(
        panel.contains("this agent hasn't run yet"),
        "the shipped Work log panel must carry the terminal empty state \
         (CXA-B131): nothing else paints it until openAgent() runs, so a \
         'loading…' default is what a /#activity visit sees forever"
    );
    assert!(
        !panel.contains("loading…"),
        "the shipped Work log panel must not sit on 'loading…' — the \
         pre-fix placeholder that never reached a terminal state"
    );
}

/// AC2: the nav('activity') render path must normalise the panel, so the
/// fix holds even if a stale/cached page or a future path leaves the
/// placeholder behind.
#[test]
fn activity_render_path_paints_the_worklog_terminal_state() {
    assert!(
        CORE_JS.contains("if(typeof paintAgentLogIdle===\"function\")paintAgentLogIdle();"),
        "renderActive()'s activity branch must call paintAgentLogIdle() — \
         the missing call in the nav('activity') path was the root cause"
    );
}

/// AC3: the painter must exist in the panel's owner module AND refuse to
/// fight a live drawer — an open drawer (role set, stream or retry in
/// flight) or buffered history is left untouched, or a state-snapshot
/// re-render would wipe a live log back to the empty state.
#[test]
fn paint_agent_log_idle_is_guarded_against_a_live_drawer() {
    let idx = SHELL_JS.find("function paintAgentLogIdle()").unwrap_or_else(|| {
        panic!("shell.js must define paintAgentLogIdle() next to the other terminal-state painters")
    });
    let body = &SHELL_JS[idx..];
    assert!(
        body.contains("AGENT_LOG_ROLE||AGENT_LOG_ES||AGENT_LOG_BUF.trim()"),
        "paintAgentLogIdle must bail when the drawer is engaged (role set, \
         stream open) or history is buffered — otherwise a re-render while \
         the drawer is open would erase the live log"
    );
    assert!(
        body.contains("this agent hasn\\'t run yet"),
        "paintAgentLogIdle must paint the same terminal empty state the \
         drawer's openAgent() path paints"
    );
}
