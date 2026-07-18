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

/// How often (in sprints) the SA runs a whole-system architecture review.
const ARCH_REVIEW_EVERY_SPRINTS: u32 = 8;

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
                        // Surface it where the human lives: the team chat.
                        self.notify(
                            "pr_opened",
                            format!(
                                "PR #{} ({id}) is awaiting your review — {}",
                                pr.number, pr.url
                            ),
                        )
                        .await;
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
        // With full merge authority (auto_merge on) the SA owns the queue and
        // works it hard — draining the pile-up — rather than nibbling a few PRs.
        // As a suggestion-only reviewer it stays light. Bounded either way for cost.
        let batch = if auto_merge { 12 } else { 3 };
        for pr in prs.into_iter().rev().take(batch) {
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
                // A conflict comment on a PR is a dead-end — no agent works PRs.
                // Turn it into a chore so a DEV actually rebases/resolves it,
                // draining the pile-up instead of leaving it stuck.
                if !pr.mergeable {
                    self.file_conflict_chore(pr.number, &pr.head).await;
                }
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
            "You are the SA with full merge authority on this pull request. Do a deep code review \
             for correctness, completeness, safety, and architecture fit. You may APPROVE (which \
             merges it) only when ALL hold: the change is functionally correct and will keep the \
             build/tests green after merge; it carries adequate tests for what it changes; and the \
             code is clean (clear naming, no dead code, follows the repo's conventions and the \
             established architecture). If it merely works but is untested, sloppy, or drifts from \
             the architecture, REQUEST_CHANGES with the concrete fixes — a green diff is not enough, \
             the merged code must be good.\n\nPR: {title}\nBranch: {head}\n\nUnified diff:\n```\n\
             {clipped}\n```\n{impact}\nRespond with ONLY JSON: {{\"decision\": \"approve\" | \
             \"request_changes\", \"summary\": \"one short paragraph; if request_changes, list the \
             concrete fixes\"}}.{terse}"
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
        let _ = cycle; // the per-process cycle resets on restart — use the
                       // persistent counter below so sprints keep advancing.
        let len = self.config.workflow.sprint_length_cycles;
        // Migrate: seed the persistent counter from the current sprint's stored
        // (old per-process) cycle the first time, so an in-flight sprint doesn't
        // roll instantly, then advance it once per leader cycle.
        if state.sprint_cycle == 0 {
            state.sprint_cycle = state.sprint.as_ref().map_or(0, |s| s.started_cycle);
        }
        state.sprint_cycle += 1;
        let sc = state.sprint_cycle;
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
        let Some(n) = crate::sprint::advance(&mut state, sc, len) else {
            // No roll this cycle — still persist the bumped counter.
            let _ = self.store.save(&state).await;
            return;
        };
        let _ = prev; // superseded by the richer review below
        let lang = self.config.workflow.language;
        if let Some(cl) = &closing {
            Self::sprint_review_and_retro(&mut state, cl, lang);
        }
        Self::sprint_planning(&mut state, n, lang);
        state.log_activity("SM", "opened sprint", Some(format!("sprint {n}")));
        let goal = state
            .sprint
            .as_ref()
            .map(|s| s.goal.clone())
            .unwrap_or_default();
        let _ = self.store.save(&state).await;
        self.notify("sprint_rolled", format!("Sprint {n} opened — goal: {goal}"))
            .await;
        // Learn: distill one concrete lesson from the closing sprint and keep
        // it — it gets fed back into the agents' prompts so they improve.
        if closing.is_some() {
            self.capture_retro_lesson().await;
        }
        // The team actually talks the plan through (PO/SA/DEV weigh in, SM
        // confirms the commitment) — planning as a ceremony, not an announce.
        self.scrum_planning().await;
        // Every few sprints the SA steps back and reviews the whole architecture,
        // filing refactor tickets and asking the PO to prioritise a hardening
        // sprint before tech debt compounds.
        if n % ARCH_REVIEW_EVERY_SPRINTS == 0 {
            self.architecture_audit(n).await;
            self.docs_audit(n).await;
        }
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

    /// Drive the refactor sprint the SA called for. Returns whether one is active
    /// (so BA stays quiet). When the refactor chores are all done it clears the
    /// mode and announces; while they're open the SA realigns one pending
    /// feature's technical spec to the target architecture.
    async fn maintain_refactor_sprint(&self) -> bool {
        let Ok(state) = self.store.load().await else {
            return false;
        };
        if !state.refactor_mode {
            return false;
        }
        let vi = self.config.workflow.language.is_vi();
        if state.open_refactor_count() == 0 {
            if let Ok(mut s) = self.store.load().await {
                s.refactor_mode = false;
                let msg = if vi {
                    "✅ Refactor sprint hoàn tất — nền tảng đã dọn xong, quay lại làm feature."
                } else {
                    "✅ Refactor sprint complete — the foundation is cleaned up; back to features."
                };
                s.post_comment("SA", msg, None);
                s.log_activity("SA", "refactor sprint complete", None);
                let _ = self.store.save(&s).await;
            }
            return false;
        }
        self.realign_one_spec(vi).await;
        true
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

    /// Run the Sprint Planning ceremony: after the deterministic announce, the
    /// team discusses scope, risk, and capacity, and the SM confirms commitment.
    async fn scrum_planning(&self) {
        self.report("SM", "sprint planning");
        let uc = crate::use_cases::RunPlanningUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language)
        .with_repo_note(self.pr_queue_note().await);
        if let Err(e) = uc.execute().await {
            tracing::warn!("sprint planning: {e}");
        }
    }

    /// A one-line summary of the open-PR queue for planning ("6 open PRs into
    /// main (#80 #82 …), 3 with merge conflicts"), or empty when clean/no forge.
    async fn pr_queue_note(&self) -> String {
        let Some(forge) = &self.forge else {
            return String::new();
        };
        let Ok(prs) = forge.list_open_prs().await else {
            return String::new();
        };
        let target = self.flow_base().to_owned();
        let open: Vec<_> = prs.into_iter().filter(|p| p.base == target).collect();
        if open.is_empty() {
            return String::new();
        }
        let conflicted = open.iter().filter(|p| !p.mergeable).count();
        let ids = open
            .iter()
            .map(|p| format!("#{}", p.number))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "- Open PR queue into {target}: {} PR(s) ({ids}), {conflicted} with merge conflicts.",
            open.len()
        )
    }

    /// Run the Backlog Grooming ceremony: BA/SA/PO refine the top un-ready items
    /// so the backlog is healthy for the next planning.
    async fn scrum_grooming(&self) {
        self.report("SM", "backlog grooming");
        let uc = crate::use_cases::RunGroomingUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language);
        if let Err(e) = uc.execute().await {
            tracing::warn!("backlog grooming: {e}");
        }
    }

    /// Ask the SM to distill ONE concrete, actionable lesson from how the last
    /// sprint went, store it, and post it — closing the learn-and-improve loop.
    async fn capture_retro_lesson(&self) {
        use std::fmt::Write as _;
        let Ok(state) = self.store.load().await else {
            return;
        };
        let mut ctx = String::from("Recent activity + outcomes:\n");
        for a in state.activity.iter().rev().take(20) {
            let _ = writeln!(ctx, "- {}: {}", a.agent, a.action);
        }
        let prior = if state.lessons.is_empty() {
            String::new()
        } else {
            format!(
                "\nLessons already recorded:\n- {}\n",
                state.lessons.join("\n- ")
            )
        };
        let request = AgentRequest {
            role: coxagent_domain::Role::Sm,
            system_prompt: format!(
                "You are the SM running a sprint retrospective. Output ONE concrete, \
                 actionable lesson the team should apply next sprint — a single sentence, \
                 imperative, specific to what actually happened. No preamble.{}",
                self.config.workflow.language.reply_directive()
            ),
            task_prompt: format!(
                "{ctx}{prior}\nWhat is the single most valuable NEW lesson to carry forward? \
                 One sentence."
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(90),
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return;
        };
        let lesson = outcome.stdout.trim().trim_start_matches("- ").to_owned();
        if lesson.is_empty() || !outcome.succeeded() {
            return;
        }
        if let Ok(mut s) = self.store.load().await {
            s.add_lesson(&lesson);
            s.post_comment("SM", &format!("🎓 Retro lesson: {lesson}"), None);
            let _ = self.store.save(&s).await;
        }
    }

    /// Sprint Review + Retrospective, posted to the team channel: what shipped
    /// vs. what was committed, and a plain-spoken takeaway for next time.
    fn sprint_review_and_retro(
        state: &mut crate::state::ProjectState,
        closing: &crate::state::Sprint,
        lang: crate::config::Language,
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
        let shipped_list = if shipped.is_empty() {
            if lang.is_vi() {
                "chưa có gì lần này".to_owned()
            } else {
                "nothing this time".to_owned()
            }
        } else {
            shipped.join(", ")
        };
        let review = if lang.is_vi() {
            format!(
                "📋 Sprint {} review — đã ship {}/{}: {shipped_list}.",
                closing.number,
                shipped.len(),
                total
            )
        } else {
            format!(
                "📋 Sprint {} review — shipped {}/{}: {shipped_list}.",
                closing.number,
                shipped.len(),
                total
            )
        };
        state.post_comment("SM", &review, None);
        let takeaway = match (carry.is_empty(), lang.is_vi()) {
            (true, true) => {
                "Sprint gọn — mọi thứ cam kết đều ship. Giữ phạm vi thực tế thì sẽ duy trì được."
                    .to_owned()
            }
            (true, false) => {
                "Clean sprint — everything committed shipped. Keep the scope realistic and this holds."
                    .to_owned()
            }
            (false, true) => format!(
                "{} ticket bị mang sang ({}). Có thể đã cam kết quá tay — sprint sau lấy phần nhỏ hơn, rõ hơn.",
                carry.len(),
                carry.join(", ")
            ),
            (false, false) => format!(
                "{} ticket(s) carried over ({}). Likely over-committed — pull a smaller, clearer slice next sprint.",
                carry.len(),
                carry.join(", ")
            ),
        };
        state.post_comment(
            "SM",
            &format!(
                "🔄 Sprint {} retro — velocity {pct}%. {takeaway}",
                closing.number // "Sprint N retro" giữ nguyên nhãn cho bộ lọc timeline
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
    fn sprint_planning(
        state: &mut crate::state::ProjectState,
        number: u32,
        lang: crate::config::Language,
    ) {
        let Some(sp) = state.sprint.clone() else {
            return;
        };
        let committed: Vec<String> = sp.committed.iter().map(ToString::to_string).collect();
        let list = if committed.is_empty() {
            if lang.is_vi() {
                "backlog trống".to_owned()
            } else {
                "backlog empty".to_owned()
            }
        } else {
            committed.join(", ")
        };
        let tag = if state.refactor_mode {
            " · 🛠️ REFACTOR SPRINT"
        } else {
            ""
        };
        let post = if lang.is_vi() {
            format!(
                "🏃 Sprint {number} planning{tag} — mục tiêu: {}. Cam kết {} ticket theo ưu tiên: {list}.",
                sp.goal,
                committed.len()
            )
        } else {
            format!(
                "🏃 Sprint {number} planning{tag} — goal: {}. Committed {} ticket(s) by priority: {list}.",
                sp.goal,
                committed.len()
            )
        };
        state.post_comment("SM", &post, None);
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

            // Ops/SRE: ping the deployed app; file a bug + alert on an outage.
            self.ops_monitor().await;

            // Daily standup: every few cycles the SM runs the room — but only when
            // the team actually did something since last time. A standup with no
            // real activity is pure token burn (and reads like noise), so we skip
            // it when the board has been quiet.
            if cycle % 3 == 1 && self.has_recent_activity().await {
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
            if !refactoring && ba_every > 0 && (cycle - 1) % ba_every == 0 {
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

        // SA/PD/DEV/DOCS report "working now" from inside, only after they win
        // the per-ticket claim — so a runner that loses the race (or has nothing
        // to do) shows idle instead of falsely mirroring the busy one.
        match self.sa().execute().await {
            Ok(id) => report.sa_readied = id,
            Err(e) => report.errors.push(format!("SA: {e}")),
        }

        // PD authors UX for a UI ticket SA left pending, taking it to ready.
        match self.pd().execute().await {
            Ok(id) => report.pd_designed = id,
            Err(e) => report.errors.push(format!("PD: {e}")),
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
        if self.config.workflow.feature_dev_enabled && !queue_full {
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

        // DOCS documents the next completed feature (per-ticket stage claim).
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
                    // Hard DoD gate: run the real test suite. A red suite becomes a
                    // high-priority bug (deduped) — deterministic quality, not just
                    // the LLM TEST agent's judgement.
                    match deploy.run_tests(&self.work_dir).await {
                        Ok(r) if r.deployed && !r.success => {
                            if let Some(id) = self.file_test_failure(&r.summary).await {
                                report.bugs_filed.push(id);
                            }
                        }
                        Ok(_) => {}
                        Err(e) => report.errors.push(format!("TESTS: {e}")),
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
    /// Turn a stuck (conflicted) PR into a chore so a DEV rebases and resolves
    /// it — otherwise conflicted PRs pile up forever. Deduped per PR number.
    async fn file_conflict_chore(&self, pr: u64, head: &str) {
        use coxagent_domain::ticket::{Complexity, Priority, TicketType};
        let marker = format!("Resolve merge conflict on PR #{pr}");
        let Ok(state) = self.store.load().await else {
            return;
        };
        if state
            .tickets
            .iter()
            .any(|t| t.title().starts_with(&marker) && t.status() != coxagent_domain::Status::Done)
        {
            return;
        }
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        let Ok(id) = adder
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: TicketType::Chore,
                title: marker,
                description: format!(
                    "PR #{pr} (branch `{head}`) has merge conflicts with the base branch. \
                     Check out `{head}`, rebase it onto the latest base, resolve every conflict \
                     keeping both sides' intended behaviour, run the build/tests, and push so the \
                     PR becomes mergeable."
                ),
                priority: Priority::High,
                complexity: Complexity::Small,
                has_ui: false,
                acceptance_criteria: vec![
                    format!("PR #{pr} is mergeable (no conflicts)"),
                    "No behaviour from either side is lost".to_owned(),
                ],
            })
            .await
        else {
            return;
        };

        // A rebase is mechanical — it needs no architecture. Attach a minimal
        // technical design and ready it directly so a DEV picks it up next cycle
        // instead of waiting a full SA pass. Then post it as a visible ACTION so
        // the raised blocker is tied to concrete, tracked work in the feed.
        if let Ok(mut state) = self.store.load().await {
            if let Some(t) = state.ticket_mut(&id) {
                let design = coxagent_domain::TechnicalDesign {
                    approach: format!(
                        "Rebase `{head}` onto base and resolve conflicts; no design change."
                    ),
                    ..Default::default()
                };
                let _ = t.set_technical_design(coxagent_domain::Role::Sa, design);
                let _ = t.transition_to(coxagent_domain::Role::Sa, coxagent_domain::Status::Ready);
            }
            let action = if self.config.workflow.language.is_vi() {
                format!("🎫 Action: đã tạo {id} — DEV rebase & xử lý merge conflict cho PR #{pr}.")
            } else {
                format!(
                    "🎫 Action: {id} filed — DEV to rebase & resolve merge conflict on PR #{pr}."
                )
            };
            state.post_comment("SM", &action, None);
            let _ = self.store.save(&state).await;
        }
    }

    /// File a High bug when the test-suite DoD gate goes red (deduped on an open
    /// one). Deterministic quality signal from the actual toolchain.
    async fn file_test_failure(&self, summary: &str) -> Option<TicketId> {
        use coxagent_domain::ticket::{Complexity, Priority, Status, TicketType};
        const MARKER: &str = "Tests failing";
        let Ok(state) = self.store.load().await else {
            return None;
        };
        if state.tickets.iter().any(|t| {
            t.ticket_type() == TicketType::Bug
                && t.status() == Status::Open
                && t.title().starts_with(MARKER)
        }) {
            return None;
        }
        let first = summary.lines().next().unwrap_or("test suite is red");
        crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store))
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: TicketType::Bug,
                title: format!("{MARKER}: {first}"),
                description: format!(
                    "The test suite is failing — Definition of Done is not met. Make the tests \
                     pass (fix the code or the test).\n\nOutput:\n{summary}"
                ),
                priority: Priority::High,
                complexity: Complexity::Medium,
                has_ui: false,
                acceptance_criteria: vec!["The full test suite passes".to_owned()],
            })
            .await
            .ok()
    }

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
        // Port clashes are an infra fault, not a code bug: the compose file must
        // map the host port from an env var (the project's assigned port) instead
        // of hardcoding one, so two stacks on one host never fight. Give the agent
        // that specific fix rather than a generic "make it build".
        let low = summary.to_lowercase();
        let is_port = low.contains("already allocated")
            || low.contains("address already in use")
            || low.contains("bind for");
        let host_port = self.config.deploy.host_port;
        let (description, acceptance) = if is_port {
            let port_hint = host_port.map_or_else(
                || "the project's assigned host port".to_owned(),
                |p| format!("host port {p} (this project's assigned port)"),
            );
            (
                format!(
                    "The docker deploy failed because a host port is already in use — an infra \
                     clash, not a code defect. Fix the compose file so every published port maps \
                     from an environment variable defaulting to {port_hint} (e.g. \
                     `\"${{APP_PORT:-<port>}}:<container>\"`), never a hardcoded shared port, so \
                     redeploys and other stacks don't collide. Verify `docker compose up -d \
                     --build` then succeeds.\n\nDeploy output: {summary}"
                ),
                vec![
                    "Published ports come from an env var, not a hardcoded value".to_owned(),
                    "`docker compose up -d --build` succeeds with the container running".to_owned(),
                ],
            )
        } else {
            (
                format!(
                    "The docker deploy failed and the container is not running. \
                     Root-cause and fix so `docker compose up -d --build` succeeds.\n\n\
                     Deploy output: {summary}"
                ),
                Vec::new(),
            )
        };
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        adder
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: TicketType::Bug,
                title: format!("{MARKER}: {summary}"),
                description,
                priority: Priority::High,
                complexity: coxagent_domain::ticket::Complexity::Medium,
                has_ui: false,
                acceptance_criteria: acceptance,
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
    async fn post_daily_digest(&self) {
        let today = crate::state::now_rfc3339()[..10].to_owned();
        let Ok(state) = self.store.load().await else {
            return;
        };
        if state.last_digest_day == today {
            return;
        }
        let first_ever = state.last_digest_day.is_empty();
        let digest = crate::metrics::digest_markdown(&state, &crate::state::now_rfc3339());
        drop(state);
        let posted = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if s.last_digest_day == today {
                return Ok(()); // another operator beat us to it
            }
            s.last_digest_day.clone_from(&today);
            if !first_ever {
                s.post_chat_in(
                    "COX",
                    &format!("📰 {digest}"),
                    crate::state::AGENTS_CHANNEL,
                    Vec::new(),
                );
            }
            Ok(())
        })
        .await;
        if posted.is_ok() && !first_ever {
            tracing::info!("posted daily digest for {today}");
        }
    }

    /// The PR feedback loop: for the newest open PR with unaddressed
    /// change-requests (a review submitted after the branch's last commit), run
    /// a DEV agent that checks out the branch, fixes exactly what the review
    /// asked, and pushes — then replies on the PR and tells the chat. One PR per
    /// cycle, so a review queue drains steadily without a token spike.
    #[allow(clippy::too_many_lines)] // one linear queue-drain pass; splitting hurts readability
    async fn address_pr_feedback(&self) {
        if !self.config.git.enabled {
            return;
        }
        let Some(forge) = &self.forge else { return };
        let Ok(prs) = forge.list_open_prs().await else {
            return;
        };
        let target = self.flow_base();
        // OLDEST first: merging the oldest PR first minimises how many times the
        // rest have to re-resolve — the opposite order feeds the conflict cascade.
        let mut queue: Vec<_> = prs.into_iter().filter(|p| p.base == target).collect();
        queue.sort_by(|a, b| a.created.cmp(&b.created));
        // Normally 1 fix per cycle bounds token cost; under the clean-base gate
        // (refactor waiting on an empty queue) drain twice as fast.
        let mut fix_budget: u32 = if self.clean_base_required().await {
            2
        } else {
            1
        };
        for pr in queue.into_iter().take(6) {
            // What needs fixing? Explicit review feedback, and/or merge conflicts
            // — conflicts are handled IMMEDIATELY, not parked for a review round.
            let feedback = forge.pr_feedback(pr.number).await.unwrap_or_default();
            let conflicted = !pr.mergeable;
            if feedback.is_empty() && !conflicted {
                continue;
            }
            // Ping-pong brake: after 2 fix rounds on one PR, escalate to a human
            // instead of burning tokens on an endless review↔fix loop.
            let attempts = self.store.load().await.map_or(0, |s| {
                s.pr_fix_attempts.get(&pr.number).copied().unwrap_or(0)
            });
            if attempts >= 2 {
                if attempts == 2 {
                    self.notify(
                        "pr_stuck",
                        format!(
                            "PR #{} has been fixed {attempts} times and is still blocked — needs a \
                             human decision: {}",
                            pr.number, pr.url
                        ),
                    )
                    .await;
                    let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                        s.pr_fix_attempts.insert(pr.number, attempts + 1);
                        Ok(())
                    })
                    .await;
                }
                continue;
            }
            self.report("DEV-BUG", &format!("fixing PR #{}", pr.number));
            let mut asks: Vec<String> = feedback
                .iter()
                .map(|f| format!("- ({}) {}", f.author, f.body.trim()))
                .collect();
            if conflicted {
                asks.push(format!(
                    "- (merge-queue) The branch conflicts with `{target}`. Merge the latest \
                     `{target}` INTO this branch (`git fetch origin && git merge origin/{target}`), \
                     resolve every conflict preserving BOTH the branch's fix and what landed on \
                     {target}, and make the build/tests green."
                ));
            }
            let asks = asks.join("\n");
            let request = crate::ports::outbound::AgentRequest {
                role: coxagent_domain::Role::DevBug,
                system_prompt: crate::prompts::system_prompt(crate::prompts::DEV),
                task_prompt: format!(
                    "Pull request #{} (branch `{}`) is blocked and YOU are unblocking the merge \
                     queue.\n\n\
                     WHAT TO FIX:\n{asks}\n\n\
                     Do exactly this:\n\
                     1. `git fetch origin && git checkout {} && git pull origin {}`\n\
                     2. Address the items above — nothing more.\n\
                     3. Run the tests/build to make sure nothing broke.\n\
                     4. `git add -A && git commit -m \"fix: unblock PR #{}\"` \
                        and `git push origin {}`.\n",
                    pr.number, pr.head, pr.head, pr.head, pr.number, pr.head
                ),
                work_dir: self.work_dir.clone(),
                timeout: std::time::Duration::from_secs(1800),
            };
            match self.engine.run(request).await {
                Ok(o) if o.succeeded() => {
                    let _ = forge
                        .comment_pr(
                            pr.number,
                            "🔧 Addressed the review feedback — changes pushed to this branch. \
                             Please take another look.",
                        )
                        .await;
                    self.log_git(&format!("DEV addressed feedback on PR #{}", pr.number))
                        .await;
                    let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                        *s.pr_fix_attempts.entry(pr.number).or_insert(0) += 1;
                        Ok(())
                    })
                    .await;
                    self.notify(
                        "pr_fixed",
                        format!(
                            "PR #{} — review feedback addressed and pushed; ready for another look: {}",
                            pr.number, pr.url
                        ),
                    )
                    .await;
                }
                _ => {
                    self.log_git(&format!("feedback fix on PR #{} failed", pr.number))
                        .await;
                }
            }
            self.report_idle();
            fix_budget -= 1;
            if fix_budget == 0 {
                break;
            }
        }
    }

    /// Whether the open-PR queue blocks NEW branch work. Normally that's the
    /// WIP limit (`git.max_open_prs`); but when the team is about to
    /// RESTRUCTURE (refactor mode, or a sprint goal that says so), the bar is a
    /// CLEAN BASE — every open PR must merge or close first, because branches
    /// cut before a restructure can never merge sanely after it.
    async fn pr_queue_full(&self) -> bool {
        let limit = self.config.git.max_open_prs;
        if !self.config.git.enabled || limit == 0 {
            return false;
        }
        let Some(forge) = &self.forge else {
            return false;
        };
        let Ok(prs) = forge.list_open_prs().await else {
            return false;
        };
        let target = self.flow_base();
        let open = prs.iter().filter(|p| p.base == target).count();
        if open == 0 {
            return false;
        }
        if self.clean_base_required().await {
            tracing::info!("clean-base gate: {open} open PR(s) must merge before refactor work");
            self.announce_drain_hold(open).await;
            return true;
        }
        open >= limit as usize
    }

    /// The SA says the quiet part out loud, once per sprint: the refactor is ON
    /// HOLD until every open PR merges — posted to the Scrum feed AND #agents
    /// so the human sees the plan instead of a silently paused team.
    async fn announce_drain_hold(&self, open: usize) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let sprint_no = state.sprint.as_ref().map_or(0, |s| s.number);
        if state.drain_notice_sprint == sprint_no {
            return;
        }
        drop(state);
        let vi = self.config.workflow.language.is_vi();
        let msg = if vi {
            format!(
                "🏗️ Sprint refactor tạm HOÃN khởi công: còn {open} PR đang mở — refactor trên nền \
                 chưa merge sạch sẽ làm các PR đó không thể merge nổi sau này. Kế hoạch: team dồn \
                 toàn lực merge/đóng hết queue (fix conflict 2 PR/cycle, cũ nhất trước), queue sạch \
                 là refactor bắt đầu ngay. Bạn có thể tự merge các PR xanh trong tab Review để đẩy \
                 nhanh."
            )
        } else {
            format!(
                "🏗️ Refactor sprint ON HOLD: {open} PR(s) still open — restructuring on an \
                 unmerged base would make them unmergeable. Plan: the team drains the whole queue \
                 first (2 conflict-fixes per cycle, oldest first); the refactor starts the moment \
                 it's empty. You can speed this up by merging green PRs in the Review tab."
            )
        };
        let ok = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if s.drain_notice_sprint == sprint_no {
                return Ok(()); // another operator announced first
            }
            s.drain_notice_sprint = sprint_no;
            s.post_comment("SA", &msg, None);
            s.post_chat_in("SA", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            Ok(())
        })
        .await;
        if ok.is_ok() {
            tracing::info!("SA announced clean-base drain hold for sprint {sprint_no}");
        }
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

    /// Run the daily standup: the SM opens, each active agent posts a grounded
    /// update (done/next/blockers), and the SM highlights blockers + focus.
    async fn scrum_standup(&self) {
        self.report("SM", "running standup");
        let uc = crate::use_cases::RunStandupUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language)
        .with_operator(self.worker.clone());
        match uc.execute().await {
            Ok(blockers) if blockers > 0 => {
                tracing::info!("standup surfaced {blockers} blocker(s)");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("standup: {e}"),
        }
    }

    /// Make Scrum lively: when a real tension exists, run a facilitated
    /// discussion (PO & SA weigh in, SM decides, a decision may create a ticket),
    /// posting the whole exchange to the Scrum feed.
    async fn scrum_discussion(&self, report: &CycleReport, cycle: u64) {
        let Ok(state) = self.store.load().await else {
            return;
        };
        let Some(topic) = Self::scrum_topic(&state, report, cycle, self.config.workflow.language)
        else {
            return;
        };
        self.report("SM", "scrum discussion");
        let uc = crate::use_cases::RunDiscussionUseCase::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine),
            self.work_dir.clone(),
        )
        .with_language(self.config.workflow.language);
        if let Err(e) = uc.execute(&topic).await {
            tracing::warn!("scrum discussion: {e}");
        }
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
        .with_phase(self.phase.clone())
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
        .with_phase(self.phase.clone())
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
                trace: String::new(),
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
                trace: String::new(),
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
