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
        let root = self.work_dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let g = crate::codegraph::CodeGraph::index(&root);
            // save() also writes REPO_MAP.md — one producer, both artifacts.
            let _ = g.save(&root);
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
        );
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
        crate::cleanup::kill_orphaned_drivers(&self.work_dir);
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

    /// Whether the board has any recent team activity to hold a standup over —
    /// the gate that stops empty, token-wasting standups on a quiet board.
    async fn has_recent_activity(&self) -> bool {
        self.store
            .load()
            .await
            .is_ok_and(|s| !s.activity.is_empty())
    }

    /// Pick the most pressing thing worth a team discussion this cycle, or `None`
    /// when there's nothing to talk about (so the team isn't noisy for no reason).
    fn scrum_topic(
        state: &crate::state::ProjectState,
        report: &CycleReport,
        cycle: u64,
        lang: crate::config::Language,
    ) -> Option<String> {
        use coxagent_domain::{Status, TicketType};
        let vi = lang.is_vi();
        // A failed deploy is the loudest signal — discuss root cause + prevention.
        if report.errors.iter().any(|e| e.contains("DEPLOY")) || !report.bugs_filed.is_empty() {
            return Some(
                if vi {
                    "Lần deploy hoặc chạy test gần nhất phát sinh lỗi. Nguyên nhân gốc có thể là gì, \
                     và ta nên thay đổi gì để nó không tái diễn?"
                } else {
                    "The last deploy or test run surfaced failures. What's the likely root cause, \
                     and what should we change to stop it recurring?"
                }
                .to_owned(),
            );
        }
        let open_bugs = state
            .tickets
            .iter()
            .filter(|t| t.ticket_type() == TicketType::Bug && t.status() == Status::Open)
            .count();
        if open_bugs >= 3 {
            return Some(if vi {
                format!(
                    "Đang có {open_bugs} bug mở. Nên tạm dừng tính năng mới để dọn hết bug trước, \
                     hay tiếp tục ship? Quyết định đi, và nếu cần thì tạo ticket theo dõi."
                )
            } else {
                format!(
                    "We have {open_bugs} open bugs. Should we pause new features and burn down the \
                     bug backlog first, or keep shipping? Decide and, if useful, create a tracking ticket."
                )
            });
        }
        // A stalled in-progress ticket is worth flagging as a possible blocker.
        if let Some(t) = state
            .tickets
            .iter()
            .find(|t| t.status() == Status::InProgress)
        {
            if cycle % 4 == 0 {
                return Some(if vi {
                    format!(
                        "{} đã ở trạng thái đang làm khá lâu. Có bị block hay quá lớn không? \
                         Nên tách nhỏ hay gỡ block cho nó?",
                        t.id()
                    )
                } else {
                    format!(
                        "{} has been in progress for a while. Is it blocked or too big? \
                         Should we split it or unblock it?",
                        t.id()
                    )
                });
            }
        }
        // Otherwise a light periodic check-in keeps the sprint honest.
        if cycle % 6 == 0 {
            return Some(
                if vi {
                    "Điểm tin sprint: có đang đúng hướng với mục tiêu sprint không? Có rủi ro, phình \
                     phạm vi, hay blocker nào cần nêu? Chốt một bước tiếp theo cụ thể."
                } else {
                    "Sprint check-in: are we on track for the sprint goal? Any risks, scope creep, \
                     or blockers to raise? Decide on one concrete next step."
                }
                .to_owned(),
            );
        }
        None
    }

    /// Clarification loop: if the next ready feature has no acceptance criteria,
    /// DEV-FEATURE "raises it" on the ticket thread and the BA jumps in to pin
    /// down the definition of done — so nobody builds against a fuzzy spec.
    /// Best-effort, at most one clarification per cycle.
    async fn clarify_next_feature(&self) {
        use coxagent_domain::ticket::{Status, TicketType};
        let Ok(state) = self.store.load().await else {
            return;
        };
        let Some((id, title)) = state
            .tickets
            .iter()
            .find(|t| {
                t.ticket_type() == TicketType::Feature
                    && t.status() == Status::Ready
                    && t.acceptance_criteria().is_empty()
            })
            .map(|t| (t.id().clone(), t.title().to_owned()))
        else {
            return;
        };

        // DEV flags it — in a human tone — on the ticket thread.
        if let Ok(mut s) = self.store.load().await {
            s.post_comment(
                "DEV-FEATURE",
                &format!(
                    "Hold on — {id} has no acceptance criteria. I'm not going to guess what \
                     \"done\" means and risk building the wrong thing. BA, can you pin it down?"
                ),
                Some(id.to_string()),
            );
            let _ = self.store.save(&s).await;
        }

        // BA answers by generating concrete criteria.
        let request = AgentRequest {
            role: coxagent_domain::Role::Ba,
            system_prompt: crate::prompts::system_prompt(crate::prompts::BA),
            task_prompt: format!(
                "A developer flagged that ticket {id} (\"{title}\") has no acceptance criteria \
                 and won't start without them. Write 2-5 concrete, testable acceptance criteria \
                 (user-visible behaviour, not implementation). Respond with ONLY a JSON array of \
                 strings."
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(300),
            escalation_level: 0,
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return;
        };
        if !outcome.succeeded() {
            return;
        }
        let Ok(criteria) = crate::parsing::parse_string_list(&outcome.stdout) else {
            return;
        };
        let criteria: Vec<String> = criteria.into_iter().take(5).collect();
        if criteria.is_empty() {
            return;
        }
        if let Ok(mut s) = self.store.load().await {
            if let Some(t) = s.ticket_mut(&id) {
                t.set_acceptance_criteria(criteria.clone());
            }
            s.post_comment(
                "BA",
                &format!(
                    "Good catch — my bad for leaving {id} fuzzy. Definition of done: {}. \
                     Updated the ticket, you're clear to build.",
                    criteria.join("; ")
                ),
                Some(id.to_string()),
            );
            s.log_activity("BA", "clarified acceptance criteria", Some(id.to_string()));
            let _ = self.store.save(&s).await;
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
                for (role, n) in std::mem::take(&mut m.runs_by_role) {
                    *state.spend.runs_by_role.entry(role).or_default() += n;
                }
                for (role, cost) in std::mem::take(&mut m.metered_cost_by_role) {
                    *state.spend.metered_cost_by_role.entry(role).or_default() += cost;
                }
                // Attribute this cycle's spend to the operator that ran it, so
                // each user's token usage is measurable in a shared project.
                if !self.worker.is_empty() {
                    let op = state
                        .spend
                        .by_operator
                        .entry(self.worker.clone())
                        .or_default();
                    op.cost_usd += m.total_cost_usd;
                    op.input_tokens += m.input_tokens;
                    op.output_tokens += m.output_tokens;
                    op.runs += m.runs;
                }
                *m = Spend::default();
            }
        }
        let spent_today = state.add_daily_spend(cycle_cost);

        // Effective caps: the live cell (adjustable without restart) when present,
        // otherwise the caps from the loaded config.
        let (lifetime_cap, daily_cap) = self.budget.as_ref().map_or_else(
            || {
                (
                    self.config.workflow.budget_usd,
                    self.config.policy.daily_budget_usd,
                )
            },
            |b| {
                b.lock().map_or(
                    (
                        self.config.workflow.budget_usd,
                        self.config.policy.daily_budget_usd,
                    ),
                    |caps| (caps.lifetime_usd, caps.daily_usd),
                )
            },
        );
        // Pause on either the lifetime cap or the per-day cap.
        let over_lifetime =
            lifetime_cap.is_some_and(|cap| cap > 0.0 && state.spend.total_cost_usd >= cap);
        let over_daily = daily_cap.is_some_and(|cap| cap > 0.0 && spent_today >= cap);
        if over_daily {
            state.log_activity("POLICY", "daily budget cap reached", None);
        }

        let warn_pct = self.config.policy.budget_warn_pct;
        let (new_lifetime_warning, new_daily_warning) = apply_budget_warnings(
            &mut state,
            warn_pct,
            lifetime_cap,
            daily_cap,
            spent_today,
            over_lifetime,
            over_daily,
        );

        let _ = self.store.save(&state).await;
        self.notify_budget_warnings(new_lifetime_warning, new_daily_warning, warn_pct)
            .await;

        over_lifetime || over_daily
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
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_context(Some(self.context.clone()))
    }

    fn milestones(&self) -> crate::use_cases::RunMilestonesUseCase<S, E> {
        crate::use_cases::RunMilestonesUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
            self.context.clone(),
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
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_context(Some(self.context.clone()))
    }

    fn dev(&self, mode: DevMode) -> RunDevUseCase<S, E> {
        RunDevUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
            mode,
        )
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_verify(self.deploy.clone())
        .with_git(self.git.clone())
        .with_context(Some(self.context.clone()))
    }

    fn test(&self) -> RunTestUseCase<S, E> {
        RunTestUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
        .with_context(Some(self.context.clone()))
    }

    fn docs(&self) -> RunDocsUseCase<S, E> {
        RunDocsUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.config.clone(),
            self.work_dir.clone(),
        )
        .with_worker(self.worker.clone())
        .with_phase(self.phase.clone())
        .with_git(self.git.clone())
        .with_context(Some(self.context.clone()))
    }

    fn conformance(&self) -> RunConformanceUseCase<S> {
        RunConformanceUseCase::new(
            Arc::clone(&self.store),
            self.work_dir.clone(),
            self.config.architecture.clone(),
        )
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
mod tests {
    use super::*;
    use crate::ports::outbound::{AgentOutcome, AgentRequest};
    use crate::ports::outbound::{GitAuthor, SandboxStatus};
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

        /// Simulates what a REAL engine on this host would report — the same
        /// bwrap-presence probe the platform tests below use — so tests that
        /// enable `workflow.sandbox` exercise the actual unsupported-platform
        /// path instead of a hardcoded stub value.
        fn sandbox_status(&self) -> crate::ports::outbound::SandboxStatus {
            use crate::ports::outbound::SandboxStatus;
            #[cfg(target_os = "macos")]
            {
                SandboxStatus::Confined("seatbelt")
            }
            #[cfg(target_os = "linux")]
            {
                let has_bwrap = std::process::Command::new("bwrap")
                    .arg("--version")
                    .output()
                    .is_ok();
                if has_bwrap {
                    SandboxStatus::Confined("bwrap")
                } else {
                    SandboxStatus::Unavailable("bwrap not found on PATH")
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                SandboxStatus::Unavailable("sandboxing not supported on this platform")
            }
        }

        async fn run(&self, r: AgentRequest) -> Result<AgentOutcome, PortError> {
            let stdout = if r.system_prompt.contains("Business Analyst") {
                r#"[{"title":"Feature A","priority":"high","complexity":"small","has_ui":false}]"#
                    .to_owned()
            } else if r.system_prompt.contains("Solution Architect") {
                r#"{"approach":"a","files":["a.rs"],"api_contract":"","data_changes":"","test_plan":"t","ux":null}"#
                    .to_owned()
            } else if r.system_prompt.contains("Product Owner") {
                r#"[{"name":"MVP","goal":"ship it","target_version":"1.0.0"}]"#.to_owned()
            } else if r.system_prompt.contains("QA Engineer") {
                "[]".to_owned()
            } else if r.system_prompt.contains("Tech Writer") {
                // Must satisfy the DOCS structure gate, like a real run.
                "FOLDER: -\n# Feature A\n**Keywords:** feature, a, flow\n## Overview\nIt does \
                 the thing the ticket asked for, end to end, for the people who need it. The \
                 flow starts at the API, runs through the use case, and lands in the store, so \
                 a reader can follow one request from edge to persistence without guessing. \
                 Failures surface as errors rather than silent no-ops.\n\
                 ## How it works\nrun_feature() drives the flow: it validates the request, \
                 claims the work, and writes the result once.\n\
                 ## Usage\nCall it from the API or the cycle; both paths are the same code.\n\
                 ## Interface\nPOST /api/a\n## Configuration\nnone\n\
                 ## Edge cases and limits\nFails closed: an invalid request is rejected \
                 before any state is written, and a partial write is never observable.\n\
                 ## Code map\n- src/a.rs — the flow, from request validation to the store\n\
                 ## Related\nnone\n"
                    .to_owned()
            } else {
                "done".to_owned()
            };
            Ok(AgentOutcome {
                stdout,
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
            })
        }
    }

    #[tokio::test]
    async fn one_cycle_carries_a_feature_from_proposal_to_done() {
        // DOCS validates its Code map against the tree, so the workspace must
        // contain the file the scripted page cites.
        let dir = std::env::temp_dir().join(format!("cyclerun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::write(dir.join("src/a.rs"), "// the flow\n").expect("write");
        let store = Arc::new(MemStore::default());
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            dir.clone(),
            "goal".to_owned(),
        );
        // Cycle 1: BA proposes → SA designs → DEV-FEATURE implements → TEST clean.
        let report = Box::pin(uc.run_cycle(1)).await;
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
        let _ = std::fs::remove_dir_all(&dir);
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
        let mut cfg = Config::default();
        cfg.deploy.enabled = true;
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&spy) as Arc<dyn crate::ports::outbound::DeployPort>);
        Box::pin(uc.run_cycle(1)).await;
        assert_eq!(spy.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Records commit_all calls so we can assert the cycle commits completed work.
    #[derive(Default)]
    struct SpyGit {
        commits: Mutex<Vec<(String, String)>>, // (message, author_email)
    }
    #[async_trait::async_trait]
    impl GitPort for SpyGit {
        async fn is_repo(&self, _: &std::path::Path) -> bool {
            true
        }
        async fn current_branch(&self, _: &std::path::Path) -> Result<String, PortError> {
            Ok("main".to_owned())
        }
        async fn checkout_branch(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
            Ok(())
        }
        async fn commit_all(
            &self,
            _: &std::path::Path,
            message: &str,
            author: &GitAuthor,
        ) -> Result<Option<String>, PortError> {
            self.commits
                .lock()
                .expect("lock")
                .push((message.to_owned(), author.email.clone()));
            Ok(Some("abc1234".to_owned()))
        }
        async fn push(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
            Ok(())
        }
        async fn sync_base(
            &self,
            _: &std::path::Path,
            _: &str,
        ) -> Result<crate::ports::outbound::SyncBase, PortError> {
            Ok(crate::ports::outbound::SyncBase::UpToDate)
        }
        async fn abort_merge(&self, _: &std::path::Path) -> Result<(), PortError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn commits_completed_feature_only_when_git_enabled() {
        // git disabled (default) → no commit.
        let store = Arc::new(MemStore::default());
        let spy = Arc::new(SpyGit::default());
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_git(Arc::clone(&spy) as Arc<dyn GitPort>);
        Box::pin(uc.run_cycle(1)).await;
        assert!(
            spy.commits.lock().expect("lock").is_empty(),
            "no commit when git.enabled is false"
        );

        // git enabled + a custom commit email → one conventional commit.
        let store = Arc::new(MemStore::default());
        let spy = Arc::new(SpyGit::default());
        let mut cfg = Config::default();
        cfg.git.enabled = true;
        cfg.git.commit_email = "5204779+bot@users.noreply.github.com".to_owned();
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_git(Arc::clone(&spy) as Arc<dyn GitPort>);
        Box::pin(uc.run_cycle(1)).await;

        let commits = spy.commits.lock().expect("lock");
        assert_eq!(commits.len(), 1, "one commit for the completed feature");
        assert!(
            commits[0].0.starts_with("feat(") && commits[0].0.contains("Feature A"),
            "conventional message with ticket + title: {}",
            commits[0].0
        );
        assert_eq!(
            commits[0].1, "5204779+bot@users.noreply.github.com",
            "commits under the configured noreply email"
        );
    }

    use crate::ports::outbound::{ForgePort, PullRequest};

    /// Forge with one open PR that records merge / request-changes calls.
    #[derive(Default)]
    struct SpyForge {
        ci: String,
        mergeable: bool,
        merged: Mutex<Vec<u64>>,
        changes: Mutex<Vec<u64>>,
    }
    #[async_trait::async_trait]
    impl ForgePort for SpyForge {
        async fn open_pr(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
        ) -> Result<PullRequest, PortError> {
            unimplemented!()
        }
        async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
            Ok(vec![PullRequest {
                number: 7,
                title: "feat(X-1): add a".to_owned(),
                head: "feat/X-1".to_owned(),
                base: "main".to_owned(),
                url: String::new(),
                author: "coxagent-bot".to_owned(),
                ci: self.ci.clone(),
                mergeable: self.mergeable,
                created: String::new(),
            }])
        }
        async fn pr_diff(&self, _: u64) -> Result<String, PortError> {
            Ok("+ added a line".to_owned())
        }
        async fn merge_pr(&self, n: u64) -> Result<(), PortError> {
            self.merged.lock().expect("lock").push(n);
            Ok(())
        }
        async fn request_changes(&self, n: u64, _: &str) -> Result<(), PortError> {
            self.changes.lock().expect("lock").push(n);
            Ok(())
        }
        async fn close_pr(&self, _: u64) -> Result<(), PortError> {
            Ok(())
        }
    }

    /// Engine whose SA review verdict is fixed to `decision`.
    struct ReviewEngine {
        decision: &'static str,
    }
    #[async_trait::async_trait]
    impl AgentEnginePort for ReviewEngine {
        fn id(&self) -> &'static str {
            "review"
        }
        async fn run(&self, _: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: format!("{{\"decision\":\"{}\",\"summary\":\"s\"}}", self.decision),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
            })
        }
    }

    fn review_uc(
        forge: Arc<SpyForge>,
        decision: &'static str,
        auto_merge: bool,
    ) -> RunCycleUseCase<MemStore, ReviewEngine> {
        let mut cfg = Config::default();
        cfg.git.enabled = true;
        cfg.git.auto_merge = auto_merge;
        RunCycleUseCase::new(
            Arc::new(MemStore::default()),
            Arc::new(ReviewEngine { decision }),
            cfg,
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_forge(forge as Arc<dyn ForgePort>)
    }

    #[tokio::test]
    async fn a_project_with_nothing_to_verify_with_still_merges() {
        // Auto-merge demands the merged tree build and test, but a project with
        // no git and no runner wired has nothing to check — blocking every
        // merge forever would be the wrong answer to a missing capability.
        let forge = Arc::new(SpyForge {
            ci: "passing".to_owned(),
            mergeable: true,
            ..Default::default()
        });
        review_uc(Arc::clone(&forge), "approve", true)
            .review_open_prs()
            .await;
        assert_eq!(*forge.merged.lock().expect("lock"), vec![7]);
    }

    #[tokio::test]
    async fn require_ci_off_reviews_and_merges_despite_failing_ci() {
        // CI unavailable (e.g. Actions billing dead) + require_ci off: the SA
        // judges the diff on its own and an approve still merges.
        let forge = Arc::new(SpyForge {
            ci: "failing".to_owned(),
            mergeable: true,
            ..Default::default()
        });
        let mut cfg = Config::default();
        cfg.git.enabled = true;
        cfg.git.auto_merge = true;
        cfg.git.require_ci = false;
        RunCycleUseCase::new(
            Arc::new(MemStore::default()),
            Arc::new(ReviewEngine {
                decision: "approve",
            }),
            cfg,
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_forge(Arc::clone(&forge) as Arc<dyn ForgePort>)
        .review_open_prs()
        .await;
        assert_eq!(*forge.merged.lock().expect("lock"), vec![7]);
        assert!(forge.changes.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn sa_merges_on_approve_but_only_when_auto_merge() {
        // auto_merge off → never merges even with an approve verdict.
        let forge = Arc::new(SpyForge {
            ci: "passing".to_owned(),
            mergeable: true,
            ..Default::default()
        });
        review_uc(Arc::clone(&forge), "approve", false)
            .review_open_prs()
            .await;
        assert!(forge.merged.lock().expect("lock").is_empty());

        // auto_merge on + approve + CI passing → merges PR #7.
        let forge = Arc::new(SpyForge {
            ci: "passing".to_owned(),
            mergeable: true,
            ..Default::default()
        });
        review_uc(Arc::clone(&forge), "approve", true)
            .review_open_prs()
            .await;
        assert_eq!(*forge.merged.lock().expect("lock"), vec![7]);
    }

    #[tokio::test]
    async fn auto_merge_only_touches_prs_into_the_target_branch() {
        // PR targets `main`, but the flow target is `develop` → left alone.
        let forge = Arc::new(SpyForge {
            ci: "passing".to_owned(),
            mergeable: true,
            ..Default::default()
        });
        let mut cfg = Config::default();
        cfg.git.enabled = true;
        cfg.git.auto_merge = true;
        cfg.git.target_branch = "develop".to_owned();
        RunCycleUseCase::new(
            Arc::new(MemStore::default()),
            Arc::new(ReviewEngine {
                decision: "approve",
            }),
            cfg,
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_forge(Arc::clone(&forge) as Arc<dyn ForgePort>)
        .review_open_prs()
        .await;
        assert!(
            forge.merged.lock().expect("lock").is_empty(),
            "a PR into main is not auto-merged when the target is develop"
        );
    }

    #[tokio::test]
    async fn sa_requests_changes_on_reject_and_never_merges_failing_ci() {
        // SA says request_changes → no merge, a changes request instead.
        let forge = Arc::new(SpyForge {
            ci: "passing".to_owned(),
            mergeable: true,
            ..Default::default()
        });
        review_uc(Arc::clone(&forge), "request_changes", true)
            .review_open_prs()
            .await;
        assert!(forge.merged.lock().expect("lock").is_empty());
        assert_eq!(*forge.changes.lock().expect("lock"), vec![7]);

        // Failing CI is never merged, even if the SA would approve.
        let forge = Arc::new(SpyForge {
            ci: "failing".to_owned(),
            mergeable: true,
            ..Default::default()
        });
        review_uc(Arc::clone(&forge), "approve", true)
            .review_open_prs()
            .await;
        assert!(forge.merged.lock().expect("lock").is_empty());
        assert_eq!(*forge.changes.lock().expect("lock"), vec![7]);
    }

    // ---- COX-F001: auto-rollback to last known-good deploy on failure ----
    //
    // These encode the acceptance criteria only. No production rollback logic
    // exists yet — several of these are expected to be RED until it's built.

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A `DeployPort` that returns a scripted sequence of `deploy()` results
    /// (one per call, in order) and always reports tests passing. Once the
    /// script is exhausted, extra calls return a report clearly marked as
    /// unexpected, so an over-eager retry loop shows up in assertions instead
    /// of silently blending in.
    struct ScriptedDeploy {
        script: Mutex<VecDeque<crate::ports::outbound::DeployReport>>,
        deploy_calls: AtomicUsize,
    }
    impl ScriptedDeploy {
        fn new(script: Vec<crate::ports::outbound::DeployReport>) -> Self {
            Self {
                script: Mutex::new(script.into_iter().collect()),
                deploy_calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.deploy_calls.load(Ordering::SeqCst)
        }
    }
    #[async_trait::async_trait]
    impl crate::ports::outbound::DeployPort for ScriptedDeploy {
        async fn deploy(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            self.deploy_calls.fetch_add(1, Ordering::SeqCst);
            let next = self.script.lock().expect("lock").pop_front();
            Ok(next.unwrap_or(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "UNSCRIPTED EXTRA DEPLOY CALL".to_owned(),
            }))
        }
        async fn run_tests(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            Ok(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "tests ok".to_owned(),
            })
        }
    }

    /// Records every event handed to the notifier, so tests can assert a
    /// rollback notification is distinguishable from a plain deploy one.
    #[derive(Default)]
    struct SpyNotifier {
        events: Mutex<Vec<crate::ports::outbound::NotifyEvent>>,
    }
    #[async_trait::async_trait]
    impl crate::ports::outbound::NotifierPort for SpyNotifier {
        async fn notify(&self, event: crate::ports::outbound::NotifyEvent) {
            self.events.lock().expect("lock").push(event);
        }
    }

    /// Fixed sha this fake reports as HEAD — deliberately different from
    /// [`GOOD_SHA`] so a seeded "prior good" deploy is a real rollback target,
    /// not a no-op.
    const HEAD_SHA: &str = "deadbeef";
    /// Fixed sha seeded as the last known-good deploy in rollback tests.
    const GOOD_SHA: &str = "cafef00d";

    /// A `GitPort` double for rollback tests: reports a fixed HEAD, records
    /// every worktree op so a test can assert rollback NEVER touches the live
    /// `work_dir` (only the dedicated rollback path), and reports no changed
    /// paths (no migration in the way) unless a test overrides it.
    #[derive(Default)]
    struct FakeGit {
        worktree_adds: Mutex<Vec<(std::path::PathBuf, String)>>,
        migration_paths: Vec<String>,
    }
    #[async_trait::async_trait]
    impl GitPort for FakeGit {
        async fn is_repo(&self, _: &std::path::Path) -> bool {
            true
        }
        async fn current_branch(&self, _: &std::path::Path) -> Result<String, PortError> {
            Ok("main".to_owned())
        }
        async fn checkout_branch(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
            Ok(())
        }
        async fn commit_all(
            &self,
            _: &std::path::Path,
            _: &str,
            _: &GitAuthor,
        ) -> Result<Option<String>, PortError> {
            Ok(None)
        }
        async fn push(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
            Ok(())
        }
        async fn sync_base(
            &self,
            _: &std::path::Path,
            _: &str,
        ) -> Result<crate::ports::outbound::SyncBase, PortError> {
            Ok(crate::ports::outbound::SyncBase::UpToDate)
        }
        async fn abort_merge(&self, _: &std::path::Path) -> Result<(), PortError> {
            Ok(())
        }
        async fn head_sha(&self, _: &std::path::Path) -> Result<String, PortError> {
            Ok(HEAD_SHA.to_owned())
        }
        async fn update_ref(&self, _: &std::path::Path, _: &str, _: &str) -> Result<(), PortError> {
            Ok(())
        }
        async fn worktree_add(
            &self,
            _work_dir: &std::path::Path,
            path: &std::path::Path,
            sha: &str,
        ) -> Result<(), PortError> {
            self.worktree_adds
                .lock()
                .expect("lock")
                .push((path.to_path_buf(), sha.to_owned()));
            Ok(())
        }
        async fn worktree_remove(
            &self,
            _: &std::path::Path,
            _: &std::path::Path,
        ) -> Result<(), PortError> {
            Ok(())
        }
        async fn changed_paths(
            &self,
            _: &std::path::Path,
            _: &str,
            _: &str,
        ) -> Result<Vec<String>, PortError> {
            Ok(self.migration_paths.clone())
        }
    }

    /// A cycle that will complete a fresh feature (so the deploy step runs),
    /// with `state.last_good_deploy` pre-seeded to reflect whether a prior
    /// deploy+tests pass exists. `auto_rollback` is on (opt-in in production,
    /// but these tests exist to exercise it).
    fn rollback_uc(
        prior_good: bool,
        deploy: &Arc<ScriptedDeploy>,
        notifier: &Arc<SpyNotifier>,
    ) -> (
        Arc<MemStore>,
        Arc<FakeGit>,
        RunCycleUseCase<MemStore, RoleAwareEngine>,
    ) {
        let mut initial = ProjectState::default();
        if prior_good {
            initial.last_good_deploy = Some(crate::state::KnownGoodDeploy {
                sha: GOOD_SHA.to_owned(),
                at: crate::state::now_rfc3339(),
                deploy_index: 1,
                summary: "prior deploy + tests passed".to_owned(),
            });
        }
        let store = Arc::new(MemStore {
            state: Mutex::new(initial),
        });
        let git = Arc::new(FakeGit::default());
        let mut cfg = Config::default();
        cfg.deploy.auto_rollback = true;
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
        .with_git(Arc::clone(&git) as Arc<dyn GitPort>)
        .with_notifier(Arc::clone(notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);
        (store, git, uc)
    }

    fn deploy_bug_tickets(state: &ProjectState) -> Vec<&coxagent_domain::Ticket> {
        use coxagent_domain::ticket::{Priority, TicketType};
        state
            .tickets
            .iter()
            .filter(|t| {
                t.ticket_type() == TicketType::Bug
                    && t.title().starts_with("Deploy failing")
                    && t.priority() == Priority::High
            })
            .collect()
    }

    /// AC1: a failed deploy (or a failed post-deploy `run_tests()`) must
    /// automatically redeploy the last version that previously passed both
    /// deploy and tests — with no human intervention.
    #[tokio::test]
    async fn deploy_failure_auto_rolls_back_to_last_known_good() {
        let deploy = Arc::new(ScriptedDeploy::new(vec![
            crate::ports::outbound::DeployReport {
                success: false,
                deployed: true,
                summary: "deploy failed: container exited 1".to_owned(),
            },
            crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "rollback redeploy ok".to_owned(),
            },
        ]));
        let notifier = Arc::new(SpyNotifier::default());
        let (store, git, uc) = rollback_uc(true, &deploy, &notifier);

        Box::pin(uc.run_cycle(1)).await;

        assert_eq!(
            deploy.calls(),
            2,
            "the failed deploy must be followed by exactly one automatic \
             redeploy of the last known-good version, with no human involved"
        );
        let state = store.load().await.expect("load");
        assert!(
            state.deploy.as_ref().is_some_and(|d| d.ok),
            "after a successful rollback the recorded deploy status must be healthy again"
        );
        // Regression (#1/#2): rollback must target ONLY the dedicated
        // secondary worktree, never the live `work_dir` — so DEV/worker
        // concurrency and the leader tail's own `checkout_branch` calls can
        // never race against it.
        let adds = git.worktree_adds.lock().expect("lock");
        assert_eq!(
            adds.len(),
            1,
            "exactly one worktree created for the rollback"
        );
        assert_ne!(
            adds[0].0,
            PathBuf::from("/tmp/proj"),
            "rollback must never check out into the live work_dir"
        );
        assert_eq!(
            adds[0].1, GOOD_SHA,
            "rollback checks out the known-good sha"
        );
    }

    /// AC2: a rollback event is logged to the activity feed and sent via the
    /// existing `NotifierPort` (same channel as deploy_ok/deploy_failed), and
    /// is distinguishable from a normal deploy notification.
    #[tokio::test]
    async fn rollback_is_logged_and_notified_distinctly_from_a_normal_deploy() {
        let deploy = Arc::new(ScriptedDeploy::new(vec![
            crate::ports::outbound::DeployReport {
                success: false,
                deployed: true,
                summary: "deploy failed: container exited 1".to_owned(),
            },
            crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "rollback redeploy ok".to_owned(),
            },
        ]));
        let notifier = Arc::new(SpyNotifier::default());
        let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

        Box::pin(uc.run_cycle(1)).await;

        let state = store.load().await.expect("load");
        assert!(
            state
                .activity
                .iter()
                .any(|a| a.action.to_lowercase().contains("rollback")),
            "a rollback activity entry must be logged: {:?}",
            state.activity
        );
        let events = notifier.events.lock().expect("lock");
        assert!(
            events
                .iter()
                .any(|e| e.kind.to_lowercase().contains("rollback")
                    || e.message.to_lowercase().contains("rollback")),
            "a rollback notification must be sent via NotifierPort: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.kind != "deploy_ok"
                && (e.kind.to_lowercase().contains("rollback")
                    || e.message.to_lowercase().contains("rollback"))),
            "the rollback notification must be distinguishable from a plain deploy_ok: {events:?}"
        );
    }

    /// AC3: the failure that triggered the rollback still files exactly one
    /// deduped High-priority bug (existing behavior preserved) — root cause
    /// stays tracked work even though the app is back up via rollback.
    #[tokio::test]
    async fn triggering_failure_still_files_exactly_one_deduped_high_bug() {
        let deploy = Arc::new(ScriptedDeploy::new(vec![
            crate::ports::outbound::DeployReport {
                success: false,
                deployed: true,
                summary: "deploy failed: container exited 1".to_owned(),
            },
            crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "rollback redeploy ok".to_owned(),
            },
        ]));
        let notifier = Arc::new(SpyNotifier::default());
        let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

        Box::pin(uc.run_cycle(1)).await;

        let state = store.load().await.expect("load");
        assert_eq!(
            deploy_bug_tickets(&state).len(),
            1,
            "exactly one deduped High bug for the root cause, rollback or not"
        );
    }

    /// AC4: with no prior successful deploy, the system must not attempt a
    /// rollback and falls back to today's bug-filing behavior.
    #[tokio::test]
    async fn no_rollback_without_a_prior_successful_deploy() {
        let deploy = Arc::new(ScriptedDeploy::new(vec![
            crate::ports::outbound::DeployReport {
                success: false,
                deployed: true,
                summary: "deploy failed: container exited 1".to_owned(),
            },
        ]));
        let notifier = Arc::new(SpyNotifier::default());
        let (store, _git, uc) = rollback_uc(false, &deploy, &notifier);

        Box::pin(uc.run_cycle(1)).await;

        assert_eq!(
            deploy.calls(),
            1,
            "no prior successful deploy exists, so no rollback attempt is made"
        );
        let state = store.load().await.expect("load");
        assert_eq!(
            deploy_bug_tickets(&state).len(),
            1,
            "falls back to the existing deduped High-bug-filing behavior"
        );
        let events = notifier.events.lock().expect("lock");
        assert!(
            !events
                .iter()
                .any(|e| e.kind.to_lowercase().contains("rollback")
                    || e.message.to_lowercase().contains("rollback")),
            "no rollback notification should fire when there is nothing to roll back to: {events:?}"
        );
    }

    /// AC5: if the rollback attempt itself fails, the system does not retry
    /// indefinitely — it stops after one retry and escalates via the existing
    /// bug+notify path.
    #[tokio::test]
    async fn rollback_failure_stops_after_one_retry_and_escalates() {
        let deploy = Arc::new(ScriptedDeploy::new(vec![
            crate::ports::outbound::DeployReport {
                success: false,
                deployed: true,
                summary: "deploy failed: container exited 1".to_owned(),
            },
            crate::ports::outbound::DeployReport {
                success: false,
                deployed: true,
                summary: "rollback redeploy ALSO failed".to_owned(),
            },
        ]));
        let notifier = Arc::new(SpyNotifier::default());
        let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

        Box::pin(uc.run_cycle(1)).await;

        assert_eq!(
            deploy.calls(),
            2,
            "exactly one rollback retry — the original failed attempt plus one \
             rollback attempt, never an unbounded retry loop"
        );
        let state = store.load().await.expect("load");
        assert_eq!(
            deploy_bug_tickets(&state).len(),
            1,
            "the failure still escalates via the existing deduped High-bug path"
        );
        assert!(
            !notifier.events.lock().expect("lock").is_empty(),
            "a failed rollback must still escalate via the existing NotifierPort path"
        );
    }

    // --- COX-B004: deploy success gate must verify the app bound its port -

    /// `docker compose up -d --build` exits 0 (container started) but the app
    /// inside never answers on the configured port — e.g. it panics right
    /// after entrypoint, or binds the wrong internal port. `health()` reports
    /// down for every probe.
    struct DeployWithDeadPort {
        deploy_calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl crate::ports::outbound::DeployPort for DeployWithDeadPort {
        async fn deploy(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            self.deploy_calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "docker compose up -d --build succeeded".to_owned(),
            })
        }
        async fn run_tests(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            Ok(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "tests ok".to_owned(),
            })
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Ok(false)
        }
    }

    /// AC (COX-B004): a `docker compose up` exit-0 that never binds the
    /// configured port must be treated as a deploy FAILURE — not recorded as
    /// known-good, and filed as a bug like any other deploy failure.
    #[tokio::test(start_paused = true)]
    async fn deploy_that_never_binds_its_port_is_treated_as_a_failure() {
        let store = Arc::new(MemStore::default());
        let deploy = Arc::new(DeployWithDeadPort {
            deploy_calls: AtomicUsize::new(0),
        });
        let mut cfg = Config::default();
        cfg.deploy.host_port = Some(8101);
        let notifier = Arc::new(SpyNotifier::default());
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

        Box::pin(uc.run_cycle(1)).await;

        let state = store.load().await.expect("load");
        assert!(
            state.deploy.as_ref().is_some_and(|d| !d.ok),
            "exit 0 from `docker compose up` must not be enough on its own — \
             the app never bound its port: {:?}",
            state.deploy
        );
        assert!(
            state.last_good_deploy.is_none(),
            "a deploy that never bound its port must never become the auto-rollback target"
        );
        assert_eq!(
            deploy_bug_tickets(&state).len(),
            1,
            "a deploy that never binds its port must file a bug like any other deploy failure"
        );
        let events = notifier.events.lock().expect("lock");
        assert!(
            events.iter().any(|e| e.kind == "deploy_failed"),
            "the notified event must be deploy_failed, not deploy_ok: {events:?}"
        );
    }

    /// Scripted deploy where only the 2nd `deploy()` call (the rollback
    /// redeploy) actually binds the port — models the forward deploy starting
    /// a container that never listens, followed by a rollback that does.
    struct HealthOnSecondDeployOnly {
        deploy_script: Mutex<VecDeque<crate::ports::outbound::DeployReport>>,
        deploy_calls: AtomicUsize,
    }
    impl HealthOnSecondDeployOnly {
        fn new(deploy_script: Vec<crate::ports::outbound::DeployReport>) -> Self {
            Self {
                deploy_script: Mutex::new(deploy_script.into_iter().collect()),
                deploy_calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.deploy_calls.load(Ordering::SeqCst)
        }
    }
    #[async_trait::async_trait]
    impl crate::ports::outbound::DeployPort for HealthOnSecondDeployOnly {
        async fn deploy(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            self.deploy_calls.fetch_add(1, Ordering::SeqCst);
            let next = self.deploy_script.lock().expect("lock").pop_front();
            Ok(next.unwrap_or(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "UNSCRIPTED EXTRA DEPLOY CALL".to_owned(),
            }))
        }
        async fn run_tests(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            Ok(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "tests ok".to_owned(),
            })
        }
        async fn health(&self, _port: u16) -> Result<bool, PortError> {
            Ok(self.calls() >= 2)
        }
    }

    /// AC (COX-B004): a deploy whose containers start but never bind the
    /// port must drive the SAME auto-rollback path as a hard deploy failure
    /// (docker exit != 0) — the health gate cannot be skipped just because
    /// the compose command itself reported success.
    #[tokio::test(start_paused = true)]
    async fn health_check_failure_triggers_rollback_like_any_other_deploy_failure() {
        let deploy = Arc::new(HealthOnSecondDeployOnly::new(vec![
            crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "docker compose up -d --build succeeded".to_owned(),
            },
            crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "rollback redeploy ok".to_owned(),
            },
        ]));
        let initial = ProjectState {
            last_good_deploy: Some(crate::state::KnownGoodDeploy {
                sha: GOOD_SHA.to_owned(),
                at: crate::state::now_rfc3339(),
                deploy_index: 1,
                summary: "prior deploy + tests passed".to_owned(),
            }),
            ..ProjectState::default()
        };
        let store = Arc::new(MemStore {
            state: Mutex::new(initial),
        });
        let git = Arc::new(FakeGit::default());
        let mut cfg = Config::default();
        cfg.deploy.auto_rollback = true;
        cfg.deploy.host_port = Some(8101);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
        .with_git(Arc::clone(&git) as Arc<dyn GitPort>);

        Box::pin(uc.run_cycle(1)).await;

        assert_eq!(
            deploy.calls(),
            2,
            "a deploy that never binds its port must trigger exactly one automatic \
             rollback redeploy"
        );
        let state = store.load().await.expect("load");
        assert!(
            state.deploy.as_ref().is_some_and(|d| d.ok),
            "once the rollback redeploy actually binds the port, the recorded deploy \
             status must be healthy again: {:?}",
            state.deploy
        );
        assert!(
            state.last_rollback.as_ref().is_some_and(|r| r.ok),
            "the rollback must be recorded as successful: {:?}",
            state.last_rollback
        );
    }

    // --- COX-F005: mandatory pre-deploy health-endpoint check ------------
    //
    // After a deploy attempt, the app's health endpoint on the configured
    // port must be checked within a bounded timeout (~30s) before the
    // deploy is marked successful. A failing/timed-out check marks the
    // deploy failed and triggers the existing COX-F001 auto-rollback; a
    // passing check marks it successful with no rollback. Either way, the
    // check's pass/fail, HTTP status, and response time are recorded in the
    // deploy history for that attempt. An unreachable endpoint (connection
    // refused, DNS failure) must be treated as a failed check, never left
    // hanging.

    /// Deploy that always starts cleanly; the detailed health-endpoint probe
    /// is scripted so tests can model pass/fail scenarios independently of the
    /// legacy `health()` TCP gate (COX-B004), which this mock leaves at its
    /// default `Ok(true)` so it never masks the new gate under test.
    ///
    /// The script is keyed on how many deploys have run — not on how many
    /// probes have — so a scenario stays stable however many times the gate
    /// polls: "the forward deploy is unhealthy, the rollback redeploy is
    /// healthy" is `deploy_calls <= 1`, regardless of poll count.
    struct ScriptedHealthCheckDeploy {
        deploy_calls: AtomicUsize,
        health_check_calls: AtomicUsize,
        health_check_script: fn(usize) -> crate::state::HealthCheckResult,
        /// Models an adapter whose own probe wedges (a half-open connection
        /// that never completes) rather than returning a failure.
        health_check_hangs: bool,
    }
    #[async_trait::async_trait]
    impl crate::ports::outbound::DeployPort for ScriptedHealthCheckDeploy {
        async fn deploy(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            self.deploy_calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "docker compose up -d --build succeeded".to_owned(),
            })
        }
        async fn run_tests(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            Ok(crate::ports::outbound::DeployReport {
                success: true,
                deployed: true,
                summary: "tests ok".to_owned(),
            })
        }
        async fn health_check(&self, _port: u16) -> crate::state::HealthCheckResult {
            self.health_check_calls.fetch_add(1, Ordering::SeqCst);
            if self.health_check_hangs {
                // Far longer than any bound under test: the caller's guard, not
                // this probe, has to be what ends the wait.
                tokio::time::sleep(std::time::Duration::from_secs(86_400)).await;
            }
            (self.health_check_script)(self.deploy_calls.load(Ordering::SeqCst))
        }
    }

    /// AC (COX-F005): a health check that passes marks the deploy successful,
    /// triggers no rollback, and records pass/status/timing in deploy
    /// history for that attempt.
    #[tokio::test(start_paused = true)]
    async fn health_check_pass_marks_deploy_successful_with_no_rollback() {
        let deploy = Arc::new(ScriptedHealthCheckDeploy {
            deploy_calls: AtomicUsize::new(0),
            health_check_calls: AtomicUsize::new(0),
            health_check_script: |_deploys| crate::state::HealthCheckResult {
                passed: true,
                http_status: Some(200),
                response_time_ms: Some(45),
            },
            health_check_hangs: false,
        });
        let store = Arc::new(MemStore::default());
        let mut cfg = Config::default();
        cfg.deploy.auto_rollback = true;
        cfg.deploy.host_port = Some(8101);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>);

        Box::pin(uc.run_cycle(1)).await;

        assert_eq!(
            deploy.deploy_calls.load(Ordering::SeqCst),
            1,
            "a passing health check must not trigger a rollback redeploy"
        );
        let state = store.load().await.expect("load");
        assert!(
            state.last_rollback.is_none(),
            "no rollback may be attempted when the health check passes: {:?}",
            state.last_rollback
        );
        assert_eq!(
            state.deploy.as_ref().and_then(|d| d.health_check.clone()),
            Some(crate::state::HealthCheckResult {
                passed: true,
                http_status: Some(200),
                response_time_ms: Some(45)
            }),
            "the health check's pass/status/response-time must be recorded in deploy \
             history for this attempt: {:?}",
            state.deploy
        );
    }

    /// AC (COX-F005): a failing health check marks the deploy failed,
    /// automatically triggers the existing COX-F001 auto-rollback, and
    /// records the failing status/timing in deploy history for that
    /// attempt.
    #[tokio::test(start_paused = true)]
    async fn health_check_failure_marks_deploy_failed_and_triggers_rollback() {
        let deploy = Arc::new(ScriptedHealthCheckDeploy {
            deploy_calls: AtomicUsize::new(0),
            health_check_calls: AtomicUsize::new(0),
            // The forward deploy stays unhealthy for the whole bounded wait —
            // every poll fails, so the gate can only resolve by timing out.
            // The rollback redeploy (deploy #2) is healthy.
            health_check_script: |deploys| {
                if deploys <= 1 {
                    crate::state::HealthCheckResult {
                        passed: false,
                        http_status: Some(503),
                        response_time_ms: Some(120),
                    }
                } else {
                    crate::state::HealthCheckResult {
                        passed: true,
                        http_status: Some(200),
                        response_time_ms: Some(50),
                    }
                }
            },
            health_check_hangs: false,
        });
        let initial = ProjectState {
            last_good_deploy: Some(crate::state::KnownGoodDeploy {
                sha: GOOD_SHA.to_owned(),
                at: crate::state::now_rfc3339(),
                deploy_index: 1,
                summary: "prior deploy + tests passed".to_owned(),
            }),
            ..Default::default()
        };
        let store = Arc::new(MemStore {
            state: Mutex::new(initial),
        });
        let git = Arc::new(FakeGit::default());
        let mut cfg = Config::default();
        cfg.deploy.auto_rollback = true;
        cfg.deploy.host_port = Some(8101);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
        .with_git(Arc::clone(&git) as Arc<dyn GitPort>);

        Box::pin(uc.run_cycle(1)).await;

        assert_eq!(
            deploy.deploy_calls.load(Ordering::SeqCst),
            2,
            "a failing health check must trigger exactly one automatic rollback redeploy"
        );
        let state = store.load().await.expect("load");
        assert!(
            state.last_rollback.as_ref().is_some_and(|r| r.ok),
            "the auto-rollback (COX-F001) must fire and succeed: {:?}",
            state.last_rollback
        );
        assert_eq!(
            state.deploy.as_ref().and_then(|d| d.health_check.clone()),
            Some(crate::state::HealthCheckResult {
                passed: false,
                http_status: Some(503),
                response_time_ms: Some(120),
            }),
            "the FAILING attempt's health-check result (status/timing) must be recorded \
             in deploy history, not silently dropped when the rollback overwrites the \
             live deploy status: {:?}",
            state.deploy
        );
        assert!(
            state.activity.iter().any(|a| a
                .action
                .contains("health check failed: HTTP 503 after 120ms")),
            "the health outcome must reach the activity log too, so the history reads \
             as more than a bare 'deploy failed'"
        );
    }

    /// AC (COX-F005): a health endpoint that never answers must be bounded and
    /// reported as a failed check, never left hanging indefinitely — even when
    /// it is the adapter's own probe that wedges rather than the poll loop.
    #[tokio::test(start_paused = true)]
    async fn health_endpoint_that_never_answers_is_bounded_and_fails_the_deploy() {
        let deploy = Arc::new(ScriptedHealthCheckDeploy {
            deploy_calls: AtomicUsize::new(0),
            health_check_calls: AtomicUsize::new(0),
            health_check_script: |_deploys| unreachable!("the hanging probe never returns"),
            health_check_hangs: true,
        });
        let store = Arc::new(MemStore::default());
        let mut cfg = Config::default();
        cfg.deploy.host_port = Some(8101);
        // Small bound so the assertion below names a concrete window; virtual
        // time makes the wait itself instant.
        cfg.deploy.health_check_timeout_secs = 6;
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>);

        let started = tokio::time::Instant::now();
        Box::pin(uc.run_cycle(1)).await;
        let waited = started.elapsed();

        assert!(
            waited < std::time::Duration::from_secs(60),
            "a wedged health probe must be abandoned near its bound, not awaited \
             forever; waited {waited:?}"
        );
        let state = store.load().await.expect("load");
        let status = state.deploy.as_ref().expect("a deploy status was recorded");
        assert!(
            !status.ok,
            "a health endpoint that never answers must fail the deploy: {status:?}"
        );
        assert_eq!(
            status.health_check.as_ref().map(|h| h.passed),
            Some(false),
            "the failed check must still be recorded in deploy history: {status:?}"
        );
    }

    /// AC (COX-F005): the gate polls rather than probing once — an app that
    /// needs a few seconds after `docker compose up` to bind its port is
    /// healthy, not a rollback trigger.
    #[tokio::test(start_paused = true)]
    async fn health_check_polls_until_a_slow_starting_app_binds_its_port() {
        // Unhealthy until the fourth poll — well past a single immediate probe.
        static POLLS: AtomicUsize = AtomicUsize::new(0);
        let deploy = Arc::new(ScriptedHealthCheckDeploy {
            deploy_calls: AtomicUsize::new(0),
            health_check_calls: AtomicUsize::new(0),
            health_check_script: |_deploys| {
                if POLLS.fetch_add(1, Ordering::SeqCst) < 3 {
                    crate::state::HealthCheckResult {
                        passed: false,
                        http_status: None,
                        response_time_ms: Some(1),
                    }
                } else {
                    crate::state::HealthCheckResult {
                        passed: true,
                        http_status: Some(200),
                        response_time_ms: Some(12),
                    }
                }
            },
            health_check_hangs: false,
        });
        let store = Arc::new(MemStore::default());
        let mut cfg = Config::default();
        cfg.deploy.auto_rollback = true;
        cfg.deploy.host_port = Some(8101);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj"),
            "goal".to_owned(),
        )
        .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>);

        Box::pin(uc.run_cycle(1)).await;

        assert!(
            deploy.health_check_calls.load(Ordering::SeqCst) >= 4,
            "the gate must keep polling within its bound, not give up after one probe"
        );
        assert_eq!(
            deploy.deploy_calls.load(Ordering::SeqCst),
            1,
            "an app that binds its port a few seconds late must not be rolled back"
        );
        let state = store.load().await.expect("load");
        assert_eq!(
            state.deploy.as_ref().and_then(|d| d.health_check.clone()),
            Some(crate::state::HealthCheckResult {
                passed: true,
                http_status: Some(200),
                response_time_ms: Some(12)
            }),
            "the passing poll's detail is what belongs in deploy history: {:?}",
            state.deploy
        );
    }

    // --- COX-F003: unsupported-platform sandbox warning -------------------
    //
    // When `workflow.sandbox` is on but the platform has no supported
    // confinement mechanism (no macOS Seatbelt, and on Linux no `bwrap` on
    // PATH — or Windows), the run must NOT hard-fail: the cycle still
    // executes. It must instead raise exactly one `sandbox_unsupported`
    // NotifierPort event per project per process lifetime — visible in the
    // #agents channel/dashboard, not just a log line — and must NOT re-post
    // it on every cycle.

    /// AC: on a platform with no supported sandbox backend, a cycle with
    /// `sandbox: true` still completes and posts exactly one
    /// `sandbox_unsupported` NotifierPort event across multiple cycles (not
    /// one per cycle).
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn sandbox_unsupported_warning_fires_once_not_every_cycle() {
        // This AC only bites where there is truly no confinement backend. If
        // this machine happens to have bwrap installed, Linux sandboxing IS
        // supported and no warning is expected — skip rather than false-fail.
        if cfg!(target_os = "linux")
            && std::process::Command::new("bwrap")
                .arg("--version")
                .output()
                .is_ok()
        {
            return;
        }
        let notifier = Arc::new(SpyNotifier::default());
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState::default()),
        });
        let mut cfg = Config::default();
        cfg.workflow.sandbox = true;
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj-sandbox-warn"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

        Box::pin(uc.run_cycle(1)).await;
        Box::pin(uc.run_cycle(2)).await;

        let events: Vec<_> = notifier
            .events
            .lock()
            .expect("lock")
            .iter()
            .filter(|e| e.kind == "sandbox_unsupported")
            .cloned()
            .collect();
        assert_eq!(
            events.len(),
            1,
            "must warn about missing sandbox support exactly once per project \
             per process lifetime, not on every cycle (got {events:?})"
        );
    }

    // --- COX-F002: early-warning notification before budget cap halts the loop
    //
    // When accumulated spend crosses 80% of the configured lifetime or daily
    // budget (whichever applies), the loop must raise exactly one
    // `budget_warning` NotifierPort event before the hard `budget_reached`
    // pause at 100%. The warning fires once per threshold crossing — it must
    // not repeat every cycle while spend sits between 80% and 100% — and can
    // fire again once a daily cap resets for a new day and spend re-crosses
    // 80%. With no budget cap configured, no warning is ever sent, and a
    // warning alone must never pause the loop.

    fn budget_warning_events(notifier: &SpyNotifier) -> Vec<crate::ports::outbound::NotifyEvent> {
        notifier
            .events
            .lock()
            .expect("lock")
            .iter()
            .filter(|e| e.kind == "budget_warning")
            .cloned()
            .collect()
    }

    /// AC: spend at 80% of the configured lifetime cap raises exactly one
    /// `budget_warning` and does NOT pause the loop.
    #[tokio::test]
    async fn budget_warning_fires_at_80_percent_of_lifetime_cap_without_pausing() {
        let store = Arc::new(MemStore::default());
        let notifier = Arc::new(SpyNotifier::default());
        let meter = Arc::new(Mutex::new(Spend {
            total_cost_usd: 80.0,
            ..Spend::default()
        }));
        let mut cfg = Config::default();
        cfg.workflow.budget_usd = Some(100.0);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj-budget-warn-lifetime"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
        .with_meter(Arc::clone(&meter));

        let report = Box::pin(uc.run_cycle(1)).await;

        assert_eq!(
            budget_warning_events(&notifier).len(),
            1,
            "spend at 80% of the lifetime cap must raise exactly one budget_warning"
        );
        assert!(
            !report.over_budget,
            "80% spend must not pause the loop — only the 100% cap does"
        );
    }

    /// AC: the warning does not repeat every cycle while spend stays between
    /// 80% and 100% of the cap.
    #[tokio::test]
    async fn budget_warning_does_not_repeat_while_spend_stays_between_80_and_100_percent() {
        let store = Arc::new(MemStore::default());
        let notifier = Arc::new(SpyNotifier::default());
        let meter = Arc::new(Mutex::new(Spend::default()));
        let mut cfg = Config::default();
        cfg.workflow.budget_usd = Some(100.0);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj-budget-warn-no-repeat"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
        .with_meter(Arc::clone(&meter));

        meter.lock().expect("lock").total_cost_usd = 80.0;
        Box::pin(uc.run_cycle(1)).await;
        assert_eq!(
            budget_warning_events(&notifier).len(),
            1,
            "first cycle crosses 80% — exactly one warning"
        );

        meter.lock().expect("lock").total_cost_usd = 5.0; // cumulative 85% — still under the cap
        Box::pin(uc.run_cycle(2)).await;
        assert_eq!(
            budget_warning_events(&notifier).len(),
            1,
            "spend staying between 80% and 100% across cycles must not re-fire the warning"
        );
    }

    /// AC: after the daily cap resets for a new day, the warning can fire
    /// again that day if spend re-crosses 80%.
    #[tokio::test]
    async fn budget_warning_can_fire_again_after_the_daily_cap_resets_for_a_new_day() {
        let store = Arc::new(MemStore::default());
        let notifier = Arc::new(SpyNotifier::default());
        let meter = Arc::new(Mutex::new(Spend::default()));
        let mut cfg = Config::default();
        cfg.policy.daily_budget_usd = Some(100.0);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj-budget-warn-daily-reset"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
        .with_meter(Arc::clone(&meter));

        meter.lock().expect("lock").total_cost_usd = 80.0;
        Box::pin(uc.run_cycle(1)).await;
        assert_eq!(
            budget_warning_events(&notifier).len(),
            1,
            "day 1: crossing 80% of the daily cap warns once"
        );

        // Force a day rollover: production code keys the daily cap off the
        // real UTC date, so back-date the persisted counter directly rather
        // than mocking the clock.
        let mut state = store.load().await.expect("load");
        state.spend_day = "2000-01-01".to_owned();
        store.save(&state).await.expect("save");

        meter.lock().expect("lock").total_cost_usd = 80.0;
        Box::pin(uc.run_cycle(2)).await;
        assert_eq!(
            budget_warning_events(&notifier).len(),
            2,
            "day 2: re-crossing 80% of the reset daily cap must warn again"
        );
    }

    /// AC: with no budget cap configured (neither lifetime nor daily), no
    /// warning notification is ever sent, no matter how much is spent.
    #[tokio::test]
    async fn no_budget_warning_when_no_cap_is_configured() {
        let store = Arc::new(MemStore::default());
        let notifier = Arc::new(SpyNotifier::default());
        let meter = Arc::new(Mutex::new(Spend {
            total_cost_usd: 999_999.0,
            ..Spend::default()
        }));
        let cfg = Config::default(); // budget_usd and daily_budget_usd both None
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj-budget-warn-no-cap"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
        .with_meter(Arc::clone(&meter));

        let report = Box::pin(uc.run_cycle(1)).await;

        assert!(
            budget_warning_events(&notifier).is_empty(),
            "no cap configured must never raise a budget_warning"
        );
        assert!(
            !report.over_budget,
            "no cap configured must never pause the loop"
        );
    }

    /// AC: only crossing 100% still triggers the existing hard stop — spend
    /// jumping straight past 80% to (or over) the cap in one cycle must still
    /// raise the 80% warning (before the pause) AND pause the loop.
    #[tokio::test]
    async fn crossing_the_full_cap_still_pauses_the_loop_after_the_warning() {
        let store = Arc::new(MemStore::default());
        let notifier = Arc::new(SpyNotifier::default());
        let meter = Arc::new(Mutex::new(Spend {
            total_cost_usd: 150.0,
            ..Spend::default()
        }));
        let mut cfg = Config::default();
        cfg.workflow.budget_usd = Some(100.0);
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            cfg,
            PathBuf::from("/tmp/proj-budget-warn-hard-stop"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
        .with_meter(Arc::clone(&meter));

        let report = Box::pin(uc.run_cycle(1)).await;

        assert!(
            report.over_budget,
            "spend at/over the lifetime cap must still pause the loop"
        );
        let events = notifier.events.lock().expect("lock");
        assert_eq!(
            events.iter().filter(|e| e.kind == "budget_reached").count(),
            1,
            "exactly one hard budget_reached event"
        );
        assert_eq!(
            events.iter().filter(|e| e.kind == "budget_warning").count(),
            1,
            "the 80% warning must still fire even when spend jumps straight past it to the cap"
        );
        let warning_at = events.iter().position(|e| e.kind == "budget_warning");
        let reached_at = events.iter().position(|e| e.kind == "budget_reached");
        assert!(
            warning_at < reached_at,
            "the warning must be emitted before the hard pause, got order {events:?}"
        );
    }

    // --- COX-F007: route the SM impediment digest through the external notifier ---

    /// AC1: with one or more impediment items AND an external webhook
    /// configured (a `NotifierPort` attached), the digest must be delivered
    /// through that notifier — not just posted to the in-app `AGENTS_CHANNEL`.
    #[tokio::test]
    async fn impediment_digest_is_delivered_via_the_external_notifier_when_configured() {
        let store = Arc::new(MemStore {
            // one deterministic impediment item
            state: Mutex::new(ProjectState {
                queue_recovery: true,
                ..ProjectState::default()
            }),
        });
        let notifier = Arc::new(SpyNotifier::default());
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp/proj-imp-webhook"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

        Box::pin(uc.run_cycle(1)).await;

        // The ticket comment trail must still receive its post_comment write.
        let state = store.load().await.expect("load");
        // Clone out of the guard so nothing is held across the await above.
        let (all, digest) = {
            let events = notifier.events.lock().expect("lock");
            let digest: Vec<_> = events
                .iter()
                .filter(|e| e.kind == "impediment_digest")
                .cloned()
                .collect();
            (events.clone(), digest)
        };
        assert_eq!(
            digest.len(),
            1,
            "the impediment digest must be delivered exactly once through the external \
             NotifierPort when one is configured: {all:?}"
        );
        assert!(
            digest[0].message.contains("Impediment watch")
                && digest[0].message.contains("RECOVERY"),
            "the notified message must carry the digest body: {:?}",
            digest[0].message
        );
        assert!(
            !digest[0].message.starts_with('🚧'),
            "the icon belongs to the notifier adapter, not the message body: {:?}",
            digest[0].message
        );
        assert!(
            state
                .comments
                .iter()
                .any(|c| c.body.contains("Impediment watch")),
            "the digest must still be written to the comment trail: {:?}",
            state.comments
        );
    }

    /// AC2: with no external webhook configured (no `NotifierPort` attached),
    /// behavior is unchanged from today — the digest still posts to the
    /// in-app `AGENTS_CHANNEL` only.
    #[tokio::test]
    async fn impediment_digest_still_posts_to_chat_only_when_no_webhook_is_configured() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                queue_recovery: true,
                ..ProjectState::default()
            }),
        });
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp/proj-imp-no-webhook"),
            "goal".to_owned(),
        );

        Box::pin(uc.run_cycle(1)).await;

        let state = store.load().await.expect("load");
        assert!(
            state
                .chat
                .iter()
                .any(|m| m.channel == crate::state::AGENTS_CHANNEL
                    && m.body.to_lowercase().contains("impediment watch")),
            "the impediment digest must still post to the in-app AGENTS_CHANNEL when no \
             external webhook is configured: {:?}",
            state.chat
        );
    }

    /// AC3: with no impediment items, no notification fires on either sink —
    /// same as current behavior.
    #[tokio::test]
    async fn no_impediment_items_means_no_notification_on_either_sink() {
        let store = Arc::new(MemStore::default()); // clean state: nothing stuck/parked/red/recovering
        let notifier = Arc::new(SpyNotifier::default());
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp/proj-imp-empty"),
            "goal".to_owned(),
        )
        .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

        Box::pin(uc.run_cycle(1)).await;

        let state = store.load().await.expect("load");
        assert!(
            !state
                .chat
                .iter()
                .any(|m| m.body.to_lowercase().contains("impediment watch")),
            "no impediment chat post should fire when there is nothing to report: {:?}",
            state.chat
        );
        let events = notifier.events.lock().expect("lock");
        assert!(
            !events
                .iter()
                .any(|e| e.kind.to_lowercase().contains("impediment")
                    || e.message.to_lowercase().contains("impediment watch")),
            "no impediment notifier event should fire when there is nothing to report: {events:?}"
        );
    }

    /// AC4: the once-per-day gate (`last_impediment_day`) still prevents a
    /// duplicate send — on both sinks — within the same day after the change.
    #[tokio::test]
    async fn impediment_digest_once_per_day_gate_still_dedupes_both_sinks() {
        let store = Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                queue_recovery: true,
                ..ProjectState::default()
            }),
        });
        let notifier = Arc::new(SpyNotifier::default());
        // Mirror production's build_notifier wiring: a FanoutNotifier over a
        // real ChatNotifier (in-app delivery) plus the external sink — so the
        // gate is exercised end-to-end exactly as it runs live, instead of
        // against a bare spy that can't itself post chat.
        let fanout = Arc::new(crate::ports::outbound::FanoutNotifier(vec![
            Arc::new(crate::ports::outbound::ChatNotifier::new(Arc::clone(
                &store,
            ))) as Arc<dyn crate::ports::outbound::NotifierPort>,
            Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>,
        ]));
        let uc = RunCycleUseCase::new(
            Arc::clone(&store),
            Arc::new(RoleAwareEngine),
            Config::default(),
            PathBuf::from("/tmp/proj-imp-dedupe"),
            "goal".to_owned(),
        )
        .with_notifier(fanout as Arc<dyn crate::ports::outbound::NotifierPort>);

        Box::pin(uc.run_cycle(1)).await;
        Box::pin(uc.run_cycle(2)).await; // same UTC day — must not resend

        let state = store.load().await.expect("load");
        assert_eq!(
            state.last_impediment_day,
            crate::state::now_rfc3339()[..10].to_owned(),
            "the once-per-day gate must be stamped with today's UTC day"
        );
        let chat_hits = state
            .chat
            .iter()
            .filter(|m| {
                m.channel == crate::state::AGENTS_CHANNEL
                    && m.body.to_lowercase().contains("impediment watch")
            })
            .count();
        assert_eq!(
            chat_hits, 1,
            "the once-per-day gate must prevent a duplicate in-app chat post: {:?}",
            state.chat
        );
        let events = notifier.events.lock().expect("lock");
        let notifier_hits = events
            .iter()
            .filter(|e| {
                e.kind.to_lowercase().contains("impediment")
                    || e.message.to_lowercase().contains("impediment watch")
            })
            .count();
        assert_eq!(
            notifier_hits, 1,
            "the once-per-day gate must prevent a duplicate external notifier event: {events:?}"
        );
    }
}
