// Part of the cycle module split by concern — see cycle/mod.rs.
//! SA reviews open PRs: verdicts, review records, blast-radius hints.

use super::{diff_has_conflict_markers, ReviewVerdict, RunCycleUseCase};
use crate::ports::outbound::{AgentEnginePort, AgentRequest, StateStorePort};
use crate::use_cases::merge_policy::{
    changed_files, competing_pr, needs_human_eyes, CompeteCandidate, CompeteOutcome,
    resolve_competing,
};
use std::fmt::Write as _;

impl<S: StateStorePort, E: AgentEnginePort> RunCycleUseCase<S, E> {
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
        // Nothing open → say nothing. Reporting "reviewing PRs" here with an
        // empty queue is what made the SA card look busy-but-idle.
        if prs.iter().all(|p| p.base != self.flow_base()) {
            return;
        }
        self.report("SA", "reviewing PRs");
        let target = self.flow_base();
        // With full merge authority (auto_merge on) the SA owns the queue and
        // works it hard — draining the pile-up — rather than nibbling a few PRs.
        // As a suggestion-only reviewer it stays light. Bounded either way for cost.
        let batch = if auto_merge { 12 } else { 3 };
        // Tickets whose fix ALREADY landed. A bug sits at `fixed` until someone
        // verifies it, which is live work — so ticket status alone cannot tell
        // a redundant PR from a real one. What settles it is the forge: if a PR
        // for this ticket is already merged, a second branch for it is building
        // what main has. #36 was exactly that, opened hours after #34 merged.
        let merged: Vec<(u64, String)> = forge.recently_merged().await.unwrap_or_default();
        let merged_tickets: std::collections::BTreeSet<String> = merged
            .iter()
            .filter_map(|(_, head)| crate::use_cases::merge_policy::ticket_id_in(head))
            .collect();
        // Ticket ids visible in the open queue, for the competing-PR check.
        let open_titles: Vec<(u64, String)> =
            prs.iter().map(|p| (p.number, p.title.clone())).collect();
        // Mergeability per open PR, so a competing-PR resolution can tell
        // whether the *other* PR (not this loop's current) can actually land.
        let open_mergeable: std::collections::HashMap<u64, bool> =
            prs.iter().map(|p| (p.number, p.mergeable)).collect();
        for pr in prs.into_iter().rev().take(batch) {
            // Only review PRs into the configured target branch; leave PRs aimed
            // elsewhere (e.g. an integration → main promotion) to humans.
            if pr.base != target {
                continue;
            }
            // These two run BEFORE the "head has not moved" guard below, and
            // that ordering is the whole point: a PR nobody should keep open
            // has a frozen head BY DEFINITION, so a check placed after the
            // guard never sees the PRs it exists for. Asked first, they cost
            // one diff read and end the queue's dead weight.
            // The ticket this PR names is already finished — by another PR, by
            // a human, by anything. Three such PRs held WIP slots here for a
            // week, blocking new dev work, for bugs that were verified days
            // earlier. Nobody had ever asked the board whether the work was
            // still wanted.
            if let Some(tid) = crate::use_cases::merge_policy::ticket_id_in(&pr.title) {
                if merged_tickets.contains(&tid) && !merged.iter().any(|(n, _)| *n == pr.number) {
                    let note = format!(
                        "Closing: a pull request for {tid} is already merged — this branch \
                         rebuilds what main has. Reopen only if something here is genuinely \
                         missing from the merged fix."
                    );
                    let _ = forge.comment_pr(pr.number, &note).await;
                    if forge.close_pr(pr.number).await.is_ok() {
                        self.log_git(&format!(
                            "review: closed PR #{} — {tid} already merged elsewhere",
                            pr.number
                        ))
                        .await;
                    }
                    continue;
                }
                let settled = self.store.load().await.ok().and_then(|s| {
                    s.tickets
                        .iter()
                        .find(|t| t.id().as_str() == tid)
                        .map(coxagent_domain::Ticket::status)
                });
                if matches!(
                    settled,
                    Some(
                        coxagent_domain::Status::Verified
                            | coxagent_domain::Status::Done
                            | coxagent_domain::Status::Documented
                            | coxagent_domain::Status::Rejected
                    )
                ) {
                    let note = format!(
                        "Closing: {tid} is already {:?} — this branch is superseded. Nothing is \
                         lost; the branch stays in git if any of it is ever wanted.",
                        settled.unwrap_or(coxagent_domain::Status::Done)
                    );
                    let _ = forge.comment_pr(pr.number, &note).await;
                    if forge.close_pr(pr.number).await.is_ok() {
                        self.log_git(&format!(
                            "review: closed PR #{} — {tid} already settled",
                            pr.number
                        ))
                        .await;
                    }
                    continue;
                }
            }
            // Scratch in the diff is wrong the moment it exists — there is no
            // staleness to wait out and no review round that fixes it. #28 was
            // ENTIRELY worktrees and state backups, claiming to fix a health
            // gate; the SA correctly refused it three times while it sat in the
            // queue holding a WIP slot.
            if let Ok(d) = forge.pr_diff(pr.number).await {
                if let Some(path) = crate::use_cases::merge_policy::commits_scratch(&d) {
                    let note = format!(
                        "Closing: this branch commits `{path}` — agent scratch, not product \
                         code. No review round fixes that. The ticket returns to the queue to \
                         be redone on a fresh branch off current main."
                    );
                    let _ = forge.comment_pr(pr.number, &note).await;
                    if forge.close_pr(pr.number).await.is_ok() {
                        self.log_git(&format!(
                            "review: closed PR #{} — commits scratch ({path})",
                            pr.number
                        ))
                        .await;
                    }
                    continue;
                }
            }
            // Skip PRs whose head has not moved since the last request-changes:
            // the verdict cannot change and the repeat comment is pure noise.
            let head_sha = self.pr_head_sha(&pr.head).await;
            if self.already_reviewed_at(pr.number, &head_sha).await {
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
                self.record_review(pr.number, "request_changes", &reason, &head_sha)
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
                self.record_review(pr.number, "request_changes", reason, &head_sha)
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
            // fixing it twice. Where the winner PROVABLY covers the loser, the
            // SA self-resolves — closes the duplicate and lets the winner ship,
            // instead of parking both on a human forever (which is also what
            // kept the scorecard's useful count at zero). It keeps the human
            // in the loop only when closing would DROP work or the change is
            // too load-bearing to auto-merge.
            if let Some(other) = competing_pr(pr.number, &pr.title, &open_titles) {
                // `diff` is the current PR's diff (already fetched and proven
                // conflict-marker-free by the hard gate above). Only the other
                // PR needs a fresh read.
                let Ok(other_diff) = forge.pr_diff(other).await else {
                    let reason = format!(
                        "PR #{other} is open for the same ticket. Could not read its diff to \
                         auto-resolve — close one or fold this into the other before either \
                         merges."
                    );
                    let _ = forge.request_changes(pr.number, &reason).await;
                    self.record_review(pr.number, "request_changes", &reason, &head_sha)
                        .await;
                    self.log_git(&format!(
                        "SA held PR #{}: competes with #{other} (unreadable)",
                        pr.number
                    ))
                    .await;
                    continue;
                };
                let max_changed_lines = self.config.git.max_changed_lines;
                let cand = |n: u64, d: &str| CompeteCandidate {
                    number: n,
                    files: Some(changed_files(d)),
                    unsafe_change: needs_human_eyes(d, max_changed_lines).is_some()
                        || crate::use_cases::merge_policy::commits_scratch(d).is_some(),
                };
                match resolve_competing(cand(pr.number, &diff), cand(other, &other_diff)) {
                    CompeteOutcome::MergeClose { winner, loser } => {
                        // Only close the duplicate once the winner can ACTUALLY
                        // land; otherwise the queue is left with no live PR.
                        let winner_ready = if winner == pr.number {
                            // this diff is conflict-free by the hard gate above
                            pr.mergeable
                        } else {
                            !diff_has_conflict_markers(&other_diff)
                                && open_mergeable.get(&winner).copied().unwrap_or(false)
                        };
                        if winner_ready {
                            let note = format!(
                                "Closing #{loser}: PR #{winner} already covers every file this \
                                 touches — a duplicate fix for the same ticket. Nothing here is \
                                 lost; the winner proceeds through review."
                            );
                            let _ = forge.comment_pr(loser, &note).await;
                            let _ = forge.close_pr(loser).await;
                            if loser == pr.number {
                                self.log_git(&format!(
                                    "SA resolved competing PRs: closed #{loser} (duplicate), \
                                     keeping #{winner}"
                                ))
                                .await;
                                continue;
                            }
                            // This PR is the winner: drop the duplicate, then fall
                            // through to the normal review that lands this PR.
                            self.log_git(&format!(
                                "SA resolved competing PRs: closed #{loser} (duplicate); \
                                 reviewing #{winner}"
                            ))
                            .await;
                        } else {
                            let reason = format!(
                                "PR #{other} is open for the same ticket. The preferred fix \
                                 #{winner} is not currently landable (conflict/CI), so this is \
                                 left for a person rather than closing the only candidate."
                            );
                            let _ = forge.request_changes(pr.number, &reason).await;
                            self.record_review(pr.number, "request_changes", &reason, &head_sha)
                                .await;
                            self.log_git(&format!(
                                "SA held PR #{}: competes with #{other}",
                                pr.number
                            ))
                            .await;
                            continue;
                        }
                    }
                    CompeteOutcome::Proceed { winner, unsafe_other } => {
                        // The current PR is a SAFE, small subset of a
                        // load-bearing same-ticket competitor. It is not held
                        // hostage by that risk — it proceeds to the normal
                        // review below, where its own size/impact gates still
                        // apply, while the unsafe sibling waits for a person.
                        // (The resolver only ever yields Proceed for the safe
                        // PR, so `winner` is this PR; guard anyway.)
                        if winner != pr.number {
                            let reason = format!(
                                "PR #{other} is open for the same ticket. Holding for a person: \
                                 the safe candidate is not this PR. Close one or fold this in."
                            );
                            let _ = forge.request_changes(pr.number, &reason).await;
                            self.record_review(
                                pr.number,
                                "request_changes",
                                &reason,
                                &head_sha,
                            )
                            .await;
                            self.log_git(&format!(
                                "SA held PR #{}: competes with #{other}",
                                pr.number
                            ))
                            .await;
                            continue;
                        }
                        self.log_git(&format!(
                            "SA unblocked PR #{}: safe subset of load-bearing #{unsafe_other}; \
                             routing to normal review",
                            pr.number
                        ))
                        .await;
                    }
                    CompeteOutcome::Hold(why) => {
                        let reason = format!(
                            "PR #{other} is open for the same ticket. Holding for a person: \
                             {why}. Close one or fold this into the other."
                        );
                        let _ = forge.request_changes(pr.number, &reason).await;
                        self.record_review(pr.number, "request_changes", &reason, &head_sha)
                            .await;
                        self.log_git(&format!(
                            "SA held PR #{}: competes with #{other}",
                            pr.number
                        ))
                        .await;
                        continue;
                    }
                }
            }
            match self.sa_review(&pr.title, &pr.head, &diff).await {
                Some((true, summary)) => {
                    self.record_review(pr.number, "approve", &summary, &head_sha)
                        .await;
                    if auto_merge {
                        // Size and blast radius the machine should not decide
                        // alone: a change this large, or one that edits how the
                        // project builds and deploys itself, gets a human even
                        // when every gate is green.
                        if let Some(why) = needs_human_eyes(&diff, self.config.git.max_changed_lines) {
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
                            self.record_review(pr.number, "request_changes", &msg, &head_sha)
                                .await;
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
                    self.record_review(pr.number, "request_changes", &comment, &head_sha)
                        .await;
                    self.log_git(&format!("SA requested changes on PR #{}", pr.number))
                        .await;
                }
                None => {}
            }
        }
    }
    /// Persist the SA's verdict so the Review tab can show it as a suggestion.
    /// The PR head's current commit sha via `git ls-remote` — cheap, no
    /// checkout. Empty when it cannot be established (then no skip happens).
    pub(super) async fn pr_head_sha(&self, head: &str) -> String {
        let Some(git) = &self.git else {
            return String::new();
        };
        let (ok, out) = git
            .raw(
                &self.work_dir,
                &["ls-remote", "origin", &format!("refs/heads/{head}")],
            )
            .await;
        if !ok {
            return String::new();
        }
        out.split_whitespace().next().unwrap_or("").to_owned()
    }

    /// Whether this PR already got a request-changes at exactly this head —
    /// nothing new to judge until the DEV pushes. Reads the persisted reviews
    /// from the hub over HTTP (the runner never writes the shared DB directly).
    pub(super) async fn already_reviewed_at(&self, number: u64, head_sha: &str) -> bool {
        if head_sha.is_empty() {
            return false;
        }
        self.reporter().fetch_reviews().await.iter().any(|r| {
            r.number == number && r.decision == "request_changes" && r.head_sha == head_sha
        })
    }

    pub(super) async fn record_review(
        &self,
        number: u64,
        decision: &str,
        summary: &str,
        head_sha: &str,
    ) {
        self.reporter()
            .report_review(number, decision, summary, head_sha)
            .await;
    }
    /// From a unified diff, list the functions it touches and who calls them
    /// (from the code graph). Empty when no graph or nothing recognised — a
    /// best-effort blast-radius hint for the reviewer.
    pub(super) async fn diff_impact(&self, diff: &str) -> String {
        let Some(files) = self.files.as_deref() else {
            return String::new();
        };
        let Some(g) = crate::codegraph::CodeGraph::load(files, &self.work_dir).await else {
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
        let impact = self.diff_impact(diff).await;
        let task = format!(
            "You are the SA with full merge authority on this pull request. Do a deep code review \
             for correctness, completeness, safety, and architecture fit. You may APPROVE (which \
             merges it) only when ALL hold: the change is functionally correct and will keep the \
             build/tests green after merge; it carries adequate tests for what it changes; and the \
             code is clean (clear naming, no dead code, follows the repo's conventions and the \
             established architecture). If it merely works but is untested, sloppy, or drifts from \
             the architecture, REQUEST_CHANGES with the concrete fixes — a green diff is not enough, \
             the merged code must be good. Judge the diff against the codebase AS IT IS NOW, not as \
             it was when the branch was cut: open the files it touches and check the change still \
             fits — right module, current structure, current conventions. When it no longer fits \
             (moved code, dead paths, superseded patterns — any reason), REQUEST_CHANGES and say \
             concretely what to change and where that code lives now, so the DEV can fix and you \
             re-review the corrected PR on the next pass.\n\nPR: {title}\nBranch: {head}\n\nUnified diff:\n```\n\
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
            label: None,
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
}
