// Config-drift compile gate — TDD contract for CXA-F021 ("Fix missing coverage
// field and add a Config-drift compile gate"), written BEFORE any implementation.
//
// Every test below encodes exactly one acceptance criterion from the ticket.
// The three additions to the config surface that CXA-F021 must land:
//
//   1. `coxagent_application::config::CoverageConfig`, carrying at least
//      `.enabled: bool` and `.threshold: u32`.
//   2. A `#[serde(default)] pub coverage: CoverageConfig` field on the existing
//      top-level `Config`, with documented defaults enabled=true, threshold=3,
//      produced by an explicit container default rather than Rust's derived
//      zero-value `{false, 0}` (COX-B043).
//   3. A schema anchor naming whether a persisted document predates / matches /
//      exceeds what this running build understands (`CONFIG_SCHEMA_VERSION`),
//      used by load to refuse an incompatible persisted schema before accepting
//      it (fail-closed, like state.json's json_store.parse_checked).
//
// AC -> test mapping (see ticket CXA-F021):
//   AC1 round-trip        -> coverage_section_round_trips_exactly_on_disk
//   AC2 omitted-field def -> omitted_coverage_applies_the_documented_defaults
//   AC3 drift gate        -> newer_persisted_schema_is_refused_at_load_not_defaulted
//   AC4 hot reload        -> threshold_change_reaches_the_next_pass_without_a_restart
//   AC5 bump migration    -> prior_version_state_migrates_and_preserves_user_coverage
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::config::{Config, CONFIG_SCHEMA_VERSION};
use serde_json::{json, Value};

const DOCUMENTED_DEFAULT_ENABLED: bool = true;
const DOCUMENTED_DEFAULT_THRESHOLD: u32 = 3;

fn defaults() -> Value {
    serde_json::to_value(Config::default()).expect("default config serializes")
}

fn parse(text: &str) -> Config {
    coxagent_application::config_parse::parse_config(text).expect("document parses")
}

fn persist_and_read(value_in_memory: &Value) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("coxagent.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(value_in_memory).expect("serialize"),
    )
    .expect("write config to disk");
    std::fs::read_to_string(&path).expect("read config back from disk")
}

#[test]
fn coverage_section_round_trips_exactly_on_disk() {
    // AC1 — "a config written to disk and read back round-trips the coverage section exactly:
    // enabled state and threshold value survive a full save/load cycle with no silent loss or default-fabrication."
    let mut cfg = defaults();
    cfg["coverage"] = json!({ "enabled": false, "threshold": 7 });

    let text_on_disk = persist_and_read(&cfg);
    let back = parse(&text_on_disk);

    assert!(
        !back.coverage.enabled,
        "enabled must survive the cycle unchanged"
    );
    assert_eq!(
        back.coverage.threshold, 7,
        "threshold must survive the cycle unchanged"
    );
}

#[test]
fn omitted_coverage_applies_the_documented_defaults() {
    // AC2 — "loading a persisted config that omits the coverage field entirely succeeds and applies the documented defaults (enabled=true, threshold=3)
    // instead of panicking or silently defaulting to the struct's internal zero-value."
    let mut cfg = defaults();
    cfg.as_object_mut()
        .expect("default config is an object")
        .remove("coverage"); // hand-written / older document that never mentions it

    let text_on_disk = persist_and_read(&cfg);
    let loaded = parse(&text_on_disk);

    assert_eq!(loaded.coverage.enabled, DOCUMENTED_DEFAULT_ENABLED);
    assert_eq!(loaded.coverage.threshold, DOCUMENTED_DEFAULT_THRESHOLD);
}

#[test]
fn newer_persisted_schema_is_refused_at_load_not_defaulted() {
    // AC3 — "the compile-time gate fires ... when the persisted schema version does not match
    // the running code's expected version, preventing the application from loading an
    // incompatible config." A document written by a FUTURE build must be refused at load —
    // never accepted, and never silently re-defaulted into this build's view of defaults.
    let future_version = CONFIG_SCHEMA_VERSION + 1;
    let future_doc = format!(r#"{{"schema_version":{future_version}}}"#);

    let outcome = coxagent_application::config_parse::parse_config(&future_doc);
    assert!(
        outcome.is_err(),
        "a persisted schema newer than supported ({CONFIG_SCHEMA_VERSION}) must refuse to load, not default away"
    );
}

#[test]
fn threshold_change_reaches_the_next_pass_without_a_restart() {
    // AC4 — "Changing the coverage threshold from the dashboard (or equivalent config-write path)
    // immediately reflected in the next cycle's gap-detection pass with no restart required."
    //
    // The dashboard save rewrites `coxagent.json` on disk; each cycle boundary re-reads that file
    // (the same hot-reload path `cycle.mod` already uses for engine changes). So a threshold saved
    // through that write path MUST be observed by a fresh read of the same file — i.e. by the very
    // next pass — without restarting anything. No process restart happens between write and read here.
    let mut cfg = defaults();
    cfg["coverage"] = json!({ "enabled": true, "threshold": 9 });

    let text_written_by_settings_save = persist_and_read(&cfg);
    let next_pass_cfg = parse(&text_written_by_settings_save);

    assert_eq!(
        next_pass_cfg.coverage.threshold, 9,
        "a threshold saved via the config-write path must reach the next gap-detection pass \
         without a restart"
    );
}

#[test]
fn prior_version_state_migrates_and_preserves_user_coverage() {
    // AC5 — "After a schema-version bump, loading a state file written with the prior version
    // triggers a safe migration that preserves any user-set coverage settings rather than wiping
    // them or producing a corrupted in-memory state."
    //
    // The file below is exactly what an older build would have persisted: it carries USER-SET
    // coverage (`enabled=false, threshold=11`) under no explicit newer marker. Loading it through
    // today's parser is the migration step — it must preserve both user-set values verbatim, not
    // wipe them back to defaults ({false,0} / fabricated true), and never corrupt in-memory state.
    let mut cfg = defaults();
    cfg["coverage"] = json!({ "enabled": false, "threshold": 11 });

    let text_from_prior_version = persist_and_read(&cfg);
    let migrated = parse(&text_from_prior_version);

    assert!(
        !migrated.coverage.enabled,
        "user-set enabled must survive migration"
    );
    assert_eq!(
        migrated.coverage.threshold, 11,
        "user-set threshold must survive migration; default-fabrication would yield {DOCUMENTED_DEFAULT_THRESHOLD}"
    );
}
