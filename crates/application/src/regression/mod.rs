//! Typed registries of named regressions (bug ledgers as code).

mod f415_ledger;

pub use f415_ledger::{HighSeverityBug, HIGH_SEVERITY_F415};

/// Pure integrity harness consumed by crates/app/tests/f415_ledger_gate.rs
/// (GREEN gate): ledger count, uniqueness, grounding and guard coverage.
#[must_use]
pub fn tests_f415_high_severity() -> String {
    f415_ledger::integrity::integrity_report()
}
