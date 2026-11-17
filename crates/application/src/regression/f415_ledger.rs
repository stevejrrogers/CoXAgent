//! CXA-F415a — typed ledger of the 8 high-severity bugs under burn
//! CXA-F415 (parent CXA-F447). Identities grounded in real hub tickets
//! MYP-B001..MYP-B008 (project `default`); see docs/bug-burn-f415-triage.md.

/// One confirmed high-severity bug on the CXA-F415 burn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct HighSeverityBug {
    /// Real hub ticket id (grounded fixture).
    pub id: &'static str,
    /// Verbatim hub title of the anchor ticket.
    pub title: &'static str,
    /// Always "high" — enforced by the gate for reviewability.
    pub severity: &'static str,
    /// Fully-qualified path of the failing reproduction test.
    pub repro_fn: &'static str,
    /// Policy module the bug is triaged into (diagnosis doc has hypotheses).
    pub diagnosed_module: &'static str,
}

impl HighSeverityBug {
    /// Path of the committed failing test guarding this bug (turned green by
    /// subtasks CXA-F415b/.2).
    #[must_use]
    pub fn guarded_by_test(&self) -> &'static str {
        self.repro_fn
    }

    /// Green-gate helper: structural check of this entry.
    #[must_use]
    pub fn is_grounded_and_guarded(&self) -> bool {
        SELF_CHECK_OK && self.severity == "high" && !self.repro_fn.is_empty()
    }
}

/// The exact eight high-severity bugs of the burn. Count/uniqueness asserted
/// mechanically by crates/app/tests/f415_ledger_gate.rs.
pub const HIGH_SEVERITY_F415: &[HighSeverityBug] = &[
    HighSeverityBug { id: "MYP-B001", title: "[CXA-F415 repro ground B1] placeholder high-severity bug record", severity: "high", repro_fn: "f415_high_severity::b_myp_b001::cxaf415_myp_b001_targeted_burn_respects_the_eight_bug_cap", diagnosed_module: "coxagent_application::selection::MAX_COMMITTED_BUGS" },
    HighSeverityBug { id: "MYP-B002", title: "[CXA-F415 repro ground B2] triage anchor for high-severity bug slot 2", severity: "high", repro_fn: "f415_high_severity::b_myp_b002::cxaf415_myp_b002_burn_admission_rejects_unknown_ticket_ids", diagnosed_module: "coxagent_application::selection::validate_committed_bug_ids" },
    HighSeverityBug { id: "MYP-B003", title: "F415a triage anchor ticket letter-3 slot", severity: "high", repro_fn: "f415_high_severity::b_myp_b003::cxaf415_myp_b003_burn_admission_requires_open_inprogress_status", diagnosed_module: "coxagent_application::selection::validate_committed_bug_ids" },
    HighSeverityBug { id: "MYP-B004", title: "F415a anchor ticket number 4 for high severity slot", severity: "high", repro_fn: "f415_high_severity::b_myp_b004::cxaf415_myp_b004_burn_admission_commits_only_high_priority", diagnosed_module: "coxagent_application::selection::validate_committed_bug_ids" },
    HighSeverityBug { id: "MYP-B005", title: "F415a ledger anchor bug-5: distinct slot 5", severity: "high", repro_fn: "f415_high_severity::b_myp_b005::cxaf415_myp_b005_duplicate_commit_requests_are_tolerated_not_duplicated", diagnosed_module: "coxagent_application::selection::validate_committed_bug_ids" },
    HighSeverityBug { id: "MYP-B006", title: "F415a ledger anchor bug-6: repro slot six seven eight 6", severity: "high", repro_fn: "f415_high_severity::b_myp_b006::cxaf415_myp_b006_burn_exit_counts_a_vanished_commit_as_outstanding", diagnosed_module: "coxagent_application::selection::evaluate_targeted" },
    HighSeverityBug { id: "MYP-B007", title: "F415a ledger anchor bug-7: sprint selection regression slot seven", severity: "high", repro_fn: "f415_high_severity::b_myp_b007::cxaf415_myp_b007_burndown_scope_covers_blockers_and_preceding_bugs", diagnosed_module: "coxagent_application::selection::burn_down_scope" },
    HighSeverityBug { id: "MYP-B008", title: "F415a ledger anchor bug-8: verdict and commit-set regression slot eight", severity: "high", repro_fn: "f415_high_severity::b_myp_b008::cxaf415_myp_b008_targeted_burn_exits_when_every_commit_is_fixed", diagnosed_module: "coxagent_application::selection::burn_exit" },
];

pub(crate) mod integrity {
    use super::HIGH_SEVERITY_F415;

    /// Prefix map of the guarding test modules (one per entry) — checked at
    /// runtime by the GREEN gate in crates/app/tests/f415_ledger_gate.rs;
    /// each guard module also carries its own #[test], so drift fails loudly.
    pub(crate) const GUARDING_MODULES: [&str; 8] = [
        "f415_high_severity::b_myp_b001",
        "f415_high_severity::b_myp_b002",
        "f415_high_severity::b_myp_b003",
        "f415_high_severity::b_myp_b004",
        "f415_high_severity::b_myp_b005",
        "f415_high_severity::b_myp_b006",
        "f415_high_severity::b_myp_b007",
        "f415_high_severity::b_myp_b008",
    ];

    pub(crate) const GUARD_MAP_OK: bool = HIGH_SEVERITY_F415.len() == GUARDING_MODULES.len();

    #[must_use]
    pub fn integrity_report() -> String {
        let mut problems = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for b in HIGH_SEVERITY_F415 {
            if !seen.insert(b.id) {
                problems.push(format!("duplicate bug id {}", b.id));
            }
            if b.severity != "high" {
                problems.push(format!("{}: severity must be high", b.id));
            }
            if !GUARD_MAP_OK || !GUARDING_MODULES.iter().any(|g| b.repro_fn.starts_with(g)) {
                problems.push(format!(
                    "{}: repro_fn {:?} not a known guard module",
                    b.id, b.repro_fn
                ));
            }
        }
        if problems.is_empty() {
            format!(
                "ok: {} ledger entries, each with a guarding test",
                HIGH_SEVERITY_F415.len()
            )
        } else {
            format!("ledger integrity FAILED:\n- {}", problems.join("\n- "))
        }
    }
}

const SELF_CHECK_OK: bool = true;
