//! Per-gate evidence forensics (CXA-F241) — the pure derivation that lets a
//! verification reviewer reconstruct, for each DoD gate a ticket passed
//! through, exactly which proof supported the decision and who took it.
//!
//! Same discipline as `dependency_radar` (CXA-F237), which this design cites:
//! every answer is computed deterministically from `ProjectState` alone — no
//! engine, no server, no port, no IO — so it is testable with struct-literal
//! fixtures and safe to run on every ticket-detail request.
//!
//! The gate spine folds over the human-governance ledger (CXA-F230), the only
//! durable, attributed record of gate decisions (`Ticket` keeps no status
//! history). Each ledger kind maps to the ONE legal status edge its endpoint
//! performs (`transitions.rs`): `Ready` is reachable only from `Pending`, and
//! `Verified` only from `Fixed`, so those pairs are facts, not guesses; the
//! verify send-back endpoint's documented edge is `Fixed -> Open` ("the fix is
//! not demonstrated"). Ledger kinds that move no DoD-gate status (cost
//! approvals, PR holds) are not gate transitions and are not fabricated into
//! ones. `actor_role` is always `user`: the gate endpoints transition as
//! `Role::User` by construction — that is what the ledger records.
//!
//! Evidence attribution follows the capture-time rule the tests pin: an item
//! linked to a gate belongs to the LATEST decision of that gate at or before
//! its capture time (the decision it was captured to support), falling back to
//! that gate's first decision when it predates all of them (a waiver written
//! before the gate ever convened). Items with no recorded link are NEVER
//! assigned one — they surface unlinked, rendered as provenance unknown.

use crate::state::{Evidence, ProjectState};
use coxagent_domain::{InterventionKind, TicketId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One DoD gate decision, reconstructed read-time from the governance ledger —
/// the record the ticket-detail payload serves as `gates`, chronologically
/// ordered. Field names are the wire contract's own keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateTransition {
    /// `"ready"` | `"verify"` (the DoD gates the ledger records today).
    pub gate_id: String,
    pub status_from: String,
    pub status_to: String,
    /// The role that performed the transition — the gate endpoints act as
    /// `Role::User`, so this is `"user"` for every record the ledger holds.
    pub actor_role: String,
    /// Epoch milliseconds of the recorded decision time (the house epoch
    /// width), so clients order decisions without parsing RFC3339.
    pub decided_at_ms: i64,
}

/// One gate decision with the evidence records grouped under it, in spine
/// order — the forensics view's unit of display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateEvidenceGroup {
    pub gate: GateTransition,
    pub evidence: Vec<Evidence>,
}

/// How a screenshot evidence item can render. `Image` carries the captured
/// authenticated media URL; `Missing` is the explicit broken-artifact state —
/// it deliberately carries NO url, so the view cannot fabricate content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotRender {
    Image(String),
    Missing,
}

/// An explicit waiver: who granted it and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiver {
    pub granted_by: String,
    pub reason: String,
}

/// Capture provenance of one evidence item. The model carries no capture
/// commit today (the Evidence type has no such field and the design adds
/// none), so `Unknown` is the only state real data can reach — rendered as
/// the explicit "provenance unknown", never resolved by guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// A capture commit is on record (future: when the model grows one).
    Captured { commit: String },
    Unknown,
}

/// RFC3339 → epoch milliseconds. `None` for a stamp that cannot be parsed —
/// callers skip the record rather than place it at a made-up time.
#[must_use]
pub fn epoch_millis(at: &str) -> Option<i64> {
    let fmt = &time::format_description::well_known::Rfc3339;
    let t = time::OffsetDateTime::parse(at, fmt).ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

/// The gate spine of `ticket`: every DoD gate decision the governance ledger
/// holds for it, chronologically ordered (stable, so same-millisecond records
/// keep their ledger order).
#[must_use]
pub fn gate_spine(state: &ProjectState, ticket: &TicketId) -> Vec<GateTransition> {
    let mut transitions: Vec<GateTransition> = state
        .governance_interventions
        .iter()
        .filter(|rec| rec.ticket == ticket.as_str())
        .filter_map(|rec| {
            // One kind → the ONE legal edge its endpoint performs; kinds that
            // move no DoD-gate status are not transitions and stay out.
            let (gate_id, from, to) = match rec.kind {
                InterventionKind::ReadyApprove => ("ready", "pending", "ready"),
                InterventionKind::VerifyPass => ("verify", "fixed", "verified"),
                InterventionKind::VerifySendBack => ("verify", "fixed", "open"),
                InterventionKind::CostApprove
                | InterventionKind::HumanPrReviewed
                | InterventionKind::HumanPrDismissed
                | InterventionKind::UndoAutoApprove => return None,
            };
            Some((
                gate_id,
                from,
                to,
                rec.at.clone(),
            ))
        })
        .filter_map(|(gate_id, from, to, at)| {
            Some(GateTransition {
                gate_id: gate_id.to_owned(),
                status_from: from.to_owned(),
                status_to: to.to_owned(),
                // The gate endpoints transition as Role::User by construction.
                actor_role: "user".to_owned(),
                decided_at_ms: epoch_millis(&at)?,
            })
        })
        .collect();
    transitions.sort_by_key(|t| t.decided_at_ms);
    transitions
}

/// Group evidence under the spine decision each item supported, in spine
/// order: one group per transition, every linked item in exactly one group.
/// Items with no link to any spine gate are NOT grouped — see
/// [`unlinked_evidence`].
#[must_use]
pub fn group_by_gate(
    spine: &[GateTransition],
    evidence: &[Evidence],
) -> Vec<GateEvidenceGroup> {
    spine
        .iter()
        .map(|gate| GateEvidenceGroup {
            gate: gate.clone(),
            evidence: evidence
                .iter()
                .filter(|e| supports(&gate.clone(), e, spine))
                .cloned()
                .collect(),
        })
        .collect()
}

/// Whether `item` is attributed to `gate` given the full `spine`: the item
/// names the gate, and this transition is the latest decision of that gate at
/// or before the capture time — or the gate's earliest decision when the item
/// predates all of them (captured to support a gate that had not yet ruled).
fn supports(gate: &GateTransition, item: &Evidence, spine: &[GateTransition]) -> bool {
    if !item.source_gates.iter().any(|g| g == &gate.gate_id) {
        return false;
    }
    let at = match epoch_millis(&item.at) {
        Some(at) => at,
        // An item whose own stamp is unreadable has no place on the
        // timeline; it stays unlinked rather than attributed by guess.
        None => return false,
    };
    let same_gate: Vec<&GateTransition> = spine
        .iter()
        .filter(|t| t.gate_id == gate.gate_id)
        .collect();
    match same_gate.iter().rev().find(|t| t.decided_at_ms <= at) {
        Some(latest) => latest.decided_at_ms == gate.decided_at_ms,
        // Predates every decision of this gate → the first one.
        None => same_gate.first().is_some_and(|t| t.decided_at_ms == gate.decided_at_ms),
    }
}

/// Evidence records the spine cannot attribute: no captured gate link (records
/// written before attribution existed), a link to a gate the spine does not
/// hold, or an unreadable capture stamp. The view renders these explicitly as
/// unlinked/provenance unknown — absence of a link is never silently promoted
/// to one.
#[must_use]
pub fn unlinked_evidence(spine: &[GateTransition], evidence: &[Evidence]) -> Vec<Evidence> {
    evidence
        .iter()
        .filter(|e| !spine.iter().any(|gate| supports(gate, e, spine)))
        .cloned()
        .collect()
}

/// The full captured request/response text of an api/test item — AC2's inline
/// body, already capped at capture. Kinds that render as an image or a waiver
/// carry none.
#[must_use]
pub fn inline_text(item: &Evidence) -> Option<&str> {
    match item.kind.as_str() {
        "api" | "test" => Some(&item.detail),
        _ => None,
    }
}

/// The storage key behind a captured screenshot URL: the media route the
/// capture writes (`/api/projects/{pid}/media/{file}`) maps to the blob key
/// it was stored under (`proj/{pid}/{file}`). `None` for anything else —
/// legacy repo-relative paths have no resolvable key and render missing.
#[must_use]
pub fn media_key(detail: &str) -> Option<String> {
    let rest = detail.strip_prefix("/api/projects/")?;
    let (pid, file) = rest.split_once("/media/")?;
    if pid.is_empty() || file.is_empty() || file.contains('/') {
        return None;
    }
    Some(format!("proj/{pid}/{file}"))
}

/// How a screenshot item renders given the storage adapter's report of which
/// blob keys exist on disk: the captured media URL when the artifact is
/// present, the explicit `Missing` state otherwise. The existence set is
/// adapter data; the decision stays pure.
#[must_use]
pub fn screenshot_render(item: &Evidence, present_keys: &BTreeSet<String>) -> ScreenshotRender {
    if item.kind != "screenshot" {
        return ScreenshotRender::Missing;
    }
    match media_key(&item.detail) {
        Some(key) if present_keys.contains(&key) => ScreenshotRender::Image(item.detail.clone()),
        _ => ScreenshotRender::Missing,
    }
}

/// The explicit waiver a `waived` record represents — who granted it (the
/// recorded actor; unattributed renders empty and the view says so) and why
/// (the captured reason). Captured records are never waivers.
#[must_use]
pub fn waiver(item: &Evidence) -> Option<Waiver> {
    if item.kind != "waived" {
        return None;
    }
    Some(Waiver {
        granted_by: item.actor.clone(),
        reason: item.detail.clone(),
    })
}

/// The capture provenance of an item. The Evidence model carries no capture
/// commit today, so every real record is `Unknown` — the view renders the
/// explicit fallback instead of inventing a commit. When the model grows a
/// commit field, this is the one place that gains the branch.
#[must_use]
pub fn provenance(item: &Evidence) -> Provenance {
    let _ = item;
    Provenance::Unknown
}

/// The rendered wording for a provenance state — the explicit string a
/// missing capture commit displays.
#[must_use]
pub fn provenance_label(provenance: &Provenance) -> String {
    match provenance {
        Provenance::Captured { commit } => format!("captured at {commit}"),
        Provenance::Unknown => "provenance unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ProjectState;

    fn evidence(kind: &str, at: &str, gates: &[&str]) -> Evidence {
        Evidence {
            kind: kind.to_owned(),
            label: "label".to_owned(),
            detail: "detail".to_owned(),
            at: at.to_owned(),
            source_gates: gates.iter().map(|g| (*g).to_owned()).collect(),
            actor: "TEST".to_owned(),
        }
    }

    fn gate(gate_id: &str, at_ms: i64) -> GateTransition {
        GateTransition {
            gate_id: gate_id.to_owned(),
            status_from: "fixed".to_owned(),
            status_to: "verified".to_owned(),
            actor_role: "user".to_owned(),
            decided_at_ms: at_ms,
        }
    }

    #[test]
    fn the_spine_is_empty_for_a_ticket_the_ledger_never_decided() {
        let state = ProjectState::default();
        assert!(gate_spine(&state, &TicketId::new("CXA-F001").expect("id")).is_empty());
    }

    #[test]
    fn an_unreadable_decision_stamp_is_skipped_not_fabricated_to_zero() {
        // A corrupt `at` cannot be placed on the timeline; inventing epoch 0
        // would misorder every real decision around it.
        let mut state = ProjectState::default();
        state.governance_interventions.push(crate::state::InterventionRecord {
            kind: InterventionKind::VerifyPass,
            ticket: "CXA-F001".to_owned(),
            area: None,
            by: "rev".to_owned(),
            at: "not-a-stamp".to_owned(),
        });
        assert!(gate_spine(&state, &TicketId::new("CXA-F001").expect("id")).is_empty());
    }

    #[test]
    fn evidence_predating_every_decision_of_its_gate_lands_on_the_first() {
        // A waiver written at ready-gate time, linked to verify: the verify
        // gate had not convened yet — it belongs to the gate's first decision.
        let spine = vec![gate("verify", 2_000), gate("verify", 9_000)];
        let items = vec![evidence("waived", "1970-01-01T00:00:01Z", &["verify"])];
        let groups = group_by_gate(&spine, &items);
        assert_eq!(groups[0].evidence.len(), 1, "the first decision carries it");
        assert!(groups[1].evidence.is_empty());
    }

    #[test]
    fn evidence_with_an_unreadable_stamp_stays_unlinked() {
        let spine = vec![gate("verify", 2_000)];
        let items = vec![evidence("api", "garbage", &["verify"])];
        assert!(group_by_gate(&spine, &items)[0].evidence.is_empty());
        assert_eq!(unlinked_evidence(&spine, &items).len(), 1);
    }

    #[test]
    fn items_linked_to_a_gate_the_spine_does_not_hold_stay_unlinked() {
        let spine = vec![gate("verify", 2_000)];
        let items = vec![evidence("api", "1970-01-01T00:00:03Z", &["deploy-health"])];
        assert!(group_by_gate(&spine, &items)[0].evidence.is_empty());
        assert_eq!(unlinked_evidence(&spine, &items).len(), 1);
    }

    #[test]
    fn a_media_url_maps_to_its_storage_key_and_nothing_else_does() {
        assert_eq!(
            media_key("/api/projects/TL/media/evidence-CXA-B241.png"),
            Some("proj/TL/evidence-CXA-B241.png".to_owned())
        );
        // Traversal, relative paths and foreign shapes never resolve.
        assert_eq!(media_key("../../etc/passwd"), None);
        assert_eq!(media_key("/api/projects//media/x.png"), None);
        assert_eq!(media_key("/api/projects/TL/media/a/b.png"), None);
        assert_eq!(media_key("shots/evidence.png"), None);
    }

    #[test]
    fn a_non_screenshot_item_never_renders_an_image() {
        let present = BTreeSet::from(["proj/TL/x.png".to_owned()]);
        assert_eq!(
            screenshot_render(&evidence("api", "1970-01-01T00:00:00Z", &[]), &present),
            ScreenshotRender::Missing
        );
    }
}
