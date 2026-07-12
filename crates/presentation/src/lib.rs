//! CoXAgent presentation layer — inbound adapters (CLI, HTTP/SSE/WS).
//!
//! M0 provides a plain-text state report; the axum server and clap CLI land in
//! later milestones. Presentation is swappable — a native shell reuses the same
//! application ports without touching domain/application.

use coxagent_application::state::ProjectState;
use coxagent_domain::ticket::Status;
use std::fmt::Write as _;

/// Render a terminal-friendly summary of the current state.
#[must_use]
pub fn render_report(state: &ProjectState) -> String {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for t in &state.tickets {
        *counts.entry(status_label(t.status())).or_default() += 1;
    }
    let mut out = String::from("=== COXAGENT STATE ===\n");
    // Writes to a String are infallible; the discarded results are intentional.
    let _ = writeln!(out, "schema_version : {}", state.schema_version);
    let _ = writeln!(out, "version        : {}", state.current_version);
    let _ = writeln!(out, "tickets        : {}", state.tickets.len());
    for (status, n) in counts {
        let _ = writeln!(out, "  {status:<12} : {n}");
    }
    out
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Pending => "pending",
        Status::Ready => "ready",
        Status::InProgress => "in_progress",
        Status::Done => "done",
        Status::Documented => "documented",
        Status::Rejected => "rejected",
        Status::Open => "open",
        Status::Fixed => "fixed",
        Status::Verified => "verified",
    }
}
