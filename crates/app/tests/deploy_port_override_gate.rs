//! CXA-B334 regression guard: the one-shot deploy compose must take the hub's
//! HOST publish port from `APP_PORT` (default 8101) — never a hardcoded
//! literal.
//!
//! The bug: `deploy/docker-compose.yml` published `- "8101:4000"`, so any
//! re-deploy racing a still-running old container — or any other stack
//! squatting 8101 on the same host — died with
//! `Bind for 0.0.0.0:8101 failed: port is already allocated` and the operator
//! had NO knob to move out of the way. The root compose already spoke the
//! env-driven form (`${APP_PORT:-8101}:4000`, CXA-B069/B077 lineage); the
//! one-shot deploy file was the outlier.
//!
//! Guard shape matches this suite's house pattern (cf. `hub_docker_gate.rs`):
//! pure functions over compose source returning `Err`, exercised against the
//! REAL committed file and against synthetic drift to prove each rule bites.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// The guards: pure functions over the compose source.
// ---------------------------------------------------------------------------

/// Every quoted short-form entry in the compose source that maps something to
/// container port 4000 — the port `service.rs` binds inside the hub image.
/// Volume mounts (`…:/data/TL:rw`) and bare names never carry a numeric
/// container-side `4000`, so this isolates the hub's publish mapping(s).
///
/// Public-in-crate only so the drift tests below exercise it directly.
fn hub_publish_specs(compose_src: &str) -> Vec<String> {
    compose_src
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            line.strip_prefix("- \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_owned)
        })
        .filter(|spec| {
            spec.rsplit_once(':')
                .is_some_and(|(host, container)| container == "4000" && !host.is_empty())
        })
        .collect()
}

/// `Ok` only when the compose source publishes the hub through the
/// `APP_PORT` knob with the documented default 8101, and nothing ELSE also
/// claims container 4000 (a second mapping would reintroduce the collision
/// under a different name).
const EXPECTED_HOST_SPEC: &str = "${APP_PORT:-8101}";

fn app_port_publish_gate(compose_src: &str) -> Result<(), String> {
    let specs = hub_publish_specs(compose_src);
    if specs.is_empty() {
        return Err(
            "deploy compose no longer publishes the hub's container port 4000 at all: the \
             smoke gates probe a mapping that does not exist"
                .to_owned(),
        );
    }
    if specs.len() > 1 {
        return Err(format!(
            "deploy compose publishes container port 4000 more than once ({specs:?}): extra \
             mappings multiply the 'port is already allocated' surface (CXA-B334)"
        ));
    }
    let host = specs[0]
        .rsplit_once(':')
        .map_or("", |(host, _)| host)
        .to_owned();
    if host == EXPECTED_HOST_SPEC {
        return Ok(());
    }
    Err(format!(
        "one-shot deploy compose hardcodes its hub host-port `{}` instead of taking \
         `{EXPECTED_HOST_SPEC}` from APP_PORT (CXA-B334): a redeploy cannot escape a host \
         where 8101 is held — docker dies with 'port is already allocated' and no knob",
        specs[0],
    ))
}

// ---------------------------------------------------------------------------
// Regression: the REAL committed file satisfies the gate.
// Fails on pre-fix source (`- "8101:4000"`), proving the fix is load-bearing.
// ---------------------------------------------------------------------------

#[test]
fn one_shot_deploy_publishes_via_app_port_not_a_hardcoded_port() {
    let src = read("deploy/docker-compose.yml");
    app_port_publish_gate(&src).unwrap_or_else(|e| panic!("{e}"));
}

// ---------------------------------------------------------------------------
// Drift tests: every way to break the contract must trip ONE named branch.
// These pass with or without the real fix present — they pin the rules.
// ---------------------------------------------------------------------------

#[test]
fn hardcoded_literal_reintroduces_the_b334_collision() {
    let err = app_port_publish_gate("- \"8101:4000\"").unwrap_err();
    assert!(err.contains("APP_PORT"), "error must name the knob: {err}");
    assert!(err.contains("CXA-B334"), "error must cite the ticket: {err}");
}

#[test]
fn renamed_env_var_is_rejected_so_the_documented_knob_stays_the_knob() {
    // redeploy-smoke.sh and deploy_smoke.rs both speak APP_PORT by name; a
    // silent rename breaks them downstream even though the mapping looks
    // equally overridable.
    let err = app_port_publish_gate("- \"${HUB_PORT:-8101}:4000\"").unwrap_err();
    assert!(err.contains(EXPECTED_HOST_SPEC), "{err}");
}

#[test]
fn changed_default_port_desyncs_from_docs_and_smoke_harness() {
    // Docs (README/DEPLOYMENT) and the smoke base-port math all assume 8101.
    let err = app_port_publish_gate("- \"${APP_PORT:-9999}:4000\"").unwrap_err();
    assert!(err.contains("9999"), "error must show the drifted value: {err}");
}

#[test]
fn dropped_hub_mapping_fails_loudly_instead_of_probing_nothing() {
    let err = app_port_publish_gate("# app: ports removed entirely\n").unwrap_err();
    assert!(err.contains("no longer publishes"), "{err}");
}

#[test]
fn duplicate_hub_mappings_fail_even_when_each_is_well_formed() {
    let err =
        app_port_publish_gate("- \"${APP_PORT:-8101}:4000\"\n- \"${APP_PORT:-8102}:4000\"")
            .unwrap_err();
    assert!(err.contains("more than once"), "{err}");
}

#[test]
fn volume_mounts_and_other_services_never_confuse_the_parser() {
    // Synthetic realistic mix: mounts, a sibling service port, a bare name —
    // none of these may leak into the hub-mapping set.
    let src = concat!(
        "minio:\n",
        "    ports:\n",
        "      - \"127.0.0.1:19001:19001\"\n",
        "    volumes:\n",
        "      - ./data/TL:/data/TL:rw\n",
        "      - ./data/registry.json:/data/registry.json:rw\n",
        "    depends_on:\n",
        "      - redis\n",
        "app:\n",
        "    command:\n",
        "      - --max-port=49200\n",
        "    ports:\n",
        "      - \"${APP_PORT:-8101}:4000\"\n",
    );
    let specs = hub_publish_specs(src);
    assert_eq!(specs, vec!["${APP_PORT:-8101}:4000".to_owned()]);
    app_port_publish_gate(src).unwrap();
}
