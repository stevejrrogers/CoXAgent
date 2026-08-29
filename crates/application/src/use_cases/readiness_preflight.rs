//! CXA-F239 — project go-live readiness preflight: is this workspace ready to
//! ship its first Verified ticket?
//!
//! The core is a PURE decision over ONE snapshot of the workspace that an
//! adapter assembles (the same shape as the hexagonal gates: snapshot in,
//! verdict out — no IO here, so every rule is testable with a struct literal).
//! The snapshot carries exactly what the hub's own gates will later consult:
//! the raw `coxagent.json` text, the engine CLIs detected on this machine, the
//! docker/compose toolchain state, the publish-port probe, and the auth
//! posture. Nothing is guessed: a config that does not parse blocks its line
//! naming the offending field instead of quietly evaluating against defaults,
//! because defaults are an EMPTY policy — answering "ready" from them would be
//! the COX-B043 silent-gate-disable in preflight form.

use crate::config::{Config, EngineChoice, EngineKind, PolicyConfig};
use crate::config_parse::parse_config;
use crate::policy::model_allowed;
use coxagent_domain::Role;
use serde::Serialize;

/// One probe result the snapshot taker was able (or unable) to obtain.
/// `Unknown` is honest — "the check could not run" is never folded into a
/// pass or a fail, mirroring [`crate::ports::outbound::CrossCheck::available`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreflightProbe {
    /// The check did not run (adapter absent, tooling unprobed) — the default,
    /// so a snapshot built field-by-field starts honest rather than optimistic.
    #[default]
    Unknown,
    /// The check ran and succeeded.
    Available,
    /// The check ran and failed.
    Unavailable,
}

/// The docker CLI entry the hub's tooling probe reported at startup.
#[derive(Debug, Clone)]
pub struct DockerTooling {
    /// Whether the `docker` binary was found on PATH.
    pub present: bool,
    /// How to install it, for the detail line when it is missing.
    pub install: String,
}

/// The configured publish port and whether it is actually bindable on this
/// host right now — the collision fact `heal_host_port` acts on at load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishPortProbe {
    pub port: u16,
    /// `true` when a TCP listener could bind `127.0.0.1:port`.
    pub free: bool,
}

/// ONE snapshot of the workspace, taken by the adapter (presentation handler)
/// through ports. Every field is data; nothing here knows where it came from.
#[derive(Debug, Clone, Default)]
pub struct PreflightSnapshot {
    /// Raw `coxagent.json` text; `None` = no config file (defaults apply).
    pub config_text: Option<String>,
    /// Agent CLIs detected on this machine, as binary names.
    pub detected_engines: Vec<String>,
    /// The docker CLI entry from the tooling probe; `None` = not probed.
    pub docker: Option<DockerTooling>,
    /// Whether the deploy daemon answered / was started.
    pub daemon: PreflightProbe,
    /// Whether the compose front-end answers.
    pub compose: PreflightProbe,
    /// Whether the workspace carries a compose file; `None` = not checked.
    pub has_compose_file: Option<bool>,
    /// The configured publish port and its bindability on this host.
    pub publish_port: Option<PublishPortProbe>,
    /// Accounts configured on the hub; `None` = auth disabled (running open).
    pub auth_users: Option<usize>,
}

/// One named line item of the checklist.
#[derive(Debug, Clone, Serialize)]
pub struct PreflightItem {
    /// Stable identifier (`config`, `engine.default`, `engine.role.dev_feature`,
    /// `host_port`, `publish`, `auth`, `docker`, `compose`).
    pub id: String,
    /// Human label rendered on the dashboard.
    pub label: String,
    /// `"ok"`, `"warn"`, or `"blocked"`.
    pub status: String,
    /// The evidence: what was checked and what was found.
    pub detail: String,
    /// The offending config field, when this item is blocked by a config fault.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl PreflightItem {
    fn new(id: &str, label: &str, status: &str, detail: String) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            status: status.to_owned(),
            detail,
            field: None,
        }
    }
}

/// The preflight verdict: per-item statuses plus the overall readiness. Ready
/// means NO item is blocked — warnings are surfaced, never fatal, so the
/// "running open" posture informs without stranding the operator.
#[derive(Debug, Clone, Serialize)]
pub struct PreflightReport {
    pub ready: bool,
    pub items: Vec<PreflightItem>,
    /// Details of every blocked item — the go-live blockers, in item order.
    pub blocking: Vec<String>,
}

/// Evaluate the go-live checklist. Pure: the same snapshot always yields the
/// same report, and no default-guessing ever stands in for a config that
/// failed to parse.
#[must_use]
pub fn run_preflight(snapshot: &PreflightSnapshot) -> PreflightReport {
    let mut items = Vec::new();
    match snapshot
        .config_text
        .as_deref()
        .map_or_else(|| Ok(Config::default()), parse_config)
    {
        Ok(config) => {
            items.push(config_item(snapshot));
            items.extend(engine_items(&config, &snapshot.detected_engines));
            items.extend(host_items(config.deploy.host_port, snapshot));
            items.push(auth_item(snapshot));
            items.extend(deploy_items(config.deploy.enabled, snapshot));
        }
        Err(err) => {
            // Fail closed: name the field, refuse to evaluate anything that
            // depends on the config (engine/policy items would be lies built
            // on defaults — and defaults are an empty policy). Deploy posture
            // is fail-safe too: treated as enabled so docker/compose report
            // honestly instead of being waved through by an unverified default.
            let mut item = PreflightItem::new(
                "config",
                "coxagent.json",
                "blocked",
                format!("does not parse — {err}"),
            );
            item.field = Some(err.field.clone());
            items.push(item);
            items.push(PreflightItem::new(
                "engine",
                "Engines & models",
                "blocked",
                "cannot be evaluated — the config that declares them does not parse".to_owned(),
            ));
            items.extend(host_items(None, snapshot));
            items.push(auth_item(snapshot));
            items.extend(deploy_items(true, snapshot));
        }
    }
    let blocking: Vec<String> = items
        .iter()
        .filter(|i| i.status == "blocked")
        .map(|i| i.detail.clone())
        .collect();
    PreflightReport {
        ready: blocking.is_empty(),
        items,
        blocking,
    }
}

/// The config line item. No file at all is a designed posture (defaults), but
/// it IS an ungoverned one — an explicit warning, not a silent pass.
fn config_item(snapshot: &PreflightSnapshot) -> PreflightItem {
    match snapshot.config_text {
        None => PreflightItem::new(
            "config",
            "coxagent.json",
            "warn",
            "no config file — defaults apply: no model allowlist, no forbidden paths, no spend cap"
                .to_owned(),
        ),
        Some(_) => PreflightItem::new(
            "config",
            "coxagent.json",
            "ok",
            "parses; policy gates intact".to_owned(),
        ),
    }
}

/// `role` spelled the way config spells it (snake_case), for stable ids.
fn role_label(role: Role) -> &'static str {
    match role {
        Role::Ba => "ba",
        Role::Po => "po",
        Role::Sm => "sm",
        Role::Sa => "sa",
        Role::Pd => "pd",
        Role::DevBug => "dev_bug",
        Role::DevFeature => "dev_feature",
        Role::Test => "test",
        Role::Docs => "docs",
        Role::User => "user",
        Role::System => "system",
    }
}

/// One item per engine/model selection the runner will actually use: the
/// default mapping plus every per-role override, sorted for stable output.
fn engine_items(config: &Config, detected: &[String]) -> Vec<PreflightItem> {
    let mut choices: Vec<(String, String, &EngineChoice)> =
        vec![("engine.default".to_owned(), "default".to_owned(), &config.engine.default)];
    let mut per_role: Vec<_> = config.engine.per_role.iter().collect();
    per_role.sort_by_key(|(role, _)| role_label(**role));
    choices.extend(per_role.into_iter().map(|(role, choice)| {
        let name = role_label(*role);
        (format!("engine.role.{name}"), format!("role {name}"), choice)
    }));
    choices
        .into_iter()
        .map(|(id, label, choice)| engine_item(&id, &label, choice, &config.policy, detected))
        .collect()
}

fn engine_item(
    id: &str,
    label: &str,
    choice: &EngineChoice,
    policy: &PolicyConfig,
    detected: &[String],
) -> PreflightItem {
    let binary = choice.engine.as_binary();
    // The scripted engine is built into the binary — there is no CLI to find.
    let engine_ok = choice.engine == EngineKind::Scripted || detected.iter().any(|d| d == binary);
    // The SAME gate the cycle's model-policy check runs, per selection.
    let model_ok = model_allowed(policy, &choice.model);
    let subject = format!("Engine · {label}");
    match (engine_ok, model_ok) {
        (true, true) => {
            PreflightItem::new(id, &subject, "ok", format!("{binary} · {} ready", choice.model))
        }
        (false, true) => PreflightItem::new(
            id,
            &subject,
            "blocked",
            format!("engine '{binary}' is not installed on this machine ({label})"),
        ),
        (true, false) => PreflightItem::new(
            id,
            &subject,
            "blocked",
            format!(
                "model '{}' selected for {} is not in the policy.model_allowlist",
                choice.model, label
            ),
        ),
        (false, false) => PreflightItem::new(
            id,
            &subject,
            "blocked",
            format!(
                "engine '{binary}' is not installed and model '{}' is not in the \
                 policy.model_allowlist ({label})",
                choice.model
            ),
        ),
    }
}

/// Port assignment (`host_port`) and publishability (`publish`) as two line
/// items: the first is what the config SAYS, the second is what the HOST
/// allows right now — a collision is invisible in the config alone.
/// `config_port` is `None` when the config did not parse (the config line
/// carries that fault; the port facts below stay as informative as possible).
fn host_items(config_port: Option<u16>, snapshot: &PreflightSnapshot) -> Vec<PreflightItem> {
    let mut items = Vec::new();
    match config_port {
        Some(0) => {
            // Not publishable; load heals it, but the config line is still wrong.
            items.push(PreflightItem::new(
                "host_port",
                "Deploy host_port",
                "blocked",
                "port 0 is not a publishable TCP port — set a real port in coxagent.json"
                    .to_owned(),
            ));
            items.push(PreflightItem::new(
                "publish",
                "Publish port",
                "blocked",
                "port 0 can never be published — clients could never connect".to_owned(),
            ));
        }
        Some(port) => {
            items.push(PreflightItem::new(
                "host_port",
                "Deploy host_port",
                "ok",
                format!("assigned to {port}"),
            ));
            items.push(publish_item(snapshot, port));
        }
        None => {
            items.push(PreflightItem::new(
                "host_port",
                "Deploy host_port",
                "warn",
                "not assigned — a free port is chosen and persisted at load".to_owned(),
            ));
            items.push(match snapshot.publish_port {
                Some(probe) if !probe.free => PreflightItem::new(
                    "publish",
                    "Publish port",
                    "blocked",
                    format!(
                        "{} is already in use on this host — docker compose up would fail with \
                         \"port is already allocated\"",
                        probe.port
                    ),
                ),
                Some(probe) => PreflightItem::new(
                    "publish",
                    "Publish port",
                    "warn",
                    format!("{} is free, but no parseable config assigns it yet", probe.port),
                ),
                None => PreflightItem::new(
                    "publish",
                    "Publish port",
                    "warn",
                    "nothing configured to publish yet".to_owned(),
                ),
            });
        }
    }
    items
}

fn publish_item(snapshot: &PreflightSnapshot, port: u16) -> PreflightItem {
    match snapshot.publish_port {
        Some(probe) if probe.port == port => {
            if probe.free {
                PreflightItem::new(
                    "publish",
                    "Publish port",
                    "ok",
                    format!("{port} is free to publish on this host"),
                )
            } else {
                PreflightItem::new(
                    "publish",
                    "Publish port",
                    "blocked",
                    format!(
                        "{port} is already in use on this host — docker compose up would fail \
                         with \"port is already allocated\""
                    ),
                )
            }
        }
        // The raw-text probe and the parsed config can disagree only while the
        // config is broken; the config line above carries that fault.
        _ => PreflightItem::new(
            "publish",
            "Publish port",
            "warn",
            format!("{port} assigned, but availability on this host was not probed"),
        ),
    }
}

/// The security posture: an explicit line item either way, so "running open"
/// is a decision someone sees, not a silent default (warn, not block).
fn auth_item(snapshot: &PreflightSnapshot) -> PreflightItem {
    match snapshot.auth_users {
        Some(n) if n > 0 => PreflightItem::new(
            "auth",
            "Auth & accounts",
            "ok",
            format!("RBAC enabled — {n} account(s) provisioned"),
        ),
        Some(_) | None => PreflightItem::new(
            "auth",
            "Auth & accounts",
            "warn",
            "running open — no accounts are configured, so anyone who can reach this hub \
             controls every project (set COXAGENT_ADMIN_USER / COXAGENT_ADMIN_PASSWORD)"
                .to_owned(),
        ),
    }
}

/// Docker + compose deployability. A project that disabled deploy, or that
/// ships no compose file (the adapter skips those deploys by design), does not
/// block on a missing toolchain — the check must reflect what deploys will
/// actually attempt.
fn deploy_items(deploy_enabled: bool, snapshot: &PreflightSnapshot) -> Vec<PreflightItem> {
    if !deploy_enabled {
        return vec![
            PreflightItem::new(
                "docker",
                "Docker",
                "ok",
                "deploy disabled in coxagent.json — docker not required".to_owned(),
            ),
            PreflightItem::new(
                "compose",
                "Docker compose",
                "ok",
                "deploy disabled in coxagent.json — compose not required".to_owned(),
            ),
        ];
    }
    vec![docker_item(snapshot), compose_item(snapshot)]
}

fn docker_item(snapshot: &PreflightSnapshot) -> PreflightItem {
    match &snapshot.docker {
        None => PreflightItem::new(
            "docker",
            "Docker",
            "warn",
            "docker tooling was not probed at hub startup".to_owned(),
        ),
        Some(tooling) if !tooling.present => PreflightItem::new(
            "docker",
            "Docker",
            "blocked",
            format!("docker CLI is not installed — {}", tooling.install),
        ),
        Some(_) => match snapshot.daemon {
            PreflightProbe::Available => PreflightItem::new(
                "docker",
                "Docker",
                "ok",
                "docker CLI present, daemon up".to_owned(),
            ),
            PreflightProbe::Unavailable => PreflightItem::new(
                "docker",
                "Docker",
                "blocked",
                "docker CLI present but the daemon did not come up — start Docker and retry"
                    .to_owned(),
            ),
            PreflightProbe::Unknown => PreflightItem::new(
                "docker",
                "Docker",
                "warn",
                "docker CLI present, daemon state not probed".to_owned(),
            ),
        },
    }
}

fn compose_item(snapshot: &PreflightSnapshot) -> PreflightItem {
    if snapshot.has_compose_file == Some(false) {
        return PreflightItem::new(
            "compose",
            "Docker compose",
            "ok",
            "no compose file in the workspace — deploys skip (nothing for compose to run)"
                .to_owned(),
        );
    }
    let docker_missing = matches!(&snapshot.docker, Some(t) if !t.present);
    match snapshot.compose {
        PreflightProbe::Available => PreflightItem::new(
            "compose",
            "Docker compose",
            "ok",
            "docker compose answers (CLI + plugin present)".to_owned(),
        ),
        PreflightProbe::Unavailable => {
            let why = if docker_missing {
                "docker CLI is missing — compose cannot run"
            } else {
                "`docker compose version` failed — install the compose plugin"
            };
            PreflightItem::new("compose", "Docker compose", "blocked", why.to_owned())
        }
        PreflightProbe::Unknown => PreflightItem::new(
            "compose",
            "Docker compose",
            "warn",
            "compose availability was not probed".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot with nothing configured: defaults, no engines detected, no
    /// docker, open auth. The worst case a fresh project can present.
    fn bare() -> PreflightSnapshot {
        PreflightSnapshot::default()
    }

    fn ok_config_text() -> String {
        r#"{
          "engine": { "default": { "engine": "scripted", "model": "offline" } },
          "policy": { "model_allowlist": ["offline"], "forbidden_paths": [], "daily_budget_usd": 10.0 },
          "deploy": { "host_port": 8101 }
        }"#
        .to_owned()
    }

    /// AC2: a config that fails to parse is BLOCKED on its config line naming
    /// the offending field — and nothing downstream is evaluated from defaults.
    #[test]
    fn an_unparseable_config_blocks_its_line_naming_the_field_and_guesses_nothing() {
        let mut s = bare();
        s.config_text = Some(
            r#"{"engine":{"default":{"engine":"claude","model":"sonnet"}},
                "policy":{"daily_budget_usd":"twenty"},"deploy":{"host_port":8101}}"#
                .to_owned(),
        );

        let report = run_preflight(&s);

        let config = report.items.iter().find(|i| i.id == "config").expect("config line");
        assert_eq!(config.status, "blocked");
        assert_eq!(config.field.as_deref(), Some("policy.daily_budget_usd"));
        assert!(config.detail.contains("policy.daily_budget_usd"), "{}", config.detail);
        // No default-guessing: the engine line refuses to evaluate, and the
        // report is not ready.
        let engine = report.items.iter().find(|i| i.id == "engine").expect("engine line");
        assert_eq!(engine.status, "blocked");
        assert!(!report.ready);
        assert!(!report.blocking.is_empty());
    }

    /// AC3: no account configured → an explicit "running open" warning line,
    /// not a silent pass — and not a blocker either.
    #[test]
    fn an_open_hub_reports_its_security_posture_as_a_warning_line() {
        let mut s = bare();
        s.config_text = Some(ok_config_text());

        let report = run_preflight(&s);

        let auth = report.items.iter().find(|i| i.id == "auth").expect("auth line");
        assert_eq!(auth.status, "warn");
        assert!(auth.detail.contains("running open"), "{}", auth.detail);
        assert!(report.ready, "a warning informs; it does not block: {:?}", report.blocking);
    }

    /// AC4: a model selected for a role that the allowlist forbids flags that
    /// role+model as blocked, naming both.
    #[test]
    fn a_role_model_outside_the_allowlist_is_blocked_naming_role_and_model() {
        let mut s = bare();
        s.config_text = Some(
            r#"{
              "engine": {
                "default": { "engine": "scripted", "model": "offline" },
                "per_role": { "dev_feature": { "engine": "claude", "model": "gpt-4" } }
              },
              "policy": { "model_allowlist": ["offline"], "forbidden_paths": [], "daily_budget_usd": 10.0 },
              "deploy": { "host_port": 8101 }
            }"#
            .to_owned(),
        );
        s.detected_engines = vec!["claude".to_owned(), "scripted".to_owned()];

        let report = run_preflight(&s);

        let item = report
            .items
            .iter()
            .find(|i| i.id == "engine.role.dev_feature")
            .expect("per-role engine line");
        assert_eq!(item.status, "blocked");
        assert!(item.detail.contains("dev_feature"), "{}", item.detail);
        assert!(item.detail.contains("gpt-4"), "{}", item.detail);
        assert!(!report.ready);
    }

    /// A healthy workspace reads ready: every hard gate ok, the provisioned
    /// auth posture confirmed.
    #[test]
    fn a_healthy_workspace_is_ready_with_every_hard_gate_ok() {
        let mut s = bare();
        s.config_text = Some(ok_config_text());
        s.detected_engines = vec!["scripted".to_owned()];
        s.docker = Some(DockerTooling {
            present: true,
            install: String::new(),
        });
        s.daemon = PreflightProbe::Available;
        s.compose = PreflightProbe::Available;
        s.has_compose_file = Some(true);
        s.publish_port = Some(PublishPortProbe {
            port: 8101,
            free: true,
        });
        s.auth_users = Some(1);

        let report = run_preflight(&s);

        assert!(report.ready, "blockers: {:?}", report.blocking);
        for id in ["config", "engine.default", "host_port", "publish", "docker", "compose"] {
            let item = report.items.iter().find(|i| i.id == id).expect(id);
            assert_eq!(item.status, "ok", "{id}: {}", item.detail);
        }
    }

    /// An occupied publish port is the collision the preflight exists to catch
    /// before a docker compose up fails on it.
    #[test]
    fn an_occupied_publish_port_blocks_go_live() {
        let mut s = bare();
        s.config_text = Some(ok_config_text());
        s.detected_engines = vec!["scripted".to_owned()];
        s.publish_port = Some(PublishPortProbe {
            port: 8101,
            free: false,
        });

        let report = run_preflight(&s);

        let publish = report.items.iter().find(|i| i.id == "publish").expect("publish line");
        assert_eq!(publish.status, "blocked");
        assert!(publish.detail.contains("8101"), "{}", publish.detail);
        assert!(!report.ready);
    }

    /// The same collision, seen while the config is broken: the port fact is
    /// still reported (fail-safe deploy posture), even though the config line
    /// carries the parse fault.
    #[test]
    fn an_occupied_publish_port_is_reported_even_when_the_config_does_not_parse() {
        let mut s = bare();
        s.config_text = Some("{ not json at all".to_owned());
        s.publish_port = Some(PublishPortProbe {
            port: 8101,
            free: false,
        });

        let report = run_preflight(&s);

        let publish = report.items.iter().find(|i| i.id == "publish").expect("publish line");
        assert_eq!(publish.status, "blocked");
        let config = report.items.iter().find(|i| i.id == "config").expect("config line");
        assert_eq!(config.status, "blocked");
        assert!(!report.ready);
    }

    /// The raw-text probe and the parsed config can only disagree while the
    /// config is broken; when they do, the publish line must not claim a
    /// verdict it cannot back.
    #[test]
    fn a_publish_probe_for_a_different_port_is_reported_as_unprobed() {
        let mut s = bare();
        s.config_text = Some(ok_config_text()); // assigns 8101
        s.publish_port = Some(PublishPortProbe {
            port: 9000,
            free: true,
        });

        let report = run_preflight(&s);

        let publish = report.items.iter().find(|i| i.id == "publish").expect("publish line");
        assert_eq!(publish.status, "warn");
        assert!(publish.detail.contains("was not probed"), "{}", publish.detail);
        assert!(report.ready, "blockers: {:?}", report.blocking);
    }

    /// Deploy disabled by config is a decision, not a fault: docker/compose
    /// report ok even though the toolchain is absent.
    #[test]
    fn a_project_that_disabled_deploy_does_not_block_on_a_missing_docker() {
        let mut s = bare();
        s.config_text = Some(
            r#"{"engine":{"default":{"engine":"scripted","model":"offline"}},"deploy":{"enabled":false}}"#
                .to_owned(),
        );

        let report = run_preflight(&s);

        for id in ["docker", "compose"] {
            let item = report.items.iter().find(|i| i.id == id).expect(id);
            assert_eq!(item.status, "ok", "{id}: {}", item.detail);
        }
        assert!(report.ready, "blockers: {:?}", report.blocking);
    }

    /// A workspace with no coxagent.json at all: warn on the config line (the
    /// ungoverned posture is visible), engines evaluated against the defaults
    /// that would actually run.
    #[test]
    fn a_missing_config_file_warns_and_evaluates_the_defaults_it_will_run() {
        let mut s = bare();
        s.detected_engines = vec!["claude".to_owned()];

        let report = run_preflight(&s);

        let config = report.items.iter().find(|i| i.id == "config").expect("config line");
        assert_eq!(config.status, "warn");
        let engine = report
            .items
            .iter()
            .find(|i| i.id == "engine.default")
            .expect("default engine line");
        assert_eq!(engine.status, "ok", "{}", engine.detail);
        assert!(engine.detail.contains("sonnet"), "{}", engine.detail);
        assert!(report.ready, "blockers: {:?}", report.blocking);
    }

    /// A missing engine binary blocks its line, naming the engine — the exact
    /// one-failed-gate-at-a-time discovery this preflight replaces.
    #[test]
    fn a_missing_engine_binary_blocks_its_line_naming_the_engine() {
        let mut s = bare();
        s.config_text = Some(
            r#"{
              "engine": { "default": { "engine": "claude", "model": "sonnet" } },
              "policy": { "model_allowlist": ["sonnet"], "forbidden_paths": [], "daily_budget_usd": 10.0 },
              "deploy": { "host_port": 8101 }
            }"#
            .to_owned(),
        );
        s.detected_engines = Vec::new();

        let report = run_preflight(&s);

        let engine = report
            .items
            .iter()
            .find(|i| i.id == "engine.default")
            .expect("default engine line");
        assert_eq!(engine.status, "blocked");
        assert!(engine.detail.contains("claude"), "{}", engine.detail);
        assert!(!report.ready);
    }

    /// The scripted engine is built into the binary: it needs no CLI on PATH.
    #[test]
    fn the_scripted_engine_needs_no_detected_cli() {
        let mut s = bare();
        s.config_text = Some(ok_config_text());

        let report = run_preflight(&s);

        let engine = report
            .items
            .iter()
            .find(|i| i.id == "engine.default")
            .expect("default engine line");
        assert_eq!(engine.status, "ok", "{}", engine.detail);
    }
}
