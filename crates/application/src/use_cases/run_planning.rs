//! `RunPlanningUseCase` — a real Sprint Planning ceremony. After the SM lays out
//! the goal and the committed slice, the team actually talks it through: the PO
//! frames value and priority, the SA raises technical risk and dependencies, a
//! developer gives a capacity/reality check, and the SM confirms a realistic
//! commitment. Grounded in the sprint's committed tickets so it reads like a
//! human planning session, not a canned announcement.
//!
//! The whole ceremony runs in a **single** engine call (see
//! [`crate::use_cases::ceremony`]).

use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::use_cases::ceremony;
use std::path::PathBuf;
use std::sync::Arc;

/// The roles that speak at planning, in order.
const VOICES: &[(&str, &str)] = &[
    (
        "PO",
        "Product Owner — frames value, priority, and the sprint goal",
    ),
    (
        "SA",
        "Solution Architect — technical risk, dependencies, feasibility",
    ),
    (
        "DEV-FEATURE",
        "Lead Developer — capacity and a realistic reality check",
    ),
];

/// Runs a facilitated Sprint Planning over the shared engine + store.
pub struct RunPlanningUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    lang: crate::config::Language,
    /// Extra repo reality (e.g. the open-PR queue) the team must weigh in
    /// planning — so the SA judges "can this sprint even start?" out loud.
    repo_note: String,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunPlanningUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
            lang: crate::config::Language::En,
            repo_note: String::new(),
        }
    }

    /// Set the language the ceremony speaks (English or Vietnamese).
    #[must_use]
    pub fn with_language(mut self, lang: crate::config::Language) -> Self {
        self.lang = lang;
        self
    }

    /// Attach repo reality (e.g. "6 open PRs, 3 conflicted") for the team to
    /// weigh during planning.
    #[must_use]
    pub fn with_repo_note(mut self, note: impl Into<String>) -> Self {
        self.repo_note = note.into();
        self
    }

    /// Run the planning ceremony. No-op (returns `Ok`) when there is no sprint.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails.
    pub async fn execute(&self) -> Result<(), AppError> {
        let Some(context) = self.plan_context().await else {
            return Ok(());
        };

        let repo = if self.repo_note.trim().is_empty() {
            String::new()
        } else {
            format!(
                "\nREPO REALITY the team MUST address before committing:\n{}\n\
                 If this blocks the goal (e.g. a restructure cannot start on a dirty merge \
                 queue), the SA must SAY SO and the SM's commitment must state the unblock \
                 plan first (e.g. \"merge/close every open PR, then start\").\n",
                self.repo_note.trim()
            )
        };
        let task = format!(
            "{context}{repo}\nRun a full sprint planning now. Each teammate weighs in from their \
             angle — is the scope realistic, what's the biggest risk or dependency, and what (if \
             anything) should we defer? Then SM confirms the final commitment in 2-3 sentences: \
             the goal, what we're committing to, and anything explicitly deferred. Be decisive."
        );
        let turns = ceremony::run_transcript(
            self.engine.as_ref(),
            &self.work_dir,
            self.lang,
            "You are facilitating an autonomous software team's sprint planning.",
            VOICES,
            &task,
        )
        .await?;
        for turn in &turns {
            self.post(&turn.speaker, &turn.text).await;
        }
        Ok(())
    }

    /// The committed slice + goal the team plans around, or `None` if no sprint.
    async fn plan_context(&self) -> Option<String> {
        use std::fmt::Write as _;
        let s = self.store.load().await.ok()?;
        let sp = s.sprint.clone()?;
        let mut out = format!(
            "Sprint #{} planning. Goal: {}\nCommitted tickets:\n",
            sp.number, sp.goal
        );
        if sp.committed.is_empty() {
            out.push_str("- (backlog empty)\n");
        } else {
            for id in &sp.committed {
                let title = s
                    .tickets
                    .iter()
                    .find(|t| t.id() == id)
                    .map_or("", coxagent_domain::Ticket::title);
                let _ = writeln!(out, "- {id}: {title}");
            }
        }
        Some(out)
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut state) = self.store.load().await {
            state.post_comment(author, body, None);
            let _ = self.store.save(&state).await;
        }
    }
}
