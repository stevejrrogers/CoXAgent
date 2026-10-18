//! Human focus-window question batching (CXA-F176): a person who wants an
//! uninterrupted build block configures a focus window; questions addressed
//! to them during it are HELD and flush once as a digest at the window's
//! end, instead of dribbling in one interrupt at a time. Urgency is never
//! batched away: a question already past its SLA delivers straight through
//! even mid-window, and turning the feature off restores yesterday's
//! behaviour exactly.
//!
//! Every decision here is a PURE function of (config, wall-clock minute,
//! `ProjectState`) — no IO, no ports — so each rule is testable as a struct
//! literal over the real state types (the `run_dev/gates.rs` pattern).

use std::collections::BTreeMap;

use coxagent_domain::Status;

use crate::config::{in_quiet_window, FocusWindow, HumanConfig};
use crate::state::{AgentQuestion, ProjectState};

/// The addressee of a question, with any `@` prefix stripped and lowercased —
/// the canonical username a focus window is keyed by (`@Luffy`, `@LUFFY` and
/// `luffy` are the same person, matching how the inbox resolves the caller).
/// Lowercase is the group key everywhere, so mixed-case addressing of one
/// person still produces ONE batch, not two.
fn owner_of(to: &str) -> String {
    to.trim()
        .strip_prefix('@')
        .unwrap_or_else(|| to.trim())
        .to_ascii_lowercase()
}

/// The focus settings for a question's addressee, if one is configured for
/// them. Lookup is case-insensitive on the bare username.
fn focus_of<'a>(human: &'a HumanConfig, to: &str) -> Option<&'a FocusWindow> {
    let owner = owner_of(to);
    if owner.is_empty() {
        return None;
    }
    human
        .focus_windows
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&owner))
        .map(|(_, window)| window)
}

/// Whether defer-to-digest is actually switched ON for this addressee: a
/// configured entry with a real window and the opt-in set. The presence of
/// an entry alone is not enough — `defer_to_digest` is the kill switch that
/// restores immediate delivery without deleting the schedule.
fn batching_on<'a>(human: &'a HumanConfig, to: &str) -> Option<&'a FocusWindow> {
    focus_of(human, to).filter(|fw| fw.defer_to_digest && !fw.window_utc.trim().is_empty())
}

/// Minutes since midnight UTC right now — the axis focus windows are
/// expressed on (the same convention as `quiet_hours_utc`; the hub has no
/// reliable local-timezone source).
#[must_use]
pub fn now_minutes_utc() -> u32 {
    let now = time::OffsetDateTime::now_utc();
    u32::from(now.hour()) * 60 + u32::from(now.minute())
}

/// The hold-vs-deliver decision for one question addressed to a person
/// (CXA-F176): `true` = hold it for their focus-window digest, `false` =
/// deliver immediately, exactly as questions have always delivered.
///
/// Only questions explicitly addressed with `@username` are batchable — a
/// bare-username `to` is agent-queue territory (see `answer_open_questions`)
/// and every batching surface (selection, flush, SLA escalation) only
/// handles `@` ones; deferring anything else would hold it where nothing
/// ever looks. Pure over (per-user config, current minute, question age,
/// SLA). A question already past its SLA never waits — batching must not
/// become a second way for a blocked ticket to starve.
#[must_use]
pub fn should_defer(
    human: &HumanConfig,
    to: &str,
    now_minutes: u32,
    age_minutes: u64,
    sla_minutes: u64,
) -> bool {
    if !to.trim().starts_with('@') {
        return false;
    }
    let Some(fw) = batching_on(human, to) else {
        return false;
    };
    if sla_minutes > 0 && age_minutes >= sla_minutes {
        return false;
    }
    in_quiet_window(&fw.window_utc, now_minutes)
}

/// The questions currently HELD for batching, grouped per owner — one
/// consolidated batch per username (case-insensitive), questions in ask
/// order. This is the queue a focus window is sitting on at any moment; the
/// digest flush delivers exactly one such group per person.
#[must_use]
pub fn select_deferred(state: &ProjectState) -> Vec<(String, Vec<AgentQuestion>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<AgentQuestion>> = BTreeMap::new();
    for q in &state.questions {
        if !(q.is_open() && q.deferred && q.to.starts_with('@')) {
            continue;
        }
        let owner = owner_of(&q.to);
        if owner.is_empty() {
            continue;
        }
        if !order.contains(&owner) {
            order.push(owner.clone());
        }
        groups.entry(owner).or_default().push(q.clone());
    }
    order
        .into_iter()
        .map(|owner| {
            let batch = groups.remove(&owner).unwrap_or_default();
            (owner, batch)
        })
        .collect()
}

/// Whether a held question is still worth surfacing at flush time. A
/// question whose author resolved it while queued (it has an answer now),
/// or whose ticket was completed or rejected meanwhile, is history — the
/// digest delivers open questions, not stale ones.
fn still_relevant(q: &AgentQuestion, state: &ProjectState) -> bool {
    if !q.is_open() {
        return false;
    }
    if q.ticket.is_empty() {
        return true; // product-level question: no ticket to outlive it
    }
    match state
        .tickets
        .iter()
        .find(|t| t.id().to_string() == q.ticket)
    {
        // The ticket was pruned: nothing left for the question to unblock.
        None => false,
        Some(t) => !matches!(
            t.status(),
            Status::Done | Status::Documented | Status::Rejected | Status::Verified
        ),
    }
}

/// The flush-side complement of [`should_defer`]: every owner whose held
/// questions are due RIGHT NOW, with the batch the digest should carry.
/// Due means the owner's focus window is no longer active — the boundary
/// passed while the questions sat queued — or defer-to-digest was switched
/// off for them (the off switch must also release what it already held).
/// Stale questions (answered, or their ticket resolved while queued) are
/// dropped here and never surface in a digest.
#[must_use]
pub fn flush_batches(
    state: &ProjectState,
    human: &HumanConfig,
    now_minutes: u32,
) -> Vec<(String, Vec<AgentQuestion>)> {
    select_deferred(state)
        .into_iter()
        .map(|(owner, batch)| {
            let live: Vec<AgentQuestion> = batch
                .into_iter()
                // Due: this question's window is no longer active.
                .filter(|q| {
                    !batching_on(human, &q.to)
                        .is_some_and(|fw| in_quiet_window(&fw.window_utc, now_minutes))
                })
                // Surface only what still needs the person (AC edge).
                .filter(|q| still_relevant(q, state))
                .collect();
            (owner, live)
        })
        .filter(|(_, live)| !live.is_empty())
        .collect()
}

/// The ONE message a flush posts per owner: every held question, listed
/// with its ticket and asker so each is individually answerable from the
/// inbox cards that reappear with the flush.
#[must_use]
pub fn digest_message(owner: &str, batch: &[AgentQuestion]) -> String {
    use std::fmt::Write as _;
    let n = batch.len();
    let mut msg = format!(
        "📬 @{owner} — your focus window ended: {n} question{} waited while you were \
         heads-down:",
        if n == 1 { "" } else { "s" }
    );
    for q in batch {
        let ticket = if q.ticket.is_empty() {
            String::new()
        } else {
            format!(" ({})", q.ticket)
        };
        let body: String = q.body.chars().take(160).collect();
        let _ = write!(msg, "\n• {}{ticket}: {body} — reply in your inbox", q.from);
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::now_rfc3339;

    fn human_with_window(user: &str, window: &str, defer: bool) -> HumanConfig {
        HumanConfig {
            focus_windows: BTreeMap::from([(
                user.to_owned(),
                FocusWindow {
                    window_utc: window.to_owned(),
                    defer_to_digest: defer,
                },
            )]),
            ..HumanConfig::default()
        }
    }

    fn question(ticket: &str, to: &str, body: &str, deferred: bool) -> AgentQuestion {
        AgentQuestion {
            id: format!("{ticket}#1"),
            ticket: ticket.to_owned(),
            from: "DEV-FEATURE".to_owned(),
            to: to.to_owned(),
            body: body.to_owned(),
            answer: String::new(),
            asked_at: now_rfc3339(),
            answered_at: String::new(),
            forwarded: false,
            escalated: false,
            deferred,
        }
    }

    // (1) One consolidated batch per username across multiple open questions.
    #[test]
    fn held_questions_consolidate_into_one_batch_per_user() {
        let mut state = ProjectState::default();
        state
            .questions
            .push(question("CXC-F001", "@luffy", "soft-delete or move?", true));
        state
            .questions
            .push(question("CXC-F002", "@LUFFY", "which storage?", true));
        state.questions.push(question(
            "CXC-B001",
            "@nami",
            "what does 'done' mean?",
            true,
        ));
        // An agent-queue question and an answered one never batch.
        state
            .questions
            .push(question("CXC-F003", "BA", "agent-queue question", false));
        let mut answered = question("CXC-F004", "@luffy", "resolved while queued", true);
        answered.answer = "found it myself".to_owned();
        state.questions.push(answered);

        let batches = select_deferred(&state);
        assert_eq!(batches.len(), 2, "one batch per user, not per question");
        // Mixed-case addressing of the same person is still ONE group.
        assert_eq!(batches[0].0, "luffy");
        assert_eq!(batches[0].1.len(), 2, "answered dropped, both live kept");
        assert_eq!(batches[1].0, "nami");
    }

    // (2) Boundary: everything outside the feature delivers immediately.
    #[test]
    fn without_an_active_window_questions_deliver_immediately() {
        let inside = 9 * 60 + 30; // 09:30, inside 09:00-12:00
        let outside = 13 * 60; // 13:00, after it
        let human = human_with_window("luffy", "09:00-12:00", true);
        // Inside the window + opted in -> hold.
        assert!(should_defer(&human, "@luffy", inside, 0, 60));
        // Window not active -> deliver now.
        assert!(!should_defer(&human, "@luffy", outside, 0, 60));
        // No config for this person -> deliver now.
        assert!(!should_defer(&human, "@zoro", inside, 0, 60));
        // Opt-out switch off -> deliver now even inside the window.
        let off = human_with_window("luffy", "09:00-12:00", false);
        assert!(!should_defer(&off, "@luffy", inside, 0, 60));
        // No feature at all (default config) -> deliver now, always.
        assert!(!should_defer(
            &HumanConfig::default(),
            "@luffy",
            inside,
            0,
            60
        ));
        // Bare-username addressing is agent-queue territory, never batched:
        // every batching surface (selection, flush, escalation) handles only
        // @-person questions, so deferring these would lose them.
        assert!(!should_defer(&human, "luffy", inside, 0, 60));
    }

    // (3) Urgency is never batched away: past its SLA a question goes
    // straight through even mid-window, and batching never shields a
    // question from escalating once its SLA has passed.
    #[test]
    fn a_question_past_its_sla_bypasses_batching_entirely() {
        let human = human_with_window("luffy", "09:00-12:00", true);
        let inside = 9 * 60 + 30;
        assert!(
            !should_defer(&human, "@luffy", inside, 30, 30),
            "age == SLA: starving, deliver immediately"
        );
        assert!(!should_defer(&human, "@luffy", inside, 90, 30));
        // Still under the SLA: hold.
        assert!(should_defer(&human, "@luffy", inside, 29, 30));
        // SLA disabled (0 = never escalate): no bypass, the window governs.
        assert!(should_defer(&human, "@luffy", inside, 500, 0));
    }

    // (4) Regression, gates style: adding `deferred` must not change which
    // questions the agent answering queue picks up — person-addressed
    // questions (deferred or not) stay out of it.
    #[test]
    fn deferred_questions_never_enter_the_agent_answering_queue() {
        let mut state = ProjectState::default();
        state
            .questions
            .push(question("CXC-F001", "@luffy", "held for digest", true));
        state
            .questions
            .push(question("CXC-F002", "BA", "normal agent question", false));
        let agent_queue: Vec<&AgentQuestion> = state
            .questions
            .iter()
            .filter(|q| q.is_open() && !q.to.starts_with('@'))
            .collect();
        assert_eq!(agent_queue.len(), 1);
        assert_eq!(agent_queue[0].to, "BA");
    }

    // (5) Flush boundary: held questions flush once the window is no longer
    // active; questions resolved while queued never surface.
    #[test]
    fn flush_delivers_only_still_relevant_questions_after_the_boundary() {
        let mut state = ProjectState::default();
        state
            .questions
            .push(question("CXC-F001", "@luffy", "still blocked", true));
        state
            .questions
            .push(question("CXC-F002", "@luffy", "author resolved it", true));
        if let Some(q) = state.questions.last_mut() {
            q.answer = "figured it out".to_owned();
        }
        state
            .questions
            .push(question("CXC-F003", "@luffy", "ticket got verified", true));
        // A ticket driven along the only legal route to Verified: the work is
        // done, so a question asked to unblock it is history.
        let mut done = coxagent_domain::Ticket::new(
            coxagent_domain::TicketId::new("CXC-F003").expect("valid id"),
            coxagent_domain::TicketType::Bug,
            "defect CXC-F003",
            "repro",
            coxagent_domain::Priority::Medium,
            coxagent_domain::Complexity::Medium,
            false,
        )
        .expect("valid ticket");
        done.transition_to(
            coxagent_domain::Role::DevBug,
            coxagent_domain::Status::InProgress,
        )
        .expect("legal route");
        done.transition_to(
            coxagent_domain::Role::DevBug,
            coxagent_domain::Status::Fixed,
        )
        .expect("legal route");
        done.transition_to(
            coxagent_domain::Role::Test,
            coxagent_domain::Status::Verified,
        )
        .expect("legal route");
        state.tickets.push(done);
        // The still-blocked question's ticket is alive and in play — the
        // question must survive the flush filter.
        state.tickets.push(
            coxagent_domain::Ticket::new(
                coxagent_domain::TicketId::new("CXC-F001").expect("valid id"),
                coxagent_domain::TicketType::Bug,
                "defect CXC-F001",
                "repro",
                coxagent_domain::Priority::High,
                coxagent_domain::Complexity::Medium,
                false,
            )
            .expect("valid ticket"),
        );
        // A question whose ticket was pruned is equally moot.
        state
            .questions
            .push(question("CXC-F004", "@luffy", "ticket is gone", true));

        let human = human_with_window("luffy", "09:00-12:00", true);
        // Still inside the window: nothing is due yet.
        assert!(flush_batches(&state, &human, 9 * 60 + 30).is_empty());
        // Boundary passed: exactly the one live question surfaces, once.
        let due = flush_batches(&state, &human, 13 * 60);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, "luffy");
        assert_eq!(due[0].1.len(), 1, "answered, verified and pruned stay out");
        assert_eq!(due[0].1[0].body, "still blocked");
        // Switching the feature off also releases the queue (today's
        // behaviour restored).
        let off = HumanConfig::default();
        let released = flush_batches(&state, &off, 9 * 60 + 30);
        assert_eq!(released[0].1.len(), 1);
    }

    // (6) The digest is ONE message carrying every question replyably.
    #[test]
    fn digest_message_lists_every_question_once() {
        let batch = vec![
            question("CXC-F001", "@luffy", "soft-delete or move?", true),
            question("CXC-F002", "@luffy", "which storage?", true),
        ];
        let msg = digest_message("luffy", &batch);
        assert!(msg.contains("@luffy"), "{msg}");
        assert!(msg.contains("2 questions"), "{msg}");
        assert_eq!(msg.matches("CXC-F00").count(), 2, "{msg}");
        assert!(msg.contains("DEV-FEATURE"), "{msg}");
        let single = digest_message("nami", &batch[..1]);
        assert!(single.contains("1 question waited"), "{single}");
    }

    // (7) Config + state round-trip across a restart (AC4): a held question
    // and its owner's window survive a full serialize/deserialize cycle —
    // the exact transformation StateStorePort persists through.
    #[test]
    fn deferred_state_and_focus_config_round_trip_through_the_store() {
        let mut state = ProjectState::default();
        state
            .questions
            .push(question("CXC-F001", "@luffy", "still blocked", true));
        let human = human_with_window("luffy", "09:00-12:00", true);

        let state_json = serde_json::to_string(&state).expect("state serializes");
        let human_json = serde_json::to_string(&human).expect("config serializes");
        let state: ProjectState = serde_json::from_str(&state_json).expect("state restores");
        let human: HumanConfig = serde_json::from_str(&human_json).expect("config restores");

        assert!(state.questions[0].deferred, "held status survives restart");
        assert!(should_defer(&human, "@luffy", 9 * 60 + 30, 0, 30));
    }
}
