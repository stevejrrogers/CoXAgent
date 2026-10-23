//! Executes every registered guardrail case against one shared input snapshot
//! and collects per-case outcomes.
//!
//! Pure by construction: it calls each case's check with the value the caller
//! supplied and records the verdict. The runner does not fetch snapshots or
//! open files — when CXA-B240 grows a live source for hub state, the fetch
//! lives behind a `ports/outbound` trait whose output type becomes the `T`
//! these cases already take.

use super::case::{GuardrailCase, GuardrailVerdict};

/// How one registered case fared. A held case keeps its reason too ("smoke:
/// input matched"), so a report line is never just a boolean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    pub name: &'static str,
    pub passed: bool,
    pub reason: String,
}

/// Run all registered cases against one input snapshot. Order follows the
/// registration list, so reports stay stable run-to-run (diff-friendly in CI).
pub fn run_suite<T>(cases: &[GuardrailCase<T>], input: &T) -> Vec<CaseResult> {
    cases.iter().map(|c| run_case(c, input)).collect()
}

/// One case against the snapshot, folded into the flat result record the
/// aggregate reporter consumes.
pub fn run_case<T>(guardrail: &GuardrailCase<T>, input: &T) -> CaseResult {
    match (guardrail.check)(input) {
        GuardrailVerdict::Hold => CaseResult {
            name: guardrail.name,
            passed: true,
            reason: format!("{}: holds", guardrail.description),
        },
        GuardrailVerdict::Breach { expected, actual } => CaseResult {
            name: guardrail.name,
            passed: false,
            reason: format!(
                "{}: expected {expected}, but got {actual}",
                guardrail.description
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(name: &'static str, threshold: i32) -> GuardrailCase<i32> {
        GuardrailCase::new(name, "count stays under budget", move |v: &i32| {
            if *v < threshold {
                GuardrailVerdict::Hold
            } else {
                GuardrailVerdict::Breach {
                    expected: format!("count < {threshold}"),
                    actual: v.to_string(),
                }
            }
        })
    }

    #[test]
    fn results_follow_registration_order_and_carry_reasons() {
        let cases = vec![case("budget_low", 10), case("budget_high", 20)];
        let results = run_suite(&cases, &11);
        assert_eq!(
            results
                .iter()
                .map(|r| (r.name, r.passed))
                .collect::<Vec<_>>(),
            [("budget_low", false), ("budget_high", true)],
        );
        assert!(results[0].reason.contains("expected count < 10"));
        assert!(results[0].reason.contains("but got 11"));
    }

    #[test]
    fn an_empty_registry_runs_to_an_empty_report_without_error() {
        let empty: Vec<GuardrailCase<u8>> = Vec::new();
        assert!(run_suite(&empty, &0).is_empty());
    }
}
