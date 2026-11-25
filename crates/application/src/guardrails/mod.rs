//! The guardrail harness (CXA-B280) — registered, pure invariant checks run
//! against ONE in-memory snapshot, reporting per-case outcomes and aggregate
//! counts.
//!
//! Same discipline as [`crate::use_cases::run_dev::gates`]: every case is a
//! plain function over data the caller hands in, so a suite runs identically
//! under `cargo test`, a CI job, or any future surface — with no server, no
//! filesystem and no network anywhere in the loop. Any future IO (e.g. a
//! snapshot fetched from `/api/state`) enters ONLY behind a port in
//! `ports/outbound/` with an infrastructure adapter; this module stays pure
//! forever, exactly like `gates.rs` after the GitPort split.
//!
//! Layout — one file per cohesive unit:
//! - [`case`]   — the [`GuardrailCase`](case::GuardrailCase) unit: name +
//!   description + pure check producing an expected-vs-actual verdict.
//! - [`runner`] — executes all registered cases over one input.
//! - [`report`] — aggregate counts and printable text for logs / CI.

pub mod case;
pub mod filter_sort_cases;
pub mod report;
pub mod runner;

pub use case::{GuardrailCase, GuardrailVerdict};
pub use report::GuardrailReport;
pub use runner::{run_suite, CaseResult};

/// The sentinel the built-in smoke case expects. Arbitrary on purpose: it only
/// proves the case → runner → report loop closes; real guardrail cases
/// (follow-up tickets in CXA-B240) replace it with domain checks over the
/// hub-state snapshot types.
pub const SMOKE_SENTINEL: u32 = 42;

#[must_use]
/// The trivial in-memory smoke case wired end-to-end through the runner: the
/// only check installed at scaffold time, so `cargo test guardrail` exercises
/// the whole loop before a single real invariant exists.
pub fn smoke_case() -> GuardrailCase<u32> {
    GuardrailCase::new(
        "harness_smoke_loop_closes",
        "Harness self-check: one trivial in-memory invariant flows through \
         case → runner → report unchanged.",
        |input| {
            if *input == SMOKE_SENTINEL {
                GuardrailVerdict::Hold
            } else {
                GuardrailVerdict::Breach {
                    expected: format!("input == SMOKE_SENTINEL ({SMOKE_SENTINEL})"),
                    actual: format!("{input}"),
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smoke_case_passes_on_the_sentinel_and_names_a_mismatch_otherwise() {
        let verdict = (smoke_case().check)(&SMOKE_SENTINEL);
        assert!(matches!(verdict, GuardrailVerdict::Hold));

        let broken = SMOKE_SENTINEL + 1;
        let GuardrailVerdict::Breach { expected, actual } = (smoke_case().check)(&broken) else {
            panic!("43 must read as a breach");
        };
        assert_eq!(expected, "input == SMOKE_SENTINEL (42)");
        assert_eq!(actual, "43");
    }
}
