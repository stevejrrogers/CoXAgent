//! `RunDevUseCase` — the DEV-BUG and DEV-FEATURE agents.
//!
//! The orchestrator owns claim/release: it atomically claims a ticket
//! (`Ready|Open -> InProgress` as `System`), runs the engine, and on success
//! completes it (`-> Done` / `-> Fixed`). The version never moves here — the
//! release flow owns it. The agent
//! only does the coding; state moves are code, not prompt.

use crate::config::Config;
use crate::error::{AppError, PortError};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::selection::{open_bug_candidates, ready_feature_candidates};
use crate::{prompts, state::ProjectState};
use coxagent_domain::{Role, Status, TicketId};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod briefing;
mod failures;
mod gates;

/// Which developer role to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevMode {
    /// Fix the highest-priority open bug (`Open -> InProgress -> Fixed`).
    Bug,
    /// Implement the next ready feature (`Ready -> InProgress -> Done`).
    Feature,
}

impl DevMode {
    fn role(self) -> Role {
        match self {
            DevMode::Bug => Role::DevBug,
            DevMode::Feature => Role::DevFeature,
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
    /// Git access for the working-tree snapshot the DoD gates read. `None`
    /// (tests, git-less projects) reads as an empty tree — the same answer the
    /// old shell-out gave outside a repo.
    git: Option<Arc<dyn crate::ports::outbound::GitPort>>,
    files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
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
            git: None,
            files: None,
            context: None,
        }
    }

    /// Attach workspace file access, used to stat dirty paths for the green
    /// fingerprint. Without it (tests) the boot-check cache stands aside.
    #[must_use]
    pub fn with_files(
        mut self,
        files: Option<Arc<dyn crate::ports::outbound::WorkspaceFilesPort>>,
    ) -> Self {
        self.files = files;
        self
    }

    /// Attach git access for the gates' working-tree snapshot.
    #[must_use]
    pub fn with_git(mut self, git: Option<Arc<dyn crate::ports::outbound::GitPort>>) -> Self {
        self.git = git;
        self
    }

    /// One snapshot of the uncommitted tree, taken through the port. The gates
    /// all read the SAME snapshot, so they cannot disagree about what changed.
    async fn working_tree(&self) -> crate::ports::outbound::WorkingTreeDiff {
        match &self.git {
            Some(git) => git.working_tree(&self.work_dir).await.unwrap_or_default(),
            None => crate::ports::outbound::WorkingTreeDiff::default(),
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

    /// The dashboard name for this runner's mode.
    fn role_name(&self) -> &'static str {
        match self.mode {
            DevMode::Bug => "DEV-BUG",
            DevMode::Feature => "DEV-FEATURE",
        }
    }

    /// Attach the live "working now" reporter.
    #[must_use]
    pub fn with_phase(mut self, phase: Option<crate::use_cases::runner::PhaseReporter>) -> Self {
        self.phase = phase;
        self
    }

    /// The current tree fingerprint (HEAD + dirty paths with metadata), or
    /// `None` when it cannot be established — the cache then stands aside.
    async fn tree_fingerprint(&self) -> Option<String> {
        let (git, files) = (self.git.as_ref()?, self.files.as_ref()?);
        let (ok, head) = git.raw(&self.work_dir, &["rev-parse", "HEAD"]).await;
        if !ok {
            return None;
        }
        let (ok, status) = git
            .raw(
                &self.work_dir,
                &["status", "--porcelain", "--untracked-files=all"],
            )
            .await;
        if !ok {
            return None;
        }
        let mut dirty: Vec<crate::verify_cache::DirtyEntry> = Vec::new();
        for line in status.lines() {
            // Rename/copy lines (status code R or C) read "old -> new"; the
            // live path — the one whose mtime/size changes on a post-rename
            // edit — is the part after the arrow, not the whole blob. Gate on
            // the status code, not a literal " -> " search: an ordinary
            // path can itself contain that text.
            let is_rename_or_copy = line.get(0..2).is_some_and(|xy| xy.contains(['R', 'C']));
            let meta = match line.get(3..) {
                Some(rest) => {
                    let path = if is_rename_or_copy {
                        rest.rsplit_once(" -> ").map_or(rest, |(_, new)| new)
                    } else {
                        rest
                    };
                    files
                        .stat(&self.work_dir.join(path.trim().trim_matches('"')))
                        .await
                        .map(|m| (m.size, m.modified_epoch))
                }
                None => None,
            };
            dirty.push((line.to_owned(), meta));
        }
        Some(crate::verify_cache::fingerprint(&head, &dirty))
    }

    /// Execute one developer pass.
    ///
    /// # Errors
    /// [`AppError`] on engine failure or an unexpected state transition error.
    #[allow(clippy::too_many_lines)] // one linear pass; splitting hurts readability
    pub async fn execute(&self) -> Result<Option<TicketId>, AppError> {
        // Fresh base for an idle per-slot worktree: it starts DETACHED at
        // whatever HEAD existed when it was created and only ages from there —
        // an agent coding on a ten-commit-old base ships conflicts. When the
        // tree is detached AND clean (nothing in flight to lose), fast-forward
        // it to origin/<base> before claiming. A checked-out branch (the
        // leader's primary tree) is left alone.
        if self.config.git.enabled {
            if let Some(git) = &self.git {
                let detached = !git
                    .raw(&self.work_dir, &["symbolic-ref", "-q", "HEAD"])
                    .await
                    .0;
                if detached && self.working_tree().await.changed_paths.is_empty() {
                    let base = if self.config.git.default_branch.is_empty() {
                        "main"
                    } else {
                        &self.config.git.default_branch
                    };
                    let _ = git.raw(&self.work_dir, &["fetch", "origin", base]).await;
                    let target = format!("origin/{base}");
                    if git.raw(&self.work_dir, &["rev-parse", &target]).await.0 {
                        let _ = git.raw(&self.work_dir, &["reset", "--hard", &target]).await;
                    }
                }
            }
        }
        // Self-healing boot: if the project doesn't compile, fix that BEFORE
        // touching any tickets. Otherwise every ticket will fail anyway.
        // Skipped entirely when the tree is unchanged since the last green
        // suite run (process-wide fingerprint cache) — one green check per
        // tree-state serves every runner in the cycle.
        if let Some(deploy) = &self.verify {
            let fp = self.tree_fingerprint().await;
            // Scratch dirs (`.claude/`, `backups/`, engine config the runner
            // writes itself) show up in `git status` but cannot break a build.
            // Counting them made a clean tree look dirty, forced a full-suite
            // fallback, and the timeout was then misread as a compile break.
            let dirty = gates::build_relevant(&self.working_tree().await.changed_paths);
            // A CLEAN tree has nothing to prove: main is whatever CI and the
            // merge gate already blessed. The old code ran the whole suite
            // anyway, could not finish inside the cap, therefore never
            // recorded green — so every cycle burned the full timeout and no
            // ticket was ever reached. An hour of "working" produced nothing.
            if dirty.is_empty()
                || crate::verify_cache::is_green(&self.work_dir, fp.as_deref())
                // Another runner is already verifying this exact tree state:
                // three concurrent runners used to start three identical
                // `cargo test` compiles that only slowed each other down.
                || !crate::verify_cache::claim_verify(&self.work_dir, fp.as_deref())
            {
                // fall through — nothing changed since the last green run
            } else {
                // The boot check can run for minutes. Without a phase report
                // the dashboard shows nobody working for the whole stretch —
                // exactly the "agents look dead" symptom.
                if let Some(p) = &self.phase {
                    p(Some((
                        self.role_name().to_owned(),
                        format!("boot check: verifying {} changed file(s)", dirty.len()),
                    )));
                }
                let outcome = tokio::time::timeout(
                    // 30 minutes, and SCOPED to the dirty paths: the boot
                    // check exists to catch a broken working tree, not to
                    // re-verify the whole workspace on every cycle. The full
                    // suite still runs on the merged tree and at the sprint
                    // boundary.
                    std::time::Duration::from_secs(1800),
                    deploy.run_tests_scoped(&self.work_dir, &dirty),
                )
                .await;
                // Whatever happened — green, red, spawn error, timeout — the
                // claim is done. Holding it after a failure would wedge the
                // boot check shut for every runner on this tree state.
                crate::verify_cache::release_verify(&self.work_dir, fp.as_deref());
                match outcome {
                    Ok(Ok(r)) if r.success => {
                        crate::verify_cache::mark_green(&self.work_dir, fp.as_deref());
                    }
                    Ok(Ok(r)) => {
                        tracing::warn!(
                            "DEV boot check: cargo test failed — self-healing. {}",
                            &r.summary[..r.summary.len().min(200)]
                        );
                        // One clear point for every self-heal outcome, so the
                        // "boot check" phase note can never outlive the run.
                        let healed = self.self_heal_compile(&r.summary).await;
                        if let Some(p) = &self.phase {
                            p(None);
                        }
                        return healed;
                    }
                    Ok(Err(e)) => {
                        // Spawn errors and timeouts are INFRASTRUCTURE, not compile
                        // breakage — healing on them tells the LLM "the project
                        // doesn't compile" with no compile error to fix. Clear the
                        // phase note on the way out: leaving it set froze the card
                        // at "boot check: verifying N files" long after this run
                        // gave up, which reads as a hung agent.
                        tracing::warn!("DEV boot check: cargo test spawn error — {e}");
                        if let Some(p) = &self.phase {
                            p(None);
                        }
                        return Ok(None);
                    }
                    Err(_timeout) => {
                        tracing::warn!("DEV boot check: cargo test timed out after 30 min");
                        if let Some(p) = &self.phase {
                            p(None);
                        }
                        return Ok(None);
                    }
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
        // Cost approval gate: estimate this role's run cost from the rolling
        // average; a ticket estimated above `approve_over_usd` is HELD for a
        // human instead of silently burning the budget.
        let role_key = match self.mode {
            DevMode::Bug => "dev_bug",
            DevMode::Feature => "dev_feature",
        };
        let estimate = state.spend.avg_role_cost(role_key);
        let mut new_holds: Vec<(String, f64)> = Vec::new();
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
            if let (Some(cap), Some(est)) = (self.config.workflow.approve_over_usd, estimate) {
                if est > cap && !state.cost_approved.contains(cand.as_str()) {
                    if !state.cost_holds.contains_key(cand.as_str()) {
                        new_holds.push((cand.to_string(), est));
                    }
                    continue;
                }
            }
            if self.store.claim_ticket(&cand, &worker, &now).await? {
                // Announce the START in the activity feed: it only ever logged
                // completions, so two DEVs grinding in parallel were invisible
                // in Fleet river until the first one finished — an operator
                // watching the live stream saw an idle team doing work.
                let role_label = self.role_name().to_owned();
                let cid = cand.to_string();
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    s.log_activity(&role_label, "started implementing", Some(cid.clone()));
                    Ok(())
                })
                .await;
                chosen = Some(cand);
                break;
            }
        }
        if !new_holds.is_empty() {
            let cap = self.config.workflow.approve_over_usd.unwrap_or_default();
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                for (id, est) in &new_holds {
                    s.cost_holds.insert(id.clone(), *est);
                    s.post_comment(
                        "SM",
                        &format!(
                            "⏸️ {id} held for cost approval: estimated ~${est:.2}/run                              exceeds the ${cap:.2} gate. Approve it from the ticket to run."
                        ),
                        Some(id.clone()),
                    );
                }
                Ok(())
            })
            .await;
        }
        let Some(id) = chosen else {
            // Nothing claimed: drop any boot-check phase so the dashboard does
            // not keep showing this runner as busy.
            if let Some(p) = &self.phase {
                p(None);
            }
            return Ok(None);
        };
        if let Some(p) = &self.phase {
            p(Some((self.role_name().to_owned(), id.to_string())));
        }

        let state = self.store.load().await?;
        // TDD gate: on a feature with acceptance criteria, a TEST-role call
        // writes FAILING tests from those criteria FIRST — the definition of
        // done becomes machine-checkable before implementation starts. DEV
        // then codes until the suite (including these) is green; the DoD
        // check below enforces it mechanically. Best-effort: a failed TDD
        // call never blocks the ticket.
        if self.config.workflow.tdd && self.mode == DevMode::Feature {
            let criteria: Vec<String> = state
                .ticket(&id)
                .map(|t| t.acceptance_criteria().to_vec())
                .unwrap_or_default();
            if !criteria.is_empty() {
                let title = state.ticket(&id).map_or("", |t| t.title());
                let tdd_req = AgentRequest {
                    role: Role::Test,
                    system_prompt: prompts::system_prompt(prompts::TEST),
                    task_prompt: format!(
                        "TDD: ticket {id} ({title}) is about to be implemented. Write \
                         FAILING tests that encode EXACTLY these acceptance criteria — \
                         nothing else, no implementation, no fixing existing tests:\n- {}\n\
                         Put them where this project keeps tests, as PURE function tests \
                         over the state/domain types that exist in the codebase — never a \
                         fake HTTP server, host harness or network port. Every fixture must \
                         be buildable from data the codebase actually has; if an acceptance \
                         criterion asserts data that does not exist in the codebase, that is \
                         a design gap — do NOT fabricate it, report it. The tests must \
                         COMPILE (no word-salad signatures, no invented identifiers) and \
                         fail only for the missing behaviour. Commit nothing.",
                        criteria.join("\n- ")
                    ),
                    work_dir: self.work_dir.clone(),
                    timeout: Duration::from_secs(900),
                    escalation_level: 0,
                    label: Some(id.to_string()),
                };
                let _ = self.engine.run(tdd_req).await;
            }
        }
        // On ANY engine failure (error or non-zero exit — e.g. a quota wall),
        // release the claim so the ticket returns to the queue instead of being
        // stranded In-Progress forever (which piled up 100+ orphaned tickets and
        // kept burning tokens re-claiming fresh ones).
        // Two-phase expert flow: PLAN first (read the code, commit to steps,
        // name the risks — no code yet), then EXECUTE the plan in the SAME
        // conversation. Thinking is cheap; unplanned code is not. Falls back
        // to single-shot on engines without session resume.
        let mut request = self.build_request(&state, &id).await;
        // Two-phase plan→execute costs an extra engine call per ticket. That
        // buys real risk reduction on a LARGE change — and mostly latency on a
        // small one, where the plan restates the ticket. So: plan-first only
        // for Large complexity, single-shot for the rest (retries already
        // carry a failure journal either way).
        let is_large = state
            .ticket(&id)
            .is_some_and(|t| t.complexity() == coxagent_domain::Complexity::Large);
        let plan_first = request.escalation_level == 0 && is_large;
        // The full task, kept before the plan wrapper below — it becomes the
        // follow-up when RE-ENTERING a ticket on a stored session, so a resumed
        // (or stale) conversation still gets the complete instructions.
        let task_full = request.task_prompt.clone();
        if plan_first {
            request.task_prompt = format!(
                "{}\n\nFIRST: do NOT write code yet. Explore the relevant code (use the repo \
                 map / code-graph tools), then output a concise implementation PLAN: the exact \
                 steps, the files you will touch, the tests you will add, and the biggest risk. \
                 Wait for the follow-up before implementing.",
                request.task_prompt
            );
        }
        // Cross-cycle context reuse: on a RE-ENTRY (a retry, or after a parked
        // question was answered — never the first, planning pass) resume the
        // conversation this ticket+role left behind, so the agent keeps what it
        // already read instead of paying to rediscover it. Resume routes to the
        // role's configured engine; a miss (engine changed, session expired)
        // falls straight back to a cold run — the follow-up is the full task, so
        // the worst case is exactly a cold run.
        let sess_key = format!("{id}/{role_key}");
        let prior_session = if plan_first {
            None
        } else {
            state.ticket_sessions.get(&sess_key).cloned()
        };
        let initial = match prior_session {
            Some(sid) => match self
                .engine
                .resume_run(
                    self.mode.role(),
                    &sid,
                    &task_full,
                    &self.work_dir,
                    Duration::from_secs(3600),
                )
                .await
            {
                Ok(o) if o.succeeded() => Ok(o),
                _ => self.engine.run(request).await,
            },
            None => self.engine.run(request).await,
        };
        // Keep the engine's conversation id: the execute pass and the repair
        // pass (below) resume this session so the agent keeps everything it
        // just read and wrote in context instead of rediscovering it cold.
        let session = match initial {
            Ok(o) if o.succeeded() => {
                // Persist any BRIEF: notes the agent left for the next
                // role/engine on this ticket — durable memory that outlives the
                // engine session (Tầng 2 of per-ticket context reuse).
                let briefs = crate::prompts::extract_brief_notes(&o.stdout);
                if !briefs.is_empty() {
                    let (key, role_tag, briefs) =
                        (id.to_string(), self.role_name().to_owned(), briefs);
                    let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                        for b in &briefs {
                            s.journal_note(&key, &format!("{role_tag}: {b}"));
                        }
                        Ok(())
                    })
                    .await;
                }
                // The engine answered: whatever outage was raised against it is
                // over. Closing it out loud matters as much as raising it — an
                // alert that never clears is one people stop reading.
                let engine = self.engine.id().to_owned();
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    if let Some(inc) = s.close_engine_incident(&engine) {
                        let msg = format!(
                            "✅ {engine} is answering again after {} failed run(s) — {} is \
                             resolved, work resumes.",
                            inc.hits, inc.reason
                        );
                        s.post_chat_in("SYSTEM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                    }
                    Ok(())
                })
                .await;
                // A question is not a failure. If the agent says it would have
                // to guess, park the QUESTION (not the ticket): the BA answers
                // next cycle and the retry starts from an answer instead of an
                // assumption. Counting this as an attempt would punish exactly
                // the behaviour we want.
                if let Some((to, body)) = parse_ask(&o.stdout) {
                    let (key, from) = (id.to_string(), format!("{:?}", self.mode.role()));
                    // A person-addressed question may be held for their
                    // focus-window digest (CXA-F176) instead of landing as
                    // its own interrupt — a pure call over config + clock.
                    let human = self.config.workflow.human.clone();
                    let sla = human.question_sla_minutes;
                    let defer = crate::use_cases::question_batching::should_defer(
                        &human,
                        &to,
                        crate::use_cases::question_batching::now_minutes_utc(),
                        0,
                        sla,
                    );
                    let asked =
                        crate::ports::outbound::mutate_state(self.store.as_ref(), move |st| {
                            if st.ask_question(&key, &from, &to, &body) {
                                if defer {
                                    // The question was just pushed: the tail
                                    // IS the new one, still inside the same
                                    // write pass.
                                    if let Some(q) = st.questions.last_mut() {
                                        q.deferred = true;
                                    }
                                }
                                let msg = format!("❓ {from} → {to}: {body}");
                                st.post_comment(&from, &msg, Some(key.clone()));
                            }
                            Ok(())
                        })
                        .await;
                    if asked.is_ok() {
                        self.release_claim(&id).await;
                        if let Some(p) = &self.phase {
                            p(None);
                        }
                        return Ok(None);
                    }
                }
                let sid = o.session_id.clone();
                match (&sid, plan_first) {
                    (Some(sid_v), true) => {
                        // Phase 2: execute the plan it just committed to.
                        let exec = self
                            .engine
                            .resume_run(
                                self.mode.role(),
                                sid_v,
                                "Plan accepted. Now IMPLEMENT it exactly: follow your steps, \
                                 write the tests you named, and flag (don't silently absorb) \
                                 anything that forces a deviation from the plan.",
                                &self.work_dir,
                                Duration::from_secs(3600),
                            )
                            .await;
                        match exec {
                            Ok(e) if e.succeeded() => sid,
                            _ => {
                                // Resume unsupported/failed: run single-shot fresh
                                // with the plan folded in as a normal task.
                                let fresh = self.build_request(&state, &id).await;
                                match self.engine.run(fresh).await {
                                    Ok(f) if f.succeeded() => f.session_id.clone(),
                                    Ok(f) => {
                                        self.record_failure(&id, &f.failure_detail()).await;
                                        self.release_claim(&id).await;
                                        return Err(PortError::Backend(format!(
                                            "{:?} engine failed on {id}: {}",
                                            self.mode,
                                            f.failure_detail()
                                        ))
                                        .into());
                                    }
                                    Err(e) => {
                                        self.record_failure(&id, &e.to_string()).await;
                                        self.release_claim(&id).await;
                                        return Err(e.into());
                                    }
                                }
                            }
                        }
                    }
                    _ => sid,
                }
            }
            Ok(o) => {
                self.record_failure(&id, &o.failure_detail()).await;
                self.release_claim(&id).await;
                return Err(PortError::Backend(format!(
                    "{:?} engine failed on {id}: {}",
                    self.mode,
                    o.failure_detail()
                ))
                .into());
            }
            Err(e) => {
                self.record_failure(&id, &e.to_string()).await;
                self.release_claim(&id).await;
                return Err(e.into());
            }
        };

        // Remember this run's conversation so a re-entry on this ticket+role can
        // resume it instead of reading the code cold. Best-effort.
        if let Some(sid) = &session {
            let (k, v) = (sess_key.clone(), sid.clone());
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                s.ticket_sessions.insert(k.clone(), v.clone());
                Ok(())
            })
            .await;
        }

        // Expert habit: review your OWN diff before anyone else sees it.
        // Same conversation (context intact) = one cheap pass that catches
        // nits, dead code and missed edge cases. Best-effort.
        if let Some(sid) = &session {
            let _ = self
                .engine
                .resume_run(
                    self.mode.role(),
                    sid,
                    "Before handing off: run `git diff` and review YOUR OWN change like a \
                     principal engineer reviewing a stranger's PR. Fix what you find — dead \
                     code, debug leftovers, missed edge cases, naming, missing tests for new \
                     logic. Do NOT start new work or commit.",
                    &self.work_dir,
                    Duration::from_secs(900),
                )
                .await;
        }

        // Mechanical Definition of Done: the suite must be GREEN after the
        // change. Red → one bounded repair pass fed the failure output; still
        // red → the ticket is NOT done (claim released, failure recorded)
        // instead of shipping a broken build for TEST to rediscover later.
        if let Some(deploy) = &self.verify {
            let failed = |r: &crate::ports::outbound::DeployReport| !r.success;
            // Per-ticket gate: only what this change can reach. The full
            // suite still runs at the sprint boundary and on the merged tree
            // (docs/ADAPTIVE_APPROVAL.md's sibling rule for tests).
            let changed = gates::build_relevant(&self.working_tree().await.changed_paths);
            let mut red = match deploy.run_tests_scoped(&self.work_dir, &changed).await {
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
                        .resume_run(
                            self.mode.role(),
                            sid,
                            &follow_up,
                            &self.work_dir,
                            Duration::from_secs(1800),
                        )
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
                        escalation_level: 0,
                        label: Some(id.to_string()),
                    };
                    let _ = self.engine.run(repair).await;
                }
                red = match deploy.run_tests_scoped(&self.work_dir, &changed).await {
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

            // Lint gate: a change may never ADD clippy errors. Baseline is
            // learned on first measure and ratchets DOWN when improved.
            if let Ok(Some(report)) = deploy.lint_report(&self.work_dir).await {
                let count = report.errors;
                let prior = self.store.load().await.ok().and_then(|s| s.clippy_baseline);
                match prior {
                    None => {
                        let _ =
                            crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                                s.clippy_baseline = Some(count);
                                Ok(())
                            })
                            .await;
                    }
                    Some(base) if count > base => {
                        // One bounded repair pass for the NEW lint errors only.
                        // Naming the actual lints beats a bare count: the agent
                        // can fix them without hunting through the whole run.
                        let detail = if report.sample.is_empty() {
                            String::new()
                        } else {
                            format!("\nCurrent errors include:\n{}", report.sample)
                        };
                        let fixup = format!(
                            "Your change introduced NEW `cargo clippy` errors (was {base}, now \
                             {count}). Run `cargo clippy --workspace --all-targets`, fix ONLY \
                             errors caused by your change, and do not start new work. If a lint \
                             fires on code you must keep (e.g. platform-gated symbols), gate it \
                             with the right #[cfg(...)] rather than deleting behaviour.{detail}"
                        );
                        if let Some(sid) = &session {
                            let _ = self
                                .engine
                                .resume_run(
                                    self.mode.role(),
                                    sid,
                                    &fixup,
                                    &self.work_dir,
                                    Duration::from_secs(900),
                                )
                                .await;
                        } else {
                            let repair = AgentRequest {
                                role: self.mode.role(),
                                system_prompt: prompts::system_prompt(prompts::DEV),
                                task_prompt: fixup,
                                work_dir: self.work_dir.clone(),
                                timeout: Duration::from_secs(900),
                                escalation_level: 0,
                                label: Some(id.to_string()),
                            };
                            let _ = self.engine.run(repair).await;
                        }
                        // The repair pass just edited code again — the earlier
                        // green check is stale. Re-confirm the suite before
                        // trusting this tree: skipping this let a lint fixup
                        // silently break tests, and the green-cache below
                        // would then mark_green() over a red suite for every
                        // sibling runner this cycle (COX-B033).
                        if let Ok(r) = deploy.run_tests_scoped(&self.work_dir, &changed).await {
                            if !r.success {
                                self.record_failure_at(
                                    &id,
                                    "clippy repair pass left the test suite red",
                                    crate::state::FailureLayer::Gate,
                                    "tests",
                                    Vec::new(),
                                )
                                .await;
                                self.release_claim(&id).await;
                                return Err(PortError::Backend(format!(
                                    "{:?} clippy repair broke tests on {id} — ticket returned \
                                     to the queue",
                                    self.mode
                                ))
                                .into());
                            }
                        }
                        let after_report = deploy.lint_report(&self.work_dir).await.ok().flatten();
                        let after = after_report.as_ref().map_or(count, |r| r.errors);
                        // Blame only what this change touched. The workspace
                        // carries pre-existing lints, and a rebase can import
                        // someone else's — failing the holder of the ticket for
                        // those parks perfectly good fixes after three tries.
                        // MSRV 1.80 predates Option::is_none_or.
                        let tree = self.working_tree().await;
                        let mine = after_report.as_ref().map_or(true, |r| {
                            r.files.is_empty() || gates::lints_touch_changed_files(&tree, &r.files)
                        });
                        if after > base && mine {
                            // Keep the files the lints named: the next attempt
                            // is told where to look, not just that it failed.
                            let lint_files = after_report
                                .as_ref()
                                .map(|r| {
                                    let mut f: Vec<String> = r
                                        .files
                                        .iter()
                                        .filter(|f| !f.trim().is_empty())
                                        .cloned()
                                        .collect();
                                    f.sort();
                                    f.dedup();
                                    f.truncate(5);
                                    f
                                })
                                .unwrap_or_default();
                            let sample = after_report
                                .filter(|r| !r.sample.is_empty())
                                .map_or_else(String::new, |r| {
                                    format!("; e.g. {}", r.sample.lines().next().unwrap_or(""))
                                });
                            self.record_failure_at(
                                &id,
                                &format!("added clippy errors ({base} -> {after}{sample})"),
                                crate::state::FailureLayer::Gate,
                                "clippy",
                                lint_files,
                            )
                            .await;
                            self.release_claim(&id).await;
                            return Err(PortError::Backend(format!(
                                "{:?} added lint errors on {id} — ticket returned to the queue",
                                self.mode
                            ))
                            .into());
                        }
                        if after > base {
                            // Not ours: record the new reality so the next
                            // change isn't measured against a stale baseline.
                            tracing::warn!(
                                "lint count rose {base} -> {after} but no new lint sits in a \
                                 file {id} touched — not attributing it to this ticket"
                            );
                            let _ = crate::ports::outbound::mutate_state(
                                self.store.as_ref(),
                                move |s| {
                                    s.clippy_baseline = Some(after);
                                    Ok(())
                                },
                            )
                            .await;
                        }
                        if after < base {
                            let _ = crate::ports::outbound::mutate_state(
                                self.store.as_ref(),
                                move |s| {
                                    s.clippy_baseline = Some(after);
                                    Ok(())
                                },
                            )
                            .await;
                        }
                    }
                    Some(base) if count < base => {
                        let _ =
                            crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                                s.clippy_baseline = Some(count);
                                Ok(())
                            })
                            .await;
                    }
                    Some(_) => {}
                }
            }

            // Platform gate: this host is macOS, the product ships on Linux.
            // A symbol gated to the wrong platforms compiles clean here and is
            // dead code there — the failure cox filed three times under three
            // different ticket numbers. The check provisions what it needs and
            // falls back to the Docker build, so an unavailable answer is a
            // real gap, not laziness, and it is stated rather than skipped.
            match deploy.cross_target_check(&self.work_dir).await {
                Ok(check) if check.available && !check.errors.is_empty() => {
                    let detail = check.errors.join("; ");
                    let short: String = detail.chars().take(200).collect();
                    self.record_failure_at(
                        &id,
                        &format!("does not compile for the deploy platform: {short}"),
                        crate::state::FailureLayer::Gate,
                        "linux-build",
                        Vec::new(),
                    )
                    .await;
                    self.release_claim(&id).await;
                    return Err(PortError::Backend(format!(
                        "{:?} broke the Linux build on {id} — ticket returned to the queue",
                        self.mode
                    ))
                    .into());
                }
                Ok(check) if !check.available => {
                    tracing::warn!("platform verification unavailable — {}", check.reason);
                }
                Ok(_) | Err(_) => {}
            }

            // Phantom-bug guard: a BUG ticket that ends with NO build-affecting
            // change against an already-green tree is not a reproducible bug —
            // typically one already fixed by a merged change, or a report that
            // never matched main. The suite is green here by construction
            // (every earlier gate already returned on red), so the empty diff
            // is the genuine "no bug exists on main" signal. Previously the
            // regression-test gate below demanded a test that "fails without
            // your fix" — impossible when there is no fix — and the ticket was
            // re-queued to burn a full investigation every sprint (the
            // CXA-B002/B003/B004 loop). Close it Rejected with a finding, via the
            // orchestrator's `System` role (a transition DEV is not allowed to
            // make), so it leaves `open_bug_candidates` and stays in history.
            let tree = self.working_tree().await;
            if self.mode == DevMode::Bug && gates::build_relevant(&tree.changed_paths).is_empty() {
                let msg = format!(
                    "{id}: not reproducible — DEV ran against a green tree and produced no \
                     code change. The bug does not reproduce on main (likely already resolved \
                     by a merged fix). Closing as not-reproducible so the sprint stops \
                     re-investigating it."
                );
                let id_c = id.clone();
                crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    transition(s, &id_c, Role::System, Status::Rejected)
                        .map_err(|e| PortError::Corrupt(e.to_string()))?;
                    s.post_comment("SYSTEM", &msg, Some(id_c.to_string()));
                    s.ticket_journal.remove(&id_c.to_string());
                    s.cost_holds.remove(&id_c.to_string());
                    s.cost_approved.remove(&id_c.to_string());
                    Ok(())
                })
                .await?;
                tracing::info!(
                    "DEV bug pass: {id} closed not-reproducible (no change on green tree)"
                );
                if let Some(p) = &self.phase {
                    p(None);
                }
                return Ok(Some(id));
            }
            if self.mode == DevMode::Bug
                && !gates::diff_is_docs_only(&tree)
                && !gates::diff_touches_tests(&tree)
            {
                let fixup = format!(
                    "Your fix for {id} ships with NO regression test. Add a test that FAILS \
                     without your fix and passes with it — that is the only proof the bug is \
                     dead. Put it where the gate can see it: a `tests/` path, a `*_test.rs` / \
                     `*.test.ts` file, or an added `#[test]`/`#[tokio::test]`/`it(`/`def test_` \
                     block. Do not start new work or commit."
                );
                if let Some(sid) = &session {
                    let _ = self
                        .engine
                        .resume_run(
                            self.mode.role(),
                            sid,
                            &fixup,
                            &self.work_dir,
                            Duration::from_secs(900),
                        )
                        .await;
                } else {
                    let repair = AgentRequest {
                        role: self.mode.role(),
                        system_prompt: prompts::system_prompt(prompts::DEV),
                        task_prompt: fixup,
                        work_dir: self.work_dir.clone(),
                        timeout: Duration::from_secs(900),
                        escalation_level: 0,
                        label: Some(id.to_string()),
                    };
                    let _ = self.engine.run(repair).await;
                }
                if !gates::diff_touches_tests(&self.working_tree().await) {
                    self.record_failure_at(
                        &id,
                        "bug fix shipped without a regression test",
                        crate::state::FailureLayer::Gate,
                        "regression-test",
                        Vec::new(),
                    )
                    .await;
                    self.release_claim(&id).await;
                    return Err(PortError::Backend(format!(
                        "{:?} fix for {id} has no regression test — returned to the queue",
                        self.mode
                    ))
                    .into());
                }
                // Suite must STILL be green with the new test in place.
                if let Ok(r) = deploy.run_tests_scoped(&self.work_dir, &changed).await {
                    if !r.success {
                        self.record_failure_at(
                            &id,
                            "regression test added but suite is red",
                            crate::state::FailureLayer::Gate,
                            "tests",
                            Vec::new(),
                        )
                        .await;
                        self.release_claim(&id).await;
                        return Err(PortError::Backend(format!(
                            "{:?} regression test left suite red on {id}",
                            self.mode
                        ))
                        .into());
                    }
                }
            }
        }

        // The suite (plus gates) is green against this exact tree — remember
        // it so sibling runners skip their boot check this cycle.
        if self.verify.is_some() {
            let fp = self.tree_fingerprint().await;
            crate::verify_cache::mark_green(&self.work_dir, fp.as_deref());
        }

        // Complete under an atomic read-modify-write with retry: move to the
        // terminal status and record the deploy. A concurrent operator saving
        // the shared state can't make us lose this completion (which would
        // strand the ticket and waste tokens redoing it).
        let (role, status, id_c) = (self.mode.role(), self.mode.complete_status(), id.clone());
        crate::ports::outbound::mutate_state(self.store.as_ref(), |state| {
            transition(state, &id_c, role, status)
                .map_err(|e| PortError::Corrupt(e.to_string()))?;
            // Done — the work journal and any cost hold served their purpose.
            // The resumable session is kept: DEV reaching Done/Fixed is NOT the
            // end of the ticket — a review send-back re-enters DEV, and resuming
            // the pre-Done conversation there is exactly the context-reuse win.
            // The session is dropped only when the PR actually merges (see
            // forge_merge's merged-PR sync).
            state.ticket_journal.remove(&id_c.to_string());
            state.cost_holds.remove(&id_c.to_string());
            state.cost_approved.remove(&id_c.to_string());
            // The version does NOT move here. A ticket completing locally is
            // not a release: bumping before the PR even merged minted phantom
            // versions that reconcile_version then had to claw back. The
            // version is owned by the release flow (manifest on main is the
            // single source; state only mirrors it) — the deploy record below
            // simply stamps the version the tree currently declares.
            let version = state.current_version.clone();
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

    /// When pre-check fails, ask the LLM to fix ALL compile/test errors until
    /// the codebase is green or we hit the retry cap. This runs BEFORE any
    /// tickets are touched — infrastructure repair, not feature work.
    async fn self_heal_compile(&self, error_summary: &str) -> Result<Option<TicketId>, AppError> {
        use crate::prompts;

        let Some(deploy) = &self.verify else {
            return Ok(None);
        };

        // Photo of the tree BEFORE the LLM touches it. If healing gives up, we
        // restore this snapshot so the next cycle's boot check starts from a
        // known-green tree instead of re-failing on the same broken edits —
        // that re-fail/re-heal loop is exactly what strands a sprint for hours.
        // Note the codebase is expected to be a clean git checkout at boot; if
        // it is NOT a repo we simply skip the restore (nothing to restore to).
        let checkpoint = if let Some(g) = self.git.as_ref() {
            g.head_sha(&self.work_dir).await.ok()
        } else {
            None
        };

        let mut last_error = error_summary.to_owned();
        for attempt in 1_u32..=3 {
            let task = if attempt == 1 {
                format!(
                    "The project does NOT compile. Fix ALL errors:\n\n\
                     ```\n{last_error}\n```\n\n{}\
                     Run `cargo check`, fix every error, then `cargo test` to verify.",
                    stub_hint(&last_error)
                )
            } else {
                format!(
                    "Still not compiling. Last test output:\n\n\
                     ```\n{last_error}\n```\n\n\
                     {}, Fix the remaining errors. Check what you missed.",
                    stub_hint(&last_error)
                )
            };

            let req = AgentRequest {
                role: Role::DevBug,
                // Through the house wrapper: BASE + engineering standards +
                // process law ride along, and the prefix stays cache-stable.
                system_prompt: prompts::system_prompt(prompts::DEV_HEAL),
                task_prompt: task,
                work_dir: self.work_dir.clone(),
                timeout: std::time::Duration::from_secs(600),
                escalation_level: u8::try_from(attempt.saturating_sub(1)).unwrap_or(3),
                label: None,
            };

            match self.engine.run(req).await {
                Ok(o) if o.succeeded() => {
                    tracing::info!("DEV self-heal attempt {attempt}: LLM OK, verifying…");
                }
                Ok(o) => {
                    tracing::warn!(
                        "DEV self-heal attempt {attempt}: LLM failed — {}",
                        o.stderr.chars().take(200).collect::<String>()
                    );
                    break;
                }
                Err(e) => {
                    tracing::warn!("DEV self-heal attempt {attempt}: engine error — {e}");
                    break;
                }
            }

            // Verify: is the codebase green now?
            match tokio::time::timeout(
                std::time::Duration::from_secs(1800),
                deploy.run_tests(&self.work_dir),
            )
            .await
            {
                Ok(Ok(r)) if r.success => {
                    tracing::info!("DEV self-heal: codebase GREEN after {attempt} attempt(s)!");
                    return Ok(None);
                }
                Ok(Ok(r)) => {
                    last_error = r.summary;
                    tracing::warn!(
                        "DEV self-heal attempt {attempt}: still red — {}",
                        &last_error[..last_error.len().min(150)]
                    );
                }
                Ok(Err(e)) => {
                    tracing::warn!("DEV self-heal verify error: {e}");
                    break;
                }
                Err(_) => {
                    tracing::warn!("DEV self-heal verify timed out");
                    break;
                }
            }
        }

        tracing::warn!("DEV self-heal: gave up after max retries");
        // Un-stick the next cycle: throw away whatever the LLM left half-done so
        // the boot check starts from the checkpointed tree again, not from the
        // still-red edits. Without this the same failure recurs every cycle and
        // no ticket ever works.
        if checkpoint.is_some() {
            self.restore_worktree_to_head().await;
        }
        Ok(None)
    }

    /// Restore the working tree to its recorded HEAD — discard dirty tracked
    /// edits and untracked files the self-heal pass created. Only called on a
    /// git checkout, which the boot path guarantees. Safe when there is nothing
    /// to discard.
    async fn restore_worktree_to_head(&self) {
        let Some(git) = self.git.as_ref() else {
            return;
        };
        let (checked_out, _) = git.raw(&self.work_dir, &["checkout", "."]).await;
        let (cleaned, _) = git.raw(&self.work_dir, &["clean", "-fd"]).await;
        tracing::warn!(
            "DEV self-heal: restored worktree to HEAD (checkout={checked_out}, clean={cleaned})"
        );
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
        let mut ids = match self.mode {
            DevMode::Bug => open_bug_candidates(state),
            DevMode::Feature => ready_feature_candidates(state),
        };
        // Real-world scope gate: DEV only pulls tickets the team committed to
        // the current sprint (PO/SM aligned via the sprint-board action), plus
        // emergency open bugs. In Kanban mode (no sprint open) any ready
        // ticket stays in scope. A feature/chore the PO/SM has not committed
        // to an open sprint is out of scope — DEV must ask to have it added
        // before picking it up.
        ids.retain(|id| crate::selection::in_dev_scope(state, id));
        ids
    }
}

/// A targeted hint for the self-heal prompt when the unresolved failure is a
/// leftover stub — `unimplemented!()`, `todo!()`, `unreachable!()`, or a
/// "not implemented" panic. These are the classic "agent left half-finished
/// work behind" marker that otherwise spins the boot check in circles: the LLM
/// keeps "fixing" the file but the stub repanics at runtime. Naming the pattern
/// tells it to either implement the body or remove the failing stub path.
fn stub_hint(error_summary: &str) -> String {
    let mk = ["unimplemented!", "todo!", "unreachable!", "not implemented"];
    if mk.iter().any(|m| error_summary.contains(m)) {
        return "One or more errors are a leftover stub (`unimplemented!`/`todo!`/\
                 `not implemented`). For each stub: either implement its real body NOW,\
                 or if it is untested scaffolding, remove the stub call so the suite is\
                 green — a stub must never block the whole build.\n\n"
            .to_owned();
    }
    String::new()
}

/// The full working brief for a ticket, BOUNDED: the description (what & why),
/// the acceptance criteria, and the SA's technical design — so the DEV builds
/// what was specified instead of guessing from the title (and the SA's design
/// tokens aren't wasted). Caps keep a verbose ticket from bloating the prompt.
/// Pull an `ASK <ROLE>: <question>` line out of agent output. Shared with the
/// cycle, where an answerer uses the same line to hand a question on. Returns the role
/// to ask and the question. Only BA and SA can be asked: those are the roles
/// that own the requirement and the design.
#[must_use]
pub fn parse_ask(stdout: &str) -> Option<(String, String)> {
    for line in stdout.lines().rev().take(20) {
        let t = line.trim().trim_start_matches(['`', '*', '-', ' ']);
        // `?` here would abandon the scan at the first ordinary line, so the
        // loop would only ever see the very last one.
        let Some(rest) = t.strip_prefix("ASK ") else {
            continue;
        };
        let Some((role, question)) = rest.split_once(':') else {
            continue;
        };
        let role = role.trim().to_uppercase();
        // `ASK @username:` is the agent → human hop (docs/HYBRID_TEAM.md):
        // the question enters that person's inbox with an SLA. The username
        // must be a single token — anything else is not an addressee.
        let is_person = role.len() > 1
            && role.starts_with('@')
            && role[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !matches!(role.as_str(), "BA" | "SA") && !is_person {
            continue;
        }
        let q = question.trim();
        if q.len() < 10 {
            continue;
        }
        return Some((role, q.to_owned()));
    }
    None
}

#[must_use]
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
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
    use crate::selection::next_ready_feature;
    use coxagent_domain::{Complexity, Priority, TechnicalDesign, Ticket, TicketType};
    use std::sync::Mutex;

    #[test]
    fn stub_hint_targets_leftover_stub_markers() {
        assert!(stub_hint("panicked: not implemented: release pipeline").contains("leftover stub"));
        assert!(stub_hint("unimplemented!()").contains("leftover stub"));
        // A real compile error gets no stub guidance.
        assert_eq!(stub_hint("error[E0308]: mismatched types"), "");
    }

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
                sandbox: SandboxStatus::default(),
                engine: String::new(),
                model: String::new(),
                attempts: Vec::new(),
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
    async fn feature_dev_completes_without_touching_the_version() {
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
        // The version does NOT move on ticket completion — releases own it.
        assert_eq!(state.current_version.to_string(), "0.0.0");
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

    fn open_bug(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Bug,
            "b",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("t")
    }

    /// Green on the pre-lint check, then RED once the clippy repair pass
    /// edits the code a second time — the exact sequence COX-B033 covers:
    /// a lint-fixup that quietly breaks a test must not slip past the gate.
    struct ClippyRepairBreaksTests {
        scoped_calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::ports::outbound::DeployPort for ClippyRepairBreaksTests {
        async fn deploy(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            Ok(crate::ports::outbound::DeployReport {
                failure_bundle: None,
                success: true,
                deployed: false,
                summary: String::new(),
            })
        }
        async fn run_tests_scoped(
            &self,
            _work_dir: &std::path::Path,
            _changed: &[String],
        ) -> Result<crate::ports::outbound::DeployReport, PortError> {
            let call = self
                .scoped_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::ports::outbound::DeployReport {
                failure_bundle: None,
                success: call == 0,
                deployed: true,
                summary: if call == 0 {
                    "green".to_owned()
                } else {
                    "still broken after clippy repair".to_owned()
                },
            })
        }
        async fn lint_report(
            &self,
            _work_dir: &std::path::Path,
        ) -> Result<Option<crate::ports::outbound::LintReport>, PortError> {
            Ok(Some(crate::ports::outbound::LintReport {
                errors: 5,
                sample: String::new(),
                files: Vec::new(),
            }))
        }
    }

    #[tokio::test]
    async fn clippy_repair_pass_is_reverified_against_the_test_suite() {
        let mut state = ProjectState {
            tickets: vec![open_bug("BUG-001")],
            ..ProjectState::default()
        };
        state.clippy_baseline = Some(0); // so the first lint measurement (5) reads as a regression
        let store = Arc::new(MemStore {
            state: Mutex::new(state),
        });
        let deploy = Arc::new(ClippyRepairBreaksTests {
            scoped_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let uc = RunDevUseCase::new(
            Arc::clone(&store),
            Arc::new(OkEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            DevMode::Bug,
        )
        .with_verify(Some(deploy));

        let err = uc.execute().await.expect_err("repair broke tests");
        assert!(err.to_string().contains("clippy repair"), "{err}");

        // The ticket must be back in the queue, not silently marked done.
        let state = store.load().await.expect("load");
        assert_eq!(state.tickets[0].status(), Status::Open);
    }

    #[test]
    fn parse_ask_finds_the_question_anywhere_near_the_end() {
        use super::parse_ask;
        // Real output ends with prose; the ASK line is not guaranteed last.
        let out = "I read the ticket and the code.\n\
                   ASK BA: does 'archive' mean soft-delete or move to cold storage?\n\
                   I stopped rather than guess.";
        let (to, q) = parse_ask(out).expect("question found");
        assert_eq!(to, "BA");
        assert!(q.starts_with("does 'archive' mean"), "{q}");
        // Only the two roles that own requirement and design can be asked.
        assert!(parse_ask("ASK TEST: is this covered?").is_none());
        // A bare marker with no real question is not a question.
        assert!(parse_ask("ASK BA: ?").is_none());
        assert!(parse_ask("no question here").is_none());
    }

    #[test]
    fn parse_ask_reaches_a_person_by_at_username() {
        use super::parse_ask;
        // The agent → human hop (docs/HYBRID_TEAM.md): a question the
        // answering role cannot ground goes to a named person's inbox.
        let (to, q) =
            parse_ask("ASK @luffy: the customer decided archive semantics verbally — soft delete?")
                .expect("person question");
        assert_eq!(to, "@LUFFY");
        assert!(q.starts_with("the customer decided"), "{q}");
        // Not an addressee: bare marker, whitespace in the name, or empty.
        assert!(parse_ask("ASK @: is this a question?").is_none());
        assert!(parse_ask("ASK @luffy zoro: shared question?").is_none());
        // The role-level hop is untouched.
        assert!(parse_ask("ASK SA: how does the store behave?").is_some());
    }

    /// An engine whose only act is to ask a person — the run must park the
    /// ticket on the question, not on a failure.
    struct AskEngine;
    #[async_trait::async_trait]
    impl AgentEnginePort for AskEngine {
        fn id(&self) -> &'static str {
            "ask"
        }
        async fn run(&self, _r: AgentRequest) -> Result<AgentOutcome, PortError> {
            Ok(AgentOutcome {
                stdout: "ASK @luffy: the customer decided archive semantics verbally — soft \
                         delete or purge?"
                    .to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                usage: None,
                trace: String::new(),
                session_id: None,
                sandbox: SandboxStatus::default(),
                engine: String::new(),
                model: String::new(),
                attempts: Vec::new(),
            })
        }
    }

    /// A focus window that is active RIGHT NOW, whatever time the test runs
    /// (wraps midnight cleanly for the last two minutes of the day).
    fn window_covering_now() -> String {
        let now = crate::use_cases::question_batching::now_minutes_utc();
        let end = (now + 2) % (24 * 60);
        format!(
            "{:02}:{:02}-{:02}:{:02}",
            now / 60,
            now % 60,
            end / 60,
            end % 60
        )
    }

    fn store_with_ready_feature() -> Arc<MemStore> {
        Arc::new(MemStore {
            state: Mutex::new(ProjectState {
                tickets: vec![ready_feature("FEAT-001")],
                ..ProjectState::default()
            }),
        })
    }

    // (AC1) With a focus window configured for the addressee, a new
    // person-addressed question is queued (deferred) instead of landing as
    // its own interrupt.
    #[tokio::test]
    async fn a_person_question_inside_their_focus_window_is_held_for_the_digest() {
        let mut config = Config::default();
        config.workflow.human.focus_windows.insert(
            "luffy".to_owned(),
            crate::config::FocusWindow {
                window_utc: window_covering_now(),
                defer_to_digest: true,
            },
        );
        let store = store_with_ready_feature();
        let uc = RunDevUseCase::new(
            Arc::clone(&store),
            Arc::new(AskEngine),
            config,
            PathBuf::from("/tmp"),
            DevMode::Feature,
        );
        // The ask parks the run: nothing was "done", the question waits.
        assert!(uc.execute().await.expect("run").is_none());
        let state = store.load().await.expect("load");
        let q = &state.questions[0];
        assert_eq!(q.to, "@LUFFY");
        assert!(q.deferred, "held for the owner's focus-window digest");
        assert!(!q.escalated, "the window, not the SLA, is what holds it");
    }

    // (AC boundary) Without a window the same question delivers immediately —
    // today's behaviour, unchanged.
    #[tokio::test]
    async fn without_a_focus_window_a_person_question_delivers_immediately() {
        let store = store_with_ready_feature();
        let uc = RunDevUseCase::new(
            Arc::clone(&store),
            Arc::new(AskEngine),
            Config::default(),
            PathBuf::from("/tmp"),
            DevMode::Feature,
        );
        assert!(uc.execute().await.expect("run").is_none());
        let state = store.load().await.expect("load");
        assert!(!state.questions[0].deferred);
    }

    /// Shells to the real `git` binary — `tree_fingerprint` parses actual
    /// `git status --porcelain` output, which the pure `verify_cache` unit
    /// tests never exercise.
    struct RealGit;
    #[async_trait::async_trait]
    impl crate::ports::outbound::GitPort for RealGit {
        async fn raw(&self, work_dir: &std::path::Path, args: &[&str]) -> (bool, String) {
            match std::process::Command::new("git")
                .current_dir(work_dir)
                .args(args)
                .output()
            {
                Ok(out) => (
                    out.status.success(),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                ),
                Err(_) => (false, String::new()),
            }
        }
        async fn is_repo(&self, _work_dir: &std::path::Path) -> bool {
            unimplemented!("unused by tree_fingerprint")
        }
        async fn current_branch(&self, _work_dir: &std::path::Path) -> Result<String, PortError> {
            unimplemented!("unused by tree_fingerprint")
        }
        async fn checkout_branch(
            &self,
            _work_dir: &std::path::Path,
            _branch: &str,
        ) -> Result<(), PortError> {
            unimplemented!("unused by tree_fingerprint")
        }
        async fn commit_all(
            &self,
            _work_dir: &std::path::Path,
            _message: &str,
            _author: &crate::ports::outbound::GitAuthor,
        ) -> Result<Option<String>, PortError> {
            unimplemented!("unused by tree_fingerprint")
        }
        async fn push(&self, _work_dir: &std::path::Path, _branch: &str) -> Result<(), PortError> {
            unimplemented!("unused by tree_fingerprint")
        }
        async fn sync_base(
            &self,
            _work_dir: &std::path::Path,
            _base: &str,
        ) -> Result<crate::ports::outbound::SyncBase, PortError> {
            unimplemented!("unused by tree_fingerprint")
        }
        async fn abort_merge(&self, _work_dir: &std::path::Path) -> Result<(), PortError> {
            unimplemented!("unused by tree_fingerprint")
        }
    }

    /// Reads real filesystem metadata for the one method `tree_fingerprint`
    /// calls; the rest are unused by this test.
    struct RealFiles;
    #[async_trait::async_trait]
    impl crate::ports::outbound::WorkspaceFilesPort for RealFiles {
        async fn read(&self, _path: &std::path::Path) -> Option<String> {
            None
        }
        async fn read_bytes(&self, _path: &std::path::Path) -> Option<Vec<u8>> {
            None
        }
        async fn write(&self, _path: &std::path::Path, _content: &str) -> bool {
            false
        }
        async fn write_bytes(&self, _path: &std::path::Path, _bytes: &[u8]) -> bool {
            false
        }
        async fn delete(&self, _path: &std::path::Path) -> bool {
            false
        }
        async fn list(&self, _dir: &std::path::Path) -> Vec<crate::ports::outbound::FileMeta> {
            vec![]
        }
        async fn stat(&self, path: &std::path::Path) -> Option<crate::ports::outbound::FileMeta> {
            let m = std::fs::metadata(path).ok()?;
            let modified_epoch = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            Some(crate::ports::outbound::FileMeta {
                path: path.to_path_buf(),
                modified_epoch,
                size: m.len(),
            })
        }
        async fn list_recursive(&self, _dir: &std::path::Path) -> Vec<std::path::PathBuf> {
            vec![]
        }
        async fn list_dirs(&self, _dir: &std::path::Path) -> Vec<std::path::PathBuf> {
            vec![]
        }
    }

    /// Regression for COX-B063: COX-B031 hashed per-file (size, mtime) so
    /// edits inside a new untracked dir invalidate the green cache, but a
    /// renamed TRACKED file hits a different branch — `git status` prints one
    /// `RM old -> new` line whose status-line text does not change between
    /// edits, so the fingerprint must come from the new path's metadata, not
    /// the raw "old -> new" text.
    #[tokio::test]
    async fn edit_after_rename_changes_the_fingerprint() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let dir = tmp.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?} failed");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("tracked.rs"), "fn a() {}\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        git(&["mv", "tracked.rs", "renamed.rs"]);
        std::fs::write(dir.join("renamed.rs"), "fn a() {}\n// first edit\n").expect("write");

        // Confirms the fixture actually hits the reported shape — one `RM old
        // -> new` line — before trusting the fingerprint assertions below.
        let status = git(&["status", "--porcelain", "--untracked-files=all"]);
        assert_eq!(status.trim(), "RM tracked.rs -> renamed.rs");

        let uc = RunDevUseCase::new(
            Arc::new(MemStore::default()),
            Arc::new(OkEngine),
            Config::default(),
            dir.to_path_buf(),
            DevMode::Feature,
        )
        .with_git(Some(Arc::new(RealGit)))
        .with_files(Some(Arc::new(RealFiles)));

        let fp1 = uc.tree_fingerprint().await.expect("fp1");
        std::fs::write(
            dir.join("renamed.rs"),
            "fn a() {}\n// second edit, different length entirely\n",
        )
        .expect("write");
        let fp2 = uc.tree_fingerprint().await.expect("fp2");

        assert_ne!(
            fp1, fp2,
            "editing renamed.rs again must invalidate the green cache"
        );
    }
}
