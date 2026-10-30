//! Reusable serde round-trip harness (CXA-B326, fulfilling CXA-B317 AC2).
//!
//! Before this module every boundary-crossing type was proven stable by a
//! hand-rolled five-line block copy-pasted per test (`dto_serde_round_trip_*`),
//! so a suite could only ever fail on its FIRST broken type. This harness is
//! the one shared implementation, used by every contract/consumer suite:
//!
//! * [`assert_symmetric_round_trip`] — encode→decode→equal, plus byte-stable
//!   re-encode of the decoded tree.
//! * [`assert_wire_shape`] — additionally pins the exact JSON a type emits
//!   (and accepts) against a checked-in golden, so renames/dropped fields fail
//!   loudly.
//! * [`SymmetrySuite`] — collects N fixtures of one type and verifies them ALL,
//!   reporting every failure in one run instead of dying on the first.
//!
//! Deliberately dependency-light (`serde`/`serde_json` only) so it can sit next
//! to the wire types themselves and be consumed by any downstream crate's test
//! suite. IO-free by construction; nothing here touches process/fs/network.
//!
//! Fixture-construction `unwrap`/`expect` inside `#[cfg(test)]` follows the
//! allow-scope convention of the B254/B252 suites.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Debug;

/// Prove `value` survives a full serde_json round trip unchanged AND that the
/// encoded form is byte-stable under re-encoding.
///
/// Two assertions, both load-bearing for wire compatibility:
///
/// 1. `deserialize(serialize(value)) == value` — no field lost or mangled.
/// 2. `serialize(deserialize(text)) == text` — a peer that decodes and
///    re-encodes produces the same bytes, so proxies/relays cannot corrupt
///    frames they forward.
///
/// # Panics
/// With a message naming the failing direction whenever either assertion (or
/// serde itself) fails.
pub fn assert_symmetric_round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let outbound = match serde_json::to_vec(value) {
        Ok(bytes) => bytes,
        Err(e) => panic!("encode failed: {e}"),
    };
    let decoded: T = match serde_json::from_slice(&outbound) {
        Ok(back) => back,
        Err(e) => panic!("decode failed — wire shape is unreadable by its own type: {e}"),
    };
    assert_eq!(
        &decoded,
        value,
        "asymmetric encode→decode: a field was lost or altered in transit\n\
         wire bytes: {}",
        String::from_utf8_lossy(&outbound),
    );

    let reencoded = serde_json::to_vec(&decoded)
        .unwrap_or_else(|e| panic!("re-encode failed after successful decode: {e}"));
    assert_eq!(
        reencoded, outbound,
        "re-encode of the decoded value is not byte-stable — \
         forwarding peers would rewrite frames",
    );
}

/// Pin BOTH directions of one wire shape: `value` encodes to exactly
/// `expected_wire`, and `expected_wire` decodes back into exactly `value`.
///
/// Use with checked-in goldens (see the B254 fixtures/) so a rename or dropped
/// field fails loudly instead of silently changing the persisted/broadcast
/// shape.
///
/// # Panics
/// When either direction disagrees with the golden.
pub fn assert_wire_shape<T>(value: &T, expected_wire: &serde_json::Value)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let encoded = match serde_json::to_value(value) {
        Ok(encoded) => encoded,
        Err(e) => panic!("encode-to-value failed: {e}"),
    };
    assert_eq!(
        &encoded, expected_wire,
        "wire shape drifted — update the golden deliberately, not silently"
    );
    let decoded: T = match serde_json::from_value(expected_wire.clone()) {
        Ok(decoded) => decoded,
        Err(e) => panic!(
            "golden is unreadable by its own type — the pinned wire shape and \
             the deserializer disagree: {e}"
        ),
    };
    assert_eq!(
        &decoded, value,
        "decoding the golden did not reconstruct the original value"
    );
}

/// A batch verifier: many fixtures of ONE type, verified together.
///
/// The reason this exists (and a plain `for` loop does not): a loop stops at
/// the first panic, hiding every later breakage. A suite runs all cases and
/// reports EVERY broken fixture in one message, which turns "fix, rerun, find
/// the next one" into a single diagnostic pass.
///
/// ```
/// use coxagent_contracts::round_trip::SymmetrySuite;
///
/// let a = sample_one();
/// let b = sample_two();
/// SymmetrySuite::of("JobSpec")
///     .case("minimal", &a)
///     .case("with args", &b)
///     .verify();
/// # fn sample_one() -> coxagent_contracts::JobSpec {
/// #     coxagent_contracts::JobSpec { v: 1, project: "p".into(), kind: "cycle".into(), args: Default::default() }
/// # }
/// # fn sample_two() -> coxagent_contracts::JobSpec {
/// #     coxagent_contracts::JobSpec { v: 1, project: "q".into(), kind: "force_merge".into(), args: serde_json::json!({"id": 7}) }
/// # }
/// ```
#[derive(Debug)]
pub struct SymmetrySuite<'a, T>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    type_name: &'a str,
    cases: Vec<(&'a str, &'a T)>,
    failures: Vec<String>,
}

impl<'a, T> SymmetrySuite<'a, T>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    /// Start a suite for `type_name` (used only in diagnostics).
    #[must_use]
    pub fn of(type_name: &'a str) -> Self {
        Self {
            type_name,
            cases: Vec::new(),
            failures: Vec::new(),
        }
    }

    /// Register one fixture under a short label.
    #[must_use]
    pub fn case(mut self, label: &'a str, value: &'a T) -> Self {
        self.cases.push((label, value));
        self
    }

    /// Run every registered case through [`assert_symmetric_round_trip`],
    /// collecting instead of propagating the first failure.
    ///
    /// # Panics
    /// After ALL cases ran, listing every label that broke — never mid-suite.
    pub fn verify(mut self) {
        for (label, value) in &self.cases {
            if let Err(message) = run_case::<T>(value) {
                self.failures.push(format!("{label}: {message}"));
            }
        }
        assert!(
            self.failures.is_empty(),
            "{} wire round trip broke for {} of {} fixtures:\n  - {}",
            self.type_name,
            self.failures.len(),
            self.cases.len(),
            self.failures.join("\n  - "),
        );
    }
}

/// One symmetric round trip expressed as a verdict instead of a panic, so the
/// suite can keep going past a failure.
fn run_case<T>(value: &T) -> Result<(), String>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let outbound = serde_json::to_vec(value).map_err(|e| format!("encode failed: {e}"))?;
    let decoded: T =
        serde_json::from_slice(&outbound).map_err(|e| format!("decode failed: {e}"))?;
    if decoded != *value {
        return Err(format!(
            "asymmetric encode→decode (wire bytes: {})",
            String::from_utf8_lossy(&outbound),
        ));
    }
    let reencoded = serde_json::to_vec(&decoded).map_err(|e| format!("re-encode failed: {e}"))?;
    if reencoded != outbound {
        return Err("re-encode of the decoded value is not byte-stable".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Compliant {
        name: String,
        count: u32,
    }

    /// Violation fixture: `secret` carries data the wire drops, so a naive
    /// encode→decode must NOT compare equal — this is what the harness catches.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Lossy {
        visible: bool,
        #[serde(skip)]
        hidden: u32,
    }

    fn compliant(name: &str, count: u32) -> Compliant {
        Compliant {
            name: name.to_owned(),
            count,
        }
    }

    #[test]
    fn a_compliant_fixture_passes_the_symmetric_round_trip() {
        let fixture = compliant("bus envelope payload", 42);
        assert_symmetric_round_trip(&fixture);
    }

    #[test]
    fn a_compliant_fixture_matches_its_pinned_wire_golden_in_both_directions() {
        let fixture = compliant("golden", 1);
        assert_wire_shape(&fixture, &serde_json::json!({"name": "golden", "count": 1}));
    }

    #[test]
    #[should_panic(expected = "asymmetric encode→decode")]
    fn a_lossy_type_is_caught_red_by_the_harness_not_silently_passed() {
        let lossy = Lossy {
            visible: true,
            hidden: 7,
        };
        assert_symmetric_round_trip(&lossy);
    }

    #[test]
    #[should_panic(expected = "wire shape drifted")]
    fn a_renamed_field_fails_the_wire_golden_proof() {
        let fixture = compliant("shape", 0);
        assert_wire_shape(&fixture, &serde_json::json!({"title": "shape"}));
    }

    #[test]
    fn a_suite_runs_every_case_and_reports_only_when_at_least_one_breaks() {
        let ok1 = compliant("first", 1);
        let ok2 = compliant("second", 2);
        SymmetrySuite::of("Compliant")
            .case("first", &ok1)
            .case("second", &ok2)
            .verify(); // must complete without panicking

        // And the counting message is honest: 2 fixtures, both broken below.
        let broken = Lossy {
            visible: false,
            hidden: 1,
        };
        let result = std::panic::catch_unwind(|| {
            SymmetrySuite::of("Lossy")
                .case("one", &broken)
                .case("two", &broken)
                .verify()
        });
        let message = result
            .err()
            .and_then(|payload| {
                payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            })
            .expect("suite with broken fixtures must panic");
        assert!(
            message.contains("broke for 2 of 2 fixtures"),
            "suite must account for every case, got: {message}"
        );
    }
}
