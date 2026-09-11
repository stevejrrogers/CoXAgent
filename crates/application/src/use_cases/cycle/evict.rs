//! CXA-F273: hot-state eviction — terminal tickets move to the cold archive.
//!
//! The hot `ProjectState` row had grown past 5 MB (3.4 MB of it the tickets
//! array, most of them long-terminal), and every `store.save()` serialized
//! the whole blob — a thread dump caught the leader cycle spending its life
//! inside serde on that row, which read from the outside as a 30-minute
//! "freeze". Eviction is the structural fix: a terminal ticket that is not
//! in the current sprint and has no live children moves to the
//! `ArchiveStorePort` cold store (already served back by the F274 read
//! path), and its per-ticket bookkeeping maps are pruned with it.

use crate::state::ProjectState;
use coxagent_domain::Status;

/// Terminal statuses whose tickets no longer participate in any lane.
fn terminal(s: Status) -> bool {
    matches!(s, Status::Documented | Status::Verified | Status::Rejected)
}

/// Which tickets may leave the hot state right now. Pure over the state so
/// the policy is testable with a literal: terminal, not committed to the
/// current sprint, and not the parent of any still-live subtask.
#[must_use]
pub fn eviction_candidates(state: &ProjectState) -> Vec<String> {
    let sprint_ids: std::collections::HashSet<&str> = state
        .sprint
        .as_ref()
        .map(|s| s.committed.iter().map(coxagent_domain::TicketId::as_str).collect())
        .unwrap_or_default();
    state
        .tickets
        .iter()
        .filter(|t| terminal(t.status()))
        .filter(|t| !sprint_ids.contains(t.id().as_str()))
        .filter(|t| {
            // A parent with a live child stays: the child's "Split from" link
            // and the parent's hold-until-children-land contract need it hot.
            !state
                .tickets
                .iter()
                .any(|c| c.parent_id().map(coxagent_domain::TicketId::as_str) == Some(t.id().as_str())
                    && !terminal(c.status()))
        })
        .map(|t| t.id().to_string())
        .collect()
}

/// Drop one evicted ticket's per-ticket bookkeeping. The maps below are the
/// measured heavyweights (provenance/journal/failures each ~240 KB across
/// the board); entries for archived tickets are dead weight in every save.
pub fn prune_ticket_bookkeeping(state: &mut ProjectState, id: &str) {
    state.ticket_journal.remove(id);
    state.ticket_step_provenance.remove(id);
    state.ticket_failures.remove(id);
    state.ticket_evidence.remove(id);
    state.ticket_attachments.remove(id);
    state.ticket_fail_attempts.remove(id);
    state.ticket_redesigns.remove(id);
    state.hold_reasons.remove(id);
}

impl<S, E> super::RunCycleUseCase<S, E>
where
    S: crate::ports::outbound::StateStorePort,
    E: crate::ports::outbound::AgentEnginePort,
{
    /// Move eligible terminal tickets to the cold archive, bounded per cycle
    /// so one sweep never turns into its own stall. Archive-put first, drop
    /// from hot state only for tickets the cold store confirmed.
    pub(super) async fn evict_terminal_tickets(&self) {
        let Some(archive) = &self.archive else { return };
        // The archive is keyed by project id — the workspace directory name,
        // the same key the F274 read-back endpoint receives in its URL.
        let project = self
            .work_dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if project.is_empty() {
            return;
        }
        let candidates = {
            let Ok(state) = self.store.load().await else {
                return;
            };
            let mut c = eviction_candidates(&state);
            c.truncate(50); // bounded per cycle
            c
        };
        if candidates.is_empty() {
            return;
        }
        let mut archived: Vec<String> = Vec::new();
        {
            let Ok(state) = self.store.load().await else { return };
            for id in &candidates {
                let Some(t) = state.tickets.iter().find(|t| t.id().as_str() == id) else {
                    continue;
                };
                match archive.put(&project, t).await {
                    Ok(()) => archived.push(id.clone()),
                    Err(e) => {
                        tracing::warn!("eviction: cold-store put failed for {id}: {e}");
                    }
                }
            }
        }
        if archived.is_empty() {
            return;
        }
        let n = archived.len();
        // The removal MUST commit — a swallowed optimistic-concurrency
        // failure here reported "moved 50" while the hot row kept all 605
        // tickets (first live run of this sweep). mutate_state retries
        // conflicts internally; anything it still returns is a real failure
        // worth a loud warn, and the archive copies are harmless duplicates
        // the next sweep re-covers.
        match crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            for id in &archived {
                s.tickets.retain(|t| t.id().as_str() != id);
                prune_ticket_bookkeeping(s, id);
            }
            // Surviving tickets may still name the archived ones in
            // `depends_on`; the save validator rejects unknown ids, which
            // vetoed the whole sweep on the first live run. An archived
            // dependency is terminal — satisfied — so drop the edge.
            for t in &mut s.tickets {
                for id in &archived {
                    let _ = t.remove_dependency(coxagent_domain::Role::System, id);
                }
            }
            Ok(())
        })
        .await
        {
            Ok(()) => {
                self.report("SM", &format!("evicted {n} terminal ticket(s) to the archive"));
                tracing::info!("eviction: {n} terminal ticket(s) moved to the cold archive");
            }
            Err(e) => {
                tracing::warn!(
                    "eviction: archived {n} ticket(s) but hot-state removal did not commit: {e}"
                );
            }
        }
    }
}

#[cfg(test)]
mod eviction_tests {
    use super::*;
    use coxagent_domain::{Complexity, Priority, Role, Ticket, TicketId, TicketType};

    fn ticket(id: &str, status: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            format!("t {id}"),
            "d".to_owned(),
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("ticket");
        t.set_technical_design(
            Role::Sa,
            coxagent_domain::TechnicalDesign {
                approach: "x".into(),
                ..Default::default()
            },
        )
        .expect("design");
        match status {
            "documented" => {
                t.transition_to(Role::Sa, Status::Ready).unwrap();
                t.transition_to(Role::DevFeature, Status::InProgress).unwrap();
                t.transition_to(Role::DevFeature, Status::Done).unwrap();
                t.transition_to(Role::Docs, Status::Documented).unwrap();
            }
            "ready" => {
                t.transition_to(Role::Sa, Status::Ready).unwrap();
            }
            _ => {}
        }
        t
    }

    #[test]
    fn eviction_shrinks_the_serialized_row_and_spares_live_work() {
        let mut s = ProjectState::default();
        s.tickets.push(ticket("F001", "documented"));
        s.tickets.push(ticket("F002", "ready"));
        s.tickets.push(ticket("F003", "documented"));
        s.ticket_journal
            .insert("F001".into(), vec!["a long journal entry".into(); 50]);
        let before = serde_json::to_string(&s).expect("serialize").len();

        let mut cands = eviction_candidates(&s);
        cands.sort();
        assert_eq!(cands, vec!["F001".to_string(), "F003".to_string()]);

        for id in &cands {
            s.tickets.retain(|t| t.id().as_str() != id);
            prune_ticket_bookkeeping(&mut s, id);
        }
        let after = serde_json::to_string(&s).expect("serialize").len();
        // The review mandate for this ticket line: assert the ROW SHRINKS.
        assert!(
            after < before,
            "eviction must shrink the serialized state ({before} -> {after})"
        );
        assert!(s.tickets.iter().any(|t| t.id().as_str() == "F002"));
    }

    #[test]
    fn evicting_a_dependency_target_drops_the_edge_on_survivors() {
        let mut s = ProjectState::default();
        s.tickets.push(ticket("F020", "documented"));
        let mut live = ticket("F021", "ready");
        live.add_dependency(Role::Sa, TicketId::new("F020").unwrap())
            .unwrap();
        s.tickets.push(live);

        let cands = eviction_candidates(&s);
        assert_eq!(cands, vec!["F020".to_string()]);
        for id in &cands {
            s.tickets.retain(|t| t.id().as_str() != id);
            prune_ticket_bookkeeping(&mut s, id);
        }
        for t in &mut s.tickets {
            for id in &cands {
                let _ = t.remove_dependency(Role::System, id);
            }
        }
        assert!(
            s.tickets[0].depends_on().is_empty(),
            "a dangling dependency on an archived ticket must be dropped"
        );
    }

    #[test]
    fn a_parent_with_a_live_child_is_not_evicted() {
        let mut s = ProjectState::default();
        s.tickets.push(ticket("F010", "documented"));
        let mut child = ticket("F011", "ready");
        child
            .set_parent(Role::Sa, TicketId::new("F010").unwrap())
            .unwrap();
        s.tickets.push(child);
        assert!(eviction_candidates(&s).is_empty(), "live child pins the parent");
    }
}
