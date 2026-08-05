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
    #[must_use]
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
#[allow(clippy::struct_excessive_bools)] // config flags, not a state machine
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
    /// and tool caches (macOS Seatbelt today; other platforms run unsandboxed
    /// with a warning). Off by default — turn on for untrusted codebases.
    #[serde(default)]
    pub sandbox: bool,
    /// Hybrid-team knobs: which lifecycle moves wait for a person, and where
    /// exception work routes. All off by default — an unstaffed project
    /// behaves exactly like the fully autonomous mode.
    #[serde(default)]
    pub human: HumanConfig,
    /// See [`WorkflowConfig::backlog_cap`]; 0 = default (40).
    #[serde(default)]
    pub backlog_cap: usize,
    /// Days a PR may sit with an unchanged head after request-changes before
    /// forge hygiene force-rescues it once and then closes it (the ticket is
    /// linked back so no work is lost). 0 = default (2).
    #[serde(default)]
    pub pr_stale_days: u64,
}

/// Human-in-the-loop configuration (see docs/HYBRID_TEAM.md).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HumanConfig {
    /// `Pending → Ready` waits for a person (the PO gate): agents may draft
    /// and design, but only a human approval releases work to DEV.
    pub gate_ready: bool,
    /// `Fixed → Verified` waits for a person (the QA gate): the TEST agent
    /// still attaches evidence, but a human renders the verdict.
    pub gate_verify: bool,
    /// Route exception tickets to a person automatically: `large` complexity
    /// at design time, and tickets parked after repeated agent failures.
    pub route_exceptions_to: Option<String>,
    /// Minutes a question @mentioning a person may wait before it escalates
    /// to the SM channel and the impediment digest. 0 = never escalate.
    pub question_sla_minutes: u64,
    /// Adaptive approval: routine work proceeds with an undo window instead of
    /// waiting for a rubber stamp (docs/ADAPTIVE_APPROVAL.md).
    pub adaptive: AdaptiveConfig,
}

/// See docs/ADAPTIVE_APPROVAL.md. Off by default: a project starts with the
/// fixed gates and opts into the moving one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdaptiveConfig {
    pub enabled: bool,
    /// Minutes an auto-approved ticket can be pulled back. 0 = no auto lane.
    pub undo_window_minutes: u64,
    /// Consistent human decisions before a shape changes lane. 0 = default 8.
    pub learn_after_samples: usize,
    /// Blast-radius cap: auto-approvals per cycle. 0 = default 3.
    pub max_auto_per_cycle: usize,
}

impl AdaptiveConfig {
    #[must_use]
    pub fn undo_window_minutes(&self) -> u64 {
        if self.undo_window_minutes == 0 {
            30
        } else {
            self.undo_window_minutes
        }
    }
    #[must_use]
    pub fn learn_after_samples(&self) -> usize {
        if self.learn_after_samples == 0 {
            8
        } else {
            self.learn_after_samples
        }
    }
    #[must_use]
    pub fn max_auto_per_cycle(&self) -> usize {
        if self.max_auto_per_cycle == 0 {
            3
        } else {
            self.max_auto_per_cycle
        }
    }
}

impl WorkflowConfig {
    /// Pending feature/chore count past which the BA stops proposing —
    /// backlog inflation drowns the board long before DEV runs dry.
    #[must_use]
    pub fn backlog_cap(&self) -> usize {
        if self.backlog_cap == 0 {
            40
        } else {
            self.backlog_cap
        }
    }

    /// See the `pr_stale_days` field; 0 = default (2 days).
    #[must_use]
    pub fn pr_stale_days(&self) -> u64 {
        if self.pr_stale_days == 0 {
            2
        } else {
            self.pr_stale_days
        }
    }
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
            human: HumanConfig::default(),
            backlog_cap: 0,
            pr_stale_days: 0,
        }
    }
}

/// Governance policy — human gates turned into configuration (M9-10). Empty
/// fields mean "no restriction", so policy is opt-in and backward compatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Fraction of whichever cap applies (lifetime `budget_usd` or
    /// `daily_budget_usd`) at which an early `budget_warning` notification
    /// fires, before the hard stop at 100%. Matches the dashboard's existing
    /// amber threshold for space budgets, so the UX language stays consistent.
    #[serde(default = "default_budget_warn_pct")]
    pub budget_warn_pct: f64,
}

fn default_budget_warn_pct() -> f64 {
    0.8
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            model_allowlist: Vec::new(),
            forbidden_paths: Vec::new(),
            daily_budget_usd: None,
            budget_warn_pct: default_budget_warn_pct(),
        }
    }
}

/// Deploy configuration. `host_port` is assigned per project at onboard so two
/// projects deploying with `docker compose` on one host do not fight over the
/// same published port — agents are told which port to bind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Auto-redeploy the last known-good version when `deploy()` or a
    /// post-deploy `run_tests()` fails, so the shared environment self-heals
    /// instead of staying broken until a DEV agent picks up the bug ticket.
    /// Opt-in (default off) — an existing project's behavior never changes
    /// until an operator turns this on.
    #[serde(default)]
    pub auto_rollback: bool,
    /// A known-good deploy older than this is considered too stale to roll
    /// back to (the environment may have drifted too far) — rollback is
    /// skipped, not attempted, and the failure just files its bug as before.
    #[serde(default = "default_max_rollback_age_secs")]
    pub max_rollback_age_secs: u64,
    /// Repo-relative path prefixes that mark a database migration. If any
    /// file under one of these changed since the known-good deploy, rollback
    /// is skipped — the app would run against a DB schema ahead of it.
    #[serde(default = "default_migration_detection_paths")]
    pub migration_detection_paths: Vec<String>,
    /// How long the mandatory post-deploy health check (COX-F005) waits for
    /// the app's health endpoint to answer before the deploy is marked
    /// failed and auto-rollback is triggered.
    #[serde(default = "default_health_check_timeout_secs")]
    pub health_check_timeout_secs: u64,
}

fn default_max_rollback_age_secs() -> u64 {
    3600
}

fn default_health_check_timeout_secs() -> u64 {
    60
}

fn default_migration_detection_paths() -> Vec<String> {
    vec!["migrations".to_owned()]
}

impl Default for DeployConfig {
    fn default() -> Self {
        Self {
            host_port: None,
            enabled: true,
            auto_rollback: false,
            max_rollback_age_secs: default_max_rollback_age_secs(),
            migration_detection_paths: default_migration_detection_paths(),
            health_check_timeout_secs: default_health_check_timeout_secs(),
        }
    }
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
    /// Which forge account to act as, when the CLI holds more than one login
    /// (`gh auth login` twice). Empty = whichever account is currently active.
    ///
    /// Naming the account per project is what lets two projects push and open
    /// PRs as DIFFERENT users at the same time: the token is fetched from the
    /// CLI's own credential store on each call, so no secret is stored here —
    /// only the login name. Without it, a machine whose CLI is signed in as the
    /// wrong user pushes fine over ssh and then 404s on every pull request.
    #[serde(default)]
    pub account: String,
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
    /// Whether PR review waits on / blocks over the forge's CI status (default
    /// on). Turn OFF when CI is unavailable (e.g. Actions billing disabled):
    /// the SA then judges the diff and relies on the local test/lint gates,
    /// instead of endlessly requesting changes for a CI that can never run.
    #[serde(default = "default_true")]
    pub require_ci: bool,
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
            account: String::new(),
            auto_pr: false,
            auto_review: true,
            auto_merge: false,
            require_ci: true,
            max_open_prs: default_max_open_prs(),
        }
    }
}

/// Per-project release settings. Drives the automated release pipeline that
/// tags a milestone once its target version is shipped and files the Release
/// chore. `enabled` is off by default: creating a git tag mutates the managed
/// codebase's history, so an existing project's release history is never
/// touched until an operator opts in — the same convention as `GitConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReleasesConfig {
    /// Master switch. When false, the cycle never tags or files releases,
    /// no matter how many milestones have been reached.
    #[serde(default)]
    pub enabled: bool,
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
    /// Release pipeline settings (automated tag + Release chore per milestone).
    #[serde(default)]
    pub releases: ReleasesConfig,
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
            releases: ReleasesConfig::default(),
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
