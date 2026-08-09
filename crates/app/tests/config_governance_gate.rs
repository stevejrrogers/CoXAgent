//! COX-B043 regression: one malformed field in `coxagent.json` must not boot a
//! project on `Config::default()`.
//!
//! Defaults are not a neutral fallback — they empty `policy.model_allowlist`,
//! `policy.forbidden_paths` and `policy.daily_budget_usd`, i.e. they turn the
//! governance gates OFF. The loader therefore refuses a config it cannot parse
//! (the project fails to load, loudly) instead of quietly running ungoverned.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_app::load_config;

/// A workspace laid out the way `load_config` expects: `coxagent.json` at the
/// root, state beside it. Returns the state dir the loader is given.
fn workspace(config_json: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp workspace");
    std::fs::write(dir.path().join("coxagent.json"), config_json).expect("write config");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    (dir, state)
}

/// A governed project config with `host_port` left open for injection.
fn governed(host_port: &str) -> String {
    format!(
        r#"{{
          "engine": {{ "default": {{ "engine": "claude", "model": "sonnet" }} }},
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
fn an_out_of_range_host_port_fails_the_load_instead_of_disabling_governance() {
    let (_dir, state) = workspace(&governed("999999"));

    let msg = load_config(&state).expect_err("999999 is outside u16");

    assert!(msg.contains("coxagent.json"), "{msg}");
    assert!(msg.contains("deploy.host_port"), "{msg}");
}

#[test]
fn a_document_that_is_not_json_fails_the_load_too() {
    let (_dir, state) = workspace("{ not json at all");

    let msg = load_config(&state).expect_err("broken document");

    assert!(msg.contains("coxagent.json"), "{msg}");
}

#[test]
fn a_valid_config_still_loads_with_its_governance_intact() {
    let (_dir, state) = workspace(&governed("8101"));

    let cfg = load_config(&state).expect("a valid config loads");

    assert_eq!(cfg.policy.model_allowlist, ["claude/sonnet"]);
    assert_eq!(cfg.policy.forbidden_paths, ["infra/"]);
    assert_eq!(cfg.policy.daily_budget_usd, Some(25.0));
    assert_eq!(cfg.deploy.host_port, Some(8101));
}

#[test]
fn a_workspace_with_no_config_at_all_still_gets_defaults() {
    let dir = tempfile::tempdir().expect("temp workspace");
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).expect("state dir");

    let cfg = load_config(&state).expect("no config is not an error — greenfield defaults");

    assert!(cfg.policy.model_allowlist.is_empty());
}
