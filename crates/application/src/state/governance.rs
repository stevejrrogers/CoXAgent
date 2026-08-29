// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The human governance-attention ledger (CXA-F230): an append-only, bounded
//! record of every discrete operator review decision, tagged with the
//! ticket's class (`TicketType`) and an [`InterventionKind`].
//!
//! Attribution contract: the ticket class is FROZEN into the record at
//! action time, and `at` is stamped from the action — so later edits to a
//! ticket (rename, re-association, even deletion) never rewrite recorded
//! history. A record that could not resolve a class (a PR decision with no
//! resolvable ticket) keeps `area: None` and lands in the summary's explicit
//! `unattributed` bucket — never silently assigned, never dropped.

use coxagent_domain::{InterventionKind, TicketId, TicketType};
use serde::{Deserialize, Serialize};

use super::*;

/// Keep the governance ledger bounded (newest last, like the activity feed
/// and the outcome ledger). A project's human-gate history fits comfortably;
/// overflow drops the oldest.
pub const MAX_GOVERNANCE_INTERVENTIONS: usize = 500;

/// One recorded operator decision: which gate, on which ticket, by whom,
/// when. Small by design — the ledger is aggregated per area/kind for the
/// dashboard, and the raw records are the audit trail behind those counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterventionRecord {
    pub kind: InterventionKind,
    /// The ticket the decision was taken on (empty for decisions that could
    /// not resolve one, e.g. a PR hold whose branch names no ticket).
    #[serde(default)]
    pub ticket: String,
    /// Ticket class frozen at action time; `None` when no ticket was
    /// resolvable — the unattributed bucket, never a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<TicketType>,
    /// The operator who took the decision (the authenticated principal).
    #[serde(default)]
    pub by: String,
    /// RFC3339 action-time snapshot, stamped once here and never rewritten.
    #[serde(default)]
    pub at: String,
}

impl InterventionRecord {
    /// The decision identity concurrent writers agree on: two records that
    /// match on kind, ticket, operator and whole second are the SAME
    /// resolution double-written by a lost-update race, not two decisions —
    /// a second take on the same gate item is refused by the transition
    /// guards, and a re-opened item re-decided later lands minutes apart.
    /// Aggregation counts each identity once (AC5).
    #[must_use]
    pub fn decision_identity(&self) -> (&'static str, &str, &str, &str) {
        // RFC3339 with or without fractional seconds agrees on the first 19
        // chars: `YYYY-MM-DDTHH:MM:SS`.
        (
            self.kind.key(),
            &self.ticket,
            &self.by,
            self.at.get(..19).unwrap_or(&self.at),
        )
    }
}

impl ProjectState {
    /// Record one operator gate decision on a ticket. The ticket class is
    /// resolved from the live ticket and frozen into the record; an unknown
    /// ticket id still records (unattributed) rather than dropping the fact
    /// that a human had to decide.
    pub fn record_intervention(&mut self, kind: InterventionKind, ticket: &str, by: &str) {
        let area = TicketId::new(ticket)
            .ok()
            .and_then(|tid| self.ticket(&tid))
            .map(coxagent_domain::Ticket::ticket_type);
        self.push_intervention(InterventionRecord {
            kind,
            ticket: ticket.to_owned(),
            area,
            by: by.to_owned(),
            at: now_rfc3339(),
        });
    }

    /// Record one operator decision on a pull request (CXA-F230: the
    /// `human_eyes` inbox actions). The ticket is resolved the same way the
    /// merge sync resolves it — the branch head's tail segment — so a held PR
    /// attributes to the ticket its branch carries; a PR that names no ticket
    /// (release branches, foreign branches) records unattributed.
    pub fn record_pr_intervention(&mut self, kind: InterventionKind, number: u64, by: &str) {
        let head = self
            .open_prs
            .iter()
            .find(|p| p.number == number)
            .map(|p| p.head.clone())
            .unwrap_or_default();
        let ticket = head.rsplit('/').next().unwrap_or(&head).to_owned();
        let area = TicketId::new(&ticket)
            .ok()
            .and_then(|tid| self.ticket(&tid))
            .map(coxagent_domain::Ticket::ticket_type);
        self.push_intervention(InterventionRecord {
            kind,
            ticket,
            area,
            by: by.to_owned(),
            at: now_rfc3339(),
        });
    }

    /// Append + trim, the one place the bounded ledger grows.
    fn push_intervention(&mut self, rec: InterventionRecord) {
        self.governance_interventions.push(rec);
        let overflow = self
            .governance_interventions
            .len()
            .saturating_sub(MAX_GOVERNANCE_INTERVENTIONS);
        if overflow > 0 {
            self.governance_interventions.drain(0..overflow);
        }
    }

    /// Per-area / per-kind intervention totals recorded strictly AFTER
    /// `since` (RFC3339 lexicographic compare, like the daily digest's
    /// 24h cutoff). The cycle scorecard folds this delta into its
    /// accumulators so each scorecard is a self-contained snapshot of the
    /// human attention that landed during its cycle.
    ///
    /// Dedupes by decision identity exactly like the summary aggregation —
    /// a same-resolution double-write must not count once here and once
    /// there (AC5 covers every aggregated total, not just the dashboard's).
    #[must_use]
    pub fn attention_delta_since(
        &self,
        since: &str,
    ) -> (
        std::collections::BTreeMap<String, u64>,
        std::collections::BTreeMap<String, u64>,
    ) {
        let mut seen = std::collections::BTreeSet::new();
        let mut by_area = std::collections::BTreeMap::new();
        let mut by_kind = std::collections::BTreeMap::new();
        for rec in &self.governance_interventions {
            if rec.at.as_str() <= since || !seen.insert(rec.decision_identity()) {
                continue;
            }
            if let Some(area) = rec.area {
                *by_area.entry(area.key().to_owned()).or_insert(0) += 1;
            }
            *by_kind.entry(rec.kind.key().to_owned()).or_insert(0) += 1;
        }
        (by_area, by_kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, Ticket};

    fn feature(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("ticket")
    }

    #[test]
    fn a_ready_approval_freezes_the_ticket_class_at_action_time() {
        // AC1: the record attributes to the owning ticket-class, and the
        // class is frozen — deleting the ticket afterwards must not rewrite
        // the recorded history.
        let mut s = ProjectState {
            tickets: vec![feature("CXC-F001")],
            ..ProjectState::default()
        };
        s.record_intervention(InterventionKind::ReadyApprove, "CXC-F001", "po");
        assert_eq!(s.governance_interventions.len(), 1);
        let rec = &s.governance_interventions[0];
        assert_eq!(rec.kind, InterventionKind::ReadyApprove);
        assert_eq!(rec.area, Some(TicketType::Feature));
        assert_eq!(rec.by, "po");
        assert!(!rec.at.is_empty(), "time stamped at action time");

        // Later edits never rewrite history: the ticket disappears, the
        // record keeps its frozen class.
        s.tickets.clear();
        assert_eq!(
            s.governance_interventions[0].area,
            Some(TicketType::Feature)
        );
    }

    #[test]
    fn a_decision_on_an_unknown_ticket_records_unattributed_not_dropped() {
        // AC4: lacking a derivable owner/area lands in the explicit bucket.
        let mut s = ProjectState::default();
        s.record_intervention(InterventionKind::CostApprove, "NOPE-9", "po");
        let rec = &s.governance_interventions[0];
        assert_eq!(rec.area, None);
        assert_eq!(rec.ticket, "NOPE-9");
    }

    #[test]
    fn a_pr_hold_resolution_attributes_via_the_branch_head_ticket() {
        let mut s = ProjectState {
            tickets: vec![feature("CXC-F001")],
            ..ProjectState::default()
        };
        s.open_prs.push(crate::ports::outbound::PrOpen {
            number: 7,
            title: "feat".into(),
            head: "feat/CXC-F001".into(),
            base: "main".into(),
            url: "u".into(),
            author: "sa".into(),
            ci: "ok".into(),
            mergeable: true,
            created: "2026-08-01T00:00:00Z".into(),
        });
        s.record_pr_intervention(InterventionKind::HumanPrReviewed, 7, "dev");
        let rec = &s.governance_interventions[0];
        assert_eq!(rec.kind, InterventionKind::HumanPrReviewed);
        assert_eq!(rec.area, Some(TicketType::Feature));
        assert_eq!(rec.ticket, "CXC-F001");
    }

    #[test]
    fn a_pr_without_a_resolvable_ticket_records_unattributed() {
        let mut s = ProjectState::default();
        s.open_prs.push(crate::ports::outbound::PrOpen {
            number: 8,
            title: "release".into(),
            head: "release/v1.2.0".into(),
            base: "main".into(),
            url: "u".into(),
            author: "sa".into(),
            ci: "ok".into(),
            mergeable: true,
            created: "2026-08-01T00:00:00Z".into(),
        });
        s.record_pr_intervention(InterventionKind::HumanPrDismissed, 8, "dev");
        let rec = &s.governance_interventions[0];
        assert_eq!(rec.area, None);
    }

    #[test]
    fn the_ledger_stays_bounded_newest_last() {
        let mut s = ProjectState::default();
        for _ in 0..(MAX_GOVERNANCE_INTERVENTIONS + 10) {
            s.record_intervention(InterventionKind::VerifyPass, "CXC-F001", "qa");
        }
        assert_eq!(s.governance_interventions.len(), MAX_GOVERNANCE_INTERVENTIONS);
        // Newest last: the first kept record is the (max+10-kept+1)-th write.
        assert_eq!(s.governance_interventions[0].by, "qa");
    }

    #[test]
    fn state_written_before_the_ledger_existed_still_loads() {
        // serde(default): old persisted state deserializes cleanly, no
        // migration.
        let mut doc = serde_json::to_value(ProjectState::default()).expect("serialize");
        doc.as_object_mut()
            .expect("object")
            .remove("governance_interventions");
        let back: ProjectState = serde_json::from_value(doc).expect("load legacy state");
        assert!(back.governance_interventions.is_empty());
    }

    #[test]
    fn the_delta_since_the_last_scorecard_counts_only_newer_records() {
        let mut s = ProjectState {
            tickets: vec![feature("CXC-F001")],
            ..ProjectState::default()
        };
        let mut old = InterventionRecord {
            kind: InterventionKind::ReadyApprove,
            ticket: "CXC-F001".into(),
            area: Some(TicketType::Feature),
            by: "po".into(),
            at: "2026-07-01T00:00:00Z".into(),
        };
        s.push_intervention(old.clone());
        old.kind = InterventionKind::VerifyPass;
        old.at = "2026-08-02T00:00:00Z".into();
        s.push_intervention(old);
        let (by_area, by_kind) = s.attention_delta_since("2026-08-01T00:00:00Z");
        assert_eq!(by_area.get("feature").copied(), Some(1));
        assert_eq!(by_kind.get("verify_pass").copied(), Some(1));
        assert_eq!(by_kind.get("ready_approve").copied(), None);
        // An empty `since` folds everything.
        let (by_area, _) = s.attention_delta_since("");
        assert_eq!(by_area.get("feature").copied(), Some(2));
    }

    #[test]
    fn unattributed_records_count_toward_by_kind_but_not_by_area() {
        let mut s = ProjectState::default();
        s.record_intervention(InterventionKind::CostApprove, "NOPE-9", "po");
        let (by_area, by_kind) = s.attention_delta_since("");
        assert!(by_area.is_empty());
        assert_eq!(by_kind.get("cost_approve").copied(), Some(1));
    }

    #[test]
    fn the_delta_dedupes_a_same_resolution_double_write() {
        // AC5 covers every aggregated total: the scorecard's delta counts a
        // concurrent double-write once, exactly like the dashboard summary.
        let mut s = ProjectState::default();
        let decision = |at: &str| InterventionRecord {
            kind: InterventionKind::HumanPrReviewed,
            ticket: "CXC-F001".into(),
            area: Some(TicketType::Feature),
            by: "po".into(),
            at: at.to_owned(),
        };
        s.push_intervention(decision("2026-08-01T10:00:00.100000Z"));
        s.push_intervention(decision("2026-08-01T10:00:00.900000Z"));
        let (by_area, by_kind) = s.attention_delta_since("");
        assert_eq!(by_area.get("feature").copied(), Some(1));
        assert_eq!(by_kind.get("human_pr_reviewed").copied(), Some(1));
    }
}
