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
    /// Webhook URL notified on significant events (deploy, budget, policy).
    /// Empty = no notifications. Slack/Teams incoming webhooks work directly.
    #[serde(default)]
    pub webhook_url: Option<String>,
}

fn default_sprint_len() -> u64 {
    10
}

/// Live, runtime-adjustable spend caps. Shared between the config API and the
/// running cycle loop so budget changes apply immediately without a restart.
/// `None` on a field means "no cap" (unlimited) for that dimension.
#[derive(Debug, Clone, Copy, Default)]
pub struct BudgetCaps {
    /// Lifetime total-spend cap in USD.
    pub lifetime_usd: Option<f64>,
    /// Per-day spend cap in USD (resets at UTC midnight).
    pub daily_usd: Option<f64>,
}

/// A [`BudgetCaps`] cell shared between the HTTP layer and the cycle loop.
pub type LiveBudget = std::sync::Arc<std::sync::Mutex<BudgetCaps>>;

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            ba_every_n_cycles: 4,
            feature_dev_enabled: true,
            sleep_seconds: 30,
            budget_usd: None,
            mode: Mode::Kanban,
            sprint_length_cycles: default_sprint_len(),
            webhook_url: None,
        }
    }
}

/// Governance policy — human gates turned into configuration (M9-10). Empty
/// fields mean "no restriction", so policy is opt-in and backward compatible.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PolicyConfig {
    /// Permitted engine models. Empty = any model allowed. A configured model
    /// outside this list stops the loop before spending a token on it.
    #[serde(default)]
    pub model_allowlist: Vec<String>,
    /// Path prefixes agents must not touch (e.g. `infra/`, `.github/`). Empty =
    /// none. Evaluated by [`crate::policy::forbidden_hits`] against a change set.
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
    /// Per-day spend cap in USD. The loop pauses once today's spend reaches it,
    /// independent of the lifetime `budget_usd` cap.
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,
}

/// Deploy configuration. `host_port` is assigned per project at onboard so two
/// projects deploying with `docker compose` on one host do not fight over the
/// same published port — agents are told which port to bind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployConfig {
    /// The host port this project's app should publish (None = agent's choice).
    #[serde(default)]
    pub host_port: Option<u16>,
}

/// Per-project version-control settings. Drives the git flow (branch + commit
/// per ticket) and the forge integration (GitHub/GitLab PR/MR). `enabled` is
/// off by default, so an existing project's repo is never touched until the
/// user opts in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // config flags, not a state machine
pub struct GitConfig {
    /// Master switch. When false, the agent loop performs no git operations.
    #[serde(default)]
    pub enabled: bool,
    /// Forge provider: `"github"` or `"gitlab"`.
    #[serde(default = "default_provider")]
    pub provider: String,
    /// API base URL for self-hosted / enterprise (empty = provider default).
    #[serde(default)]
    pub base_url: String,
    /// Repository slug `owner/name` (e.g. `stevejrrogers/CoXChat`).
    #[serde(default)]
    pub repo: String,
    /// The repository's main branch.
    #[serde(default = "default_branch")]
    pub default_branch: String,
    /// The branch the agent opens PRs/MRs into and auto-merges (empty = use
    /// `default_branch`). Set this to route agent work onto an integration
    /// branch (e.g. `develop`) while a human promotes it to `main`.
    #[serde(default)]
    pub target_branch: String,
    /// Prefix for per-ticket branches (e.g. `feat/` → `feat/CXC-123`).
    #[serde(default = "default_branch_prefix")]
    pub branch_prefix: String,
    /// Email agents commit under. Use a GitHub `…@users.noreply.github.com`
    /// address to avoid email-privacy push rejections.
    #[serde(default)]
    pub commit_email: String,
    /// Open a PR/MR automatically after pushing a ticket branch.
    #[serde(default)]
    pub auto_pr: bool,
    /// The SA agent reviews each open PR and posts its verdict as a suggestion
    /// (on by default). With `auto_merge` off, an approval is only a suggestion —
    /// the user merges; a request-changes feeds the fix-and-re-review loop.
    #[serde(default = "default_true")]
    pub auto_review: bool,
    /// Auto-merge when the SA approves and CI passes (opt-in; default off — the
    /// user merges). Implies `auto_review`.
    #[serde(default)]
    pub auto_merge: bool,
}

fn default_true() -> bool {
    true
}

fn default_provider() -> String {
    "github".to_owned()
}
fn default_branch() -> String {
    "main".to_owned()
}
fn default_branch_prefix() -> String {
    "feat/".to_owned()
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_provider(),
            base_url: String::new(),
            repo: String::new(),
            default_branch: default_branch(),
            target_branch: String::new(),
            branch_prefix: default_branch_prefix(),
            commit_email: String::new(),
            auto_pr: false,
            auto_review: true,
            auto_merge: false,
        }
    }
}

/// Top-level configuration persisted as `coxagent.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub engine: EngineMapping,
    /// Version-control / forge integration settings.
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub workflow: WorkflowConfig,
    /// Architecture conformance rules enforced against the codebase (empty = off).
    #[serde(default)]
    pub architecture: Vec<crate::conformance::StackRule>,
    /// Governance policy (model allowlist, forbidden paths, daily budget).
    #[serde(default)]
    pub policy: PolicyConfig,
    /// Deploy settings (per-project host port allocation).
    #[serde(default)]
    pub deploy: DeployConfig,
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
            git: GitConfig::default(),
            workflow: WorkflowConfig::default(),
            architecture: Vec::new(),
            policy: PolicyConfig::default(),
            deploy: DeployConfig::default(),
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
