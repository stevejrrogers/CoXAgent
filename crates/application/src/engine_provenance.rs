//! Pure render decisions for engine/model provenance (CXA-F257) — the shared
//! contract every surface (inbox verify card, ticket detail) renders through,
//! so "model unknown" reads identically everywhere. No IO, no state: pure
//! functions over the provenance records, like `forensics`/`dependency_radar`.

use crate::state::EngineAttempt;
use coxagent_domain::Role;

/// The explicit marker shown when an engine cannot report a model id —
/// the engine name is still shown, never a blank field (CXA-F257 AC3).
pub const MODEL_UNKNOWN: &str = "model unknown";

/// The AC3 case split: the model id an engine reported, or `None` when it
/// reported none (empty/blank) — `None` is the explicit unknown downstream;
/// an empty string never is.
#[must_use]
pub fn model_id(raw: &str) -> Option<String> {
    let m = raw.trim();
    (!m.is_empty()).then(|| m.to_owned())
}

/// One attempt as the verify surfaces render it: `engine · model`, or
/// `engine · model unknown` when the model is absent. Mirrors the
/// `forensics::provenance_label` unknown-state precedent (CXA-F241).
#[must_use]
pub fn attempt_label(a: &EngineAttempt) -> String {
    match a.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => format!("{} · {m}", a.engine),
        None => format!("{} · {MODEL_UNKNOWN}", a.engine),
    }
}

/// The dashboard's label for the role that ran a step — the exact strings
/// the activity feed uses (`DEV-BUG`), so a provenance row reads like the
/// activity it belongs to.
#[must_use]
pub fn role_label(role: &Role) -> String {
    (match role {
        Role::Ba => "BA",
        Role::Po => "PO",
        Role::Sm => "SM",
        Role::Sa => "SA",
        Role::Pd => "PD",
        Role::DevBug => "DEV-BUG",
        Role::DevFeature => "DEV-FEATURE",
        Role::Test => "TEST",
        Role::Docs => "DOCS",
        Role::User => "USER",
        Role::System => "SYSTEM",
    })
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(engine: &str, model: Option<&str>) -> EngineAttempt {
        EngineAttempt {
            engine: engine.to_owned(),
            model: model.map(str::to_owned),
        }
    }

    #[test]
    fn a_known_model_renders_engine_and_model_without_the_unknown_marker() {
        let label = attempt_label(&attempt("claude", Some("opus")));
        assert_eq!(label, "claude · opus");
        assert!(!label.contains(MODEL_UNKNOWN));
    }

    #[test]
    fn a_missing_or_blank_model_is_explicit_never_a_blank_field() {
        for a in [attempt("claude", None), attempt("claude", Some("  "))] {
            let label = attempt_label(&a);
            assert!(label.contains("claude"), "{label:?}");
            assert!(label.contains(MODEL_UNKNOWN), "{label:?}");
            assert!(!label.trim().is_empty());
        }
    }

    #[test]
    fn model_id_normalizes_blank_to_the_explicit_unknown() {
        assert_eq!(model_id("opus"), Some("opus".to_owned()));
        assert_eq!(model_id(" opus "), Some("opus".to_owned()));
        assert_eq!(model_id(""), None);
        assert_eq!(model_id("   "), None);
    }

    #[test]
    fn role_labels_match_the_activity_feeds_strings() {
        assert_eq!(role_label(&Role::DevBug), "DEV-BUG");
        assert_eq!(role_label(&Role::DevFeature), "DEV-FEATURE");
        assert_eq!(role_label(&Role::Ba), "BA");
    }
}
