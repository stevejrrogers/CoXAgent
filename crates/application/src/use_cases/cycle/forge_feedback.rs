// Part of the cycle module split by concern — see cycle/mod.rs.
//! Addressing human PR feedback: the DEV pass that answers review comments.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, StateStorePort};

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// The PR feedback loop: for the newest open PR with unaddressed
    /// change-requests (a review submitted after the branch's last commit), run
    /// a DEV agent that checks out the branch, fixes exactly what the review
    /// asked, and pushes — then replies on the PR and tells the chat. One PR per
    /// cycle, so a review queue drains steadily without a token spike.
    #[allow(clippy::too_many_lines)] // one linear queue-drain pass; splitting hurts readability
    pub(super) async fn address_pr_feedback(&self) {
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
        // (refactor waiting on an empty queue) drain twice as fast; in full
        // queue RECOVERY (nothing else runs) drain as hard as we can afford.
        let recovering = self.store.load().await.is_ok_and(|s| s.queue_recovery);
        let mut fix_budget: u32 = if recovering {
            8
        } else if self.clean_base_required().await {
            2
        } else {
            1
        };
        // In recovery scan the WHOLE queue — capacity is bounded by fix_budget,
        // not by how far we look; skipping fixable PRs just slows the drain.
        let scan = if recovering { usize::MAX } else { 6 };
        for pr in queue.into_iter().take(scan) {
            // What needs fixing? Explicit review feedback, and/or merge conflicts
            // — conflicts are handled IMMEDIATELY, not parked for a review round.
            let feedback = forge.pr_feedback(pr.number).await.unwrap_or_default();
            let conflicted = !pr.mergeable;
            if feedback.is_empty() && !conflicted {
                continue;
            }
            // Run this fix in the leader's ISOLATED feedback worktree — the
            // shared checkout is mid-cycle and dirty, so `git checkout` there
            // aborts ("local changes would be overwritten") and the fix never
            // lands (the PR that spins forever). The feedback tree shares the
            // repo's refs, so the branch checkout, commit and push all work
            // exactly as on the main tree.
            let fix_dir = self
                .feedback_work_dir
                .clone()
                .unwrap_or_else(|| self.work_dir.clone());
            // Ping-pong brake with an SM escalation LADDER: after 2 fix rounds
            // the SM first sends the SA in for a root-cause rescue (close the
            // PR, or leave concrete instructions and grant one informed retry).
            // Only a SECOND stall after that rescue goes to a human.
            let (attempts, rescued) = self.store.load().await.map_or((0, 0), |s| {
                (
                    s.pr_fix_attempts.get(&pr.number).copied().unwrap_or(0),
                    s.pr_rescues.get(&pr.number).copied().unwrap_or(0),
                )
            });
            if attempts >= 2 {
                if rescued == 0 {
                    self.sa_rescue_pr(&pr).await;
                    continue;
                }
                if attempts == 2 {
                    self.notify(
                        "pr_stuck",
                        if self.config.workflow.language.is_vi() {
                            format!(
                                "PR #{} vẫn kẹt SAU khi SA đã rescue — cần người quyết: {}",
                                pr.number, pr.url
                            )
                        } else {
                            format!(
                                "PR #{} is still stuck AFTER an SA rescue — a person must decide: {}",
                                pr.number, pr.url
                            )
                        },
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
            let task_prompt = format!(
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
            );
            // Round 2 RESUMES round 1's conversation when the engine supports
            // it — the agent still has the branch and feedback in context.
            let prior_session = self
                .store
                .load()
                .await
                .ok()
                .and_then(|s| s.pr_sessions.get(&pr.number).cloned());
            let resumed = match &prior_session {
                Some(sid) => self
                    .engine
                    .resume_run(
                        coxagent_domain::Role::DevBug,
                        sid,
                        &task_prompt,
                        &fix_dir,
                        std::time::Duration::from_secs(1800),
                    )
                    .await
                    .ok(),
                None => None,
            };
            let outcome = if let Some(o) = resumed {
                Ok(o)
            } else {
                {
                    let request = crate::ports::outbound::AgentRequest {
                        role: coxagent_domain::Role::DevBug,
                        system_prompt: crate::prompts::system_prompt(crate::prompts::DEV),
                        task_prompt,
                        work_dir: fix_dir.clone(),
                        timeout: std::time::Duration::from_secs(1800),
                        // Each prior fix round escalates the model ladder.
                        escalation_level: u8::try_from(attempts.min(3)).unwrap_or(3),
                        label: None,
                    };
                    self.engine.run(request).await
                }
            };
            // Remember this run's conversation for the next fix round.
            if let Ok(o) = &outcome {
                if let Some(sid) = o.session_id.clone() {
                    let n = pr.number;
                    let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                        s.pr_sessions.insert(n, sid.clone());
                        Ok(())
                    })
                    .await;
                }
            }
            // The engine may leave the checkout on the PR branch — always park
            // the (isolated) feedback worktree back on the base branch for the
            // next stage, so the shared checkout stays clean.
            if let Some(git) = &self.git {
                let _ = git.checkout_branch(&fix_dir, target).await;
            }
            match outcome {
                Ok(o) if o.succeeded() => {
                    // A conflict fix counts ONLY after independent verification —
                    // a bad "resolution" that merges is how you ship broken code.
                    let verified = if conflicted {
                        self.verify_conflict_resolution(pr.number).await
                    } else {
                        Ok(())
                    };
                    let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                        *s.pr_fix_attempts.entry(pr.number).or_insert(0) += 1;
                        Ok(())
                    })
                    .await;
                    match verified {
                        Ok(()) => {
                            let _ = forge
                                .comment_pr(
                                    pr.number,
                                    "🔧 Addressed the review feedback — changes pushed to this \
                                     branch. Please take another look.",
                                )
                                .await;
                            self.log_git(&format!("DEV addressed feedback on PR #{}", pr.number))
                                .await;
                            self.notify(
                                "pr_fixed",
                                format!(
                                    "PR #{} — review feedback addressed and pushed; ready for \
                                     another look: {}",
                                    pr.number, pr.url
                                ),
                            )
                            .await;
                        }
                        Err(why) => {
                            let _ = forge
                                .request_changes(
                                    pr.number,
                                    &format!(
                                        "⛔ Conflict-resolution verification FAILED: {why}. This \
                                         branch must NOT be merged until a clean pass fixes it."
                                    ),
                                )
                                .await;
                            self.log_git(&format!(
                                "conflict fix on PR #{} REJECTED by verification: {why}",
                                pr.number
                            ))
                            .await;
                        }
                    }
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
    /// Drain queued execution jobs (control plane → this runner). Called at the
    /// top of every cycle AND from the operator's fast 15s poll, so a human's
    /// force-merge starts within seconds, not a full cycle later. One job per
    /// call; claim-and-remove is atomic so parallel operators never double-run.
    pub async fn drain_jobs(&self) {
        let job: Option<crate::state::PendingJob> = {
            let mut taken = None;
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                taken = if s.jobs.is_empty() {
                    None
                } else {
                    Some(s.jobs.remove(0))
                };
                Ok(())
            })
            .await;
            taken
        };
        let Some(job) = job else { return };
        match job.kind.as_str() {
            "force_merge" => {
                let num = job.args.get("pr").and_then(serde_json::Value::as_u64);
                if let Some(num) = num {
                    self.force_merge_job(num, &job.queued_by).await;
                }
            }
            other => {
                tracing::warn!("unknown queued job kind {other} — dropped");
            }
        }
    }
}
