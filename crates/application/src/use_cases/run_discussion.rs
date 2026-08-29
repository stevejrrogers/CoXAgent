//! `RunDiscussionUseCase` — a facilitated multi-agent discussion (M6 teamwork).
//! A few role-agents each give their take on a topic, then the SM concludes with
//! a decision and an optional concrete action (create a ticket). Every turn is
//! posted to the team channel so the reasoning is auditable, and a decided
//! action goes through the same guarded `AddTicket` path — the agents deliberate,
//! code commits the outcome.

use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::use_cases::{ceremony, AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, TicketType};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;

/// The outcome of a discussion: the posted turns and any created ticket.
pub struct DiscussionOutcome {
    pub turns: usize,
    pub decision: String,
    pub created_ticket: Option<String>,
}

/// An action the SM may decide on at the end of a discussion.
#[derive(Debug, Deserialize)]
struct DecidedAction {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    priority: Option<Priority>,
}

/// Runs a facilitated discussion over the shared engine + store.
pub struct RunDiscussionUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    lang: crate::config::Language,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunDiscussionUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
            lang: crate::config::Language::En,
        }
    }

    /// Set the language the discussion speaks (English or Vietnamese).
    #[must_use]
    pub fn with_language(mut self, lang: crate::config::Language) -> Self {
        self.lang = lang;
        self
    }

    /// Facilitate a discussion on `topic`: PO and SA weigh in, SM decides. Each
    /// turn is posted to the team channel. Returns the outcome.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails on a turn.
    pub async fn execute(&self, topic: &str) -> Result<DiscussionOutcome, AppError> {
        // Open the discussion with a marked topic so the Scrum feed frames the
        // whole exchange as one ceremony.
        self.post("SM", &format!("💬 Discussion — {topic}")).await;

        // One engine call role-plays the whole discussion: PO and SA give their
        // takes, then SM decides and (optionally) appends an ACTION marker.
        let task = format!(
            "Team discussion topic: {topic}\n\nRun the discussion now. PO gives their take (user \
             value, business priority, scope) and SA gives theirs (technical feasibility, risk, \
             effort) — 2-3 sentences each, with a real point of view. Then SM summarises and \
             states ONE clear decision. If the team should build something, AFTER the transcript \
             append on a NEW LINE exactly:\nACTION: {{\"title\": string, \"description\": string, \
             \"priority\": \"low\"|\"medium\"|\"high\"}}\nOtherwise append:\nACTION: none"
        );
        let (turns, raw) = ceremony::run_transcript_raw(
            self.engine.as_ref(),
            &self.work_dir,
            self.lang,
            "You are facilitating an autonomous software team's discussion.",
            &[
                (
                    "PO",
                    "Product Owner — user value, business priority, and scope",
                ),
                (
                    "SA",
                    "Solution Architect — technical feasibility, risk, and effort",
                ),
            ],
            &task,
        )
        .await?;

        // Post each turn; the ACTION marker (if any) is stripped from display.
        let mut posted = 1usize; // the topic header
        let mut decision = String::new();
        for turn in &turns {
            let text = strip_action(&turn.text);
            if text.trim().is_empty() {
                continue;
            }
            if turn.speaker == "SM" {
                self.post("SM", &format!("✅ Decision: {}", text.trim()))
                    .await;
                decision = text.trim().to_owned();
            } else {
                self.post(&turn.speaker, &text).await;
            }
            posted += 1;
        }

        let (_prose, action) = split_action(&raw);
        // Persist the decision to durable memory so every agent honours it later.
        if !decision.is_empty() {
            if let Ok(mut s) = self.store.load().await {
                s.add_decision(&decision);
                let _ = self.store.save(&s).await;
            }
        }

        // Enact a decided action through the guarded AddTicket path.
        let mut created_ticket = None;
        if let Some(a) = action {
            if !a.title.trim().is_empty() {
                let id = AddTicketUseCase::new(Arc::clone(&self.store))
                    .execute(AddTicketInput {
                        ticket_type: TicketType::Feature,
                        title: a.title,
                        description: a.description,
                        priority: a.priority.unwrap_or(Priority::Medium),
                        complexity: Complexity::Medium,
                        has_ui: false,
                        acceptance_criteria: Vec::new(),
                        goal: None,
                    })
                    .await?;
                self.post(
                    "SM",
                    &format!("🎫 Action: created {id} from this decision."),
                )
                .await;
                created_ticket = Some(id.to_string());
            }
        }

        Ok(DiscussionOutcome {
            turns: posted,
            decision,
            created_ticket,
        })
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut state) = self.store.load().await {
            state.post_comment(author, body, None);
            let _ = self.store.save(&state).await;
        }
    }
}

/// Cut an `ACTION:` marker (and anything after it) off a transcript turn so it
/// never shows in the feed — the marker is parsed separately from the raw text.
fn strip_action(text: &str) -> String {
    match text.find("ACTION:") {
        Some(idx) => text[..idx].trim().to_owned(),
        None => text.trim().to_owned(),
    }
}

/// Split an SM decision into (prose, optional action) on the `ACTION:` marker.
fn split_action(raw: &str) -> (String, Option<DecidedAction>) {
    let Some(idx) = raw.find("ACTION:") else {
        return (raw.trim().to_owned(), None);
    };
    let prose = raw[..idx].trim().to_owned();
    let rest = raw[idx + "ACTION:".len()..].trim();
    if rest.eq_ignore_ascii_case("none") {
        return (prose, None);
    }
    let action = rest
        .find('{')
        .zip(rest.rfind('}'))
        .filter(|(a, b)| b > a)
        .and_then(|(a, b)| serde_json::from_str::<DecidedAction>(&rest[a..=b]).ok());
    (prose, action)
}

#[cfg(test)]
mod tests {
    use super::split_action;

    #[test]
    fn parses_decision_with_action() {
        let raw = "We should build it.\nACTION: {\"title\":\"Reset\",\"description\":\"d\",\"priority\":\"high\"}";
        let (prose, action) = split_action(raw);
        assert_eq!(prose, "We should build it.");
        let a = action.expect("action");
        assert_eq!(a.title, "Reset");
        assert_eq!(a.priority, Some(coxagent_domain::Priority::High));
    }

    #[test]
    fn parses_decision_without_action() {
        let (prose, action) = split_action("Let's wait a sprint.\nACTION: none");
        assert_eq!(prose, "Let's wait a sprint.");
        assert!(action.is_none());
    }

    #[test]
    fn no_marker_keeps_all_prose() {
        let (prose, action) = split_action("Just a plain summary.");
        assert_eq!(prose, "Just a plain summary.");
        assert!(action.is_none());
    }
}
