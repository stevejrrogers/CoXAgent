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
        }
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
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
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
        match self.engine.run(self.build_request(&state, &id)).await {
            Ok(o) if o.succeeded() => {}
            Ok(o) => {
                self.release_claim(&id).await;
                return Err(PortError::Backend(format!(
                    "{:?} engine failed on {id}: {}",
                    self.mode,
                    o.stderr.trim()
                ))
                .into());
            }
            Err(e) => {
                self.release_claim(&id).await;
                return Err(e.into());
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
        AgentRequest {
            role: self.mode.role(),
            system_prompt: format!(
                "{}{stack}{deploy}{design}",
                prompts::system_prompt(prompts::DEV)
            ),
            task_prompt: format!(
                "Ticket {id}: {title}\n{}\nImplement it now.{}{}",
                ticket_brief(ticket),
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
fn ticket_brief(ticket: Option<&coxagent_domain::Ticket>) -> String {
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
