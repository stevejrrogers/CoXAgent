//! `RunDevUseCase` — the DEV-BUG and DEV-FEATURE agents.
//!
//! The orchestrator owns claim/release: it atomically claims a ticket
//! (`Ready|Open -> InProgress` as `System`), runs the engine, and on success
//! completes it (`-> Done` / `-> Fixed`) while bumping the version. The agent
//! only does the coding; state moves are code, not prompt.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::selection::{open_bug_candidates, ready_feature_candidates};
use crate::{prompts, state::ProjectState};
use coxagent_domain::{Bump, Role, Status, TicketId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Which developer role to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevMode {
    /// Fix the highest-priority open bug (`Open -> InProgress -> Fixed`, patch bump).
    Bug,
    /// Implement the next ready feature (`Ready -> InProgress -> Done`, minor bump).
    Feature,
}

impl DevMode {
    fn role(self) -> Role {
        match self {
            DevMode::Bug => Role::DevBug,
            DevMode::Feature => Role::DevFeature,
        }
    }

    fn bump(self) -> Bump {
        match self {
            DevMode::Bug => Bump::Patch,
            DevMode::Feature => Bump::Minor,
        }
    }

    fn complete_status(self) -> Status {
        match self {
            DevMode::Bug => Status::Fixed,
            DevMode::Feature => Status::Done,
        }
    }
}

/// Runs one developer pass. Returns the completed ticket id, or `None` when
/// there was nothing to do.
pub struct RunDevUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    mode: DevMode,
    /// Identity of this runner (`account@host`) recorded as the ticket's claim
    /// owner, so concurrent runners never work the same ticket.
    worker: String,
    phase: Option<crate::use_cases::runner::PhaseReporter>,
    /// Test runner for the mechanical Definition-of-Done check: after the
    /// engine finishes, the suite must be green or the ticket is NOT done.
    verify: Option<Arc<dyn crate::ports::outbound::DeployPort>>,
    context: Option<String>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunDevUseCase<S, E> {
    pub fn new(
        store: Arc<S>,
        engine: Arc<E>,
        config: Config,
        work_dir: PathBuf,
        mode: DevMode,
    ) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            mode,
            worker: String::new(),
            phase: None,
            verify: None,
            context: None,
        }
    }

    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Attach the test runner enforcing the mechanical DoD (green tests).
    #[must_use]
    pub fn with_verify(
        mut self,
        verify: Option<Arc<dyn crate::ports::outbound::DeployPort>>,
    ) -> Self {
        self.verify = verify;
        self
    }

    /// Set the runner identity (`account@host`) recorded as the claim owner.
    #[must_use]
    pub fn with_worker(mut self, worker: impl Into<String>) -> Self {
        self.worker = worker.into();
        self
    }

    /// Attach the live "working now" reporter; fired only after the ticket is
    /// claimed, so a runner that loses the race never shows a false-busy card.
    #[must_use]
    pub fn with_phase(mut self, phase: Option<crate::use_cases::runner::PhaseReporter>) -> Self {
        self.phase = phase;
        self
    }

    /// Execute one developer pass.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or an unexpected state transition error.
    #[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        // Guard: verify the codebase builds before doing any work.
        if let Some(deploy) = &self.verify {
            match tokio::time::timeout(
                std::time::Duration::from_secs(300),
                deploy.run_tests(&self.work_dir),
            )
            .await
            {
                Ok(Ok(r)) if r.success => {}
                Ok(Ok(r)) => {
                    tracing::warn!(
                        "DEV pre-check: cargo test failed — {}",
                        &r.summary[..r.summary.len().min(200)]
                    );
                    return Ok(None);
                }
                Ok(Err(e)) => {
                    tracing::warn!("DEV pre-check: cargo test error — {e}");
                    return Ok(None);
                }
                Err(_timeout) => {
                    tracing::warn!("DEV pre-check: cargo test timed out after 5 min");
                    return Ok(None);
                }
            }
        }
        let state = self.store.load().await?;
        // Walk the work queue best-first and atomically claim the first ticket no
        // other runner holds (cross-process lock). A second runner thus grabs a
        // *different* ticket and builds in parallel, rather than idling on a lost
        // race for the same top ticket.
        let worker = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        let now = now_rfc3339();
        let mut chosen = None;
        for cand in self.candidates(&state) {
            // Parked: a ticket that failed 3 times needs a human, not more tokens.
            if state
                .ticket_fail_attempts
                .get(cand.as_str())
                .copied()
                .unwrap_or(0)
                >= 3
            {
                continue;
            }
            if self.store.claim_ticket(&cand, &worker, &now).await? {
                chosen = Some(cand);
                break;
            }
        }
        let Some(id) = chosen else {
            return Ok(None);
        };
        if let Some(p) = &self.phase {
            let role = match self.mode {
                DevMode::Bug => "DEV-BUG",
                DevMode::Feature => "DEV-FEATURE",
            };
            p(Some((role.to_owned(), id.to_string())));
        }

        let state = self.store.load().await?;
        // On ANY engine failure (error or non-zero exit — e.g. a quota wall),
        // release the claim so the ticket returns to the queue instead of being
        // stranded In-Progress forever (which piled up 100+ orphaned tickets and
        // kept burning tokens re-claiming fresh ones).
        // Keep the engine's conversation id: the repair pass (below) resumes
        // this session so the agent keeps everything it just read and wrote
        // in context instead of rediscovering its own change cold.
        let session = match self.engine.run(self.build_request(&state, &id)).await {
            Ok(o) if o.succeeded() => o.session_id.clone(),
            Ok(o) => {
                self.record_failure(&id, o.stderr.trim()).await;
                self.release_claim(&id).await;
                return Err(PortError::Backend(format!(
                    "{:?} engine failed on {id}: {}",
                    self.mode,
                    o.stderr.trim()
                ))
                .into());
            }
            Err(e) => {
                self.record_failure(&id, &e.to_string()).await;
                self.release_claim(&id).await;
                return Err(e.into());
            }
        };

        // Mechanical Definition of Done: the suite must be GREEN after the
        // change. Red → one bounded repair pass fed the failure output; still
        // red → the ticket is NOT done (claim released, failure recorded)
        // instead of shipping a broken build for TEST to rediscover later.
        if let Some(deploy) = &self.verify {
            let failed = |r: &crate::ports::outbound::DeployReport| !r.success;
            let mut red = match deploy.run_tests(&self.work_dir).await {
                Ok(r) if failed(&r) => Some(r.summary),
                _ => None,
            };
            if let Some(fail) = red.take() {
                let tail: String = fail
                    .chars()
                    .rev()
                    .take(3000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let follow_up = format!(
                    "Your change for ticket {id} left the test suite FAILING. Fix ONLY \
                     these failures now (do not start new work):\n{tail}"
                );
                // Resume the same conversation when the engine supports it —
                // the agent still has its own change in context, so the fix is
                // faster and far cheaper than a cold re-read. Fall back to a
                // fresh run otherwise.
                let resumed = match &session {
                    Some(sid) => self
                        .engine
                        .resume_run(sid, &follow_up, &self.work_dir, Duration::from_secs(1800))
                        .await
                        .is_ok(),
                    None => false,
                };
                if !resumed {
                    let repair = AgentRequest {
                        role: self.mode.role(),
                        system_prompt: prompts::system_prompt(prompts::DEV),
                        task_prompt: follow_up,
                        work_dir: self.work_dir.clone(),
                        timeout: Duration::from_secs(1800),
                    };
                    let _ = self.engine.run(repair).await;
                }
                red = match deploy.run_tests(&self.work_dir).await {
                    Ok(r) if failed(&r) => Some(r.summary),
                    _ => None,
                };
            }
            if let Some(fail) = red {
                self.record_failure(&id, &fail).await;
                self.release_claim(&id).await;
                return Err(PortError::Backend(format!(
                    "{:?} left tests red on {id} — ticket returned to the queue",
                    self.mode
                ))
                .into());
            }
        }

        // Complete under an atomic read-modify-write with retry: move to the
        // terminal status, bump the version, record the deploy. A concurrent
        // operator saving the shared state can't make us lose this completion
        // (which would strand the ticket and waste tokens redoing it).
        let (role, status, bump, id_c) = (
            self.mode.role(),
            self.mode.complete_status(),
            self.mode.bump(),
            id.clone(),
        );
        crate::ports::outbound::mutate_state(self.store.as_ref(), |state| {
            transition(state, &id_c, role, status)
                .map_err(|e| PortError::Corrupt(e.to_string()))?;
            // Done — the work journal served its purpose.
            state.ticket_journal.remove(&id_c.to_string());
            let version = state.current_version.bumped(bump);
            state.current_version = version.clone();
            let title = state
                .ticket(&id_c)
                .map_or_else(String::new, |t| t.title().to_owned());
            state.history.push(crate::state::DeployRecord {
                version,
                ticket: id_c.clone(),
                title,
                at: now_rfc3339(),
            });
            Ok(())
        })
        .await?;
        if let Some(p) = &self.phase {
            p(None);
        }
        Ok(Some(id))
    }

    /// Count a failed attempt on `id`; at the 3rd, park it with a visible note
    /// so a human decides instead of the team burning tokens forever.
    async fn record_failure(&self, id: &TicketId, why: &str) {
        let key = id.to_string();
        let short: String = why.chars().take(300).collect();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            let n = {
                let c = s.ticket_fail_attempts.entry(key.clone()).or_insert(0);
                *c += 1;
                *c
            };
            // Brief the NEXT attempt on what this one hit, so a retry builds
            // on prior findings instead of rediscovering them.
            s.journal_note(&key, &format!("attempt {n} failed: {short}"));
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

    /// Return a stranded ticket to the queue when the run failed, so it isn't
    /// stuck In-Progress. `System` is the only actor allowed to un-claim.
    async fn release_claim(&self, id: &TicketId) {
        if let Ok(mut state) = self.store.load().await {
            if let Some(t) = state.ticket_mut(id) {
                if t.release_claim(Role::System).is_ok() {
                    let _ = self.store.save(&state).await;
                }
            }
        }
    }

    fn candidates(&self, state: &ProjectState) -> Vec<TicketId> {
        match self.mode {
            DevMode::Bug => open_bug_candidates(state),
            DevMode::Feature => ready_feature_candidates(state),
        }
    }

    fn build_request(&self, state: &ProjectState, id: &TicketId) -> AgentRequest {
        let ticket = state.ticket(id);
        let title = ticket.map_or("", coxagent_domain::Ticket::title);
        let _choice = self.config.engine.resolve(self.mode.role());
        let stack = prompts::stack_constraints(&self.config.architecture);
        let deploy = prompts::deploy_constraints(&self.config.deploy);
        // A UI ticket also carries the project design system into the prompt.
        let design = if ticket.is_some_and(coxagent_domain::Ticket::has_ui) {
            prompts::design_constraints(state.design_system.as_ref())
        } else {
            String::new()
        };
        let context_block = self
            .context
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| format!("\n\n## Project context (goal, stack, scope, constraints):\n{c}\n"))
            .unwrap_or_default();
        // Prior attempts' findings on this ticket (empty first time).
        let journal = state
            .ticket_journal
            .get(&id.to_string())
            .filter(|notes| !notes.is_empty())
            .map(|notes| {
                format!(
                    "\n\nPREVIOUS ATTEMPTS on this ticket — build on these, do not repeat them:\n- {}",
                    notes.join("\n- ")
                )
            })
            .unwrap_or_default();
        AgentRequest {
            role: self.mode.role(),
            // The system prompt stays BYTE-IDENTICAL across every DEV run of a
            // project: engines put it in the provider prompt cache, so a stable
            // prefix means cache READ pricing on back-to-back runs. Anything
            // per-ticket (stack/deploy/design blocks included — the design one
            // exists only for UI tickets) belongs in the task prompt below.
            system_prompt: prompts::system_prompt(prompts::DEV),
            task_prompt: format!(
                "Ticket {id}: {title}\n{}\nImplement it now.{stack}{deploy}{design}{context_block}{}{}{}{journal}",
                ticket_brief(ticket),
                prompts::focus_block(
                    &self.work_dir,
                    &format!(
                        "{title} {}",
                        ticket
                            .and_then(|t| t.design().technical.as_ref())
                            .map_or("", |d| d.approach.as_str())
                    ),
                ),
                prompts::repo_map_block(&self.work_dir, self.config.workflow.token_saver),
                prompts::team_memory_block(&state.decisions, &state.lessons),
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(3600),
        }
    }
}

/// The full working brief for a ticket, BOUNDED: the description (what & why),
/// the acceptance criteria, and the SA's technical design — so the DEV builds
/// what was specified instead of guessing from the title (and the SA's design
/// tokens aren't wasted). Caps keep a verbose ticket from bloating the prompt.
pub fn ticket_brief(ticket: Option<&coxagent_domain::Ticket>) -> String {
    use std::fmt::Write as _;
    let Some(t) = ticket else {
        return String::new();
    };
    let cap = |s: &str, n: usize| -> String {
        if s.chars().count() <= n {
            s.trim().to_owned()
        } else {
            let cut: String = s.chars().take(n).collect();
            format!("{}…", cut.trim_end())
        }
    };
    let mut out = String::new();
    if !t.description().trim().is_empty() {
        let _ = write!(out, "\nWHAT & WHY:\n{}\n", cap(t.description(), 1500));
    }
    if !t.acceptance_criteria().is_empty() {
        out.push_str("\nACCEPTANCE CRITERIA (all must pass):\n");
        for c in t.acceptance_criteria() {
            let _ = writeln!(out, "- {}", cap(c, 200));
        }
    }
    if let Some(d) = &t.design().technical {
        out.push_str("\nTECHNICAL DESIGN (from the SA — follow it, flag if it's wrong):\n");
        if !d.approach.trim().is_empty() {
            let _ = writeln!(out, "- Approach: {}", cap(&d.approach, 1200));
        }
        if !d.files.is_empty() {
            let _ = writeln!(out, "- Files: {}", cap(&d.files.join(", "), 600));
        }
        if !d.api_contract.trim().is_empty() {
            let _ = writeln!(out, "- API contract: {}", cap(&d.api_contract, 800));
        }
        if !d.data_changes.trim().is_empty() {
            let _ = writeln!(out, "- Data changes: {}", cap(&d.data_changes, 600));
        }
        if !d.test_plan.trim().is_empty() {
            let _ = writeln!(out, "- Test plan: {}", cap(&d.test_plan, 800));
        }
    }
    out
}

/// Current UTC time as an RFC3339 string, or a stable fallback if formatting
/// fails (it does not, for `now_utc`).
fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Apply a guarded transition to a ticket in state, mapping a missing ticket to
/// a corruption error.
fn transition(
    state: &mut ProjectState,
    id: &TicketId,
    actor: Role,
    to: Status,
) -> Result<(), AppError> {
    let ticket = state
        .ticket_mut(id)
        .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished mid-cycle")))?;
    ticket.transition_to(actor, to)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::AgentOutcome;
    use crate::selection::next_ready_feature;
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, Ticket, TicketType};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemStore {
        state: Mutex<ProjectState>,
    }
    #[async_trait::async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            Ok(self.state.lock().expect("lock").clone())
        }
        async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
            s.validate().map_err(PortError::Corrupt)?;
            *self.state.lock().expect("lock") = s.clone();
            Ok(())
        }
    }

    struct OkEngine;
    #[async_trait::async_trait]
    impl AgentEnginePort for OkEngine {
        fn id(&self) -> &'static str {
            "ok"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: "changed foo.rs".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
            })
        }
    }

    fn ready_feature(id: &str) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t");
        t.set_technical_design(Role::Sa, TechnicalDesign::default())
            .expect("d");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t
    }

    #[tokio::test]
    async fn feature_dev_completes_and_bumps_minor() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets: vec![ready_feature("FEAT-001")],
                ..ProjectState::default()
            }),
        });
        let uc = RunDevUseCase::new(
            Arc::clone(&store),
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            DevMode::Feature,
        );
        let done = uc.execute().await.expect("run");
        assert_eq!(done.expect("some").as_str(), "FEAT-001");

        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Done);
        assert_eq!(state.current_version.to_string(), "0.1.0");
        assert!(next_ready_feature(&state).is_none());
    }

    #[tokio::test]
    async fn returns_none_when_no_work() {
        let store = Arc::new(MemStore::default());
        let uc = RunDevUseCase::new(
            store,
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            DevMode::Feature,
        );
        assert!(uc.execute().await.expect("run").is_none());
    }
}
