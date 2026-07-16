//! `RunGroomingUseCase` — a real Backlog Grooming / Refinement ceremony. The
//! team looks at the top pending backlog items that aren't ready yet: the BA
//! clarifies acceptance, the SA flags anything too big or too unknown to build,
//! and the PO (re)prioritizes what should be Ready for the next planning. It's a
//! discussion ceremony — the actual ticket edits still flow through the BA/SA
//! gates — but it keeps the backlog healthy and reads like a human refinement.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use coxagent_domain::{Role, Status};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
    /// [`AppError`] when the engine fails on a turn.
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

        let mut thread: Vec<(String, String)> = Vec::new();
        for (role, persona) in VOICES {
            let text = self.turn(role, persona, &context, &thread).await?;
            self.post(role, &text).await;
            thread.push(((*role).to_owned(), text));
        }

        let close = self.close(&context, &thread).await?;
        self.post("SM", &format!("📌 {}", close.trim())).await;
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

    async fn turn(
        &self,
        role: &str,
        persona: &str,
        context: &str,
        thread: &[(String, String)],
    ) -> Result<String, AppError> {
        use std::fmt::Write as _;
        let mut prior = String::new();
        if !thread.is_empty() {
            prior.push_str("\nSaid so far:\n");
            for (r, t) in thread {
                let _ = writeln!(prior, "{r}: {t}");
            }
        }
        let task = format!(
            "{context}{prior}\nYou are {role} ({persona}). In 1-2 sentences, refine this backlog \
             from your angle — which items are unclear or too big, what to split, and which are \
             ready to pull into the next sprint. Reference item ids. Concrete, first person."
        );
        self.run(role, &task).await
    }

    async fn close(&self, context: &str, thread: &[(String, String)]) -> Result<String, AppError> {
        use std::fmt::Write as _;
        let mut said = String::new();
        for (r, t) in thread {
            let _ = writeln!(said, "{r}: {t}");
        }
        let task = format!(
            "{context}\nGrooming notes:\n{said}\nYou are the SM. In 2-3 sentences, summarize the \
             outcome: which items are now ready, which need more work (and who owns that), and \
             the refined top of the backlog. Be concrete with ids."
        );
        self.run("SM", &task).await
    }

    async fn run(&self, role: &str, task: &str) -> Result<String, AppError> {
        let request = AgentRequest {
            role: Role::Sm,
            system_prompt: format!(
                "You are {role} at your team's backlog grooming. Speak plainly in the first \
                 person like a real teammate — concise, specific, reference ticket ids. No \
                 preamble, no sign-off, 1-3 sentences.{}",
                self.lang.reply_directive()
            ),
            task_prompt: task.to_owned(),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(90),
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "grooming engine failed for {role}: {}",
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
