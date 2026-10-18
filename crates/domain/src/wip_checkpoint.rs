//! Slot-WIP checkpoints (CXA-F318): where uncommitted slot work was parked.
//!
//! When an engine dies mid-edit (or a slot worktree is reclaimed), the
//! orchestrator commits the residue to a local git ref and records THAT —
//! ref, sha, summary, time — on the ticket, so a restart starts from a real
//! commit instead of `git stash list` archaeology. The ref itself lives in
//! the shared repository and is pruned once the ticket closes; this value
//! object is the durable, per-ticket record of what was parked and where.
//!
//! The aggregate caps the history so the JSONB ticket document stays bounded:
//! the newest [`MAX_WIP_CHECKPOINTS`] parks survive, older ones drop off.

use crate::error::DomainError;
use crate::kinds::Role;
use crate::ticket::Ticket;
use crate::transitions::field_permitted;
use serde::{Deserialize, Serialize};

/// How many parked-WIP records a ticket keeps. Bounded like the rest of the
/// aggregate's lists — the checkpoint history is a breadcrumb trail, not a log.
pub const MAX_WIP_CHECKPOINTS: usize = 3;

/// One parked-WIP record: the git ref the work was committed to, the commit
/// it points at, a one-line summary (branch + diffstat) and when it happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WipCheckpoint {
    /// The local, never-pushed ref holding the parked commit (e.g.
    /// `refs/coxagent/wip/CXA-F318`). Lives in the shared repo, so deleting
    /// the slot worktree cannot destroy the work.
    pub ref_name: String,
    /// The sha of the parked commit.
    pub sha: String,
    /// One-line summary the operator can read at a glance: branch + diffstat.
    pub note: String,
    /// RFC3339 instant the WIP was parked.
    pub recorded_at: String,
}

impl Ticket {
    /// Record that this ticket's uncommitted work was parked on a checkpoint
    /// ref. Only `System` (the orchestrator's own bookkeeping) may write it —
    /// an agent must not be able to fabricate park records. Keeps the newest
    /// [`MAX_WIP_CHECKPOINTS`] entries, dropping the oldest.
    ///
    /// # Errors
    /// [`DomainError::FieldNotPermitted`] if `actor` is not `System`.
    pub fn record_wip_checkpoint(
        &mut self,
        actor: Role,
        cp: WipCheckpoint,
    ) -> Result<(), DomainError> {
        if !field_permitted(actor, "wip_checkpoints") {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "wip_checkpoints",
            });
        }
        self.wip_checkpoint_list_mut().push(cp);
        let list = self.wip_checkpoint_list_mut();
        let overflow = list.len().saturating_sub(MAX_WIP_CHECKPOINTS);
        if overflow > 0 {
            list.drain(0..overflow);
        }
        Ok(())
    }

    /// The parked-WIP history, oldest first.
    #[must_use]
    pub fn wip_checkpoints(&self) -> &[WipCheckpoint] {
        self.wip_checkpoint_list()
    }

    /// Drop every parked-WIP record. Used when the checkpoint refs themselves
    /// are pruned (ticket closed/archived) so the ticket never shows pointers
    /// to refs that no longer exist. Only `System` may clear them.
    ///
    /// # Errors
    /// [`DomainError::FieldNotPermitted`] if `actor` is not `System`.
    pub fn clear_wip_checkpoints(&mut self, actor: Role) -> Result<(), DomainError> {
        if !field_permitted(actor, "wip_checkpoints") {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "wip_checkpoints",
            });
        }
        self.wip_checkpoint_list_mut().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TicketId;
    use crate::kinds::{Complexity, Priority, TicketType};

    fn ticket() -> Ticket {
        Ticket::new(
            TicketId::new("FEAT-1").expect("id"),
            TicketType::Feature,
            "a feature",
            "desc",
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket")
    }

    fn cp(sha: &str) -> WipCheckpoint {
        WipCheckpoint {
            ref_name: "refs/coxagent/wip/FEAT-1".to_owned(),
            sha: sha.to_owned(),
            note: "branch main; 2 files changed".to_owned(),
            recorded_at: "2026-09-02T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn record_appends_and_caps_at_three_dropping_the_oldest() {
        let mut t = ticket();
        for sha in ["a", "b", "c", "d"] {
            t.record_wip_checkpoint(Role::System, cp(sha))
                .expect("system records");
        }
        let shas: Vec<&str> = t.wip_checkpoints().iter().map(|c| c.sha.as_str()).collect();
        // Newest three survive, the oldest ("a") dropped off.
        assert_eq!(shas, vec!["b", "c", "d"]);
    }

    #[test]
    fn non_system_actor_is_refused() {
        let mut t = ticket();
        assert!(matches!(
            t.record_wip_checkpoint(Role::DevFeature, cp("a")),
            Err(DomainError::FieldNotPermitted {
                field: "wip_checkpoints",
                ..
            })
        ));
        assert!(t.wip_checkpoints().is_empty());
        assert!(t.clear_wip_checkpoints(Role::Po).is_err());
    }

    #[test]
    fn clear_drops_every_record_and_is_system_only() {
        let mut t = ticket();
        t.record_wip_checkpoint(Role::System, cp("a"))
            .expect("record");
        t.clear_wip_checkpoints(Role::System).expect("clear");
        assert!(t.wip_checkpoints().is_empty());
    }

    #[test]
    fn checkpoints_round_trip_through_ticket_serde() {
        let mut t = ticket();
        t.record_wip_checkpoint(Role::System, cp("a"))
            .expect("record");
        t.record_wip_checkpoint(Role::System, cp("b"))
            .expect("record");
        let json = serde_json::to_string(&t).expect("serialize");
        assert!(json.contains("wip_checkpoints"), "field is serialized");
        let back: Ticket = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.wip_checkpoints(), t.wip_checkpoints());
    }

    #[test]
    fn ticket_json_without_the_field_loads_clean() {
        // Old persisted tickets predate the field entirely (skip_serializing_if
        // keeps fresh tickets without parks at the same shape) — they must
        // deserialize with an empty history, never fail.
        let t = ticket();
        let json = serde_json::to_string(&t).expect("serialize");
        assert!(!json.contains("wip_checkpoints"));
        let back: Ticket = serde_json::from_str(&json).expect("old shape loads");
        assert!(back.wip_checkpoints().is_empty());
    }
}
