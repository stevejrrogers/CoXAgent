//! `RunCycleUseCase` — one turn of the loop: BA (periodic) → DEV-BUG →
//! DEV-FEATURE → TEST. A failing agent is recorded and the cycle continues, so
//! one bad run never stalls the team (matching the reference workflow).

use crate::config::Config;
use crate::ports::outbound::{
    AgentEnginePort, AgentRequest, DeployPort, ForgePort, GitPort, StateStorePort,
};
use crate::state::Spend;
use crate::use_cases::run_dev::DevMode;
use crate::use_cases::{
    RunBaUseCase, RunConformanceUseCase, RunDesignSystemUseCase, RunDevUseCase, RunDocsUseCase,
    RunPdUseCase, RunSaUseCase, RunTestUseCase,
};
use coxagent_domain::TicketId;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

mod audits;
mod backlog;
mod ceremonies;
mod debt_sweep;
mod escalation;
mod preflight;
mod scrum;
mod wiring;

mod forge;
mod forge_feedback;
mod forge_merge;
mod forge_review;
mod ops;
mod qa_evidence;
mod recovery;

/// How often (in sprints) the SA runs a whole-system architecture review.
const ARCH_REVIEW_EVERY_SPRINTS: u32 = 8;

/// Local, non-pushed ref updated after every deploy that passes both
/// `deploy()` and `run_tests()` — auto-rollback's source of truth for "last
/// known good". A ref (not a branch tip) survives ticket-branch deletion
/// after a squash-merge.
const LAST_GOOD_REF: &str = "refs/coxagent/last-good";

/// The SA reviewer's JSON verdict on a pull request.
#[derive(serde::Deserialize)]
pub(super) struct ReviewVerdict {
    decision: String,
    #[serde(default)]
    summary: String,
}

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
    /// Whether the cycle produced any real work (used to auto-stop an idle
    /// worker). Errors don't count as work.
    #[must_use]
    pub fn did_work(&self) -> bool {
        !self.ba_created.is_empty()
            || self.sa_readied.is_some()
            || self.design_system_created
            || self.pd_designed.is_some()
            || self.bug_fixed.is_some()
            || self.feature_done.is_some()
            || self.documented.is_some()
            || !self.bugs_filed.is_empty()
    }

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

/// Extract the `version = "MAJOR.MINOR.PATCH"` from a Cargo.toml body, or
/// `None` when absent/unparseable.
fn parse_cargo_version(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("version") {
            if let Some(rhs) = rest.trim_start().strip_prefix('=') {
                let v = rhs.trim().trim_matches('"').trim();
                if coxagent_domain::SemVer::parse(v).is_ok() {
                    return Some(v.to_owned());
                }
            }
        }
    }
    None
}

/// The version to reconcile `state_ver` down to, or `None` when there is no
/// drift to correct. Returns `repo_ver` when state has optimistically bumped
/// ahead of what the tree declares; `None` when state is at or behind reality
/// or `repo_ver` is unparseable.
fn reconcile_target(
    state_ver: &coxagent_domain::SemVer,
    repo_ver: &str,
) -> Option<coxagent_domain::SemVer> {
    let repo = coxagent_domain::SemVer::parse(repo_ver).ok()?;
    (state_ver > &repo).then_some(repo)
}

/// The composition root's engine-rebuild hook: `Some(new parts)` only when the
/// on-disk config changed since last asked (see [`RunCycleUseCase::with_reloader`]).
pub type Reloader<E> = Arc<dyn Fn() -> Option<(Config, Arc<E>, Arc<Mutex<Spend>>)> + Send + Sync>;

/// Runs the sequential agent cycle over shared adapters.
pub struct RunCycleUseCase<S: StateStorePort, E: AgentEnginePort> {
    store: Arc<S>,
    engine: Arc<E>,
    config: Config,
    work_dir: PathBuf,
    context: String,
    meter: Option<Arc<Mutex<Spend>>>,
    shot: Option<Arc<dyn crate::ports::outbound::ScreenshotPort>>,
    probe: Option<Arc<dyn crate::ports::outbound::ApiProbePort>>,
    storage: Option<Arc<dyn crate::ports::outbound::StoragePort>>,
    deploy: Option<Arc<dyn DeployPort>>,
    /// The published `host_port` to probe for the mandatory post-deploy
    /// health gate, or `Err` when the raw `coxagent.json`'s
    /// `deploy.host_port` is present but malformed (COX-B035). Defaults to
    /// `Ok(config.deploy.host_port)`; callers reading the raw config
    /// separately (to fail closed on a malformed value the `Config` parse
    /// itself may have folded into a default) override it via
    /// [`Self::with_host_port_probe`].
    host_port_probe: Result<Option<u16>, ()>,
    notifier: Option<Arc<dyn crate::ports::outbound::NotifierPort>>,
    /// Reporter that pushes PR/review activity to the hub over HTTP. The runner
    /// is the sole holder of forge credentials, so the hub must be told what it
    /// learned rather than listing PRs itself.
    reporter: Option<Arc<dyn crate::ports::outbound::PrReporterPort>>,
    /// Live, runtime-adjustable spend caps (overrides the config caps when set).
    budget: Option<crate::config::LiveBudget>,
    /// Local git, used for branch + commit per ticket when `config.git.enabled`.
    git: Option<Arc<dyn GitPort>>,
    /// Workspace file access for team notes, memory indexes and generated
    /// maps — `None` in tests reads as an empty filesystem.
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    /// OS process janitor for the pre-cycle orphan sweep; `None` in tests.
    janitor: Option<Arc<dyn crate::ports::outbound::ProcessJanitorPort>>,
    /// The code host, used to open PRs when `config.git.auto_pr`.
    forge: Option<Arc<dyn ForgePort>>,
    /// Reports the currently executing agent to the runner (live "working now").
    phase: Option<crate::use_cases::runner::PhaseReporter>,
    /// Rebuild hook for hot-reloading engine+config on a file change (set by
    /// the composition root; `None` in tests).
    reloader: Option<Reloader<E>>,
    /// Polled between phases: `true` = the user pressed Pause, stop starting
    /// new phases and end this cycle early. `None` (tests/headless) = never.
    pause_check: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    /// This runner's identity (`account@host`) — recorded as the ticket claim
    /// owner so concurrent runners on a shared backlog never collide.
    worker: String,
    /// What this machine can run (see `set_capabilities`).
    caps: crate::ports::outbound::WorkerCaps,
    /// Last scrum discussion topic — skip duplicate discussions.
    /// Whether the `sandbox_unsupported` warning has already fired — posted
    /// once per project per process lifetime, never once per cycle.
    sandbox_warned: AtomicBool,
}

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    pub fn new(
        store: Arc<S>,
        engine: Arc<E>,
        config: Config,
        work_dir: PathBuf,
        context: String,
    ) -> Self {
        let host_port_probe = Ok(config.deploy.host_port);
        Self {
            store,
            engine,
            config,
            work_dir,
            context,
            meter: None,
            shot: None,
            probe: None,
            storage: None,
            deploy: None,
            host_port_probe,
            notifier: None,
            reporter: None,
            budget: None,
            git: None,
            files: None,
            janitor: None,
            forge: None,
            phase: None,
            reloader: None,
            pause_check: None,
            worker: String::new(),
            caps: crate::ports::outbound::WorkerCaps::default(),
            sandbox_warned: AtomicBool::new(false),
        }
    }

    /// Set this runner's identity (`account@host`), used as the ticket claim
    /// owner. Called by `run_forever` from the live operator each cycle.
    pub fn set_worker(&mut self, worker: impl Into<String>) {
        self.worker = worker.into();
    }

    /// Hot-reload the engine + config mid-run, called at a cycle boundary when
    /// `coxagent.json` changed — so a Settings edit (a new default engine, a
    /// per-role model, a routing change) takes effect on the NEXT cycle WITHOUT
    /// restarting the process. The spend meter is swapped too; the previous one
    /// was already drained into state at the last cycle's end, so nothing is
    /// lost. Everything else — worker identity, forge, the phase reporter — is
    /// preserved, since none of it depends on the engine config.
    pub fn reload(&mut self, config: Config, engine: Arc<E>, meter: Arc<Mutex<Spend>>) {
        self.config = config;
        self.engine = engine;
        self.meter = Some(meter);
    }

    /// Attach the composition root's rebuild hook: it returns `Some(new parts)`
    /// only when the on-disk config actually changed since last asked. The
    /// application layer cannot read the file itself (IO stays in adapters), so
    /// the closure carries that knowledge in.
    #[must_use]
    pub fn with_reloader(mut self, reloader: Reloader<E>) -> Self {
        self.reloader = Some(reloader);
        self
    }

    /// Install the runner's pause probe (composition root only).
    pub fn set_pause_check(&mut self, check: std::sync::Arc<dyn Fn() -> bool + Send + Sync>) {
        self.pause_check = Some(check);
    }

    /// Whether the user has asked to pause — checked between phases so Pause
    /// cuts the cycle at the next boundary instead of after the whole cycle.
    fn pause_requested(&self) -> bool {
        self.pause_check.as_ref().is_some_and(|c| c())
    }

    /// Apply a pending config change if the reload hook reports one. Called by
    /// the runner at each cycle boundary; a no-op without a hook or a change.
    /// Returns whether a reload happened, so the caller can invalidate anything
    /// derived from the OLD engine stack (e.g. the per-role observed-engine
    /// badges — an observation of an engine that no longer runs is not truth).
    pub fn maybe_reload(&mut self) -> bool {
        let Some(hook) = &self.reloader else {
            return false;
        };
        let Some((config, engine, meter)) = hook() else {
            return false;
        };
        tracing::info!("config changed — engine reloaded and applied without a restart");
        self.reload(config, engine, meter);
        true
    }

    /// Declare the agent CLIs this machine can launch, so the presence heartbeat
    /// carries them. Both this and the runner's own heartbeat upsert the SAME
    /// registry key — leaving it unset here would blank out what the runner
    /// reported, and the dashboard would flicker back to "no agent CLI".
    pub fn set_capabilities(&mut self, caps: crate::ports::outbound::WorkerCaps) {
        self.caps = caps;
    }

    /// Trigger the whole-system architecture review on demand (same work the
    /// periodic 8-sprint review does), for acceptance/manual runs.
    pub async fn review_architecture_now(&self, sprint: u32) {
        self.architecture_audit(sprint).await;
    }

    /// Trigger the Wiki documentation review on demand.
    pub async fn review_docs_now(&self, sprint: u32) {
        self.docs_audit(sprint).await;
    }

    /// Set the live phase reporter (called by `run_forever`).
    pub fn set_phase_reporter(&mut self, reporter: crate::use_cases::runner::PhaseReporter) {
        self.phase = Some(reporter);
    }

    /// A clone of the shared store handle — lets the loop wire a phase reporter
    /// that heartbeats the worker registry with the live role + ticket.
    #[must_use]
    pub fn store(&self) -> Arc<S> {
        Arc::clone(&self.store)
    }

    /// Report the agent about to run (live "working now"). `note` is a short
    /// context like a ticket id; empty when there's none.
    fn report(&self, role: &str, note: &str) {
        if let Some(p) = &self.phase {
            p(Some((role.to_owned(), note.to_owned())));
        }
    }

    /// Clear the live indicator between phases.
    fn report_idle(&self) {
        if let Some(p) = &self.phase {
            p(None);
        }
    }

    /// Attach a live budget cell so cap changes apply without restarting.
    #[must_use]
    pub fn with_live_budget(mut self, budget: crate::config::LiveBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Attach local git so completed tickets are committed (and, later, pushed)
    /// when `config.git.enabled`.
    #[must_use]
    pub fn with_git(mut self, git: Arc<dyn GitPort>) -> Self {
        self.git = Some(git);
        self
    }

    /// Attach the code host so completed tickets open a PR when
    /// `config.git.auto_pr`.
    #[must_use]
    pub fn with_forge(mut self, forge: Arc<dyn ForgePort>) -> Self {
        self.forge = Some(forge);
        self
    }

    /// The branch the agent opens PRs into and auto-merges — `target_branch`
    /// when set, otherwise the repository default.
    fn flow_base(&self) -> &str {
        let t = self.config.git.target_branch.trim();
        if t.is_empty() {
            &self.config.git.default_branch
        } else {
            t
        }
    }

    /// Record a git action in the activity feed (best-effort).
    async fn log_git(&self, action: &str) {
        if let Ok(mut s) = self.store.load().await {
            s.log_activity("GIT", action, None);
            let _ = self.store.save(&s).await;
        }
    }

    /// (Re)index the working tree into the code graph + `REPO_MAP.md`, so agents
    /// can orient from it. Gated by the token-saver; refreshed on the first cycle
    /// and periodically (indexing off the async runtime). Best-effort.
    async fn refresh_codegraph(&self, cycle: u64) {
        if !self.config.workflow.token_saver {
            return;
        }
        // Only index a real managed codebase — marked by coxagent.json in the
        // codebase itself OR (the standard layout) in the workspace root one
        // level up (<ws>/coxagent.json beside <ws>/codebase). The old
        // codebase-only check never matched the standard layout, so the map
        // silently went stale forever.
        let managed = self.work_dir.join("coxagent.json").exists()
            || self
                .work_dir
                .parent()
                .is_some_and(|p| p.join("coxagent.json").exists());
        if !managed {
            return;
        }
        let map = self.work_dir.join(".coxagent").join("codegraph.json");
        let missing = !map.exists();
        if !(missing || cycle % 3 == 1) {
            return;
        }
        let Some(files) = self.files.clone() else {
            return;
        };
        let root = self.work_dir.clone();
        let _ = tokio::spawn(async move {
            let g = crate::codegraph::CodeGraph::index(files.as_ref(), &root).await;
            // save() also writes REPO_MAP.md — one producer, both artifacts.
            let _ = g.save(files.as_ref(), &root).await;
        })
        .await;
    }

    /// Reconcile the state's `current_version` against the version the checked-out
    /// tree actually declares. The version is bumped optimistically when a DEV
    /// ticket completes locally — *before* its PR merges. If that PR is later
    /// rejected or closed, `state.current_version` stays a phantom release no
    /// build carries. This pass pulls it back to reality, but only when it is
    /// provably safe: no ticket is mid-flight (in-progress/claimed/fixed pending
    /// PR), so nothing is actively bumping the version.
    async fn reconcile_version(&self) {
        let Some(files) = self.files.clone() else {
            return;
        };
        let cargo = self.work_dir.join("Cargo.toml");
        let Some(text) = files.read(&cargo).await else {
            return;
        };
        let Some(repo_ver) = parse_cargo_version(&text) else {
            return;
        };

        let Ok(mut state) = self.store.load().await else {
            return;
        };
        // Never stomp active work: if a ticket is claimed/in-flight it may be
        // mid-bump; wait for it to resolve.
        let ticket_in_flight = state.tickets.iter().any(|t| {
            matches!(
                t.status(),
                coxagent_domain::ticket::Status::InProgress
                    | coxagent_domain::ticket::Status::Fixed
            )
        });
        if ticket_in_flight {
            return;
        }
        let state_ver = state.current_version.clone();
        // What version should state be reconciled to, or None when no drift.
        // Purely a function of (state version, repo version) — the in-flight
        // guard above already ruled out active work.
        if let Some(target) = reconcile_target(&state_ver, &repo_ver) {
            tracing::warn!(
                "reconcile: state version {} ahead of repo {} with no work in flight — reverting",
                state_ver,
                repo_ver
            );
            state.current_version = target.clone();
            state.log_activity(
                "SYSTEM",
                &format!("reconciled version {state_ver} → {target} (phantom bump, no PR merged)"),
                None,
            );
            let _ = self.store.save(&state).await;
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

    /// Attach the reporter that pushes PR/review activity to the hub over HTTP.
    ///
    /// The runner is the only holder of forge credentials, so without this it
    /// keeps working (git/forge operations are unaffected) but never publishes
    /// what it opened or reviewed to the shared dashboard.
    #[must_use]
    pub fn with_reporter(
        mut self,
        reporter: Arc<dyn crate::ports::outbound::PrReporterPort>,
    ) -> Self {
        self.reporter = Some(reporter);
        self
    }

    /// The runner's PR reporter, or a no-op when none was configured.
    pub(crate) fn reporter(&self) -> Arc<dyn crate::ports::outbound::PrReporterPort> {
        self.reporter
            .clone()
            .unwrap_or_else(|| Arc::new(crate::ports::outbound::NullPrReporter))
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

    /// Attach a screenshot capability for post-deploy visual QA.
    #[must_use]
    pub fn with_shot(
        mut self,
        shot: Option<Arc<dyn crate::ports::outbound::ScreenshotPort>>,
    ) -> Self {
        self.shot = shot;
        self
    }

    /// Attach an HTTP probe for API evidence capture.
    #[must_use]
    pub fn with_probe(
        mut self,
        probe: Option<Arc<dyn crate::ports::outbound::ApiProbePort>>,
    ) -> Self {
        self.probe = probe;
        self
    }

    /// Attach blob storage so evidence files (screenshots) live with the rest
    /// of the project media (local disk or MinIO/S3).
    #[must_use]
    pub fn with_storage(
        mut self,
        storage: Option<Arc<dyn crate::ports::outbound::StoragePort>>,
    ) -> Self {
        self.storage = storage;
        self
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

    /// Override the post-deploy health-gate probe port (COX-B035): pass
    /// `Err(())` when the raw `coxagent.json` names a malformed
    /// `deploy.host_port`, so the gate fails closed instead of the default
    /// (`Ok(config.deploy.host_port)`) treating a `Config`-parse fallback's
    /// `None` as "nothing configured".
    #[must_use]
    pub fn with_host_port_probe(mut self, probe: Result<Option<u16>, ()>) -> Self {
        self.host_port_probe = probe;
        self
    }

    /// Run one cycle. Never returns `Err`: agent failures are collected into the
    /// report so the outer loop keeps going.
    #[allow(clippy::too_many_lines)] // a linear sequence of agent phases; splitting hurts readability
    pub async fn run_cycle(&self, cycle: u64) -> CycleReport {
        if let Some(janitor) = &self.janitor {
            janitor.kill_orphaned_drivers(&self.work_dir);
        }
        self.warn_if_sandbox_unsupported().await;

        let mut report = CycleReport {
            cycle,
            ..CycleReport::default()
        };

        // Policy gate: refuse to run agents on a model outside the allowlist —
        // stop before spending a token rather than after.
        if self.model_policy_blocks(&mut report).await {
            return report;
        }

        // Coordinate concurrent runners: at most one leads the singleton phases
        // (BA/PO/design-system/deploy/TEST/review) that must run once per project,
        // not once per runner. Non-leaders still do per-ticket stages (SA/PD/DEV/
        // DOCS) on tickets they claim, so real work runs in parallel without
        // duplication. Falls back to leader when the backend can't coordinate.
        let me = if self.worker.is_empty() {
            "local".to_owned()
        } else {
            self.worker.clone()
        };
        let now = crate::state::now_rfc3339();
        let leader = self.store.acquire_leader(&me, &now).await.unwrap_or(true);
        // Authoritative cycle number: the persistent, project-wide counter the
        // leader advances once per cycle. Scoring *and* cadence key off this,
        // NOT the per-process local number — so restarts / leader handovers
        // neither renumber the scorecard nor reset the codegraph/debt/BA/scrum
        // cadence. Falls back to the local number if the store hiccups.
        let cycle = if leader {
            // Advance the persistent project-cycle counter: load state, bump
            // `state.cycle`, save, and use the advanced number. On any store
            // hiccup fall back to the local number so a state problem never
            // stalls a cycle.
            match self.store.load().await {
                Ok(mut s) => {
                    // Pure, testable: seeds past existing scorecard history on
                    // first use, then advances the persistent counter (see
                    // `advance_project_cycle` below).
                    let next = advance_project_cycle(&mut s);
                    let _ = self.store.save(&s).await;
                    next
                }
                Err(_) => cycle,
            }
        } else {
            cycle
        };
        report.cycle = cycle;
        // Merge-queue recovery flag (set by the leader once the queue blows up).
        let mut recovery = false;
        // Human-queued execution jobs (force-merge …) run before anything else.
        self.drain_jobs().await;
        // Announce presence in the shared registry so every dashboard (even on
        // another machine) can list this team as online.
        let _ = self
            .store
            .heartbeat_worker(
                &me,
                if leader { "leader" } else { "worker" },
                "",
                &self.caps,
                &now,
            )
            .await;

        // Keep the code map fresh so `.coxagent/REPO_MAP.md` reflects the tree
        // the agents are about to work on (best-effort, token-saver-gated).
        // Leader-only: it writes shared files under the repo.
        if leader {
            self.refresh_codegraph(cycle).await;
            // Drift-check: if the state's version has been bumped ahead of what
            // the checked-out tree actually declares, and no ticket is mid-flight
            // to justify it, pull it back to reality. This closes the "version
            // bumped but the PR never merged" drift (PR rejected/closed) that
            // otherwise leaves a phantom release version that no build carries.
            self.reconcile_version().await;
            // Seed the standard Wiki spaces and re-file any legacy pages that
            // were dumped under the wrong space (once, on the first cycle).
            if cycle == 1 {
                if let Ok(mut s) = self.store.load().await {
                    s.ensure_standard_folders();
                    s.normalize_doc_folders();
                    let _ = self.store.save(&s).await;
                }
            }
            // Scrum: open/roll over the sprint at the start of the cycle.
            self.advance_sprint_if_scrum(cycle).await;

            // One digest per UTC day into the team chat: shipped/spend/sprint at
            // a glance, so the user doesn't need the dashboard open to keep up.
            self.post_daily_digest().await;

            // Self-correcting memory: audit the engine's per-machine notes
            // against the current process law once a day.
            self.memory_hygiene().await;
            // Self-tuning: react to our own evals (daily) — quality/intake brakes.
            self.self_tune().await;
            // Forge hygiene: rebase open PRs onto the moving base + learn
            // from PRs a human closed without merging.
            self.forge_hygiene().await;
            // Stop starting, start finishing: review + merge the PR queue at
            // the TOP of the cycle. This used to run at the very end — after
            // codegraph, ceremonies and the (tens-of-minutes) dev phases — so
            // mergeable PRs aged a whole cycle before anyone looked at them.
            self.review_open_prs().await;
            self.address_pr_feedback().await;
            // Debt sweep cadence: every 10th cycle files ONE tech-debt chore
            // (lint baseline, dead code, missing docs) if none is open — the
            // discipline of paying debt down on a schedule instead of never.
            if cycle % 10 == 0 {
                self.file_debt_sweep(cycle).await;
            }

            // SM dispatch FIRST (agents resolve), then report what remains.
            // Boxed: the escalation ladder holds a failure log across its awaits,
            // and inlining it here pushes the whole cycle future over 16KB.
            Box::pin(self.sm_unpark_tickets()).await;
            self.impediment_watch().await;

            // Merge-queue recovery gate: with a blown-up queue the ONLY useful
            // work is merging — creative roles are paused below.
            recovery = self.run_queue_recovery(self.open_pr_count().await).await;

            // Ops/SRE: ping the deployed app; file a bug + alert on an outage.
            self.ops_monitor().await;

            // Daily standup: every few cycles the SM runs the room — but only when
            // the team actually did something since last time. A standup with no
            // real activity is pure token burn (and reads like noise), so we skip
            // it when the board has been quiet.
            // A standup is a DAILY ceremony. Every third cycle means one every
            // ~20 minutes, and each one asks every agent for an update — that
            // alone was most of the SM's 487 engine runs. Once a day, and only
            // if the team did something since.
            if self.claim_daily("standup").await && self.has_recent_activity().await {
                self.scrum_standup().await;
            }
            // Backlog grooming is a DAILY ceremony, not an every-few-cycles one —
            // `cycle % 3` reposted "let's get the top items ready" every ~90s.
            // Once a day (persisted, like the standup), and only in scrum mode.
            if self.config.workflow.mode == crate::config::Mode::Scrum
                && self.claim_daily("grooming").await
            {
                self.scrum_grooming().await;
            }

            // During a refactor sprint BA proposes nothing new — the whole point
            // is to stop piling features on a shaky base. SA also realigns the
            // specs of pending features to the target architecture, and the mode
            // clears itself once the refactor chores are done.
            let refactoring = self.maintain_refactor_sprint().await;

            // BA runs on the first cycle of each period. `(cycle-1) % n == 0` is
            // correct for every n including 1 (unlike `cycle % n == 1`).
            let ba_every = self.config.workflow.ba_every_n_cycles;
            let ba_paused = self.store.load().await.is_ok_and(|s| s.tuning.skip_ba);
            if ba_paused && ba_every > 0 && (cycle - 1) % ba_every == 0 {
                report
                    .errors
                    .push("BA: paused by self-tuning — backlog outgrew throughput".to_owned());
            }
            if !ba_paused && !refactoring && ba_every > 0 && (cycle - 1) % ba_every == 0 {
                if recovery {
                    report
                        .errors
                        .push("BA: paused — merge-queue recovery".to_owned());
                } else {
                    self.report("BA", "proposing features");
                    match self.ba().execute().await {
                        Ok(ids) => report.ba_created = ids,
                        Err(e) => report.errors.push(format!("BA: {e}")),
                    }
                }
            }

            // Team hygiene: reject any duplicate tickets before design/dev.
            self.dedup_backlog().await;

            // Both of these are author-once jobs that re-check every cycle and
            // almost always exit early — but the early exit still costs a
            // model call. Once a day is enough for work that changes monthly.
            if self.claim_daily("po-milestones").await {
                self.report("PO", "planning milestones");
                if let Err(e) = self.milestones().execute().await {
                    report.errors.push(format!("PO milestones: {e}"));
                }
            }
            // Release pipeline: tag + file a Release chore for every milestone
            // whose target version is reached and whose goal is complete. Cheap
            // and idempotent (skips unfulfilled/not-yet-reached milestones and
            // already-tagged releases), so it runs each leader cycle rather than
            // waiting for a daily slot — releasing the moment a milestone ships.
            match self.releases().execute().await {
                Ok(released) if !released.is_empty() => {
                    self.report(
                        "RELEASE",
                        &format!("released milestone(s): {}", released.join(", ")),
                    );
                }
                Ok(_) => {}
                Err(e) => report.errors.push(format!("RELEASE: {e}")),
            }
            if self.claim_daily("pd-design-system").await {
                self.report("PD", "designing UX");
                match self.design_system().execute().await {
                    Ok(created) => report.design_system_created = created,
                    Err(e) => report.errors.push(format!("PD design-system: {e}")),
                }
            }
        }

        // SA/PD/DEV/DOCS report "working now" from inside, only after they win
        // the per-ticket claim — so a runner that loses the race (or has nothing
        // to do) shows idle instead of falsely mirroring the busy one.
        //
        // RECOVERY law: conflicts are cleared BEFORE any new feature work —
        // that includes design. Designing new tickets during recovery just
        // grows the backlog pressure that caused the pile-up.
        if recovery {
            report
                .errors
                .push("SA/PD: design paused — merge-queue recovery, conflicts first".to_owned());
        } else {
        if self.pause_requested() {
            report.errors.push("cycle cut short — paused by user".to_owned());
            return report;
        }
            match self.sa().execute().await {
                Ok(id) => report.sa_readied = id,
                Err(e) => report.errors.push(format!("SA: {e}")),
            }

            // PD authors UX for a UI ticket SA left pending, taking it to ready.
            match self.pd().execute().await {
                Ok(id) => report.pd_designed = id,
                Err(e) => report.errors.push(format!("PD: {e}")),
            }
        }

        // PR-queue backpressure: when too many PRs are already open, STOP
        // starting new branches (bugs AND features) — every extra parallel
        // branch multiplies merge conflicts (cascade). Draining the queue
        // (review / fix feedback / resolve conflicts / merge) IS the dev work
        // until it's back under the limit.
        let queue_full = self.pr_queue_full().await;
        if queue_full {
            report
                .errors
                .push("DEV: paused new work — PR queue full, draining reviews first".to_owned());
        }

        if !queue_full {
        if self.pause_requested() {
            report.errors.push("cycle cut short — paused by user".to_owned());
            return report;
        }
            match self.dev(DevMode::Bug).execute().await {
                Ok(id) => {
                    if let Some(tid) = &id {
                        self.commit_for_ticket(tid, "fix").await;
                    }
                    report.bug_fixed = id;
                }
                Err(e) => report.errors.push(format!("DEV-BUG: {e}")),
            }
        }
        let bugs_first = self.store.load().await.is_ok_and(|s| s.tuning.bugs_first);
        if bugs_first {
            report
                .errors
                .push("DEV-FEATURE: paused by self-tuning — burning down bugs first".to_owned());
        }
        if self.pause_requested() {
            report.errors.push("cycle cut short — paused by user".to_owned());
            return report;
        }
        if self.config.workflow.feature_dev_enabled && !queue_full && !bugs_first {
            // Before building, make sure the next feature has a clear definition
            // of done — DEV raises unclear tickets and the BA fills them in.
            self.clarify_next_feature().await;
            match self.dev(DevMode::Feature).execute().await {
                Ok(id) => {
                    if let Some(tid) = &id {
                        self.commit_for_ticket(tid, "feat").await;
                    }
                    report.feature_done = id;
                }
                Err(e) => report.errors.push(format!("DEV-FEATURE: {e}")),
            }
        }

        // Answer what the team asked before anyone works on it again: a
        // developer waiting on a requirement is blocked, and the answer is
        // cheap compared with a wrong implementation. Bounded per cycle.
        Box::pin(self.answer_open_questions()).await;
        self.escalate_stale_human_questions().await;
        // Fill the acceptance criteria BEFORE the gate judges the ticket: a
        // ticket nobody can check is one a human can only bounce, and the
        // missing AC alone scores it out of the auto lane.
        Box::pin(self.preflight_acceptance_criteria()).await;
        // The adaptive gate runs AFTER design and before the next dev pass:
        // routine work reaches Ready in the same cycle it was designed.
        self.adaptive_approval_pass().await;

        // DOCS documents the next completed feature (per-ticket stage claim).
        match self.docs().execute().await {
            Ok(id) => report.documented = id,
            Err(e) => report.errors.push(format!("DOCS: {e}")),
        }

        // Leader-only tail: deploy, whole-build verification, governance, and PR
        // review/merge each act on the shared build/repo and must run once.
        if leader {
            // Deploy after code changes so TEST verifies a running build — and
            // ALSO retry when the last deploy failed, even with no new code:
            // transient causes (a port squatter that's since gone, docker
            // hiccups) must self-resolve, not sit red until a human clicks.
            let pre_deploy_state = self.store.load().await.ok();
            let last_deploy_failed = pre_deploy_state
                .as_ref()
                .and_then(|s| s.deploy.as_ref())
                .is_some_and(|d| !d.ok);
            // Once a rollback is live, keep retrying a forward deploy every
            // leader cycle (self-healing) even with no new ticket work —
            // otherwise the team only finds out the fix landed the next time
            // a feature/bug ships, which could be a long time.
            let was_in_rollback = pre_deploy_state.as_ref().is_some_and(|s| s.in_rollback);
            if (report.feature_done.is_some()
                || report.bug_fixed.is_some()
                || last_deploy_failed
                || was_in_rollback)
                && self.deploy.is_some()
                && self.config.deploy.enabled
            {
                if let Some(deploy) = &self.deploy {
                    // Captured now, before the attempt — names exactly what
                    // this deploy builds/runs, so a rollback trigger below
                    // (or a later known-good record) points at a real commit.
                    let attempt_sha = match &self.git {
                        Some(git) => git.head_sha(&self.work_dir).await.ok(),
                        None => None,
                    };
                    let mut deploy_bad = false;
                    let mut deploy_ran_ok = false;
                    match deploy.deploy(&self.work_dir).await {
                        Ok(r) if r.deployed => {
                            // Mandatory health gate (COX-B004): `docker compose
                            // up` exiting 0 only proves the containers
                            // started — it says nothing about whether the app
                            // inside actually bound its configured port. A
                            // "successful" exit that never answers on the
                            // port is downgraded to a deploy failure here, so
                            // it can never become the auto-rollback target
                            // and always drives rollback/bug-filing below
                            // like any other deploy failure. Can't be skipped
                            // — runs whenever the compose command reported
                            // success.
                            let (health_ok, health_check) = if r.success {
                                self.run_health_check().await
                            } else {
                                (true, None)
                            };
                            let success = r.success && health_ok;
                            let summary = match &health_check {
                                Some(h) if !health_ok => {
                                    format!("{} {}", r.summary, describe_health_failure(h))
                                }
                                _ => r.summary.clone(),
                            };
                            self.record_deploy(
                                success,
                                &summary,
                                attempt_sha.clone(),
                                health_check,
                            )
                            .await;
                            let kind = if success {
                                "deploy_ok"
                            } else {
                                "deploy_failed"
                            };
                            self.notify(kind, summary.clone()).await;
                            // Visual QA: someone finally LOOKS at the shipped
                            // UI. Only when this cycle shipped a UI feature.
                            if success {
                                deploy_ran_ok = true;
                                for id in [report.feature_done.clone(), report.bug_fixed.clone()]
                                    .into_iter()
                                    .flatten()
                                {
                                    self.collect_evidence(&id).await;
                                }
                                if let Some(id) = report.feature_done.clone() {
                                    self.visual_qa(&id, &mut report).await;
                                }
                            }
                            // Retry: shipped tickets still missing DoD
                            // evidence (earlier capture failed) get another
                            // attempt each cycle, so the TEST gate can't
                            // deadlock on a transient failure.
                            if success {
                                self.collect_missing_evidence().await;
                            }
                            // A failed deploy must become work, or nothing fixes
                            // it: file it as a high-priority bug (deduped).
                            if !success {
                                deploy_bad = true;
                                if let Some(id) = self.file_deploy_bug(&summary).await {
                                    report.bugs_filed.push(id);
                                }
                            }
                        }
                        Ok(_) => {}
                        // A spawn/timeout error never even produced a
                        // DeployReport (COX-B039) — without this arm
                        // `deploy_bad` stays false, `record_deploy` never
                        // runs (so `state.deploy.ok` keeps reporting the
                        // PREVIOUS deploy's status), no bug is filed, and
                        // `attempt_rollback` never fires even though
                        // `docker_compose::deploy()` already ran `down`
                        // before failing — the app is left stopped. Route it
                        // through the same success=false path as an unhealthy
                        // deploy so all four (state, bug, notify, rollback)
                        // happen here too.
                        Err(e) => {
                            deploy_bad = true;
                            let summary = format!("deploy failed: {e}");
                            self.record_deploy(false, &summary, attempt_sha.clone(), None)
                                .await;
                            self.notify("deploy_failed", summary.clone()).await;
                            if let Some(id) = self.file_deploy_bug(&summary).await {
                                report.bugs_filed.push(id);
                            }
                            report.errors.push(format!("DEPLOY: {e}"));
                        }
                    }
                    // Hard DoD gate: run the real test suite. A red suite becomes a
                    // high-priority bug (deduped) — deterministic quality, not just
                    // the LLM TEST agent's judgement.
                    let mut tests_bad = false;
                    match deploy.run_tests(&self.work_dir).await {
                        Ok(r) if r.deployed && !r.success => {
                            tests_bad = true;
                            if let Some(id) = self.file_test_failure(&r.summary).await {
                                report.bugs_filed.push(id);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => report.errors.push(format!("TESTS: {e}")),
                    }
                    if deploy_bad || tests_bad {
                        let reason = if deploy_bad {
                            "deploy failed"
                        } else {
                            "tests failed"
                        };
                        self.attempt_rollback(reason, attempt_sha, &mut report)
                            .await;
                    } else if deploy_ran_ok {
                        self.record_known_good(attempt_sha, "deploy + tests passed")
                            .await;
                    }
                }
            }

            // Show what TEST is verifying: the ticket that just shipped into this
            // build (falls back to a generic build check).
            let verifying = report
                .feature_done
                .as_ref()
                .or(report.bug_fixed.as_ref())
                .map_or_else(|| "build".to_owned(), ToString::to_string);
            if recovery {
                // TEST would only re-discover bugs whose fixes are stuck in the
                // queue and file duplicates — hold it until the queue drains.
                report
                    .errors
                    .push("TEST: paused — merge-queue recovery".to_owned());
            } else if !self.test_has_work(&report).await {
                // Nothing shipped since last TEST and no fixes await
                // verification — an idle TEST pass only burns tokens
                // re-confirming what it confirmed last cycle.
                report
                    .errors
                    .push("TEST: skipped — nothing new to verify".to_owned());
            } else {
                self.report("TEST", &verifying);
                match self.test().execute().await {
                    Ok(ids) => report.bugs_filed = ids,
                    Err(e) => report.errors.push(format!("TEST: {e}")),
                }
                // Deterministic screenshot pass: attach a real image to each UI
                // ticket's test cases the TEST agent just marked pass/fail.
                self.attach_test_case_screenshots().await;

                // Governance: architecture-conformance drift becomes tracked bugs.
                match self.conformance().execute().await {
                    Ok(mut ids) => report.bugs_filed.append(&mut ids),
                    Err(e) => report.errors.push(format!("CONFORMANCE: {e}")),
                }
            }

            // Second review pass at cycle end: catches PRs the DEV phases just
            // opened, so fresh work can land within the SAME cycle instead of
            // waiting for the next one. (The main drain runs at the cycle top.)
            self.review_open_prs().await;
            self.address_pr_feedback().await;

            // "Agents don't sleep": if finished work is sitting UNCOMMITTED in
            // the tree — DEV completed & verified a ticket but its ship path
            // (commit_for_ticket) was interrupted by a crash or transient
            // failure before it ran — sweep it up into a per-ticket branch +
            // PR now instead of leaving it stranded on main cycle after cycle.
            self.sweep_unshipped_work().await;

            // Scrum comes alive: when there's a real tension (deploy failure, a
            // bug pile-up, or a periodic check-in), the team actually discusses
            // it — PO & SA weigh in, SM decides, and a decision can spawn a
            // ticket. Posts land in the Scrum feed.
            if self.pause_requested() {
            report.errors.push("cycle cut short — paused by user".to_owned());
            return report;
        }
            self.scrum_discussion(&report, cycle).await;
        }
        self.report_idle();

        report.over_budget = self.record_activity(&report, leader).await;
        if report.over_budget {
            self.notify(
                "budget_reached",
                "spend cap reached — loop paused".to_owned(),
            )
            .await;
        }
        // Every configured engine hit a quota / rate-limit wall — pause the loop
        // instead of spinning uselessly, and tell the team why.
        if report
            .errors
            .iter()
            .any(|e| e.contains("ALL_ENGINES_QUOTA_EXHAUSTED"))
        {
            report.over_budget = true;
            let vi = self.config.workflow.language.is_vi();
            let msg = if vi {
                "🛑 Tất cả engine đều hết quota/token — tạm dừng vòng chạy. Nạp lại quota hoặc \
                 thêm engine fallback (Settings → engine.fallbacks) rồi resume."
            } else {
                "🛑 Every engine hit a quota/token wall — pausing the loop. Top up quota or add a \
                 fallback engine (Settings → engine.fallbacks), then resume."
            };
            // Land where people actually look — the team channel, with the
            // configured exception owner tagged so it pings them — not a ticket
            // comment thread nobody has open.
            let owner = self
                .config
                .workflow
                .human
                .route_exceptions_to
                .as_deref()
                .unwrap_or("")
                .trim();
            let text = if owner.is_empty() {
                msg.to_owned()
            } else {
                // An @mention in the body is what the chat UI highlights and
                // pings on — tag the configured exception owner directly.
                format!("@{owner} {msg}")
            };
            if let Ok(mut s) = self.store.load().await {
                s.post_chat_in("SYSTEM", &text, crate::state::AGENTS_CHANNEL, Vec::new());
                s.log_activity("SM", "paused — engines out of quota", None);
                let _ = self.store.save(&s).await;
            }
            self.notify("quota_exhausted", msg.to_owned()).await;
        }
        report
    }

    /// Attach the OS process janitor for the pre-cycle orphan sweep.
    #[must_use]
    pub fn with_janitor(
        mut self,
        janitor: Option<Arc<dyn crate::ports::outbound::ProcessJanitorPort>>,
    ) -> Self {
        self.janitor = janitor;
        self
    }

    /// Attach workspace-file access (team notes, memory indexes, maps).
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// The engine this cycle drives, for outage reporting.
    pub fn engine_id(&self) -> &'static str {
        self.engine.id()
    }

    /// Fires the `budget_warning` NotifierPort event(s) computed by
    /// [`apply_budget_warnings`] — split out of `record_activity` to keep it
    /// under the function-length lint, and because dispatch is a distinct
    /// concern from the pure flag bookkeeping.
    async fn notify_budget_warnings(&self, new_lifetime: bool, new_daily: bool, warn_pct: f64) {
        let pct = warn_pct * 100.0;
        if new_lifetime {
            self.notify(
                "budget_warning",
                format!("lifetime spend crossed {pct:.0}% of the budget cap — approaching the automatic pause"),
            )
            .await;
        }
        if new_daily {
            self.notify(
                "budget_warning",
                format!("today's spend crossed {pct:.0}% of the daily budget cap — approaching the automatic pause"),
            )
            .await;
        }
    }
}

/// Drop dangling index lines from the memory dir's `MEMORY.md` after a file is
/// deleted — a broken index quietly poisons future recall.
async fn prune_memory_index(
    files: &dyn crate::ports::outbound::WorkspaceFilesPort,
    dir: &std::path::Path,
    deleted: &str,
) {
    let idx = dir.join("MEMORY.md");
    let Some(cur) = files.read(&idx).await else {
        return;
    };
    let next: String = cur
        .lines()
        .filter(|l| !l.contains(deleted))
        .collect::<Vec<_>>()
        .join("\n");
    if next != cur {
        let _ = files.write(&idx, &(next + "\n")).await;
    }
}

/// Activity-log tail for a deploy that failed its health gate (COX-F005), so
/// the log line and notification carry the probe's HTTP status and response
/// time — not just "deploy failed" — and whoever reads the history can tell a
/// port that never opened from an app that answered 503.
fn describe_health_failure(result: &crate::state::HealthCheckResult) -> String {
    let status = result
        .http_status
        .map_or_else(|| "no response".to_owned(), |code| format!("HTTP {code}"));
    let timing = result
        .response_time_ms
        .map_or_else(String::new, |ms| format!(" after {ms}ms"));
    format!(
        "(containers started but the app never answered healthily on its port — \
         health check failed: {status}{timing})"
    )
}

/// First 7 chars of a commit sha, for compact activity/notification text.
fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

/// Seconds elapsed since an RFC3339 timestamp, or `None` if it can't be
/// parsed — the caller then treats the value conservatively (as unknown-age).
/// Public wrapper: the presentation layer needs the same age arithmetic for
/// the undo window.
#[must_use]
pub fn seconds_since_public(at: &str) -> Option<u64> {
    seconds_since(at)
}

pub(super) fn seconds_since(at: &str) -> Option<u64> {
    let then =
        time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).ok()?;
    let secs = (time::OffsetDateTime::now_utc() - then).whole_seconds();
    u64::try_from(secs).ok()
}

/// Committed git conflict markers in a PR diff — the tell-tale of a botched
/// "resolution". Only added (`+`) and context (` `) lines count; removed
/// (`-`) marker lines are the fix, not the disease.
pub(crate) fn diff_has_conflict_markers(diff: &str) -> bool {
    diff.lines().any(|l| {
        l.starts_with("+<<<<<<< ")
            || l.starts_with(" <<<<<<< ")
            || l.starts_with("+>>>>>>> ")
            || l.starts_with(" >>>>>>> ")
    })
}

/// Early-warning budget check (COX-F002): updates `state.budget_warned_*` for
/// this cycle and returns which cap(s), if any, just newly entered the
/// warning band. `over_*` is OR'd into the "approaching" check so a spend
/// jump that leaps straight past the warning band into the hard cap in one
/// cycle still raises the warning — it just never observed the narrower
/// "approaching but not yet over" window on its own. The `budget_warned_*`
/// flags dedupe repeats while spend stays in the band, and clear the moment
/// spend falls back out of it — whether because a human raised the cap, or
/// the cap was breached and the hard stop already took over — so a later
/// crossing can warn again.
fn apply_budget_warnings(
    state: &mut crate::state::ProjectState,
    warn_pct: f64,
    lifetime_cap: Option<f64>,
    daily_cap: Option<f64>,
    spent_today: f64,
    over_lifetime: bool,
    over_daily: bool,
) -> (bool, bool) {
    let approaching_lifetime =
        crate::policy::approaching_cap(state.spend.total_cost_usd, lifetime_cap, warn_pct)
            || over_lifetime;
    let new_lifetime_warning = approaching_lifetime && !state.budget_warned_lifetime;
    state.budget_warned_lifetime = approaching_lifetime;

    let approaching_daily =
        crate::policy::approaching_cap(spent_today, daily_cap, warn_pct) || over_daily;
    let new_daily_warning = approaching_daily && !state.budget_warned_daily;
    state.budget_warned_daily = approaching_daily;

    (new_lifetime_warning, new_daily_warning)
}

/// Advance `state.cycle` — the persistent, restart-safe project-cycle counter —
/// and return its new value. On a project whose scorecard already has history
/// (loaded from before `state.cycle` existed), the counter is first seeded past
/// that history so the scorecard dedupe gate (new cycle > scored max) doesn't
/// drop the first N real cycles, and so cadence never renumbers onto values
/// `sweeps_done` already recorded. Pure + testable; the leader calls it with
/// its loaded state and persists the result.
fn advance_project_cycle(state: &mut crate::state::ProjectState) -> u64 {
    if state.cycle == 0 {
        state.cycle = state
            .cycle_scores
            .iter()
            .map(|c| c.cycle)
            .max()
            .unwrap_or(0);
    }
    state.cycle = state.cycle.saturating_add(1);
    state.cycle
}

#[cfg(test)]
mod cycle_counter_tests {
    use super::advance_project_cycle;
    use crate::state::ProjectState;

    #[test]
    fn fresh_project_starts_and_advances_from_one() {
        let mut s = ProjectState::default();
        assert_eq!(advance_project_cycle(&mut s), 1);
        assert_eq!(advance_project_cycle(&mut s), 2);
        assert_eq!(advance_project_cycle(&mut s), 3);
    }

    #[test]
    fn resumes_from_an_existing_runner_local_counter() {
        // A project whose persistent counter was seeded by an older local
        // counter (e.g. it ran 42 cycles before this field existed).
        let mut s = ProjectState::default();
        s.cycle = 42;
        assert_eq!(advance_project_cycle(&mut s), 43);
        assert_eq!(advance_project_cycle(&mut s), 44);
    }

    #[test]
    fn seeds_past_existing_scorecard_history_on_first_use() {
        // Migrated project: cycle_scores already has history but `state.cycle`
        // is 0. The first advance must jump PAST the scored max so the dedupe
        // gate doesn't suppress real fresh cycles.
        use crate::state::CycleScore;
        let mut s = ProjectState::default();
        s.cycle_scores.push(CycleScore {
            cycle: 42,
            ..CycleScore::default()
        });
        assert_eq!(advance_project_cycle(&mut s), 43);
        // Subsequent advances stay monotonic.
        assert_eq!(advance_project_cycle(&mut s), 44);
    }

    #[test]
    fn never_regresses_the_persistent_counter() {
        // The counter only moves forward — it never wraps or renumbers.
        let mut s = ProjectState::default();
        s.cycle = u64::MAX - 1;
        assert_eq!(advance_project_cycle(&mut s), u64::MAX);
        // Saturates rather than wrapping to 0 (which would collide with cadence).
        assert_eq!(advance_project_cycle(&mut s), u64::MAX);
    }
}

#[cfg(test)]
mod marker_tests {
    use super::diff_has_conflict_markers;

    #[test]
    fn detects_committed_markers_only() {
        // Added marker = botched resolution.
        assert!(diff_has_conflict_markers(
            "+<<<<<<< HEAD\n+x\n+>>>>>>> main\n"
        ));
        // Context (already-committed) marker counts too.
        assert!(diff_has_conflict_markers(" <<<<<<< HEAD\n stuff\n"));
        // REMOVING markers is the fix — must not trip the gate.
        assert!(!diff_has_conflict_markers(
            "-<<<<<<< HEAD\n-old\n->>>>>>> main\n+resolved\n"
        ));
        // ======= alone is ambiguous (markdown underline) — not a trigger.
        assert!(!diff_has_conflict_markers("+=======\n+Title\n"));
        assert!(!diff_has_conflict_markers("+normal code line\n context\n"));
    }
}

#[cfg(test)]
mod version_reconcile_tests {
    use super::{parse_cargo_version, reconcile_target};
    use coxagent_domain::SemVer;

    #[test]
    fn parses_top_level_version() {
        let out = parse_cargo_version("[package]\nname = \"x\"\nversion = \"2.22.0\"\n");
        assert_eq!(out.as_deref(), Some("2.22.0"));
    }

    #[test]
    fn ignores_dependency_versions() {
        // A dependency `serde = { version = "1.x" }` is NOT at line start, so it
        // is skipped. Cargo's own `[package] version` is the intended target.
        let out = parse_cargo_version(
            "[package]\nname = \"x\"\nversion = \"2.22.0\"\n[dependencies]\nserde = { version = \"1.0.0\" }\n",
        );
        assert_eq!(out.as_deref(), Some("2.22.0"));
    }

    #[test]
    fn missing_or_garbage_version_is_none() {
        assert_eq!(parse_cargo_version("[package]\nname=\"x\"\n"), None);
        assert_eq!(parse_cargo_version("version = \"not-a-version\"\n"), None);
    }

    #[test]
    fn reconciles_only_when_state_is_ahead() {
        // State ahead of repo → reconcile DOWN to repo.
        assert_eq!(
            reconcile_target(&SemVer::new(2, 23, 0), "2.22.0"),
            Some(SemVer::new(2, 22, 0))
        );
        // State at or behind reality → no-op.
        assert_eq!(reconcile_target(&SemVer::new(2, 22, 0), "2.22.0"), None);
        assert_eq!(reconcile_target(&SemVer::new(2, 21, 0), "2.22.0"), None);
        // Unparseable repo version → no-op (cannot guess).
        assert_eq!(reconcile_target(&SemVer::new(2, 23, 0), "garbage"), None);
    }

    #[test]
    fn semver_ordering_is_not_lexicographic() {
        // 2.9.0 < 2.10.0 must hold so we never wrongly treat 2.9 as "ahead".
        assert!(SemVer::new(2, 9, 0) < SemVer::new(2, 10, 0));
    }
}

#[cfg(test)]
mod cycle_tests;
