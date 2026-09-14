// Part of the cycle module split by concern — see cycle/mod.rs.
//! The SM as a real orchestrator, not a figurehead: every leader cycle it
//! polices the sprint deterministically (zero tokens) — work outside the
//! committed scope gets called out and legalised, committed work that is not
//! moving mid-sprint gets routed to the role that can unblock it (SA for a
//! missing design, PO for a cost hold), and near the end untouched features
//! are descoped so the sprint closes honest instead of carrying everything
//! over with velocity 0%.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use coxagent_domain::{Status, TicketType};

/// Sprint progress as a fraction of its wall-clock window (Days policy) —
/// `None` when unknown (no sprint, unparseable stamp), which disables the
/// time-based interventions but never the scope police.
fn sprint_fraction(state: &crate::state::ProjectState, days: u64) -> Option<f64> {
    let sprint = state.sprint.as_ref()?;
    let fmt = &time::format_description::well_known::Rfc3339;
    let started = time::OffsetDateTime::parse(&sprint.started_at, fmt).ok()?;
    let elapsed = (time::OffsetDateTime::now_utc() - started).whole_minutes();
    let window = i64::try_from(days.max(1) * 24 * 60).unwrap_or(i64::MAX);
    #[allow(clippy::cast_precision_loss)]
    Some((elapsed.max(0) as f64 / window as f64).min(2.0))
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// One deterministic SM supervision pass. Leader-only, scrum-only; every
    /// intervention fires at most once per sprint per ticket (guarded through
    /// `daily_jobs` keys), so the SM speaks when something changes, not on a
    /// loop.
    pub(super) async fn sm_sprint_watch(&self) {
        if self.config.workflow.mode != crate::config::Mode::Scrum {
            return;
        }
        let Ok(mut state) = self.store.load().await else {
            return;
        };
        let Some(sprint) = state.sprint.clone() else {
            return;
        };
        let vi = self.config.workflow.language.is_vi();
        let frac = sprint_fraction(&state, self.config.workflow.sprint_length_days);
        let mut changed = Self::police_scope(&mut state, &sprint, vi);
        if frac.is_some_and(|f| f >= 0.5) {
            changed |= Self::chase_stalled(&mut state, &sprint);
        }
        if frac.is_some_and(|f| f >= 0.8) {
            changed |= Self::descope_endgame(&mut state, &sprint);
        }
        if changed {
            let _ = self.store.save(&state).await;
        }
    }

    /// (1) Scope police: someone is WORKING a feature/chore the team never
    /// committed. Call it out and commit it — visible scope beats silent
    /// scope, and stopping half-done work wastes more than finishing it.
    fn police_scope(
        state: &mut crate::state::ProjectState,
        sprint: &crate::state::Sprint,
        vi: bool,
    ) -> bool {
        let mut changed = false;
        let rogue: Vec<String> = state
            .tickets
            .iter()
            .filter(|t| {
                t.status() == Status::InProgress
                    && matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                    && !sprint.committed.iter().any(|c| c == t.id())
            })
            .map(|t| t.id().to_string())
            .collect();
        for id in rogue {
            let guard = format!("sm_watch:scope:{}:{id}", sprint.number);
            if state.daily_jobs.contains_key(&guard) {
                continue;
            }
            state.daily_jobs.insert(guard, "1".to_owned());
            if let Ok(tid) = coxagent_domain::TicketId::new(&id) {
                let _ = crate::sprint::commit_ticket(state, &tid);
            }
            let msg = if vi {
                format!(
                    "🧭 SM: {id} đang được làm nhưng KHÔNG nằm trong scope sprint \
                     {} — lần sau phải qua PO/SM commit trước khi kéo việc. Đã đưa \
                     vào sprint cho minh bạch.",
                    sprint.number
                )
            } else {
                format!(
                    "🧭 SM: {id} is being worked but was NEVER committed to sprint \
                     {} — next time scope goes through PO/SM before work starts. \
                     Committed it now so the board stays honest.",
                    sprint.number
                )
            };
            state.post_comment("SM", &msg, None);
            state.log_activity("SM", "flagged out-of-scope work", Some(id));
            changed = true;
        }
        changed
    }

    /// (2) Mid-sprint risk radar: half the window is gone and a committed
    /// ticket has not moved. Route each one to the role that can unblock it
    /// instead of watching it slide into the retro.
    fn chase_stalled(
        state: &mut crate::state::ProjectState,
        sprint: &crate::state::Sprint,
    ) -> bool {
        let mut changed = false;
        {
            let committed = sprint.committed.clone();
            for id in committed {
                let Some(t) = state.tickets.iter().find(|t| t.id() == &id) else {
                    continue;
                };
                let untouched =
                    matches!(t.status(), Status::Open | Status::Pending | Status::Ready);
                if !untouched {
                    continue;
                }
                let guard = format!("sm_watch:risk:{}:{id}", sprint.number);
                if state.daily_jobs.contains_key(&guard) {
                    continue;
                }
                let missing_design =
                    t.status() == Status::Pending && t.design().technical.is_none();
                let cost_held = state.cost_holds.contains_key(id.as_str())
                    && !state.cost_approved.contains(&id.to_string());
                state.daily_jobs.insert(guard, "1".to_owned());
                changed = true;
                if missing_design {
                    // The SA is the unblock: no design, no DEV.
                    state.ask_question(
                        id.as_str(),
                        "SM",
                        "SA",
                        &format!(
                            "Sprint {} is half over and {id} still has no technical \
                             design — it cannot reach DEV without one. Design it this \
                             cycle or tell me it must be descoped.",
                            sprint.number
                        ),
                    );
                    state.log_activity("SM", "escalated missing design", Some(id.to_string()));
                } else if cost_held {
                    // Only a person (PO) clears a cost hold — say so where
                    // approvals are watched.
                    let msg = format!(
                        "⏳ SM: {id} is committed to sprint {} but sits behind a cost \
                         hold — approve or reject it, half the sprint is gone.",
                        sprint.number
                    );
                    state.post_chat_in("SM", &msg, crate::state::APPROVALS_CHANNEL, Vec::new());
                    state.log_activity("SM", "chased a cost hold", Some(id.to_string()));
                } else {
                    let msg = format!(
                        "⏱️ SM: half of sprint {} is gone and {id} is untouched — \
                         DEV, pull this next or say what blocks it.",
                        sprint.number
                    );
                    state.post_comment("SM", &msg, None);
                    state.log_activity("SM", "nudged a stalled ticket", Some(id.to_string()));
                }
            }
        }
        changed
    }

    /// (3) Endgame descope: 80% of the window gone, a committed FEATURE/CHORE
    /// still untouched — it will not ship. Uncommit it now so the sprint
    /// closes honest (bugs stay: they outrank the board by rule).
    fn descope_endgame(
        state: &mut crate::state::ProjectState,
        sprint: &crate::state::Sprint,
    ) -> bool {
        let mut changed = false;
        {
            let victims: Vec<coxagent_domain::TicketId> = state
                .tickets
                .iter()
                .filter(|t| {
                    sprint.committed.iter().any(|c| c == t.id())
                        && matches!(t.ticket_type(), TicketType::Feature | TicketType::Chore)
                        && matches!(t.status(), Status::Pending | Status::Ready)
                })
                .map(|t| t.id().clone())
                .collect();
            for id in victims {
                let guard = format!("sm_watch:descope:{}:{id}", sprint.number);
                if state.daily_jobs.contains_key(&guard) {
                    continue;
                }
                state.daily_jobs.insert(guard, "1".to_owned());
                if crate::sprint::uncommit_ticket(state, &id) {
                    let msg = format!(
                        "✂️ SM: descoping {id} from sprint {} — 80% of the window is \
                         gone and it was never started. It returns to the backlog; \
                         next planning pulls a smaller, clearer slice.",
                        sprint.number
                    );
                    state.post_comment("SM", &msg, None);
                    state.log_activity("SM", "descoped an unstarted ticket", Some(id.to_string()));
                    changed = true;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::sprint_fraction;
    use crate::state::ProjectState;

    fn with_sprint(started_at: &str) -> ProjectState {
        ProjectState {
            sprint: Some(crate::state::Sprint {
                number: 1,
                goal: String::new(),
                started_cycle: 0,
                length_cycles: 5,
                committed: Vec::new(),
                started_at: started_at.to_owned(),
                bug_burn_floor: None,
            }),
            ..ProjectState::default()
        }
    }

    #[test]
    fn fraction_reads_the_wall_clock() {
        // Ancient sprint: fraction caps at 2.0 (way past the window).
        let s = with_sprint("2020-01-01T00:00:00Z");
        assert_eq!(sprint_fraction(&s, 1), Some(2.0));
        // Unparseable stamp disables time-based interventions.
        let s = with_sprint("");
        assert_eq!(sprint_fraction(&s, 1), None);
        // No sprint at all.
        assert_eq!(sprint_fraction(&ProjectState::default(), 1), None);
    }
}
