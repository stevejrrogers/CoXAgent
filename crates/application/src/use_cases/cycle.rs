//! `RunCycleUseCase` — one turn of the loop: BA (periodic) → DEV-BUG →
//! DEV-FEATURE → TEST. A failing agent is recorded and the cycle continues, so
//! one bad run never stalls the team (matching the reference workflow).

use crate::config::Config;
use crate::ports::outbound::{AgentEnginePort, DeployPort, StateStorePort};
use crate::state::Spend;
use crate::use_cases::run_dev::DevMode;
use crate::use_cases::{
    RunBaUseCase, RunConformanceUseCase, RunDesignSystemUseCase, RunDevUseCase, RunDocsUseCase,
    RunPdUseCase, RunSaUseCase, RunTestUseCase,
};
use coxagent_domain::TicketId;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// What happened during one cycle. Rendered for logs and the eventual dashboard.
#[derive(Debug, Default)]
pub struct CycleReport {
    pub cycle: u64,
    pub ba_created: Vec<TicketId>,
    pub sa_readied: Option<TicketId>,
    /// True when PD established the project design system this cycle.
    pub design_system_created: bool,
    pub pd_designed: Option<TicketId>,
    pub bug_fixed: Option<TicketId>,
    pub feature_done: Option<TicketId>,
    pub documented: Option<TicketId>,
    pub bugs_filed: Vec<TicketId>,
    pub errors: Vec<String>,
    /// True when accumulated spend has crossed the configured budget cap.
    pub over_budget: bool,
}

impl CycleReport {
    #[must_use]
    pub fn summary(&self) -> String {
        let mut s = format!("cycle {} —", self.cycle);
        let _ = write!(s, " BA+{}", self.ba_created.len());
        if let Some(r) = &self.sa_readied {
            let _ = write!(s, " design {r}");
        }
        if let Some(u) = &self.pd_designed {
            let _ = write!(s, " ux {u}");
        }
        if let Some(b) = &self.bug_fixed {
            let _ = write!(s, " fixed {b}");
        }
        if let Some(f) = &self.feature_done {
            let _ = write!(s, " done {f}");
        }
        if let Some(d) = &self.documented {
            let _ = write!(s, " docs {d}");
        }
        let _ = write!(s, " bugs+{}", self.bugs_filed.len());
        if !self.errors.is_empty() {
            let _ = write!(s, " ({} errors)", self.errors.len());
        }
        s
    }
}

/// Runs the sequential agent cycle over shared adapters.
pub struct RunCycleUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    context: String,
    meter: Option<Arc<Mutex<Spend>>>,
    deploy: Option<Arc<dyn DeployPort>>,
    notifier: Option<Arc<dyn crate::ports::outbound::NotifierPort>>,
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    pub fn new(
        store: Arc<S>,
        engine: Arc<E>,
        config: Config,
        work_dir: PathBuf,
        context: String,
    ) -> Self {
        Self {
            store,
            engine,
            config,
            work_dir,
            context,
            meter: None,
            deploy: None,
            notifier: None,
        }
    }

    /// Attach a notifier fired on significant events (deploy, budget, policy).
    #[must_use]
    pub fn with_notifier(
        mut self,
        notifier: Arc<dyn crate::ports::outbound::NotifierPort>,
    ) -> Self {
        self.notifier = Some(notifier);
        self
    }

    /// Model-allowlist gate. When the configured model is disallowed, record the
    /// block, notify, mark the report to pause the loop, and return `true`.
    async fn model_policy_blocks(&self, report: &mut CycleReport) -> bool {
        let model = &self.config.engine.default.model;
        if crate::policy::model_allowed(&self.config.policy, model) {
            return false;
        }
        report
            .errors
            .push(format!("POLICY: model '{model}' is not in the allowlist"));
        report.over_budget = true; // pause the loop until config is fixed
        if let Ok(mut state) = self.store.load().await {
            state.log_activity("POLICY", &format!("blocked model '{model}'"), None);
            let _ = self.store.save(&state).await;
        }
        self.notify(
            "policy_blocked",
            format!("model '{model}' is not in the allowlist — loop paused"),
        )
        .await;
        true
    }

    /// Emit an event to the notifier, if one is attached. Best-effort.
    async fn notify(&self, kind: &str, message: String) {
        if let Some(n) = &self.notifier {
            let project = self.config_project_label();
            n.notify(crate::ports::outbound::NotifyEvent {
                kind: kind.to_owned(),
                project,
                message,
            })
            .await;
        }
    }

    fn config_project_label(&self) -> String {
        self.work_dir
            .parent()
            .and_then(|p| p.file_name())
            .map_or_else(
                || "project".to_owned(),
                |n| n.to_string_lossy().into_owned(),
            )
    }

    /// Attach a spend meter (drained into state each cycle for FinOps tracking).
    #[must_use]
    pub fn with_meter(mut self, meter: Arc<Mutex<Spend>>) -> Self {
        self.meter = Some(meter);
        self
    }

    /// Attach a deploy port, run after DEV so TEST verifies a running build.
    #[must_use]
    pub fn with_deploy(mut self, deploy: Arc<dyn DeployPort>) -> Self {
        self.deploy = Some(deploy);
        self
    }

    /// Run one cycle. Never returns `Err`: agent failures are collected into the
    /// report so the outer loop keeps going.
    pub async fn run_cycle(&self, cycle: u64) -> CycleReport {
        let mut report = CycleReport {
            cycle,
            ..CycleReport::default()
        };

        // Policy gate: refuse to run agents on a model outside the allowlist —
        // stop before spending a token rather than after.
        if self.model_policy_blocks(&mut report).await {
            return report;
        }

        // Scrum: open/roll over the sprint at the start of the cycle, with an
        // SM retro line when a previous sprint closes.
        if self.config.workflow.mode == crate::config::Mode::Scrum {
            if let Ok(mut state) = self.store.load().await {
                let len = self.config.workflow.sprint_length_cycles;
                let prev = state.sprint.as_ref().map(|s| {
                    (
                        s.number,
                        s.committed.len(),
                        crate::sprint::done_count(&state),
                    )
                });
                if let Some(n) = crate::sprint::advance(&mut state, cycle, len) {
                    if let Some((pn, committed, done)) = prev {
                        state.log_activity(
                            "SM",
                            &format!("closed sprint {pn}: {done}/{committed} shipped"),
                            None,
                        );
                    }
                    state.log_activity("SM", "opened sprint", Some(format!("sprint {n}")));
                    let goal = state
                        .sprint
                        .as_ref()
                        .map_or_else(String::new, |s| s.goal.clone());
                    state.post_comment("SM", &format!("Sprint {n} started. Goal: {goal}"), None);
                    let _ = self.store.save(&state).await;
                }
            }
        }

        // BA runs on the first cycle of each period. `(cycle-1) % n == 0` is
        // correct for every n including 1 (unlike `cycle % n == 1`).
        let ba_every = self.config.workflow.ba_every_n_cycles;
        if ba_every > 0 && (cycle - 1) % ba_every == 0 {
            match self.ba().execute().await {
                Ok(ids) => report.ba_created = ids,
                Err(e) => report.errors.push(format!("BA: {e}")),
            }
        }

        match self.sa().execute().await {
            Ok(id) => report.sa_readied = id,
            Err(e) => report.errors.push(format!("SA: {e}")),
        }

        // PD establishes the project design system once UI work appears.
        match self.design_system().execute().await {
            Ok(created) => report.design_system_created = created,
            Err(e) => report.errors.push(format!("PD design-system: {e}")),
        }

        // PD authors UX for a UI ticket SA left pending, taking it to ready.
        match self.pd().execute().await {
            Ok(id) => report.pd_designed = id,
            Err(e) => report.errors.push(format!("PD: {e}")),
        }

        match self.dev(DevMode::Bug).execute().await {
            Ok(id) => report.bug_fixed = id,
            Err(e) => report.errors.push(format!("DEV-BUG: {e}")),
        }

        if self.config.workflow.feature_dev_enabled {
            match self.dev(DevMode::Feature).execute().await {
                Ok(id) => report.feature_done = id,
                Err(e) => report.errors.push(format!("DEV-FEATURE: {e}")),
            }
        }

        // Deploy after code changes so TEST verifies a running build.
        if (report.feature_done.is_some() || report.bug_fixed.is_some()) && self.deploy.is_some() {
            if let Some(deploy) = &self.deploy {
                match deploy.deploy(&self.work_dir).await {
                    Ok(r) if r.deployed => {
                        self.record_deploy(r.success, &r.summary).await;
                        let kind = if r.success {
                            "deploy_ok"
                        } else {
                            "deploy_failed"
                        };
                        self.notify(kind, r.summary.clone()).await;
                    }
                    Ok(_) => {}
                    Err(e) => report.errors.push(format!("DEPLOY: {e}")),
                }
            }
        }

        match self.test().execute().await {
            Ok(ids) => report.bugs_filed = ids,
            Err(e) => report.errors.push(format!("TEST: {e}")),
        }

        match self.docs().execute().await {
            Ok(id) => report.documented = id,
            Err(e) => report.errors.push(format!("DOCS: {e}")),
        }

        // Governance: architecture-conformance drift becomes tracked bugs.
        match self.conformance().execute().await {
            Ok(mut ids) => report.bugs_filed.append(&mut ids),
            Err(e) => report.errors.push(format!("CONFORMANCE: {e}")),
        }

        report.over_budget = self.record_activity(&report).await;
        if report.over_budget {
            self.notify(
                "budget_reached",
                "spend cap reached — loop paused".to_owned(),
            )
            .await;
        }
        report
    }

    /// Record a deploy outcome (activity + dashboard status). Best-effort.
    async fn record_deploy(&self, ok: bool, summary: &str) {
        if let Ok(mut state) = self.store.load().await {
            let at = crate::state::now_rfc3339();
            state.log_activity("DEPLOY", summary, None);
            let verb = if ok { "shipped" } else { "deploy failed" };
            state.post_comment("SM", &format!("{verb}: {summary}"), None);
            state.deploy = Some(crate::state::DeployStatus {
                at,
                ok,
                summary: summary.to_owned(),
            });
            let _ = self.store.save(&state).await;
        }
    }

    /// Append a human-readable activity trail plus drain the spend meter into
    /// state. Returns whether accumulated spend has crossed the budget cap.
    /// Best-effort: a failure here never fails a cycle.
    async fn record_activity(&self, report: &CycleReport) -> bool {
        let Ok(mut state) = self.store.load().await else {
            return false;
        };
        for id in &report.ba_created {
            state.log_activity("BA", "proposed feature", Some(id.to_string()));
        }
        if let Some(id) = &report.sa_readied {
            state.log_activity("SA", "designed (technical)", Some(id.to_string()));
        }
        if report.design_system_created {
            state.log_activity("PD", "established design system", None);
        }
        if let Some(id) = &report.pd_designed {
            state.log_activity("PD", "designed UX & readied", Some(id.to_string()));
        }
        if let Some(id) = &report.bug_fixed {
            state.log_activity("DEV-BUG", "fixed bug", Some(id.to_string()));
        }
        if let Some(id) = &report.feature_done {
            state.log_activity("DEV-FEATURE", "implemented feature", Some(id.to_string()));
        }
        if let Some(id) = &report.documented {
            state.log_activity("DOCS", "documented", Some(id.to_string()));
        }
        for id in &report.bugs_filed {
            state.log_activity("TEST", "filed bug", Some(id.to_string()));
        }

        // Drain the spend meter (deltas since last cycle) into persistent state.
        let mut cycle_cost = 0.0;
        if let Some(meter) = &self.meter {
            if let Ok(mut m) = meter.lock() {
                cycle_cost = m.total_cost_usd;
                state.spend.total_cost_usd += m.total_cost_usd;
                state.spend.input_tokens += m.input_tokens;
                state.spend.output_tokens += m.output_tokens;
                state.spend.runs += m.runs;
                for (role, cost) in std::mem::take(&mut m.by_role) {
                    *state.spend.by_role.entry(role).or_default() += cost;
                }
                *m = Spend::default();
            }
        }
        let spent_today = state.add_daily_spend(cycle_cost);

        // Pause on either the lifetime cap or the per-day policy cap.
        let over_lifetime = self
            .config
            .workflow
            .budget_usd
            .is_some_and(|cap| cap > 0.0 && state.spend.total_cost_usd >= cap);
        let over_daily = crate::policy::over_daily_budget(&self.config.policy, spent_today);
        if over_daily {
            state.log_activity("POLICY", "daily budget cap reached", None);
        }

        let _ = self.store.save(&state).await;
        over_lifetime || over_daily
    }

    fn ba(&self) -> RunBaUseCase<S, E> {
        RunBaUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
            self.context.clone(),
        )
    }

    fn sa(&self) -> RunSaUseCase<S, E> {
        RunSaUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
    }

    fn design_system(&self) -> RunDesignSystemUseCase<S, E> {
        RunDesignSystemUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
    }

    fn pd(&self) -> RunPdUseCase<S, E> {
        RunPdUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
    }

    fn dev(&self, mode: DevMode) -> RunDevUseCase<S, E> {
        RunDevUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
            mode,
        )
    }

    fn test(&self) -> RunTestUseCase<S, E> {
        RunTestUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
    }

    fn docs(&self) -> RunDocsUseCase<S, E> {
        RunDocsUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
    }

    fn conformance(&self) -> RunConformanceUseCase<S> {
        RunConformanceUseCase::new(
            Arc::clone(&self.store),
            self.work_dir.clone(),
            self.config.architecture.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, AgentRequest};
    use crate::state::ProjectState;
    use crate::PortError;
    use coxagent_domain::Status;
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

    /// Engine that answers each role by its system prompt: BA proposes one
    /// feature, TEST reports no bugs, DEV succeeds silently.
    struct RoleAwareEngine;
    #[async_trait::async_trait]
    impl AgentEnginePort for RoleAwareEngine {
        fn id(&self) -> &'static str {
            "role-aware"
        }
        async fn run(&self, r: AgentRequest) -> Result<AgentOutcome, PortError> {
            let stdout = if r.system_prompt.contains("Business Analyst") {
                r#"[{"title":"Feature A","priority":"high","complexity":"small","has_ui":false}]"#
                    .to_owned()
            } else if r.system_prompt.contains("Solution Architect") {
                r#"{"approach":"a","files":["a.rs"],"api_contract":"","data_changes":"","test_plan":"t","ux":null}"#
                    .to_owned()
            } else if r.system_prompt.contains("QA Engineer") {
                "[]".to_owned()
            } else {
                "done".to_owned()
            };
            Ok(AgentOutcome {
                stdout,
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn one_cycle_carries_a_feature_from_proposal_to_done() {
        let store = Arc::new(MemStore::default());
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        );
        // Cycle 1: BA proposes → SA designs → DEV-FEATURE implements → TEST clean.
        let report = uc.run_cycle(1).await;
        assert!(
            report.errors.is_empty(),
            "no agent errored: {:?}",
            report.errors
        );
        assert_eq!(report.ba_created.len(), 1, "BA proposed a feature");
        assert!(report.sa_readied.is_some(), "SA readied it");
        assert!(report.feature_done.is_some(), "DEV completed it same cycle");
        assert!(report.documented.is_some(), "DOCS documented it same cycle");

        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Documented);
        assert_eq!(state.current_version.to_string(), "0.1.0");
    }

    struct SpyDeploy {
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl crate::ports::outbound::DeployPort for SpyDeploy {
        async fn deploy(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "spy deployed".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn deploys_after_a_feature_completes() {
        let store = Arc::new(MemStore::default());
        let spy = Arc::new(SpyDeploy {
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&spy) as Arc<dyn crate::ports::outbound::DeployPort>);
        uc.run_cycle(1).await;
        assert_eq!(spy.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
