// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The public share status page (CXA-F069): its read model and HTML
//! rendering. Everything here is a pure function over `ProjectState` + worker
//! heartbeats — counts, ids and sanitized summaries only. The page never
//! renders raw state (no design specs, no spend, no audit log, no
//! transcripts) and never even receives the share token, so it cannot echo
//! it into the HTML. The endpoints that resolve tokens live in
//! `share_link.rs`.

use super::*;

/// A runner heartbeat older than this renders as STALE. Runners re-beat the
/// worker registry every 45s while mid-phase and once per phase otherwise, so
/// a quarter hour of silence means the loop really is down or disconnected —
/// not merely between cycles. A code constant (not a coxagent.json field):
/// the SA design added no config knob and staleness is an operations fact,
/// not a tuning preference.
const HEARTBEAT_STALE_SECS: i64 = 15 * 60;

/// A project with in-flight work but no successful deploy this old is flagged
/// on the share page — matches the "zero recent deploys while tickets are
/// active" degraded clause.
const DEPLOY_STALE_SECS: i64 = 7 * 24 * 60 * 60;

/// How many shipped records the page lists.
const SHIPPED_SHOWN: usize = 5;

// --- Read model (pure) ------------------------------------------------------
// Everything below is a pure function over state/store data so the page's
// content policy is unit-testable: what may appear is exactly what these
// functions emit.

/// One bug-severity bucket for the share page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub(super) struct BugCounts {
    pub(super) high: usize,
    pub(super) medium: usize,
    pub(super) low: usize,
}

impl BugCounts {
    fn total(self) -> usize {
        self.high + self.medium + self.low
    }
}

/// How fresh the freshest worker heartbeat is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub(super) enum Heartbeat {
    /// No runner has heartbeated (or timestamps were unparsable).
    None,
    /// Freshest heartbeat is within [`HEARTBEAT_STALE_SECS`]; carries its age.
    Fresh(i64),
    /// Freshest heartbeat is older than the threshold; carries its age.
    Stale(i64),
}

/// The share page's safe aggregates — the entire allow-list of what the
/// public page may show.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub(super) struct ShareSnapshot {
    pub(super) heartbeat: Option<Heartbeat>,
    /// (role, ticket id) pairs the worker registry reports as live — the
    /// in-progress view "by role".
    pub(super) in_flight: Vec<(String, String)>,
    pub(super) bugs: BugCounts,
    /// Open PR numbers currently in review.
    pub(super) prs_in_review: Vec<u64>,
    /// (version, ticket id, title) of recent deploys.
    pub(super) shipped: Vec<(String, String, String)>,
    /// Ticket ids parked after repeated failed attempts.
    pub(super) parked: Vec<String>,
    /// Ticket ids held for a human cost approval.
    pub(super) cost_holds: Vec<String>,
    /// Ticket ids parked on hold by a human.
    pub(super) on_hold: Vec<String>,
    /// PR numbers waiting for human eyes.
    pub(super) human_holds: Vec<u64>,
    /// Degraded/warning banners (empty = healthy).
    pub(super) warnings: Vec<String>,
    /// [`metrics::digest_markdown`] with every spend line removed — the
    /// sanitized "what happened" summary.
    pub(super) digest: Vec<String>,
}

/// Compute the share page's aggregates. Pure: same inputs, same page.
#[must_use]
pub(super) fn share_snapshot(
    state: &coxagent_application::ProjectState,
    workers: &[coxagent_application::ports::outbound::WorkerEntry],
    now_secs: i64,
) -> ShareSnapshot {
    let mut snap = ShareSnapshot {
        heartbeat: Some(heartbeat_age(workers, now_secs)),
        ..ShareSnapshot::default()
    };

    // In-flight by role: live worker registrations are the ground truth for
    // "which role is working which ticket right now".
    for w in workers {
        if w.ticket.is_empty() || w.role == "idle" {
            continue;
        }
        snap.in_flight.push((w.role.clone(), w.ticket.clone()));
    }

    // Open bugs by severity.
    for t in &state.tickets {
        if t.ticket_type() != coxagent_domain::TicketType::Bug
            || t.status() != coxagent_domain::Status::Open
        {
            continue;
        }
        match t.priority() {
            coxagent_domain::Priority::High => snap.bugs.high += 1,
            coxagent_domain::Priority::Medium => snap.bugs.medium += 1,
            coxagent_domain::Priority::Low => snap.bugs.low += 1,
        }
    }

    // PRs in review (the machine cannot merge its own work).
    snap.prs_in_review = state.open_prs.iter().map(|pr| pr.number).collect();

    // Recent shipped work (history is append-order, newest last).
    snap.shipped = state
        .history
        .iter()
        .rev()
        .take(SHIPPED_SHOWN)
        .map(|d| (d.version.to_string(), d.ticket.to_string(), d.title.clone()))
        .collect();

    // Waiting on a human.
    snap.parked = state
        .ticket_fail_attempts
        .iter()
        .filter(|(_, n)| **n >= 3)
        .map(|(id, _)| id.clone())
        .collect();
    snap.cost_holds = state.cost_holds.keys().cloned().collect();
    snap.on_hold = state
        .tickets
        .iter()
        .filter(|t| t.status() == coxagent_domain::Status::OnHold)
        .map(|t| t.id().to_string())
        .collect();
    snap.human_holds = state.human_holds.keys().copied().collect();

    snap.warnings = degraded_warnings(state, snap.heartbeat, now_secs);

    // The SA-designed digest, minus everything that leaks spend.
    snap.digest = sanitized_digest(state);
    snap
}

/// Classify the freshest worker heartbeat against the staleness threshold.
fn heartbeat_age(
    workers: &[coxagent_application::ports::outbound::WorkerEntry],
    now_secs: i64,
) -> Heartbeat {
    let newest = workers.iter().filter_map(|w| rfc3339_secs(&w.at)).max();
    let Some(at) = newest else {
        return Heartbeat::None;
    };
    let age = (now_secs - at).max(0);
    if age <= HEARTBEAT_STALE_SECS {
        Heartbeat::Fresh(age)
    } else {
        Heartbeat::Stale(age)
    }
}

/// Parse an RFC3339 timestamp to unix seconds; `None` when malformed.
fn rfc3339_secs(s: &str) -> Option<i64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(time::OffsetDateTime::unix_timestamp)
}

/// Human phrase for a heartbeat age ("just now" / "4 min ago" / "2 h ago").
fn age_phrase(secs: i64) -> String {
    if secs < 60 {
        "just now".to_owned()
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else {
        format!("{} h ago", secs / 3600)
    }
}

/// Why the page should carry a degraded/warning banner. Pure.
fn degraded_warnings(
    state: &coxagent_application::ProjectState,
    heartbeat: Option<Heartbeat>,
    now_secs: i64,
) -> Vec<String> {
    let mut out = Vec::new();
    // Heartbeats stopped: the loop is down or silent past the threshold — but
    // only when there is something that SHOULD be beating (a started project).
    let work_expected = state.cycle > 0 || !state.tickets.is_empty();
    if work_expected {
        match heartbeat {
            Some(Heartbeat::Stale(age)) => out.push(format!(
                "Runner heartbeat is stale (last seen {})",
                age_phrase(age)
            )),
            Some(Heartbeat::None) => {
                out.push("No runner heartbeat has been received".to_owned());
            }
            _ => {}
        }
    }
    // Last deploy attempt failed.
    if state.deploy.as_ref().is_some_and(|d| !d.ok) {
        out.push("The last deploy attempt failed".to_owned());
    }
    // Active tickets but nothing has shipped in a week.
    let active = state
        .tickets
        .iter()
        .any(|t| t.status() == coxagent_domain::Status::InProgress);
    if active {
        let shipped_recently = state
            .history
            .iter()
            .filter_map(|d| rfc3339_secs(&d.at))
            .any(|at| now_secs - at <= DEPLOY_STALE_SECS);
        if !shipped_recently {
            out.push("Tickets are in progress but nothing has deployed in 7 days".to_owned());
        }
    }
    out
}

/// [`coxagent_application::metrics::digest_markdown`] with the spend line (and
/// the redundant header) stripped — the ticket forbids internal spend on the
/// public page, and digest_markdown is the SA-designated summary source.
fn sanitized_digest(state: &coxagent_application::ProjectState) -> Vec<String> {
    coxagent_application::metrics::digest_markdown(state, &now_rfc3339())
        .lines()
        .filter(|line| !line.starts_with("**Daily digest") && !line.starts_with("- Spend"))
        .map(str::to_owned)
        .collect()
}

// --- Rendering --------------------------------------------------------------

/// Render the read-only status page. The token must never appear here — the
/// function does not even receive it. No scripts: the CSP forbids them, so
/// liveness comes from a meta refresh of the (already-secret) URL.
pub(super) fn render_share_page(
    state: &coxagent_application::ProjectState,
    p: &ProjectHandle,
    snap: &ShareSnapshot,
) -> String {
    let project = html_escape(&project_display_name(state, p));
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="refresh" content="60">
<title>{project} — status</title>
<style>
body{{font-family:Inter,-apple-system,system-ui,sans-serif;background:#0A0A0F;color:#F1F2F6;margin:0;padding:32px 16px;line-height:1.55}}
main{{max-width:920px;margin:0 auto}}
.kicker{{font-size:10px;font-weight:700;letter-spacing:.6px;text-transform:uppercase;color:#9A9DAB;margin:0 0 8px}}
h1{{font-size:22px;font-weight:800;letter-spacing:-.4px;margin:0 0 4px}}
.sub{{color:#9A9DAB;font-size:12.5px;margin:6px 0}}
.banner{{background:rgba(248,113,113,.08);border:1px solid rgba(248,113,113,.35);border-radius:14px;padding:16px 20px;margin:24px 0}}
.banner h2{{color:#F87171;font-size:12px;font-weight:700;text-transform:uppercase;letter-spacing:.6px;margin:0 0 8px}}
.banner li{{margin:4px 0}}
.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:16px;margin-top:24px}}
.card{{background:#15151D;border:1px solid rgba(255,255,255,.13);border-radius:14px;padding:20px}}
.card h2{{font-size:12px;font-weight:700;text-transform:uppercase;letter-spacing:.6px;color:#9A9DAB;margin:0 0 12px}}
.card ul{{margin:0;padding-left:18px}} .card li{{margin:6px 0;font-size:14px}}
code{{font-family:ui-monospace,Menlo,monospace;font-size:11.5px;background:#1B1B24;border-radius:6px;padding:2px 6px}}
.row{{display:flex;gap:8px;flex-wrap:wrap}}
.badge{{display:inline-block;border-radius:20px;padding:2px 8px;font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px}}
.badge.ok{{background:rgba(74,222,128,.14);color:#4ADE80}}
.badge.warn{{background:rgba(251,191,36,.14);color:#FBBF24}}
.badge.err{{background:rgba(248,113,113,.14);color:#F87171}}
footer{{color:#5E616C;font-size:11.5px;margin-top:24px}}
</style></head><body><main>
<p class="kicker">Read-only project status</p>
<h1>{project}</h1>
{banner}
<div class="grid">{cards}</div>
<footer>Auto-refreshes every 60 s · generated {generated}</footer>
</main></body></html>"#,
        project = project,
        banner = degraded_banner(snap),
        cards = cards_html(state, snap),
        generated = html_escape(&now_rfc3339()),
    )
}

/// The red "needs attention" banner, empty when the page is healthy.
fn degraded_banner(snap: &ShareSnapshot) -> String {
    use std::fmt::Write as _;
    if snap.warnings.is_empty() {
        return String::new();
    }
    let mut banner = String::from(r#"<section class="banner warn"><h2>Needs attention</h2><ul>"#);
    for w in &snap.warnings {
        let _ = write!(banner, "<li>{}</li>", html_escape(w));
    }
    banner.push_str("</ul></section>");
    banner
}

/// The page's status cards, in reading order: health, bugs, in-flight,
/// shipped, waiting-on-human, and the sanitized daily digest.
fn cards_html(state: &coxagent_application::ProjectState, snap: &ShareSnapshot) -> String {
    let mut cards = String::new();
    cards.push_str(&health_card(state, snap));
    cards.push_str(&bugs_card(snap));
    cards.push_str(&in_flight_card(snap));
    cards.push_str(&shipped_card(snap));
    cards.push_str(&waiting_card(snap));
    cards.push_str(&digest_card(snap));
    cards
}

/// The health card: heartbeat badge plus the persisted cycle/last-activity.
fn health_card(state: &coxagent_application::ProjectState, snap: &ShareSnapshot) -> String {
    use std::fmt::Write as _;
    let (health_label, health_class) = match snap.heartbeat {
        Some(Heartbeat::Fresh(_)) => ("Healthy", "ok"),
        Some(Heartbeat::Stale(_)) => ("Stale", "warn"),
        _ => ("Unknown", "warn"),
    };
    let last_beat = match snap.heartbeat {
        Some(Heartbeat::Fresh(age) | Heartbeat::Stale(age)) => age_phrase(age),
        _ => "no heartbeat seen".to_owned(),
    };
    let mut card = String::new();
    let _ = write!(
        card,
        r#"<section class="card"><h2>Runner health</h2>
<span class="badge {health_class}">{health_label}</span>
<p class="sub">last heartbeat: {last_beat}</p>
<p class="sub">cycle {} · last agent activity {}</p></section>"#,
        state.cycle,
        html_escape(&state.activity.last().map_or_else(
            || "none yet".to_owned(),
            |a| format!("{} — {}", a.at, a.action)
        )),
    );
    card
}

/// Open bugs by severity — counts only, never bug bodies.
fn bugs_card(snap: &ShareSnapshot) -> String {
    use std::fmt::Write as _;
    let mut card = String::new();
    let _ = write!(
        card,
        r#"<section class="card"><h2>Open bugs</h2>
<div class="row"><span class="badge err">{} high</span><span class="badge warn">{} medium</span><span class="badge ok">{} low</span></div>
<p class="sub">{} open total</p></section>"#,
        snap.bugs.high,
        snap.bugs.medium,
        snap.bugs.low,
        snap.bugs.total()
    );
    card
}

/// What each role is working right now, plus PRs sitting in review.
fn in_flight_card(snap: &ShareSnapshot) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    if snap.in_flight.is_empty() && snap.prs_in_review.is_empty() {
        body.push_str(r#"<p class="sub">Nothing in flight right now.</p>"#);
    } else {
        body.push_str("<ul>");
        for (role, ticket) in &snap.in_flight {
            let _ = write!(
                body,
                "<li><code>{}</code> working <code>{}</code></li>",
                html_escape(role),
                html_escape(ticket)
            );
        }
        for num in &snap.prs_in_review {
            let _ = write!(body, "<li>PR <code>#{num}</code> in review</li>");
        }
        body.push_str("</ul>");
    }
    format!(r#"<section class="card"><h2>In flight</h2>{body}</section>"#)
}

/// Recently shipped work — version, ticket id and title only.
fn shipped_card(snap: &ShareSnapshot) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    if snap.shipped.is_empty() {
        body.push_str(r#"<p class="sub">Nothing shipped yet.</p>"#);
    } else {
        body.push_str("<ul>");
        for (version, ticket, title) in &snap.shipped {
            let _ = write!(
                body,
                "<li><code>{}</code> {} — {}</li>",
                html_escape(version),
                html_escape(ticket),
                html_escape(title)
            );
        }
        body.push_str("</ul>");
    }
    format!(r#"<section class="card"><h2>Recently shipped</h2>{body}</section>"#)
}

/// Everything parked for a person: ids and counts, never reasons (a hold
/// reason is free text an operator wrote for operators).
fn waiting_card(snap: &ShareSnapshot) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    let waiting_total =
        snap.parked.len() + snap.cost_holds.len() + snap.on_hold.len() + snap.human_holds.len();
    if waiting_total == 0 {
        body.push_str(r#"<p class="sub">Nothing is waiting on a person.</p>"#);
    } else {
        body.push_str("<ul>");
        if !snap.parked.is_empty() {
            let _ = write!(
                body,
                "<li>{} ticket(s) parked after repeated failures: {}</li>",
                snap.parked.len(),
                html_escape(&snap.parked.join(", "))
            );
        }
        if !snap.cost_holds.is_empty() {
            let _ = write!(
                body,
                "<li>{} ticket(s) awaiting cost approval: {}</li>",
                snap.cost_holds.len(),
                html_escape(&snap.cost_holds.join(", "))
            );
        }
        if !snap.on_hold.is_empty() {
            let _ = write!(
                body,
                "<li>{} ticket(s) on hold: {}</li>",
                snap.on_hold.len(),
                html_escape(&snap.on_hold.join(", "))
            );
        }
        if !snap.human_holds.is_empty() {
            let _ = write!(
                body,
                "<li>{} PR(s) waiting for human review</li>",
                snap.human_holds.len()
            );
        }
        body.push_str("</ul>");
    }
    format!(r#"<section class="card"><h2>Waiting on a human</h2>{body}</section>"#)
}

/// The sanitized daily digest (spend already stripped by the read model).
fn digest_card(snap: &ShareSnapshot) -> String {
    use std::fmt::Write as _;
    let mut body = String::from("<ul>");
    for line in &snap.digest {
        let _ = write!(
            body,
            "<li>{}</li>",
            html_escape(line.trim_start_matches("- "))
        );
    }
    body.push_str("</ul>");
    format!(r#"<section class="card"><h2>Today</h2>{body}</section>"#)
}

/// Display name for the page header: the project's custom name, else the
/// hub-registered one.
fn project_display_name(state: &coxagent_application::ProjectState, p: &ProjectHandle) -> String {
    state.display_name.clone().unwrap_or_else(|| p.name.clone())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use coxagent_application::state::{DeployRecord, ProjectState};
    use coxagent_domain::{Priority, SemVer, Status, TicketId, TicketType};

    fn ticket(
        id: &str,
        ty: TicketType,
        status: Status,
        priority: Priority,
    ) -> coxagent_domain::Ticket {
        let ty_key = match ty {
            TicketType::Feature => "feature",
            TicketType::Bug => "bug",
            TicketType::Chore => "chore",
        };
        let status_key = match status {
            Status::Pending => "pending",
            Status::Ready => "ready",
            Status::InProgress => "in_progress",
            Status::Done => "done",
            Status::Documented => "documented",
            Status::Rejected => "rejected",
            Status::Open => "open",
            Status::Fixed => "fixed",
            Status::Verified => "verified",
            Status::OnHold => "on_hold",
        };
        let prio_key = match priority {
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
        };
        let json = serde_json::json!({
            "id": id, "type": ty_key, "title": "t", "description": "",
            "priority": prio_key, "complexity": "small", "status": status_key,
            "has_ui": false, "design": {"technical": null, "ux": null},
            "parent_id": null, "depends_on": []
        });
        serde_json::from_value(json).expect("ticket")
    }

    fn worker(at: &str) -> coxagent_application::ports::outbound::WorkerEntry {
        serde_json::from_value(serde_json::json!({
            "worker": "op@mac", "role": "dev_feature", "ticket": "F001", "at": at
        }))
        .expect("worker entry")
    }

    const NOW: i64 = 1_800_000_000; // fixed unix seconds for determinism

    #[test]
    fn snapshot_sanitizes_away_spend_and_internal_fields() {
        let mut state = ProjectState::default();
        state.spend.total_cost_usd = 123.45;
        state.spend.runs = 42;
        state.tickets.push(ticket(
            "B001",
            TicketType::Bug,
            Status::Open,
            Priority::High,
        ));
        state.history.push(DeployRecord {
            version: SemVer::new(1, 2, 3),
            ticket: TicketId::new("F001").unwrap(),
            title: "login page".to_owned(),
            at: "2026-08-01T00:00:00Z".to_owned(),
        });
        let snap = share_snapshot(&state, &[], NOW);
        let rendered: String = snap.digest.join("\n");
        assert!(
            !rendered.contains("Spend") && !rendered.contains('$'),
            "spend must never reach the public digest: {rendered}"
        );
        // The safe aggregates are present…
        assert_eq!(snap.bugs.high, 1);
        assert_eq!(snap.shipped.len(), 1);
        // …and nothing that is not on the allow-list even has a field.
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("design") && !json.contains("estimate"));
    }

    #[test]
    fn heartbeat_badge_tracks_freshness_threshold() {
        let fresh = worker(&rfc3339_from(NOW - 60));
        assert_eq!(heartbeat_age(&[fresh], NOW), Heartbeat::Fresh(60));

        let stale = worker(&rfc3339_from(NOW - HEARTBEAT_STALE_SECS - 1));
        assert!(matches!(heartbeat_age(&[stale], NOW), Heartbeat::Stale(_)));

        assert_eq!(heartbeat_age(&[], NOW), Heartbeat::None);
        // An unparsable timestamp is ignored rather than trusted as fresh.
        let garbage = worker("not-a-timestamp");
        assert_eq!(heartbeat_age(&[garbage], NOW), Heartbeat::None);
    }

    #[test]
    fn degraded_flags_stale_heartbeat_failed_deploy_and_stalled_deploys() {
        let mut state = ProjectState {
            cycle: 5,
            ..ProjectState::default()
        };
        // Stale heartbeat while a project is underway → warning.
        let warnings = degraded_warnings(&state, Some(Heartbeat::Stale(9_000)), NOW);
        assert!(warnings.iter().any(|w| w.contains("stale")), "{warnings:?}");

        // A failed last deploy → warning.
        state.deploy = Some(coxagent_application::state::DeployStatus {
            at: "2026-08-01T00:00:00Z".to_owned(),
            ok: false,
            summary: "boom".to_owned(),
            commit_sha: None,
            health_check: None,
        });
        let warnings = degraded_warnings(&state, Some(Heartbeat::Fresh(10)), NOW);
        assert!(
            warnings.iter().any(|w| w.contains("deploy attempt failed")),
            "{warnings:?}"
        );

        // Active tickets + no deploy in 7 days → warning; a fresh deploy clears it.
        state.deploy = None;
        state.tickets.push(ticket(
            "F001",
            TicketType::Feature,
            Status::InProgress,
            Priority::Medium,
        ));
        let warnings = degraded_warnings(&state, Some(Heartbeat::Fresh(10)), NOW);
        assert!(
            warnings.iter().any(|w| w.contains("7 days")),
            "{warnings:?}"
        );
        state.history.push(DeployRecord {
            version: SemVer::new(0, 0, 1),
            ticket: TicketId::new("F001").unwrap(),
            title: "t".to_owned(),
            at: rfc3339_from(NOW - 60),
        });
        let warnings = degraded_warnings(&state, Some(Heartbeat::Fresh(10)), NOW);
        assert!(
            !warnings.iter().any(|w| w.contains("7 days")),
            "{warnings:?}"
        );
    }

    #[test]
    fn waiting_on_human_lists_parked_cost_and_hold_ids() {
        let mut state = ProjectState::default();
        state.ticket_fail_attempts.insert("CXC-B001".to_owned(), 3);
        state.cost_holds.insert("CXC-F002".to_owned(), 9.0);
        state.tickets.push(ticket(
            "CXC-F003",
            TicketType::Feature,
            Status::OnHold,
            Priority::Low,
        ));
        state.human_holds.insert(7, "needs eyes".to_owned());
        let snap = share_snapshot(&state, &[], NOW);
        assert_eq!(snap.parked, vec!["CXC-B001"]);
        assert_eq!(snap.cost_holds, vec!["CXC-F002"]);
        assert_eq!(snap.on_hold, vec!["CXC-F003"]);
        assert_eq!(snap.human_holds, vec![7]);
    }

    #[test]
    fn rendered_page_escapes_names_and_never_shows_spend() {
        let mut state = ProjectState {
            spend: coxagent_application::state::Spend {
                total_cost_usd: 99.0,
                ..Default::default()
            },
            ..ProjectState::default()
        };
        state.tickets.push(ticket(
            "F001",
            TicketType::Feature,
            Status::InProgress,
            Priority::Medium,
        ));
        let handle = project_handle_named("Demo <b>project</b>");
        let snap = share_snapshot(&state, &[], NOW);
        // The renderer never receives the token — share_page_ep owns it — so
        // the router-level test proves end to end that no token lands in the
        // HTML; here we pin the content policy itself.
        let html = render_share_page(&state, &handle, &snap);
        assert!(
            !html.contains("Spend") && !html.contains("$99"),
            "spend leaked"
        );
        assert!(
            !html.contains("<b>"),
            "project name must be HTML-escaped: {}",
            &html[..200.min(html.len())]
        );
        assert!(html.contains("Demo &lt;b&gt;project&lt;/b&gt;"));
        // No scripts: the page's CSP forbids them and nothing needs them.
        assert!(!html.contains("<script"), "share page must be script-free");
    }

    fn project_handle_named(name: &str) -> ProjectHandle {
        ProjectHandle {
            id: "demo".to_owned(),
            name: name.to_owned(),
            alias: "demo".to_owned(),
            store: std::sync::Arc::new(store_rpc_test_support::CountingStore::seeded(
                ProjectState::default(),
            )),
            runner: std::sync::Arc::new(coxagent_application::use_cases::RunnerHandle::default()),
            config_path: std::path::PathBuf::from("/tmp/coxagent.json"),
            engine: std::sync::Arc::new(store_rpc_test_support::UnusedEngine),
            work_dir: std::path::PathBuf::from("/tmp"),
            budget: std::sync::Arc::new(std::sync::Mutex::new(
                coxagent_application::config::BudgetCaps::default(),
            )),
            context_path: std::path::PathBuf::from("/tmp/project_context.md"),
            forge: None,
            deploy: None,
            storage: None,
            files: None,
            deps_discovery: None,
        }
    }

    /// Fixed RFC3339 for a unix-seconds instant (test-only helper).
    fn rfc3339_from(secs: i64) -> String {
        time::OffsetDateTime::from_unix_timestamp(secs)
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    }
}
