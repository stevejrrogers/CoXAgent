//! TDD tests for CXA-F241 — Per-gate evidence forensics inspector for
//! verification reviewers.
//!
//! Written before implementation so the ticket's acceptance criteria are
//! pinned as executable tests over the state/domain types the codebase has
//! today. Every fixture is built through the legal domain aggregate API and
//! the exact records the production endpoints already write (inbox.rs
//! send-back/verify, run_test.rs human-verify evidence, qa_evidence.rs
//! capture) — no server, no host harness, no network port, no fabricated
//! persisted shape.
//!
//! The forensics derivation lives in `coxagent_application::forensics` — a
//! pure module over `ProjectState`/`Evidence` (the application layer is its
//! home — it feeds the ticket-detail payload and the Forensics tab), exactly
//! the `dependency_radar` precedent (CXA-F237): pure derivation over loaded
//! state, wired into `GET /api/projects/:pid/ticket/:id` additively.
//!
//! AC → test map:
//! - AC1 (a reviewer inspecting any verified/fixed ticket sees its per-DoD-gate
//!   evidence items grouped by gate, each showing kind, label, capture time and
//!   provenance):
//!   [`ac1_gate_spine_reconstructs_the_ready_gate_decision`],
//!   [`ac1_gate_spine_reconstructs_verify_gate_decisions_chronologically`],
//!   [`ac1_evidence_groups_under_its_supporting_gate_in_spine_order`],
//!   [`ac1_each_grouped_item_carries_kind_label_capture_time_and_provenance`],
//!   [`ac1_an_item_linked_to_verify_never_groups_under_a_ready_gate`]
//! - AC2 (clicking an api/test evidence item displays its full captured
//!   request/response text inline without leaving the page):
//!   [`ac2_api_and_test_items_expose_their_full_captured_request_response_text`]
//! - AC3 (clicking a screenshot item renders the image served through an
//!   authenticated endpoint; if its artifact is missing on disk it shows
//!   'missing artifact' explicitly instead of failing silently or fabricating
//!   content):
//!   [`ac3_a_screenshot_item_renders_through_the_authenticated_media_endpoint`],
//!   [`ac3_a_missing_artifact_is_shown_explicitly_and_fabricates_no_content`]
//! - AC4 (for tickets that went through >=1 send-back cycle, the same gate's
//!   before/after evidence can be viewed side by side using CXA-F199 re-entry
//!   deltas): NOT ENCODED — DESIGN GAP, see below.
//! - AC5 (waived evidence renders as an explicit waiver stating who granted it
//!   and why; items lacking a capture commit render as 'provenance unknown'):
//!   [`ac5_a_waived_item_renders_an_explicit_waiver_naming_who_and_why`],
//!   [`ac5_an_item_lacking_a_capture_commit_renders_provenance_unknown`]
//! - Guard (the design's backward-compat requirement — "existing persisted
//!   Evidence records load unchanged via #[serde(default)]", which is what
//!   makes AC1 hold for EVERY verified/fixed ticket, old ones included):
//!   [`guard_legacy_evidence_without_source_gates_or_actor_loads_losslessly`]
//!
//! DESIGN GAP — AC4 NOT ENCODED, AND WHY (evidence, not judgement): AC4 pins
//! the side-by-side before/after view to "CXA-F199 re-entry deltas". CXA-F199
//! does not exist in this codebase: no `F199` symbol in any crate, no entry in
//! PLAN.md, no type, state field or doc; the only trace is an unimplemented
//! design stub (`.coxagent/design/CXA-F199/ticket-evidence-cycle.svg`, 211
//! bytes, header "Ticket detail modal: DoD Evidence and Cycle history") and
//! its generator. A "re-entry delta" therefore has no shape the tests could
//! name: no per-visit evidence boundary, no cycle counter, no delta record —
//! and the F241 design itself (data_changes, test_plan) adds only
//! `source_gates`/`actor` on Evidence plus the GateTransition spine, with no
//! before/after pairing in its own test plan. Encoding AC4 would mean
//! fabricating the delta semantics — exactly what the house rules forbid. The
//! send-back DATA a future delta needs does exist and is fixture-proven here
//! (a VerifySendBack record precedes the VerifyPass in the AC1 spine fixture);
//! what is missing is the F199 delta definition. ASK SA: specify the CXA-F199
//! re-entry delta shape (per-visit evidence boundary) before AC4 is built.
//!
//! NAMES, AND WHERE THEY COME FROM (pinned so implementer and reviewer share
//! one contract; every name is the design's own word or a house precedent):
//! * `forensics` — the module: the ticket's own word ("evidence forensics
//!   inspector"; design: "Forensics tab", "Forensics endpoint"), housed in the
//!   application layer like `dependency_radar` (CXA-F237), which this design
//!   cites as the lazy-derivation pattern.
//! * `GateTransition` — the design names the record; fields are the
//!   api_contract's own keys: `gate_id` (`"ready"` | `"verify"` |
//!   `"deploy-health"`), `status_from`, `status_to`, `actor_role`,
//!   `decided_at_ms` (number; i64 epoch millis, the house epoch width).
//! * `gate_spine(&ProjectState, &TicketId)` — the design's own noun
//!   ("Provenance spine", "ordered chrono spine"); signature follows
//!   `dependency_radar::blocked_by(&state, t.id())`.
//! * `group_by_gate(&[GateTransition], &[Evidence]) -> Vec<GateEvidenceGroup>`
//!   — the design test plan's own item ("assert forensics groups it under the
//!   Verify transition and NOT under Ready"); a group is the design's
//!   "evidence records grouped under their supporting gate".
//! * `inline_text(&Evidence) -> Option<&str>` — AC2's "full captured
//!   request/response text inline": Some(detail) for the api/test kinds, None
//!   for kinds that render as image or waiver.
//! * `screenshot_render(&Evidence, &BTreeSet<String>) -> ScreenshotRender` —
//!   AC3's two branches: `Image(url)` renders through the authenticated media
//!   endpoint (the URL production already captures,
//!   `/api/projects/:pid/media/:file`, served under the hub's auth middleware)
//!   when the artifact is present; `Missing` is the explicit 'missing
//!   artifact' state carrying NO url — failing loud, never fabricating. The
//!   `BTreeSet` is the storage adapter's report (media keys present on disk,
//!   the `proj/{pid}/{file}` key shape qa_evidence.rs and media_ep write) —
//!   existence is adapter data, the decision stays pure (hexagonal_gate.rs).
//! * `waiver(&Evidence) -> Option<Waiver>` with `Waiver { granted_by, reason }`
//!   — AC5's "explicit waiver stating who granted it and why": who is the
//!   design's new `actor` field, why is the captured `detail`.
//! * `provenance(&Evidence) -> Provenance` +
//!   `provenance_label(&Provenance) -> String` — AC1's per-item provenance and
//!   AC5's fallback: `Provenance::Unknown` renders as the literal
//!   "provenance unknown". NOTE: no evidence record in today's model can carry
//!   a capture commit (the type has no such field and the design adds none),
//!   so `Unknown` is the only branch real data can reach — the test pins
//!   exactly that, honestly.
//! * `Evidence` gains `#[serde(default)] source_gates: Vec<String>` and
//!   `#[serde(default)] actor: String` — the design's own field names
//!   (approach §2), which is what makes old persisted records load losslessly.
//!
//! REQUIRED SURFACE this suite compiles against:
//! - `coxagent_application::forensics` (registered in lib.rs) with the items
//!   above; `GateTransition`, `GateEvidenceGroup`, `ScreenshotRender`,
//!   `Provenance`, `Waiver` derive/Clone/Debug/PartialEq as every state record
//!   does, `GateTransition` also `Serialize + Deserialize` (it is the wire
//!   payload's `gates` array).
//! - `coxagent_application::state::Evidence` extended with the two
//!   serde-default fields above.
//!
//! RED STATE: none of that surface exists yet, so this target fails to
//! compile — for a declare-a-surface ticket the unresolved names ARE the
//! missing behaviour, exactly as a failing assertion is for a behaviour inside
//! an existing type (the artifact_registry_f224_tdd.rs precedent). Once the
//! surface lands, the target compiles and every test below fails only if the
//! behaviour it pins is wrong or missing.
//!
//! The rendered pixels themselves (click → panel opens, side-by-side layout)
//! are gated end-to-end by the e2e playwright suite per AGENTS.md; the data
//! those pixels consume is pinned here over the real types.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use coxagent_application::forensics::{
    gate_spine, group_by_gate, inline_text, provenance, provenance_label, screenshot_render,
    waiver, GateTransition, Provenance, ScreenshotRender,
};
use coxagent_application::state::{
    ActivityEntry, Comment, Evidence, InterventionRecord, ProjectState,
};
use coxagent_domain::{
    Complexity, InterventionKind, Priority, Role, Status, TechnicalDesign, Ticket, TicketId,
    TicketType,
};

// ---------------------------------------------------------------------------
// Fixtures — built ONLY through the domain aggregate's legal API and the
// exact records the production gate endpoints already write.
// ---------------------------------------------------------------------------

fn tid(s: &str) -> TicketId {
    TicketId::new(s).expect("valid ticket id")
}

/// A bug walked to `Fixed` along the only legal edges (`Open -> InProgress ->
/// Fixed`, DEV-BUG's edges) — the state a verify-gate reviewer inspects.
fn bug_at_fixed(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Bug,
        format!("bug {id}"),
        "fixture",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("claim");
    t.transition_to(Role::DevBug, Status::Fixed).expect("fix");
    t
}

/// A feature walked to `Ready` (`Pending -> Ready` needs the technical design;
/// the human ready-gate acts as `Role::User`).
fn feature_at_ready(id: &str) -> Ticket {
    let mut t = Ticket::new(
        tid(id),
        TicketType::Feature,
        format!("feature {id}"),
        "fixture",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    t.set_technical_design(Role::Sa, TechnicalDesign::default())
        .expect("SA attaches the design");
    t.transition_to(Role::User, Status::Ready).expect("ready");
    t
}

/// An extended evidence record — the shape the design mandates going forward
/// (`source_gates` + `actor` beside today's four fields).
fn evidence(
    kind: &str,
    label: &str,
    detail: &str,
    at: &str,
    gates: &[&str],
    actor: &str,
) -> Evidence {
    Evidence {
        kind: kind.to_owned(),
        label: label.to_owned(),
        detail: detail.to_owned(),
        at: at.to_owned(),
        source_gates: gates.iter().map(|g| (*g).to_owned()).collect(),
        actor: actor.to_owned(),
    }
}

/// Epoch milliseconds for a whole-second `YYYY-MM-DDTHH:MM:SSZ` stamp — the
/// exact format `now_rfc3339` writes (no fractional form appears in these
/// fixtures). Pure expected-value math (days-from-civil), so the tests can
/// pin `decided_at_ms` against the record's `at` instead of trusting it.
fn epoch_ms(stamp: &str) -> i64 {
    let y: i64 = stamp[0..4].parse().expect("year");
    let m: i64 = stamp[5..7].parse().expect("month");
    let d: i64 = stamp[8..10].parse().expect("day");
    let hh: i64 = stamp[11..13].parse().expect("hour");
    let mm: i64 = stamp[14..16].parse().expect("minute");
    let ss: i64 = stamp[17..19].parse().expect("second");
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + hh * 3_600 + mm * 60 + ss) * 1_000
}

/// One gate decision as the ledger persists it (`InterventionRecord`), with
/// the surrounding activity + comment the same endpoint writes — so the spine
/// fold has every source the design says it folds over.
fn intervention(kind: InterventionKind, ticket: &str, by: &str, at: &str) -> InterventionRecord {
    InterventionRecord {
        kind,
        ticket: ticket.to_owned(),
        area: Some(TicketType::Bug),
        by: by.to_owned(),
        at: at.to_owned(),
    }
}

fn activity(at: &str, agent: &str, action: &str, ticket: &str) -> ActivityEntry {
    ActivityEntry {
        at: at.to_owned(),
        agent: agent.to_owned(),
        action: action.to_owned(),
        ticket: Some(ticket.to_owned()),
    }
}

fn comment(id: &str, at: &str, author: &str, body: &str, ticket: &str) -> Comment {
    Comment {
        id: id.to_owned(),
        at: at.to_owned(),
        author: author.to_owned(),
        by: String::new(),
        body: body.to_owned(),
        ticket: Some(ticket.to_owned()),
        attachments: Vec::new(),
        reactions: Vec::new(),
    }
}

/// A bug through ONE send-back cycle and a final human verify pass — the
/// records are exactly what `POST …/send-back` and `POST …/verify` write
/// (inbox.rs: transition as `Role::User`, activity, comment, intervention).
/// The bug ends `Verified`: the ticket a verification reviewer inspects.
fn verified_bug_after_a_send_back() -> ProjectState {
    const ID: &str = "CXA-B241";
    let mut s = ProjectState {
        tickets: vec![bug_at_fixed(ID)],
        ..ProjectState::default()
    };
    // Send-back #1 — inbox.rs send_back_ep: Fixed -> Open by the human gate.
    s.tickets[0]
        .transition_to(Role::User, Status::Open)
        .expect("send back");
    s.activity.push(activity(
        "2026-08-01T10:00:00Z",
        "USER",
        "verification refused",
        ID,
    ));
    s.comments.push(comment(
        "c1",
        "2026-08-01T10:00:00Z",
        "rev",
        "↩️ CXA-B241 sent back by @rev: the fix is not demonstrated.",
        ID,
    ));
    s.governance_interventions.push(intervention(
        InterventionKind::VerifySendBack,
        ID,
        "rev",
        "2026-08-01T10:00:00Z",
    ));
    // The dev re-fixes (the legal edges back to Fixed)…
    s.tickets[0]
        .transition_to(Role::DevBug, Status::InProgress)
        .expect("re-claim");
    s.tickets[0]
        .transition_to(Role::DevBug, Status::Fixed)
        .expect("re-fix");
    // …and the human verify pass — inbox.rs human_transition writes.
    s.tickets[0]
        .transition_to(Role::User, Status::Verified)
        .expect("verify pass");
    s.activity.push(activity(
        "2026-08-01T11:00:00Z",
        "USER",
        "rev moved ticket to verified",
        ID,
    ));
    s.comments.push(comment(
        "c2",
        "2026-08-01T11:00:00Z",
        "USER",
        "🧑‍⚖️ @rev approved CXA-B241 → verified.",
        ID,
    ));
    s.governance_interventions.push(intervention(
        InterventionKind::VerifyPass,
        ID,
        "rev",
        "2026-08-01T11:00:00Z",
    ));
    // The evidence the gate captured (linked to its gate going forward).
    s.ticket_evidence
        .entry(ID.to_owned())
        .or_default()
        .push(evidence(
            "test",
            "REGRESSION TEST",
            "PASS on current master, verdict rendered by human QA; regression test \
             fails on pre-fix code and reproduces cleanly; root cause fixed at source.",
            "2026-08-01T11:00:01Z",
            &["verify"],
            "rev",
        ));
    s
}

// ---------------------------------------------------------------------------
// AC1 — the provenance spine: per-gate decisions, chronological, attributed.
// ---------------------------------------------------------------------------

#[test]
fn ac1_gate_spine_reconstructs_the_ready_gate_decision() {
    // The ready gate: a designed feature approved Pending -> Ready by a human
    // (the design's own attribution example: "Role::User for PO approve via
    // /ready inbox action"). The spine's entry is the api_contract's record:
    // gate_id "ready", the status pair, the acting role, the decided time.
    const ID: &str = "CXA-F241";
    let mut s = ProjectState {
        tickets: vec![feature_at_ready(ID)],
        ..ProjectState::default()
    };
    s.governance_interventions.push(InterventionRecord {
        kind: InterventionKind::ReadyApprove,
        ticket: ID.to_owned(),
        area: Some(TicketType::Feature),
        by: "po".to_owned(),
        at: "2026-08-01T09:00:00Z".to_owned(),
    });
    s.activity.push(activity(
        "2026-08-01T09:00:00Z",
        "USER",
        "po moved ticket to ready",
        ID,
    ));

    let spine = gate_spine(&s, &tid(ID));
    assert_eq!(
        spine,
        vec![GateTransition {
            gate_id: "ready".to_owned(),
            status_from: "pending".to_owned(),
            status_to: "ready".to_owned(),
            actor_role: "user".to_owned(),
            decided_at_ms: epoch_ms("2026-08-01T09:00:00Z"),
        }],
        "the ready gate decision reconstructs with its status pair, actor role \
         and capture time"
    );
}

#[test]
fn ac1_gate_spine_reconstructs_verify_gate_decisions_chronologically() {
    // Both verify-gate decisions of a sent-back-then-passed bug, in time
    // order: the send-back (Fixed -> Open) FIRST, the pass (Fixed -> Verified)
    // second, each at its recorded decision time.
    let s = verified_bug_after_a_send_back();
    let spine = gate_spine(&s, &tid("CXA-B241"));

    assert_eq!(spine.len(), 2, "two verify-gate decisions were recorded");
    assert_eq!(
        spine[0].gate_id, "verify",
        "send-back is a verify-gate event"
    );
    assert_eq!(spine[0].status_from, "fixed", "the fix was on the table");
    assert_eq!(spine[0].status_to, "open", "send-back reopens the bug");
    assert_eq!(spine[0].decided_at_ms, epoch_ms("2026-08-01T10:00:00Z"));
    assert_eq!(spine[1].gate_id, "verify", "the pass is the same gate");
    assert_eq!(spine[1].status_from, "fixed");
    assert_eq!(spine[1].status_to, "verified");
    assert_eq!(spine[1].decided_at_ms, epoch_ms("2026-08-01T11:00:00Z"));
    assert!(
        spine[0].decided_at_ms < spine[1].decided_at_ms,
        "the spine is chronological: send-back before the pass"
    );
    // The human gate acted as the user role on both decisions (inbox.rs
    // transitions as Role::User) — the provenance a reviewer reads.
    assert_eq!(spine[0].actor_role, "user");
    assert_eq!(spine[1].actor_role, "user");
}

// ---------------------------------------------------------------------------
// AC1 — the grouping: evidence under its supporting gate.
// ---------------------------------------------------------------------------

#[test]
fn ac1_evidence_groups_under_its_supporting_gate_in_spine_order() {
    let s = verified_bug_after_a_send_back();
    let spine = gate_spine(&s, &tid("CXA-B241"));
    let evidence_list = &s.ticket_evidence["CXA-B241"];

    let groups = group_by_gate(&spine, evidence_list);
    assert_eq!(
        groups.len(),
        spine.len(),
        "one group per spine decision, in spine order"
    );
    let carrying: Vec<&GateTransition> = groups
        .iter()
        .filter(|g| g.evidence.iter().any(|e| e.label == "REGRESSION TEST"))
        .map(|g| &g.gate)
        .collect();
    assert_eq!(
        carrying.len(),
        1,
        "the item groups under exactly one gate decision"
    );
    assert_eq!(
        carrying[0].gate_id, "verify",
        "an item linked to gate \"verify\" surfaces under the verify gate"
    );
}

#[test]
fn ac1_each_grouped_item_carries_kind_label_capture_time_and_provenance() {
    let s = verified_bug_after_a_send_back();
    let spine = gate_spine(&s, &tid("CXA-B241"));
    let groups = group_by_gate(&spine, &s.ticket_evidence["CXA-B241"]);

    let item = groups
        .iter()
        .flat_map(|g| g.evidence.iter())
        .find(|e| e.label == "REGRESSION TEST")
        .expect("the grouped item is the real record, fields intact");
    assert_eq!(item.kind, "test", "kind shown");
    assert_eq!(item.label, "REGRESSION TEST", "label shown");
    assert_eq!(
        item.at, "2026-08-01T11:00:01Z",
        "capture time shown as recorded"
    );
    assert_eq!(item.actor, "rev", "provenance: who attached it");
    assert_eq!(
        item.source_gates,
        vec!["verify".to_owned()],
        "provenance: which gate it proved"
    );
}

#[test]
fn ac1_an_item_linked_to_verify_never_groups_under_a_ready_gate() {
    // The design test plan's own linkage case, over a spine holding BOTH
    // gates (GateTransition literals — the group function's own input type):
    // a verify-linked item lands under the verify transition and NOT under
    // ready; a ready-linked (waived) record surfaces under ITS gate.
    let spine = vec![
        GateTransition {
            gate_id: "ready".to_owned(),
            status_from: "pending".to_owned(),
            status_to: "ready".to_owned(),
            actor_role: "user".to_owned(),
            decided_at_ms: epoch_ms("2026-08-01T09:00:00Z"),
        },
        GateTransition {
            gate_id: "verify".to_owned(),
            status_from: "fixed".to_owned(),
            status_to: "verified".to_owned(),
            actor_role: "user".to_owned(),
            decided_at_ms: epoch_ms("2026-08-01T11:00:00Z"),
        },
    ];
    let evidence_list = vec![
        evidence(
            "api",
            "live request/response",
            "GET /api/health\nHTTP 200",
            "2026-08-01T10:59:00Z",
            &["verify"],
            "TEST",
        ),
        evidence(
            "waived",
            "screenshot unavailable",
            "no headless browser/storage on this host",
            "2026-08-01T08:59:00Z",
            &["ready"],
            "TEST",
        ),
    ];

    let groups = group_by_gate(&spine, &evidence_list);
    let ready = groups
        .iter()
        .find(|g| g.gate.gate_id == "ready")
        .expect("the ready decision is a group");
    let verify = groups
        .iter()
        .find(|g| g.gate.gate_id == "verify")
        .expect("the verify decision is a group");
    assert!(
        ready.evidence.iter().all(|e| e.kind != "api"),
        "the verify-linked api item never groups under the ready gate"
    );
    assert!(
        verify.evidence.iter().all(|e| e.kind != "waived"),
        "the ready-linked waived record never groups under the verify gate"
    );
    assert!(verify.evidence.iter().any(|e| e.kind == "api"));
    assert!(ready.evidence.iter().any(|e| e.kind == "waived"));
}

// ---------------------------------------------------------------------------
// AC2 — the full captured request/response, inline.
// ---------------------------------------------------------------------------

#[test]
fn ac2_api_and_test_items_expose_their_full_captured_request_response_text() {
    // The capture qa_evidence.rs writes for a non-UI ticket: request line,
    // status line, body — the WHOLE captured text, nothing clipped further.
    let captured = "GET http://127.0.0.1:4123/api/health\nHTTP 200\n{\"ok\":true,\
                    \"version\":\"2.28.0\",\"uptime_s\":812}\n--\nGET \
                    http://127.0.0.1:4123/\nHTTP 200\n<!doctype html>…";
    let api = evidence(
        "api",
        "live request/response",
        captured,
        "2026-08-01T11:00:01Z",
        &["verify"],
        "TEST",
    );
    let test = evidence(
        "test",
        "REGRESSION TEST",
        "PASS on current master; reproduces cleanly; root cause fixed at source.",
        "2026-08-01T11:00:02Z",
        &["verify"],
        "rev",
    );

    assert_eq!(
        inline_text(&api),
        Some(captured),
        "an api item's inline body IS the full captured request/response"
    );
    assert_eq!(
        inline_text(&test),
        Some(test.detail.as_str()),
        "a test item's inline body is its full captured text"
    );
    // The kinds that render as image or waiver carry no inline text body.
    let shot = evidence(
        "screenshot",
        "deployed UI screenshot",
        "/api/projects/TL/media/evidence-CXA-B241.png",
        "2026-08-01T11:00:03Z",
        &["verify"],
        "TEST",
    );
    let waived = evidence(
        "waived",
        "screenshot unavailable",
        "no headless browser/storage on this host",
        "2026-08-01T11:00:04Z",
        &["verify"],
        "TEST",
    );
    assert_eq!(inline_text(&shot), None, "a screenshot renders as an image");
    assert_eq!(inline_text(&waived), None, "a waiver renders as a waiver");
}

// ---------------------------------------------------------------------------
// AC3 — screenshots: authenticated endpoint, honest about missing artifacts.
// ---------------------------------------------------------------------------

#[test]
fn ac3_a_screenshot_item_renders_through_the_authenticated_media_endpoint() {
    // The URL production captures (qa_evidence.rs): the project media route,
    // which the hub serves behind its auth middleware — the forensics view
    // renders THAT url as the image source, never a fabricated path.
    let shot = evidence(
        "screenshot",
        "deployed UI screenshot",
        "/api/projects/TL/media/evidence-CXA-B241.png",
        "2026-08-01T11:00:03Z",
        &["verify"],
        "TEST",
    );
    // The storage adapter reports the artifact present (the `proj/{pid}/{file}`
    // key shape the capture and media_ep both use).
    let stored: BTreeSet<String> = BTreeSet::from(["proj/TL/evidence-CXA-B241.png".to_owned()]);

    match screenshot_render(&shot, &stored) {
        ScreenshotRender::Image(url) => {
            assert_eq!(
                url, shot.detail,
                "the rendered source is the captured authenticated media url"
            );
            assert!(
                url.starts_with("/api/projects/") && url.contains("/media/"),
                "the url is the project media route the auth middleware guards"
            );
        }
        ScreenshotRender::Missing => panic!("the artifact is on disk — it must render"),
    }
}

#[test]
fn ac3_a_missing_artifact_is_shown_explicitly_and_fabricates_no_content() {
    // Same captured url, but the storage adapter reports the key GONE (the
    // blob was evicted / the volume reset): the view says so explicitly and
    // carries no url to render — no silent broken image, no invented content.
    let shot = evidence(
        "screenshot",
        "deployed UI screenshot",
        "/api/projects/TL/media/evidence-CXA-B241.png",
        "2026-08-01T11:00:03Z",
        &["verify"],
        "TEST",
    );
    let stored: BTreeSet<String> = BTreeSet::new();

    match screenshot_render(&shot, &stored) {
        ScreenshotRender::Missing => {
            // The explicit state itself is the assertion: the caller renders
            // 'missing artifact' from it — there is nothing else to show.
        }
        ScreenshotRender::Image(url) => {
            panic!("a missing artifact must not render an image, got url {url}")
        }
    }
}

// ---------------------------------------------------------------------------
// AC5 — waivers are explicit; missing capture provenance is explicit.
// ---------------------------------------------------------------------------

#[test]
fn ac5_a_waived_item_renders_an_explicit_waiver_naming_who_and_why() {
    // The waiver production writes when the host cannot capture
    // (qa_evidence.rs): kind "waived", the reason in the detail — and going
    // forward the actor names who granted it.
    let rec = evidence(
        "waived",
        "screenshot unavailable",
        "no headless browser/storage on this host, or the app did not render",
        "2026-08-01T11:00:05Z",
        &["verify"],
        "TEST",
    );
    let w = waiver(&rec).expect("a waived record IS a waiver");
    assert_eq!(w.granted_by, "TEST", "the waiver names who granted it");
    assert_eq!(
        w.reason, "no headless browser/storage on this host, or the app did not render",
        "the waiver states why"
    );
    // Explicit means ONLY waived records are waivers.
    let api = evidence(
        "api",
        "live request/response",
        "GET /api/health\nHTTP 200",
        "2026-08-01T11:00:01Z",
        &["verify"],
        "TEST",
    );
    assert!(waiver(&api).is_none(), "a captured record is not a waiver");
}

#[test]
fn ac5_an_item_lacking_a_capture_commit_renders_provenance_unknown() {
    // No evidence record in this model carries a capture commit, so every
    // real record's provenance is the explicit unknown — never a guess.
    for rec in [
        evidence(
            "api",
            "live request/response",
            "GET /api/health\nHTTP 200",
            "2026-08-01T11:00:01Z",
            &["verify"],
            "TEST",
        ),
        evidence(
            "test",
            "REGRESSION TEST",
            "PASS on current master",
            "2026-08-01T11:00:02Z",
            &["verify"],
            "rev",
        ),
        evidence(
            "screenshot",
            "deployed UI screenshot",
            "/api/projects/TL/media/evidence-CXA-B241.png",
            "2026-08-01T11:00:03Z",
            &["verify"],
            "TEST",
        ),
        evidence(
            "waived",
            "screenshot unavailable",
            "no headless browser/storage on this host",
            "2026-08-01T11:00:05Z",
            &["verify"],
            "TEST",
        ),
    ] {
        assert!(
            matches!(provenance(&rec), Provenance::Unknown),
            "a record without a capture commit has unknown provenance ({})",
            rec.label
        );
        assert_eq!(
            provenance_label(&provenance(&rec)),
            "provenance unknown",
            "the rendered wording is explicit"
        );
    }
}

// ---------------------------------------------------------------------------
// Guard — old persisted evidence loads losslessly (the design's backward
// compatibility requirement, which makes AC1 hold for every existing
// verified/fixed ticket).
// ---------------------------------------------------------------------------

#[test]
fn guard_legacy_evidence_without_source_gates_or_actor_loads_losslessly() {
    // The EXACT shape every evidence record has persisted until now: four
    // fields, no gate link, no actor. It must deserialize with the documented
    // defaults — and keep carrying them on the wire.
    let legacy = r#"{"kind":"api","label":"live request/response","detail":"GET /api/health\nHTTP 200","at":"2026-08-01T11:00:01Z"}"#;
    let rec: Evidence = serde_json::from_str(legacy).expect("legacy evidence loads");
    assert!(rec.source_gates.is_empty(), "no gate link was recorded");
    assert!(rec.actor.is_empty(), "no actor was recorded");
    assert_eq!(rec.kind, "api");
    assert_eq!(rec.label, "live request/response");

    // The extended record keeps its new fields on the wire (the endpoint's
    // additive contract: source_gates + actor travel with the item).
    let enriched = evidence(
        "api",
        "live request/response",
        "GET /api/health\nHTTP 200",
        "2026-08-01T11:00:01Z",
        &["verify"],
        "TEST",
    );
    let json = serde_json::to_value(&enriched).expect("evidence serializes");
    assert_eq!(json["source_gates"], serde_json::json!(["verify"]));
    assert_eq!(json["actor"], "TEST");
}
