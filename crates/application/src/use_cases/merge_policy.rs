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
pub fn needs_human_eyes(diff: &str) -> Option<String> {
    const MAX_CHANGED_LINES: usize = 800;
    const SENSITIVE: &[&str] = &[
        ".github/workflows",
        "Dockerfile",
        "docker-compose",
        "scripts/",
        "Cargo.toml",
        "coxagent.json",
    ];
    let changed = diff
        .lines()
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .count();
    if changed > MAX_CHANGED_LINES {
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
    use super::{commits_scratch, competing_pr, needs_human_eyes};

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
        assert!(needs_human_eyes(small).is_none());

        let huge = format!(
            "diff --git a/src/a.rs b/src/a.rs\n+++ b/src/a.rs\n{}",
            "+line\n".repeat(900)
        );
        assert!(needs_human_eyes(&huge)
            .expect("held")
            .contains("changed lines"));

        // Green tests say the code works, not that the release pipeline should
        // change itself unwatched.
        let ci = "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml\n\
                  +++ b/.github/workflows/ci.yml\n+  run: cargo test\n";
        assert!(needs_human_eyes(ci)
            .expect("held")
            .contains(".github/workflows"));
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
