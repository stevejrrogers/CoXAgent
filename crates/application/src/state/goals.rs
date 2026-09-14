// Part of the `state` module split by bounded context — see state/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The goal-line outcome ledger (CXA-F228): declared product goals, and the
//! append-only record of which verified deliverable advanced which goal.
//!
//! Identity contract: everything binds to a stable [`GoalId`]. Renaming a
//! goal's wording never touches recorded associations (AC3), and Verified
//! tickets with no resolvable association land in the report's explicit
//! `unattributed` bucket — never silently dropped (AC4/AC5).

use coxagent_domain::{DomainError, Goal, GoalId, GoalStatus, Status};
use serde::{Deserialize, Serialize};

use super::*;

/// Keep the outcome ledger bounded (newest last, like the activity feed). A
/// project's verified history fits comfortably; overflow drops the oldest.
pub const MAX_OUTCOME_LEDGER: usize = 500;

/// One recorded outcome: a ticket reached `Verified`, and at that moment the
/// ledger froze the provenance — which goal the ticket declared, which commit
/// the deploy evidence was captured against, and when verification happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeLedgerEntry {
    pub ticket: TicketId,
    /// The goal declared at verification time. `None` when the ticket carried
    /// no resolvable association — the entry still records that the work
    /// verified, so nothing ships invisible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalId>,
    /// The commit sha the last deploy attempt built/ran (absent when git
    /// isn't wired) — the "capture commit" the verification is anchored to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_commit: Option<String>,
    /// RFC3339 verification timestamp.
    pub verified_at: String,
}

/// Per-goal aggregation row: the goal line and how many verified deliverables
/// advanced it. Goals with zero contributions appear with `0` — the
/// "which goals have received nothing" signal planning runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalOutcome {
    pub goal_id: String,
    pub title: String,
    pub status: GoalStatus,
    pub verified_contributions: u32,
}

/// A Verified ticket whose recorded data has no resolvable goal association.
/// Always visible, never counted toward any goal total; `reason` says why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnattributedOutcome {
    pub ticket: String,
    pub title: String,
    pub reason: String,
}

/// The read-side answer to "which goal lines did verified work actually move?"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalOutcomeReport {
    pub goals: Vec<GoalOutcome>,
    pub unattributed: Vec<UnattributedOutcome>,
}

impl ProjectState {
    /// Declare a new product goal with a freshly minted stable id.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when `title` is blank.
    pub fn add_goal(&mut self, title: &str) -> Result<GoalId, DomainError> {
        // Mint `G001`, `G002`, ... — never reusing an id already in the list,
        // so a stable id stays stable even if goals were ever pruned by hand.
        let mut n = self.goals.len() + 1;
        let mut id = GoalId::new(format!("G{n:03}"))?;
        while self.goals.iter().any(|g| g.id == id) {
            n += 1;
            id = GoalId::new(format!("G{n:03}"))?;
        }
        let goal = Goal::new(id.clone(), title)?;
        self.goals.push(goal);
        Ok(id)
    }

    /// Restate a goal's wording. The id is untouched by construction, so every
    /// ticket association and ledger entry survives the rename.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when the new title is blank.
    /// Returns `false` when no goal carries `id`.
    pub fn rename_goal(&mut self, id: &GoalId, title: &str) -> Result<bool, DomainError> {
        match self.goals.iter_mut().find(|g| &g.id == id) {
            Some(goal) => {
                goal.rename(title)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Close a goal's gate: no NEW ticket may declare it; recorded work keeps
    /// its attribution. Returns `false` when no goal carries `id`.
    pub fn retire_goal(&mut self, id: &GoalId) -> bool {
        match self.goals.iter_mut().find(|g| &g.id == id) {
            Some(goal) => {
                goal.retire();
                true
            }
            None => false,
        }
    }

    /// Record the outcome when a ticket reaches `Verified`. Called from every
    /// verification path (agent TEST, human inbox verdict, human chat
    /// verdict) so the ledger is the one append-only truth of shipped work.
    ///
    /// The goal snapshot and the deploy's capture commit are frozen here —
    /// later goal edits never rewrite what was true at verification time.
    /// Returns `false` when no ticket carries `ticket` (nothing recorded).
    pub fn record_verified_outcome(&mut self, ticket: &str) -> bool {
        let Some(t) = self.tickets.iter().find(|t| t.id().as_str() == ticket) else {
            return false;
        };
        let entry = OutcomeLedgerEntry {
            ticket: t.id().clone(),
            goal: t.goal_id().cloned(),
            capture_commit: self.deploy.as_ref().and_then(|d| d.commit_sha.clone()),
            verified_at: now_rfc3339(),
        };
        self.outcome_ledger.push(entry);
        let overflow = self.outcome_ledger.len().saturating_sub(MAX_OUTCOME_LEDGER);
        if overflow > 0 {
            self.outcome_ledger.drain(0..overflow);
        }
        true
    }

    /// Aggregate verified outcomes per declared goal (pure — a function of
    /// state, so the read endpoint renders it and tests exercise it directly).
    ///
    /// Every goal in the project appears, including zero-contribution lines.
    /// Every `Verified` ticket is counted at most once, toward at most one
    /// goal: the ticket's live association wins (so a backfilled pre-tracking
    /// ticket re-attributes), falling back to the goal the ledger froze at
    /// verification time. Anything unresolvable lands in `unattributed` with
    /// the reason visible.
    #[must_use]
    pub fn outcome_report(&self) -> GoalOutcomeReport {
        let mut goals: Vec<GoalOutcome> = self
            .goals
            .iter()
            .map(|g| GoalOutcome {
                goal_id: g.id.to_string(),
                title: g.title.clone(),
                status: g.status,
                verified_contributions: 0,
            })
            .collect();
        let mut unattributed = Vec::new();
        for t in &self.tickets {
            if t.status() != Status::Verified {
                continue;
            }
            let entry = self
                .outcome_ledger
                .iter()
                .rev()
                .find(|e| e.ticket == *t.id());
            let declared = t
                .goal_id()
                .cloned()
                .or_else(|| entry.and_then(|e| e.goal.clone()));
            match declared {
                Some(gid) => match goals.iter_mut().find(|g| g.goal_id == gid.as_str()) {
                    Some(g) => g.verified_contributions += 1,
                    None => unattributed.push(UnattributedOutcome {
                        ticket: t.id().to_string(),
                        title: t.title().to_owned(),
                        reason: format!("declared goal {gid} is not in the project's goal list"),
                    }),
                },
                None => unattributed.push(UnattributedOutcome {
                    ticket: t.id().to_string(),
                    title: t.title().to_owned(),
                    reason: if entry.is_some() {
                        "verified without a declared goal association".to_owned()
                    } else {
                        "verified before goal-line tracking shipped — no declared goal \
                         association (backfill the ticket to attribute it)"
                            .to_owned()
                    },
                }),
            }
        }
        GoalOutcomeReport {
            goals,
            unattributed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, Role, Ticket, TicketId, TicketType};

    /// A bug shepherded to `Verified` (the only status that reaches it),
    /// optionally bound to a goal first.
    fn verified_bug(id: &str, goal: Option<GoalId>) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            format!("fix {id}"),
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("ticket");
        if let Some(g) = goal {
            t.set_goal_id(Role::System, g).expect("bind");
        }
        for to in [Status::InProgress, Status::Fixed, Status::Verified] {
            t.transition_to(Role::System, to).expect("system walks");
        }
        t
    }

    fn state_with(tickets: Vec<Ticket>) -> ProjectState {
        ProjectState {
            tickets,
            ..ProjectState::default()
        }
    }

    #[test]
    fn ledger_entry_links_ticket_goal_commit_and_timestamp() {
        // AC1: ticket id -> declared goal id -> capture commit -> verification
        // timestamp, recorded when an evidenced ticket reaches Verified.
        let gid = GoalId::new("G001").expect("gid");
        let mut s = state_with(vec![verified_bug("CXC-B001", Some(gid.clone()))]);
        s.deploy = Some(DeployStatus {
            at: "t".to_owned(),
            ok: true,
            summary: "deployed".to_owned(),
            commit_sha: Some("abc123".to_owned()),
            health_check: None,
            failure_bundle: None,
        });
        assert!(s.record_verified_outcome("CXC-B001"));
        let e = s.outcome_ledger.first().expect("entry");
        assert_eq!(e.ticket.as_str(), "CXC-B001");
        assert_eq!(e.goal.as_ref().map(GoalId::as_str), Some("G001"));
        assert_eq!(e.capture_commit.as_deref(), Some("abc123"));
        assert!(!e.verified_at.is_empty());
    }

    #[test]
    fn attributed_ticket_counts_toward_its_goal() {
        let gid = GoalId::new("G001").expect("gid");
        let mut s = state_with(vec![verified_bug("B001", Some(gid))]);
        s.add_goal("Faster merges").expect("goal");
        s.record_verified_outcome("B001");
        let report = s.outcome_report();
        assert_eq!(report.goals.len(), 1);
        assert_eq!(report.goals[0].verified_contributions, 1);
        assert!(report.unattributed.is_empty());
    }

    #[test]
    fn renaming_a_goal_never_severs_attributions() {
        // AC3: the association binds to the stable id, not the wording.
        let gid = GoalId::new("G001").expect("gid");
        let mut s = state_with(vec![verified_bug("B001", Some(gid.clone()))]);
        s.add_goal("Faster merges").expect("goal");
        s.record_verified_outcome("B001");
        assert!(s
            .rename_goal(&gid, "Faster, safer merges")
            .expect("renamed"));
        let report = s.outcome_report();
        assert_eq!(report.goals[0].title, "Faster, safer merges");
        assert_eq!(report.goals[0].verified_contributions, 1);
        // The ledger entry still names the same stable id.
        assert_eq!(
            s.outcome_ledger[0].goal.as_ref().map(GoalId::as_str),
            Some("G001")
        );
        assert!(report.unattributed.is_empty());
    }

    #[test]
    fn zero_contribution_goals_still_appear() {
        let mut s = state_with(vec![verified_bug("B001", None)]);
        s.add_goal("Multi-tenant hub").expect("goal");
        s.record_verified_outcome("B001");
        let report = s.outcome_report();
        assert_eq!(report.goals[0].verified_contributions, 0);
        assert_eq!(report.unattributed.len(), 1);
    }

    #[test]
    fn verified_without_declared_goal_is_unattributed_and_uncounted() {
        // AC4: never silently excluded, never counted toward a goal total.
        let mut s = state_with(vec![verified_bug("B001", None)]);
        s.add_goal("Faster merges").expect("goal");
        s.record_verified_outcome("B001");
        let report = s.outcome_report();
        assert_eq!(report.goals[0].verified_contributions, 0);
        let u = report.unattributed.first().expect("listed");
        assert_eq!(u.ticket, "B001");
        assert_eq!(u.reason, "verified without a declared goal association");
    }

    #[test]
    fn pre_feature_verified_ticket_shows_why_until_backfilled() {
        // AC5 (edge): a ticket verified before this feature shipped has no
        // ledger entry and no association — visible with its reason, then
        // re-attributed once a goal is backfilled.
        let mut s = state_with(vec![verified_bug("OLD-001", None)]);
        s.add_goal("Faster merges").expect("goal");
        let report = s.outcome_report();
        let u = report.unattributed.first().expect("listed");
        assert!(
            u.reason.contains("before goal-line tracking shipped"),
            "{u:?}"
        );
        // Backfill re-attributes: the live association wins over the absent
        // ledger snapshot.
        let old = s.tickets.first().expect("ticket").id().clone();
        let gid = GoalId::new("G001").expect("gid");
        s.ticket_mut(&old)
            .expect("ticket")
            .set_goal_id(Role::Po, gid)
            .expect("po backfills");
        let report = s.outcome_report();
        assert!(report.unattributed.is_empty());
        assert_eq!(report.goals[0].verified_contributions, 1);
    }

    #[test]
    fn dangling_goal_reference_is_unattributed_with_reason() {
        let gid = GoalId::new("G404").expect("gid");
        let s = state_with(vec![verified_bug("B001", Some(gid))]);
        let report = s.outcome_report();
        let u = report.unattributed.first().expect("listed");
        assert!(u.reason.contains("not in the project's goal list"), "{u:?}");
    }

    #[test]
    fn retired_goal_keeps_its_recorded_work() {
        let gid = GoalId::new("G001").expect("gid");
        let mut s = state_with(vec![verified_bug("B001", Some(gid.clone()))]);
        s.add_goal("Faster merges").expect("goal");
        s.record_verified_outcome("B001");
        assert!(s.retire_goal(&gid));
        let report = s.outcome_report();
        assert_eq!(report.goals[0].status, GoalStatus::Retired);
        assert_eq!(report.goals[0].verified_contributions, 1);
    }

    #[test]
    fn unknown_ticket_records_nothing() {
        let mut s = ProjectState::default();
        assert!(!s.record_verified_outcome("NOPE-1"));
        assert!(s.outcome_ledger.is_empty());
    }

    #[test]
    fn ledger_stays_bounded() {
        let mut s = state_with(vec![verified_bug("B001", None)]);
        for _ in 0..(MAX_OUTCOME_LEDGER + 10) {
            s.record_verified_outcome("B001");
        }
        assert_eq!(s.outcome_ledger.len(), MAX_OUTCOME_LEDGER);
    }

    #[test]
    fn re_verification_appends_and_the_latest_snapshot_wins() {
        // Verified -> Open -> Verified is a legal walk (failed regression
        // reopen). The ledger is append-only; the report counts the ticket
        // once, against its current association.
        let gid = GoalId::new("G001").expect("gid");
        let mut s = state_with(vec![verified_bug("B001", Some(gid))]);
        s.add_goal("Faster merges").expect("goal");
        s.record_verified_outcome("B001");
        let id = s.tickets.first().expect("t").id().clone();
        s.ticket_mut(&id)
            .expect("t")
            .transition_to(Role::Po, Status::Open)
            .expect("reopen");
        let b = s.ticket_mut(&id).expect("t");
        for to in [Status::InProgress, Status::Fixed, Status::Verified] {
            b.transition_to(Role::System, to).expect("re-walk");
        }
        s.record_verified_outcome("B001");
        assert_eq!(s.outcome_ledger.len(), 2);
        let report = s.outcome_report();
        assert_eq!(report.goals[0].verified_contributions, 1);
    }
}
