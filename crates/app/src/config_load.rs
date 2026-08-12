// Part of the composition root split by concern — see lib.rs.
#![allow(clippy::wildcard_imports)]
//! Loading `coxagent.json`: read it once, parse it, validate the values that
//! must be right before anything runs, and self-heal the ones that can be.
//!
//! Config problems are settled HERE, at load time, rather than guessed at
//! wherever the value is eventually used — a `deploy.host_port` that no client
//! can connect to is a config error, and the further from the file it is
//! noticed, the more it looks like a bug in the app instead.
//!
//! Settled means answered, not swallowed. A document that does not parse is an
//! ERROR, never `Config::default()`: defaults empty `policy.model_allowlist`,
//! `policy.forbidden_paths` and `policy.daily_budget_usd`, so falling back to
//! them let one stray field turn the governance gates off on the next start
//! (COX-B043). The project fails to load instead, naming the field.

use super::*;

/// A project's config as the process should actually use it, plus what the
/// mandatory post-deploy health gate should probe.
///
/// The two travel together because they can disagree: the gate probes what the
/// raw file says (so a port `Config` accepts but nothing can publish still
/// fails the gate instead of silently skipping it — COX-B035), EXCEPT where
/// load itself replaced the port, in which case the replacement is what the app
/// publishes and therefore what is worth probing.
#[derive(Debug)]
pub(crate) struct LoadedConfig {
    /// Config with self-healed values applied.
    pub(crate) config: Config,
    /// The port [`coxagent_application::ports::outbound::verify_deploy_health`]
    /// should probe; `Err(())` means the configured port is one no client could
    /// reach and the gate must fail rather than pass unprobed.
    pub(crate) host_port_probe: Result<Option<u16>, ()>,
}

/// Load `coxagent.json` from the workspace root (parent of the state dir).
/// Config lives beside the state, written by `onboard`.
///
/// # Errors
///
/// The file exists but is unreadable or does not parse — see
/// [`load_config_with_probe`].
pub(crate) fn load_config(state_dir: &Path) -> Result<Config, String> {
    Ok(load_config_with_probe(state_dir)?.config)
}

/// Load `coxagent.json` and derive the deploy health gate's probe port from the
/// same single read, so the two can never drift apart.
///
/// # Errors
///
/// The file exists but cannot be read, is not valid JSON, or holds a value the
/// config schema cannot represent (COX-B043). Every one of those would
/// otherwise be answered with `Config::default()` — a config whose governance
/// policy is empty — so they stop the project from loading instead.
pub(crate) fn load_config_with_probe(state_dir: &Path) -> Result<LoadedConfig, String> {
    let root = state_dir.parent().unwrap_or(state_dir);
    let path = root.join("coxagent.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // No file at all is not a config error: a project that never wrote one
        // gets defaults, and the gate has nothing to probe.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedConfig {
                config: Config::default(),
                host_port_probe: Ok(None),
            })
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    // Healing is deliberately not attempted on a document we failed to
    // understand: rewriting it would destroy whatever the operator meant to say.
    let mut config = coxagent_application::config_parse::parse_config(&text)
        .map_err(|e| format!("invalid {}: {e}", path.display()))?;
    let healed = heal_host_port(root, &path, &mut config);
    let host_port_probe = if healed {
        // The raw text's port is the one we just rejected or found missing;
        // the app will publish the replacement, so probe that instead.
        Ok(config.deploy.host_port)
    } else {
        probe_from_raw(&text)
    };
    Ok(LoadedConfig {
        config,
        host_port_probe,
    })
}

/// The health gate's view of the raw config text — shared with chat and the PR
/// preview so every deploy call site rejects the same values.
pub(crate) fn probe_from_raw(text: &str) -> Result<Option<u16>, ()> {
    coxagent_application::ports::outbound::parse_deploy_host_port(text)
}

/// Self-heal a project left without a usable deploy port: assign a free
/// `host_port` and persist it, so a project onboarded before per-project ports
/// (or with the field cleared, or set to a port nothing can bind) stops
/// colliding on the shared default port. Picks the lowest port in range that no
/// sibling project claims and that is currently bindable, so two port-less
/// projects on one host land on different ports. Best-effort.
///
/// Returns whether the config's `host_port` was replaced — the caller needs to
/// know, because a replaced port is the one the deploy health gate must probe.
pub(crate) fn heal_host_port(root: &Path, cfg_path: &Path, cfg: &mut Config) -> bool {
    match cfg.deploy.host_port {
        Some(port) if coxagent_application::ports::outbound::is_publishable_host_port(port) => {
            return false
        }
        // Bounds are checked HERE rather than left to whoever probes the port
        // later: `0` deserializes into `u16` happily, so every downstream
        // reader sees a configured port and the health gate spends its whole
        // window failing to connect, reporting a config error as an app that
        // "never binds its port" (COX-B042). Loud, once, at load.
        Some(rejected) => tracing::warn!(
            "{}: deploy.host_port {rejected} is not a valid TCP port — assigning a free one",
            cfg_path.display()
        ),
        None => {}
    }
    // Ports already claimed by sibling projects under the same base dir.
    let mut used: std::collections::HashSet<u16> = std::collections::HashSet::new();
    if let Some(base) = root.parent() {
        if let Ok(entries) = std::fs::read_dir(base) {
            for e in entries.flatten() {
                let sib = e.path().join("coxagent.json");
                if sib == *cfg_path {
                    continue;
                }
                // Read the sibling's port from the raw document, not through
                // `Config`: a sibling with an unrelated bad field would
                // otherwise contribute nothing, and this project would heal
                // onto the very port that sibling publishes.
                if let Ok(text) = std::fs::read_to_string(&sib) {
                    if let Ok(Some(p)) = probe_from_raw(&text) {
                        used.insert(p);
                    }
                }
            }
        }
    }
    let bindable = |p: u16| std::net::TcpListener::bind(("127.0.0.1", p)).is_ok();
    let Some(port) = (PORT_BASE..PORT_BASE + 500).find(|p| !used.contains(p) && bindable(*p))
    else {
        // Nothing free to heal with. Leave the config alone so the caller falls
        // back to the raw text — an unpublishable port must still fail the
        // gate, not be waved through as "nothing configured".
        return false;
    };
    cfg.deploy.host_port = Some(port);
    if let Ok(text) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(cfg_path, text);
        tracing::info!("self-healed host port {port} for {}", cfg_path.display());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lay out a workspace the way `load_config*` expects it: an otherwise
    /// valid `coxagent.json` at the root whose `deploy.host_port` is the raw
    /// JSON token under test, state in a child dir. Returns the state dir to
    /// load from. Built from a real `Config` so nothing but the port under
    /// test can be what makes a case fail.
    fn workspace(raw_host_port: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let mut cfg = serde_json::to_value(Config::default()).expect("config as json");
        cfg["deploy"]["host_port"] =
            serde_json::from_str(raw_host_port).expect("host_port token is JSON");
        write_workspace(&serde_json::to_string_pretty(&cfg).expect("config text"))
    }

    /// The same layout for a project whose config is under test as raw text —
    /// so a case can hand over a document `Config` was never going to accept.
    fn write_workspace(config_text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        // A project dir of its own, so the sibling scan sees a realistic base.
        let root = dir.path().join("proj");
        let state = root.join(".coxagent");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::write(root.join("coxagent.json"), config_text).expect("write config");
        (dir, state)
    }

    /// A project that governs itself: a model allowlist, a forbidden path and a
    /// daily spend cap, with `deploy.host_port` left to the caller so one bad
    /// field can be injected into an otherwise healthy document.
    fn governed(raw_host_port: &str) -> String {
        let mut cfg = serde_json::to_value(Config::default()).expect("config as json");
        cfg["policy"]["model_allowlist"] = serde_json::json!(["claude/sonnet"]);
        cfg["policy"]["forbidden_paths"] = serde_json::json!(["infra/"]);
        cfg["policy"]["daily_budget_usd"] = serde_json::json!(25.0);
        cfg["deploy"]["host_port"] =
            serde_json::from_str(raw_host_port).expect("host_port token is JSON");
        serde_json::to_string_pretty(&cfg).expect("config text")
    }

    /// AC (COX-B042): `0` deserializes into `u16` without complaint, so
    /// nothing downstream can tell it from a port an operator meant. Load must
    /// refuse it and heal, or every deploy of this project reports "the app
    /// never binds its port" for a fault that is entirely in the config.
    #[test]
    fn a_zero_host_port_is_refused_and_healed_at_load() {
        let (_dir, state) = workspace("0");

        let LoadedConfig {
            config,
            host_port_probe,
        } = load_config_with_probe(&state).expect("a representable port loads");

        let healed = config.deploy.host_port.expect("a port must be assigned");
        assert_ne!(healed, 0, "port 0 must not survive config load");
        assert_eq!(
            host_port_probe,
            Ok(Some(healed)),
            "the gate must probe the port the app will actually publish"
        );
    }

    /// The healed port is persisted, so the next boot (and every other reader
    /// of the file — chat, PR preview) sees the fixed value rather than
    /// re-deriving it and disagreeing about which port to probe.
    #[test]
    fn the_healed_port_is_written_back_to_the_config_file() {
        let (_dir, state) = workspace("0");

        let healed = load_config_with_probe(&state)
            .expect("a representable port loads")
            .config
            .deploy
            .host_port
            .expect("a port must be assigned");

        let on_disk = std::fs::read_to_string(state.parent().expect("root").join("coxagent.json"))
            .expect("config still readable");
        assert_eq!(
            super::probe_from_raw(&on_disk),
            Ok(Some(healed)),
            "the file must carry the healed port, not the rejected one"
        );
    }

    /// A port an operator did configure is left exactly as written — the
    /// bounds check must not become a licence to reassign working projects.
    #[test]
    fn a_valid_host_port_is_left_untouched() {
        let (_dir, state) = workspace("8101");

        let LoadedConfig {
            config,
            host_port_probe,
        } = load_config_with_probe(&state).expect("a representable port loads");

        assert_eq!(config.deploy.host_port, Some(8101));
        assert_eq!(host_port_probe, Ok(Some(8101)));
    }

    /// A value `Config` cannot represent at all fails the LOAD (COX-B043), so
    /// no deploy of this project can reach the health gate at all — and the
    /// file is left alone, not overwritten from defaults.
    #[test]
    fn a_negative_host_port_fails_the_load_and_the_file_survives() {
        let (_dir, state) = workspace("-1");

        let msg = load_config_with_probe(&state).expect_err("-1 is outside u16");

        assert!(msg.contains("deploy.host_port"), "{msg}");
        let on_disk = std::fs::read_to_string(state.parent().expect("root").join("coxagent.json"))
            .expect("config still readable");
        assert!(
            on_disk.contains("-1"),
            "an unparseable config must not be rewritten from defaults"
        );
    }

    /// A sibling's published port is off-limits even when the rest of that
    /// sibling's config is unreadable — healing onto a port another project
    /// already publishes is the collision this scan exists to prevent, and one
    /// bad field elsewhere in the sibling's file must not hide its port.
    #[test]
    fn a_sibling_with_one_bad_field_still_reserves_its_port() {
        // What this host heals to with no sibling in the way — the port the
        // case below must therefore NOT pick.
        let (_first, first_state) = workspace("0");
        let contested = load_config_with_probe(&first_state)
            .expect("a representable port loads")
            .config
            .deploy
            .host_port
            .expect("a port must be assigned");

        let (dir, state) = workspace("0");
        let sibling = dir.path().join("sibling");
        std::fs::create_dir_all(&sibling).expect("sibling dir");
        std::fs::write(
            sibling.join("coxagent.json"),
            format!(r#"{{"policy":{{"daily_budget_usd":"twenty"}},"deploy":{{"host_port":{contested}}}}}"#),
        )
        .expect("write sibling config");

        let healed = load_config_with_probe(&state)
            .expect("a representable port loads")
            .config
            .deploy
            .host_port
            .expect("a port must be assigned");

        assert_ne!(
            healed, contested,
            "the sibling publishes {contested}; healing onto it collides"
        );
    }

    /// The COX-B043 regression: falling back to `Config::default()` empties the
    /// model allowlist, the forbidden paths and the daily budget, so one field
    /// serde cannot represent used to disable the governance gates on the next
    /// start. The load must fail rather than hand back an ungoverned config.
    #[test]
    fn one_malformed_field_never_empties_the_governance_policy() {
        let (_dir, state) = write_workspace(&governed("999999"));

        let msg = load_config_with_probe(&state).expect_err("999999 is outside u16");

        assert!(msg.contains("coxagent.json"), "{msg}");
        assert!(msg.contains("deploy.host_port"), "{msg}");
    }

    /// The control for the case above: the same governed document, with a port
    /// that IS representable, loads with every rule it declares.
    #[test]
    fn a_governed_config_loads_with_its_policy_intact() {
        let (_dir, state) = write_workspace(&governed("8101"));

        let config = load_config(&state).expect("a valid document loads");

        assert_eq!(config.policy.model_allowlist, ["claude/sonnet"]);
        assert_eq!(config.policy.forbidden_paths, ["infra/"]);
        assert_eq!(config.policy.daily_budget_usd, Some(25.0));
    }

    /// A document that is not JSON at all is a config error too — the same
    /// silent reset to defaults reached it before.
    #[test]
    fn a_document_that_is_not_json_fails_the_load() {
        let (_dir, state) = write_workspace("{ not json at all");

        let msg = load_config_with_probe(&state).expect_err("broken document");

        assert!(msg.contains("coxagent.json"), "{msg}");
    }

    /// No `coxagent.json` at all is not a config error: defaults, and nothing
    /// for the gate to probe.
    #[test]
    fn a_missing_config_file_loads_defaults_with_nothing_to_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("proj").join(".coxagent");
        std::fs::create_dir_all(&state).expect("state dir");

        let loaded = load_config_with_probe(&state).expect("a missing file is not an error");

        assert_eq!(loaded.config.deploy.host_port, None);
        assert_eq!(loaded.host_port_probe, Ok(None));
    }

    /// COX-B045/B050 repro: `host_port` above `u16::MAX` (e.g. `70000`) poisons
    /// the whole `Config` deserialization — one bad field used to discard every
    /// other setting too, not just the port. Per COX-B043 it must fail the LOAD,
    /// naming that one field, so no deploy reaches a gate that would pass unprobed
    /// with health checks silently disabled; and the file must survive untouched
    /// rather than be rewritten from defaults.
    #[test]
    fn an_out_of_range_host_port_fails_the_load_and_the_file_survives() {
        let (_dir, state) = workspace("70000");

        let msg = load_config_with_probe(&state).expect_err("70000 is outside u16");

        assert!(msg.contains("coxagent.json"), "{msg}");
        assert!(msg.contains("deploy.host_port"), "{msg}");
        let on_disk = std::fs::read_to_string(state.parent().expect("root").join("coxagent.json"))
            .expect("config still readable");
        assert!(
            on_disk.contains("70000"),
            "an unparseable config must not be rewritten from defaults"
        );
    }

    /// `host_port` present but explicitly `null` parses fine (it's a valid
    /// `Option<u16>`), so this must take the heal path, not the corrupt-config
    /// path — same as a `host_port` key that's absent altogether.
    #[test]
    fn an_explicit_null_host_port_is_healed_like_a_missing_one() {
        let (_dir, state) = workspace("null");

        let LoadedConfig {
            config,
            host_port_probe,
        } = load_config_with_probe(&state)
            .expect("an explicit null host_port must not be a config error");

        let healed = config.deploy.host_port.expect("a port must be assigned");
        assert_eq!(
            host_port_probe,
            Ok(Some(healed)),
            "the gate must probe the port the app will actually publish"
        );
    }

    /// `coxagent.json` present but with no `deploy.host_port` key at all
    /// (rather than an explicit `null`) must be indistinguishable from the
    /// null case — `#[serde(default)]` makes both mean "unconfigured".
    #[test]
    fn a_config_with_no_host_port_key_is_healed_like_a_missing_one() {
        let mut cfg = serde_json::to_value(Config::default()).expect("config as json");
        cfg["deploy"]
            .as_object_mut()
            .expect("deploy is an object")
            .remove("host_port");
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("proj");
        let state = root.join(".coxagent");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::write(
            root.join("coxagent.json"),
            serde_json::to_string_pretty(&cfg).expect("config text"),
        )
        .expect("write config");

        let LoadedConfig {
            config,
            host_port_probe,
        } = load_config_with_probe(&state)
            .expect("a config with no host_port key must not be a config error");

        let healed = config.deploy.host_port.expect("a port must be assigned");
        assert_eq!(
            host_port_probe,
            Ok(Some(healed)),
            "the gate must probe the port the app will actually publish"
        );
    }

    /// The ticket's literal repro (COX-B050): a valid JSON document whose only
    /// fault is an out-of-range `deploy.host_port`, carrying explicit non-default
    /// neighbours. It used to come back as `Config::default()` — `auto_rollback`
    /// flipped true -> false and `max_rollback_age_secs` reset 42 -> 3600 — so the
    /// safety setting turned itself off because a neighbouring field had one digit
    /// too many. Nothing may be handed back at all now; the load fails naming the
    /// one field that is actually broken.
    #[test]
    fn the_neighbours_of_a_bad_port_are_never_silently_reset_to_defaults() {
        let (_dir, state) = write_workspace(
            r#"{"deploy":{"host_port":99999,"enabled":true,"auto_rollback":true,
               "max_rollback_age_secs":42},"engine":{}}"#,
        );

        let msg = load_config_with_probe(&state).expect_err("99999 is outside u16");

        assert!(
            msg.contains("deploy.host_port"),
            "the one broken field is named, not the whole document: {msg}"
        );
    }
}
