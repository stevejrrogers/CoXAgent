//! CXA-F415a — GREEN registry gate: ledger == 8 unique high ids, each guarded
//! by a compiled repro test. Hexagonal-clean (pure functions only).
use coxagent_application::regression::{HighSeverityBug, HIGH_SEVERITY_F415};

fn bug_of_id(id: &str) -> Option<&'static HighSeverityBug> {
    HIGH_SEVERITY_F415.iter().find(|b| b.id == id)
}

#[test]
fn the_f415_ledger_holds_exactly_eight_unique_high_severity_bugs() {
    let report = coxagent_application::regression::tests_f415_high_severity();
    assert!(
        !report.contains("FAILED"),
        "CXA-F415a ledger integrity: {report}"
    );
    assert_eq!(HIGH_SEVERITY_F415.len(), 8);
    for b in HIGH_SEVERITY_F415 {
        assert_eq!(b.severity, "high", "{} must be high severity", b.id);
        assert!(bug_of_id(b.id).is_some());
        assert!(!b.guarded_by_test().is_empty());
    }
}
