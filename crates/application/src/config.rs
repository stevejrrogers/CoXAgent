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

impl EngineChoice {
    /// Sanity-check the model string against a simple allowlist pattern to
    /// prevent accidental CLI argument injection through the config file.
    pub fn is_model_valid(&self) -> bool {
        self.model.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '/' || c == '-' || c == ':'
        }) && !self.model.is_empty()
    }
}

/// Engine mapping: a default plus optional per-role overrides. This is where
/// cost is tuned — expensive models for DEV/SA, cheap ones for BA/DOCS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineMapping {
    pub default: EngineChoice,
    #[serde(default)]
    pub per_role: HashMap<Role, EngineChoice>,
    /// Engines to fall back to, in order, when the primary hits a quota / auth /
    /// rate-limit wall — so a run keeps going on another CLI that still has
    /// budget instead of failing until the quota resets.
    #[serde(default)]
    pub fallbacks: Vec<EngineChoice>,
    /// Auto-failover (default on): append every agent CLI detected on the host —
    /// plus a cheaper same-CLI tier — to the fallback chain automatically, so the
    /// user only toggles it on/off instead of listing models by hand. Explicit
    /// `fallbacks` still take priority (tried first).
    #[serde(default = "default_true")]
    pub auto_fallback: bool,
    /// Escalation ladder: models to try on RETRIES of a failed ticket —
    /// attempt 2 uses `escalation[0]`, attempt 3 uses `escalation[1]`, and so
    /// on (the last entry repeats). Empty = each engine's built-in ladder
    /// (claude → opus; opencode → its configured custom providers first,
    /// then built-in providers).
    #[serde(default)]
    pub escalation: Vec<String>,
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

/// The language the team's Scrum ceremonies and feed posts speak. Code, tickets
/// and technical review stay in English regardless — this only sets the tone of
/// the human-facing standup / planning / grooming / retro conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    En,
    Vi,
}

impl Language {
    /// The directive appended to ceremony prompts so agents answer in this
    /// language. Empty for English (the base prompts are already English).
    #[must_use]
    pub fn reply_directive(self) -> &'static str {
        match self {
            Language::En => "",
            Language::Vi => {
                " Viết toàn bộ phản hồi bằng tiếng Việt tự nhiên (giữ nguyên các nhãn kỹ thuật \
                 như \"BLOCKER:\" và mã ticket)."
            }
        }
    }

    /// Whether this is Vietnamese — for picking the localized deterministic posts.
    #[must_use]
    pub fn is_vi(self) -> bool {
        matches!(self, Language::Vi)
    }
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
    /// Number of parallel worker runners (dev/test/docs phases). Leader phases
    /// (BA/PO/standup) still singleton. Default 1.
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    /// Optional spend cap in USD; the loop pauses when total spend reaches it.
    #[serde(default)]
    pub budget_usd: Option<f64>,
    /// Delivery mode (kanban = continuous, scrum = sprint windows).
    #[serde(default)]
    pub mode: Mode,
    /// Cycles per sprint in scrum mode.
    #[serde(default = "default_sprint_len")]
    pub sprint_length_cycles: u64,
    /// Ops/SRE monitor (default on): after a deploy, the leader pings the app on
    /// its published port each cycle and files a high-priority bug + alerts the
    /// chat if it went down — so the team also runs what it ships.
    #[serde(default = "default_true")]
    pub ops_monitor: bool,
    /// Webhook URL notified on significant events (deploy, budget, policy).
    /// Empty = no notifications. Slack/Teams incoming webhooks work directly.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Compress large prompt embeds (diffs, logs) and ask agents for terse
    /// output to cut token spend. On by default; disable for maximum verbosity.
    #[serde(default = "default_true")]
    pub token_saver: bool,
    /// Language the Scrum ceremonies and team feed speak (English or Vietnamese).
    #[serde(default)]
    pub language: Language,
    /// Cost approval gate: when the estimated cost of a DEV run on a ticket
    /// (rolling average per role) exceeds this many USD, the ticket is HELD
    /// until a human approves it from the dashboard. `None` = no gate.
    #[serde(default)]
    pub approve_over_usd: Option<f64>,
    /// TDD gate (default on): before DEV implements a feature that has
    /// acceptance criteria, a TEST-role call writes FAILING tests from those
    /// criteria first — "done" becomes machine-checkable before any code.
    #[serde(default = "default_true")]
    pub tdd: bool,
    /// Sandbox agent CLIs: confine their file WRITES to the project workspace
    /// + tool caches (macOS Seatbelt today; other platforms run unsandboxed
    /// with a warning). Off by default — turn on for untrusted codebases.
    #[serde(default)]
    pub sandbox: bool,
}

fn default_max_open_prs() -> u32 {
    4
}

fn default_sprint_len() -> u64 {
    10
}

fn default_concurrency() -> u32 {
    1
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
            ops_monitor: true,
            sleep_seconds: 30,
            concurrency: 1,
            budget_usd: None,
            mode: Mode::Kanban,
            sprint_length_cycles: default_sprint_len(),
            webhook_url: None,
            token_saver: true,
            language: Language::En,
            approve_over_usd: None,
            tdd: true,
            sandbox: false,
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
    /// Whether the cycle deploys at all (default on). Turn OFF for projects
    /// whose compose stack would collide with live infrastructure — e.g.
    /// CoXAgent developing itself, where the compose file binds the very port
    /// the live hub serves.
    #[serde(default = "default_true")]
    pub enabled: bool,
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
    /// WIP limit on open PRs into the target branch: at/above this, DEV stops
    /// starting NEW features and the team drains the review queue instead —
    /// the brake that prevents cascade merge conflicts. 0 = unlimited.
    #[serde(default = "default_max_open_prs")]
    pub max_open_prs: u32,
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
            max_open_prs: default_max_open_prs(),
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
                fallbacks: Vec::new(),
                auto_fallback: true,
                escalation: Vec::new(),
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
