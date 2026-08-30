//! Configuration types — the engine-per-role mapping with three-level
//! precedence (worker override > project default > global default).
//!
//! Pure data; loading from `coxagent.json` is an adapter concern.

use coxagent_domain::{Priority, Role};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// The `artifacts` section type lives in [`crate::artifacts`] with the schema
// anchor and build manifest it belongs to; it is re-exported here so every
// Config section type is reachable as `config::<Section>Config`.
pub use crate::artifacts::ArtifactsConfig;

/// Known agent engine CLIs. `as_binary` gives the executable name to look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Opencode,
    Claude,
    Hermes,
    Gemini,
    Codex,
    /// GitHub Copilot CLI (`copilot`) — agentic, `--model auto` routing.
    Copilot,
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
            EngineKind::Copilot => "copilot",
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
            EngineKind::Copilot,
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

impl Default for EngineMapping {
    /// Claude/sonnet with auto-failover on — the mapping a project gets when it
    /// says nothing about engines. Named here rather than only inside
    /// [`Config::default`] so `engine` can be `#[serde(default)]`: an omitted
    /// section is a config that predates the field, not an unrepresentable
    /// value, and must not fail the whole document's load (COX-B043).
    fn default() -> Self {
        Self {
            default: EngineChoice {
                engine: EngineKind::Claude,
                model: "sonnet".to_owned(),
            },
            per_role: HashMap::new(),
            fallbacks: Vec::new(),
            auto_fallback: true,
            escalation: Vec::new(),
        }
    }
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

/// What a scrum sprint window is measured in — wall-clock days (default) or
/// loop cycles. Cycles shrink and stretch with the workload (90 s idle, 30+ min
/// mid-build), so day-based sprints are what most teams mean by "a sprint";
/// cycle-based stays available for cadence experiments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SprintUnit {
    #[default]
    Days,
    Cycles,
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
    /// Cycles per sprint in scrum mode (used when `sprint_unit` is `cycles`).
    #[serde(default = "default_sprint_len")]
    pub sprint_length_cycles: u64,
    /// What a sprint window is measured in. `days` (the default) rolls on wall
    /// clock — a sprint is a real day/week regardless of how fast cycles spin;
    /// `cycles` restores the pure cycle counter for teams that want it.
    #[serde(default)]
    pub sprint_unit: SprintUnit,
    /// Days per sprint when `sprint_unit` is `days`.
    #[serde(default = "default_sprint_days")]
    pub sprint_length_days: u64,
    /// The floor under a running sprint's actionable scope: when fewer than
    /// this many committed tickets are still workable, the mid-sprint top-up
    /// commits more from the backlog. Raise it to keep more DEV work in
    /// flight; 0 disables the top-up.
    #[serde(default = "default_scope_floor")]
    pub dev_scope_floor: usize,
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
    /// and tool caches (macOS via Seatbelt, Linux via Bubblewrap when `bwrap`
    /// is on `PATH`; platforms without a backend run unsandboxed with a
    /// warning). Off by default — turn on for untrusted codebases.
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
    /// How many days back the reverted-work scan (CXA-F047) may link a
    /// `Revert` commit to the deploy record it undid. Reverts older than this
    /// are history, not feedback. 0 = default (30).
    #[serde(default)]
    pub revert_scan_days: u64,
    /// Per-phase cadence knobs (docs budget, debt sweep, architecture audit).
    #[serde(default)]
    pub cadence: CadenceConfig,
    /// Quiet window `"HH:MM-HH:MM"` in UTC during which NO new engine calls
    /// start — overnight quota walls and sleeping laptops make those hours the
    /// most failure-prone and least supervised. (UTC because the hub has no
    /// reliable local-timezone source; VN 02:00–07:00 = `"19:00-00:00"`.)
    /// Urgent work is the exception: an open high-priority bug still runs.
    /// Empty = no window.
    #[serde(default)]
    pub quiet_hours_utc: String,
    /// Bug-burn floor (CXA-F028): when set, a scrum sprint commits only open
    /// bugs AT OR ABOVE this priority and the DEV bug queue ignores the rest —
    /// the "one-week high-severity burn" knob that parks cosmetic bugs for the
    /// burn's duration without losing them. Reuses the existing three-level
    /// [`Priority`] as the severity axis. Absent/`None` = burn every open bug
    /// (the historical behaviour), so existing configs deserialize unchanged.
    #[serde(default)]
    pub bug_burn_floor: Option<Priority>,
}

/// How often the periodic phases run. Zeros mean "use the built-in default" so
/// an absent config block changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CadenceConfig {
    /// Wiki refresh budget per UTC day (0 = default 5).
    pub docs_refreshes_per_day: u32,
    /// File a tech-debt sweep chore every N cycles (0 = default 10).
    pub debt_sweep_every_cycles: u64,
    /// SA architecture + docs audit every N sprints (0 = default 8).
    pub arch_review_every_sprints: u32,
}

impl CadenceConfig {
    #[must_use]
    pub fn docs_refreshes_per_day(&self) -> u32 {
        if self.docs_refreshes_per_day == 0 {
            5
        } else {
            self.docs_refreshes_per_day
        }
    }
    #[must_use]
    pub fn debt_sweep_every_cycles(&self) -> u64 {
        if self.debt_sweep_every_cycles == 0 {
            10
        } else {
            self.debt_sweep_every_cycles
        }
    }
    #[must_use]
    pub fn arch_review_every_sprints(&self) -> u32 {
        if self.arch_review_every_sprints == 0 {
            8
        } else {
            self.arch_review_every_sprints
        }
    }
}

/// Whether local wall-clock `now` (minutes since midnight) falls inside the
/// `"HH:MM-HH:MM"` window; supports windows that wrap midnight ("22:00-06:00").
/// Malformed windows are treated as no window — quiet hours must never be able
/// to halt a team by typo.
#[must_use]
pub fn in_quiet_window(window: &str, now_minutes: u32) -> bool {
    let Some((a, b)) = window.trim().split_once('-') else {
        return false;
    };
    let parse = |s: &str| -> Option<u32> {
        let (h, m) = s.trim().split_once(':')?;
        let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
        (h < 24 && m < 60).then_some(h * 60 + m)
    };
    let (Some(start), Some(end)) = (parse(a), parse(b)) else {
        return false;
    };
    if start == end {
        return false; // zero-length window means "off", not "always"
    }
    if start < end {
        (start..end).contains(&now_minutes)
    } else {
        now_minutes >= start || now_minutes < end
    }
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
    /// Per-user focus windows (CXA-F176), keyed by bare username: while a
    /// user's window is active, low-urgency questions addressed to them are
    /// held and flush once as a digest at the window's end instead of
    /// arriving one interrupt at a time. Absent = deliver immediately
    /// (the behaviour this feature must not change).
    #[serde(default)]
    pub focus_windows: std::collections::BTreeMap<String, FocusWindow>,
}

/// One person's focus-window ("quiet hours") settings (CXA-F176). Every
/// field defaults, so a pre-existing `coxagent.json` loads unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FocusWindow {
    /// Window `"HH:MM-HH:MM"` in UTC (same format and midnight-wrap rule as
    /// [`WorkflowConfig::quiet_hours_utc`], reusing `in_quiet_window`). While
    /// `now` is inside it, deferrable questions addressed to this user are
    /// held; the first cycle after it ends flushes them as one digest.
    /// Malformed = no window — a typo must never be able to hide questions.
    pub window_utc: String,
    /// Opt-in switch for defer-to-digest. Off = questions reach this user
    /// immediately even with a window configured.
    pub defer_to_digest: bool,
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

    /// See the `revert_scan_days` field; 0 = default (30 days).
    #[must_use]
    pub fn revert_scan_days(&self) -> u64 {
        if self.revert_scan_days == 0 {
            30
        } else {
            self.revert_scan_days
        }
    }
}

fn default_max_open_prs() -> u32 {
    4
}

fn default_max_changed_lines() -> usize {
    3000
}

fn default_sprint_len() -> u64 {
    10
}

fn default_sprint_days() -> u64 {
    1
}

fn default_scope_floor() -> usize {
    4
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
            dev_scope_floor: default_scope_floor(),
            feature_dev_enabled: true,
            ops_monitor: true,
            sleep_seconds: 30,
            concurrency: 1,
            budget_usd: None,
            mode: Mode::Kanban,
            sprint_length_cycles: default_sprint_len(),
            sprint_unit: SprintUnit::default(),
            sprint_length_days: default_sprint_days(),
            webhook_url: None,
            token_saver: true,
            language: Language::En,
            approve_over_usd: None,
            tdd: true,
            sandbox: false,
            human: HumanConfig::default(),
            backlog_cap: 0,
            pr_stale_days: 0,
            revert_scan_days: 0,
            cadence: CadenceConfig::default(),
            quiet_hours_utc: String::new(),
            bug_burn_floor: None,
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
#[allow(clippy::struct_excessive_bools)] // config flags, not a state machine
pub struct DeployConfig {
    /// The host port this project's app should publish (None = agent's choice).
    /// Must be a port a client can connect to: `0` is the kernel's "any free
    /// port" sentinel, not an address, and config load replaces it with a free
    /// port rather than letting the deploy health gate probe it forever
    /// (COX-B042). See
    /// [`crate::ports::outbound::is_publishable_host_port`].
    #[serde(default)]
    pub host_port: Option<u16>,
    /// Whether the cycle deploys at all (default on). Turn OFF for projects
    /// whose compose stack would collide with live infrastructure — e.g.
    /// CoXAgent developing itself, where the compose file binds the very port
    /// the live hub serves.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Self-upgrade (dogfood CD): the hub periodically runs
    /// `deploy/self-upgrade.sh`, which builds origin/<default_branch> in a
    /// detached worktree, swaps its OWN binary (backup kept), restarts, and
    /// rolls back if the new hub fails its health check. The script is a
    /// detached process so a dying hub cannot orphan its own rescue. Opt-in
    /// (default off) — only meaningful when the hub manages its own repo.
    #[serde(default)]
    pub self_upgrade: bool,
    /// Auto-redeploy the last known-good version when `deploy()` or a
    /// post-deploy `run_tests()` fails, so the shared environment self-heals
    /// instead of staying broken until a DEV agent picks up the bug ticket.
    /// Opt-in (default off) — an existing project's behavior never changes
    /// until an operator turns this on.
    #[serde(default)]
    pub auto_rollback: bool,
    /// Auto-redeploy the last known-good version when the Ops monitor finds
    /// the ALREADY-LIVE deployment unhealthy (CXA-F240) — the post-merge
    /// counterpart of `auto_rollback`, which only covers a deploy/tests
    /// failure detected in the same cycle that shipped it. This closes the
    /// gap where CI smoke passed and the deploy looked green, but the stack
    /// went dead afterwards and sat broken until a human reverted by hand.
    /// Opt-in (default off) — an existing project's behavior never changes
    /// until an operator turns this on.
    #[serde(default)]
    pub live_health_auto_rollback: bool,
    /// How many consecutive unhealthy Ops-monitor probes (one per cycle) the
    /// live app must serve before a `live_health_auto_rollback` fires —
    /// N consecutive checks, not one flaky probe. The revert itself is still
    /// bounded by `max_rollback_age_secs` (the allowed window) and the
    /// migration safety check.
    #[serde(default = "default_live_health_fail_checks")]
    pub live_health_fail_checks: u32,
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

/// Three consecutive unhealthy probes (three leader cycles) before a
/// live-health revert — one dead probe is often a transient network blip,
/// not a broken stack.
fn default_live_health_fail_checks() -> u32 {
    3
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
            self_upgrade: false,
            auto_rollback: false,
            live_health_auto_rollback: false,
            live_health_fail_checks: default_live_health_fail_checks(),
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
    /// Largest diff (changed lines) the SA will auto-merge without a human.
    /// A change larger than this is approved but held for a human to land.
    /// 0 = no size bound (never hold for size alone). Default 3000.
    #[serde(default = "default_max_changed_lines")]
    pub max_changed_lines: usize,
    /// Absolute URL of the hub the runner reports PR/review activity to, e.g.
    /// `http://localhost:4000`. Empty = the runner uses the loopback URL on
    /// `deploy.host_port` (the same hub it serves). The runner authenticates
    /// with an internally-minted token, so no forge secret lives in config.
    #[serde(default)]
    pub server_url: String,
    /// How many consecutive times the SA reviewer may silently fail to render
    /// a verdict on a PR (engine crash / unparseable JSON) before the runner
    /// surfaces it to a human instead of letting the PR starve undistributed.
    /// Default 4. 0 = never surface the skip (old behaviour).
    #[serde(default = "default_review_max_skips")]
    pub review_max_skips: u32,
    /// How long (hours) a mergeable CLEAN PR may sit open with NO review
    /// verdict before the runner stops waiting for the SA and verifies +
    /// merges it itself (an anti-starvation deadline, only when `auto_merge`
    /// is on). It still passes `verify_merged_result` and the size/human-eyes
    /// gates before landing. 0 = disabled (never auto-land a never-reviewed PR).
    #[serde(default = "default_review_deadline_hours")]
    pub review_deadline_hours: u32,
}

fn default_review_max_skips() -> u32 {
    4
}

fn default_review_deadline_hours() -> u32 {
    12
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
            max_changed_lines: default_max_changed_lines(),
            server_url: String::new(),
            review_max_skips: default_review_max_skips(),
            review_deadline_hours: default_review_deadline_hours(),
        }
    }
}

/// Per-project release settings. Drives the automated release pipeline that
/// tags a milestone once its target version is shipped and files the Release
/// chore. `enabled` is off by default: creating a git tag mutates the managed
/// codebase's history, so an existing project's release history is never
/// touched until an operator opts in — the same convention as `GitConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasesConfig {
    /// Master switch. When false, the cycle never tags or files releases,
    /// no matter how many milestones have been reached.
    #[serde(default)]
    pub enabled: bool,
    /// Cadence of the automated release cut (days between cuts; 0 = off).
    /// Every cut scans conventional commits since the last `v*` tag, decides
    /// the bump (feat → minor, else patch; major is a human call), and opens
    /// a release PR that a person lands from the Inbox. The merge tags it.
    #[serde(default)]
    pub cut_every_days: u64,
    /// CXA-F231: a cut only carries commit subjects whose ticket refs are in
    /// the verified-complete set (bugs at `Verified`, features/chores at
    /// `Done`/`Documented`); unverified or unreferenced subjects are excluded
    /// with an explicit reason on the SM manifest line. Off restores the
    /// legacy all-subjects cut during migration.
    #[serde(default = "default_cut_only_verified")]
    pub cut_only_verified: bool,
}

fn default_cut_only_verified() -> bool {
    true
}

impl Default for ReleasesConfig {
    /// `enabled`/`cut_every_days` default OFF (tagging mutates git history, so
    /// an existing project never releases until an operator opts in);
    /// `cut_only_verified` defaults ON — an RC silently carrying unverified
    /// work is the failure mode CXA-F231 removes. Defaults are EXPLICIT
    /// (COX-B043), never derived zero-values.
    fn default() -> Self {
        ReleasesConfig {
            enabled: false,
            cut_every_days: 0,
            cut_only_verified: default_cut_only_verified(),
        }
    }
}

/// Version of the persisted `coxagent.json` schema this build understands.
///
/// A document carrying a `schema_version` HIGHER than this is written by a
/// future build: load refuses it rather than accepting a shape it cannot
/// represent or defaulting it away (the same fail-closed posture state.json
/// already has via `SCHEMA_VERSION` / `parse_checked`). Documents that omit
/// `schema_version` predate the anchor and load as prior-version state.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Gap-detection coverage policy. `enabled` switches the coverage gate on/off;
/// `threshold` is the minimum gap-free depth (in cycles) a codebase must hold
/// before the pass stops flagging it — the knob the dashboard edits.
///
/// COX-B043: defaults are set by an EXPLICIT container `Default`
/// (`enabled = true, threshold = 3`), never Rust's derived zero-value, so an
/// unset knob is *documented-and-true*, not silently `{false, 0}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageConfig {
    #[serde(default = "default_coverage_enabled")]
    pub enabled: bool,
    #[serde(default = "default_coverage_threshold")]
    pub threshold: u32,
}

fn default_coverage_enabled() -> bool {
    true
}

fn default_coverage_threshold() -> u32 {
    3
}

impl Default for CoverageConfig {
    fn default() -> Self {
        CoverageConfig {
            enabled: default_coverage_enabled(),
            threshold: default_coverage_threshold(),
        }
    }
}

/// Dependency-health scan policy (CXA-F009).
///
/// Like [`CoverageConfig`], defaults come from an EXPLICIT container [`Default`]
/// (`enabled = true`) so an unset knob is documented-and-on rather than Rust's
/// derived zero-value silently turning the governance scan off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DepsConfig {
    #[serde(default = "default_deps_enabled")]
    pub enabled: bool,
}

fn default_deps_enabled() -> bool {
    true
}

impl Default for DepsConfig {
    fn default() -> Self {
        DepsConfig {
            enabled: default_deps_enabled(),
        }
    }
}

/// Top-level configuration persisted as `coxagent.json`.
///
/// EVERY section is `#[serde(default)]`, so a document written by an older
/// version — or by hand, mentioning only the sections it cares about — still
/// loads with defaults for what it omits. Only a value the schema cannot
/// represent fails the load (COX-B043): an absent section is not a corrupt
/// config, and must not be the reason a project's governance policy is
/// discarded along with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Config {
    /// Which engine and model each role runs on.
    #[serde(default)]
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
    /// Gap-detection coverage policy (enabled state + threshold).
    #[serde(default)]
    pub coverage: CoverageConfig,
    /// Per-project artifact-version registry (which build artifacts exist and
    /// the semver each carries), anchored by
    /// [`crate::artifacts::ARTIFACT_SCHEMA_VERSION`].
    #[serde(default)]
    pub artifacts: ArtifactsConfig,
    /// Dependency-health scan policy (CXA-F009). When enabled, the periodic
    /// self-tuning scan reads lock files, flags outdated/vulnerable packages,
    /// and files remediation tickets against a master 'Dependency Audit' epic.
    #[serde(default)]
    pub deps: DepsConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_only_verified_defaults_on_and_survives_old_documents() {
        // CXA-F231: an existing coxagent.json without the knob keeps the
        // honest-by-default gate ON (documents-old = gate-on, never off).
        let old = r#"{"releases":{"enabled":true,"cut_every_days":7}}"#;
        let cfg: serde_json::Value = serde_json::from_str(old).expect("old doc parses");
        let releases: ReleasesConfig =
            serde_json::from_value(cfg["releases"].clone()).expect("old releases load");
        assert!(
            releases.cut_only_verified,
            "old documents default the gate on"
        );
        // And the explicit opt-out is honored verbatim.
        let off = r#"{"releases":{"enabled":true,"cut_every_days":7,"cut_only_verified":false}}"#;
        let cfg: serde_json::Value = serde_json::from_str(off).expect("new doc parses");
        let releases: ReleasesConfig =
            serde_json::from_value(cfg["releases"].clone()).expect("new releases load");
        assert!(!releases.cut_only_verified);
    }

    #[test]
    fn quiet_window_handles_wrap_zero_and_garbage() {
        // Plain window.
        assert!(in_quiet_window("02:00-07:00", 3 * 60));
        assert!(!in_quiet_window("02:00-07:00", 8 * 60));
        // Wraps midnight (VN overnight in UTC).
        assert!(in_quiet_window("19:00-00:00", 20 * 60));
        assert!(in_quiet_window("22:00-06:00", 60));
        assert!(!in_quiet_window("22:00-06:00", 12 * 60));
        // Zero-length = off; garbage = off (a typo must never halt the team).
        assert!(!in_quiet_window("07:00-07:00", 7 * 60));
        assert!(!in_quiet_window("bogus", 0));
        assert!(!in_quiet_window("25:00-26:00", 0));
        assert!(!in_quiet_window("", 0));
    }

    #[test]
    fn focus_windows_are_additive_and_round_trip() {
        // A pre-existing human section (CXA-F176 does not exist in it) must
        // load unchanged: no focus windows, no defer — today's behaviour.
        let old = r#"{"gate_ready":true,"question_sla_minutes":30}"#;
        let human: HumanConfig = serde_json::from_str(old).expect("old human config loads");
        assert!(human.focus_windows.is_empty());
        assert!(human.gate_ready);
        assert_eq!(human.question_sla_minutes, 30);

        // A new document round-trips its per-user windows verbatim.
        let with_window = r#"{"focus_windows":{
            "luffy":{"window_utc":"09:00-12:00","defer_to_digest":true}
        }}"#;
        let human: HumanConfig = serde_json::from_str(with_window).expect("new human config loads");
        let fw = human
            .focus_windows
            .get("luffy")
            .expect("window keyed by bare username");
        assert_eq!(fw.window_utc, "09:00-12:00");
        assert!(fw.defer_to_digest);
        let rewritten = serde_json::to_string(&human).expect("serialize");
        let back: HumanConfig = serde_json::from_str(&rewritten).expect("deserialize");
        assert_eq!(back, human);
    }

    #[test]
    fn cut_only_verified_defaults_true_for_documents_without_the_knob() {
        // CXA-F231: a pre-F231 document (no `cut_only_verified`) loads with
        // the verification gate ON — an RC silently carrying unverified work
        // is the failure mode being removed, not the default behaviour.
        let old = r#"{"enabled":true,"cut_every_days":7}"#;
        let releases: ReleasesConfig = serde_json::from_str(old).expect("pre-F231 doc loads");
        assert!(releases.enabled);
        assert_eq!(releases.cut_every_days, 7);
        assert!(releases.cut_only_verified);

        // The explicit container default stays documented-true (COX-B043):
        // releases themselves stay opt-in, the verification gate does not.
        assert!(!ReleasesConfig::default().enabled);
        assert_eq!(ReleasesConfig::default().cut_every_days, 0);
        assert!(ReleasesConfig::default().cut_only_verified);
    }

    #[test]
    fn cadence_zeros_mean_defaults() {
        let c = CadenceConfig::default();
        assert_eq!(c.docs_refreshes_per_day(), 5);
        assert_eq!(c.debt_sweep_every_cycles(), 10);
        assert_eq!(c.arch_review_every_sprints(), 8);
        let c = CadenceConfig {
            docs_refreshes_per_day: 2,
            debt_sweep_every_cycles: 50,
            arch_review_every_sprints: 3,
        };
        assert_eq!(c.docs_refreshes_per_day(), 2);
        assert_eq!(c.debt_sweep_every_cycles(), 50);
        assert_eq!(c.arch_review_every_sprints(), 3);
    }

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

    /// The engine mapping a project gets when it configures none: an
    /// unconfigured project must still have a usable engine, and auto-failover
    /// on, exactly as the hand-written `Config::default` used to spell out.
    #[test]
    fn the_default_engine_mapping_is_claude_sonnet_with_failover_on() {
        let m = EngineMapping::default();

        assert_eq!(m.default.engine, EngineKind::Claude);
        assert_eq!(m.default.model, "sonnet");
        assert!(m.auto_fallback);
        assert!(m.per_role.is_empty());
        assert!(m.fallbacks.is_empty());
        assert!(m.escalation.is_empty());
    }

    /// COX-B043: an omitted section is a config that predates the field, not a
    /// corrupt one. `engine` was the last section without `#[serde(default)]`,
    /// so a hand-written document that never mentions engines used to fail the
    /// whole parse — taking the policy it DID declare down with it.
    #[test]
    fn a_document_that_omits_the_engine_section_keeps_the_policy_it_declares() {
        let cfg: Config = serde_json::from_str(r#"{"policy":{"forbidden_paths":["infra/"]}}"#)
            .expect("an omitted section is not a corrupt config");

        assert_eq!(cfg.policy.forbidden_paths, ["infra/"]);
        assert_eq!(cfg.engine.default.model, "sonnet");
    }

    /// CXA-F028: the bug-burn floor is optional and backward compatible — a
    /// config document that never mentions it burns every open bug exactly as
    /// before; naming it picks the minimum severity.
    #[test]
    fn bug_burn_floor_is_absent_by_default_and_parses_the_priority_names() {
        let cfg: Config = serde_json::from_str("{}").expect("legacy config");
        assert_eq!(
            cfg.workflow.bug_burn_floor, None,
            "absent = burn everything"
        );

        let cfg: Config = serde_json::from_str(
            r#"{"workflow":{"ba_every_n_cycles":4,"feature_dev_enabled":true,
                "sleep_seconds":30,"bug_burn_floor":"high"}}"#,
        )
        .expect("floor config");
        assert_eq!(cfg.workflow.bug_burn_floor, Some(Priority::High));

        let cfg: Config = serde_json::from_str(
            r#"{"workflow":{"ba_every_n_cycles":4,"feature_dev_enabled":true,
                "sleep_seconds":30,"bug_burn_floor":"low"}}"#,
        )
        .expect("floor config");
        assert_eq!(cfg.workflow.bug_burn_floor, Some(Priority::Low));
    }
}
