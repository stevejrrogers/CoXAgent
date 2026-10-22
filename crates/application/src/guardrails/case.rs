//! The guardrail case unit: a named, described check that is a PURE function
//! over the input snapshot the caller passes in. No IO imports here — ever.
//!
//! A case never decides what data to look at (that is the caller's snapshot)
//! and never effects anything (that would be an adapter); it only answers one
//! question about the data it receives, as [`GuardrailVerdict`] — which must
//! say expected-vs-actual on breach, because "a bare false" is exactly the
//! unreadable failure this harness exists to kill.

/// The outcome of one pure check. `Hold` when the invariant stands; `Breach`
/// always carries both sides of the comparison, so any rendered failure reads
/// like a sentence instead of a boolean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailVerdict {
    /// The invariant holds for this input.
    Hold,
    /// The invariant broke — with enough evidence to act without re-running.
    Breach { expected: String, actual: String },
}

impl GuardrailVerdict {
    #[must_use]
    pub fn is_hold(&self) -> bool {
        matches!(self, GuardrailVerdict::Hold)
    }
}

/// One registered guardrail: identity + intent + a pure check over `T`.
///
/// `T` is generic because real CXA-B240 cases check domain shapes (panel
/// inventories, snapshot fields), while the scaffold smoke case proves the
/// loop over a plain integer. Nothing here touches a port; tests construct
/// cases inline like any other struct literal.
pub struct GuardrailCase<T> {
    /// Stable identifier used in reports and CI logs (`harness_smoke_loop_closes`).
    pub name: &'static str,
    /// What the case defends, printed alongside the name on failure.
    pub description: &'static str,
    /// The check itself: same `T` in, same verdict out, zero side effects.
    pub check: Box<dyn Fn(&T) -> GuardrailVerdict>,
}

impl<T> GuardrailCase<T> {
    /// Bundle a named, described closure into a runnable case.
    pub fn new(
        name: &'static str,
        description: &'static str,
        check: impl Fn(&T) -> GuardrailVerdict + 'static,
    ) -> Self {
        Self {
            name,
            description,
            check: Box::new(check),
        }
    }
}
