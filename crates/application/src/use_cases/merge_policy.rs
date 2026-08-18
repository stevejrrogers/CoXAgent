//! Merge and escalation POLICY — the decisions, with no IO in sight.
//!
//! Split out of `cycle.rs` (7,000+ lines) because policy is what changes when
//! the team learns something, and every ticket that touched it conflicted with
//! every other ticket in that file. These are pure functions over data: who
//! owns an answer, whether two pull requests are racing, whether a change is
//! too load-bearing to land unwatched.

/// The other open PR that names the same ticket, if any. Titles carry the id
/// (`fix(COX-B015): …`), which is how the team already labels its work.
#[must_use]
pub fn competing_pr(number: u64, title: &str, open: &[(u64, String)]) -> Option<u64> {
    let ticket = ticket_id_in(title)?;
    open.iter()
        .find(|(n, t)| *n != number && ticket_id_in(t).as_deref() == Some(ticket.as_str()))
        .map(|(n, _)| *n)
}

/// The ticket id a PR title refers to, e.g. `COX-B015`.
#[must_use]
pub fn ticket_id_in(title: &str) -> Option<String> {
    let bytes = title.as_bytes();
    let start = title.find(|c: char| c.is_ascii_uppercase())?;
    for i in start..bytes.len() {
        if !bytes[i].is_ascii_uppercase() {
            continue;
        }
        let rest = &title[i..];
        let mut chars = rest.chars();
        let letters: String = chars
            .by_ref()
            .take_while(char::is_ascii_uppercase)
            .collect::<String>();
        if letters.len() < 2 {
            continue;
        }
        let after = &rest[letters.len()..];
        let Some(tail) = after.strip_prefix('-') else {
            continue;
        };
        let digits: String = tail
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        if digits.chars().any(|c| c.is_ascii_digit()) {
            return Some(format!("{letters}-{digits}"));
        }
    }
    None
}

/// Whether a diff is too big, or too load-bearing, for a machine to land on
/// its own. Both bounds are about blast radius rather than correctness: a
/// green suite says the code works, not that a 2,000-line change or a rewrite
/// of the release pipeline should go in unwatched.
#[must_use]
pub fn needs_human_eyes(diff: &str, max_changed_lines: usize) -> Option<String> {
    const SENSITIVE: &[&str] = &[
        ".github/workflows",
        "Dockerfile",
        "docker-compose",
        "scripts/",
        "Cargo.toml",
        "coxagent.json",
        // Governance: the rules the agents themselves run under. An agent once
        // deleted the root-cause contract from CLAUDE.md inside an unrelated
        // bug-fix PR (90eaf25); no machine may land edits to its own leash.
        "CLAUDE.md",
        "AGENTS.md",
        ".claude/",
    ];
    let changed = diff
        .lines()
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .count();
    if max_changed_lines > 0 && changed > max_changed_lines {
        return Some(format!(
            "{changed} changed lines is past what lands unreviewed"
        ));
    }
    let hit = SENSITIVE.iter().find(|p| {
        diff.lines().any(|l| {
            l.contains(*p)
                && (l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff "))
        })
    })?;
    Some(format!(
        "it changes {hit}, which decides how everything else ships"
    ))
}

/// The substantive ADDED lines of a diff, grouped by target file — the
/// evidence set for "did this change actually land on main?". Trivial lines
/// (blank, braces, markers) prove nothing and are skipped.
#[must_use]
pub fn added_lines_by_file(diff: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<usize> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let file = rest
                .split_whitespace()
                .last()
                .and_then(|n| n.strip_prefix("b/"))
                .unwrap_or_default()
                .to_owned();
            out.push((file, Vec::new()));
            current = Some(out.len() - 1);
            continue;
        }
        if let (Some(i), Some(added)) = (current, line.strip_prefix('+')) {
            if line.starts_with("+++") {
                continue;
            }
            let t = added.trim();
            // Only lines distinctive enough to be evidence.
            if t.len() >= 12 && !t.starts_with("//") && !t.starts_with('*') {
                out[i].1.push(t.to_owned());
            }
        }
    }
    out.retain(|(f, lines)| !f.is_empty() && !lines.is_empty());
    out
}

/// Whether a PR's substance is PRESENT on main, judged from its added lines
/// vs the current file contents (`read` returns a file's text on main, `None`
/// when it does not exist). Deleted-only diffs and unreadable diffs return
/// `false` — no evidence means NOT landed; closing a PR is the irreversible
/// side, so it carries the burden of proof.
pub fn diff_landed_on_main(diff: &str, mut read: impl FnMut(&str) -> Option<String>) -> bool {
    let files = added_lines_by_file(diff);
    if files.is_empty() {
        return false;
    }
    let (mut total, mut found) = (0usize, 0usize);
    for (file, lines) in files {
        let content = read(&file).unwrap_or_default();
        // Sample up to 20 lines per file — enough signal, bounded work.
        for l in lines.iter().take(20) {
            total += 1;
            if content.contains(l.as_str()) {
                found += 1;
            }
        }
    }
    total > 0 && found * 10 >= total * 8 // ≥80% of the evidence is on main
}

/// The files a PR diff touches, taken from its `diff --git` headers. A best
/// effort: a diff whose headers we cannot name is conservatively treated as
/// unparseable (empty list), which the resolver turns into a human hold rather
/// than an unsafe auto-close.
#[must_use]
pub fn changed_files(diff: &str) -> Vec<String> {
    diff.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("diff --git ")?;
            // `a/a.rs b/a.rs` — the new path is the last token; `b/` strips the
            // conventional `a/` prefix pair (`a/` is the old path).
            let new = rest.split_whitespace().last()?;
            new.strip_prefix("b/").map(str::to_owned)
        })
        .collect()
}

/// One PR's signals the competing-PR resolver needs — forged from the diff and
/// forge metadata, kept pure so the decision is a function of data, not IO.
#[derive(Debug, Clone)]
pub struct CompeteCandidate {
    pub number: u64,
    /// Files the diff touches. `None` when the diff could not be read.
    pub files: Option<Vec<String>>,
    /// True when the diff is oversized or touches build/ship-sensitive paths
    /// (`needs_human_eyes`) or commits agent scratch — never auto-merged.
    pub unsafe_change: bool,
}

/// The lossless decision for two competing PRs on the same ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompeteOutcome {
    /// Merge `winner`, close `loser`. Only chosen when provably lossless: the
    /// winner's diff covers every file the loser touches, neither is unsafe,
    /// and the loser has nothing the winner lacks.
    MergeClose { winner: u64, loser: u64 },
    /// `winner` is a SAFE, small subset of `unsafe_other` — a load-bearing /
    /// oversized same-ticket change. The safe PR is NOT blocked by the risky
    /// competitor (land it through normal review); the unsafe one stays for a
    /// person. Neither is auto-closed: closing the unsafe one would drop the
    /// extra work it carries, and closing the safe one would discard a real fix.
    Proceed { winner: u64, unsafe_other: u64 },
    /// Cannot resolve without a human — closing would drop real work, a diff
    /// could not be read, or a change is too load-bearing to auto-land.
    Hold(&'static str),
}

/// Two open PRs for the SAME ticket race each other (see [`competing_pr`]);
/// the older policy parked BOTH forever waiting on a human to choose. This
/// self-resolves *only* when it costs nothing: if one PR's diff already covers
/// every file the other touches, and the winner is safe to auto-merge, then
/// closing the duplicate loses no work and the ticket can actually ship
/// (`feature_done`/`bug_fixed`, which the scorecard needs). When either diff
/// carries work the other lacks, or either is unsafe, it holds for a human
/// rather than silently drop a change.
///
/// One further case keeps a clean fix from being held hostage by a risky twin:
/// when the SAFE PR is a strict subset of the unsafe one, the safe PR is not
/// blocked by it (it lands through normal review, where its own size/impact
/// gates still apply) while the unsafe sibling stays for a person.
#[must_use]
#[allow(clippy::needless_pass_by_value)] // call sites hand over ownership; refs would just ripple clones
pub fn resolve_competing(a: CompeteCandidate, b: CompeteCandidate) -> CompeteOutcome {
    // Can't prove anything about a diff we couldn't read.
    let (Some(a_files), Some(b_files)) = (a.files.as_deref(), b.files.as_deref()) else {
        return CompeteOutcome::Hold("could not read a competing diff — keeping both");
    };
    // A winner must actually claim at least one file; an empty diff proves
    // nothing and closing its twin would be guesswork.
    if a_files.is_empty() || b_files.is_empty() {
        return CompeteOutcome::Hold("diff has no parseable file headers — keeping both");
    }
    let a_covers_b = b_files.iter().all(|f| a_files.contains(f));
    let b_covers_a = a_files.iter().all(|f| b_files.contains(f));

    // The current PR (`a`) is itself load-bearing or oversized — never
    // auto-land or auto-close it.
    if a.unsafe_change {
        return CompeteOutcome::Hold(
            "touches a load-bearing or oversized change — a human should rule on it",
        );
    }
    // `a` is safe, but its same-ticket competitor `b` is unsafe. `a` is only
    // unblocked when it is a strict subset of `b`'s sprawl — then landing `a`
    // loses nothing (its files are already inside `b`) and `b` still waits for
    // a person. If they diverge, racing `a` past a risky sibling is unsafe.
    if b.unsafe_change {
        if b_covers_a && !a_covers_b {
            return CompeteOutcome::Proceed {
                winner: a.number,
                unsafe_other: b.number,
            };
        }
        return CompeteOutcome::Hold(
            "the competing change is load-bearing or oversized — a human should rule on it",
        );
    }
    // Both safe: a strict subset decides it — the wider diff is the more
    // complete fix. Exact overlap (both cover each other) means they are the
    // same change; keep the newer one arbitrarily deterministic by picking
    // `a`'s twin.
    match (a_covers_b, b_covers_a) {
        // Exact overlap means the same change; `a` wins deterministically.
        (true, _) => CompeteOutcome::MergeClose {
            winner: a.number,
            loser: b.number,
        },
        (false, true) => CompeteOutcome::MergeClose {
            winner: b.number,
            loser: a.number,
        },
        (false, false) => CompeteOutcome::Hold(
            "each PR touches files the other does not — closing either would drop work",
        ),
    }
}

/// Paths that are the agents' own workings, never the product: worktrees the
/// engine creates, state backups, build output, engine config the runner
/// writes for itself.
const SCRATCH: &[&str] = &[
    ".claude/worktrees/",
    ".claude/settings.local.json",
    ".gitnexus/",
    "backups/",
    "target/",
    "node_modules/",
    "cox-opencode.json",
    "cox-config.json",
];

/// Whether a diff commits the agents' own scratch, and which path proves it.
///
/// Such a PR is wrong by construction, not wrong on judgement: nobody wants a
/// worktree or a state backup in the product's history, and no review round can
/// turn it into something they do. It matters because the pollution is what
/// pushes the diff past the size bound in [`needs_human_eyes`] — so the PR
/// stops being auto-landable AND stops being auto-closable, and parks forever
/// waiting for a person to decide something that was never a decision. Closing
/// it loses nothing: the ticket goes back to the queue and the work is redone
/// from a clean base.
#[must_use]
pub fn commits_scratch(diff: &str) -> Option<String> {
    diff.lines()
        .filter(|l| l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff "))
        .find_map(|l| {
            SCRATCH
                .iter()
                .find(|p| l.contains(*p))
                .map(|p| (*p).to_owned())
        })
}

/// How many senior rescues one ticket may consume before the decision is a
/// human's. Two, because the first rescue can misread the failure — a spec
/// rewrite that turns out to hide a design dead end deserves the second look
/// a person would give it — while an unbounded ladder just burns budget.
pub const MAX_TICKET_RESCUES: u32 = 2;

/// Which senior picks up a ticket the developers could not land, mirroring who
/// you would actually walk over to: the BA when there was never a spec worth
/// building against, the SA when the design is a dead end — or, when every
/// attempt died on a mechanical gate, the SA with a repair brief rather than a
/// redesign of something that was never wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationRoute {
    /// The requirement itself is unbuildable — hand it to the BA.
    Spec,
    /// A quality gate blocked it (lints, missing regression test, red suite).
    Mechanical,
    /// A genuine technical dead end — the SA revises the approach.
    Design,
}

/// Route from the structured failure log — the gates' own verdicts, so no
/// wording of an error message can change who gets called in.
#[must_use]
pub fn route_from_failures(
    failures: &[crate::state::AttemptFailure],
    spec_gap: bool,
) -> EscalationRoute {
    use crate::state::FailureLayer;
    // Infra faults are nobody's failure and never counted against a ticket.
    let real: Vec<&crate::state::AttemptFailure> = failures
        .iter()
        .filter(|f| f.layer != FailureLayer::Infra)
        .collect();
    if real.is_empty() {
        return EscalationRoute::Design;
    }
    if real.iter().any(|f| f.layer == FailureLayer::Spec) || spec_gap {
        return EscalationRoute::Spec;
    }
    if real.iter().all(|f| f.layer == FailureLayer::Gate) {
        return EscalationRoute::Mechanical;
    }
    EscalationRoute::Design
}

/// Classify three failed attempts from what the gates recorded. `spec_gap` is
/// the deterministic signal that the ticket never carried a usable requirement.
#[must_use]
pub fn escalation_route(history: &str, spec_gap: bool) -> EscalationRoute {
    let low = history.to_lowercase();
    let unclear = [
        "unclear",
        "ambiguous",
        "no acceptance",
        "missing acceptance",
        "cannot reproduce",
        "could not reproduce",
        "no repro",
        "not enough context",
    ]
    .iter()
    .any(|k| low.contains(k));
    if unclear || (spec_gap && !low.is_empty()) {
        return EscalationRoute::Spec;
    }
    let gates = [
        "clippy",
        "regression test",
        "suite is red",
        "left tests red",
        "rustfmt",
        "cargo fmt",
        "lint",
    ];
    let lines: Vec<&str> = low.lines().filter(|l| !l.trim().is_empty()).collect();
    // Only when EVERY recorded failure is a gate — one design failure mixed in
    // means the approach is still suspect.
    if !lines.is_empty() && lines.iter().all(|l| gates.iter().any(|g| l.contains(g))) {
        return EscalationRoute::Mechanical;
    }
    EscalationRoute::Design
}

#[cfg(test)]
mod merge_guard_tests {
    use super::{commits_scratch, competing_pr, diff_landed_on_main, needs_human_eyes};

    #[test]
    fn a_branch_that_committed_agent_scratch_is_named_for_it() {
        // The live PR this comes from: agent worktrees and state backups
        // committed, which blew the diff past the size bound — so it could
        // neither land nor be closed, and sat open for days.
        let diff = "diff --git a/.claude/worktrees/agent-a26d/x b/.claude/worktrees/agent-a26d/x\n                    +++ b/backups/2026-07-31/spaces.json\n+{}\n";
        assert_eq!(commits_scratch(diff).as_deref(), Some(".claude/worktrees/"));

        let backups_only = "+++ b/backups/2026-07-31/workspace.json\n+{}\n";
        assert_eq!(commits_scratch(backups_only).as_deref(), Some("backups/"));
    }

    #[test]
    fn ordinary_source_changes_are_not_scratch() {
        let diff = "diff --git a/crates/app/src/lib.rs b/crates/app/src/lib.rs\n                    +++ b/crates/app/src/lib.rs\n+fn main() {}\n";
        assert_eq!(commits_scratch(diff), None);
        // A path merely MENTIONED in an added line is not a committed path.
        let mention = "+++ b/docs/setup.md\n+ignore backups/ in your clone\n";
        assert_eq!(commits_scratch(mention), None);
    }

    #[test]
    fn two_prs_for_one_ticket_are_a_race_not_two_fixes() {
        // The real case: #17 and #18 both fixed COX-B015, and whichever landed
        // first left the other conflicting.
        let open = vec![
            (
                17,
                "fix(COX-B015): git content subcommands bypass the shim".to_owned(),
            ),
            (
                18,
                "fix(COX-B015): git content keeps its own stdout".to_owned(),
            ),
            (
                16,
                "fix(COX-B011): README points at the wrong port".to_owned(),
            ),
        ];
        assert_eq!(competing_pr(18, &open[1].1, &open), Some(17));
        assert_eq!(
            competing_pr(16, &open[2].1, &open),
            None,
            "its own ticket only"
        );
        // A title with no ticket id cannot compete with anything.
        assert_eq!(competing_pr(20, "chore: tidy imports", &open), None);
    }

    #[test]
    fn a_huge_change_or_one_that_moves_the_pipeline_waits_for_a_person() {
        let small = "diff --git a/src/a.rs b/src/a.rs\n+++ b/src/a.rs\n+let x = 1;\n-let x = 0;\n";
        // 3000 is the configurable default; a 900-line change is under it, so it
        // is NOT held for size alone (but is still held for a sensitive path).
        assert!(needs_human_eyes(small, 3000).is_none());
        assert!(needs_human_eyes(small, 0).is_none());

        let huge = format!(
            "diff --git a/src/a.rs b/src/a.rs\n+++ b/src/a.rs\n{}",
            "+line\n".repeat(900)
        );
        // Under the default 3000 limit a 900-line diff is eligible to land.
        assert!(needs_human_eyes(&huge, 3000).is_none());
        // A lower configured bound holds it for size.
        assert!(needs_human_eyes(&huge, 800)
            .expect("held")
            .contains("changed lines"));

        // Green tests say the code works, not that the release pipeline should
        // change itself unwatched.
        let ci = "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\n\
                  +++ b/.github/workflows/ci.yml\n+  run: cargo test\n";
        assert!(needs_human_eyes(ci, 3000)
            .expect("held")
            .contains(".github/workflows"));
    }

    #[test]
    fn closing_a_pr_requires_proof_its_diff_landed_on_main() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n+++ b/src/a.rs\n\
                    +fn very_distinctive_function_name() {\n\
                    +    let answer = compute_the_thing(42);\n";
        // Substance present on main → superseded, closable.
        let main_has_it =
            "fn very_distinctive_function_name() {\n    let answer = compute_the_thing(42);\n}";
        assert!(diff_landed_on_main(diff, |_| Some(main_has_it.to_owned())));
        // Substance absent → NOT landed; closing would throw away real work
        // (the #185–#196 mass-close of 2026-08-16).
        assert!(!diff_landed_on_main(diff, |_| Some(
            "fn unrelated() {}".to_owned()
        )));
        // File missing on main entirely → not landed.
        assert!(!diff_landed_on_main(diff, |_| None));
        // No evidence lines at all (empty/deletion-only diff) → not landed:
        // the irreversible side carries the burden of proof.
        assert!(!diff_landed_on_main("diff --git a/x b/x\n-gone\n", |_| {
            Some(String::new())
        }));
    }

    #[test]
    fn an_agent_never_lands_edits_to_its_own_governance_rules() {
        // Regression: 90eaf25 deleted the root-cause contract from CLAUDE.md
        // inside an unrelated bug-fix PR and auto-merged. Any diff touching the
        // rules the agents run under now waits for a person.
        for path in ["CLAUDE.md", "AGENTS.md", ".claude/skills/x/SKILL.md"] {
            let diff = format!("diff --git a/{path} b/{path}\n+++ b/{path}\n-a rule\n");
            assert!(needs_human_eyes(&diff, 3000).is_some(), "{path} must hold");
        }
        // A mention of the file INSIDE a hunk body is not a file change.
        let body_only =
            "diff --git a/src/a.rs b/src/a.rs\n+++ b/src/a.rs\n+// see CLAUDE.md for rules\n";
        assert!(needs_human_eyes(body_only, 3000).is_none());
    }

    #[test]
    fn consuming_pr_is_closed_when_the_winner_covers_it() {
        // The live deadlock: #89 and #98 both fix the same ticket. #98's diff
        // covers everything #89 touches, so keeping #98 and closing #89 loses
        // no work — the ticket can finally ship.
        let a = super::CompeteCandidate {
            number: 89,
            files: Some(vec!["crates/app/src/lib.rs".to_owned()]),
            unsafe_change: false,
        };
        let b = super::CompeteCandidate {
            number: 98,
            files: Some(vec![
                "crates/app/src/lib.rs".to_owned(),
                "crates/app/src/main.rs".to_owned(),
            ]),
            unsafe_change: false,
        };
        assert_eq!(
            super::resolve_competing(b.clone(), a.clone()),
            super::CompeteOutcome::MergeClose {
                winner: 98,
                loser: 89
            },
            "the wider diff wins, the subset is the duplicate"
        );
        // Order-invariant: swap and the same winner is chosen.
        assert_eq!(
            super::resolve_competing(a, b),
            super::CompeteOutcome::MergeClose {
                winner: 98,
                loser: 89
            }
        );
    }

    #[test]
    fn competing_prs_with_disjoint_work_are_held_not_closed() {
        // Each PR touches a file the other does not: closing either drops real
        // work, so the machine must hold and let a person reconcile.
        let a = super::CompeteCandidate {
            number: 89,
            files: Some(vec!["crates/app/src/lib.rs".to_owned()]),
            unsafe_change: false,
        };
        let b = super::CompeteCandidate {
            number: 98,
            files: Some(vec!["crates/domain/src/models.rs".to_owned()]),
            unsafe_change: false,
        };
        assert!(matches!(
            super::resolve_competing(a, b),
            super::CompeteOutcome::Hold(_)
        ));
    }

    #[test]
    fn a_load_bearing_or_unreadable_competing_diff_never_auto_closes() {
        let safe = super::CompeteCandidate {
            number: 1,
            files: Some(vec!["crates/app/src/lib.rs".to_owned()]),
            unsafe_change: false,
        };
        // Load-bearing change (pipeline) → the SAFE subset PR is unblocked
        // (Proceed: it lands through normal review, its own gates apply) and
        // the unsafe one is never auto-closed. When the UNSAFE PR is the one
        // being considered it is always held for a person.
        let pipeline = super::CompeteCandidate {
            number: 2,
            files: Some(vec![
                ".github/workflows/ci.yml".to_owned(),
                "crates/app/src/lib.rs".to_owned(),
            ]),
            unsafe_change: true,
        };
        assert!(matches!(
            super::resolve_competing(safe.clone(), pipeline.clone()),
            super::CompeteOutcome::Proceed {
                winner: 1,
                unsafe_other: 2
            }
        ));
        assert!(matches!(
            super::resolve_competing(pipeline, safe.clone()),
            super::CompeteOutcome::Hold(_)
        ));
        // A diff that could not be read → hold, never a blind close.
        let unreadable = super::CompeteCandidate {
            number: 3,
            files: None,
            unsafe_change: false,
        };
        assert!(matches!(
            super::resolve_competing(safe, unreadable),
            super::CompeteOutcome::Hold(_)
        ));
    }

    #[test]
    fn changed_files_names_simple_and_renamed_paths() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n+let x = 1;\n\
                    diff --git a/src/b.rs b/src/c.rs\n+let y = 2;\n";
        assert_eq!(
            super::changed_files(diff),
            vec!["src/a.rs".to_owned(), "src/c.rs".to_owned()]
        );
        assert!(super::changed_files("no headers here").is_empty());
    }

    #[test]
    fn a_safe_subset_pr_is_unblocked_from_a_load_bearing_competitor() {
        // The live case: #98 is a small, safe 1-file fix; #89 is a sprawling
        // 40-file same-ticket change that is load-bearing/oversized (unsafe).
        // #98 must NOT be held hostage by #89 — it proceeds to normal review
        // (where its own size gates still apply), while #89 stays for a human.
        let safe = super::CompeteCandidate {
            number: 98,
            files: Some(vec![
                "crates/infrastructure/src/deploy/docker_compose.rs".to_owned()
            ]),
            unsafe_change: false,
        };
        let unsafe_sprawl = super::CompeteCandidate {
            number: 89,
            files: Some(vec![
                "crates/infrastructure/src/deploy/docker_compose.rs".to_owned(),
                "crates/app/src/builders.rs".to_owned(),
                "crates/application/src/prompts.rs".to_owned(),
            ]),
            unsafe_change: true,
        };
        // Processing the SAFE pr as the current one: safe is a strict subset of
        // unsafe → Proceed (unblock the safe fix, keep unsafe for a person).
        assert_eq!(
            super::resolve_competing(safe.clone(), unsafe_sprawl.clone()),
            super::CompeteOutcome::Proceed {
                winner: 98,
                unsafe_other: 89
            }
        );
        // Processing the UNSAFE pr as the current one: it stays held — the SA
        // never auto-lands or auto-closes a load-bearing change.
        assert!(matches!(
            super::resolve_competing(unsafe_sprawl, safe),
            super::CompeteOutcome::Hold(_)
        ));
    }

    #[test]
    fn a_safe_pr_is_not_unblocked_when_the_unsafe_competitor_diverges() {
        // Safe PR touches a file the unsafe competitor does not, so it is NOT a
        // strict subset — racing it past the risky sibling would double-fix a
        // file in parallel. It stays held.
        let safe = super::CompeteCandidate {
            number: 7,
            files: Some(vec!["crates/a.rs".to_owned()]),
            unsafe_change: false,
        };
        let unsafe_other = super::CompeteCandidate {
            number: 8,
            files: Some(vec!["crates/b.rs".to_owned()]),
            unsafe_change: true,
        };
        assert!(matches!(
            super::resolve_competing(safe, unsafe_other),
            super::CompeteOutcome::Hold(_)
        ));
    }
}

#[cfg(test)]
mod escalation_route_tests {
    use super::{escalation_route, EscalationRoute, MAX_TICKET_RESCUES};

    #[test]
    fn a_ticket_gets_a_second_rescue_before_it_becomes_a_human_decision() {
        // The first rescue can misread which senior was needed; one more look
        // is what a team would do. Unbounded retries are not.
        assert_eq!(MAX_TICKET_RESCUES, 2);
    }

    #[test]
    fn every_attempt_dying_on_a_gate_is_a_repair_job_not_a_redesign() {
        let h = "attempt 1 failed: added clippy errors (37 -> 38)\n\
                 attempt 2 failed: added clippy errors (37 -> 38)\n\
                 attempt 3 failed: bug fix shipped without a regression test";
        assert_eq!(escalation_route(h, false), EscalationRoute::Mechanical);
    }

    #[test]
    fn a_missing_spec_goes_to_the_ba() {
        assert_eq!(
            escalation_route("attempt 1 failed: requirements unclear", false),
            EscalationRoute::Spec
        );
        // A thin ticket with any failure history is a spec problem too.
        assert_eq!(
            escalation_route("attempt 1 failed: engine failed", true),
            EscalationRoute::Spec
        );
    }

    #[test]
    fn structured_records_route_without_reading_english() {
        use crate::state::{AttemptFailure, FailureLayer};
        let f = |attempt, layer, gate: &str| AttemptFailure {
            attempt,
            layer,
            gate: gate.to_owned(),
            detail: "…".to_owned(),
            files: Vec::new(),
        };
        // Every attempt died on a gate → repair brief, not a redesign.
        assert_eq!(
            super::route_from_failures(
                &[
                    f(1, FailureLayer::Gate, "clippy"),
                    f(2, FailureLayer::Gate, "regression-test")
                ],
                false
            ),
            EscalationRoute::Mechanical
        );
        // An outage in the middle must not turn a design failure into a gate one.
        assert_eq!(
            super::route_from_failures(
                &[
                    f(1, FailureLayer::Gate, "clippy"),
                    f(2, FailureLayer::Infra, "engine"),
                    f(3, FailureLayer::Design, "engine")
                ],
                false
            ),
            EscalationRoute::Design
        );
        // Infra-only history says nothing about the work itself.
        assert_eq!(
            super::route_from_failures(&[f(1, FailureLayer::Infra, "engine")], false),
            EscalationRoute::Design
        );
        assert_eq!(
            super::route_from_failures(&[f(1, FailureLayer::Gate, "clippy")], true),
            EscalationRoute::Spec,
            "a ticket with no usable requirement is the BA's before it is anyone's"
        );
    }

    #[test]
    fn a_real_build_failure_still_gets_the_sas_redesign() {
        let h = "attempt 1 failed: left tests red on COX-B009\n\
                 attempt 2 failed: error[E0308]: mismatched types";
        assert_eq!(escalation_route(h, false), EscalationRoute::Design);
        assert_eq!(escalation_route("", false), EscalationRoute::Design);
    }
}
