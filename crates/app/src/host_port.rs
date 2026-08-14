// Part of the composition root split by concern — see lib.rs.
#![allow(clippy::wildcard_imports)]
//! Picking the host port a NEW project publishes, and writing it into that
//! project's `coxagent.json`.
//!
//! The load-time twin of this lives in [`super::config_load`]: `heal_host_port`
//! repairs a port an existing project cannot publish. This one chooses the
//! first port nobody else has taken, at onboarding, before the project has ever
//! run. Both answer the same question — "which port is free?" — from the same
//! evidence, so they read a sibling's port the same way: out of the raw
//! document, never through [`Config`], because a sibling whose config has one
//! unrelated bad field still publishes its port (COX-B043).

use super::*;

/// First host port for auto-allocation.
pub(crate) const PORT_BASE: u16 = 8100;

/// Write a free `deploy.host_port` into the new project's `coxagent.json`,
/// picking the lowest port from [`PORT_BASE`] not already used by a registered
/// project or published by a running container.
///
/// # Errors
///
/// The project's own `coxagent.json` exists but cannot be read or parsed, or
/// the file cannot be written. An unparseable config is refused rather than
/// replaced: writing defaults over it to set one port would erase the whole
/// governance policy the operator wrote there (COX-B043).
pub(crate) fn assign_host_port(
    base: &Path,
    registry_path: &Path,
    proj_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut used = ports_taken_by_registered_projects(registry_path);
    let _ = base; // reserved for future host-wide allocation policy
    used.extend(ports_published_by_containers());
    let port = (PORT_BASE..PORT_BASE + 500)
        .find(|p| !used.contains(p))
        .unwrap_or(PORT_BASE);

    let cfg_path = proj_dir.join("coxagent.json");
    let mut cfg = read_project_config(&cfg_path)?;
    cfg.deploy.host_port = Some(port);
    std::fs::write(&cfg_path, serde_json::to_string_pretty(&cfg)?)?;
    tracing::info!("assigned host port {port} to new project");
    Ok(())
}

/// The config we are about to add a port to. A file that is not there yet is
/// the normal case at onboarding — defaults, which this function then fills in.
/// A file that IS there and does not parse is an error, NOT defaults: the write
/// that follows would otherwise persist an empty `policy` over the operator's
/// model allowlist, forbidden paths and spend cap (COX-B043).
fn read_project_config(cfg_path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    match std::fs::read_to_string(cfg_path) {
        Ok(text) => Ok(coxagent_application::parse_config(&text)
            .map_err(|e| format!("invalid {}: {e}", cfg_path.display()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("cannot read {}: {e}", cfg_path.display()).into()),
    }
}

/// Ports already claimed by the projects in the registry.
///
/// Best-effort by design: an unreadable registry or project config must not
/// stop a new project from being onboarded. It costs nothing to be wrong here —
/// a collision surfaces at deploy — whereas refusing to onboard does.
fn ports_taken_by_registered_projects(registry_path: &Path) -> std::collections::HashSet<u16> {
    let mut used = std::collections::HashSet::new();
    let Ok(text) = std::fs::read_to_string(registry_path) else {
        return used;
    };
    let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
        return used;
    };
    for e in entries {
        let Some(p) = e.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(Path::new(p).join("coxagent.json")) else {
            continue;
        };
        // The raw document, not `Config`: a sibling with one bad field anywhere
        // in its file still publishes the port it names, and handing that port
        // to a new project is the collision this scan exists to prevent.
        if let Ok(Some(port)) = probe_from_raw(&text) {
            used.insert(port);
        }
    }
    used
}

/// Ports already published by running containers, so a new project never picks
/// one that is serving another app. Best-effort: no Docker means no exclusions.
fn ports_published_by_containers() -> std::collections::HashSet<u16> {
    let mut used = std::collections::HashSet::new();
    let Ok(out) = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Ports}}"])
        .output()
    else {
        return used;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for pair in text.split_whitespace() {
        if let Some((host, _)) = pair.split_once("->") {
            if let Some((_, hp)) = host.rsplit_once(':') {
                if let Ok(p) = hp.parse::<u16>() {
                    used.insert(p);
                }
            }
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace with a registry naming `siblings`, and an empty project dir
    /// for the new project. Returns the temp dir (kept alive), the registry
    /// path and the new project's dir.
    fn workspace(siblings: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut entries = Vec::new();
        for (id, config_text) in siblings {
            let sib = dir.path().join(id);
            std::fs::create_dir_all(&sib).expect("sibling dir");
            std::fs::write(sib.join("coxagent.json"), config_text).expect("sibling config");
            entries.push(serde_json::json!({ "id": id, "path": sib }));
        }
        let registry = dir.path().join("registry.json");
        std::fs::write(
            &registry,
            serde_json::to_string(&entries).expect("registry json"),
        )
        .expect("write registry");
        let proj = dir.path().join("new");
        std::fs::create_dir_all(&proj).expect("project dir");
        (dir, registry, proj)
    }

    /// The port that ends up in the new project's config.
    fn assigned(proj: &Path) -> u16 {
        let text = std::fs::read_to_string(proj.join("coxagent.json")).expect("config written");
        coxagent_application::parse_config(&text)
            .expect("the config we just wrote parses")
            .deploy
            .host_port
            .expect("a port must be assigned")
    }

    /// COX-B043: the sibling's port is visible in its raw document even though
    /// `Config` refuses the document as a whole. Reading it through `Config`
    /// hid the port, and the new project was handed the port the sibling was
    /// already publishing.
    #[test]
    fn a_sibling_with_one_bad_field_still_reserves_the_port_it_publishes() {
        let (_dir, registry, proj) = workspace(&[(
            "sib",
            &format!(
                r#"{{"policy":{{"daily_budget_usd":"twenty"}},"deploy":{{"host_port":{PORT_BASE}}}}}"#
            ),
        )]);

        assign_host_port(Path::new("/"), &registry, &proj).expect("onboarding assigns a port");

        assert_ne!(
            assigned(&proj),
            PORT_BASE,
            "the sibling publishes {PORT_BASE}; assigning it again collides"
        );
    }

    /// The control: a healthy sibling reserves its port the same way, so the
    /// case above is not passing for want of a registry scan.
    #[test]
    fn a_healthy_sibling_reserves_the_port_it_publishes() {
        let (_dir, registry, proj) = workspace(&[(
            "sib",
            &format!(r#"{{"deploy":{{"host_port":{PORT_BASE}}}}}"#),
        )]);

        assign_host_port(Path::new("/"), &registry, &proj).expect("onboarding assigns a port");

        assert_ne!(assigned(&proj), PORT_BASE);
    }

    /// COX-B043, the destructive half: the new project's own config was read
    /// with `unwrap_or_default()`, so one unrepresentable field made the write
    /// that follows persist an EMPTY policy over the operator's governance
    /// rules. Refuse the write and name the field instead.
    #[test]
    fn an_unparseable_project_config_is_never_overwritten_with_defaults() {
        let (_dir, registry, proj) = workspace(&[]);
        let doc = r#"{"engine":{"default":{"engine":"claude","model":"sonnet"}},
            "policy":{"model_allowlist":["claude/sonnet"]},"deploy":{"host_port":999999}}"#;
        std::fs::write(proj.join("coxagent.json"), doc).expect("write project config");

        let err = assign_host_port(Path::new("/"), &registry, &proj)
            .expect_err("999999 is outside u16")
            .to_string();

        assert!(err.contains("deploy.host_port"), "{err}");
        let on_disk = std::fs::read_to_string(proj.join("coxagent.json")).expect("file survives");
        assert_eq!(on_disk, doc, "the operator's document must be left alone");
    }

    /// A project with no config yet is the normal onboarding case: defaults,
    /// with the assigned port filled in.
    #[test]
    fn a_project_with_no_config_yet_gets_one_with_its_port() {
        let (_dir, registry, proj) = workspace(&[]);

        assign_host_port(Path::new("/"), &registry, &proj).expect("onboarding assigns a port");

        assert!(assigned(&proj) >= PORT_BASE);
    }

    /// An existing config keeps everything it declares — the assignment adds a
    /// port, it does not rewrite the project's policy.
    #[test]
    fn an_existing_config_keeps_its_policy_when_the_port_is_added() {
        let (_dir, registry, proj) = workspace(&[]);
        std::fs::write(
            proj.join("coxagent.json"),
            r#"{"engine":{"default":{"engine":"claude","model":"sonnet"}},
                "policy":{"model_allowlist":["claude/sonnet"],"forbidden_paths":["infra/"]}}"#,
        )
        .expect("write project config");

        assign_host_port(Path::new("/"), &registry, &proj).expect("onboarding assigns a port");

        let text = std::fs::read_to_string(proj.join("coxagent.json")).expect("config written");
        let cfg = coxagent_application::parse_config(&text).expect("still parses");
        assert_eq!(cfg.policy.model_allowlist, ["claude/sonnet"]);
        assert_eq!(cfg.policy.forbidden_paths, ["infra/"]);
        assert!(cfg.deploy.host_port.is_some());
    }
}
