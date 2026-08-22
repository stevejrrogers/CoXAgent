// Part of the run_dev module split by concern — see run_dev/mod.rs.
#![allow(clippy::wildcard_imports)]
//! How a failed attempt is recorded: counted against the ticket, journaled
//! for the next try, or — when the fault is infrastructure — raised as an
//! engine incident and NOT counted, because a revoked token is nobody's bug.

use super::*;

/// Most exception tickets routed to one person at a time — beyond this a
/// parked ticket stays unassigned (a systemic failure, not personal work).
const MAX_ROUTED: usize = 5;

/// How many not-yet-finished tickets are already routed to `owner`.
fn routed_open_count(s: &crate::state::ProjectState, owner: Option<&str>) -> usize {
    use coxagent_domain::Status;
    s.tickets
        .iter()
        .filter(|t| {
            owner == t.assignee()
                && !matches!(
                    t.status(),
                    Status::Done | Status::Verified | Status::Documented | Status::Rejected
                )
        })
        .count()
}

/// Signature of a ticket that cannot be built honestly against the codebase —
/// the "bịa ngáo" (fabricated/unresolvable) class: the acceptance criteria or
/// a test fixture demands data/state that does not exist, or the agent hit an
/// impossible constraint. Heuristic over the failure detail and the last run
/// output; matching is deliberately loose because agents phrase these many
/// ways, but it only fires AFTER 3 real failed attempts, so false positives
/// mean closing a ticket the team genuinely could not do — acceptable.
fn is_unresolvable_failure(why: &str, full_why: &str) -> bool {
    let hay = format!("{why} {full_why}").to_ascii_lowercase();
    [
        "does not exist",
        "doesn't exist",
        "cannot satisfy",
        "can't satisfy",
        "impossible",
        "no data",
        "nowhere",
        "no per-ticket",
        "design gap",
        "not in the codebase",
        "fabricate",
        "fabricated",
        "no such field",
        "no such state",
        "word salad",
        "unresolvable",
    ]
    .iter()
    .any(|k| hay.contains(k))
}

impl<S: StateStorePort, E: AgentEnginePort> RunDevUseCase<S, E> {
    /// Count a failed attempt on `id`; at the 3rd, park it with a visible note
    /// so a human decides instead of the team burning tokens forever.
    pub(super) async fn record_failure(&self, id: &TicketId, why: &str) {
        self.record_failure_at(
            id,
            why,
            crate::state::FailureLayer::Design,
            "engine",
            Vec::new(),
        )
        .await;
    }

    /// As [`Self::record_failure`], but the caller names the layer and gate it
    /// rejected the work at, so the next agent reads data instead of guessing
    /// from a sentence.
    pub(super) async fn record_failure_at(
        &self,
        id: &TicketId,
        why: &str,
        layer: crate::state::FailureLayer,
        gate: &str,
        files: Vec<String>,
    ) {
        let key = id.to_string();
        let short: String = why.chars().take(300).collect();
        // Infrastructure faults are NOT the ticket's fault — shared predicate
        // with the runner's circuit breaker (see crate::faults).
        let infra = crate::faults::is_infra_fault(why);
        let _ = layer;
        if infra {
            // Raise it where people look. An outage that only exists as a log
            // line means the team looks broken while the real problem is an
            // expired login nobody was told about.
            let (engine, role, detail) = (
                self.engine.id().to_owned(),
                format!("{:?}", self.mode.role()),
                short.clone(),
            );
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                s.log_activity(
                    "SYSTEM",
                    "engine infrastructure fault — attempt not counted",
                    Some(key.clone()),
                );
                let first = !s.engine_incidents.iter().any(|i| i.engine == engine);
                s.open_engine_incident(&engine, &role, &detail);
                if first {
                    let msg = format!(
                        "🔌 {engine} is failing for every agent: {detail}. Work is paused on this \
                         engine until it answers again — fix the credentials or the model, and \
                         this clears itself."
                    );
                    s.post_chat_in("SYSTEM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                }
                Ok(())
            })
            .await;
            return;
        }
        let route_to = self.config.workflow.human.route_exceptions_to.clone();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            let n = {
                let c = s.ticket_fail_attempts.entry(key.clone()).or_insert(0);
                *c += 1;
                *c
            };
            // Brief the NEXT attempt on what this one hit, so a retry builds
            // on prior findings instead of rediscovering them.
            s.journal_note(&key, &format!("attempt {n} failed: {short}"));
            s.record_attempt_failure(
                &key,
                crate::state::AttemptFailure {
                    attempt: n,
                    layer,
                    gate: gate.to_owned(),
                    detail: short.clone(),
                    files: files.clone(),
                },
            );
            if n == 3 {
                // "Bịa ngáo" (fabricated/unresolvable) ticket: after 3 honest
                // attempts the team could not build it because the task itself
                // is impossible against the codebase — the AC needs data/state
                // that does not exist, or the design is incoherent. That is not
                // a routing problem (a human cannot code a lie either), so close
                // it Rejected with a finding via `System` (the one actor allowed
                // to reject) instead of routing it onward forever. Mirrors the
                // phantom-bug close in run_dev — a ticket that cannot be done
                // honestly should LEAVE the queue, not linger parked.
                if is_unresolvable_failure(&short, &why) {
                    let id_c = id.clone();
                    s.post_comment(
                        "SYSTEM",
                        &format!(
                            "🚫 {id} CLOSED as unresolvable after 3 failed attempts: this \
                             ticket cannot be built honestly against the current codebase \
                             (fabricated data / missing state / incoherent design). Last \
                             failure: {short} — the owning role should re-specify or split it."
                        ),
                        Some(key.clone()),
                    );
                    if transition(s, &id_c, coxagent_domain::Role::System, Status::Rejected).is_ok()
                    {
                        s.journal_note(
                            key.as_str(),
                            "closed unresolvable after 3 failed attempts (design gap)",
                        );
                        s.ticket_fail_attempts.remove(&id_c.to_string());
                        return Ok(());
                    }
                    // Transition not allowed for this ticket state — fall through
                    // to the normal park-and-route path rather than leaving it
                    // silently stranded.
                }
                s.post_comment(
                    "DEV-BUG",
                    &format!(
                        "⛔ {id} PARKED after 3 failed attempts (last: {short}) — needs a \
                         human decision; agents will skip it."
                    ),
                    Some(key.clone()),
                );
                // Exception routing, CAPPED at MAX_ROUTED: a systemic failure
                // parks tickets by the dozen — routing them all buried the owner
                // under 58 assignments in one night, and assigned tickets are
                // invisible to agents, so the flood also starved DEV. Past the
                // cap the ticket parks unassigned.
                let routed_already = routed_open_count(s, route_to.as_deref());
                if let Some(owner) = &route_to {
                    if routed_already < MAX_ROUTED {
                        if let Some(t) = s.ticket_mut(id) {
                            t.assign_to_human(owner);
                        }
                        s.post_comment(
                            "SYSTEM",
                            &format!(
                                "🧑‍💻 {id} routed to @{owner} (workflow.human.route_exceptions_to)."
                            ),
                            Some(key.clone()),
                        );
                    } else {
                        s.post_comment(
                            "SYSTEM",
                            &format!(
                                "⛔ {id} parked (NOT routed — @{owner} already has {routed_already} \
                                 routed tickets; likely a systemic failure, fix the cause and the \
                                 parked set clears together)."
                            ),
                            Some(key.clone()),
                        );
                    }
                }
            }
            Ok(())
        })
        .await;
    }
}
