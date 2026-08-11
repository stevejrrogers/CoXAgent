//! COX-B050: the config edge cases an operator actually hand-writes, against
//! the exact documents they would write — not against a serialized
//! `Config::default()` with one field swapped.
//!
//! The distinction matters. The unit tests in `config_repair` patch a full
//! default document, so every section is present and only the field under test
//! is wrong. A real `coxagent.json` is sparse: it names the handful of settings
//! someone cared about and omits the rest. That shape exercises a different
//! path (required fields absent, whole sections missing), and it is the shape
//! in the ticket's own repro — so it is the shape pinned here.

// An integration test is its own crate, so the lib's test-only allow does not
// reach it; a failed `expect` here IS the test failing, which is the point.
#![allow(clippy::expect_used)]

use coxagent_application::salvage_config;

/// The ticket's literal repro document, verbatim.
const TICKET_REPRO: &str = r#"{"deploy":{"host_port":99999,"enabled":true,"auto_rollback":true,"max_rollback_age_secs":42},"engine":{}}"#;

/// AC (COX-B050): loading this used to hand back `Config::default()` — the
/// reported symptom was `auto_rollback` silently flipping true -> false and
/// `max_rollback_age_secs` resetting 42 -> 3600 because a NEIGHBOURING port had
/// one digit too many. Every field the operator wrote must survive; only the
/// unreadable one may be lost.
#[test]
fn the_tickets_own_repro_keeps_every_field_but_the_broken_one() {
    let salvaged = salvage_config(TICKET_REPRO).expect("a JSON config must salvage");

    assert!(
        salvaged.config.deploy.auto_rollback,
        "auto_rollback must not revert because of a bad port"
    );
    assert_eq!(
        salvaged.config.deploy.max_rollback_age_secs, 42,
        "max_rollback_age_secs must not reset to the default"
    );
    assert!(salvaged.config.deploy.enabled);
    assert_eq!(
        salvaged.config.deploy.host_port, None,
        "the out-of-range port is the one field that cannot survive"
    );
    assert!(
        salvaged
            .defects
            .iter()
            .any(|d| d.path == "deploy.host_port"),
        "and its loss is reported, not silent: {:?}",
        salvaged.defects
    );
}

/// A sparse document omits most sections entirely. Those are defaults by
/// design, not defects — an operator who never wrote a `policy` section must
/// not be told their config is broken.
#[test]
fn omitted_sections_are_defaults_not_defects() {
    let salvaged = salvage_config(r#"{"engine":{},"deploy":{"host_port":8101}}"#)
        .expect("a JSON config must salvage");

    assert_eq!(salvaged.config.deploy.host_port, Some(8101));
    assert!(
        !salvaged
            .defects
            .iter()
            .any(|d| d.path.starts_with("policy") || d.path.starts_with("workflow")),
        "sections the operator never wrote are not faults: {:?}",
        salvaged.defects
    );
}

/// Edge case per the team's recorded decision: a `host_port` of `null`. It is a
/// legal `Option<u16>`, so it is not a defect and must not be reported as one.
#[test]
fn a_null_port_is_legal_and_reported_as_clean() {
    let salvaged = salvage_config(r#"{"engine":{},"deploy":{"host_port":null,"enabled":true}}"#)
        .expect("a JSON config must salvage");

    assert_eq!(salvaged.config.deploy.host_port, None);
    assert!(salvaged.config.deploy.enabled);
    assert!(
        !salvaged
            .defects
            .iter()
            .any(|d| d.path == "deploy.host_port"),
        "an explicit null is a choice, not a fault: {:?}",
        salvaged.defects
    );
}

/// Edge case per the team's recorded decision: a `deploy` section with no
/// `host_port` key at all. Same rule — absent is unset, not broken.
#[test]
fn a_missing_port_is_legal_and_reported_as_clean() {
    let salvaged = salvage_config(r#"{"engine":{},"deploy":{"enabled":true}}"#)
        .expect("a JSON config must salvage");

    assert_eq!(salvaged.config.deploy.host_port, None);
    assert!(salvaged.config.deploy.enabled);
    assert!(
        !salvaged
            .defects
            .iter()
            .any(|d| d.path == "deploy.host_port"),
        "an unset port is not a fault: {:?}",
        salvaged.defects
    );
}

/// A truncated write is not a config with a bad field in it — there is no
/// field to blame, and the caller must be told so rather than handed defaults
/// that look like settings.
#[test]
fn a_truncated_file_is_an_error_not_a_silent_default() {
    assert!(salvage_config(r#"{"deploy":{"host_port":"#).is_err());
}
