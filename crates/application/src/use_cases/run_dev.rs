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
        // Self-healing boot: if the project doesn't compile, fix that BEFORE
        // touching any tickets. Otherwise every ticket will fail anyway.
        // Skipped entirely when the tree is unchanged since the last green
        // suite run (process-wide fingerprint cache) — one green check per
        // tree-state serves every runner in the cycle.
        if let Some(deploy) = &self.verify {
            if crate::verify_cache::is_green(&self.work_dir) {
                // fall through — nothing changed since the last green run
            } else {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    deploy.run_tests(&self.work_dir),
                )
                .await
                {
                    Ok(Ok(r)) if r.success => {
                        crate::verify_cache::mark_green(&self.work_dir);
                    }
                    Ok(Ok(r)) => {
                        tracing::warn!(
                            "DEV boot check: cargo test failed — self-healing. {}",
                            &r.summary[..r.summary.len().min(200)]
                        );
                        return self.self_heal_compile(&r.summary).await;
                    }
                    Ok(Err(e)) => {
                        // Spawn errors and timeouts are INFRASTRUCTURE, not compile
                        // breakage — healing on them tells the LLM "the project
                        // doesn't compile" with no compile error to fix.
                        tracing::warn!("DEV boot check: cargo test spawn error — {e}");
                        return Ok(None);
                    }
                    Err(_timeout) => {
                        tracing::warn!("DEV boot check: cargo test timed out after 5 min");
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
                         Put them where this project keeps tests, compiling but failing \
                         for the right reason. Commit nothing.",
                        criteria.join("\n- ")
                    ),
                    work_dir: self.work_dir.clone(),
                    timeout: Duration::from_secs(900),
                    escalation_level: 0,
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
        let mut request = self.build_request(&state, &id);
        let plan_first = request.escalation_level == 0; // retries already carry a journal
        if plan_first {
            request.task_prompt = format!(
                "{}\n\nFIRST: do NOT write code yet. Explore the relevant code (use the repo \
                 map / code-graph tools), then output a concise implementation PLAN: the exact \
                 steps, the files you will touch, the tests you will add, and the biggest risk. \
                 Wait for the follow-up before implementing.",
                request.task_prompt
            );
        }
        // Keep the engine's conversation id: the execute pass and the repair
        // pass (below) resume this session so the agent keeps everything it
        // just read and wrote in context instead of rediscovering it cold.
        let session = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => {
                let sid = o.session_id.clone();
                match (&sid, plan_first) {
                    (Some(sid_v), true) => {
                        // Phase 2: execute the plan it just committed to.
                        let exec = self
                            .engine
                            .resume_run(
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
                                let fresh = self.build_request(&state, &id);
                                match self.engine.run(fresh).await {
                                    Ok(f) if f.succeeded() => f.session_id.clone(),
                                    Ok(f) => {
                                        self.record_failure(&id, f.stderr.trim()).await;
                                        self.release_claim(&id).await;
                                        return Err(PortError::Backend(format!(
                                            "{:?} engine failed on {id}: {}",
                                            self.mode,
                                            f.stderr.trim()
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

        // Expert habit: review your OWN diff before anyone else sees it.
        // Same conversation (context intact) = one cheap pass that catches
        // nits, dead code and missed edge cases. Best-effort.
        if let Some(sid) = &session {
            let _ = self
                .engine
                .resume_run(
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
                        escalation_level: 0,
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

            // Lint gate: a change may never ADD clippy errors. Baseline is
            // learned on first measure and ratchets DOWN when improved.
            if let Ok(Some(count)) = deploy.lint(&self.work_dir).await {
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
                        let fixup = format!(
                            "Your change introduced NEW `cargo clippy` errors (was {base}, now \
                             {count}). Run `cargo clippy --workspace --all-targets`, fix ONLY \
                             errors caused by your change, and do not start new work."
                        );
                        if let Some(sid) = &session {
                            let _ = self
                                .engine
                                .resume_run(sid, &fixup, &self.work_dir, Duration::from_secs(900))
                                .await;
                        } else {
                            let repair = AgentRequest {
                                role: self.mode.role(),
                                system_prompt: prompts::system_prompt(prompts::DEV),
                                task_prompt: fixup,
                                work_dir: self.work_dir.clone(),
                                timeout: Duration::from_secs(900),
                                escalation_level: 0,
                            };
                            let _ = self.engine.run(repair).await;
                        }
                        let after = deploy
                            .lint(&self.work_dir)
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or(count);
                        if after > base {
                            self.record_failure(
                                &id,
                                &format!("added clippy errors ({base} -> {after})"),
                            )
                            .await;
                            self.release_claim(&id).await;
                            return Err(PortError::Backend(format!(
                                "{:?} added lint errors on {id} — ticket returned to the queue",
                                self.mode
                            ))
                            .into());
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

            // Regression-test gate: a BUG fix that touches no test is a fix
            // on faith. Mechanical check over the working diff; one bounded
            // repair pass to add the missing test.
            if self.mode == DevMode::Bug && !self.diff_touches_tests() {
                let fixup = format!(
                    "Your fix for {id} ships with NO regression test. Add a test that FAILS \
                     without your fix and passes with it — that is the only proof the bug is \
                     dead. Do not start new work or commit."
                );
                if let Some(sid) = &session {
                    let _ = self
                        .engine
                        .resume_run(sid, &fixup, &self.work_dir, Duration::from_secs(900))
                        .await;
                } else {
                    let repair = AgentRequest {
                        role: self.mode.role(),
                        system_prompt: prompts::system_prompt(prompts::DEV),
                        task_prompt: fixup,
                        work_dir: self.work_dir.clone(),
                        timeout: Duration::from_secs(900),
                        escalation_level: 0,
                    };
                    let _ = self.engine.run(repair).await;
                }
                if !self.diff_touches_tests() {
                    self.record_failure(&id, "bug fix shipped without a regression test")
                        .await;
                    self.release_claim(&id).await;
                    return Err(PortError::Backend(format!(
                        "{:?} fix for {id} has no regression test — returned to the queue",
                        self.mode
                    ))
                    .into());
                }
                // Suite must STILL be green with the new test in place.
                if let Ok(r) = deploy.run_tests(&self.work_dir).await {
                    if !r.success {
                        self.record_failure(&id, "regression test added but suite is red")
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
            crate::verify_cache::mark_green(&self.work_dir);
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
            // Done — the work journal and any cost hold served their purpose.
            state.ticket_journal.remove(&id_c.to_string());
            state.cost_holds.remove(&id_c.to_string());
            state.cost_approved.remove(&id_c.to_string());
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

    /// Whether the current working diff (staged/unstaged + untracked) touches
    /// tests: a test-ish path, or added lines containing test markers.
    fn diff_touches_tests(&self) -> bool {
        let run = |args: &[&str]| -> String {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&self.work_dir)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default()
        };
        let names = format!(
            "{}\n{}",
            run(&["diff", "HEAD", "--name-only"]),
            run(&["ls-files", "--others", "--exclude-standard"])
        );
        if names.lines().any(|f| {
            let f = f.trim().to_lowercase();
            !f.is_empty()
                && (f.contains("/tests/")
                    || f.starts_with("tests/")
                    || f.ends_with("_test.rs")
                    || f.ends_with("_test.go")
                    || f.ends_with(".test.ts")
                    || f.ends_with(".test.js")
                    || f.contains("test_"))
        }) {
            return true;
        }
        let diff = run(&["diff", "HEAD"]);
        diff.lines().any(|l| {
            l.starts_with('+')
                && (l.contains("#[test]")
                    || l.contains("#[tokio::test]")
                    || l.contains("def test_")
                    || l.contains("it(")
                    || l.contains("func Test"))
        })
    }

    /// When pre-check fails, ask the LLM to fix ALL compile/test errors until
    /// the codebase is green or we hit the retry cap. This runs BEFORE any
    /// tickets are touched — infrastructure repair, not feature work.
    async fn self_heal_compile(&self, error_summary: &str) -> Result<Option<TicketId>, AppError> {
        use crate::prompts;

        let Some(deploy) = &self.verify else {
            return Ok(None);
        };

        let mut last_error = error_summary.to_owned();
        for attempt in 1_u32..=3 {
            let task = if attempt == 1 {
                format!(
                    "The project does NOT compile. Fix ALL errors:\n\n\
                     ```\n{last_error}\n```\n\n\
                     Run `cargo check`, fix every error, then `cargo test` to verify."
                )
            } else {
                format!(
                    "Still not compiling. Last test output:\n\n\
                     ```\n{last_error}\n```\n\n\
                     Fix the remaining errors. Check what you missed."
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
                std::time::Duration::from_secs(300),
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
        Ok(None)
    }

    /// Count a failed attempt on `id`; at the 3rd, park it with a visible note
    /// so a human decides instead of the team burning tokens forever.
    async fn record_failure(&self, id: &TicketId, why: &str) {
        let key = id.to_string();
        let short: String = why.chars().take(300).collect();
        // Infrastructure faults (revoked auth, quota walls, rate limits) are
        // NOT the ticket's fault — counting them parked 3 innocent tickets
        // during a 401 outage. Log, don't punish.
        let low = why.to_lowercase();
        let infra = why.trim().is_empty()
            || low.contains("401")
            || low.contains("authenticate")
            || low.contains("revoked")
            || low.contains("quota")
            || low.contains("rate limit")
            || low.contains("overloaded");
        if infra {
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                s.log_activity(
                    "SYSTEM",
                    "engine infrastructure fault — attempt not counted",
                    Some(key.clone()),
                );
                Ok(())
            })
            .await;
            return;
        }
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
        // Human steering: recent USER comments on this ticket become explicit
        // instructions — commenting on an in-progress ticket steers the agent
        // on its next run instead of shouting into the void.
        let steering = {
            let notes: Vec<String> = state
                .comments
                .iter()
                .filter(|c| c.author == "USER" && c.ticket.as_deref() == Some(id.as_str()))
                .rev()
                .take(3)
                .map(|c| c.body.chars().take(400).collect::<String>())
                .collect();
            if notes.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\nHUMAN STEERING on this ticket (newest first — follow it):\n- {}",
                    notes.join("\n- ")
                )
            }
        };
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
                "Ticket {id}: {title}\n{}\nImplement it now.{stack}{deploy}{design}{context_block}{}{}{}{}{steering}{journal}",
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
                prompts::team_memory_block_relevant(
                    &state.decisions,
                    &state.lessons,
                    &format!(
                        "{title} {}",
                        ticket
                            .and_then(|t| t.design().technical.as_ref())
                            .map_or("", |d| d.approach.as_str())
                    ),
                ),
                prompts::hub_lessons_block(),
            ),
            work_dir: self.work_dir.clone(),
            timeout: Duration::from_secs(3600),
            // Escalation: retries climb the ladder — and a LARGE ticket starts
            // on rung 1 outright. Experts don't try the cheap model first on
            // the hard problem and hope.
            escalation_level: {
                let attempts = state
                    .ticket_fail_attempts
                    .get(&id.to_string())
                    .copied()
                    .unwrap_or(0)
                    .min(3);
                let floor = u32::from(ticket.is_some_and(|t| {
                    t.complexity() == coxagent_domain::ticket::Complexity::Large
                }));
                u8::try_from(attempts.max(floor)).unwrap_or(3)
            },
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
    use crate::ports::outbound::{AgentOutcome, SandboxStatus};
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
                sandbox: SandboxStatus::default(),
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

    #[tokio::test]
    async fn diff_touches_tests_detects_markers_and_paths() {
        let dir = std::env::temp_dir().join(format!("cox-dtt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "base",
        ]);
        let uc = RunDevUseCase::new(
            Arc::new(MemStore {
                state: Mutex::new(ProjectState::default()),
            }),
            Arc::new(OkEngine),
            Config::default(),
            dir.clone(),
            DevMode::Bug,
        );
        assert!(!uc.diff_touches_tests(), "clean tree touches nothing");
        std::fs::write(dir.join("lib.rs"), "fn f() {}\n").unwrap();
        assert!(!uc.diff_touches_tests(), "non-test change is not a test");
        std::fs::write(dir.join("lib.rs"), "fn f() {}\n#[test]\nfn t() {}\n").unwrap();
        git(&["add", "-A"]);
        assert!(uc.diff_touches_tests(), "added #[test] counts");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
