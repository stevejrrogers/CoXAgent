//! The forge: how agent branches become main. Review, merge (with the
//! merged-tree verification), conflict rescue, hygiene over the open queue.
//!
//! Split from the cycle orchestrator: every PR-flow change used to conflict
//! with every ceremony change purely by sharing a file.

use super::RunCycleUseCase;
use crate::ports::outbound::{AgentEnginePort, AgentRequest, GitAuthor, StateStorePort};
use coxagent_domain::TicketId;
use std::path::PathBuf;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
    /// Ship a just-completed ticket through the git flow, when enabled:
    /// commit the work on a per-ticket branch (`feat/<id>`, stacked on the
    /// current tip so nothing is lost while PRs await review), push it, and —
    /// Finish an in-progress merge of `base` into `branch`: a DEV engine call
    /// reads both sides of every conflicted file, resolves preserving both
    /// intents, and completes the merge commit. Returns true only when the
    /// resolution is VERIFIED: no conflict markers left in the previously
    /// conflicted files and the base tip is an ancestor of HEAD.
    pub(super) async fn resolve_merge_in_progress(
        &self,
        branch: &str,
        base: &str,
        files: &[String],
    ) -> bool {
        self.report(
            "DEV-BUG",
            &format!("resolving {base} conflicts on {branch}"),
        );
        let listing = files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        let task = format!(
            "A `git merge origin/{base}` into branch `{branch}` is IN PROGRESS in this working \
             directory and stopped on conflicts in:\n{listing}\n\n\
             Resolve the merge INTELLIGENTLY:\n\
             1. For every conflicted file, read BOTH sides and understand what each change is \
             trying to do — then produce a resolution that preserves the intent of BOTH the \
             branch's work and what landed on {base}. Never blindly pick one side.\n\
             2. Remove every conflict marker (<<<<<<< ======= >>>>>>>), `git add` the files, and \
             complete the merge commit (`git commit --no-edit`).\n\
             3. Make sure the project still builds and its tests pass; fix fallout from the merge \
             if needed (as additional commits on this branch).\n\
             Do NOT switch branches, do NOT push, do NOT abort the merge."
        );
        let request = AgentRequest {
            role: coxagent_domain::Role::DevBug,
            system_prompt: crate::prompts::system_prompt(crate::prompts::DEV),
            task_prompt: task,
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(1200),
            escalation_level: 0,
            label: None,
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return false;
        };
        if !outcome.succeeded() {
            return false;
        }
        // Trust nothing: verify markers are gone from the files git flagged.
        // Read through the port (`git show :file` = index content) — the same
        // truth the commit will carry, and no application-layer fs.
        let Some(git) = &self.git else { return false };
        for f in files {
            {
                let (_, text) = git.raw(&self.work_dir, &["show", &format!(":{f}")]).await;
                if text
                    .lines()
                    .any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> "))
                {
                    return false;
                }
            }
        }
        // …and that the merge actually completed (base tip now an ancestor).
        let Some(git) = &self.git else { return false };
        matches!(
            git.sync_base(&self.work_dir, base).await,
            Ok(crate::ports::outbound::SyncBase::UpToDate)
        )
    }
    /// when `auto_pr` — open a PR into the default branch for a human to review.
    /// Best-effort at every step: any failure is logged and never stalls the
    /// cycle. `kind` is `feat`/`fix`.
    #[allow(clippy::too_many_lines)] // linear best-effort git/PR pipeline
    pub(super) async fn commit_for_ticket(&self, id: &TicketId, kind: &str) {
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
        // Self-heal: an earlier agent run may have left a worktree behind that
        // is still squatting this branch (abandoned opencode temp checkouts are
        // never `worktree remove`d). Git then refuses every checkout of the
        // branch and the ticket is mis-assigned forever. Release the leftover
        // worktree once and retry before giving up.
        let mut result = git.checkout_branch(&self.work_dir, &branch).await;
        if let Err(err) = &result {
            if let Some(collision) = worktree_path_in_use(&err.to_string()) {
                self.log_git(&format!(
                    "branch {branch} held by leftover worktree {collision:?} — releasing and retrying"
                ))
                .await;
                let _ = git
                    .worktree_remove(&self.work_dir, &collision)
                    .await;
                result = git.checkout_branch(&self.work_dir, &branch).await;
            }
        }
        if let Err(e) = result {
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

        // Law: PRs are born mergeable. Bring the latest base INTO the branch
        // before pushing; a conflict is read, understood, and resolved HERE on
        // the branch — never left for the queue to discover.
        let base = self.flow_base().to_owned();
        match git.sync_base(&self.work_dir, &base).await {
            Ok(crate::ports::outbound::SyncBase::UpToDate) => {}
            Ok(crate::ports::outbound::SyncBase::Merged) => {
                self.log_git(&format!("merged latest {base} into {branch}"))
                    .await;
            }
            Ok(crate::ports::outbound::SyncBase::Conflicts(files)) => {
                self.log_git(&format!(
                    "{branch}: {} file(s) conflict with {base} — resolving on the branch",
                    files.len()
                ))
                .await;
                if self.resolve_merge_in_progress(&branch, &base, &files).await {
                    self.log_git(&format!(
                        "{branch}: conflicts with {base} resolved in place"
                    ))
                    .await;
                } else {
                    // Never push a half-done merge: restore the branch and let
                    // the drain loop (which re-runs this law) pick it up.
                    let _ = git.abort_merge(&self.work_dir).await;
                    self.log_git(&format!(
                        "{branch}: conflict resolution failed — merge aborted, drain will retry"
                    ))
                    .await;
                }
            }
            Err(e) => {
                self.log_git(&format!("sync {base} into {branch} failed: {e}"))
                    .await;
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
                        // Publish the opened PR to the hub so every dashboard
                        // (any machine, any user) can show it — the runner owns
                        // the forge credentials, the hub just persists.
                        self.reporter()
                            .report_pr(crate::ports::outbound::PrOpen::from(pr))
                            .await;
                    }
                    Err(e) => self.log_git(&format!("open PR for {id} failed: {e}")).await,
                }
            }
        }
    }
    /// End-of-cycle safety net ("agents don't sleep"). The normal ship path
    /// commits a completed ticket inline right after DEV returns — but if that
    /// window is interrupted (a process crash between the Done/Fixed transition
    /// and [`Self::commit_for_ticket`], or a transient git failure inside it),
    /// verified-green work can sit UNCOMMITTED on main indefinitely.
    ///
    /// This re-runs shipping at the END of each leader cycle for exactly the
    /// tickets whose Definition-of-Done formally passed (status `Done` /
    /// `Fixed`) — "clearly-finished feature/bug work". Calling [`Self::commit_
    /// for_ticket`] on them is idempotent: once their residue is committed on
    /// their own branch, `commit_all` reports nothing changed and returns early,
    /// so an already-shipped ticket is never re-pushed or double-PR'd.
    ///
    /// Scope guardrail: only terminal-success statuses are swept. Tickets still
    /// `Ready`/`Open`, human-assigned tickets, and unclaimed scratch edits from
    /// self-heal/boot passes have NO formal pass signal, so sweeping them would
    /// risk committing arbitrary or concurrent-workstream edits — we deliberately
    /// leave those alone.
    pub(super) async fn sweep_unshipped_work(&self) {
        if !self.config.git.enabled {
            return;
        }
        let Some(git) = &self.git else { return };
        if !git.is_repo(&self.work_dir).await {
            return;
        }
        // Respect PR-queue backpressure: sweeping new branches while over the WIP
        // limit would only multiply merge conflicts — same law as normal dev work.
        if self.pr_queue_full().await {
            return;
        }
        // One snapshot of the uncommitted tree; nothing to ship when it's clean.
        let Ok(tree) = git.working_tree(&self.work_dir).await else {
            return;
        };
        if tree.changed_paths.is_empty() {
            return;
        }
        let Ok(state) = self.store.load().await else {
            return;
        };
        for (id, kind) in unshipped_candidates(&state) {
            self.commit_for_ticket(&id, kind).await;
        }
    }

    /// Whether the open-PR queue blocks NEW branch work. Normally that's the
    /// WIP limit (`git.max_open_prs`); but when the team is about to
    /// RESTRUCTURE (refactor mode, or a sprint goal that says so), the bar is a
    /// CLEAN BASE — every open PR must merge or close first, because branches
    /// cut before a restructure can never merge sanely after it.
    pub(super) async fn pr_queue_full(&self) -> bool {
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
        // Only PRs the AGENTS can still act on count against the queue. A PR
        // that exhausted the fix→rescue ladder has been handed to a human
        // (pr_stuck in the inbox) — counting it here left the clean-base gate
        // waiting on a PR no agent may touch, which locked ALL dev work behind
        // one human decision indefinitely.
        let handed_off: std::collections::BTreeSet<u64> = self
            .store
            .load()
            .await
            .map(|s| {
                s.pr_fix_attempts
                    .iter()
                    .filter(|(_, n)| **n > 2)
                    .map(|(k, _)| *k)
                    .collect()
            })
            .unwrap_or_default();
        let open = prs
            .iter()
            .filter(|p| p.base == target && !handed_off.contains(&p.number))
            .count();
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
    /// Open PRs into the flow base, when git+forge are configured.
    pub(super) async fn open_pr_count(&self) -> Option<usize> {
        if !self.config.git.enabled || self.config.git.max_open_prs == 0 {
            return None;
        }
        let forge = self.forge.as_ref()?;
        let prs = forge.list_open_prs().await.ok()?;
        let target = self.flow_base();
        Some(prs.iter().filter(|p| p.base == target).count())
    }
    pub(super) async fn announce_drain_hold(&self, open: usize) {
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
            s.post_comment("SM", &msg, None);
            s.post_chat_in("SM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
            Ok(())
        })
        .await;
        if ok.is_ok() {
            tracing::info!("SA announced clean-base drain hold for sprint {sprint_no}");
        }
    }
}

/// Extract the path of a worktree that git says is squatting a branch, from its
/// "already used by worktree at '<path>'" diagnostic. Pure string parsing — no
/// IO — so it is unit-testable without any port double. Used to release a
/// leftover worktree and retry a branch checkout.
fn worktree_path_in_use(err: &str) -> Option<PathBuf> {
    let marker = "is already used by worktree at '";
    let start = err.find(marker)? + marker.len();
    let path = err[start..].split('\'').next()?;
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// The tickets whose finished-but-uncommitted residue an end-of-cycle sweep may
/// ship: those whose Definition-of-Done formally passed (`Done` feature/chore,
/// `Fixed` bug). Ordered oldest-first so stranded work ships in commit order.
/// Pure over state — no IO — so it's unit-testable without any port double.
#[must_use]
fn unshipped_candidates(state: &crate::state::ProjectState) -> Vec<(TicketId, &'static str)> {
    use coxagent_domain::{Status, TicketType};
    // Only tickets whose Definition-of-Done formally passed are candidates:
    // their finished-but-uncommitted residue is unambiguous ("clearly-finished"
    // feature/bug work). Everything else is deliberately out of scope — a bare
    // Ready/Open ticket or an unclaimed scratch edit carries no proof of a pass.
    let mut matched: Vec<(TicketId, &'static str)> = state
        .tickets
        .iter()
        .filter(|t| t.assignee().is_none())
        .filter_map(|t| match (t.ticket_type(), t.status()) {
            (TicketType::Feature | TicketType::Chore, Status::Done) => {
                Some((t.id().clone(), "feat"))
            }
            (TicketType::Bug, Status::Fixed) => Some((t.id().clone(), "fix")),
            _ => None,
        })
        .collect();
    matched.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    matched
}

#[cfg(test)]
mod tests {
    use super::{unshipped_candidates, worktree_path_in_use};
    use crate::state::ProjectState;
    use coxagent_domain::{
        Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
    };

    fn ticket(id: &str, kind: TicketType) -> Ticket {
        Ticket::new(
            TicketId::new(id).expect("id"),
            kind,
            id,
            "desc",
            Priority::Medium,
            Complexity::Medium,
            false,
        )
        .expect("ticket")
    }

    /// Drive a feature/chore to Done through the valid lifecycle.
    fn done(mut t: Ticket) -> Ticket {
        t.set_technical_design(
            Role::Sa,
            TechnicalDesign {
                approach: "do it".to_owned(),
                ..TechnicalDesign::default()
            },
        )
        .expect("design");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.transition_to(Role::DevFeature, Status::InProgress)
            .expect("in progress");
        t.transition_to(Role::DevFeature, Status::Done)
            .expect("done");
        t
    }

    /// Drive a bug to Fixed through its valid lifecycle (Open -> InProgress -> Fixed).
    fn fixed(mut t: Ticket) -> Ticket {
        t.transition_to(Role::DevBug, Status::InProgress)
            .expect("in progress");
        t.transition_to(Role::DevBug, Status::Fixed).expect("fixed");
        t
    }

    #[test]
    fn done_feature_and_fixed_bug_surface_with_kinds() {
        let state = ProjectState {
            tickets: vec![
                done(ticket("C-1", TicketType::Feature)),
                fixed(ticket("C-2", TicketType::Bug)),
                ticket("C-3", TicketType::Feature), // Pending — not Done
                ticket("C-4", TicketType::Bug),     // Open — not Fixed
            ],
            ..ProjectState::default()
        };
        assert_eq!(
            unshipped_candidates(&state),
            vec![
                (TicketId::new("C-1").expect("id"), "feat"),
                (TicketId::new("C-2").expect("id"), "fix"),
            ]
        );
    }

    #[test]
    fn done_feature_assigned_to_human_is_excluded() {
        let mut t = done(ticket("C-1", TicketType::Feature));
        t.assign_to_human("alice");
        let state = ProjectState {
            tickets: vec![t],
            ..ProjectState::default()
        };
        assert!(unshipped_candidates(&state).is_empty());
    }

    #[test]
    fn candidates_are_sorted_by_ticket_id_ascending() {
        let state = ProjectState {
            tickets: vec![
                done(ticket("B-001", TicketType::Feature)),
                done(ticket("A-001", TicketType::Feature)),
            ],
            ..ProjectState::default()
        };
        assert_eq!(
            unshipped_candidates(&state),
            vec![
                (TicketId::new("A-001").expect("id"), "feat"),
                (TicketId::new("B-001").expect("id"), "feat"),
            ]
        );
    }

    #[test]
    fn worktree_path_in_use_extracts_the_colliding_path_from_git_error() {
        let err = "fatal: 'feat/CXA-B043' is already used by worktree at \
                   '/private/var/folders/8c/p43zxq5n49q853zck6hywldm0000gn/T/opencode/wt-b043'";
        assert_eq!(
            worktree_path_in_use(err).map(|p| p.to_string_lossy().into_owned()),
            Some(
                "/private/var/folders/8c/p43zxq5n49q853zck6hywldm0000gn/T/opencode/wt-b043"
                    .to_owned()
            )
        );
    }

    #[test]
    fn worktree_path_in_use_is_none_when_not_a_worktree_collision() {
        assert!(worktree_path_in_use("fatal: cannot lock ref").is_none());
        assert!(worktree_path_in_use("branch already exists").is_none());
        assert!(worktree_path_in_use("already used by worktree at ''").is_none());
    }
}
