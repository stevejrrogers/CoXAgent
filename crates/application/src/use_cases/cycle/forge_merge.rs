// Part of the cycle module split by concern — see cycle/mod.rs.
//! Merging: forge hygiene, PR rescue, merged-tree verification, forced merges.

use super::{diff_has_conflict_markers, RunCycleUseCase};
use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use std::sync::Arc;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Forge hygiene, once per cycle, zero tokens:
    /// 1. AUTO-REBASE: merge the latest base into every open PR branch that
    ///    can take it cleanly — after any squash-merge, sibling PRs otherwise
    ///    rot into conflicts one by one (the stacked-PR tax). Conflicted
    ///    branches are left for the existing fix flow.
    /// 2. HUMAN REJECTION: a PR closed WITHOUT merge is the costliest review
    ///    signal — record a lesson (state + hub) and brief the ticket's next
    ///    attempt via its journal + a comment.
    /// 3. Review comments starting with `LESSON:` become team lessons.
    #[allow(clippy::too_many_lines)] // three linear hygiene passes; splitting hurts readability
    pub(super) async fn forge_hygiene(&self) {
        // 0. LEARN: merged-then-reverted work (CXA-F047) — needs only local
        //    git, not the forge, so it runs before the forge gate below.
        self.learn_reverted_work().await;
        let Some(forge) = &self.forge else {
            return;
        };
        let target = self.flow_base().to_owned();
        self.sweep_stale_prs().await;
        // 1. Rebase open PRs onto the moving base.
        if let Ok(prs) = forge.list_open_prs().await {
            // Mirror the open queue into state: the Review page (and the inbox's
            // held-PR items) read `state.open_prs`, and nothing else writes it —
            // the page sat empty while three PRs waited on the forge.
            {
                let mirror: Vec<crate::ports::outbound::PrOpen> =
                    prs.iter().cloned().map(Into::into).collect();
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    s.set_open_prs(mirror.clone());
                    Ok(())
                })
                .await;
            }
            let mut rebased: Vec<u64> = Vec::new();
            for pr in prs.iter().filter(|p| p.base == target).take(8) {
                let Some(git) = &self.git else { continue };
                let run = |args: Vec<String>| {
                    let git = Arc::clone(git);
                    let dir = self.work_dir.clone();
                    async move {
                        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        git.raw(&dir, &refs).await.0
                    }
                };
                if !run(vec![
                    "fetch".into(),
                    "origin".into(),
                    pr.head.clone(),
                    target.clone(),
                ])
                .await
                {
                    continue;
                }
                let local = format!("refs/remotes/origin/{}", pr.head);
                let base_ref = format!("origin/{target}");
                // Already contains base? skip cheaply.
                if run(vec![
                    "merge-base".into(),
                    "--is-ancestor".into(),
                    base_ref.clone(),
                    local.clone(),
                ])
                .await
                {
                    continue;
                }
                if run(vec!["checkout".into(), "-B".into(), pr.head.clone(), local]).await
                    && run(vec!["merge".into(), base_ref.clone(), "--no-edit".into()]).await
                {
                    if run(vec!["push".into(), "origin".into(), pr.head.clone()]).await {
                        rebased.push(pr.number);
                    }
                } else {
                    // A PR already open that the moving base has made conflicted
                    // must be resolved HERE, not parked — otherwise the queue
                    // hangs at CONFLICTING with nobody holding the pen (the same
                    // law that governs a fresh PR in commit_for_ticket). List
                    // the unmerged files and let the DEV engine read both sides,
                    // preserving both intents; abort only when that genuinely
                    // cannot complete.
                    let (_, out) = git
                        .raw(&self.work_dir, &["diff", "--name-only", "--diff-filter=U"])
                        .await;
                    let files: Vec<String> = out
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .map(str::to_owned)
                        .collect();
                    let resolved = !files.is_empty()
                        && self
                            .resolve_merge_in_progress(&pr.head, &target, &files)
                            .await;
                    if resolved {
                        if run(vec!["push".into(), "origin".into(), pr.head.clone()]).await {
                            rebased.push(pr.number);
                        }
                    } else {
                        let _ = run(vec!["merge".into(), "--abort".into()]).await;
                    }
                }
                let _ = run(vec!["checkout".into(), target.clone()]).await;
            }
            if !rebased.is_empty() {
                let list = rebased
                    .iter()
                    .map(|n| format!("#{n}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    s.post_chat_in(
                        "SA",
                        &format!("🔁 Rebased open PRs onto the latest base: {list}"),
                        crate::state::AGENTS_CHANNEL,
                        Vec::new(),
                    );
                    Ok(())
                })
                .await;
            }
            // 2a. Orphan branches: pushed but never got a PR (a mid-cycle
            // restart can interrupt between push and PR creation — observed
            // with COX-B022). Open the missing PR so the work isn't stranded.
            {
                let with_pr: std::collections::BTreeSet<String> =
                    prs.iter().map(|p| p.head.clone()).collect();
                // A branch whose PR was closed unmerged is not an orphan — it
                // is a decision. Reopening it puts rejected work back in front
                // of the reviewer who just rejected it, every cycle, forever.
                let rejected: std::collections::BTreeSet<String> = forge
                    .closed_unmerged()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, head)| head)
                    .collect();
                let ls = match &self.git {
                    Some(git) => {
                        git.raw(&self.work_dir, &["ls-remote", "--heads", "origin"])
                            .await
                            .1
                    }
                    None => String::new(),
                };
                let orphans: Vec<String> = ls
                    .lines()
                    .filter_map(|l| l.split('\t').nth(1))
                    .filter_map(|r| r.strip_prefix("refs/heads/"))
                    .filter(|b| {
                        (b.starts_with("feat/") || b.starts_with("fix/"))
                            && *b != target
                            && !with_pr.contains(*b)
                            && !rejected.contains(*b)
                    })
                    .map(str::to_owned)
                    .take(2)
                    .collect();
                for branch in orphans {
                    // Only when the branch actually carries commits over base.
                    let ahead = match &self.git {
                        Some(git) => {
                            let (ok, out) = git
                                .raw(
                                    &self.work_dir,
                                    &[
                                        "rev-list",
                                        "--count",
                                        &format!("origin/{target}..origin/{branch}"),
                                    ],
                                )
                                .await;
                            if ok {
                                out.trim().parse::<u64>().unwrap_or(0)
                            } else {
                                0
                            }
                        }
                        None => 0,
                    };
                    if ahead == 0 {
                        continue;
                    }
                    let title = match &self.git {
                        Some(git) => {
                            let (_, out) = git
                                .raw(
                                    &self.work_dir,
                                    &["log", "-1", "--format=%s", &format!("origin/{branch}")],
                                )
                                .await;
                            let t = out.trim().to_owned();
                            if t.is_empty() {
                                branch.clone()
                            } else {
                                t
                            }
                        }
                        None => branch.clone(),
                    };
                    if let Ok(pr) = forge
                        .open_pr(
                            &branch,
                            &target,
                            &title,
                            "Opened by forge hygiene: this branch was pushed but its PR was \
                             never created (interrupted cycle).",
                        )
                        .await
                    {
                        let n = pr.number;
                        let b2 = branch.clone();
                        let _ =
                            crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                                s.post_chat_in(
                                    "SA",
                                    &format!("🧷 Opened missing PR #{n} for orphan branch `{b2}`."),
                                    crate::state::AGENTS_CHANNEL,
                                    Vec::new(),
                                );
                                Ok(())
                            })
                            .await;
                    }
                }
            }
            // 3. LESSON: comments from reviewers become team knowledge.
            for pr in prs.iter().take(8) {
                let Ok(feedback) = forge.pr_feedback(pr.number).await else {
                    continue;
                };
                for f in feedback {
                    for line in f.body.lines() {
                        if let Some(lesson) = line.trim().strip_prefix("LESSON:") {
                            let lesson = lesson.trim().to_owned();
                            if lesson.is_empty() {
                                continue;
                            }
                            crate::prompts::record_hub_lesson(self.files.as_deref(), &lesson).await;
                            let l2 = lesson.clone();
                            let _ = crate::ports::outbound::mutate_state(
                                self.store.as_ref(),
                                move |s| {
                                    s.add_lesson(&l2);
                                    Ok(())
                                },
                            )
                            .await;
                        }
                    }
                }
            }
        }
        // 2b. Sync HUMAN-merged PRs back into ticket state (processed once):
        // the fix landed, so the ticket must stop being open/parked — run 2
        // left COX-B006 parked while its merged fix sat on main.
        if let Ok(merged) = forge.recently_merged().await {
            for (number, head) in merged {
                // A merged release PR (`release/vX.Y.Z`) gets its tag now —
                // the merge IS the release; the tag is its immutable mark.
                if head.starts_with("release/v") {
                    self.tag_merged_release(&head).await;
                }
                let ticket = head.rsplit('/').next().unwrap_or(&head).to_owned();
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    if s.seen_merged_prs.contains(&number) {
                        return Ok(());
                    }
                    // Stamp the merge time — the fix-on-fix brake reads it.
                    // Only for branches that NAME A REAL TICKET: an operator
                    // or infra branch ("fix/heal-names-refs") minted a
                    // dangling ticket_last_merge orphan on every merge, which
                    // the write-boundary healer then scrubbed every 20 min.
                    if s.tickets.iter().any(|t| t.id().to_string() == ticket) {
                        s.ticket_last_merge
                            .insert(ticket.clone(), crate::state::now_rfc3339());
                    }
                    s.ticket_fail_attempts.remove(&ticket);
                    s.ticket_journal.remove(&ticket);
                    // Merged into main — the DEV work-session for this ticket is
                    // truly finished now (not at DEV-Done, which a review can
                    // still bounce back). Drop it so a future ticket never
                    // resumes a merged conversation.
                    let sess_prefix = format!("{ticket}/");
                    s.ticket_sessions
                        .retain(|k, _| !k.starts_with(&sess_prefix));
                    let mut note = None;
                    let Ok(tid) = coxagent_domain::TicketId::new(&ticket) else {
                        return Ok(());
                    };
                    if let Some(t) = s.ticket_mut(&tid) {
                        use coxagent_domain::{Role as R, Status, TicketType};
                        // Walk the LEGAL transition path with the proper actors
                        // — Open→Fixed directly does not exist, which silently
                        // stranded merged tickets Open (night bug). The terminal
                        // state depends on the TICKET TYPE, not the status: an
                        // in-progress bug goes to Fixed via DevBug, but an
                        // in-progress feature/chore goes to Done via DevFeature
                        // — trying DevBug→Fixed there failed forever, left the
                        // merged ticket InProgress, and DEV re-implemented work
                        // that was already on main (COX-C012, twice).
                        let bug = t.ticket_type() == TicketType::Bug;
                        let (role, terminal) = if bug {
                            (R::DevBug, Status::Fixed)
                        } else {
                            (R::DevFeature, Status::Done)
                        };
                        let moved = match t.status() {
                            Status::Open | Status::Ready => {
                                t.transition_to(role, Status::InProgress).is_ok()
                                    && t.transition_to(role, terminal).is_ok()
                            }
                            Status::InProgress => t.transition_to(role, terminal).is_ok(),
                            _ => false,
                        };
                        if moved {
                            note = Some(format!(
                                "✅ PR #{number} was merged by a human — {ticket} closed and \
                                 un-parked to match."
                            ));
                        }
                    }
                    // Mark processed only when the ticket actually reached a
                    // terminal state (or no longer exists) — a failed sync
                    // must retry next cycle, not be forgotten forever.
                    let done = note.is_some()
                        || s.ticket(&tid).map_or(true, |t| {
                            use coxagent_domain::Status;
                            matches!(
                                t.status(),
                                Status::Fixed
                                    | Status::Done
                                    | Status::Verified
                                    | Status::Documented
                                    | Status::Rejected
                            )
                        });
                    if done {
                        s.seen_merged_prs.insert(number);
                    }
                    if let Some(n) = note {
                        s.post_comment("SM", &n, Some(ticket.clone()));
                    }
                    Ok(())
                })
                .await;
            }
        }
        // 2. Learn from human-closed PRs (processed once each).
        if let Ok(closed) = forge.closed_unmerged().await {
            for (number, head) in closed {
                let fresh = self
                    .store
                    .load()
                    .await
                    .is_ok_and(|s| !s.seen_closed_prs.contains(&number));
                if !fresh {
                    continue;
                }
                // feat/COX-F012 → COX-F012
                let ticket = head.rsplit('/').next().unwrap_or(&head).to_owned();
                let lesson = format!(
                    "PR #{number} ({ticket}) was closed by a human WITHOUT merging — the approach                      was rejected, not the details. Re-read the ticket and redesign before recoding."
                );
                crate::prompts::record_hub_lesson(self.files.as_deref(), &lesson).await;
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    s.seen_closed_prs.insert(number);
                    s.add_lesson(&lesson);
                    s.journal_note(&ticket, &format!("human closed PR #{number} unmerged — redesign, don't recode"));
                    s.post_comment(
                        "SM",
                        &format!(
                            "🚫 PR #{number} closed by a human without merge — treating it as a                              redesign signal for {ticket}."
                        ),
                        Some(ticket.clone()),
                    );
                    Ok(())
                })
                .await;
            }
        }
    }
    /// SM → SA rescue for a stuck PR: instead of a third blind fix round (or
    /// dumping it on a human), the SA root-causes the PR and DECIDES —
    /// `CLOSE` (superseded / wrong direction) or `INSTRUCT` (concrete steps,
    /// left as review feedback so the normal fix loop picks them up with one
    /// informed retry). One rescue per PR, tracked in state.
    // One linear rescue pass: claim → investigate → verdict → apply.
    #[allow(clippy::too_many_lines)]
    pub(super) async fn sa_rescue_pr(&self, pr: &crate::ports::outbound::PullRequest) {
        let Some(forge) = self.forge.clone() else {
            return;
        };
        // Claim the rescue first so parallel operators don't double-spend.
        let claimed = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            if s.pr_rescues.contains_key(&pr.number) {
                return Err(crate::PortError::Conflict("already rescued".into()));
            }
            s.pr_rescues.insert(pr.number, 1);
            Ok(())
        })
        .await;
        if claimed.is_err() {
            return;
        }
        self.report("SA", &format!("root-causing stuck PR #{}", pr.number));
        let diff: String = forge
            .pr_diff(pr.number)
            .await
            .unwrap_or_default()
            .chars()
            .take(12_000)
            .collect();
        // Run the SA rescue in the leader's ISOLATED feedback worktree — the
        // shared checkout is dirty mid-cycle, so branch switching there aborts
        // and the rescue never lands (the stuck PR spins forever). The tree
        // shares repo refs, so checkout/commit/push work as on the main tree.
        let fix_dir = self
            .feedback_work_dir
            .clone()
            .unwrap_or_else(|| self.work_dir.clone());
        let request = crate::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::Sa,
            system_prompt: crate::prompts::system_prompt(crate::prompts::SA),
            task_prompt: format!(
                "PR #{n} (`{h}`) has failed TWO fix rounds and is blocking the merge queue. \
                 You are the architect deciding its fate — no more blind retries.\n\n\
                 TITLE: {t}\n\nDIFF (truncated):\n{diff}\n\n\
                 Investigate against the current base branch (you are in the repo). You are \
                 the last line of technical defense — prefer SOLVING it yourself. Reply with \
                 EXACTLY one of:\n\
                 FIXED — you already unblocked it YOURSELF in this run: checked out `{h}`, \
                 resolved the problem, ran the build/tests green, committed and pushed. \
                 (Do the work first, then reply FIXED.)\n\
                 CLOSE — the change is superseded by what already landed, or fundamentally \
                 wrong; closing loses nothing.\n\
                 INSTRUCT\n<numbered, concrete steps for a DEV — ONLY when the blocker is \
                 genuinely not technical (needs product/human input)>",
                n = pr.number,
                h = pr.head,
                t = pr.title,
            ),
            work_dir: fix_dir.clone(),
            timeout: std::time::Duration::from_secs(900),
            escalation_level: 0,
            label: None,
        };
        let out = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => o.stdout.trim().to_owned(),
            _ => String::new(),
        };
        // The SA may have switched branches while fixing — repark the checkout.
        if let Some(git) = &self.git {
            let _ = git.checkout_branch(&fix_dir, self.flow_base()).await;
        }
        let vi = self.config.workflow.language.is_vi();
        let say = |msg: String| {
            let store = Arc::clone(&self.store);
            async move {
                let _ = crate::ports::outbound::mutate_state(store.as_ref(), |s| {
                    s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                    Ok(())
                })
                .await;
            }
        };
        if out.starts_with("FIXED") {
            // Trust but verify — the SA's word passes the same gates as anyone's.
            match self.verify_conflict_resolution(pr.number).await {
                Ok(()) => {
                    let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                        s.pr_fix_attempts.remove(&pr.number);
                        s.pr_sessions.remove(&pr.number);
                        s.pr_review_skips.remove(&pr.number);
                        Ok(())
                    })
                    .await;
                    say(if vi {
                        format!("🧯 SM→SA rescue PR #{}: SA TỰ XỬ xong — verification pass, chờ merge sweep.", pr.number)
                    } else {
                        format!("🧯 SM→SA rescue PR #{}: SA fixed it directly — verification passed, awaiting the merge sweep.", pr.number)
                    })
                    .await;
                }
                Err(why) => {
                    say(if vi {
                        format!("🧯 SM→SA rescue PR #{}: SA báo FIXED nhưng verification từ chối ({why}) — chuyển người quyết.", pr.number)
                    } else {
                        format!("🧯 SM→SA rescue PR #{}: SA said FIXED but verification refused it ({why}) — escalating to a person.", pr.number)
                    })
                    .await;
                }
            }
        } else if out.starts_with("CLOSE") {
            let _ = forge
                .comment_pr(
                    pr.number,
                    "Closed by SA rescue: superseded/wrong direction — see queue history.",
                )
                .await;
            if forge.close_pr(pr.number).await.is_ok() {
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    s.seen_closed_prs.insert(pr.number);
                    Ok(())
                })
                .await;
                say(if vi {
                    format!(
                        "🧯 SM→SA rescue PR #{}: SA kết luận ĐÓNG (đã bị thay thế/sai hướng).",
                        pr.number
                    )
                } else {
                    format!(
                        "🧯 SM→SA rescue PR #{}: SA ruled CLOSE (superseded / wrong direction).",
                        pr.number
                    )
                })
                .await;
            }
        } else if let Some(steps) = out.strip_prefix("INSTRUCT") {
            let steps = steps.trim();
            let _ = forge
                .request_changes(
                    pr.number,
                    &format!("SA rescue instructions (follow EXACTLY):\n{steps}"),
                )
                .await;
            let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                s.pr_fix_attempts.insert(pr.number, 0); // one informed retry
                Ok(())
            })
            .await;
            say(if vi {
                format!("🧯 SM→SA rescue PR #{}: SA để lại chỉ dẫn cụ thể — DEV được một vòng thử lại có định hướng.", pr.number)
            } else {
                format!("🧯 SM→SA rescue PR #{}: SA left concrete instructions — DEV gets one guided retry.", pr.number)
            })
            .await;
        } else {
            say(if vi {
                format!("🧯 SM→SA rescue PR #{}: SA không kết luận được — chuyển người quyết.", pr.number)
            } else {
                format!("🧯 SM→SA rescue PR #{}: SA could not reach a verdict — escalating to a person.", pr.number)
            })
            .await;
        }
    }
    /// Build and test what main would actually become: the PR's head merged
    /// into the target, in a throwaway worktree. This is the check CI would do
    /// and the one thing standing between auto-merge and a red main, since a
    /// branch's own green run says nothing about a target that has moved since.
    ///
    /// The distinction that matters: a project with no git and no test runner
    /// wired has nothing to verify, and refusing every merge forever would help
    /// nobody — that skips, loudly. A project that HAS a suite which then fails,
    /// or cannot be run, blocks: that is a result, not an absent capability.
    pub(super) async fn verify_merged_result(
        &self,
        head: &str,
        target: &str,
    ) -> Result<(), String> {
        let (Some(git), Some(deploy)) = (&self.git, &self.deploy) else {
            tracing::warn!("auto-merge: no git/test runner configured — merging {head} unverified");
            return Ok(());
        };
        let dir = std::env::temp_dir().join(format!(
            "cox-premerge-{}-{}",
            std::process::id(),
            head.replace('/', "-")
        ));
        // A crashed previous verification can leave this worktree behind and
        // registered; removing through the port both prunes the registration
        // and deletes the directory, so the add below starts clean.
        let _ = git.worktree_remove(&self.work_dir, &dir).await;
        // Fetch both sides first: the worktree is created from the target and
        // the head is merged into it, exactly as the forge would.
        if !git
            .raw(&self.work_dir, &["fetch", "origin", head, target])
            .await
            .0
        {
            return Err(format!("could not fetch {head} and {target}"));
        }
        let sha = format!("origin/{target}");
        if let Err(e) = git.worktree_add(&self.work_dir, &dir, &sha).await {
            return Err(format!("worktree for {target}: {e}"));
        }
        let (merge_ok, _) = git
            .raw(&dir, &["merge", "--no-edit", &format!("origin/{head}")])
            .await;
        let outcome = if merge_ok {
            match deploy.run_tests(&dir).await {
                // `deployed=false` means no toolchain was recognised: there is
                // no suite to be red.
                Ok(r) if r.success || !r.deployed => {
                    // UI-touching merges also face the browser suite: with
                    // hosted CI dead (billing), SA prose review was the ONLY
                    // gate between a broken screen and main. Golden
                    // screenshots + the console-error gate run here instead.
                    let (ok_diff, names) =
                        git.raw(&dir, &["diff", "--name-only", &sha, "HEAD"]).await;
                    let touches_ui = ok_diff
                        && names.lines().any(|l| {
                            l.starts_with("crates/presentation/src/web/") || l.starts_with("e2e/")
                        });
                    if touches_ui {
                        let seed = self.work_dir.join("e2e").join("node_modules");
                        match deploy.run_e2e(&dir, Some(seed.as_path())).await {
                            Ok(r) if r.success || !r.deployed => Ok(()),
                            Ok(r) => Err(format!(
                                "browser e2e fails on the merged tree: {}",
                                r.summary.chars().take(300).collect::<String>()
                            )),
                            Err(e) => Err(format!("could not run the e2e suite: {e}")),
                        }
                    } else {
                        Ok(())
                    }
                }
                Ok(r) => Err(format!(
                    "tests fail on the merged tree: {}",
                    r.summary.chars().take(300).collect::<String>()
                )),
                // A runner that cannot start is not a red suite, but it is not
                // a green one either.
                Err(e) => Err(format!("could not run the suite on the merged tree: {e}")),
            }
        } else {
            Err(format!(
                "merging {head} into {target} does not apply cleanly"
            ))
        };
        let _ = git.worktree_remove(&self.work_dir, &dir).await;
        outcome
    }

    /// Execute a human-ordered force-merge ON THE RUNNER: resolve conflicts on
    /// the PR branch, verify (markers + forge-mergeable), merge, and narrate to
    /// #agents. Same machinery as address_pr_feedback, same hard gates.
    pub(super) async fn force_merge_job(&self, num: u64, by: &str) {
        let vi = self.config.workflow.language.is_vi();
        let Some(forge) = self.forge.clone() else {
            return;
        };
        self.report("SA", &format!("force-merging PR #{num} for {by}"));
        let Ok(prs) = forge.list_open_prs().await else {
            return;
        };
        let Some(pr) = prs.into_iter().find(|p| p.number == num) else {
            self.say_agents(if vi {
                format!("⚡ Force-merge #{num} ({by}): PR không còn mở — bỏ qua.")
            } else {
                format!("⚡ Force-merge #{num} ({by}): the PR is no longer open — skipping.")
            })
            .await;
            return;
        };
        if !pr.mergeable
            && !self
                .resolve_conflicts_for_force_merge(num, by, vi, &pr)
                .await
        {
            return;
        }
        match forge.merge_pr(num).await {
            Ok(()) => {
                self.say_agents(if vi {
                    format!("⚡ Force-merge #{num} ({by}): ✅ đã merge.")
                } else {
                    format!("⚡ Force-merge #{num} ({by}): ✅ merged.")
                })
                .await;
            }
            Err(e) => {
                self.say_agents(if vi {
                    format!("⚡ Force-merge #{num}: merge bị từ chối — {e}: {}", pr.url)
                } else {
                    format!(
                        "⚡ Force-merge #{num}: the merge was refused — {e}: {}",
                        pr.url
                    )
                })
                .await;
            }
        }
    }
    /// Post to #agents as the SA — the narration channel every force-merge leg
    /// reports through.
    async fn say_agents(&self, msg: String) {
        let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
            s.post_chat_in("SA", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            Ok(())
        })
        .await;
    }
    /// The `!mergeable` leg of a force-merge: narrate, drive a DevBug agent to
    /// resolve every conflict on the PR branch, then PROVE the resolution
    /// (markers + forge-mergeable) before the merge is attempted. `false` means
    /// the PR must be abandoned — the failure is already narrated.
    async fn resolve_conflicts_for_force_merge(
        &self,
        num: u64,
        by: &str,
        vi: bool,
        pr: &crate::ports::outbound::PullRequest,
    ) -> bool {
        self.say_agents(if vi {
            format!(
                "⚡ Force-merge #{num} ({by}): runner đang gỡ conflict trên `{}`…",
                pr.head
            )
        } else {
            format!(
                "⚡ Force-merge #{num} ({by}): the runner is resolving conflicts on `{}`…",
                pr.head
            )
        })
        .await;
        let request = crate::ports::outbound::AgentRequest {
            role: coxagent_domain::Role::DevBug,
            system_prompt: crate::prompts::system_prompt(crate::prompts::DEV),
            task_prompt: format!(
                "URGENT: a human ordered PR #{num} (branch `{h}`) force-merged. It has merge \
                 conflicts with `{b}`.\n\
                 1. `git fetch origin && git checkout {h} && git pull origin {h}`\n\
                 2. `git merge origin/{b}` and resolve EVERY conflict, preserving both this \
                 branch's fix and what already landed on {b}.\n\
                 3. Run the build/tests to make sure nothing broke.\n\
                 4. `git add -A && git commit -m \"fix: resolve conflicts for #{num}\"` then \
                 `git push origin {h}`.",
                h = pr.head,
                b = pr.base,
            ),
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(1800),
            escalation_level: 0,
            label: None,
        };
        let ok = matches!(self.engine.run(request).await, Ok(o) if o.succeeded());
        if let Some(git) = &self.git {
            let _ = git.checkout_branch(&self.work_dir, self.flow_base()).await;
        }
        if !ok {
            self.say_agents(if vi {
                format!(
                    "⚡ Force-merge #{num}: gỡ conflict THẤT BẠI — cần xử lý tay: {}",
                    pr.url
                )
            } else {
                format!(
                    "⚡ Force-merge #{num}: conflict resolution FAILED — needs a manual fix: {}",
                    pr.url
                )
            })
            .await;
            return false;
        }
        if let Err(why) = self.verify_conflict_resolution(num).await {
            self.say_agents(if vi {
                format!(
                    "⚡ Force-merge #{num}: verification từ chối ({why}) — KHÔNG merge: {}",
                    pr.url
                )
            } else {
                format!(
                    "⚡ Force-merge #{num}: verification refused ({why}) — NOT merging: {}",
                    pr.url
                )
            })
            .await;
            return false;
        }
        true
    }
    /// PROVE a conflict "resolution" actually worked — never trust the engine's
    /// word for it. (1) The pushed diff must contain no conflict markers;
    /// (2) the forge must report the PR mergeable again (GitHub recomputes
    /// lazily, so poll briefly). Returns the failure reason otherwise.
    pub(super) async fn verify_conflict_resolution(&self, num: u64) -> Result<(), String> {
        let Some(forge) = &self.forge else {
            return Ok(());
        };
        if let Ok(diff) = forge.pr_diff(num).await {
            if diff_has_conflict_markers(&diff) {
                return Err(
                    "the pushed diff still contains git conflict markers (<<<<<<< / >>>>>>>)"
                        .to_owned(),
                );
            }
        }
        for wait in [10u64, 20, 30] {
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            if let Ok(prs) = forge.list_open_prs().await {
                match prs.iter().find(|p| p.number == num) {
                    // Merged or closed in the meantime — done either way.
                    None => return Ok(()),
                    Some(p) if p.mergeable => return Ok(()),
                    Some(_) => {}
                }
            }
        }
        Err("GitHub still reports the branch conflicted after the fix".to_owned())
    }

    /// Stale-PR policy: a PR that got request-changes, then sat for
    /// `workflow.pr_stale_days` with an UNCHANGED head, gets one forced
    /// rescue attempt; if the head still does not move by the next sweep it
    /// is closed with a comment pointing back at the ticket — an abandoned
    /// branch must not squat the queue (five did, for a day).
    pub(super) async fn sweep_stale_prs(&self) {
        let Some(forge) = &self.forge else { return };
        let Ok(prs) = forge.list_open_prs().await else {
            return;
        };
        let Ok(state) = self.store.load().await else {
            return;
        };
        let reviews = self.reporter().fetch_reviews().await;
        let stale_secs = self.config.workflow.pr_stale_days() * 86_400;
        for pr in prs {
            let Some(review) = reviews
                .iter()
                .find(|r| r.number == pr.number && r.decision == "request_changes")
            else {
                continue;
            };
            let age = super::seconds_since(&review.at).unwrap_or(0);
            if age < stale_secs || review.head_sha.is_empty() {
                continue;
            }
            // Head moved since the verdict? Not stale — the dedup gate will
            // re-review it.
            let cur = self.pr_head_sha(&pr.head).await;
            if cur != review.head_sha {
                continue;
            }
            // A PR that committed the agents' own scratch is wrong by
            // construction — and the pollution is exactly what pushes it past
            // the size bound below, so it stops being auto-landable AND
            // auto-closable and parks forever. Close it first: the ticket goes
            // back to the queue and is redone from a clean base.
            if let Ok(diff) = forge.pr_diff(pr.number).await {
                if let Some(path) = crate::use_cases::merge_policy::commits_scratch(&diff) {
                    let note = format!(
                        "Closing: this branch commits `{path}` — agent scratch that does not \
                         belong in the product's history. No review round fixes that. The work \
                         is not lost: the ticket returns to the queue and is redone on a fresh \
                         branch off current main."
                    );
                    let _ = forge.comment_pr(pr.number, &note).await;
                    if forge.close_pr(pr.number).await.is_ok() {
                        self.announce_pr_close(
                            pr.number,
                            &pr.title,
                            &format!(
                                "stale + commits agent scratch ({path}); ticket returns to the queue"
                            ),
                        )
                        .await;
                        self.log_git(&format!(
                            "stale sweep: closed PR #{} — commits scratch ({path})",
                            pr.number
                        ))
                        .await;
                    }
                    continue;
                }
                // Never touch work a human is deliberately sitting on.
                if crate::use_cases::merge_policy::needs_human_eyes(
                    &diff,
                    self.config.git.max_changed_lines,
                )
                .is_some()
                {
                    continue;
                }
            }
            let rescued = state
                .pr_rescue_attempts
                .get(&pr.number.to_string())
                .copied()
                .unwrap_or(0);
            if rescued == 0 {
                self.log_git(&format!(
                    "stale sweep: PR #{} idle {}d — forcing one rescue",
                    pr.number,
                    age / 86_400
                ))
                .await;
                self.sa_rescue_pr(&pr).await;
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                    s.pr_rescue_attempts.insert(pr.number.to_string(), 1);
                    Ok(())
                })
                .await;
            } else {
                let note = format!(
                    "Closing: no new commits for {}d after request-changes and one rescue \
                     attempt. The work is NOT lost — reopen from the ticket ({}) with a fresh \
                     branch off current main.",
                    age / 86_400,
                    pr.title
                );
                let _ = forge.comment_pr(pr.number, &note).await;
                if forge.close_pr(pr.number).await.is_ok() {
                    let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), |s| {
                        s.seen_closed_prs.insert(pr.number);
                        Ok(())
                    })
                    .await;
                }
                self.log_git(&format!("stale sweep: closed idle PR #{}", pr.number))
                    .await;
            }
        }
    }
}
