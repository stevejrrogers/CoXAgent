//! When work gets stuck: answering the questions agents ask each other, and
//! escalating a parked ticket to whoever can actually unstick it.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::use_cases::merge_policy::{
    escalation_route, route_from_failures, EscalationRoute, MAX_TICKET_RESCUES,
};

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Answer the questions agents asked each other. The BA answers what a
    /// requirement means; the SA reads the code and reports what the system
    /// actually does. Both get the wiki, the docs and the code graph, because
    /// an answer invented at the desk is worse than the guess it replaces.
    /// At most two per cycle — an answer costs a call, and a queue of them
    /// means the backlog, not the questions, is the problem.
    #[allow(clippy::too_many_lines)] // one question, one hand-off, one answer
    pub(super) async fn answer_open_questions(&self) {
        let open: Vec<crate::state::AgentQuestion> = {
            let Ok(state) = self.store.load().await else {
                return;
            };
            state
                .questions
                .iter()
                // Questions addressed to a PERSON (`@username`) are theirs —
                // they sit in that user's inbox with an SLA, not in the agent
                // answering queue.
                .filter(|q| q.is_open() && !q.to.starts_with('@'))
                .take(2)
                .cloned()
                .collect()
        };
        for q in open {
            let Ok(state) = self.store.load().await else {
                return;
            };
            let ticket = state
                .tickets
                .iter()
                .find(|t| t.id().to_string() == q.ticket);
            let (persona, role) = if q.to == "BA" {
                (crate::prompts::BA, coxagent_domain::Role::Ba)
            } else {
                (crate::prompts::SA, coxagent_domain::Role::Sa)
            };
            self.report(&q.to, &format!("answering {}", q.id));
            // The BA that lacks the code and the SA that lacks the ticket are
            // each one hop from someone who has it — but only one hop, or the
            // question ping-pongs. After that, the SA answers from the code:
            // when nobody remembers, the code is the only thing that knows.
            let forward_rule = if q.forwarded {
                " Nobody could answer this from memory — it already came to you from the other \
                 role. Do NOT hand it back: read the code and answer from what it actually does."
            } else if q.to == "BA" {
                " If this is really a question about how the system works rather than what the \
                 business wants, hand it over: reply with exactly `ASK SA: <question>` and \
                 nothing else."
            } else {
                " If this is really a question about what the business wants rather than how the \
                 system works, hand it over: reply with exactly `ASK BA: <question>` and nothing \
                 else."
            };
            let subject = ticket.map_or_else(
                || q.body.clone(),
                |t| format!("{} {} {}", t.title(), t.description(), q.body),
            );
            let brief = format!(
                "{} asked you about {}:\n\n\"{}\"\n\nAnswer it so the work can continue. Ground \
                 every claim in this repository — read the code and the pages below, name the \
                 files and identifiers you relied on, and if the honest answer is \"the product \
                 does not do this yet\", say that. Under 900 characters, plain text, no \
                 preamble.\n\nAn answer that leaves the asker still deciding is not an answer. \
                 END with one line, exactly:\n`ACTION: <what they should do now>` — the concrete \
                 next step, not a restatement.{forward_rule}{}{}{}",
                q.from,
                if q.ticket.is_empty() {
                    "the product".to_owned()
                } else {
                    format!("ticket {}", q.ticket)
                },
                q.body,
                crate::prompts::knowledge_block(
                    self.files.as_deref(),
                    &state.docs,
                    &state.tickets,
                    &self.work_dir,
                    &subject,
                    &q.ticket,
                )
                .await,
                crate::prompts::focus_block(self.files.as_deref(), &self.work_dir, &subject).await,
                crate::prompts::repo_map_block(
                    self.files.as_deref(),
                    &self.work_dir,
                    self.config.workflow.token_saver,
                )
                .await,
            );
            let out = self
                .engine
                .run(crate::ports::outbound::AgentRequest {
                    role,
                    system_prompt: crate::prompts::system_prompt(persona),
                    task_prompt: brief,
                    work_dir: self.work_dir.clone(),
                    timeout: std::time::Duration::from_secs(900),
                    escalation_level: 0,
                    label: None,
                })
                .await;
            let answer = match out {
                Ok(o) if o.succeeded() => o.stdout.trim().to_owned(),
                _ => continue,
            };
            // A hand-off, not an answer: re-target the question and let the
            // other role take it next pass.
            if let Some((to, forwarded_q)) = crate::use_cases::run_dev::parse_ask(&answer) {
                let (id, from_role) = (q.id.clone(), q.to.clone());
                let tkt = q.ticket.clone();
                let moved = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    if s.forward_question(&id, &to) {
                        let msg = format!("↪️ {from_role} → {to}: {forwarded_q}");
                        s.post_comment(&from_role, &msg, (!tkt.is_empty()).then(|| tkt.clone()));
                    }
                    Ok(())
                })
                .await;
                if moved.is_ok() {
                    continue;
                }
            }
            if answer.len() < 30 {
                continue; // nothing usable; it stays open for the next cycle
            }
            // A discussion that ends without a decision leaves the asker
            // exactly where it started. Ask once for the missing decision —
            // but keep the answer either way: an answer without a label still
            // unblocks the work, and losing it to a formatting rule would be
            // the rigid choice.
            let answer = if answer.to_uppercase().contains("ACTION:") {
                answer
            } else {
                match self
                    .engine
                    .run(crate::ports::outbound::AgentRequest {
                        role,
                        system_prompt: crate::prompts::system_prompt(persona),
                        task_prompt: format!(
                            "Your answer below has no decision in it. Repeat it unchanged, then \
                             add a final line `ACTION: <the concrete next step for {}>`.\n\n{answer}",
                            q.from
                        ),
                        work_dir: self.work_dir.clone(),
                        timeout: std::time::Duration::from_secs(600),
                        escalation_level: 0,
                        label: None,
                    })
                    .await
                {
                    Ok(o) if o.succeeded() && o.stdout.to_uppercase().contains("ACTION:") => o.stdout.trim().to_owned(),
                    _ => answer,
                }
            };
            let (id, to, from) = (q.id.clone(), q.to.clone(), q.from.clone());
            let tkt = q.ticket.clone();
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                if s.answer_question(&id, &answer) {
                    let msg = format!("💬 {to} → {from} ({id}): {answer}");
                    s.post_comment(&to, &msg, (!tkt.is_empty()).then(|| tkt.clone()));
                }
                Ok(())
            })
            .await;
        }
        if let Some(p) = &self.phase {
            p(None);
        }
    }
    /// SM escalation for tickets PARKED after 3 red builds — the stand-in for
    /// what a real team does when a dev is stuck: someone senior picks it up,
    /// and WHICH someone depends on why it kept failing.
    ///
    /// The three attempts' failure reasons are read from the ticket journal and
    /// routed: a ticket that never had a workable spec goes to the BA to be
    /// made answerable; one blocked on a mechanical gate (lints, a missing
    /// regression test) goes to the SA as a repair brief, because redesigning
    /// an approach that was never the problem just burns another three
    /// attempts; anything else is a genuine design dead end and gets the SA's
    /// revised approach. Every brief now carries the actual failures — the SA
    /// used to be told only that the ticket was parked. One escalation per
    /// ticket; parking again after that is a human decision.
    #[allow(clippy::too_many_lines)] // one escalation, three routes, read top to bottom
    pub(super) async fn sm_unpark_tickets(&self) {
        let candidates: Vec<(String, String)> = {
            let Ok(state) = self.store.load().await else {
                return;
            };
            state
                .ticket_fail_attempts
                .iter()
                .filter(|(id, n)| {
                    **n >= 3
                        && state.ticket_redesigns.get(*id).copied().unwrap_or(0)
                            < MAX_TICKET_RESCUES
                })
                .filter_map(|(id, _)| {
                    state
                        .tickets
                        .iter()
                        .find(|t| t.id().to_string() == *id)
                        .map(|t| (id.clone(), t.title().to_owned()))
                })
                .take(1) // one redesign per cycle bounds cost
                .collect()
        };
        for (id, title) in candidates {
            Box::pin(self.escalate_parked_ticket(id, title)).await;
        }
    }
    /// One escalation: read what actually failed, pick the senior who can
    /// unstick it, and hand the ticket back to the flow. Split out of
    /// `sm_unpark_tickets` so the cycle's future stays small — this body
    /// holds a whole failure log across its awaits.
    #[allow(clippy::too_many_lines)] // one escalation, three routes, read top to bottom
    pub(super) async fn escalate_parked_ticket(&self, id: String, title: String) {
        // What actually went wrong, in the words the gates recorded.
        let (history, spec_gap, structured) = {
            let Ok(state) = self.store.load().await else {
                return;
            };
            let structured = state.attempt_failures(&id).to_vec();
            let hist = if structured.is_empty() {
                state
                    .ticket_journal
                    .get(&id)
                    .map(|v| v.join("\n"))
                    .unwrap_or_default()
            } else {
                structured
                    .iter()
                    .map(|f| {
                        format!(
                            "attempt {} — rejected by {} ({:?}): {}",
                            f.attempt, f.gate, f.layer, f.detail
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let thin = state
                .tickets
                .iter()
                .find(|t| t.id().to_string() == id)
                .is_some_and(|t| {
                    t.acceptance_criteria().is_empty() || t.description().trim().len() < 80
                });
            (hist, thin, structured)
        };
        // Data beats prose: route on what the gates recorded, and only fall
        // back to reading English for tickets that failed before the
        // structured log existed.
        let route = if structured.is_empty() {
            escalation_route(&history, spec_gap)
        } else {
            route_from_failures(&structured, spec_gap)
        };
        let claimed = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            let done = s.ticket_redesigns.get(&id).copied().unwrap_or(0);
            if done >= MAX_TICKET_RESCUES {
                return Err(crate::PortError::Conflict("rescues exhausted".into()));
            }
            s.ticket_redesigns.insert(id.clone(), done + 1);
            Ok(())
        })
        .await;
        if claimed.is_err() {
            return;
        }
        let failures = if history.trim().is_empty() {
            "(no failure detail was recorded)".to_owned()
        } else {
            history.clone()
        };
        let (who, persona, brief) = match route {
            EscalationRoute::Spec => (
                "BA",
                crate::prompts::BA,
                format!(
                    "Ticket {id} (\"{title}\") failed THREE times and the developers never \
                     had a spec they could build against. Here is what each attempt \
                     reported:\n{failures}\n\nRewrite the requirement so it is answerable: \
                     state the exact problem, the steps to reproduce it if it is a bug, and \
                     numbered acceptance criteria a developer can verify mechanically. Under \
                     900 characters, plain text, no approach or file names — that is the \
                     SA's job."
                ),
            ),
            EscalationRoute::Mechanical => (
                "SA",
                crate::prompts::SA,
                format!(
                    "Ticket {id} (\"{title}\") failed THREE times, every time on a mechanical \
                     quality gate rather than on the design:\n{failures}\n\nThe approach is \
                     probably fine — the developer could not get past the gate. Study the \
                     repo and reply with a concrete plan under 900 characters to clear THAT \
                     blocker: which lint or missing test, in which file, and what the fix \
                     is. Do not redesign the feature. Plain text, imperative."
                ),
            ),
            EscalationRoute::Design => (
                "SA",
                crate::prompts::SA,
                format!(
                    "Ticket {id} (\"{title}\") was PARKED after THREE failed build/verify \
                     attempts — the current technical approach is not working. Each attempt \
                     reported:\n{failures}\n\nStudy the repo and reply with a REVISED \
                     approach in under 900 characters: simplify the scope, change the \
                     technique, or split out what's achievable. Plain text, imperative, \
                     concrete files/modules."
                ),
            ),
        };
        self.report(who, &format!("rescuing parked {id}"));
        let request = crate::ports::outbound::AgentRequest {
            role: if who == "BA" {
                coxagent_domain::Role::Ba
            } else {
                coxagent_domain::Role::Sa
            },
            system_prompt: crate::prompts::system_prompt(persona),
            task_prompt: brief,
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(900),
            // A ticket three attempts deep has earned the stronger model.
            escalation_level: 1,
            label: Some(id.clone()),
        };
        let out = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => o.stdout.trim().chars().take(1200).collect::<String>(),
            _ => String::new(),
        };
        if out.len() < 40 {
            return; // no usable revision — stays parked for a human
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if let Some(t) = s.tickets.iter_mut().find(|t| t.id().to_string() == id) {
                match route {
                    // A clarified requirement belongs in the ticket the DEV
                    // reads, not in a design field.
                    EscalationRoute::Spec => {
                        let _ = t.clarify(coxagent_domain::Role::Ba, &out);
                    }
                    EscalationRoute::Mechanical | EscalationRoute::Design => {
                        let design = coxagent_domain::TechnicalDesign {
                            approach: out.clone(),
                            ..Default::default()
                        };
                        let _ = t.set_technical_design(coxagent_domain::Role::Sa, design);
                    }
                }
            }
            s.ticket_fail_attempts.remove(&id);
            // The next DEV run reads the journal, so the rescue lands where
            // the work happens instead of only in a chat message.
            s.journal_note(&id, &format!("{who} rescue: {out}"));
            // Number the rescue so a second one for the same ticket reads as a
            // distinct action instead of a byte-for-byte repeat — two identical
            // messages in the stream is how the SM chat reads as spam. The
            // `claimed` closure above already bumped this counter, so the
            // current value IS the attempt number (1, then 2).
            let attempt = s.ticket_redesigns.get(&id).copied().unwrap_or(1);
            let msg = match route {
                EscalationRoute::Spec => format!(
                    "🧯 SM→BA (rescues {attempt}): {id} bị 3 lần đỏ vì spec chưa rõ — BA đã \
                     viết lại yêu cầu, DEV làm lại. Đỏ tiếp là chuyển người quyết."
                ),
                EscalationRoute::Mechanical => format!(
                    "🧯 SM→SA (rescues {attempt}): {id} bị 3 lần đỏ ở cổng chất lượng (lint/test) \
                     chứ không phải thiết kế — SA đưa cách gỡ đúng chỗ đó. Đỏ tiếp là \
                     chuyển người quyết."
                ),
                EscalationRoute::Design => format!(
                    "🧯 SM→SA (rescues {attempt}): {id} được RE-DESIGN sau 3 build đỏ — DEV thử \
                     lại với hướng mới. Đỏ tiếp là chuyển người quyết."
                ),
            };
            s.post_comment("SM", &msg, Some(id.clone()));
            s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            Ok(())
        })
        .await;
    }

    /// Escalate questions a person has sat on past the configured SLA: post
    /// to the agents channel and mark the question so it escalates once. A
    /// gate must never become the place tickets go to die.
    pub(super) async fn escalate_stale_human_questions(&self) {
        let sla_min = self.config.workflow.human.question_sla_minutes;
        if sla_min == 0 {
            return;
        }
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            let mut escalations: Vec<String> = Vec::new();
            for q in &mut s.questions {
                if !(q.is_open() && q.to.starts_with('@')) || q.escalated {
                    continue;
                }
                let age_min = super::seconds_since(&q.asked_at).map_or(0, |secs| secs / 60);
                if age_min >= sla_min {
                    q.escalated = true;
                    // Past its SLA a question bypasses batching entirely
                    // (CXA-F176): it re-enters the live inbox immediately
                    // instead of waiting for the owner's focus window to end.
                    q.deferred = false;
                    escalations.push(format!(
                        "⏰ {} has waited {age_min}m for {} (SLA {sla_min}m) — ticket {} is \
                         blocked on it: \"{}\"",
                        q.id,
                        q.to,
                        if q.ticket.is_empty() { "-" } else { &q.ticket },
                        q.body.chars().take(160).collect::<String>()
                    ));
                }
            }
            for msg in escalations {
                s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                s.log_activity("SM", "escalated an overdue human question", None);
            }
            Ok(())
        })
        .await;
    }

    /// Flush the focus-window digests whose boundary has passed (CXA-F176):
    /// each person holding queued questions gets ONE batched message the
    /// moment their window is no longer active, and every held question
    /// leaves the queue — surfaced ones inside the digest, stale ones
    /// (answered, or their ticket resolved while queued) silently released.
    /// Runs beside the SLA escalation so a focus window can delay a question
    /// but never hide it past its SLA or lose it.
    pub(super) async fn flush_focus_digests(&self) {
        let human = self.config.workflow.human.clone();
        let now = crate::use_cases::question_batching::now_minutes_utc();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
            for (owner, batch) in crate::use_cases::question_batching::flush_batches(s, &human, now)
            {
                // Re-checked under this write pass: flush_batches filtered
                // against the just-loaded state, so `batch` is exactly what
                // the digest carries. Release each one as it is delivered.
                for q in &batch {
                    if let Some(held) = s.questions.iter_mut().find(|h| h.id == q.id) {
                        held.deferred = false;
                    }
                }
                // Stale holds must not stay deferred past the boundary — a
                // held flag with no digest would be a question that is both
                // invisible and unescalatable. Released ones reappear as
                // ordinary inbox items (or nowhere, once answered).
                let owner_tag = format!("@{owner}");
                for q in s
                    .questions
                    .iter_mut()
                    .filter(|q| q.deferred && q.to.eq_ignore_ascii_case(&owner_tag))
                {
                    q.deferred = false;
                }
                let msg = crate::use_cases::question_batching::digest_message(&owner, &batch);
                s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                s.log_activity(
                    "SM",
                    &format!("flushed {} queued question(s) to @{owner}", batch.len()),
                    None,
                );
            }
            Ok(())
        })
        .await;
    }
}
