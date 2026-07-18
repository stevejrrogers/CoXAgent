//! `RunStandupUseCase` — a real daily standup, run by the SM with live agent
//! turns. The SM opens with the sprint goal and status, each participating role
//! gives a grounded update (done / next / blockers), and the SM closes by
//! highlighting blockers and the focus for the day.
//!
//! The whole ceremony is produced in a **single** engine call (see
//! [`crate::use_cases::ceremony`]) rather than one call per role turn — the feed
//! reads identically to a human team, at roughly 1/N the token cost.

use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::use_cases::ceremony;
use std::path::PathBuf;
use std::sync::Arc;

/// The role personas pulled into a standup, in order.
const PARTICIPANTS: &[(&str, &str)] = &[
    ("BA", "Business Analyst — backlog & requirements"),
    ("SA", "Solution Architect — design & technical risk"),
    ("PD", "Product Designer — UX"),
    ("DEV-FEATURE", "Feature Developer"),
    ("DEV-BUG", "Bug-fix Developer"),
    ("TEST", "QA Engineer"),
    ("DOCS", "Tech Writer"),
];

/// Runs a facilitated standup over the shared engine + store.
pub struct RunStandupUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    lang: crate::config::Language,
    operator: String,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunStandupUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
            lang: crate::config::Language::En,
            operator: String::new(),
        }
    }

    /// Set the language the ceremony speaks (English or Vietnamese).
    #[must_use]
    pub fn with_language(mut self, lang: crate::config::Language) -> Self {
        self.lang = lang;
        self
    }

    /// Attribute the ceremony's agent posts to this worker (`operator@host`).
    #[must_use]
    pub fn with_operator(mut self, operator: impl Into<String>) -> Self {
        self.operator = operator.into();
        self
    }

    /// Run the standup. Returns the number of blockers agents raised.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails.
    pub async fn execute(&self) -> Result<usize, AppError> {
        // Only pull in roles that actually did something (grounded, not noise).
        let active = self.active_roles().await;
        let roster: Vec<(&str, &str)> = PARTICIPANTS
            .iter()
            .filter(|(r, _)| active.contains(r))
            .copied()
            .collect();
        if roster.is_empty() {
            return Ok(0);
        }

        let context = self.status_context().await;
        let headline = self.headline().await;
        let task = format!(
            "{context}\nSprint status: {headline}\n\nRun the full daily standup now. Order:\n\
             1. SM opens warmly in 1-2 sentences and asks the team to report in.\n\
             2. Each teammate gives their update: what they finished, what's next, and — only if \
             real — a blocker or a cross-cutting risk. When there is one, put `BLOCKER:` or \
             `RAISE:` followed by the issue on that same line.\n\
             3. For every blocker/RAISE, the single most relevant teammate replies on its own line \
             with a concrete action to resolve it (who does what next).\n\
             4. SM closes in 1-2 sentences: which blockers to clear and the single most important \
             focus for today.\n\
             Ground every line in the activity above — do not invent work that isn't there."
        );

        let turns = ceremony::run_transcript(
            self.engine.as_ref(),
            &self.work_dir,
            self.lang,
            &format!(
                "{}\n\nYou are facilitating this team's daily standup.",
                crate::prompts::SM
            ),
            &roster,
            &task,
        )
        .await?;

        // Header frames the whole exchange as one ceremony in the feed.
        self.post("SM", "🗣️ Standup").await;
        let mut blockers = 0usize;
        for turn in &turns {
            if turn.speaker != "SM" {
                let low = turn.text.to_lowercase();
                if low.contains("block") || low.contains("raise:") || low.contains("concern") {
                    blockers += 1;
                }
            }
            self.post(&turn.speaker, &turn.text).await;
        }
        Ok(blockers)
    }

    /// One-line sprint headline for the SM's opener.
    async fn headline(&self) -> String {
        let Ok(s) = self.store.load().await else {
            return if self.lang.is_vi() {
                "họp nhanh hằng ngày".to_owned()
            } else {
                "daily sync".to_owned()
            };
        };
        let done = s
            .tickets
            .iter()
            .filter(|t| {
                matches!(
                    t.status(),
                    coxagent_domain::Status::Done
                        | coxagent_domain::Status::Documented
                        | coxagent_domain::Status::Verified
                )
            })
            .count();
        let inflight = s
            .tickets
            .iter()
            .filter(|t| t.status() == coxagent_domain::Status::InProgress)
            .count();
        match (&s.sprint, self.lang.is_vi()) {
            (Some(sp), true) => format!(
                "Sprint #{} “{}” · {done} xong · {inflight} đang làm. Điểm danh cả nhóm:",
                sp.number, sp.goal
            ),
            (Some(sp), false) => format!(
                "Sprint #{} “{}” · {done} done · {inflight} in flight. Round the room:",
                sp.number, sp.goal
            ),
            (None, true) => format!("{done} đã ship · {inflight} đang làm. Điểm danh cả nhóm:"),
            (None, false) => format!("{done} shipped · {inflight} in flight. Round the room:"),
        }
    }

    /// A compact status the agents ground their updates in.
    async fn status_context(&self) -> String {
        use std::fmt::Write as _;
        let Ok(s) = self.store.load().await else {
            return String::new();
        };
        let mut out = String::from("Recent team activity:\n");
        for a in s.activity.iter().rev().take(14) {
            let _ = writeln!(
                out,
                "- {}: {}{}",
                a.agent,
                a.action,
                a.ticket
                    .as_deref()
                    .map(|t| format!(" [{t}]"))
                    .unwrap_or_default()
            );
        }
        out
    }

    /// Which roles have recent activity (so empty roles stay quiet).
    async fn active_roles(&self) -> Vec<&'static str> {
        let Ok(s) = self.store.load().await else {
            return vec!["SA", "DEV-FEATURE", "TEST"];
        };
        let recent: std::collections::HashSet<String> = s
            .activity
            .iter()
            .rev()
            .take(30)
            .map(|a| a.agent.to_uppercase())
            .collect();
        let mut roles: Vec<&'static str> = PARTICIPANTS
            .iter()
            .map(|(r, _)| *r)
            .filter(|r| recent.contains(*r))
            .collect();
        if roles.is_empty() {
            roles = vec!["SA", "DEV-FEATURE", "TEST"];
        }
        roles
    }

    async fn post(&self, author: &str, body: &str) {
        if let Ok(mut state) = self.store.load().await {
            state.post_comment_by(author, &self.operator, body, None);
            let _ = self.store.save(&state).await;
        }
    }
}
