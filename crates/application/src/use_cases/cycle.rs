//! `RunCycleUseCase` — one turn of the loop: BA (periodic) → DEV-BUG →
//! DEV-FEATURE → TEST. A failing agent is recorded and the cycle continues, so
//! one bad run never stalls the team (matching the reference workflow).

use crate::config::Config;
use crate::ports::outbound::{
    AgentEnginePort, AgentRequest, DeployPort, ForgePort, GitAuthor, GitPort, StateStorePort,
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
use std::sync::{Arc, Mutex};

/// The SA reviewer's JSON verdict on a pull request.
#[derive(serde::Deserialize)]
struct ReviewVerdict {
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
    /// Live, runtime-adjustable spend caps (overrides the config caps when set).
    budget: Option<crate::config::LiveBudget>,
    /// Local git, used for branch + commit per ticket when `config.git.enabled`.
    git: Option<Arc<dyn GitPort>>,
    /// The code host, used to open PRs when `config.git.auto_pr`.
    forge: Option<Arc<dyn ForgePort>>,
    /// Reports the currently executing agent to the runner (live "working now").
    phase: Option<crate::use_cases::runner::PhaseReporter>,
    /// This runner's identity (`account@host`) — recorded as the ticket claim
    /// owner so concurrent runners on a shared backlog never collide.
    worker: String,
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
            budget: None,
            git: None,
            forge: None,
            phase: None,
            worker: String::new(),
        }
    }

    /// Set this runner's identity (`account@host`), used as the ticket claim
    /// owner. Called by `run_forever` from the live operator each cycle.
    pub fn set_worker(&mut self, worker: impl Into<String>) {
        self.worker = worker.into();
    }

    /// Set the live phase reporter (called by `run_forever`).
    pub fn set_phase_reporter(&mut self, reporter: crate::use_cases::runner::PhaseReporter) {
        self.phase = Some(reporter);
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

    /// Ship a just-completed ticket through the git flow, when enabled:
    /// commit the work on a per-ticket branch (`feat/<id>`, stacked on the
    /// current tip so nothing is lost while PRs await review), push it, and —
    /// when `auto_pr` — open a PR into the default branch for a human to review.
    /// Best-effort at every step: any failure is logged and never stalls the
    /// cycle. `kind` is `feat`/`fix`.
    async fn commit_for_ticket(&self, id: &TicketId, kind: &str) {
        if !self.config.git.enabled {
            return;
        }
        let Some(git) = &self.git else { return };
        if !git.is_repo(&self.work_dir).await {
            return;
        }
        let title = self
            .store
            .load()
            .await
            .ok()
            .and_then(|s| {
                s.tickets
                    .iter()
                    .find(|t| t.id() == id)
                    .map(|t| t.title().to_owned())
            })
            .unwrap_or_else(|| id.to_string());

        let branch = format!("{}{id}", self.config.git.branch_prefix);
        if let Err(e) = git.checkout_branch(&self.work_dir, &branch).await {
            self.log_git(&format!("branch {branch} failed: {e}")).await;
            return;
        }
        let email = if self.config.git.commit_email.trim().is_empty() {
            "coxagent-bot@users.noreply.github.com".to_owned()
        } else {
            self.config.git.commit_email.clone()
        };
        let author = GitAuthor {
            name: "coxagent-bot".to_owned(),
            email,
        };
        let msg = format!("{kind}({id}): {title}");
        match git.commit_all(&self.work_dir, &msg, &author).await {
            Ok(Some(sha)) => self.log_git(&format!("committed {sha} on {branch}")).await,
            Ok(None) => return, // nothing changed — no branch to push
            Err(e) => {
                self.log_git(&format!("commit failed for {id}: {e}")).await;
                return;
            }
        }

        if let Err(e) = git.push(&self.work_dir, &branch).await {
            self.log_git(&format!("push {branch} failed: {e}")).await;
            return;
        }
        self.log_git(&format!("pushed {branch}")).await;

        if self.config.git.auto_pr {
            if let Some(forge) = &self.forge {
                let base = self.flow_base();
                let body = format!(
                    "Automated by CoXAgent for **{id}** — {title}.\n\nReview and merge to ship."
                );
                match forge.open_pr(&branch, base, &msg, &body).await {
                    Ok(pr) => {
                        self.log_git(&format!("opened PR #{} for {id}", pr.number))
                            .await;
                        if let Ok(mut s) = self.store.load().await {
                            s.post_comment(
                                "GIT",
                                &format!(
                                    "Opened PR #{} for {id} — awaiting review. {}",
                                    pr.number, pr.url
                                ),
                                Some(id.to_string()),
                            );
                            let _ = self.store.save(&s).await;
                        }
                    }
                    Err(e) => self.log_git(&format!("open PR for {id} failed: {e}")).await,
                }
            }
        }
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

    /// When `auto_merge` is on, the SA agent deep-dives each open PR (reads the
    /// diff, judges correctness/completeness/safety) and either merges it or
    /// requests changes — the automated stand-in for a human reviewer. Gated by
    /// CI (never merges a failing or conflicting PR) and bounded per cycle to
    /// keep cost predictable. Best-effort throughout.
    async fn review_open_prs(&self) {
        // `auto_review` (default on) drives SA review; `auto_merge` additionally
        // lets an approval merge. With neither, humans review by hand.
        let auto_merge = self.config.git.auto_merge;
        let auto_review = self.config.git.auto_review || auto_merge;
        if !self.config.git.enabled || !auto_review {
            return;
        }
        let Some(forge) = &self.forge else { return };
        let prs = match forge.list_open_prs().await {
            Ok(p) => p,
            Err(e) => {
                self.log_git(&format!("review: list PRs failed: {e}")).await;
                return;
            }
        };
        let target = self.flow_base();
        // Bound cost: review a few PRs per cycle, oldest first.
        for pr in prs.into_iter().rev().take(3) {
            // Only review PRs into the configured target branch; leave PRs aimed
            // elsewhere (e.g. an integration → main promotion) to humans.
            if pr.base != target {
                continue;
            }
            if pr.ci == "pending" {
                continue; // wait for CI before judging
            }
            let blocked = if pr.ci == "failing" {
                Some("CI is failing — fix the build/tests.".to_owned())
            } else if !pr.mergeable {
                Some("The branch has merge conflicts — rebase on the base branch.".to_owned())
            } else {
                None
            };
            if let Some(reason) = blocked {
                let _ = forge.request_changes(pr.number, &reason).await;
                self.record_review(pr.number, "request_changes", &reason)
                    .await;
                self.log_git(&format!(
                    "SA requested changes on PR #{} ({reason})",
                    pr.number
                ))
                .await;
                continue;
            }
            let Ok(diff) = forge.pr_diff(pr.number).await else {
                continue;
            };
            match self.sa_review(&pr.title, &pr.head, &diff).await {
                Some((true, summary)) => {
                    self.record_review(pr.number, "approve", &summary).await;
                    if auto_merge {
                        match forge.merge_pr(pr.number).await {
                            Ok(()) => {
                                self.log_git(&format!("SA approved & merged PR #{}", pr.number))
                                    .await;
                            }
                            Err(e) => {
                                self.log_git(&format!("merge PR #{} failed: {e}", pr.number))
                                    .await;
                            }
                        }
                    } else {
                        // Suggestion only — the user merges from the Review tab.
                        self.log_git(&format!(
                            "SA approved PR #{} — awaiting your merge",
                            pr.number
                        ))
                        .await;
                    }
                }
                Some((false, comment)) => {
                    let _ = forge.request_changes(pr.number, &comment).await;
                    self.record_review(pr.number, "request_changes", &comment)
                        .await;
                    self.log_git(&format!("SA requested changes on PR #{}", pr.number))
                        .await;
                }
                None => {}
            }
        }
    }

    /// (Re)index the working tree into the code graph + `REPO_MAP.md`, so agents
    /// can orient from it. Gated by the token-saver; refreshed on the first cycle
    /// and periodically (indexing off the async runtime). Best-effort.
    async fn refresh_codegraph(&self, cycle: u64) {
        if !self.config.workflow.token_saver {
            return;
        }
        // Only index a real project root (marked by coxagent.json) — never a
        // bare/arbitrary working directory.
        if !self.work_dir.join("coxagent.json").exists() {
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
            let _ = g.save(&root);
            let _ = std::fs::write(
                root.join(".coxagent").join("REPO_MAP.md"),
                g.repo_map(40_000),
            );
        })
        .await;
    }

    /// Persist the SA's verdict so the Review tab can show it as a suggestion.
    async fn record_review(&self, number: u64, decision: &str, summary: &str) {
        if let Ok(mut s) = self.store.load().await {
            s.upsert_review(number, decision, summary);
            let _ = self.store.save(&s).await;
        }
    }

    /// From a unified diff, list the functions it touches and who calls them
    /// (from the code graph). Empty when no graph or nothing recognised — a
    /// best-effort blast-radius hint for the reviewer.
    fn diff_impact(&self, diff: &str) -> String {
        use std::fmt::Write as _;
        let Some(g) = crate::codegraph::CodeGraph::load(&self.work_dir) else {
            return String::new();
        };
        // Files the diff changes (`+++ b/path`), normalised.
        let changed: std::collections::HashSet<String> = diff
            .lines()
            .filter_map(|l| l.strip_prefix("+++ b/").or_else(|| l.strip_prefix("+++ ")))
            .map(|p| p.trim().replace('\\', "/"))
            .collect();
        if changed.is_empty() {
            return String::new();
        }
        // Symbols defined in a changed file whose name appears on a changed line.
        let touched: Vec<&crate::codegraph::Symbol> = g
            .symbols
            .iter()
            .filter(|s| changed.iter().any(|c| c.ends_with(&s.file) || &s.file == c))
            .filter(|s| {
                diff.lines().any(|l| {
                    (l.starts_with('+') || l.starts_with('-')) && l.contains(s.name.as_str())
                })
            })
            .collect();
        let mut out = String::new();
        for s in touched.iter().take(10) {
            let callers = g.callers(&s.name);
            if callers.is_empty() {
                continue;
            }
            let who: Vec<String> = callers.into_iter().take(8).map(|(w, _, _)| w).collect();
            let _ = writeln!(out, "- `{}` is called by: {}", s.name, who.join(", "));
        }
        if out.is_empty() {
            String::new()
        } else {
            format!("\nCall-graph impact — verify the change does not break these callers:\n{out}")
        }
    }

    /// Run the SA engine as a code reviewer over a PR diff. Returns
    /// `Some((approved, comment))`, or `None` if the engine failed / was
    /// unparseable (in which case the PR is left untouched for a human).
    async fn sa_review(&self, title: &str, head: &str, diff: &str) -> Option<(bool, String)> {
        use coxagent_domain::Role;
        let _ = self.config.engine.resolve(Role::Sa);
        // Cap the diff so a huge PR doesn't blow the prompt budget. With the
        // token-saver on, compress (dedupe + drop index noise) rather than a
        // blunt truncation, so more of the real change survives the cap.
        let saver = self.config.workflow.token_saver;
        let clipped: String = if saver {
            crate::tokens::compress_diff(diff, 16_000)
        } else {
            diff.chars().take(16_000).collect()
        };
        let terse = if saver { crate::tokens::TERSE } else { "" };
        // Call-graph impact: functions this diff touches, and who calls them —
        // so the reviewer checks the change doesn't break existing callers.
        let impact = self.diff_impact(diff);
        let task = format!(
            "You are the reviewer on a pull request before merge. Do a deep code review for \
             correctness, completeness, safety, and architecture fit.\n\nPR: {title}\nBranch: \
             {head}\n\nUnified diff:\n```\n{clipped}\n```\n{impact}\nRespond with ONLY JSON: \
             {{\"decision\": \"approve\" | \"request_changes\", \"summary\": \"one short \
             paragraph; if request_changes, list the concrete fixes\"}}.{terse}"
        );
        let request = AgentRequest {
            role: Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(600),
        };
        let outcome = self.engine.run(request).await.ok()?;
        if !outcome.succeeded() {
            return None;
        }
        let raw = &outcome.stdout;
        let start = raw.find('{')?;
        let end = raw.rfind('}')?;
        let v: ReviewVerdict = serde_json::from_str(raw.get(start..=end)?).ok()?;
        let approved = v.decision.eq_ignore_ascii_case("approve");
        let comment = if v.summary.trim().is_empty() {
            "Changes requested by the SA reviewer.".to_owned()
        } else {
            v.summary
        };
        Some((approved, comment))
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

    /// Scrum only: open/roll over the sprint at the start of a cycle, with an SM
    /// retro line when a previous sprint closes and a standup comment on open.
    async fn advance_sprint_if_scrum(&self, cycle: u64) {
        if self.config.workflow.mode != crate::config::Mode::Scrum {
            return;
        }
        let Ok(mut state) = self.store.load().await else {
            return;
        };
        let len = self.config.workflow.sprint_length_cycles;
        let prev = state.sprint.as_ref().map(|s| {
            (
                s.number,
                s.committed.len(),
                crate::sprint::done_count(&state),
            )
        });
        // Capture the closing sprint before `advance` replaces it, so we can run
        // a real review + retro on it.
        let closing = state.sprint.clone();
        if let Some(n) = crate::sprint::advance(&mut state, cycle, len) {
            let _ = prev; // superseded by the richer review below
            if let Some(cl) = &closing {
                Self::sprint_review_and_retro(&mut state, cl);
            }
            Self::sprint_planning(&mut state, n);
            state.log_activity("SM", "opened sprint", Some(format!("sprint {n}")));
            let _ = self.store.save(&state).await;
        }
    }

    /// Sprint Review + Retrospective, posted to the team channel: what shipped
    /// vs. what was committed, and a plain-spoken takeaway for next time.
    fn sprint_review_and_retro(
        state: &mut crate::state::ProjectState,
        closing: &crate::state::Sprint,
    ) {
        use coxagent_domain::ticket::Status;
        let is_done = |id: &coxagent_domain::TicketId| {
            state
                .tickets
                .iter()
                .any(|t| t.id() == id && matches!(t.status(), Status::Done | Status::Documented))
        };
        let shipped: Vec<String> = closing
            .committed
            .iter()
            .filter(|id| is_done(id))
            .map(ToString::to_string)
            .collect();
        let carry: Vec<String> = closing
            .committed
            .iter()
            .filter(|id| !is_done(id))
            .map(ToString::to_string)
            .collect();
        let total = closing.committed.len();
        let pct = (shipped.len() * 100).checked_div(total).unwrap_or(100);
        state.post_comment(
            "SM",
            &format!(
                "📋 Sprint {} review — shipped {}/{}: {}.",
                closing.number,
                shipped.len(),
                total,
                if shipped.is_empty() {
                    "nothing this time".to_owned()
                } else {
                    shipped.join(", ")
                }
            ),
            None,
        );
        let takeaway = if carry.is_empty() {
            "Clean sprint — everything committed shipped. Keep the scope realistic and this holds."
                .to_owned()
        } else {
            format!(
                "{} ticket(s) carried over ({}). Likely over-committed — pull a smaller, clearer slice next sprint.",
                carry.len(),
                carry.join(", ")
            )
        };
        state.post_comment(
            "SM",
            &format!(
                "🔄 Sprint {} retro — velocity {pct}%. {takeaway}",
                closing.number
            ),
            None,
        );
        state.log_activity(
            "SM",
            &format!("sprint {} review & retro", closing.number),
            None,
        );
    }

    /// Sprint Planning: announce the goal and the committed tickets, so the plan
    /// is visible rather than implicit.
    fn sprint_planning(state: &mut crate::state::ProjectState, number: u32) {
        let Some(sp) = state.sprint.clone() else {
            return;
        };
        let committed: Vec<String> = sp.committed.iter().map(ToString::to_string).collect();
        state.post_comment(
            "SM",
            &format!(
                "🏃 Sprint {number} planning — goal: {}. Committed {} ticket(s) by priority: {}.",
                sp.goal,
                committed.len(),
                if committed.is_empty() {
                    "backlog empty".to_owned()
                } else {
                    committed.join(", ")
                }
            ),
            None,
        );
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
    #[allow(clippy::too_many_lines)] // a linear sequence of agent phases; splitting hurts readability
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

        // Keep the code map fresh so `.coxagent/REPO_MAP.md` reflects the tree
        // the agents are about to work on (best-effort, token-saver-gated).
        // Leader-only: it writes shared files under the repo.
        if leader {
            self.refresh_codegraph(cycle).await;
            // Scrum: open/roll over the sprint at the start of the cycle.
            self.advance_sprint_if_scrum(cycle).await;

            // BA runs on the first cycle of each period. `(cycle-1) % n == 0` is
            // correct for every n including 1 (unlike `cycle % n == 1`).
            let ba_every = self.config.workflow.ba_every_n_cycles;
            if ba_every > 0 && (cycle - 1) % ba_every == 0 {
                self.report("BA", "proposing features");
                match self.ba().execute().await {
                    Ok(ids) => report.ba_created = ids,
                    Err(e) => report.errors.push(format!("BA: {e}")),
                }
            }

            // Team hygiene: reject any duplicate tickets before design/dev.
            self.dedup_backlog().await;

            // PO lays out the milestone roadmap once the backlog exists.
            self.report("PO", "planning milestones");
            if let Err(e) = self.milestones().execute().await {
                report.errors.push(format!("PO milestones: {e}"));
            }

            // PD establishes the project design system once UI work appears.
            self.report("PD", "designing UX");
            match self.design_system().execute().await {
                Ok(created) => report.design_system_created = created,
                Err(e) => report.errors.push(format!("PD design-system: {e}")),
            }
        }

        // SA designs the next unclaimed feature (per-ticket stage claim inside).
        self.report("SA", "designing");
        match self.sa().execute().await {
            Ok(id) => report.sa_readied = id,
            Err(e) => report.errors.push(format!("SA: {e}")),
        }

        // PD authors UX for a UI ticket SA left pending, taking it to ready.
        match self.pd().execute().await {
            Ok(id) => report.pd_designed = id,
            Err(e) => report.errors.push(format!("PD: {e}")),
        }

        self.report("DEV-BUG", "fixing bugs");
        match self.dev(DevMode::Bug).execute().await {
            Ok(id) => {
                if let Some(tid) = &id {
                    self.commit_for_ticket(tid, "fix").await;
                }
                report.bug_fixed = id;
            }
            Err(e) => report.errors.push(format!("DEV-BUG: {e}")),
        }

        if self.config.workflow.feature_dev_enabled {
            // Before building, make sure the next feature has a clear definition
            // of done — DEV raises unclear tickets and the BA fills them in.
            self.clarify_next_feature().await;
            self.report("DEV-FEATURE", "building feature");
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

        // DOCS documents the next completed feature (per-ticket stage claim).
        self.report("DOCS", "writing docs");
        match self.docs().execute().await {
            Ok(id) => report.documented = id,
            Err(e) => report.errors.push(format!("DOCS: {e}")),
        }

        // Leader-only tail: deploy, whole-build verification, governance, and PR
        // review/merge each act on the shared build/repo and must run once.
        if leader {
            // Deploy after code changes so TEST verifies a running build.
            if (report.feature_done.is_some() || report.bug_fixed.is_some())
                && self.deploy.is_some()
            {
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
                            // A failed deploy must become work, or nothing fixes
                            // it: file it as a high-priority bug (deduped).
                            if !r.success {
                                if let Some(id) = self.file_deploy_bug(&r.summary).await {
                                    report.bugs_filed.push(id);
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => report.errors.push(format!("DEPLOY: {e}")),
                    }
                }
            }

            self.report("TEST", "verifying build");
            match self.test().execute().await {
                Ok(ids) => report.bugs_filed = ids,
                Err(e) => report.errors.push(format!("TEST: {e}")),
            }

            // Governance: architecture-conformance drift becomes tracked bugs.
            match self.conformance().execute().await {
                Ok(mut ids) => report.bugs_filed.append(&mut ids),
                Err(e) => report.errors.push(format!("CONFORMANCE: {e}")),
            }

            // Auto-merge: SA deep-dives open PRs and merges or requests changes.
            self.report("SA", "reviewing PRs");
            self.review_open_prs().await;
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

    /// Turn a failed deploy into a high-priority bug so DEV-BUG will fix it.
    /// Deduped: only one open "Deploy failing" bug exists at a time, refreshed
    /// with the latest error. Returns the new ticket id when one is filed.
    async fn file_deploy_bug(&self, summary: &str) -> Option<TicketId> {
        use coxagent_domain::ticket::{Priority, Status, TicketType};
        const MARKER: &str = "Deploy failing";
        let Ok(state) = self.store.load().await else {
            return None;
        };
        // If an open deploy bug already exists, don't pile on duplicates.
        if state.tickets.iter().any(|t| {
            t.ticket_type() == TicketType::Bug
                && t.status() == Status::Open
                && t.title().starts_with(MARKER)
        }) {
            return None;
        }
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        adder
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: TicketType::Bug,
                title: format!("{MARKER}: {summary}"),
                description: format!(
                    "The docker deploy failed and the container is not running. \
                     Root-cause and fix so `docker compose up -d --build` succeeds.\n\n\
                     Deploy output: {summary}"
                ),
                priority: Priority::High,
                complexity: coxagent_domain::ticket::Complexity::Medium,
                has_ui: false,
                acceptance_criteria: Vec::new(),
            })
            .await
            .ok()
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
        for (dup, orig, title) in &rejected {
            state.post_comment(
                "SM",
                &format!(
                    "Heads up — {dup} duplicates {orig} (\"{title}\"). Rejecting {dup} so we don't \
                     build the same thing twice. BA, please check the backlog before proposing."
                ),
                Some(dup.to_string()),
            );
        }
        state.log_activity(
            "SM",
            &format!("rejected {} duplicate ticket(s)", rejected.len()),
            None,
        );
        let _ = self.store.save(&state).await;
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
        .with_worker(self.worker.clone())
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
        .with_worker(self.worker.clone())
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
            } else if r.system_prompt.contains("Product Owner") {
                r#"[{"name":"MVP","goal":"ship it","target_version":"1.0.0"}]"#.to_owned()
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
        uc.run_cycle(1).await;
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
        uc.run_cycle(1).await;

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
}
