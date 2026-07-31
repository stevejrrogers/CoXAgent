// Part of the run_dev module split by concern — see run_dev/mod.rs.
#![allow(clippy::wildcard_imports)]
//! How a failed attempt is recorded: counted against the ticket, journaled
//! for the next try, or — when the fault is infrastructure — raised as an
//! engine incident and NOT counted, because a revoked token is nobody's bug.

use super::*;

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
                s.post_comment(
                    "DEV-BUG",
                    &format!(
                        "⛔ {id} PARKED after 3 failed attempts (last: {short}) — needs a \
                         human decision; agents will skip it."
                    ),
                    Some(key.clone()),
                );
            }
            Ok(())
        })
        .await;
    }
}
