// Part of the composition root split by concern — see lib.rs.
#![allow(clippy::wildcard_imports)]
//! Loading `coxagent.json`: read it once, parse it, validate the values that
//! must be right before anything runs, and self-heal the ones that can be.
//!
//! Config problems are settled HERE, at load time, rather than guessed at
//! wherever the value is eventually used — a `deploy.host_port` that no client
//! can connect to is a config error, and the further from the file it is
//! noticed, the more it looks like a bug in the app instead. A single
//! unparseable top-level section (e.g. an out-of-range `deploy.host_port`)
//! recovers by defaulting only that section, not the whole document, which
//! would silently wipe unrelated settings like `policy.model_allowlist`/
//! `forbidden_paths`/`daily_budget_usd` (COX-B043).

use super::*;

/// First host port for auto-allocation — shared by `heal_host_port` (below)
/// and `assign_host_port` (lib.rs), so a newly onboarded project and a
/// self-healed one never race for the same starting port.
pub(crate) const PORT_BASE: u16 = 8100;

/// A project's config as the process should actually use it, plus what the
/// mandatory post-deploy health gate should probe.
///
/// The two travel together because they can disagree: the gate probes what the
/// raw file says (so a value `Config` could not represent still fails the gate
/// instead of silently skipping it — COX-B035), EXCEPT where load itself
/// replaced the port, in which case the replacement is what the app publishes
/// and therefore what is worth probing.
pub(crate) struct LoadedConfig {
    /// Config with self-healed values applied.
    pub(crate) config: Config,
    /// The port [`coxagent_application::ports::outbound::verify_deploy_health`]
    /// should probe; `Err(())` means the config is corrupt and the gate must
    /// fail rather than pass unprobed.
    pub(crate) host_port_probe: Result<Option<u16>, ()>,
}

/// Read `coxagent.json`'s raw text from the workspace root (parent of the
/// state dir), or `None` if it's missing/unreadable. A single disk read feeds
/// both the `Config` parse and [`coxagent_application::ports::outbound::parse_deploy_host_port`],
/// which can tell "absent" apart from "malformed" where `Config::deploy.host_port`
/// alone cannot.
pub(crate) fn read_config_text(state_dir: &Path) -> Option<String> {
    let root = state_dir.parent().unwrap_or(state_dir);
    std::fs::read_to_string(root.join("coxagent.json")).ok()
}

/// Parse `coxagent.json`'s already-read text into a `Config`, healing
/// `deploy.host_port` unconditionally afterward. A single malformed top-level
/// section falls back to defaults for THAT section only, via
/// [`parse_config_lenient`] — not the whole document (COX-B043).
pub(crate) fn parse_config_text(state_dir: &Path, text: &str) -> Config {
    let root = state_dir.parent().unwrap_or(state_dir);
    let path = root.join("coxagent.json");
    let mut cfg = match serde_json::from_str::<Config>(text) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(
                "{} does not parse as a whole ({e}); recovering field-by-field \
                 instead of discarding every setting",
                path.display()
            );
            parse_config_lenient(&path, text)
        }
    };
    heal_host_port(root, &path, &mut cfg);
    cfg
}

/// Recover a `Config` from JSON that fails to deserialize as a whole,
/// isolating the damage to just the top-level section(s) that don't parse.
/// Each bad section defaults on its own (loudly, at `error` level so it's
/// not lost among routine `warn`s); every other section — critically,
/// `policy` — keeps whatever the file actually says. Only genuinely
/// unparseable JSON (not an object) falls back to `Config::default()`
/// wholesale, since there is then no document to recover fields from.
fn parse_config_lenient(path: &Path, text: &str) -> Config {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(text)
    else {
        tracing::error!(
            "{} is not a valid JSON object; using defaults for the whole file",
            path.display()
        );
        return Config::default();
    };
    let defaults = Config::default();
    Config {
        engine: field_or_default(&map, "engine", defaults.engine, path),
        git: field_or_default(&map, "git", defaults.git, path),
        workflow: field_or_default(&map, "workflow", defaults.workflow, path),
        architecture: field_or_default(&map, "architecture", defaults.architecture, path),
        policy: field_or_default(&map, "policy", defaults.policy, path),
        deploy: field_or_default(&map, "deploy", defaults.deploy, path),
    }
}

/// Deserialize one top-level `coxagent.json` field, falling back to `default`
/// (loudly) only if that field is present and malformed. A field that's
/// simply absent is not an error — `default` is what `#[serde(default)]`
/// would have produced anyway.
fn field_or_default<T: serde::de::DeserializeOwned>(
    map: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    default: T,
    path: &Path,
) -> T {
    let Some(value) = map.get(key) else {
        return default;
    };
    serde_json::from_value(value.clone()).unwrap_or_else(|e| {
        tracing::error!(
            "{} has an invalid \"{key}\" field ({e}); defaulting only that field — \
             the rest of the config (including policy) is preserved",
            path.display()
        );
        default
    })
}

/// Load `coxagent.json` from the workspace root (parent of the state dir), or
/// fall back to defaults. Config lives beside the state, written by `onboard`.
pub(crate) fn load_config(state_dir: &Path) -> Config {
    load_config_with_probe(state_dir).config
}

/// Load `coxagent.json` and derive the deploy health gate's probe port from the
/// same single read, so the two can never drift apart.
pub(crate) fn load_config_with_probe(state_dir: &Path) -> LoadedConfig {
    let root = state_dir.parent().unwrap_or(state_dir);
    let path = root.join("coxagent.json");
    let Some(text) = read_config_text(state_dir) else {
        return LoadedConfig {
            config: Config::default(),
            host_port_probe: Ok(None),
        };
    };
    let mut config = match serde_json::from_str::<Config>(&text) {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!(
                "{} does not parse as a whole ({e}); recovering field-by-field \
                 instead of discarding every setting",
                path.display()
            );
            // A document that fails to parse as a whole may carry a
            // deploy.host_port we simply can't see through Config's lens —
            // healing here would risk overwriting a value the operator meant
            // deliberately, and the raw text is a more faithful probe target
            // than a freshly-picked port would be (COX-B035/COX-B042).
            return LoadedConfig {
                config: parse_config_lenient(&path, &text),
                host_port_probe: probe_from_raw(&text),
            };
        }
    };
    let healed = heal_host_port(root, &path, &mut config);
    let host_port_probe = if healed {
        // The raw text's port is the one we just rejected or found missing;
        // the app will publish the replacement, so probe that instead.
        Ok(config.deploy.host_port)
    } else {
        probe_from_raw(&text)
    };
    LoadedConfig {
        config,
        host_port_probe,
    }
}

/// The health gate's view of the raw config text — shared with chat and the PR
/// preview so every deploy call site rejects the same values.
fn probe_from_raw(text: &str) -> Result<Option<u16>, ()> {
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
                if let Ok(text) = std::fs::read_to_string(&sib) {
                    if let Ok(c) = serde_json::from_str::<Config>(&text) {
                        if let Some(p) = c.deploy.host_port {
                            used.insert(p);
                        }
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
    use super::{
        heal_host_port, load_config_with_probe, parse_config_text, Config, LoadedConfig,
    };

    fn write_cfg(dir: &std::path::Path, json: &str) -> std::path::PathBuf {
        let path = dir.join("coxagent.json");
        std::fs::write(&path, json).expect("write coxagent.json");
        path
    }

    /// The bug this ticket fixes: `host_port: 0` deserializes to a valid
    /// `Some(0)`, so the old `is_some()` guard treated it as "already
    /// configured" and left it alone — masking as a deploy that never comes
    /// healthy. It must be healed exactly like a missing/null port.
    #[test]
    fn a_zero_host_port_is_healed_like_a_missing_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = write_cfg(dir.path(), r#"{"deploy":{"host_port":0}}"#);
        let mut cfg = Config::default();
        cfg.deploy.host_port = Some(0);

        heal_host_port(dir.path(), &cfg_path, &mut cfg);

        let healed = cfg
            .deploy
            .host_port
            .expect("heal_host_port must assign a real port");
        assert_ne!(healed, 0, "0 must never survive self-heal");
    }

    /// A validly configured, non-zero port is the user's explicit choice —
    /// self-heal must leave it alone.
    #[test]
    fn a_nonzero_host_port_is_left_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = write_cfg(dir.path(), r#"{"deploy":{"host_port":9500}}"#);
        let mut cfg = Config::default();
        cfg.deploy.host_port = Some(9500);

        heal_host_port(dir.path(), &cfg_path, &mut cfg);

        assert_eq!(cfg.deploy.host_port, Some(9500));
    }

    /// No port configured at all takes the pre-existing self-heal path.
    #[test]
    fn a_missing_host_port_is_healed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg_path = write_cfg(dir.path(), r#"{}"#);
        let mut cfg = Config::default();

        heal_host_port(dir.path(), &cfg_path, &mut cfg);

        assert!(cfg.deploy.host_port.is_some());
    }

    /// COX-B043: an out-of-range `deploy.host_port` used to fail the whole
    /// `Config` parse, falling back to `Config::default()` and silently
    /// wiping unrelated governance policy (model allowlist, forbidden paths,
    /// daily budget). Only the malformed `deploy` section should default —
    /// `policy` must survive untouched. The port itself is NOT healed here:
    /// a raw value that couldn't even be typed as a `u16` must fail the
    /// health gate loudly (see `a_negative_host_port_still_fails_the_gate_and_the_file_survives`
    /// below), not be silently replaced.
    #[test]
    fn a_malformed_deploy_section_does_not_erase_policy_governance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("mkdir state");
        let json = r#"{
            "engine": {"default": {"engine": "claude", "model": "sonnet"}, "per_role": {}, "fallbacks": []},
            "policy": {
                "model_allowlist": ["sonnet"],
                "forbidden_paths": ["infra/"],
                "daily_budget_usd": 12.5
            },
            "deploy": {"host_port": 999999}
        }"#;

        let cfg = parse_config_text(&state_dir, json);

        assert_eq!(cfg.policy.model_allowlist, vec!["sonnet".to_owned()]);
        assert_eq!(cfg.policy.forbidden_paths, vec!["infra/".to_owned()]);
        assert_eq!(cfg.policy.daily_budget_usd, Some(12.5));
    }

    /// A field that just isn't there is not malformed — leaving it out must
    /// keep behaving exactly like `#[serde(default)]` always did, not trip
    /// the "malformed" recovery path.
    #[test]
    fn an_absent_section_is_unremarkable_and_still_gets_its_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("mkdir state");
        let json = r#"{"engine": {"default": {"engine": "claude", "model": "sonnet"}, "per_role": {}, "fallbacks": []}}"#;

        let cfg = parse_config_text(&state_dir, json);

        assert_eq!(cfg.policy, Config::default().policy);
    }

    /// Text that isn't a JSON object at all (unparseable, or e.g. a bare
    /// array) has no document to recover fields from — that's the one case
    /// where whole-file defaults are the correct, unavoidable outcome.
    #[test]
    fn unparseable_json_still_falls_back_to_defaults_wholesale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&state_dir).expect("mkdir state");

        let cfg = parse_config_text(&state_dir, "not json at all");

        assert_eq!(cfg.policy, Config::default().policy);
        assert_eq!(cfg.engine, Config::default().engine);
    }

    /// Lay out a workspace the way `load_config*` expects it: an otherwise
    /// valid `coxagent.json` at the root whose `deploy.host_port` is the raw
    /// JSON token under test, state in a child dir. Returns the state dir to
    /// load from. Built from a real `Config` so nothing but the port under
    /// test can be what makes a case fail.
    fn workspace(raw_host_port: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let mut cfg = serde_json::to_value(Config::default()).expect("config as json");
        cfg["deploy"]["host_port"] =
            serde_json::from_str(raw_host_port).expect("host_port token is JSON");
        let dir = tempfile::tempdir().expect("tempdir");
        // A project dir of its own, so the sibling scan sees a realistic base.
        let root = dir.path().join("proj");
        let state = root.join(".coxagent");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::write(
            root.join("coxagent.json"),
            serde_json::to_string_pretty(&cfg).expect("config text"),
        )
        .expect("write config");
        (dir, state)
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
        } = load_config_with_probe(&state);

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
        } = load_config_with_probe(&state);

        assert_eq!(config.deploy.host_port, Some(8101));
        assert_eq!(host_port_probe, Ok(Some(8101)));
    }

    /// A value `Config` cannot represent at all must still fail the gate
    /// rather than pass unprobed (COX-B035 regression guard) — and the file
    /// must be left alone, not overwritten with defaults.
    #[test]
    fn a_negative_host_port_still_fails_the_gate_and_the_file_survives() {
        let (_dir, state) = workspace("-1");

        let LoadedConfig {
            config,
            host_port_probe,
        } = load_config_with_probe(&state);

        assert_eq!(host_port_probe, Err(()), "a corrupt port must fail the gate");
        assert_eq!(config.deploy.host_port, None, "defaults, not a healed port");
        let on_disk = std::fs::read_to_string(state.parent().expect("root").join("coxagent.json"))
            .expect("config still readable");
        assert!(
            on_disk.contains("-1"),
            "an unparseable config must not be rewritten from defaults"
        );
    }

    /// No `coxagent.json` at all is not a config error: defaults, and nothing
    /// for the gate to probe.
    #[test]
    fn a_missing_config_file_loads_defaults_with_nothing_to_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("proj").join(".coxagent");
        std::fs::create_dir_all(&state).expect("state dir");

        let loaded = load_config_with_probe(&state);

        assert_eq!(loaded.config.deploy.host_port, None);
        assert_eq!(loaded.host_port_probe, Ok(None));
    }
}
