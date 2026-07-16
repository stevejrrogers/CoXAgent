//! `RunPlanningUseCase` — a real Sprint Planning ceremony. After the SM lays out
//! the goal and the committed slice, the team actually talks it through: the PO
//! frames value and priority, the SA raises technical risk and dependencies, a
//! developer gives a capacity/reality check, and the SM confirms a realistic
//! commitment. Grounded in the sprint's committed tickets so it reads like a
//! human planning session, not a canned announcement.

use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use coxagent_domain::Role;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
}

impl<S: StateStorePort + ?Sized, E: AgentEnginePort + ?Sized> RunPlanningUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            work_dir,
        }
    }

    /// Run the planning ceremony. No-op (returns `Ok`) when there is no sprint.
    ///
    /// # Errors
    /// [`AppError`] when the engine fails on a turn.
    pub async fn execute(&self) -> Result<(), AppError> {
        let Some(context) = self.plan_context().await else {
            return Ok(());
        };

        let mut thread: Vec<(String, String)> = Vec::new();
        for (role, persona) in VOICES {
            let text = self.turn(role, persona, &context, &thread).await?;
            self.post(role, &text).await;
            thread.push(((*role).to_owned(), text));
        }

        let commit = self.commit(&context, &thread).await?;
        self.post("SM", &format!("✅ Commitment: {}", commit.trim()))
            .await;
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
            "{context}{prior}\nYou are {role} ({persona}). In 1-2 sentences, weigh in on this \
             sprint plan from your angle — is the scope realistic, what's the biggest risk or \
             dependency, and what (if anything) should we defer? Concrete, first person, no filler."
        );
        self.run(role, &task).await
    }

    async fn commit(&self, context: &str, thread: &[(String, String)]) -> Result<String, AppError> {
        use std::fmt::Write as _;
        let mut said = String::new();
        for (r, t) in thread {
            let _ = writeln!(said, "{r}: {t}");
        }
        let task = format!(
            "{context}\nTeam input:\n{said}\nYou are the SM. In 2-3 sentences, confirm the final \
             sprint commitment: the goal, what we're committing to, and anything explicitly \
             deferred. Be decisive."
        );
        self.run("SM", &task).await
    }

    async fn run(&self, role: &str, task: &str) -> Result<String, AppError> {
        let request = AgentRequest {
            role: Role::Sm,
            system_prompt: format!(
                "You are {role} at your team's sprint planning. Speak plainly in the first person \
                 like a real teammate — concise, specific, honest about risk and scope. No \
                 preamble, no sign-off, 1-3 sentences.{}",
                crate::prompts::VI_REPLY
            ),
            task_prompt: task.to_owned(),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(90),
        };
        let outcome = self.engine.run(request).await?;
        if !outcome.succeeded() {
            return Err(PortError::Backend(format!(
                "planning engine failed for {role}: {}",
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
