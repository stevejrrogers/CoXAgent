// Part of the cycle module split by concern — see cycle/mod.rs.
#![allow(clippy::wildcard_imports)]
//! CXA-F306 lesson efficacy loop, cycle side: the matching step that feeds
//! the ledger (AC1/AC4/AC5) and the daily forcing sweep that turns repeating
//! lessons into structural work (the chore→bug ladder).
//!
//! Split out of `ops.rs` (which only CALLS the matcher, three lines) so the
//! post-mortem file keeps its size and this loop owns its own seam. Every
//! step is best-effort by contract (AC5): a failure inside matching or the
//! sweep must never delay or break the cycle, so nothing here returns a
//! caller-visible error and every store write is swallowed.

use super::*;
use crate::state::ProjectState;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// AC1: match a recorded post-mortem summary against existing project + hub
    /// lessons; a hit above the house similarity threshold increments that
    /// lesson's recurrence count exactly once for this incident, and the linked
    /// incident stays visible on the Hub lessons page. A hub match also lands in
    /// the hub sidecar — and an evicted-but-retained hub lesson is re-surfaced
    /// with its prior recurrence history (AC4). No plausible match records
    /// nothing (AC5); every failure path returns silently.
    pub(super) async fn match_lesson_recurrence(
        &self,
        summary: &str,
        reason: &str,
        incident_at: &str,
    ) {
        if summary.trim().is_empty() || incident_at.is_empty() {
            return;
        }
        let Ok(state) = self.store.load().await else {
            return;
        };
        // Project candidates: the efficacy ledger plus the prompt-facing list
        // (a lesson recorded before CXA-F306 has no ledger row yet).
        let mut candidates: Vec<String> = state
            .lesson_records
            .iter()
            .map(|r| r.text.clone())
            .collect();
        for lesson in &state.lessons {
            if !candidates.contains(lesson) {
                candidates.push(lesson.clone());
            }
        }
        // Hub candidates: the live md bullets plus the retained shelf, so a
        // lesson evicted by the 30-entry cap stays matchable (AC4).
        let md_path = crate::prompts::hub_lessons_path();
        let md_texts: Vec<String> = match self.files.as_deref() {
            Some(files) => files
                .read(&md_path)
                .await
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("- ")
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                })
                .map(ToOwned::to_owned)
                .collect(),
            None => Vec::new(),
        };
        let hub_store = crate::hub_lessons::read_store(self.files.as_deref(), &md_path).await;
        for text in md_texts
            .iter()
            .chain(crate::hub_lessons::candidate_texts(&hub_store).iter())
        {
            if !candidates.contains(text) {
                candidates.push(text.clone());
            }
        }
        let refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
        // AC5: an incident with no plausible matching lesson creates no match.
        let Some(hit) = crate::lesson_efficacy::match_lessons(summary, &refs) else {
            return;
        };
        // AC3: a dismissed match never increments.
        if state.match_is_dismissed(&hit.text, incident_at) {
            return;
        }
        let from_hub = md_texts.contains(&hit.text)
            || hub_store
                .lessons
                .iter()
                .chain(hub_store.retained.iter())
                .any(|m| m.text == hit.text);
        // Hub sidecar first: it may resurface a retained lesson and hands back
        // the FULL history (prior + this hit) to mirror into the project ledger.
        let prior = if from_hub {
            crate::hub_lessons::record_recurrence(
                self.files.as_deref(),
                &md_path,
                &hit.text,
                incident_at,
                reason,
            )
            .await
        } else {
            Vec::new()
        };
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.record_recurrence(&hit.text, incident_at, reason, &prior);
            Ok(())
        })
        .await;
    }

    /// The CXA-F306 daily forcing sweep: a lesson with 2+ recurrences (or one
    /// re-learned twice) must produce structural work, not another reminder.
    /// Ladder: the first escalation files ONE Chore; if that chore closed and
    /// the lesson STILL recurred afterwards, the rung below is a Bug. Idempotent
    /// per UTC day, deduped per lesson by its escalation record.
    pub(super) async fn lesson_efficacy_sweep(&self) {
        let today = crate::state::now_rfc3339();
        let today = today.get(..10).unwrap_or("").to_owned();
        if today.is_empty() {
            return;
        }
        let Ok(state) = self.store.load().await else {
            return;
        };
        if state.lesson_sweep_day == today {
            return;
        }
        for (text, stage) in sweep_actions(&state) {
            if let Some(ticket) = self.file_prevention_ticket(&text, stage).await {
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    s.mark_escalated(&text, ticket.as_str(), stage);
                    Ok(())
                })
                .await;
            }
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.lesson_sweep_day.clone_from(&today);
            Ok(())
        })
        .await;
    }

    /// File the structural ticket for one repeating lesson: a `Chore` on the
    /// first rung, a `Bug` once a closed chore failed to stop the recurrence.
    /// The shared `AddTicketUseCase` duplicate gate (active near-identical
    /// titles) is the second dedupe line behind the escalation record.
    async fn file_prevention_ticket(
        &self,
        lesson: &str,
        stage: &str,
    ) -> Option<coxagent_domain::TicketId> {
        use coxagent_domain::ticket::{Complexity, Priority, TicketType};
        let (ticket_type, priority, marker) = if stage == "bug" {
            (TicketType::Bug, Priority::High, "Structural fix required")
        } else {
            (TicketType::Chore, Priority::Medium, "Prevention")
        };
        let title = format!("{marker}: {}", lesson.chars().take(80).collect::<String>());
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        adder
            .execute(crate::use_cases::AddTicketInput {
                ticket_type,
                title,
                description: format!(
                    "CXA-F306 lesson efficacy: this lesson keeps recurring — its failure class \
                     matched 2+ incidents (or was re-learned twice) after it was recorded. \
                     Stop the class, not the symptom.\n\nLesson: {lesson}"
                ),
                priority,
                complexity: Complexity::Medium,
                has_ui: false,
                acceptance_criteria: vec![
                    "The lesson's failure class cannot recur without being caught by a \
                     check, not a lesson"
                        .to_owned(),
                ],
                goal: None,
                // Machine-filed prevention ticket: no BA-authored
                // shared-infrastructure tag exists here.
                service_tag: None,
            })
            .await
            .ok()
    }
}

/// The pure ladder decision: which repeating lessons need which rung filed
/// right now. Over state only, so it pins the forcing rules without any IO.
fn sweep_actions(state: &ProjectState) -> Vec<(String, &'static str)> {
    let closed = |ticket: &str| {
        state.tickets.iter().any(|t| {
            t.id().as_str() == ticket
                && matches!(
                    t.status(),
                    coxagent_domain::Status::Done
                        | coxagent_domain::Status::Documented
                        | coxagent_domain::Status::Rejected
                )
        })
    };
    state
        .lesson_records
        .iter()
        .filter(|r| r.is_repeating())
        .filter_map(|record| match &record.escalated {
            // No structural work yet: file the chore rung.
            None => Some((record.text.clone(), "chore")),
            Some(escalation) if escalation.stage == "chore" => {
                if closed(&escalation.ticket)
                    && record
                        .recurrences
                        .iter()
                        .any(|e| e.at.as_str() > escalation.at.as_str())
                {
                    // The chore closed and the class recurred anyway: bug rung.
                    Some((record.text.clone(), "bug"))
                } else {
                    None
                }
            }
            // Open chore pending, or already escalated to a bug: wait.
            Some(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::state::ProjectState;
    use crate::state::{LessonEscalation, LessonRecurrence};
    use coxagent_domain::{Status, Ticket};

    fn ticket(id: &str, status: Status) -> Ticket {
        let json = serde_json::json!({
            "id": id,
            "type": "chore",
            "title": "Prevention: pin the base image version",
            "description": "",
            "priority": "medium",
            "complexity": "medium",
            "status": status,
            "has_ui": false,
            "depends_on": [],
            "design": {"technical": null, "ux": null},
            "parent_id": null,
        });
        serde_json::from_value(json).expect("ticket fixture")
    }

    fn recurring(text: &str, count: usize) -> crate::state::LessonRecord {
        let mut r = crate::state::LessonRecord {
            text: text.to_owned(),
            at: "2026-09-01T10:00:00Z".to_owned(),
            cycle: 1,
            re_recordings: 0,
            recurrences: Vec::new(),
            escalated: None,
            id: None,
            source: None,
        };
        for i in 0..count {
            r.recurrences.push(LessonRecurrence {
                at: format!("2026-09-0{}T10:00:00Z", i + 1),
                incident_at: format!("2026-09-0{}T09:00:00Z", i + 1),
                incident_reason: "deploy failed".to_owned(),
            });
        }
        r
    }

    #[test]
    fn a_repeater_without_structural_work_files_the_chore_rung() {
        let mut s = ProjectState::default();
        s.lesson_records.push(recurring("pin the base image", 2));
        let actions = sweep_actions(&s);
        assert_eq!(actions, vec![("pin the base image".to_owned(), "chore")]);
    }

    #[test]
    fn one_recurrence_never_files_anything() {
        let mut s = ProjectState::default();
        s.lesson_records.push(recurring("watched once", 1));
        assert!(
            sweep_actions(&s).is_empty(),
            "AC2: only 2+ recurrences force work"
        );
    }

    #[test]
    fn an_open_chore_waits_and_a_closed_chore_that_failed_climbs_to_bug() {
        let mut s = ProjectState::default();
        let mut record = recurring("pin the base image", 2);
        record.escalated = Some(LessonEscalation {
            ticket: "CXC-C001".to_owned(),
            at: "2026-09-02T10:00:00Z".to_owned(),
            stage: "chore".to_owned(),
        });
        s.lesson_records.push(record);
        s.tickets.push(ticket("CXC-C001", Status::Ready));
        assert!(
            sweep_actions(&s).is_empty(),
            "an open chore is the escalation in flight"
        );

        s.tickets.clear();
        s.tickets.push(ticket("CXC-C001", Status::Documented));
        assert!(
            sweep_actions(&s).is_empty(),
            "a closed chore with NO recurrence after it stays done"
        );

        let mut s = s.clone();
        if let Some(record) = s.lesson_records.first_mut() {
            record.recurrences.push(LessonRecurrence {
                at: "2026-09-05T10:00:00Z".to_owned(),
                incident_at: "2026-09-05T09:00:00Z".to_owned(),
                incident_reason: "deploy failed".to_owned(),
            });
        }
        assert_eq!(
            sweep_actions(&s),
            vec![("pin the base image".to_owned(), "bug")],
            "closed chore + recurrence after it = the bug rung"
        );
    }

    #[test]
    fn a_bug_rung_escalation_is_the_top_of_the_ladder() {
        let mut s = ProjectState::default();
        let mut record = recurring("pin the base image", 2);
        record.escalated = Some(LessonEscalation {
            ticket: "CXC-B002".to_owned(),
            at: "2026-09-05T10:00:00Z".to_owned(),
            stage: "bug".to_owned(),
        });
        s.lesson_records.push(record);
        assert!(sweep_actions(&s).is_empty(), "nothing above the bug rung");
    }
}
