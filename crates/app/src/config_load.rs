// Split out of builders.rs (COX-B042) — config loading and the
// `deploy.host_port` self-heal, kept together since `heal_host_port` is
// invoked from inside the parse step and nowhere else.
use coxagent_application::config::Config;
use std::path::Path;

/// First host port for auto-allocation — shared by `heal_host_port` (below)
/// and `assign_host_port` (lib.rs), so a newly onboarded project and a
/// self-healed one never race for the same starting port.
pub(crate) const PORT_BASE: u16 = 8100;

/// Read `coxagent.json`'s raw text from the workspace root (parent of the
/// state dir), or `None` if it's missing/unreadable. Split out of
/// `load_config` (COX-B035) so a caller that also needs `deploy.host_port`
/// parsed independently of `Config` — via
/// `coxagent_application::ports::outbound::parse_deploy_host_port`, which can
/// tell "absent" apart from "malformed" where `Config::deploy.host_port`
/// alone cannot — reads the file ONCE and feeds the same string to both
/// parses, rather than reading it twice. See `build_project` and `run_loop`.
pub(crate) fn read_config_text(state_dir: &Path) -> Option<String> {
    let root = state_dir.parent().unwrap_or(state_dir);
    std::fs::read_to_string(root.join("coxagent.json")).ok()
}

/// Parse `coxagent.json`'s already-read text into a `Config`. A single
/// malformed top-level section (e.g. an out-of-range `deploy.host_port`)
/// falls back to defaults for THAT section only, via [`parse_config_lenient`]
/// — not the whole document, which would silently wipe unrelated settings
/// like `policy.model_allowlist`/`forbidden_paths`/`daily_budget_usd`
/// (COX-B043). Split from the old `load_config` so `read_config_text`'s
/// single disk read can feed both this and `parse_deploy_host_port`.
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

pub(crate) fn load_config(state_dir: &Path) -> Config {
    read_config_text(state_dir)
        .map_or_else(Config::default, |text| parse_config_text(state_dir, &text))
}

/// Self-heal a project left without a valid deploy port: assign a free
/// `host_port` and persist it, so a project onboarded before per-project
/// ports (or with the field cleared, or set to `0` — COX-B042) stops
/// colliding on the shared default port or silently failing every deploy's
/// health gate forever. Picks the lowest port in range that no sibling
/// project claims and that is currently bindable, so two unhealed projects
/// on one host land on different ports. Best-effort.
pub(crate) fn heal_host_port(root: &Path, cfg_path: &Path, cfg: &mut Config) {
    match cfg.deploy.host_port {
        // `0` deserializes fine as a `u16` — it is not a type error `serde`
        // can catch — but it is not a connectable TCP port either. Left
        // unhealed, it flows straight into `verify_deploy_health`/
        // `wait_healthy` (`Some(0)` is not `None`, so the mandatory health
        // gate does not skip it) and every deploy fails forever with "app
        // never binds its port" — indistinguishable from a real app bug.
        Some(0) => tracing::warn!(
            "{} has deploy.host_port 0, which is not a valid TCP port — \
             self-healing to a free port instead of letting every deploy's \
             health check fail forever",
            cfg_path.display()
        ),
        Some(_) => return,
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
        return;
    };
    cfg.deploy.host_port = Some(port);
    if let Ok(text) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(cfg_path, text);
        tracing::info!("self-healed host port {port} for {}", cfg_path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::{heal_host_port, parse_config_text, Config};

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
    /// `policy` must survive untouched.
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
        assert!(
            cfg.deploy.host_port.is_some(),
            "the out-of-range value must not survive — the malformed section defaults \
             (and then self-heals to a real port), while policy stays untouched"
        );
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
}
