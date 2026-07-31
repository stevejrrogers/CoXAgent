//! The forge: how agent branches become main. Review, merge (with the
//! merged-tree verification), conflict rescue, hygiene over the open queue.
//!
//! Split from the cycle orchestrator: every PR-flow change used to conflict
//! with every ceremony change purely by sharing a file.

use super::{diff_has_conflict_markers, ReviewVerdict, RunCycleUseCase};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, GitAuthor, StateStorePort};
use crate::use_cases::merge_policy::{competing_pr, needs_human_eyes};
use coxagent_domain::TicketId;
use std::fmt::Write as _;
use std::sync::Arc;

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
        };
        let Ok(outcome) = self.engine.run(request).await else {
            return false;
        };
        if !outcome.succeeded() {
            return false;
        }
        // Trust nothing: verify markers are gone from the files git flagged…
        for f in files {
            if let Ok(text) = std::fs::read_to_string(self.work_dir.join(f)) {
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
                    }
                    Err(e) => self.log_git(&format!("open PR for {id} failed: {e}")).await,
                }
            }
        }
    }
    /// When `auto_merge` is on, the SA agent deep-dives each open PR (reads the
    /// diff, judges correctness/completeness/safety) and either merges it or
    /// requests changes — the automated stand-in for a human reviewer. Gated by
    /// CI (never merges a failing or conflicting PR) and bounded per cycle to
    /// keep cost predictable. Best-effort throughout.
    #[allow(clippy::too_many_lines)] // one review pass: gate, judge, verify, land
    pub(super) async fn review_open_prs(&self) {
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
        // Ticket ids visible in the open queue, for the competing-PR check.
        let open_titles: Vec<(u64, String)> =
            prs.iter().map(|p| (p.number, p.title.clone())).collect();
        for pr in prs.into_iter().rev().take(batch) {
            // Only review PRs into the configured target branch; leave PRs aimed
            // elsewhere (e.g. an integration → main promotion) to humans.
            if pr.base != target {
                continue;
            }
            // With require_ci off (CI unavailable, e.g. Actions billing dead),
            // CI status is ignored entirely — local test/lint gates plus the
            // SA's diff judgement carry the review instead.
            let require_ci = self.config.git.require_ci;
            if require_ci && pr.ci == "pending" {
                continue; // wait for CI before judging
            }
            let blocked = if require_ci && pr.ci == "failing" {
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
                // Conflicts are resolved IN PLACE on the original branch by
                // address_pr_feedback — never filed as tickets: a ticket spawns
                // a NEW branch + PR, which is how a queue explodes.
                continue;
            }
            let Ok(diff) = forge.pr_diff(pr.number).await else {
                continue;
            };
            // Hard gate: a diff carrying committed conflict markers must NEVER
            // merge, no matter what the review says.
            if diff_has_conflict_markers(&diff) {
                let reason = "Committed git conflict markers found in the diff — the conflict \
                              was not actually resolved. Fix the affected files and push again.";
                let _ = forge.request_changes(pr.number, reason).await;
                self.record_review(pr.number, "request_changes", reason)
                    .await;
                self.log_git(&format!(
                    "SA blocked PR #{}: committed conflict markers",
                    pr.number
                ))
                .await;
                continue;
            }
            // Two open PRs solving the same ticket is a race, not twice the
            // work: whichever lands first leaves the other conflicting or
            // fixing it twice. It happened here — #17 and #18 were competing
            // fixes for one bug — so say so and let a human choose, rather than
            // letting arrival order decide.
            if let Some(other) = competing_pr(pr.number, &pr.title, &open_titles) {
                let reason = format!(
                    "PR #{other} is open for the same ticket. Two changes for one ticket race \
                     each other: the second to land conflicts or fixes it twice. Close one, or \
                     fold this into the other, before either merges."
                );
                let _ = forge.request_changes(pr.number, &reason).await;
                self.record_review(pr.number, "request_changes", &reason)
                    .await;
                self.log_git(&format!(
                    "SA held PR #{}: competes with #{other}",
                    pr.number
                ))
                .await;
                continue;
            }
            match self.sa_review(&pr.title, &pr.head, &diff).await {
                Some((true, summary)) => {
                    self.record_review(pr.number, "approve", &summary).await;
                    if auto_merge {
                        // Size and blast radius the machine should not decide
                        // alone: a change this large, or one that edits how the
                        // project builds and deploys itself, gets a human even
                        // when every gate is green.
                        if let Some(why) = needs_human_eyes(&diff) {
                            let msg = format!(
                                "Approved, but not auto-merging: {why}. Ask a human to land this."
                            );
                            let _ = forge.comment_pr(pr.number, &msg).await;
                            self.log_git(&format!(
                                "PR #{} approved but held for a human: {why}",
                                pr.number
                            ))
                            .await;
                            continue;
                        }
                        // The DoD gates ran on the agent's branch, against the
                        // base as it was then. Between that and now the target
                        // has moved, and a PR that was green on an older main
                        // can still break it — the failure CI would normally
                        // catch, which is not available here. Build and test
                        // the MERGED result before landing it.
                        if let Err(why) = self.verify_merged_result(&pr.head, target).await {
                            let msg = format!(
                                "Approved, but the merged result does not build/test clean: \
                                 {why}. Rebase on {target} and fix it there — nothing lands red."
                            );
                            let _ = forge.request_changes(pr.number, &msg).await;
                            self.record_review(pr.number, "request_changes", &msg).await;
                            self.log_git(&format!(
                                "PR #{} held: merged result failed verification",
                                pr.number
                            ))
                            .await;
                            continue;
                        }
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
    /// Persist the SA's verdict so the Review tab can show it as a suggestion.
    pub(super) async fn record_review(&self, number: u64, decision: &str, summary: &str) {
        if let Ok(mut s) = self.store.load().await {
            s.upsert_review(number, decision, summary);
            let _ = self.store.save(&s).await;
        }
    }
    /// From a unified diff, list the functions it touches and who calls them
    /// (from the code graph). Empty when no graph or nothing recognised — a
    /// best-effort blast-radius hint for the reviewer.
    pub(super) fn diff_impact(&self, diff: &str) -> String {
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
    pub(super) async fn sa_review(
        &self,
        title: &str,
        head: &str,
        diff: &str,
    ) -> Option<(bool, String)> {
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
            escalation_level: 0,
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
        let Some(forge) = &self.forge else {
            return;
        };
        let target = self.flow_base().to_owned();
        // 1. Rebase open PRs onto the moving base.
        if let Ok(prs) = forge.list_open_prs().await {
            let mut rebased: Vec<u64> = Vec::new();
            for pr in prs.iter().filter(|p| p.base == target).take(8) {
                let git = |args: &[&str]| {
                    let mut c = std::process::Command::new("git");
                    c.args(args).current_dir(&self.work_dir);
                    c.output().is_ok_and(|o| o.status.success())
                };
                if !git(&["fetch", "origin", &pr.head, &target]) {
                    continue;
                }
                let local = format!("refs/remotes/origin/{}", pr.head);
                let base_ref = format!("origin/{target}");
                // Already contains base? skip cheaply.
                let up_to_date = std::process::Command::new("git")
                    .args(["merge-base", "--is-ancestor", &base_ref, &local])
                    .current_dir(&self.work_dir)
                    .status()
                    .is_ok_and(|s| s.success());
                if up_to_date {
                    continue;
                }
                if git(&["checkout", "-B", &pr.head, &local])
                    && git(&["merge", &base_ref, "--no-edit"])
                {
                    if git(&["push", "origin", &pr.head]) {
                        rebased.push(pr.number);
                    }
                } else {
                    let _ = git(&["merge", "--abort"]);
                }
                let _ = git(&["checkout", &target]);
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
                let ls = std::process::Command::new("git")
                    .args(["ls-remote", "--heads", "origin"])
                    .current_dir(&self.work_dir)
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                    .unwrap_or_default();
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
                    let ahead = std::process::Command::new("git")
                        .args([
                            "rev-list",
                            "--count",
                            &format!("origin/{target}..origin/{branch}"),
                        ])
                        .current_dir(&self.work_dir)
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .and_then(|o| {
                            String::from_utf8_lossy(&o.stdout)
                                .trim()
                                .parse::<u64>()
                                .ok()
                        })
                        .unwrap_or(0);
                    if ahead == 0 {
                        continue;
                    }
                    let title = std::process::Command::new("git")
                        .args(["log", "-1", "--format=%s", &format!("origin/{branch}")])
                        .current_dir(&self.work_dir)
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| branch.clone());
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
                            crate::prompts::record_hub_lesson(&lesson);
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
                let ticket = head.rsplit('/').next().unwrap_or(&head).to_owned();
                let _ = crate::ports::outbound::mutate_state(self.store.as_ref(), move |s| {
                    if s.seen_merged_prs.contains(&number) {
                        return Ok(());
                    }
                    s.ticket_fail_attempts.remove(&ticket);
                    s.ticket_journal.remove(&ticket);
                    let mut note = None;
                    let Ok(tid) = coxagent_domain::TicketId::new(&ticket) else {
                        return Ok(());
                    };
                    if let Some(t) = s.ticket_mut(&tid) {
                        use coxagent_domain::{Role as R, Status};
                        // Walk the LEGAL transition path with the proper
                        // actors — Open→Fixed directly does not exist, which
                        // silently stranded merged tickets Open (night bug).
                        let moved = match t.status() {
                            Status::Open => {
                                t.transition_to(R::DevBug, Status::InProgress).is_ok()
                                    && t.transition_to(R::DevBug, Status::Fixed).is_ok()
                            }
                            Status::InProgress => t.transition_to(R::DevBug, Status::Fixed).is_ok(),
                            Status::Ready => {
                                t.transition_to(R::DevFeature, Status::InProgress).is_ok()
                                    && t.transition_to(R::DevFeature, Status::Done).is_ok()
                            }
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
                crate::prompts::record_hub_lesson(&lesson);
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
            work_dir: self.work_dir.clone(),
            timeout: std::time::Duration::from_secs(900),
            escalation_level: 0,
        };
        let out = match self.engine.run(request).await {
            Ok(o) if o.succeeded() => o.stdout.trim().to_owned(),
            _ => String::new(),
        };
        // The SA may have switched branches while fixing — repark the checkout.
        if let Some(git) = &self.git {
            let _ = git.checkout_branch(&self.work_dir, self.flow_base()).await;
        }
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
                        Ok(())
                    })
                    .await;
                    say(format!(
                        "🧯 SM→SA rescue PR #{}: SA TỰ XỬ xong — verification pass, chờ merge sweep.",
                        pr.number
                    ))
                    .await;
                }
                Err(why) => {
                    say(format!(
                        "🧯 SM→SA rescue PR #{}: SA báo FIXED nhưng verification từ chối ({why}) — chuyển người quyết.",
                        pr.number
                    ))
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
                say(format!(
                    "🧯 SM→SA rescue PR #{}: SA kết luận ĐÓNG (đã bị thay thế/sai hướng).",
                    pr.number
                ))
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
            say(format!(
                "🧯 SM→SA rescue PR #{}: SA để lại chỉ dẫn cụ thể — DEV được một vòng thử lại có định hướng.",
                pr.number
            ))
            .await;
        } else {
            say(format!(
                "🧯 SM→SA rescue PR #{}: SA không kết luận được — chuyển người quyết.",
                pr.number
            ))
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
        let _ = std::fs::remove_dir_all(&dir);
        // Fetch both sides first: the worktree is created from the target and
        // the head is merged into it, exactly as the forge would.
        let fetch = std::process::Command::new("git")
            .args(["fetch", "origin", head, target])
            .current_dir(&self.work_dir)
            .output();
        if !fetch.is_ok_and(|o| o.status.success()) {
            return Err(format!("could not fetch {head} and {target}"));
        }
        let sha = format!("origin/{target}");
        if let Err(e) = git.worktree_add(&self.work_dir, &dir, &sha).await {
            return Err(format!("worktree for {target}: {e}"));
        }
        let merged = std::process::Command::new("git")
            .args(["merge", "--no-edit", &format!("origin/{head}")])
            .current_dir(&dir)
            .output();
        let outcome = match merged {
            Ok(o) if o.status.success() => match deploy.run_tests(&dir).await {
                // `deployed=false` means no toolchain was recognised: there is
                // no suite to be red.
                Ok(r) if r.success || !r.deployed => Ok(()),
                Ok(r) => Err(format!(
                    "tests fail on the merged tree: {}",
                    r.summary.chars().take(300).collect::<String>()
                )),
                // A runner that cannot start is not a red suite, but it is not
                // a green one either.
                Err(e) => Err(format!("could not run the suite on the merged tree: {e}")),
            },
            Ok(o) => Err(format!(
                "merging {head} into {target} does not apply cleanly: {}",
                String::from_utf8_lossy(&o.stderr)
                    .chars()
                    .take(200)
                    .collect::<String>()
            )),
            Err(e) => Err(format!("merge failed to run: {e}")),
        };
        let _ = git.worktree_remove(&self.work_dir, &dir).await;
        let _ = std::fs::remove_dir_all(&dir);
        outcome
    }
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
                        format!(
                            "PR #{} vẫn kẹt SAU khi SA đã rescue — cần người quyết: {}",
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
                        sid,
                        &task_prompt,
                        &self.work_dir,
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
                        work_dir: self.work_dir.clone(),
                        timeout: std::time::Duration::from_secs(1800),
                        // Each prior fix round escalates the model ladder.
                        escalation_level: u8::try_from(attempts.min(3)).unwrap_or(3),
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
            // the shared work_dir back on the base branch for the next stage.
            if let Some(git) = &self.git {
                let _ = git.checkout_branch(&self.work_dir, target).await;
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
    /// Execute a human-ordered force-merge ON THE RUNNER: resolve conflicts on
    /// the PR branch, verify (markers + forge-mergeable), merge, and narrate to
    /// #agents. Same machinery as address_pr_feedback, same hard gates.
    pub(super) async fn force_merge_job(&self, num: u64, by: &str) {
        let Some(forge) = self.forge.clone() else {
            return;
        };
        self.report("SA", &format!("force-merging PR #{num} for {by}"));
        let say = |msg: String| {
            let store = Arc::clone(&self.store);
            async move {
                let _ = crate::ports::outbound::mutate_state(store.as_ref(), |s| {
                    s.post_chat_in("SA", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                    Ok(())
                })
                .await;
            }
        };
        let Ok(prs) = forge.list_open_prs().await else {
            return;
        };
        let Some(pr) = prs.into_iter().find(|p| p.number == num) else {
            say(format!(
                "⚡ Force-merge #{num} ({by}): PR không còn mở — bỏ qua."
            ))
            .await;
            return;
        };
        if !pr.mergeable {
            say(format!(
                "⚡ Force-merge #{num} ({by}): runner đang gỡ conflict trên `{}`…",
                pr.head
            ))
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
            };
            let ok = matches!(self.engine.run(request).await, Ok(o) if o.succeeded());
            if let Some(git) = &self.git {
                let _ = git.checkout_branch(&self.work_dir, self.flow_base()).await;
            }
            if !ok {
                say(format!(
                    "⚡ Force-merge #{num}: gỡ conflict THẤT BẠI — cần xử lý tay: {}",
                    pr.url
                ))
                .await;
                return;
            }
            if let Err(why) = self.verify_conflict_resolution(num).await {
                say(format!(
                    "⚡ Force-merge #{num}: verification từ chối ({why}) — KHÔNG merge: {}",
                    pr.url
                ))
                .await;
                return;
            }
        }
        match forge.merge_pr(num).await {
            Ok(()) => say(format!("⚡ Force-merge #{num} ({by}): ✅ đã merge.")).await,
            Err(e) => {
                say(format!(
                    "⚡ Force-merge #{num}: merge bị từ chối — {e}: {}",
                    pr.url
                ))
                .await;
            }
        }
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
