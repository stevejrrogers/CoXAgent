//! Configuration types — the engine-per-role mapping with three-level
//! precedence (worker override > project default > global default).
//!
//! Pure data; loading from `coxagent.json` is an adapter concern.

use coxagent_domain::Role;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Known agent engine CLIs. `as_binary` gives the executable name to look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Opencode,
    Claude,
    Hermes,
    Gemini,
    Codex,
    /// Deterministic offline engine for demos/tests (writes real code, no LLM).
    Scripted,
}

impl EngineKind {
    /// The executable name expected on `PATH`.
    #[must_use]
    pub fn as_binary(self) -> &'static str {
        match self {
            EngineKind::Opencode => "opencode",
            EngineKind::Claude => "claude",
            EngineKind::Hermes => "hermes",
            EngineKind::Gemini => "gemini",
            EngineKind::Codex => "codex",
            EngineKind::Scripted => "scripted",
        }
    }

    /// All known engines, for discovery.
    #[must_use]
    pub fn all() -> &'static [EngineKind] {
        &[
            EngineKind::Opencode,
            EngineKind::Claude,
            EngineKind::Hermes,
            EngineKind::Gemini,
            EngineKind::Codex,
        ]
    }
}

/// A concrete engine + model selection (model is the full `provider/model`
/// string passed to the engine, e.g. `bizbrain/DeepSeek-V4-Pro`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineChoice {
    pub engine: EngineKind,
    pub model: String,
}

/// Engine mapping: a default plus optional per-role overrides. This is where
/// cost is tuned — expensive models for DEV/SA, cheap ones for BA/DOCS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineMapping {
    pub default: EngineChoice,
    #[serde(default)]
    pub per_role: HashMap<Role, EngineChoice>,
}

impl EngineMapping {
    /// Resolve the effective choice for a role (override, else default).
    #[must_use]
    pub fn resolve(&self, role: Role) -> &EngineChoice {
        self.per_role.get(&role).unwrap_or(&self.default)
    }
}

/// Delivery mode: continuous flow, or fixed sprint windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Kanban,
    Scrum,
}

/// Loop tuning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowConfig {
    /// BA runs when `cycle % ba_every == 1` (0 disables BA).
    pub ba_every_n_cycles: u64,
    /// Whether the DEV-FEATURE agent runs (false = single-dev mode).
    pub feature_dev_enabled: bool,
    /// Seconds to sleep between cycles.
    pub sleep_seconds: u64,
    /// Optional spend cap in USD; the loop pauses when total spend reaches it.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Delivery mode (kanban = continuous, scrum = sprint windows).
    #[serde(default)]
    pub mode: Mode,
    /// Cycles per sprint in scrum mode.
    #[serde(default = "default_sprint_len")]
    pub sprint_length_cycles: u64,
}

fn default_sprint_len() -> u64 {
    10
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            ba_every_n_cycles: 4,
            feature_dev_enabled: true,
            sleep_seconds: 30,
            budget_usd: None,
            mode: Mode::Kanban,
            sprint_length_cycles: default_sprint_len(),
        }
    }
}

/// Top-level configuration persisted as `coxagent.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub engine: EngineMapping,
    #[serde(default)]
    pub workflow: WorkflowConfig,
    /// Architecture conformance rules enforced against the codebase (empty = off).
    #[serde(default)]
    pub architecture: Vec<crate::conformance::StackRule>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            engine: EngineMapping {
                default: EngineChoice {
                    engine: EngineKind::Claude,
                    model: "sonnet".to_owned(),
                },
                per_role: HashMap::new(),
            },
            workflow: WorkflowConfig::default(),
            architecture: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_per_role_override() {
        let mut m = Config::default().engine;
        m.per_role.insert(
            Role::Docs,
            EngineChoice {
                engine: EngineKind::Claude,
                model: "anthropic/haiku".to_owned(),
            },
        );
        assert_eq!(m.resolve(Role::Docs).model, "anthropic/haiku");
        assert_eq!(m.resolve(Role::DevFeature).model, "sonnet");
    }

    #[test]
    fn config_round_trips_through_json() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: Config = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, back);
    }
}
