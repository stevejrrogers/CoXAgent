//! Deploy, the health gate, and rollback to the last known-good build.
//!
//! Everything here answers one question — is what we just shipped actually
//! running — and nothing here needs to know how a sprint works.

use super::{seconds_since, short_sha, CycleReport, RunCycleUseCase, LAST_GOOD_REF};
use crate::ports::outbound::{AgentEnginePort, GitPort, SandboxStatus, StateStorePort};
use coxagent_domain::TicketId;
use std::sync::atomic::Ordering;
use std::sync::Arc;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// When `workflow.sandbox` is on but this host has no supported
    /// confinement mechanism, warn once (never hard-fail — the run still
    /// executes unconfined). Deduplicated per project per process lifetime via
    /// `sandbox_warned`, so a long-running loop doesn't spam the channel every
    /// cycle.
    pub(super) async fn warn_if_sandbox_unsupported(&self) {
        if !self.config.workflow.sandbox {
            return;
        }
        let SandboxStatus::Unavailable(reason) = self.engine.sandbox_status() else {
            return;
        };
        if self.sandbox_warned.swap(true, Ordering::SeqCst) {
            return; // already warned this process lifetime.
        }
        self.notify(
            "sandbox_unsupported",
            format!(
                "workflow.sandbox is on but this host has no supported confinement \
                 mechanism ({reason}) — agent file writes are NOT confined to the \
                 workspace."
            ),
        )
        .await;
    }
    /// Record a deploy outcome (activity + dashboard status). Best-effort.
    pub(super) async fn record_deploy(
        &self,
        ok: bool,
        summary: &str,
        commit_sha: Option<String>,
        health_check: Option<crate::state::HealthCheckResult>,
    ) {
        if let Ok(mut state) = self.store.load().await {
            let at = crate::state::now_rfc3339();
            state.log_activity("DEPLOY", summary, None);
            let verb = if ok { "shipped" } else { "deploy failed" };
            state.post_comment("SM", &format!("{verb}: {summary}"), None);
            state.deploy = Some(crate::state::DeployStatus {
                at,
                ok,
                summary: summary.to_owned(),
                commit_sha,
                health_check,
            });
            let _ = self.store.save(&state).await;
        }
    }
    /// Mandatory post-deploy health probe (COX-B004): see
    /// [`crate::ports::outbound::verify_deploy_health`] for the shared gate
    /// every deploy call site (cycle, chat, PR preview) runs through.
    ///
    /// Kept alongside the richer COX-F005 gate because the rollback path only
    /// needs the yes/no answer, not a result worth recording.
    pub(super) async fn verify_health_after_deploy(&self) -> bool {
        let Some(deploy) = &self.deploy else {
            return true;
        };
        crate::ports::outbound::verify_deploy_health_probe(deploy, self.host_port_probe).await
    }
    /// Detailed post-deploy health check (COX-F005): poll the app's health
    /// endpoint via [`crate::ports::outbound::DeployPort::wait_healthy`] for
    /// up to `config.deploy.health_check_timeout_secs`, returning whether it
    /// passed plus the probe detail (HTTP status, response time) to record in
    /// deploy history. `None` host_port means nothing to probe (mirrors the
    /// COX-B004 gate): reports healthy with no recorded detail.
    pub(super) async fn run_health_check(&self) -> (bool, Option<crate::state::HealthCheckResult>) {
        /// Slack on top of the configured bound before the cycle gives up on
        /// the gate itself. `wait_healthy` owns the real deadline and returns
        /// the failing probe's detail; this only catches an adapter whose own
        /// `health_check` wedges, and firing it first would cost us that
        /// detail — hence the slack rather than an exact-fit timeout.
        const HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(10);

        let Some(deploy) = &self.deploy else {
            return (true, None);
        };
        // A malformed `host_port` (COX-B035) must fail the gate outright,
        // not be treated as unconfigured — see `host_port_probe`.
        let port = match self.host_port_probe {
            Ok(Some(port)) => port,
            Ok(None) => return (true, None),
            Err(()) => return (false, None),
        };
        let bound = std::time::Duration::from_secs(self.config.deploy.health_check_timeout_secs);
        let result = match tokio::time::timeout(
            bound + HANG_GUARD,
            deploy.wait_healthy(port, bound),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => crate::state::HealthCheckResult {
                passed: false,
                http_status: None,
                response_time_ms: None,
            },
        };
        (result.passed, Some(result))
    }
    /// Point [`LAST_GOOD_REF`] at this deploy and record it as auto-rollback's
    /// new target — called only after a deploy passed both `deploy()` and
    /// `run_tests()`. Also clears `in_rollback`: forward progress recovered.
    pub(super) async fn record_known_good(&self, sha: Option<String>, summary: &str) {
        let Some(sha) = sha else { return };
        if let Some(git) = &self.git {
            let _ = git.update_ref(&self.work_dir, LAST_GOOD_REF, &sha).await;
        }
        let at = crate::state::now_rfc3339();
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.deploy_index += 1;
            s.in_rollback = false;
            s.last_good_deploy = Some(crate::state::KnownGoodDeploy {
                sha: sha.clone(),
                at: at.clone(),
                deploy_index: s.deploy_index,
                summary: summary.to_owned(),
            });
            Ok(())
        })
        .await;
    }
    /// Path to the dedicated secondary worktree rollback deploys into — never
    /// the live `work_dir`, so DEV/worker concurrency and the leader tail's
    /// own `checkout_branch` calls never race against it.
    pub(super) fn rollback_worktree_path(&self) -> std::path::PathBuf {
        let name = self.work_dir.file_name().map_or_else(
            || "project".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        let dirname = format!("{name}-rollback");
        self.work_dir
            .parent()
            .map_or_else(|| std::path::PathBuf::from(&dirname), |p| p.join(&dirname))
    }
    /// Auto-rollback: on a deploy or post-deploy test failure, redeploy the
    /// last version that passed both gates, in a dedicated secondary
    /// worktree so the LIVE `work_dir` is never touched — the shared
    /// environment stays trustworthy while the root cause works through the
    /// backlog like any other bug. Opt-in (`config.deploy.auto_rollback`,
    /// default off). Caps at one retry — a second failure escalates via the
    /// bug+notify path instead of looping. Best-effort throughout.
    pub(super) async fn attempt_rollback(
        &self,
        reason: &str,
        failed_sha: Option<String>,
        report: &mut CycleReport,
    ) {
        if !self.config.deploy.auto_rollback {
            return;
        }
        let (Some(deploy), Some(git)) = (&self.deploy, &self.git) else {
            return;
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        // Edge case: first-ever deploy has nothing to roll back to — keep
        // today's bug-only behavior.
        let Some(good) = state.last_good_deploy.clone() else {
            return;
        };
        // Already ON the known-good version — nothing a rollback would
        // change. Guards against a redundant second rollback when deploy AND
        // tests both fail the same cycle.
        if failed_sha.as_deref() == Some(good.sha.as_str()) {
            return;
        }

        let too_old = match seconds_since(&good.at) {
            Some(age) => age > self.config.deploy.max_rollback_age_secs,
            None => true, // unparseable timestamp — don't guess, treat as stale
        };
        if too_old {
            self.record_rollback_skipped(
                reason,
                failed_sha.clone(),
                &good.sha,
                "known-good deploy is stale",
                true,
                false,
                report,
            )
            .await;
            return;
        }

        // Migration safety: rolling the app back without the DB schema it
        // expects can corrupt data — skip rather than guess.
        if let Some(sha) = &failed_sha {
            if self.migration_shipped_since(git, &good.sha, sha).await {
                self.record_rollback_skipped(
                    reason,
                    failed_sha.clone(),
                    &good.sha,
                    "a migration shipped since the known-good deploy — rolling back the app code \
                     alone would be unsafe",
                    false,
                    true,
                    report,
                )
                .await;
                return;
            }
        }

        // The rollback IS the one retry of the failed forward deploy: attempt
        // it exactly once — worktree always freshly created (remove + add) —
        // and if it also fails, stop here and escalate rather than loop.
        let path = self.rollback_worktree_path();
        let _ = git.worktree_remove(&self.work_dir, &path).await;
        let (ok, summary) = match git.worktree_add(&self.work_dir, &path, &good.sha).await {
            Err(e) => (false, format!("rollback worktree failed: {e}")),
            Ok(()) => match deploy.deploy(&path).await {
                // Same mandatory health gate as a forward deploy: a rollback
                // that starts a container but never binds the port must not
                // be reported as a successful recovery.
                Ok(r) if r.success => {
                    if self.verify_health_after_deploy().await {
                        (true, r.summary)
                    } else {
                        (
                            false,
                            format!(
                                "{} (containers started but the app never bound its port — \
                                 health check failed)",
                                r.summary
                            ),
                        )
                    }
                }
                Ok(r) => (false, r.summary),
                Err(e) => (false, format!("rollback deploy failed: {e}")),
            },
        };

        self.finish_rollback(reason, failed_sha, &good.sha, ok, summary, report)
            .await;
    }
    /// Whether any file under `config.deploy.migration_detection_paths`
    /// changed between the known-good sha and the failing one.
    pub(super) async fn migration_shipped_since(
        &self,
        git: &Arc<dyn GitPort>,
        good_sha: &str,
        failed_sha: &str,
    ) -> bool {
        let changed = git
            .changed_paths(&self.work_dir, good_sha, failed_sha)
            .await
            .unwrap_or_default();
        changed.iter().any(|p| {
            self.config
                .deploy
                .migration_detection_paths
                .iter()
                .any(|prefix| p.starts_with(prefix.as_str()))
        })
    }
    /// Record the outcome of a rollback attempt (activity + dashboard status,
    /// distinct from a plain deploy) and, on failure, escalate via the
    /// existing bug+notify path — the mirror of what a normal deploy failure
    /// already does, so a broken rollback mechanism can't fail silently. On a
    /// successful rollback it also runs the incident post-mortem loop
    /// (CXA-F012): blacklist the broken sha, write a docs post-mortem, file or
    /// link a root-cause prevention ticket and record a team lesson.
    pub(super) async fn finish_rollback(
        &self,
        reason: &str,
        failed_sha: Option<String>,
        good_sha: &str,
        ok: bool,
        summary: String,
        report: &mut CycleReport,
    ) {
        let at = crate::state::now_rfc3339();
        let short = short_sha(good_sha);
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.log_activity("ROLLBACK", &summary, None);
            s.post_comment(
                "SM",
                &format!("🔙 auto-rollback ({reason}) to {short}: {summary}"),
                None,
            );
            s.last_rollback = Some(crate::state::RollbackStatus {
                at: at.clone(),
                reason: reason.to_owned(),
                to_sha: good_sha.to_owned(),
                ok,
                summary: summary.clone(),
                stale: false,
                migration_blocked: false,
            });
            if ok {
                s.in_rollback = true;
                // COX-F005: the failing forward attempt's health-check result
                // is the diagnostic reason this rollback fired — preserve it
                // rather than dropping it under the rollback's own
                // (unchecked-in-detail) DeployStatus overwrite.
                let health_check = s.deploy.as_ref().and_then(|d| d.health_check.clone());
                s.deploy = Some(crate::state::DeployStatus {
                    at: at.clone(),
                    ok: true,
                    summary: format!("rolled back to {short}: {summary}"),
                    commit_sha: Some(good_sha.to_owned()),
                    health_check,
                });
            }
            Ok(())
        })
        .await;

        self.notify(
            if ok { "rollback_ok" } else { "rollback_failed" },
            format!("auto-rollback ({reason}) to {short}: {summary}"),
        )
        .await;

        if !ok {
            if let Some(id) = self.file_rollback_failed_bug(&summary).await {
                report.bugs_filed.push(id);
            }
            return;
        }

        // A successful rollback is an incident worth learning from — run the
        // post-mortem loop exactly once for this revision.
        self.post_mortem(reason, failed_sha, good_sha, false, summary.clone(), report)
            .await;
    }
    /// Record a rollback that was deliberately NOT attempted (stale target or
    /// a migration in the way) — distinct from an attempt that failed. The
    /// incident is still worth recording, but it is explicitly marked as
    /// 'rolled-forward/stale' rather than 'rolled-back' (CXA-F012), and the
    /// prevention ticket is still filed so the root cause cannot silently ride
    /// forward into the next good deploy.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_rollback_skipped(
        &self,
        reason: &str,
        failed_sha: Option<String>,
        to_sha: &str,
        summary: &str,
        stale: bool,
        migration_blocked: bool,
        report: &mut CycleReport,
    ) {
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.last_rollback = Some(crate::state::RollbackStatus {
                at: crate::state::now_rfc3339(),
                reason: reason.to_owned(),
                to_sha: to_sha.to_owned(),
                ok: false,
                summary: summary.to_owned(),
                stale,
                migration_blocked,
            });
            Ok(())
        })
        .await;
        self.notify(
            "rollback_blocked",
            format!("Rollback skipped for `{reason}`: {summary}"),
        )
        .await;
        // A skipped rollback is still an incident — record its post-mortem once.
        self.post_mortem(
            reason,
            failed_sha,
            to_sha,
            true,
            summary.to_string(),
            report,
        )
        .await;
    }
    /// The CXA-F012 incident post-mortem loop: run exactly once per deploy
    /// revision that auto-rolled back (or was deliberately skipped from rolling
    /// back). It blacklists the broken sha so self-healing never re-promotes it,
    /// writes a durable [`IncidentRecord`](crate::state::IncidentRecord), posts
    /// a human-readable post-mortem into #incidents (the room someone watching
    /// for outages actually looks at), emits a distinguishable NotifierPort
    /// event, files or links a deduped root-cause PREVENTION ticket and records
    /// a team lesson.
    ///
    /// Best-effort throughout: none of these failures may break or stall a run,
    /// and there must never be more than one post-mortem per revision.
    pub(super) async fn post_mortem(
        &self,
        reason: &str,
        failed_sha: Option<String>,
        target_sha: &str,
        rolled_forward: bool,
        summary: String,
        _report: &mut CycleReport,
    ) {
        use crate::state::{INCIDENTS_CHANNEL, MAX_INCIDENTS};

        let failed = failed_sha.clone().unwrap_or_default();
        let short_target = short_sha(target_sha);
        let mood = if rolled_forward {
            "rolled-forward/stale"
        } else {
            "rolled-back"
        };
        let body = format!(
            "🩺 Post-mortem ({reason}): {summary} → {mood} to {short_target} \
             [{failed}] — root cause filed as tracked work."
        );

        // Blacklist + durable record + #incidents surface in ONE state mutation:
        // they describe the same incident and must land atomically.
        crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if !failed.is_empty() {
                s.rolled_back_commits.insert(failed.clone());
            }
            s.post_chat_in("SM", &body, INCIDENTS_CHANNEL, Vec::new());
            s.incidents.push(crate::state::IncidentRecord {
                at: crate::state::now_rfc3339(),
                reason: reason.to_owned(),
                failed_sha: failed.clone(),
                to_sha: target_sha.to_owned(),
                ok: !rolled_forward,
                summary: summary.clone(),
                root_cause_ticket: None,
                lesson: None,
            });
            let overflow = s.incidents.len().saturating_sub(MAX_INCIDENTS);
            if overflow > 0 {
                s.incidents.drain(0..overflow);
            }
            Ok(())
        })
        .await
        .ok();

        // Surface it distinctly — a NotifierPort event that can't be confused
        // with a routine deploy notification.
        self.notify("incident_post_mortem", body).await;

        // File or link a deduped root-cause PREVENTION ticket so the same
        // symptom can't ride forward into the next good deploy unaddressed.
        let lesson_text = format!(
            "{reason} triggered an auto-rollback to {short_target}; fix shipped as tracked work."
        );
        crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.add_lesson(&lesson_text);
            Ok(())
        })
        .await
        .ok();
    }
    /// File a High bug when a rollback attempt itself fails (deduped on an
    /// open one) — the root-cause failure already filed its own bug via
    /// `file_deploy_bug`/`file_test_failure`; this one is about the ops
    /// mechanism (e.g. the Docker daemon is down), separate work.
    pub(super) async fn file_rollback_failed_bug(&self, summary: &str) -> Option<TicketId> {
        use coxagent_domain::ticket::{Priority, Status, TicketType};
        const MARKER: &str = "Rollback failed";
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
        let adder = crate::use_cases::AddTicketUseCase::new(Arc::clone(&self.store));
        adder
            .execute(crate::use_cases::AddTicketInput {
                ticket_type: TicketType::Bug,
                title: format!("{MARKER}: {summary}"),
                description: format!(
                    "Auto-rollback to the last known-good deploy failed after one retry — the \
                     environment may still be on a broken build. Investigate the deploy \
                     tooling (e.g. is the Docker daemon up?) and restore service by hand if \
                     needed.\n\nRollback output: {summary}"
                ),
                priority: Priority::High,
                complexity: coxagent_domain::ticket::Complexity::Medium,
                has_ui: false,
                acceptance_criteria: vec![
                    "The app is reachable and serving a known-good build".to_owned()
                ],
                goal: None,
            })
            .await
            .ok()
    }
    pub(super) async fn file_deploy_bug(&self, summary: &str) -> Option<TicketId> {
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
                goal: None,
            })
            .await
            .ok()
    }

    /// Announce an automated PR close where PEOPLE look — the team channel and
    /// the webhook — never just a comment on the PR itself. Twelve PRs died
    /// silently in one night (#185–#196) because the close only wrote to
    /// GitHub and the activity log; an irreversible act done by a machine must
    /// be loud enough to challenge.
    pub(super) async fn announce_pr_close(&self, number: u64, title: &str, why: &str) {
        let msg = format!("🗑️ auto-closed PR #{number} (\"{title}\") — {why}");
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.post_chat_in("SA", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            // The app closed this PR itself. Mark it so `forge_hygiene`'s
            // closed-unmerged pass does NOT misread our own action as a human
            // rejection and force a redesign loop. Only externally-closed PRs
            // (a real person) must trigger that signal.
            s.seen_closed_prs.insert(number);
            Ok(())
        })
        .await;
        self.notify("pr_closed", msg).await;
    }

    /// Pre-cycle git tree hygiene — undo the wreckage an engine death leaves
    /// behind, BEFORE any git op this cycle trips over it:
    ///
    /// - a SLOT worktree sitting on a named branch (worktrees must stay
    ///   detached, or they hold `main`/feature branches hostage) → detach;
    /// - a SLOT worktree with uncommitted residue (an engine died mid-edit)
    ///   that would otherwise block every future checkout → stash the WIP with
    ///   a named marker;
    /// - the LEADER tree dirty on a feature branch (half-written edits block
    ///   every checkout) → stash the WIP with a named marker and return to the
    ///   base branch;
    /// - the LEADER's local base carrying commits origin does not have (an
    ///   agent merged a feature branch into local base by mistake) → keep them
    ///   on a `backup/…` branch and hard-reset base to origin.
    ///
    /// Best-effort: every step logs what it did; a failure never stops the
    /// cycle. Pure orchestration over `GitPort::raw` — no direct IO here.
    pub(super) async fn tree_hygiene(&self) {
        let Some(git) = &self.git else { return };
        let wd = &self.work_dir;
        if !self.config.git.enabled || !git.is_repo(wd).await {
            return;
        }
        let base = {
            let t = self.config.git.target_branch.trim();
            if t.is_empty() {
                self.config.git.default_branch.clone()
            } else {
                t.to_owned()
            }
        };
        let branch = git.current_branch(wd).await.unwrap_or_default();
        let is_slot = wd
            .components()
            .any(|c| c.as_os_str() == ".coxagent-worktrees");
        if is_slot {
            // Slots must stay detached; holding a branch blocks every other
            // tree from checking it out.
            if !branch.is_empty() && branch != "HEAD" {
                let (ok, _) = git.raw(wd, &["checkout", "--detach"]).await;
                if ok {
                    self.log_git(&format!(
                        "hygiene: slot worktree released branch {branch} (detached)"
                    ))
                    .await;
                }
            }
            // A slot left with uncommitted residue (an engine died mid-edit)
            // is the SAME trap as the leader tree: every later `checkout` of a
            // new branch fails with "local changes would be overwritten" and
            // the ticket is mis-assigned forever. Stash the WIP with a named
            // marker so the slot is clean for the next checkout.
            let (_, status) = git.raw(wd, &["status", "--porcelain"]).await;
            if !status.trim().is_empty() {
                let (ok, _) = git
                    .raw(
                        wd,
                        &[
                            "stash",
                            "push",
                            "-u",
                            "-m",
                            "hygiene: slot WIP (engine died mid-edit)",
                        ],
                    )
                    .await;
                if ok {
                    self.log_git("hygiene: stashed slot WIP — recover with `git stash list`")
                        .await;
                }
            }
            return;
        }
        // Leader-tree hygiene lives in its own method to keep tree_hygiene lean.
        self.tree_hygiene_leader(git, wd, &base, &branch).await;
    }

    /// Leader-tree hygiene: orphan WIP on a feature branch -> stash + back to
    /// base; local base ahead of origin -> backup + reset. Pure orchestration
    /// over `GitPort::raw`; a failure never stops the cycle.
    async fn tree_hygiene_leader(
        &self,
        git: &Arc<dyn GitPort>,
        wd: &std::path::Path,
        base: &str,
        branch: &str,
    ) {
        // Leader tree. 1) Orphan WIP on a feature branch → stash + back to base.
        let (_, status) = git.raw(wd, &["status", "--porcelain"]).await;
        let dirty = !status.trim().is_empty();
        if dirty && !branch.is_empty() && branch != base {
            let msg = format!("hygiene: orphan WIP on {branch} (engine died mid-run)");
            let (ok, _) = git.raw(wd, &["stash", "push", "-u", "-m", &msg]).await;
            if ok {
                self.log_git(&format!(
                    "hygiene: stashed orphan WIP from {branch} — recover with `git stash list`"
                ))
                .await;
            }
        } else if dirty && branch == base {
            // Residue ON the base blocks every branch checkout too — the
            // repeating "checkout failed: The following untracked working
            // tree files would be overwritten" (COX-B016/B080, hourly). This
            // runs at the TOP of a cycle, before any phase edits — dirt here
            // is leftovers, never live work.
            let msg = format!("hygiene: untracked/dirty residue on {base}");
            let (ok, _) = git.raw(wd, &["stash", "push", "-u", "-m", &msg]).await;
            if ok {
                self.log_git(&format!(
                    "hygiene: stashed residue blocking checkouts on {base} — `git stash list`"
                ))
                .await;
            }
        }
        if !branch.is_empty() && branch != base && branch != "HEAD" {
            let (ok, _) = git.raw(wd, &["checkout", base]).await;
            if !ok {
                // Base may be held by a stray worktree; a detached base is
                // still a working position for the cycle.
                let _ = git
                    .raw(wd, &["checkout", "--detach", &format!("origin/{base}")])
                    .await;
            }
        }
        // 2) Local base polluted with commits origin lacks → backup + reset.
        let _ = git.raw(wd, &["fetch", "origin", base]).await;
        let (ok, ahead) = git
            .raw(
                wd,
                &["rev-list", "--count", &format!("origin/{base}..{base}")],
            )
            .await;
        let ahead: u64 = if ok {
            ahead.trim().parse().unwrap_or(0)
        } else {
            0
        };
        if ahead > 0 {
            let backup = format!("backup/{base}-hygiene");
            let _ = git.raw(wd, &["branch", "-f", &backup, base]).await;
            // Reset only moves the base ref when we actually sit on it.
            let cur = git.current_branch(wd).await.unwrap_or_default();
            if cur == base {
                let (ok, _) = git
                    .raw(wd, &["reset", "--hard", &format!("origin/{base}")])
                    .await;
                if ok {
                    self.log_git(&format!(
                        "hygiene: local {base} had {ahead} commit(s) origin lacks — \
                         kept on {backup}, reset to origin/{base}"
                    ))
                    .await;
                }
            }
        }
    }
}
