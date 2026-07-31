//! `RunSaUseCase` — the SA design gate. Picks the top pending feature missing a
//! design, runs the engine to produce one, attaches it, and moves the ticket to
//! `ready`. The aggregate re-checks Definition of Ready, so a UI feature without
//! UX simply won't pass — enforced by code, not trusted to the prompt.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::prompts;
use crate::selection::design_candidates;
use coxagent_domain::{Role, Status, TechnicalDesign, TicketId};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The SA's JSON output: the technical design. UX is authored separately by PD.
#[derive(Debug, Serialize, Deserialize)]
struct DesignOutput {
    approach: String,
    #[serde(default)]
    alternatives: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    api_contract: String,
    #[serde(default)]
    data_changes: String,
    #[serde(default)]
    test_plan: String,
}

/// Runs one SA design pass.
pub struct RunSaUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    worker: String,
    phase: Option<crate::use_cases::runner::PhaseReporter>,
    context: Option<String>,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunSaUseCase<S, E> {
    pub fn new(store: Arc<S>, engine: Arc<E>, config: Config, work_dir: PathBuf) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            worker: String::new(),
            phase: None,
            context: None,
            files: None,
        }
    }

    /// Attach workspace file access for prompt context blocks; `None` (tests)
    /// reads as no context.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// Attach the project context (`project_context.md`) so the SA understands
    /// the goal, stack, scope, and constraints before designing.
    #[must_use]
    pub fn with_context(mut self, context: Option<String>) -> Self {
        self.context = context;
        self
    }

    /// Set this runner's identity (`account@host`) so the SA stage is claimed
    /// per-ticket, letting concurrent runners design different tickets in
    /// parallel without both designing the same one.
    #[must_use]
    pub fn with_worker(mut self, worker: impl Into<String>) -> Self {
        self.worker = worker.into();
        self
    }

    /// Attach the live "working now" reporter; fired only after the stage is
    /// won, so a runner that loses the claim never shows a false-busy card.
    #[must_use]
    pub fn with_phase(mut self, phase: Option<crate::use_cases::runner::PhaseReporter>) -> Self {
        self.phase = phase;
        self
    }

    /// Design the next pending feature. Returns the readied ticket id, or `None`
    /// when nothing needs design.
    ///
    /// # Errors
    /// [`AppError`] on engine failure, unparseable output, or a DoR violation.
    #[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        let state = self.store.load().await?;
        // Walk the SA queue best-first and claim the first ticket no other runner
        // holds — so a second runner grabs a *different* ticket instead of idling.
        let worker = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        // Design-WIP limit: designing far ahead of DEV throughput just piles
        // up a stale ready-queue (run 2 hit 18). Six ready tickets is plenty
        // of runway; past that the SA stands down this cycle.
        {
            use coxagent_domain::Status;
            let ready = state
                .tickets
                .iter()
                .filter(|t| t.status() == Status::Ready)
                .count();
            if ready >= 6 {
                return Ok(None);
            }
        }
        let now = crate::state::now_rfc3339();
        let mut chosen = None;
        for cand in design_candidates(&state) {
            if self.store.claim_stage(&cand, "sa", &worker, &now).await? {
                chosen = Some(cand);
                break;
            }
        }
        let Some(id) = chosen else {
            return Ok(None);
        };
        if let Some(p) = &self.phase {
            p(Some(("SA".to_owned(), id.to_string())));
        }
        let has_ui = state
            .ticket(&id)
            .is_some_and(coxagent_domain::Ticket::has_ui);
        let title = state
            .ticket(&id)
            .map_or("", coxagent_domain::Ticket::title)
            .to_owned();

        let memory = crate::prompts::team_memory_block(&state.decisions, &state.lessons);
        // The architect is meant to be the encyclopedia: it cannot arbitrate a
        // design for a product whose own wiki, docs and solved tickets it has
        // never been shown.
        let knowledge = crate::prompts::knowledge_block(
            self.files.as_deref(),
            &state.docs,
            &state.tickets,
            &self.work_dir,
            &format!(
                "{title} {}",
                state
                    .ticket(&id)
                    .map_or("", coxagent_domain::Ticket::description)
            ),
            &id.to_string(),
        )
        .await;
        let outcome = self
            .engine
            .run(self.build_request(&id, &title, &memory, &knowledge).await)
            .await?;
        if !outcome.succeeded() {
            self.store.release_stage(&id, "sa", &worker).await.ok();
            return Err(PortError::Backend(format!(
                "SA engine failed on {id}: {}",
                outcome.failure_detail()
            ))
            .into());
        }
        let design = match parse_design(&outcome.stdout) {
            Ok(d) => d,
            Err(first) => {
                let fixed = crate::use_cases::repair_json(
                    self.engine.as_ref(),
                    &outcome.stdout,
                    "a JSON object with the technical design fields",
                    &self.work_dir,
                )
                .await;
                let Some(Ok(repaired)) = fixed.as_deref().map(parse_design) else {
                    self.store.release_stage(&id, "sa", &worker).await.ok();
                    return Err(PortError::Corrupt(format!("SA output: {first}")).into());
                };
                repaired
            }
        };

        // LARGE ticket: experts ship SMALL PRs. Split it into ≤3 focused
        // sub-tickets instead of designing a thousand-line change; the parent
        // is rejected with a pointer to its children. Falls back to the
        // critic-reviewed single design when the split call fails.
        if state
            .ticket(&id)
            .is_some_and(|t| t.complexity() == coxagent_domain::ticket::Complexity::Large)
        {
            if let Some(children) = self.decompose_large(&id, &title).await {
                let worker2 = worker.clone();
                let id2 = id.clone();
                let kids = children.join(", ");
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |st| {
                    if let Some(t) = st.ticket_mut(&id2) {
                        let _ = t.transition_to(Role::Po, coxagent_domain::Status::Rejected);
                    }
                    st.post_comment(
                        "SA",
                        &format!("✂️ {id2} was LARGE — split into {kids}; parent closed."),
                        Some(id2.to_string()),
                    );
                    Ok(())
                })
                .await;
                self.store.release_stage(&id, "sa", &worker2).await.ok();
                return Ok(Some(id));
            }
        }
        // Critic pass on LARGE tickets only (when a split wasn't possible): a
        // second opinion is cheap here and architecture mistakes are cheapest
        // before DEV burns hours on them.
        let design = if state
            .ticket(&id)
            .is_some_and(|t| t.complexity() == coxagent_domain::ticket::Complexity::Large)
        {
            self.critic_pass(&id, &title, design).await
        } else {
            design
        };

        // Atomic read-modify-write with retry, so a concurrent operator can't
        // clobber this SA design or lose the transition (parallel-safe).
        let td = technical_of(&design);
        crate::ports::outbound::mutate_state(self.store.as_ref(), |state| {
            let ticket = state
                .ticket_mut(&id)
                .ok_or_else(|| PortError::Corrupt(format!("ticket {id} vanished")))?;
            ticket
                .set_technical_design(Role::Sa, td.clone())
                .map_err(|e| PortError::Corrupt(e.to_string()))?;
            // SA owns the technical design only. A non-UI ticket is ready now;
            // a UI ticket stays pending for PD to author UX.
            if !has_ui {
                ticket
                    .transition_to(Role::Sa, Status::Ready)
                    .map_err(|e| PortError::Corrupt(e.to_string()))?;
            }
            Ok(())
        })
        .await?;
        // Work done: clear the live phase so the dashboard/keepalive stops
        // showing this role once we move on (no stale "still on SA" label).
        if let Some(p) = &self.phase {
            p(None);
        }
        Ok(Some(id))
    }

    /// Ask the SA to split a large ticket into ≤3 small/medium sub-tickets.
    /// Returns the created ids, or `None` when the call failed/was unusable
    /// (caller falls back to a single reviewed design).
    async fn decompose_large(&self, id: &TicketId, title: &str) -> Option<Vec<String>> {
        let request = AgentRequest {
            role: Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: format!(
                "Ticket {id} ('{title}') is LARGE. Split it into 2-3 INDEPENDENT, \
                 individually shippable sub-tickets (small or medium each) that together \
                 deliver it. Output ONLY a JSON array: [{{\"title\": string, \
                 \"description\": string, \"complexity\": \"small\"|\"medium\", \
                 \"has_ui\": boolean, \"acceptance_criteria\": [string]}}]"
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 1,
        };
        let out = self.engine.run(request).await.ok()?;
        if !out.succeeded() {
            return None;
        }
        let raw = &out.stdout;
        let (start, end) = (raw.find('[')?, raw.rfind(']')?);
        let items: Vec<serde_json::Value> = serde_json::from_str(&raw[start..=end]).ok()?;
        if items.is_empty() || items.len() > 3 {
            return None;
        }
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        let mut created = Vec::new();
        for it in items.iter().take(3) {
            let title = it.get("title")?.as_str()?.to_owned();
            let desc = it
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
            let cx = match it.get("complexity").and_then(serde_json::Value::as_str) {
                Some("medium") => coxagent_domain::ticket::Complexity::Medium,
                _ => coxagent_domain::ticket::Complexity::Small,
            };
            let ui = it
                .get("has_ui")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let ac: Vec<String> = it
                .get("acceptance_criteria")
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let nid = adder
                .execute(crate::use_cases::AddTicketInput {
                    ticket_type: coxagent_domain::TicketType::Feature,
                    title: format!("{title} (from {id})"),
                    description: desc,
                    priority: coxagent_domain::ticket::Priority::Medium,
                    complexity: cx,
                    has_ui: ui,
                    acceptance_criteria: ac,
                })
                .await
                .ok()?;
            created.push(nid.to_string());
        }
        Some(created)
    }

    /// One critique round for a large ticket's design: a reviewer call judges
    /// it (APPROVED / must-fix list); on must-fix, ONE revision call amends
    /// the design. Any failure keeps the original design — the pass can only
    /// help, never block.
    async fn critic_pass(&self, id: &TicketId, title: &str, design: DesignOutput) -> DesignOutput {
        let design_json = serde_json::to_string(&design).unwrap_or_default();
        let critique_req = AgentRequest {
            role: coxagent_domain::Role::Sa,
            system_prompt: crate::prompts::system_prompt(
                "You are a principal engineer REVIEWING another architect's design. \
                 Judge only architecture-level risk: wrong decomposition, missing \
                 failure modes, scaling traps, security gaps — and REJECT any design \
                 whose `alternatives` field is empty or hand-wavy: no alternatives \
                 considered means no design happened. If sound, reply exactly \
                 APPROVED. Otherwise list ONLY must-fix items, one per line, no praise.",
            ),
            task_prompt: format!(
                "Ticket {id}: {title} (complexity: large)\nProposed design JSON:\n{design_json}"
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(300),
            escalation_level: 1, // the critic runs on the stronger ladder model
        };
        let critique = match self.engine.run(critique_req).await {
            Ok(o) if o.succeeded() => o.stdout.trim().to_owned(),
            _ => return design,
        };
        if critique.is_empty() || critique.to_uppercase().starts_with("APPROVED") {
            return design;
        }
        let revise_req = AgentRequest {
            role: coxagent_domain::Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: format!(
                "Your design for ticket {id} ({title}) got review feedback. Address \
                 EVERY must-fix item and output the FULL corrected design JSON only.\n\
                 Your design:\n{design_json}\nMust-fix feedback:\n{critique}"
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 0,
        };
        match self.engine.run(revise_req).await {
            Ok(o) if o.succeeded() => parse_design(&o.stdout).unwrap_or(design),
            _ => design,
        }
    }

    async fn build_request(
        &self,
        id: &TicketId,
        title: &str,
        memory: &str,
        knowledge: &str,
    ) -> AgentRequest {
        let _choice = self.config.engine.resolve(Role::Sa);
        let context_block = self
            .context
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| format!("\n\n## Project context (goal, stack, scope, constraints — design within this):\n{c}\n"))
            .unwrap_or_default();
        let stack = prompts::stack_constraints(&self.config.architecture);
        AgentRequest {
            role: Role::Sa,
            system_prompt: prompts::system_prompt(prompts::SA),
            task_prompt: format!(
                "Design feature {id}: {title}{context_block}{stack}{}{}{knowledge}{memory}",
                prompts::focus_block(self.files.as_deref(), &self.work_dir, title).await,
                prompts::repo_map_block(
                    self.files.as_deref(),
                    &self.work_dir,
                    self.config.workflow.token_saver,
                )
                .await,
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(1200),
            escalation_level: 0,
        }
    }
}

fn parse_design(raw: &str) -> Result<DesignOutput, String> {
    let start = raw.find('{').ok_or("no JSON object found")?;
    let end = raw.rfind('}').ok_or("no closing brace")?;
    if end < start {
        return Err("malformed object bounds".to_owned());
    }
    serde_json::from_str(&raw[start..=end]).map_err(|e| e.to_string())
}

fn technical_of(d: &DesignOutput) -> TechnicalDesign {
    TechnicalDesign {
        approach: d.approach.clone(),
        alternatives: d.alternatives.clone(),
        files: d.files.clone(),
        api_contract: d.api_contract.clone(),
        data_changes: d.data_changes.clone(),
        test_plan: d.test_plan.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::state::ProjectState;
    use crate::use_cases::{AddTicketInput, AddTicketUseCase};
    use coxagent_domain::{Complexity, Priority, TicketType};
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

    struct Canned(String);
    #[async_trait::async_trait]
    impl AgentEnginePort for Canned {
        fn id(&self) -> &'static str {
            "canned"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: self.0.clone(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
            })
        }
    }

    async fn seed(store: &Arc<MemStore>, has_ui: bool) {
        AddTicketUseCase::new(Arc::clone(store))
            .execute(AddTicketInput {
                ticket_type: TicketType::Feature,
                title: "F".to_owned(),
                description: String::new(),
                priority: Priority::High,
                complexity: Complexity::Small,
                has_ui,
                acceptance_criteria: Vec::new(),
            })
            .await
            .expect("seed");
    }

    fn uc(store: Arc<MemStore>, out: &str) -> RunSaUseCase<MemStore, Canned> {
        RunSaUseCase::new(
            store,
            Arc::new(Canned(out.to_owned())),
            Config::default(),
            PathBuf::from("/tmp"),
        )
    }

    #[tokio::test]
    async fn designs_non_ui_feature_to_ready() {
        let store = Arc::new(MemStore::default());
        seed(&store, false).await;
        let out = r#"{"approach":"do it","files":["a.rs"],"api_contract":"","data_changes":"","test_plan":"t","ux":null}"#;
        let id = uc(Arc::clone(&store), out).execute().await.expect("run");
        assert!(id.is_some());
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Ready);
        assert!(state.tickets[0].design().technical.is_some());
    }

    #[tokio::test]
    async fn ui_feature_gets_technical_but_stays_pending_for_pd() {
        let store = Arc::new(MemStore::default());
        seed(&store, true).await;
        let out =
            r#"{"approach":"do it","files":[],"api_contract":"","data_changes":"","test_plan":""}"#;
        let id = uc(Arc::clone(&store), out).execute().await.expect("run");
        assert!(id.is_some());
        let state = store.load().await.expect("load");
        // SA attaches technical design but leaves the UI ticket pending; the PD
        // gate authors UX before it can go ready.
        assert_eq!(state.tickets[0].status(), Status::Pending);
        assert!(state.tickets[0].design().technical.is_some());
        assert!(state.tickets[0].design().ux.is_none());
    }

    #[tokio::test]
    async fn no_pending_feature_returns_none() {
        let store = Arc::new(MemStore::default());
        assert!(uc(store, "{}").execute().await.expect("run").is_none());
    }
}
