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

mod ceremonies;
mod scrum;
mod wiring;
mod escalation;
mod forge;
mod ops;
mod qa_evidence;

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
    notifier: Option<Arc<dyn crate::ports::outbound::NotifierPort>>,
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
    /// This runner's identity (`account@host`) — recorded as the ticket claim
    /// owner so concurrent runners on a shared backlog never collide.
    worker: String,
    /// Last scrum discussion topic — skip duplicate discussions.
    last_discussion_topic: Mutex<String>,
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
            notifier: None,
            budget: None,
            git: None,
            files: None,
            janitor: None,
            forge: None,
            phase: None,
            worker: String::new(),
            last_discussion_topic: Mutex::new(String::new()),
            sandbox_warned: AtomicBool::new(false),
        }
    }

    /// Set this runner's identity (`account@host`), used as the ticket claim
    /// owner. Called by `run_forever` from the live operator each cycle.
    pub fn set_worker(&mut self, worker: impl Into<String>) {
        self.worker = worker.into();
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

    /// Whole-system architecture review: the SA examines the codebase against
    /// clean architecture / DDD / SOLID, coupling and module boundaries,
    /// monolith-vs-microservices fit, and horizontal scalability — then files
    /// concrete refactor chores and asks the PO to prioritise them.
    async fn architecture_audit(&self, sprint: u32) {
        self.report("SA", "architecture review");
        let uc = crate::use_cases::RunArchitectureAuditUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
            self.config.workflow.token_saver,
            self.config.workflow.language,
        )
        .with_files(self.files.clone());
        if let Err(e) = uc.execute(sprint).await {
            tracing::warn!("architecture review: {e}");
        }
    }

    /// Documentation review (same cadence as the architecture review): DOCS scans
    /// the Wiki for shipped work that has no page (or only a thin stub) and writes
    /// the missing documentation in full — so the knowledge base doesn't drift
    /// behind the code.
    async fn docs_audit(&self, sprint: u32) {
        self.report("DOCS", "documentation review");
        let uc = crate::use_cases::RunDocsAuditUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
            self.config.workflow.language,
        );
        if let Err(e) = uc.execute(sprint).await {
            tracing::warn!("documentation review: {e}");
        }
    }

    /// While refactoring, the SA rewrites one pending feature's technical design
    /// so it targets the new architecture instead of the old bad one (bounded to
    /// one per cycle; marked so it isn't redone).
    async fn realign_one_spec(&self, vi: bool) {
        use coxagent_domain::{Status, TicketType};
        let Ok(state) = self.store.load().await else {
            return;
        };
        let target = state
            .tickets
            .iter()
            .filter(|t| {
                t.ticket_type() == TicketType::Feature
                    && matches!(t.status(), Status::Pending | Status::Ready)
            })
            .find_map(|t| {
                let d = t.design().technical.as_ref()?;
                (!d.approach.contains("[realigned]"))
                    .then(|| (t.id().clone(), t.title().to_owned(), d.approach.clone()))
            });
        let Some((id, title, approach)) = target else {
            return;
        };
        self.report("SA", "realigning spec");
        let task = format!(
            "The team is in a REFACTOR SPRINT fixing the architecture. Update the technical design \
             for feature {id} ({title}) so it targets the NEW clean architecture, not the old one \
             it was written against. Current approach:\n{approach}\n\nRespond with ONLY JSON: \
             {{\"approach\": string, \"files\": [string], \"api_contract\": string, \
             \"data_changes\": string, \"test_plan\": string}}. Begin `approach` with \
             \"[realigned] \"."
        );
        let request = AgentRequest {
            role: coxagent_domain::Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
            escalation_level: 0,
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return;
        };
        if !outcome.succeeded() {
            return;
        }
        let (Some(start), Some(end)) = (outcome.stdout.find('{'), outcome.stdout.rfind('}')) else {
            return;
        };
        let Ok(design) =
            serde_json::from_str::<coxagent_domain::TechnicalDesign>(&outcome.stdout[start..=end])
        else {
            return;
        };
        if let Ok(mut s) = self.store.load().await {
            if let Some(t) = s.ticket_mut(&id) {
                if t.set_technical_design(coxagent_domain::Role::Sa, design)
                    .is_ok()
                {
                    let msg = if vi {
                        format!("🧭 SA đã cập nhật lại technical spec cho {id} theo kiến trúc mới.")
                    } else {
                        format!(
                            "🧭 SA realigned the technical spec for {id} to the new architecture."
                        )
                    };
                    s.post_comment("SA", &msg, Some(id.to_string()));
                    let _ = self.store.save(&s).await;
                }
            }
        }
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
        // Merge-queue recovery flag (set by the leader once the queue blows up).
        let mut recovery = false;
        // Human-queued execution jobs (force-merge …) run before anything else.
        self.drain_jobs().await;
        // Announce presence in the shared registry so every dashboard (even on
        // another machine) can list this team as online.
        let _ = self
            .store
            .heartbeat_worker(&me, if leader { "leader" } else { "worker" }, "", &now)
            .await;

        // Keep the code map fresh so `.coxagent/REPO_MAP.md` reflects the tree
        // the agents are about to work on (best-effort, token-saver-gated).
        // Leader-only: it writes shared files under the repo.
        if leader {
            self.refresh_codegraph(cycle).await;
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
            // Mid-sprint backlog grooming, offset from the standup so the two
            // ceremonies don't land in the same cycle.
            if self.config.workflow.mode == crate::config::Mode::Scrum && cycle % 3 == 2 {
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
                        Err(e) => report.errors.push(format!("DEPLOY: {e}")),
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

                // Governance: architecture-conformance drift becomes tracked bugs.
                match self.conformance().execute().await {
                    Ok(mut ids) => report.bugs_filed.append(&mut ids),
                    Err(e) => report.errors.push(format!("CONFORMANCE: {e}")),
                }
            }

            // Auto-merge: SA deep-dives open PRs and merges or requests changes.
            self.report("SA", "reviewing PRs");
            self.review_open_prs().await;
            // Close the loop: when a human (or the SA) requested changes on a
            // PR, a DEV agent addresses the feedback and pushes to the branch.
            self.address_pr_feedback().await;

            // Scrum comes alive: when there's a real tension (deploy failure, a
            // bug pile-up, or a periodic check-in), the team actually discusses
            // it — PO & SA weigh in, SM decides, and a decision can spawn a
            // ticket. Posts land in the Scrum feed.
            self.scrum_discussion(&report, cycle).await;
        }
        self.report_idle();

        report.over_budget = self.record_activity(&report).await;
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
            if let Ok(mut s) = self.store.load().await {
                s.post_comment("SM", msg, None);
                s.log_activity("SM", "paused — engines out of quota", None);
                let _ = self.store.save(&s).await;
            }
            self.notify("quota_exhausted", msg.to_owned()).await;
        }
        report
    }

    /// Reject duplicate tickets (same normalised title, or a semantic near-match)
    /// that haven't started real work yet, keeping the earliest, and call it out
    /// on the thread — so a blind re-proposal never gets designed or built twice.
    async fn dedup_backlog(&self) {
        use coxagent_domain::ticket::Status;
        let Ok(mut state) = self.store.load().await else {
            return;
        };
        // Kept tickets carry their title token-set so we can flag not only exact
        // re-titles but semantic near-duplicates (paraphrases) too.
        let mut first: std::collections::HashMap<String, TicketId> =
            std::collections::HashMap::new();
        let mut kept: Vec<(TicketId, std::collections::HashSet<String>)> = Vec::new();
        let mut dupes: Vec<(TicketId, TicketId, String)> = Vec::new();
        for t in &state.tickets {
            if t.status() == Status::Rejected {
                continue;
            }
            let key = crate::parsing::normalize_title(t.title());
            if key.is_empty() {
                continue;
            }
            if let Some(orig) = first.get(&key) {
                dupes.push((t.id().clone(), orig.clone(), t.title().to_owned()));
                continue;
            }
            // Semantic pass: reject when the title overlaps an earlier ticket's
            // heavily (Jaccard ≥ 0.6 on content tokens), e.g. "Disappearing
            // Messages" vs "Auto-deleting messages".
            let tokens = crate::parsing::title_tokens(t.title());
            if let Some((orig, _)) = kept
                .iter()
                .find(|(_, seen)| crate::parsing::jaccard(&tokens, seen) >= 0.6)
            {
                dupes.push((t.id().clone(), orig.clone(), t.title().to_owned()));
                continue;
            }
            first.insert(key, t.id().clone());
            kept.push((t.id().clone(), tokens));
        }
        let mut rejected = Vec::new();
        for (dup, orig, title) in dupes {
            if let Some(t) = state.ticket_mut(&dup) {
                // Only reject work that hasn't been picked up yet.
                if matches!(t.status(), Status::Pending | Status::Ready | Status::Open)
                    && t.transition_to(coxagent_domain::Role::User, Status::Rejected)
                        .is_ok()
                {
                    rejected.push((dup, orig, title));
                }
            }
        }
        if rejected.is_empty() {
            return;
        }
        let vi = self.config.workflow.language.is_vi();
        for (dup, orig, title) in &rejected {
            let msg = if vi {
                format!(
                    "Heads up — {dup} trùng với {orig} (\"{title}\"). Từ chối {dup} để khỏi làm \
                     trùng. BA nhớ kiểm tra backlog trước khi đề xuất."
                )
            } else {
                format!(
                    "Heads up — {dup} duplicates {orig} (\"{title}\"). Rejecting {dup} so we don't \
                     build the same thing twice. BA, please check the backlog before proposing."
                )
            };
            state.post_comment("SM", &msg, Some(dup.to_string()));
        }
        state.log_activity(
            "SM",
            &format!("rejected {} duplicate ticket(s)", rejected.len()),
            None,
        );
        let _ = self.store.save(&state).await;
    }

    /// Post the daily digest into the team chat, at most once per UTC day (the
    /// marker lives in state, so restarts and multiple operators can't repeat
    /// it). The very first run only stamps the day — no digest of nothing.
    /// Append a promoted team lesson to the repo's `CLAUDE.md` under a
    /// dedicated section — versioned via git, read by the engine on EVERY
    /// machine. Dedupes on exact text. Returns whether anything was written.
    async fn promote_team_note(&self, note: &str) -> bool {
        use std::fmt::Write as _;
        const HEADER: &str = "## Team learnings (auto-promoted by memory hygiene)";
        let Some(files) = &self.files else {
            return false;
        };
        let path = self.work_dir.join("CLAUDE.md");
        let cur = files.read(&path).await.unwrap_or_default();
        if cur.contains(note) {
            return false;
        }
        let mut next = cur.clone();
        if !next.contains(HEADER) {
            if !next.is_empty() && !next.ends_with('\n') {
                next.push('\n');
            }
            next.push('\n');
            next.push_str(HEADER);
            next.push('\n');
        }
        let _ = writeln!(next, "- {note}");
        files.write(&path, &next).await
    }

    /// Where the claude CLI keeps its per-project auto-memory for this
    /// codebase: `~/.claude/projects/<work_dir with '/'→'-'>/memory`.
    fn engine_memory_dir(&self) -> Option<std::path::PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let slug = self.work_dir.to_string_lossy().replace('/', "-");
        let dir = std::path::Path::new(&home)
            .join(".claude/projects")
            .join(slug)
            .join("memory");
        dir.is_dir().then_some(dir)
    }

    /// File the periodic tech-debt chore (deduped by title prefix).
    async fn file_debt_sweep(&self, cycle: u64) {
        use coxagent_domain::ticket::Status;
        let Ok(state) = self.store.load().await else {
            return;
        };
        let open_exists = state.tickets.iter().any(|t| {
            t.title().starts_with("Debt sweep")
                && !matches!(
                    t.status(),
                    Status::Done | Status::Documented | Status::Verified | Status::Rejected
                )
        });
        if open_exists {
            return;
        }
        let lint_note = match &self.deploy {
            Some(d) => match d.lint(&self.work_dir).await {
                Ok(Some(n)) if n > 0 => format!(" Current clippy baseline: {n} errors."),
                _ => String::new(),
            },
            None => String::new(),
        };
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        if let Ok(id) = adder
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: coxagent_domain::TicketType::Chore,
                title: format!("Debt sweep (cycle {cycle})"),
                description: format!(
                    "Scheduled tech-debt pass — no new features. Pick the highest-leverage \
                     debt and pay it down: reduce the lint/clippy baseline, delete dead code, \
                     fill missing module docs, strengthen the weakest test area.{lint_note}"
                ),
                priority: coxagent_domain::ticket::Priority::Medium,
                complexity: coxagent_domain::ticket::Complexity::Medium,
                has_ui: false,
                acceptance_criteria: vec![
                    "The clippy/lint baseline is LOWER than before this ticket".to_owned(),
                    "No behavior change: full test suite still green".to_owned(),
                ],
            })
            .await
        {
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                s.log_activity("SM", "filed scheduled debt sweep", Some(id.to_string()));
                Ok(())
            })
            .await;
        }
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

    /// Recovery trips when the queue is at 2× the WIP limit (min 8) — past that
    /// point normal cycles can never drain it, so the team must switch to
    /// merge-only work.
    fn recovery_threshold(&self) -> usize {
        (self.config.git.max_open_prs as usize * 2).max(8)
    }

    /// Merge-queue RECOVERY: entered automatically when the queue blows past
    /// [`Self::recovery_threshold`]. While active, cycles do merge/conflict work
    /// ONLY — no BA proposals, no TEST bug-filing (they just re-discover bugs
    /// whose fixes are stuck in the queue), no new branches. On entry: announce
    /// in #agents, reset the per-PR fix-attempt brakes so parked PRs get retried,
    /// and close obsolete "Resolve merge conflict on PR #N" resolver-PRs (that
    /// anti-pattern is exactly what piled the queue up). Exits, with an
    /// announcement, once the queue is back under the WIP limit.
    async fn run_queue_recovery(&self, open: Option<usize>) -> bool {
        let Some(open) = open else { return false };
        let limit = self.config.git.max_open_prs as usize;
        let vi = self.config.workflow.language.is_vi();
        if open < self.recovery_threshold() {
            // Below the trip point. If we were recovering and are now under the
            // WIP limit, declare recovery over.
            if open <= limit {
                let msg = if vi {
                    format!("✅ Queue đã hồi phục — còn {open} PR mở (limit {limit}). Team quay lại làm việc bình thường.")
                } else {
                    format!("✅ Merge queue recovered — {open} open PR(s) (limit {limit}). Back to normal work.")
                };
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    if s.queue_recovery {
                        s.queue_recovery = false;
                        s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                    }
                    Ok(())
                })
                .await;
            }
            return false;
        }
        let msg = if vi {
            format!(
                "🚨 RECOVERY MODE: {open} PR đang mở (ngưỡng {}). Từ giờ mỗi cycle chỉ merge + gỡ \
                 conflict — không code mới, không file bug mới (bug cũ chưa merge thì test lại chỉ \
                 đẻ trùng). Đóng các PR 'resolve conflict' mồ côi. Queue về dưới {limit} là team \
                 chạy lại bình thường.",
                self.recovery_threshold()
            )
        } else {
            format!(
                "🚨 RECOVERY MODE: {open} open PRs (threshold {}). Cycles now do merge/conflict \
                 work only — no new code, no new bug filing. Obsolete resolver-PRs get closed. \
                 Normal work resumes under {limit} open PRs.",
                self.recovery_threshold()
            )
        };
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if !s.queue_recovery {
                s.queue_recovery = true;
                // Give every parked PR another shot under the new regime.
                s.pr_fix_attempts.clear();
                s.post_comment("SM", &msg, None);
                s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            }
            Ok(())
        })
        .await;
        self.report("SM", &format!("recovery: draining {open} open PRs"));
        // SM highlights the drain status EVERY recovery cycle — the team (and
        // any human watching chat) always knows how many conflicts stand
        // between them and new feature work.
        if let Some(forge) = &self.forge {
            if let Ok(prs) = forge.list_open_prs().await {
                let conflicted = prs.iter().filter(|p| !p.mergeable).count();
                let status = if vi {
                    format!(
                        "🔧 Recovery: còn {open} PR mở, {conflicted} dính conflict. Luật: xử hết \
                         conflict TRƯỚC rồi mới design/feature mới — DEV fix 8 conflict/cycle \
                         (cũ nhất trước), SA re-review và merge ngay khi xanh."
                    )
                } else {
                    format!(
                        "🔧 Recovery: {open} open PRs, {conflicted} conflicting. Law: conflicts \
                         are cleared BEFORE any new design/feature work — DEV fixes 8 per cycle \
                         (oldest first), SA re-reviews and merges as soon as they're green."
                    )
                };
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    if s.queue_recovery {
                        s.post_chat_in("SM", &status, crate::state::AGENTS_CHANNEL, Vec::new());
                    }
                    Ok(())
                })
                .await;
            }
        }
        // Resolver-PRs ("Resolve merge conflict on PR #N") are the anti-pattern
        // that inflated the queue — conflicts are fixed on the ORIGINAL branch by
        // address_pr_feedback, so these are pure noise. Close them.
        if let Some(forge) = &self.forge {
            if let Ok(prs) = forge.list_open_prs().await {
                for p in prs
                    .iter()
                    .filter(|p| p.title.contains("Resolve merge conflict on PR #"))
                {
                    if forge.close_pr(p.number).await.is_ok() {
                        self.log_git(&format!(
                            "recovery: closed obsolete resolver PR #{} ({})",
                            p.number, p.title
                        ))
                        .await;
                    }
                }
            }
        }
        true
    }

    /// A restructure is planned or underway: architecture refactor mode is on,
    /// or the PO's sprint goal reads like a refactor/migration.
    async fn clean_base_required(&self) -> bool {
        let Ok(state) = self.store.load().await else {
            return false;
        };
        if state.refactor_mode {
            return true;
        }
        let goal = state
            .sprint
            .as_ref()
            .map(|s| s.goal.to_lowercase())
            .unwrap_or_default();
        [
            "refactor",
            "restructure",
            "migrat",
            "tái cấu trúc",
            "cấu trúc lại",
        ]
        .iter()
        .any(|k| goal.contains(k))
    }

    /// Ops/SRE monitor: once the app has been deployed, ping its published port
    /// each leader cycle. On an outage, file exactly one high-priority bug and
    /// alert the chat; on recovery, announce it. State-tracked so it never spams.
    async fn ops_monitor(&self) {
        use coxagent_domain::ticket::{Complexity, Priority, TicketType};
        if !self.config.workflow.ops_monitor {
            return;
        }
        let Some(port) = self.config.deploy.host_port else {
            return;
        };
        let Some(deploy) = &self.deploy else {
            return;
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        // Only meaningful once something has actually been deployed.
        if state.history.is_empty() {
            return;
        }
        let was_down = state.ops_down;
        let healthy = deploy.health(port).await.unwrap_or(true);
        if !healthy && !was_down {
            let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
            let _ = adder
                .execute(crate::use_cases::AddTicketInput {
                    ticket_type: TicketType::Bug,
                    title: format!("App is DOWN — no response on port {port}"),
                    description: "The Ops monitor found the deployed app not accepting \
                                  connections. Check the container/logs for a crash and restore \
                                  service."
                        .to_owned(),
                    priority: Priority::High,
                    complexity: Complexity::Medium,
                    has_ui: false,
                    acceptance_criteria: vec![format!("App answers on 127.0.0.1:{port} again")],
                })
                .await;
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.ops_down = true;
                Ok(())
            })
            .await;
            self.notify(
                "ops_down",
                format!(
                    "App is DOWN — nothing responding on port {port}. Filed a high-priority bug."
                ),
            )
            .await;
        } else if healthy && was_down {
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.ops_down = false;
                Ok(())
            })
            .await;
            self.notify(
                "ops_up",
                format!("App recovered — responding on port {port} again."),
            )
            .await;
        }
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
fn seconds_since(at: &str) -> Option<u64> {
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
mod cycle_tests;
