//! Turning a `coxagent.json` document into a [`Config`] — fail-closed, with the
//! offending field named.
//!
//! This lives apart from `config.rs` (which describes what a project may
//! configure) because it answers a different question: what happens when the
//! document does NOT match those types. The rule is COX-B043's lesson — a
//! document that fails to deserialize is an ERROR, never `Config::default()`.
//! Substituting defaults empties `policy.model_allowlist`,
//! `policy.forbidden_paths` and `policy.daily_budget_usd`, so one stray field
//! anywhere in the file (an out-of-range `deploy.host_port`, a hand edit, a bad
//! migration) would turn the governance gates off on the next start, unasked
//! and unseen. Gates are unskippable by default; a config problem is locked to
//! load time and named, not guessed at during a run.

use crate::config::Config;

/// A `coxagent.json` document that does not deserialize into a [`Config`],
/// carrying the path of the field that broke it (e.g. `deploy.host_port`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {detail}")]
pub struct ConfigParseError {
    /// Dotted path of the offending field, or [`WHOLE_DOCUMENT`] when the text
    /// is not valid JSON at all, so there is no one field to blame.
    pub field: String,
    /// What was wrong with the value, in serde's words.
    pub detail: String,
}

/// [`ConfigParseError::field`] when the failure belongs to no single field —
/// the text is not JSON, so serde never reached one.
pub const WHOLE_DOCUMENT: &str = "<document>";

/// Parse a `coxagent.json` document into a [`Config`].
///
/// Every section is `#[serde(default)]`, so a config written by an older
/// version — or by hand, mentioning only what it cares about — still loads with
/// the defaults for whatever it omits. A field that IS present but
/// unrepresentable is refused instead: callers must not fall back to
/// [`Config::default`], which would silently drop the project's governance
/// policy (COX-B043).
///
/// # Errors
///
/// Returns [`ConfigParseError`] naming the offending field when the text is not
/// valid JSON, or holds a value the config schema cannot represent (a port
/// outside `u16`, a string where a number belongs, an unknown engine, …).
pub fn parse_config(text: &str) -> Result<Config, ConfigParseError> {
    let deserializer = &mut serde_json::Deserializer::from_str(text);
    serde_path_to_error::deserialize(deserializer).map_err(|err| {
        let path = err.path().to_string();
        let inner = err.into_inner();
        // Only a data error is ABOUT a field. A syntax/EOF failure never got
        // far enough to be inside one, and the tracker renders that path as
        // "?" — useless in a log line, so name what actually broke.
        let field = match inner.classify() {
            serde_json::error::Category::Data => path,
            _ => WHOLE_DOCUMENT.to_owned(),
        };
        ConfigParseError {
            field,
            detail: inner.to_string(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully governed project config, with `host_port` left to the caller so
    /// one bad field can be injected into an otherwise healthy document.
    fn governed(host_port: &str) -> String {
        format!(
            r#"{{
              "engine": {{
                "default": {{ "engine": "claude", "model": "sonnet" }},
                "per_role": {{}},
                "fallbacks": []
              }},
              "policy": {{
                "model_allowlist": ["claude/sonnet"],
                "forbidden_paths": ["infra/"],
                "daily_budget_usd": 25.0
              }},
              "deploy": {{ "host_port": {host_port} }}
            }}"#
        )
    }

    #[test]
    fn an_out_of_range_host_port_names_the_field_instead_of_resetting_the_config() {
        let err = parse_config(&governed("999999")).expect_err("999999 is outside u16");
        assert_eq!(err.field, "deploy.host_port");
        assert!(err.detail.contains("u16"), "{}", err.detail);
    }

    #[test]
    fn a_malformed_policy_field_is_refused_rather_than_defaulted_away() {
        let text = r#"{
          "engine": { "default": { "engine": "claude", "model": "sonnet" } },
          "policy": { "daily_budget_usd": "twenty" }
        }"#;
        let err = parse_config(text).expect_err("a budget is a number");
        assert_eq!(err.field, "policy.daily_budget_usd");
    }

    #[test]
    fn a_document_that_is_not_json_blames_the_document_not_a_field() {
        let err = parse_config("{ not json at all").expect_err("broken document");
        assert_eq!(err.field, WHOLE_DOCUMENT);
    }

    #[test]
    fn a_valid_document_keeps_every_governance_rule_it_declares() {
        let cfg = parse_config(&governed("8101")).expect("a valid document");
        assert_eq!(cfg.policy.model_allowlist, ["claude/sonnet"]);
        assert_eq!(cfg.policy.forbidden_paths, ["infra/"]);
        assert_eq!(cfg.policy.daily_budget_usd, Some(25.0));
        assert_eq!(cfg.deploy.host_port, Some(8101));
    }

    #[test]
    fn a_document_that_omits_a_section_still_gets_that_sections_defaults() {
        let cfg = parse_config(r#"{"engine":{"default":{"engine":"claude","model":"sonnet"}}}"#)
            .expect("an older, shorter config still loads");
        assert!(cfg.policy.model_allowlist.is_empty());
        assert_eq!(cfg.deploy.host_port, None);
        assert!(cfg.deploy.enabled);
    }

    /// Fail-closed is about VALUES the schema cannot represent, not about
    /// sections a document never mentions. A hand-written config that only sets
    /// a policy and a port must load — refusing it would discard exactly the
    /// governance rules this ticket exists to protect.
    #[test]
    fn a_hand_written_document_without_an_engine_section_loads() {
        let cfg = parse_config(
            r#"{"policy":{"model_allowlist":["claude/sonnet"]},"deploy":{"host_port":8101}}"#,
        )
        .expect("an omitted engine section is not a corrupt config");

        assert_eq!(cfg.policy.model_allowlist, ["claude/sonnet"]);
        assert_eq!(cfg.deploy.host_port, Some(8101));
        assert_eq!(cfg.engine.default.model, "sonnet");
    }

    /// The other side of the same line: a section that IS present but holds a
    /// value the schema cannot represent still fails, naming the field.
    #[test]
    fn an_engine_section_with_an_unknown_engine_is_still_refused() {
        let err = parse_config(r#"{"engine":{"default":{"engine":"gpt5","model":"x"}}}"#)
            .expect_err("gpt5 is not an EngineKind");

        assert_eq!(err.field, "engine.default.engine");
    }
}
