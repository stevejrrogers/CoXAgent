//! `RunDiscussionUseCase` — a facilitated multi-agent discussion (M6 teamwork).
//! A few role-agents each give their take on a topic, then the SM concludes with
//! a decision and an optional concrete action (create a ticket). Every turn is
//! posted to the team channel so the reasoning is auditable, and a decided
//! action goes through the same guarded `AddTicket` path — the agents deliberate,
//! code commits the outcome.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::use_cases::{AddTicketInput, AddTicketUseCase};
use coxagent_domain::{Complexity, Priority, Role, TicketType};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// One participant's contribution, kept for the transcript and the next prompt.
struct Turn {
    role: &'static str,
    text: String,
}

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
        let mut turns: Vec<Turn> = Vec::new();

        // Open the discussion with a marked topic so the Scrum feed frames the
        // whole exchange as one ceremony.
        self.post("SM", &format!("💬 Discussion — {topic}")).await;

        // Opinion turns — each role reacts to the topic and the thread so far.
        for (role, persona) in [
            (
                "PO",
                "the Product Owner, who cares about user value, business priority, and scope",
            ),
            (
                "SA",
                "the Solution Architect, who cares about technical feasibility, risk, and effort",
            ),
        ] {
            let text = self.opinion(role, persona, topic, &turns).await?;
            self.post(role, &text).await;
            turns.push(Turn { role, text });
        }

        // Decision turn — the SM concludes and may propose an action.
        let raw = self.decision(topic, &turns).await?;
        let (decision, action) = split_action(&raw);
        self.post("SM", &format!("✅ Decision: {}", decision.trim()))
            .await;
        // Persist it to the team's durable memory so every agent honours it later.
        if !decision.trim().is_empty() {
            if let Ok(mut s) = self.store.load().await {
                s.add_decision(decision.trim());
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
            turns: turns.len() + 1,
            decision,
            created_ticket,
        })
    }

    async fn opinion(
        &self,
        role: &str,
        persona: &str,
        topic: &str,
        thread: &[Turn],
    ) -> Result<String, AppError> {
        let task = format!(
            "Team discussion topic: {topic}\n\n{}\n\nYou are {role}, {persona}. Give your \
             take in 2-3 sentences. Be concrete and concise. No preamble, no sign-off.",
            render_thread(thread),
        );
        self.run(role, &task).await
    }

    async fn decision(&self, topic: &str, thread: &[Turn]) -> Result<String, AppError> {
        let task = format!(
            "Team discussion topic: {topic}\n\n{}\n\nYou are SM (Scrum Master). Summarise and \
             state ONE clear decision in 2-3 sentences. If the team should build something, \
             append on a NEW LINE exactly:\nACTION: {{\"title\": string, \"description\": string, \
             \"priority\": \"low\"|\"medium\"|\"high\"}}\nOtherwise append:\nACTION: none",
            render_thread(thread),
        );
        self.run("SM", &task).await
    }

    async fn run(&self, role: &str, task: &str) -> Result<String, AppError> {
        let request = AgentRequest {
            role: Role::Sm,
            system_prompt: format!(
                "You are {role} in an autonomous software team's discussion. Speak plainly \
                 in the first person, like a real teammate — have a point of view, agree or \
                 push back, ask a pointed question when something is unclear, and don't be \
                 afraid to raise a concern. Be concise (2-4 sentences), no bullet lists.{}",
                self.lang.reply_directive()
            ),
            task_prompt: task.to_owned(),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(120),
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "discussion engine failed for {role}: {}",
                outcome.stderr.trim()
            ))
            .into());
        }
        Ok(outcome.stdout.trim().to_owned())
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut state) = self.store.load().await {
            state.post_comment(author, body, None);
            let _ = self.store.save(&state).await;
        }
    }
}

/// Render the thread so far for inclusion in the next prompt.
fn render_thread(thread: &[Turn]) -> String {
    use std::fmt::Write as _;
    if thread.is_empty() {
        return "(no comments yet)".to_owned();
    }
    let mut s = String::from("Discussion so far:\n");
    for t in thread {
        let _ = writeln!(s, "{}: {}", t.role, t.text);
    }
    s
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
