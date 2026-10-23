//! The aggregate view over a suite run: passed/failed/total counts plus the
//! printable rendering a test binary or CI entry point emits.
//!
//! A failing line always reads `<name> — <why>`, where `<why>` is the case's
//! expected-vs-actual sentence from [`super::runner::run_case`]. The harness
//! never prints a bare `false`: that verdict shape is pinned by unit tests, so
//! a future edit cannot quietly regress failure readability.

use super::runner::CaseResult;

/// Counts over one suite run, in the `{passed, failed, total}` shape the
/// acceptance criteria pin. Invariant (test-enforced): `total == passed + failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardrailReport {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
}

impl GuardrailReport {
    #[must_use]
    pub fn of(results: &[CaseResult]) -> Self {
        let passed = results.iter().filter(|r| r.passed).count();
        let failed = results.len() - passed;
        Self {
            passed,
            failed,
            total: results.len(),
        }
    }

    /// Whether every registered case held.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.failed == 0
    }

    /// Multi-line text for logs / CI. First line is the counts; each failing
    /// case gets its own indented detail line naming it with expected vs
    /// actual, so one glance answers "what broke and why".
    #[must_use]
    pub fn render(&self, results: &[CaseResult]) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "guardrails: {} passed / {} failed / {} total",
            self.passed, self.failed, self.total
        );
        for r in results.iter().filter(|r| !r.passed) {
            let _ = writeln!(out, "FAIL {} — {}", r.name, r.reason);
        }
        if self.all_passed() {
            out.push_str("guardrails: OK\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(name: &'static str, passed: bool, reason: &str) -> CaseResult {
        CaseResult {
            name,
            passed,
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn counts_add_up_and_all_passed_tracks_failures() {
        let results = [
            result("a", true, "ok"),
            result("b", false, "expected 2 panels, but got 3"),
            result("c", true, "ok"),
        ];
        let report = GuardrailReport::of(&results);
        assert_eq!(
            report,
            GuardrailReport {
                passed: 2,
                failed: 1,
                total: 3
            }
        );
        assert!(!report.all_passed());
        assert!(GuardrailReport::of(&[]).all_passed());
    }

    #[test]
    fn a_failure_renders_name_plus_expected_vs_actual_never_a_bare_boolean() {
        let results = [result(
            "panel_inventory_complete",
            false,
            "every registered panel renders: expected 6 above-the-fold panels, but got 5",
        )];
        let rendered = GuardrailReport::of(&results).render(&results);
        assert!(
            rendered.contains(
                "FAIL panel_inventory_complete — every registered panel renders: \
                               expected 6 above-the-fold panels, but got 5"
            ),
            "failure line must carry name and expected-vs-actual: {rendered}"
        );
        assert!(!rendered.contains("\nfalse"), "no bare boolean: {rendered}");
    }

    #[test]
    fn an_all_green_run_renders_counts_and_an_ok_line() {
        let results = [result("harness_smoke_loop_closes", true, "holds")];
        let rendered = GuardrailReport::of(&results).render(&results);
        assert!(rendered.contains("guardrails: 1 passed / 0 failed / 1 total"));
        assert!(rendered.ends_with("guardrails: OK\n"));
    }
}
