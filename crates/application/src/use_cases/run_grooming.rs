//! `RunGroomingUseCase` — a real Backlog Grooming / Refinement ceremony. The
//! team looks at the top pending backlog items that aren't ready yet: the BA
//! clarifies acceptance, the SA flags anything too big or too unknown to build,
//! and the PO (re)prioritizes what should be Ready for the next planning. It's a
//! discussion ceremony — the actual ticket edits still flow through the BA/SA
//! gates — but it keeps the backlog healthy and reads like a human refinement.
//!
//! The whole ceremony runs in a **single** engine call (see
//! [`crate::use_cases::ceremony`]).

use crate::error::AppError;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::use_cases::ceremony;
use coxagent_domain::Status;
use std::path::PathBuf;
use std::sync::Arc;

/// The roles that groom the backlog, in order.
const VOICES: &[(&str, &str)] = &[
    (
        "BA",
        "Business Analyst — clarifies scope & acceptance criteria",
    ),
    (
        "SA",
        "Solution Architect — flags size, unknowns, and splits",
    ),
    (
        "PO",
        "Product Owner — reprioritizes what should be ready next",
    ),
];

/// How many un-ready backlog items to bring into grooming.
const GROOM_LIMIT: usize = 6;

/// Runs a facilitated Backlog Grooming over the shared engine + store.
pub struct RunGroomingUseCase<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> {
    store: Arc<S>,
    engine: Arc<E>,
    work_dir: PathBuf,
    lang: crate::config::Language,
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunGroomingUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
            lang: crate::config::Language::En,
        }
    }

    /// Set the language the ceremony speaks (English or Vietnamese).
    #[must_use]
    pub fn with_language(mut self, lang: crate::config::Language) -> Self {
        self.lang = lang;
        self
    }

    /// Run the grooming ceremony. No-op (returns `Ok`) when the backlog has
    /// nothing un-ready worth refining.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails.
    pub async fn execute(&self) -> Result<(), AppError> {
        let Some(context) = self.backlog_context().await else {
            return Ok(());
        };

        let opener = if self.lang.is_vi() {
            "🧹 Backlog grooming — cùng đưa các mục ưu tiên vào trạng thái sẵn sàng."
        } else {
            "🧹 Backlog grooming — let's get the top items ready."
        };
        self.post("SM", opener).await;

        let task = format!(
            "{context}\nRun a full backlog grooming now. Each teammate speaks in turn from their \
             angle — which items are unclear or too big, what to split, which are ready to pull \
             into the next sprint (reference item ids). Then SM closes in 2-3 sentences: which \
             items are now ready, which need more work (and who owns that), and the refined top of \
             the backlog."
        );
        let turns = ceremony::run_transcript(
            self.engine.as_ref(),
            &self.work_dir,
            self.lang,
            "You are facilitating an autonomous software team's backlog grooming.",
            VOICES,
            &task,
        )
        .await?;
        for turn in &turns {
            self.post(&turn.speaker, &turn.text).await;
        }
        Ok(())
    }

    /// The un-ready backlog slice, or `None` when there's nothing to groom.
    async fn backlog_context(&self) -> Option<String> {
        use std::fmt::Write as _;
        let s = self.store.load().await.ok()?;
        let items: Vec<&coxagent_domain::Ticket> = s
            .tickets
            .iter()
            .filter(|t| matches!(t.status(), Status::Pending | Status::Open))
            .take(GROOM_LIMIT)
            .collect();
        if items.is_empty() {
            return None;
        }
        let mut out = String::from("Top backlog items not yet ready:\n");
        for t in items {
            let ui = if t.has_ui() { " [has UI]" } else { "" };
            let _ = writeln!(out, "- {} ({:?}){ui}: {}", t.id(), t.priority(), t.title());
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
