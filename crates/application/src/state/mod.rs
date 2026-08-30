//! `ProjectState` — the persisted aggregate the store loads and saves as a unit.
//!
//! Kept in the application layer because `schema_version` is a persistence
//! concern; the domain stays free of it.

use coxagent_domain::{DebtSignal, Goal, SemVer, Ticket, TicketId};
use serde::{Deserialize, Serialize};

mod chat;
mod docs;
mod drift;
mod goals;
mod governance;
mod integrity;
mod ops;
mod outbox;
mod work;

pub use chat::*;
pub use docs::*;
pub use drift::*;
pub use goals::*;
pub use governance::*;
pub use integrity::*;
pub use ops::*;
pub use outbox::*;
pub use work::*;

/// Current on-disk schema version. Bumped when the serialized shape changes;
/// the store refuses to silently load a newer version than it understands.
pub const SCHEMA_VERSION: u32 = 1;

/// One entry in the activity feed — who did what to which ticket, when. Powers
/// the dashboard's "what are the agents doing" view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub at: String,
    pub agent: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
}

/// Keep the activity feed bounded.
pub const MAX_ACTIVITY: usize = 60;

pub const MAX_COMMENTS: usize = 500;

pub const MAX_CHAT: usize = 500;

/// The id of the default channel every project has and everyone can see.
pub const GENERAL_CHANNEL: &str = "general";

/// The system feed channel: agent/bot notifications (PRs, deploys, digest,
/// previews) land here instead of spamming `#general`. Open to everyone.
pub const AGENTS_CHANNEL: &str = "agents";
/// Where work waiting on a PERSON is announced. Separate from `agents` so a
/// human can watch decisions without reading the whole machine's chatter.
pub const APPROVALS_CHANNEL: &str = "approvals";
/// Where incident post-mortems land (CXA-F012): a single room for outage
/// analysis so someone watching for service problems isn't sifting `#general`
/// or re-reading routine deploy chatter.
pub const INCIDENTS_CHANNEL: &str = "incidents";

fn general_channel() -> String {
    GENERAL_CHANNEL.to_owned()
}

pub const STANDARD_DOC_FOLDERS: &[&str] = &[
    "Product",       // BA / PO — feature docs & specs
    "Architecture",  // SA — technical designs & decisions
    "Design",        // PD — UX flows & the design system
    "Engineering",   // DEV — chores, maintenance, how-tos
    "QA",            // TEST — test plans & reports
    "Release Notes", // shipped versions, merges, deploys (the log)
    "Operations",    // ops — runbooks, deploy & infra
    "Team",          // SM — retros, decisions, ways of working
];

/// The whole state of one managed project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // a persisted data aggregate, not a state machine
pub struct ProjectState {
    pub schema_version: u32,
    /// Short project alias prefixed onto every ticket id (e.g. `CXC`). Empty for
    /// backward compatibility (ids then read `FEAT-001` with no prefix).
    #[serde(default)]
    pub alias: String,
    /// Custom display name set by the user (overrides the auto-generated
    /// `"<alias> project"`). Absent until the project is renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub current_version: SemVer,
    pub tickets: Vec<Ticket>,
    #[serde(default)]
    pub history: Vec<DeployRecord>,
    /// Merged-then-reverted work (CXA-F047): revert commits the scan linked to
    /// shipped tickets, each with a human approve/dismiss verdict. Bounded,
    /// newest last — approved events are what planning is allowed to learn from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reverted_work: Vec<RevertEvent>,
    #[serde(default)]
    pub activity: Vec<ActivityEntry>,
    #[serde(default)]
    pub spend: Spend,
    #[serde(default)]
    pub sprint: Option<Sprint>,
    #[serde(default)]
    pub sprints: Vec<SprintRecord>,
    /// Upcoming sprints prepared ahead of time, consumed front-first at
    /// rollover. See [`PlannedSprint`].
    #[serde(default)]
    pub sprint_queue: Vec<PlannedSprint>,
    /// Why each on-hold ticket is parked (ticket id → reason). Written on
    /// hold (human or the auto-hold sweep), cleared on resume.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub hold_reasons: std::collections::BTreeMap<String, String>,
    /// Per-role engine health (role label → counters), fed by the cycle's
    /// error report. Rendered on the Agents view.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub role_health: std::collections::BTreeMap<String, RoleHealth>,
    #[serde(default)]
    pub deploy: Option<DeployStatus>,
    /// Discussion threads: per-ticket and team-channel comments.
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// The SA agent's latest review verdict per open PR (by number).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<PrReview>,
    /// Open pull requests as reported by the runner over HTTP. The runner owns
    /// the forge credentials, so the hub only ever reads this list back for the
    /// Review tab — it never lists PRs itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_prs: Vec<crate::ports::outbound::PrOpen>,
    /// PRs the SA approved but held for a person (`needs_human_eyes`): PR
    /// number → why the machine refused to land it alone. Surfaced in the
    /// Inbox with approve/dismiss; entries for PRs no longer open are pruned
    /// on every open-PR sync.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub human_holds: std::collections::BTreeMap<u64, String>,
    /// Attachments per ticket id (PD design images, screenshots) — the bytes
    /// live in blob storage (`StoragePort`: MinIO/S3 or the local blob dir);
    /// this holds the records the UI lists.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ticket_attachments: std::collections::BTreeMap<String, Vec<TicketAttachment>>,
    /// Team chat: human-to-human messages among the people on the project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chat: Vec<ChatMsg>,
    /// Slack-style chat channels beyond the implicit `#general`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<Channel>,
    /// Project-level design system authored by PD (absent until a UI ticket
    /// prompts PD to create it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_system: Option<DesignSystem>,
    /// Product milestones the sprints work toward (authored once by the PO).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub milestones: Vec<Milestone>,
    /// Declared product goals with stable ids (CXA-F228) — the lines the PO's
    /// goal gate proposes against. Associations and ledger entries bind to
    /// `Goal::id`, never to the title, so rewording a goal never rewrites
    /// attribution. Absent until the first goal is declared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goals: Vec<Goal>,
    /// Append-only outcome ledger (CXA-F228): one entry per ticket that
    /// reached `Verified`, freezing ticket -> declared goal -> capture commit
    /// -> verification timestamp. Newest last; see [`MAX_OUTCOME_LEDGER`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outcome_ledger: Vec<OutcomeLedgerEntry>,
    /// Living documentation pages (product + technical) written by agents/humans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<DocPage>,
    /// Explicit Wiki folder paths (`/`-separated, nested), so a folder can exist
    /// and nest even before it holds a page — Confluence-style spaces/pages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doc_folders: Vec<String>,
    /// Per-page refresh bookkeeping (page id → mark), so the idle-cycle Wiki
    /// refresher does not re-run the SAME page every cycle: a just-refreshed page
    /// cools down, and a page whose rewrite keeps failing the structure gate is
    /// parked (needs a human/redesign) instead of burning a call forever — the
    /// root of the DOCS run churn.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub doc_refresh: std::collections::BTreeMap<String, DocRefreshMark>,
    /// Per-cycle scorecards (bounded, newest last) — the deterministic
    /// stability/cost/effectiveness grade the dashboard charts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cycle_scores: Vec<CycleScore>,
    /// Lessons the team learned in past retros — fed back into agent prompts so
    /// the team actually improves over time (kept bounded, newest last).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lessons: Vec<String>,
    /// The team's durable decisions & conventions (ADR-style one-liners): the
    /// architecture calls, tech choices, and "how we do X" every agent should
    /// honour — so parallel LLM calls stay consistent instead of contradicting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<String>,
    /// Set when the SA has called a halt on new features to run a hardening /
    /// refactor sprint (the codebase risk is too high to keep building on).
    /// While true, BA proposes no new features and planning dedicates the sprint
    /// to the refactor chores; cleared once they're all done.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refactor_mode: bool,
    /// Persistent, restart-safe leader-cycle counter that drives sprint timing.
    /// The per-process cycle number resets to 1 every worker launch, so sprints
    /// stalled after a restart; this counter lives in state and only moves
    /// forward, so sprints keep rolling regardless of restarts.
    #[serde(default)]
    pub sprint_cycle: u64,
    /// Persistent, restart-safe project-cycle counter — the authoritative cycle
    /// number, advanced once per leader cycle and reused for scoring + cadence.
    /// The per-process counter resets to 1 every worker launch, so both the
    /// scorecard key and the `% N` cadence (codegraph, debt sweep, BA, scrum
    /// topic) drifted after a restart. This counter lives in state, only moves
    /// forward, and is unbounded (it is NOT truncated by the cycle_scores 100-cap),
    /// so cadence positions and `sweeps_done` values stay consistent across
    /// restarts. Non-leader runners ignore it — their reports are un-scored.
    #[serde(default)]
    pub cycle: u64,
    /// The PO's goal for the upcoming sprint (human-set from the Scrum view). When
    /// set it becomes the sprint goal on the next roll-over and steers the BA's
    /// proposals, so the team works toward what the PO asked for — not just
    /// whatever happens to be in the backlog.
    #[serde(default)]
    pub sprint_goal: String,
    /// UTC date (YYYY-MM-DD) the daily digest was last posted to the team chat,
    /// so exactly one digest lands per day regardless of restarts or operators.
    #[serde(default)]
    pub last_digest_day: String,
    /// How many times a DEV agent has pushed fixes to each open PR (by number).
    /// Capped so a review↔fix ping-pong escalates to a human instead of
    /// burning tokens forever; entries are dropped when the PR closes.
    #[serde(default)]
    pub pr_fix_attempts: std::collections::BTreeMap<u64, u32>,
    /// How many times in a ROW the SA reviewer failed to render a verdict on
    /// each open PR (engine crash / unparseable JSON), so a PR the reviewer
    /// silently chokes on is surfaced to a human instead of starving forever.
    /// Cleared whenever the PR gets a real review or the record is reset on
    /// merge/close.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub pr_review_skips: std::collections::BTreeMap<u64, u32>,
    /// Engine conversation id of the last fix run per PR — the next fix round
    /// RESUMES that conversation (the agent still has the branch, the feedback
    /// and its own changes in context) instead of starting cold. Dropped with
    /// `pr_fix_attempts` when the PR closes.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub pr_sessions: std::collections::BTreeMap<u64, String>,
    /// Engine conversation id per ticket work-session, keyed `"<ticket>/<role>"`
    /// (e.g. `COX-B002/dev_bug`). When a DEV agent RE-ENTERS a ticket it already
    /// worked (a retry, or after a parked question is answered), it resumes this
    /// conversation instead of re-reading the code cold. Resume routes to the
    /// role's configured engine; a session minted by a different engine (a prior
    /// failover) simply fails to resume and falls back to a cold run — a session
    /// id is engine-native, so this is safe, just a missed optimization.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ticket_sessions: std::collections::BTreeMap<String, String>,
    /// Tickets held for HUMAN cost approval: estimated run cost exceeded
    /// `workflow.approve_over_usd`. Value = the estimate shown to the human.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub cost_holds: std::collections::BTreeMap<String, f64>,
    /// Lint (clippy) error baseline: a DEV change may never ADD errors; an
    /// improvement lowers the bar for everyone after. `None` until first
    /// measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clippy_baseline: Option<u64>,
    /// Debt findings from the most recent debt-sweep run, persisted so a sweep
    /// and its outcomes are auditable across cycles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub debt_signals: Vec<DebtSignal>,
    /// Cycle numbers on which a debt-sweep was already filed, so a sweep is not
    /// re-filed for the same cycle after a restart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sweeps_done: Vec<u64>,
    /// Merged PR numbers already synced into ticket state (human merges on
    /// the forge must reflect back exactly once).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub seen_merged_prs: std::collections::BTreeSet<u64>,
    /// When each ticket last had a PR MERGE (ticket id → RFC3339). Feeds the
    /// fix-on-fix brake: a second PR for a ticket merged within the last day
    /// is the stacked-chain smell (B036→B044, B065 twice in one night) — it
    /// waits for a person instead of auto-landing another layer.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ticket_last_merge: std::collections::BTreeMap<String, String>,
    /// Closed-without-merge PR numbers already processed into lessons, so a
    /// human rejection is learned from exactly once.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub seen_closed_prs: std::collections::BTreeSet<u64>,
    /// Ticket ids the end-of-cycle ship sweep has already committed and pushed
    /// a branch for. The sweep is otherwise a pure function of status
    /// (`Fixed`/`Done` + unassigned), so a shipped ticket would be re-selected
    /// every cycle forever — re-cloning its work, re-opening duplicate PRs, and
    /// (when the worktree is dirty from those rejected attempts) logging the
    /// same `checkout` failure each time. Recording the sweep here makes it
    /// idempotent in truth, not just in happy-path theory: a shipped ticket is
    /// shipped once.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub swept_tickets: std::collections::BTreeSet<String>,
    /// Tickets a human approved to run despite the cost estimate.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub cost_approved: std::collections::BTreeSet<String>,
    /// Self-tuning knobs the orchestrator sets FROM the evals — the loop
    /// reacts to its own health instead of waiting for a human to read a
    /// dashboard. All deterministic; SM announces every change.
    #[serde(default, skip_serializing_if = "Tuning::is_default")]
    pub tuning: Tuning,
    /// Operator freeze/override per self-tuning brake (CXA-F238), keyed by
    /// brake field name (`bugs_first` / `skip_ba`). Composed AFTER the
    /// autonomous decision each tuning pass and expired against a bound, so
    /// an override steers the loop without rewriting its hysteresis state.
    /// Persists across restarts until cleared by another operator action or
    /// its own expiry.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tuning_overrides: std::collections::BTreeMap<String, BrakeHold>,
    /// Append-only brake-cockpit audit trail (CXA-F238): one entry per brake
    /// field change, whoever wrote it — the autonomous pass included. Bounded,
    /// newest last; see [`MAX_TUNING_HISTORY`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tuning_history: Vec<TuningAuditEntry>,
    /// Definition-of-Done evidence per ticket (bounded per ticket) — a ticket
    /// only reaches Verified with context-appropriate proof attached.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ticket_evidence: std::collections::BTreeMap<String, Vec<Evidence>>,
    /// True while the team is in merge-queue RECOVERY: the open-PR count blew
    /// past twice the WIP limit, so cycles do merge/conflict work only until
    /// the queue is back under the limit.
    #[serde(default)]
    pub queue_recovery: bool,
    /// UTC day (YYYY-MM-DD) the engine-memory hygiene pass last ran, so the
    /// audit of `~/.claude` project memory happens once a day, not every cycle.
    #[serde(default)]
    pub last_memory_hygiene_day: String,
    /// Execution jobs the control plane queued for a runner (e.g. a human's
    /// force-merge). Runners claim + remove atomically via `mutate_state`.
    #[serde(default)]
    pub jobs: Vec<PendingJob>,
    /// UTC day the SM last posted the consolidated impediment report.
    #[serde(default)]
    pub last_impediment_day: String,
    /// PRs the SM already sent to the SA for a stuck-PR rescue (root-cause →
    /// close or concrete instructions). One rescue per PR, ever — the second
    /// stall goes to a human.
    #[serde(default)]
    pub pr_rescues: std::collections::BTreeMap<u64, u32>,
    /// Tickets the SA already re-designed after 3 red builds. One redesign per
    /// ticket; failing again stays parked for a human.
    #[serde(default)]
    pub ticket_redesigns: std::collections::BTreeMap<String, u32>,
    /// The sprint number the clean-base drain notice was last announced for, so
    /// the SA explains the "merge everything first" hold once per sprint, not
    /// every cycle.
    #[serde(default)]
    pub drain_notice_sprint: u32,
    /// Failed DEV attempts per ticket id. At 3 the ticket is parked (skipped by
    /// agents, flagged for a human) so a poisoned ticket can't burn tokens
    /// forever. Cleared when a human edits the ticket.
    #[serde(default)]
    pub ticket_fail_attempts: std::collections::BTreeMap<String, u32>,
    /// Stale-PR sweep memory: PR number → rescue attempts already spent, so
    /// the policy rescues once and closes on the second pass, never loops.
    #[serde(default)]
    pub pr_rescue_attempts: std::collections::BTreeMap<String, u32>,
    /// Every human approval decision, for the adaptive gate to learn from
    /// (docs/ADAPTIVE_APPROVAL.md).
    #[serde(default)]
    pub approval_samples: Vec<crate::use_cases::approval_memory::ApprovalSample>,
    /// Auto-approved tickets still inside their undo window: ticket id → the
    /// RFC3339 time the window opened.
    #[serde(default)]
    pub auto_approved_at: std::collections::BTreeMap<String, String>,
    /// Shapes a human explicitly asked to be asked about again — a manual
    /// override that outranks anything learned.
    #[serde(default)]
    pub ask_again_shapes: Vec<String>,
    /// Per-ticket work journal: what past attempts tried and where they got
    /// stuck, fed into the next attempt's prompt so a retried ticket resumes
    /// from prior findings instead of rediscovering them (bounded per ticket;
    /// entry removed when the ticket completes).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub ticket_journal: std::collections::BTreeMap<String, Vec<String>>,
    /// Structured failure log per ticket (see [`AttemptFailure`]). Defaulted so
    /// state written before this existed still loads.
    #[serde(default)]
    pub ticket_failures: std::collections::BTreeMap<String, Vec<AttemptFailure>>,
    /// Questions agents have asked each other (see [`AgentQuestion`]).
    #[serde(default)]
    pub questions: Vec<AgentQuestion>,
    /// Open engine-infrastructure incidents, keyed by engine id.
    #[serde(default)]
    pub engine_incidents: Vec<EngineIncident>,
    /// Last day each once-a-day job ran (`standup`, `po-milestones`, …), so a
    /// daily ceremony is not repeated every cycle.
    #[serde(default)]
    pub daily_jobs: std::collections::BTreeMap<String, String>,
    /// Whether the Ops/SRE monitor currently sees the deployed app as down —
    /// tracked so it files exactly one bug per outage and can announce recovery.
    #[serde(default)]
    pub ops_down: bool,
    /// Spend accumulated on the current calendar day (UTC), for the daily budget
    /// policy. Resets when the day rolls over.
    #[serde(default)]
    pub spend_today_usd: f64,
    /// The UTC date (`YYYY-MM-DD`) `spend_today_usd` is counting.
    #[serde(default)]
    pub spend_day: String,
    /// Whether the lifetime `budget_usd` early-warning (`budget_warning`,
    /// [`crate::policy::approaching_cap`]) has already fired for the current
    /// approach toward the cap. Cleared once spend is no longer approaching
    /// it (cap raised, or spend passed it into the hard-stop range), so a
    /// later crossing can warn again.
    #[serde(default)]
    pub budget_warned_lifetime: bool,
    /// Same as `budget_warned_lifetime` but for the per-day `daily_budget_usd`
    /// cap. Reset by [`ProjectState::add_daily_spend`] whenever the UTC day
    /// rolls over, so the warning can re-fire each day.
    #[serde(default)]
    pub budget_warned_daily: bool,
    /// Ordinal of the last deploy that passed both `deploy()` and
    /// `run_tests()` — advances only on a known-good deploy, so
    /// `max_rollback_distance` can bound how far a rollback may reach.
    #[serde(default)]
    pub deploy_index: u64,
    /// True while the currently-live deploy is a rollback, not the tip of
    /// `work_dir` — forces the leader tail to keep retrying a forward deploy
    /// each cycle (self-healing) instead of waiting for new ticket work.
    #[serde(default)]
    pub in_rollback: bool,
    /// The last deploy that passed both `deploy()` and `run_tests()` —
    /// auto-rollback's target. `None` until the first one ever succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_good_deploy: Option<KnownGoodDeploy>,
    /// Outcome of the most recent auto-rollback attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rollback: Option<RollbackStatus>,
    /// Rolling incident history (CXA-F012): one post-mortem per deploy
    /// revision or incident, newest last, capped so a long outage storm cannot
    /// grow state without bound. The durable inspection record that links a
    /// rollback to its root-cause prevention ticket and team lesson.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incidents: Vec<IncidentRecord>,
    /// Commit shas blacklisted from known-good promotion (CXA-F012): after a
    /// rollback the failing sha is pinned here so the self-healing loop cannot
    /// re-promote the SAME broken commit every cycle until its root-cause bug
    /// is verified. Consult-only against state — no git ref changes.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub rolled_back_commits: std::collections::BTreeSet<String>,
    /// One bug-status count per UTC day (`YYYY-MM-DD` → counts), recorded by
    /// the leader cycle so the burn-down history survives restarts instead of
    /// leaving only today's snapshot in `metrics::compute` (CXA-F032). Bounded
    /// by [`MAX_BUG_SNAPSHOT_DAYS`]; every added field is serde-defaulted so
    /// the schema stays at version 1.
    #[serde(default)]
    pub bug_snapshots: std::collections::BTreeMap<String, BugSnapshot>,
    /// Human governance-attention ledger (CXA-F230): the append-only, bounded
    /// record of every discrete operator review decision (ready approvals,
    /// verify verdicts, cost approvals, PR hold resolutions, undo approvals),
    /// each frozen with its ticket class and action time. serde-defaulted so
    /// state persisted before this existed loads clean — no migration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub governance_interventions: Vec<InterventionRecord>,
    /// Stale human-gate hold tracking (CXA-F236): ticket id → when the ticket
    /// entered its current gate wait, which gate that is, and how far up the
    /// escalation ladder it has climbed since the last decision. Entries are
    /// recorded by the gate-escalation pass, cleared when the ticket leaves
    /// the gate status or changes gates (a decision resets the ladder).
    /// serde-defaulted — additive, no migration.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub gate_holds: std::collections::BTreeMap<String, GateHold>,
    /// Open architecture-drift alerts (CXA-F226): one per standing conformance
    /// violation, deduped by (area, message), each linking the bug filed for
    /// it. Deliberately serialized even when empty (like `engine_incidents`)
    /// — the dashboard's zero indicator must read 0, never absence.
    #[serde(default)]
    pub drift_alerts: Vec<DriftAlert>,
}

/// One day's open/fixed/verified bug counts — the persisted burn-down point
/// (CXA-F032).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BugSnapshot {
    #[serde(default)]
    pub open: u32,
    #[serde(default)]
    pub fixed: u32,
    #[serde(default)]
    pub verified: u32,
}

/// Cap on persisted daily bug snapshots — a full leap year of days; older
/// entries are dropped as new ones arrive so state cannot grow without bound.
pub const MAX_BUG_SNAPSHOT_DAYS: usize = 366;

/// Stale human-gate hold tracking for one ticket (CXA-F236): when the ticket
/// entered the gate wait it is still sitting in, and how far up the configured
/// escalation ladder it has climbed since the last human decision on it.
/// serde-defaulted so state persisted before this existed loads clean — no
/// migration; a missing entry reads as "not escalated".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GateHold {
    /// Unix seconds when the ticket entered the human-gate wait it is still
    /// in. 0 = unknown (no entry recorded yet).
    #[serde(default)]
    pub entered_at_unix_s: i64,
    /// Highest escalation tier reached since the last decision (0 = none).
    #[serde(default)]
    pub escalated_to_tier: u32,
    /// Which gate this hold is waiting at (true = ready gate, false = verify
    /// gate). A hold carried across a gate CHANGE (approved, then the work
    /// later reached the other gate) is not the same wait — the escalation
    /// resets tier and clock instead of inheriting them.
    #[serde(default)]
    pub at_ready_gate: bool,
}

/// Cap on how many incident records are kept (newest first). One per deploy
/// revision means a storm of failures still stays bounded and readable.
pub const MAX_INCIDENTS: usize = 12;

/// Cap on the brake-cockpit audit trail (CXA-F238): ~500 entries covers months
/// of daily flips plus every operator intervention; older entries drop as new
/// ones arrive so the trail cannot grow without bound.
pub const MAX_TUNING_HISTORY: usize = 500;

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            queue_recovery: false,
            last_memory_hygiene_day: String::new(),
            jobs: Vec::new(),
            last_impediment_day: String::new(),
            pr_rescues: std::collections::BTreeMap::new(),
            ticket_redesigns: std::collections::BTreeMap::new(),
            alias: String::new(),
            display_name: None,
            current_version: SemVer::default(),
            tickets: Vec::new(),
            history: Vec::new(),
            reverted_work: Vec::new(),
            activity: Vec::new(),
            spend: Spend::default(),
            sprint: None,
            sprints: Vec::new(),
            sprint_queue: Vec::new(),
            hold_reasons: std::collections::BTreeMap::new(),
            role_health: std::collections::BTreeMap::new(),
            deploy: None,
            comments: Vec::new(),
            reviews: Vec::new(),
            open_prs: Vec::new(),
            human_holds: std::collections::BTreeMap::new(),
            ticket_attachments: std::collections::BTreeMap::new(),
            chat: Vec::new(),
            channels: Vec::new(),
            design_system: None,
            milestones: Vec::new(),
            goals: Vec::new(),
            outcome_ledger: Vec::new(),
            docs: Vec::new(),
            doc_folders: Vec::new(),
            doc_refresh: std::collections::BTreeMap::new(),
            cycle_scores: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            refactor_mode: false,
            sprint_cycle: 0,
            cycle: 0,
            sprint_goal: String::new(),
            last_digest_day: String::new(),
            pr_fix_attempts: std::collections::BTreeMap::new(),
            pr_review_skips: std::collections::BTreeMap::new(),
            pr_sessions: std::collections::BTreeMap::new(),
            ticket_sessions: std::collections::BTreeMap::new(),
            cost_holds: std::collections::BTreeMap::new(),
            clippy_baseline: None,
            debt_signals: Vec::new(),
            sweeps_done: Vec::new(),
            seen_merged_prs: std::collections::BTreeSet::new(),
            ticket_last_merge: std::collections::BTreeMap::new(),
            seen_closed_prs: std::collections::BTreeSet::new(),
            swept_tickets: std::collections::BTreeSet::new(),
            cost_approved: std::collections::BTreeSet::new(),
            tuning: Tuning::default(),
            tuning_overrides: std::collections::BTreeMap::new(),
            tuning_history: Vec::new(),
            ticket_evidence: std::collections::BTreeMap::new(),
            drain_notice_sprint: 0,
            ticket_fail_attempts: std::collections::BTreeMap::new(),
            pr_rescue_attempts: std::collections::BTreeMap::new(),
            approval_samples: Vec::new(),
            auto_approved_at: std::collections::BTreeMap::new(),
            ask_again_shapes: Vec::new(),
            ticket_journal: std::collections::BTreeMap::new(),
            ticket_failures: std::collections::BTreeMap::new(),
            questions: Vec::new(),
            engine_incidents: Vec::new(),
            daily_jobs: std::collections::BTreeMap::new(),
            ops_down: false,
            spend_today_usd: 0.0,
            spend_day: String::new(),
            budget_warned_lifetime: false,
            budget_warned_daily: false,
            deploy_index: 0,
            in_rollback: false,
            last_good_deploy: None,
            last_rollback: None,
            incidents: Vec::new(),
            rolled_back_commits: std::collections::BTreeSet::new(),
            bug_snapshots: std::collections::BTreeMap::new(),
            governance_interventions: Vec::new(),
            gate_holds: std::collections::BTreeMap::new(),
            drift_alerts: Vec::new(),
        }
    }
}

impl ProjectState {
    /// Append an activity entry, trimming the feed to [`MAX_ACTIVITY`].
    pub fn log_activity(&mut self, agent: &str, action: &str, ticket: Option<String>) {
        self.activity.push(ActivityEntry {
            at: now_rfc3339(),
            agent: agent.to_owned(),
            action: action.to_owned(),
            ticket,
        });
        let overflow = self.activity.len().saturating_sub(MAX_ACTIVITY);
        if overflow > 0 {
            self.activity.drain(0..overflow);
        }
    }

    /// Record one detected revert (CXA-F047), deduped by commit sha — the
    /// ledger and the human decision surface are both keyed by sha, so one
    /// git undo is one event no matter how attribution drifts between scans.
    /// A re-scan must never re-flag (or double-count) a commit this ledger
    /// already holds, whatever its decision. Returns whether the event is
    /// NEW; callers announce it only then.
    pub fn record_revert(&mut self, ev: RevertEvent) -> bool {
        if self.reverted_work.iter().any(|e| e.sha == ev.sha) {
            return false;
        }
        self.reverted_work.push(ev);
        let overflow = self.reverted_work.len().saturating_sub(MAX_REVERT_EVENTS);
        if overflow > 0 {
            self.reverted_work.drain(0..overflow);
        }
        true
    }

    /// Apply a human's approve/dismiss verdict to the revert commit `sha`
    /// (CXA-F047). Returns whether a PENDING event was found and decided —
    /// an already-decided event is never re-decided.
    pub fn decide_revert(&mut self, sha: &str, decision: RevertDecision, by: &str) -> bool {
        let Some(ev) = self
            .reverted_work
            .iter_mut()
            .find(|e| e.sha == sha && e.decision == RevertDecision::Pending)
        else {
            return false;
        };
        ev.decision = decision;
        ev.decided_at = Some(now_rfc3339());
        ev.decided_by = Some(by.to_owned());
        true
    }

    /// Attach a piece of DoD evidence to a ticket (bounded: 6 per ticket,
    /// detail capped) — dashboards render these; TEST requires them.
    pub fn add_evidence(&mut self, ticket: &str, kind: &str, label: &str, detail: &str) {
        let ev = Evidence {
            kind: kind.to_owned(),
            label: label.chars().take(120).collect(),
            detail: detail.chars().take(1200).collect(),
            at: now_rfc3339(),
        };
        let list = self.ticket_evidence.entry(ticket.to_owned()).or_default();
        list.push(ev);
        let overflow = list.len().saturating_sub(6);
        if overflow > 0 {
            list.drain(0..overflow);
        }
    }

    /// Append a work-journal note for a ticket (what an attempt tried / where
    /// it got stuck). Bounded: 4 notes per ticket, 500 chars per note — the
    /// journal is a briefing for the next attempt, not a log.
    /// Record a failed attempt as DATA, not prose. Everything downstream — the
    /// escalation router, the next developer's brief, the impediment digest —
    /// used to re-derive the failure class by grepping an English sentence,
    /// which meant the classification was only ever as good as the wording of
    /// whoever wrote the message. The gate that rejected the work knows exactly
    /// what it rejected; this is where it says so.
    pub fn record_attempt_failure(&mut self, ticket: &str, failure: AttemptFailure) {
        let log = self.ticket_failures.entry(ticket.to_owned()).or_default();
        log.push(failure);
        let overflow = log.len().saturating_sub(6);
        if overflow > 0 {
            log.drain(0..overflow);
        }
    }

    /// Append one brake-cockpit audit entry (CXA-F238), pruning the oldest
    /// past [`MAX_TUNING_HISTORY`]. Never fails: a full trail drops history,
    /// it does not block the tuning write it is recording.
    pub fn record_tuning_change(&mut self, entry: TuningAuditEntry) {
        self.tuning_history.push(entry);
        let overflow = self.tuning_history.len().saturating_sub(MAX_TUNING_HISTORY);
        if overflow > 0 {
            self.tuning_history.drain(0..overflow);
        }
    }

    /// Raise or reinforce an engine incident. Repeats bump the count rather
    /// than filling the list with the same outage a hundred times.
    pub fn open_engine_incident(&mut self, engine: &str, role: &str, reason: &str) {
        let reason: String = reason.trim().chars().take(300).collect();
        if let Some(inc) = self
            .engine_incidents
            .iter_mut()
            .find(|i| i.engine == engine)
        {
            inc.hits = inc.hits.saturating_add(1);
            inc.reason = reason;
            return;
        }
        self.engine_incidents.push(EngineIncident {
            engine: engine.to_owned(),
            reason,
            role: role.to_owned(),
            since: now_rfc3339(),
            hits: 1,
        });
    }

    /// Clear an engine's incident because a run just succeeded on it. Returns
    /// the incident that was resolved, so the caller can say so out loud —
    /// an alert nobody sees close is an alert people learn to ignore.
    #[must_use]
    pub fn close_engine_incident(&mut self, engine: &str) -> Option<EngineIncident> {
        let i = self
            .engine_incidents
            .iter()
            .position(|i| i.engine == engine)?;
        Some(self.engine_incidents.remove(i))
    }

    /// Record a question from one role to another, unless that ticket already
    /// has one open — a second unanswered question means the first was not the
    /// blocker, and two of them just queue cost.
    pub fn ask_question(&mut self, ticket: &str, from: &str, to: &str, body: &str) -> bool {
        let body = body.trim();
        if body.is_empty() || self.open_question(ticket).is_some() {
            return false;
        }
        let key = if ticket.is_empty() { "open" } else { ticket };
        let n = self
            .questions
            .iter()
            .filter(|q| q.ticket == ticket)
            .count()
            .saturating_add(1);
        self.questions.push(AgentQuestion {
            id: format!("{key}#{n}"),
            ticket: ticket.to_owned(),
            from: from.to_owned(),
            to: to.to_owned(),
            body: body.chars().take(600).collect(),
            answer: String::new(),
            asked_at: now_rfc3339(),
            answered_at: String::new(),
            forwarded: false,
            escalated: false,
            deferred: false,
        });
        // Keep the log bounded; answered questions age out before open ones.
        while self.questions.len() > 40 {
            if let Some(i) = self.questions.iter().position(|q| !q.is_open()) {
                self.questions.remove(i);
            } else {
                self.questions.remove(0);
            }
        }
        true
    }

    /// The open question for `ticket`, if any.
    #[must_use]
    pub fn open_question(&self, ticket: &str) -> Option<&AgentQuestion> {
        self.questions
            .iter()
            .find(|q| q.ticket == ticket && q.is_open())
    }

    /// Hand a question to the other role, once. The BA that lacks the code and
    /// the SA that lacks the ticket are each one hop from someone who has it;
    /// a second hop is a loop, so this refuses it.
    pub fn forward_question(&mut self, id: &str, to: &str) -> bool {
        let Some(q) = self.questions.iter_mut().find(|q| q.id == id) else {
            return false;
        };
        if q.forwarded || q.to == to || !q.is_open() {
            return false;
        }
        to.clone_into(&mut q.to);
        q.forwarded = true;
        true
    }

    /// Attach an answer to a question. Returns whether it landed.
    pub fn answer_question(&mut self, id: &str, answer: &str) -> bool {
        let answer = answer.trim();
        if answer.is_empty() {
            return false;
        }
        let Some(q) = self.questions.iter_mut().find(|q| q.id == id) else {
            return false;
        };
        q.answer = answer.chars().take(1500).collect();
        q.answered_at = now_rfc3339();
        true
    }

    /// Answered questions about `ticket`, newest last.
    #[must_use]
    pub fn answered_questions(&self, ticket: &str) -> Vec<&AgentQuestion> {
        self.questions
            .iter()
            .filter(|q| q.ticket == ticket && !q.is_open())
            .collect()
    }

    /// Structured failures recorded for `ticket`, oldest first.
    #[must_use]
    pub fn attempt_failures(&self, ticket: &str) -> &[AttemptFailure] {
        self.ticket_failures
            .get(ticket)
            .map_or(&[][..], Vec::as_slice)
    }

    pub fn journal_note(&mut self, ticket: &str, note: &str) {
        let entry: String = note.trim().chars().take(500).collect();
        if entry.is_empty() {
            return;
        }
        let notes = self.ticket_journal.entry(ticket.to_owned()).or_default();
        notes.push(entry);
        let overflow = notes.len().saturating_sub(4);
        if overflow > 0 {
            notes.drain(0..overflow);
        }
    }

    /// Add `usd` to today's spend, rolling the counter over when the UTC date
    /// changes. Returns the new same-day total.
    pub fn add_daily_spend(&mut self, usd: f64) -> f64 {
        let today = now_rfc3339().get(..10).unwrap_or_default().to_owned();
        if self.spend_day != today {
            self.spend_day = today;
            self.spend_today_usd = 0.0;
            // A new day resets the cap itself, so a stale "already warned"
            // flag must not suppress a fresh warning today.
            self.budget_warned_daily = false;
        }
        self.spend_today_usd += usd;
        self.spend_today_usd
    }

    /// Post a comment to a discussion thread, trimming to [`MAX_COMMENTS`].
    pub fn post_comment(&mut self, author: &str, body: &str, ticket: Option<String>) {
        self.post_comment_att(author, body, ticket, Vec::new());
    }

    /// Post an agent comment attributed to the worker identity (`operator@host`)
    /// that produced it, so parallel runners are distinguishable.
    pub fn post_comment_by(&mut self, author: &str, by: &str, body: &str, ticket: Option<String>) {
        self.comments.push(Comment {
            id: mint_id(),
            at: now_rfc3339(),
            author: author.to_owned(),
            by: by.to_owned(),
            body: body.to_owned(),
            ticket,
            attachments: Vec::new(),
            reactions: Vec::new(),
        });
        let overflow = self.comments.len().saturating_sub(MAX_COMMENTS);
        if overflow > 0 {
            self.comments.drain(0..overflow);
        }
    }

    /// Post a comment with attachments.
    pub fn post_comment_att(
        &mut self,
        author: &str,
        body: &str,
        ticket: Option<String>,
        attachments: Vec<Attachment>,
    ) {
        self.comments.push(Comment {
            id: mint_id(),
            at: now_rfc3339(),
            author: author.to_owned(),
            by: String::new(),
            body: body.to_owned(),
            ticket,
            attachments,
            reactions: Vec::new(),
        });
        let overflow = self.comments.len().saturating_sub(MAX_COMMENTS);
        if overflow > 0 {
            self.comments.drain(0..overflow);
        }
    }

    /// Record (or replace) the SA agent's latest review verdict for a PR.
    pub fn upsert_review(&mut self, number: u64, decision: &str, summary: &str, head_sha: &str) {
        // PR-open → verdict latency, from the mirrored open-PR record: the
        // dispatch model's headline health metric. First verdict wins — a
        // re-review of a moved head measures the fix loop, not dispatch.
        let latency_secs = self
            .open_prs
            .iter()
            .find(|p| p.number == number)
            .and_then(|p| {
                let fmt = &time::format_description::well_known::Rfc3339;
                let created = time::OffsetDateTime::parse(&p.created, fmt).ok()?;
                let secs = (time::OffsetDateTime::now_utc() - created).whole_seconds();
                u64::try_from(secs).ok()
            })
            .or_else(|| {
                self.reviews
                    .iter()
                    .find(|r| r.number == number)
                    .and_then(|r| r.latency_secs)
            });
        let review = PrReview {
            number,
            decision: decision.to_owned(),
            summary: summary.to_owned(),
            at: now_rfc3339(),
            head_sha: head_sha.to_owned(),
            latency_secs,
        };
        if let Some(r) = self.reviews.iter_mut().find(|r| r.number == number) {
            let first_latency = r.latency_secs.or(review.latency_secs);
            *r = review;
            r.latency_secs = first_latency;
        } else {
            self.reviews.push(review);
        }
        // Keep the list bounded to recent PRs.
        let overflow = self.reviews.len().saturating_sub(50);
        if overflow > 0 {
            self.reviews.drain(0..overflow);
        }
    }

    /// Insert or replace a reported open PR, keeping the list to recent entries.
    pub fn upsert_open_pr(&mut self, pr: crate::ports::outbound::PrOpen) {
        if let Some(existing) = self.open_prs.iter_mut().find(|p| p.number == pr.number) {
            *existing = pr;
        } else {
            self.open_prs.push(pr);
        }
        let overflow = self.open_prs.len().saturating_sub(50);
        if overflow > 0 {
            self.open_prs.drain(0..overflow);
        }
    }

    /// Replace the whole reported open-PR list (e.g. a runner refresh).
    pub fn set_open_prs(&mut self, prs: Vec<crate::ports::outbound::PrOpen>) {
        let mut v = prs;
        v.truncate(50);
        self.open_prs = v;
        // A hold on a PR that is no longer open is stale — merged or closed
        // elsewhere; prune so the Inbox never asks about a decided PR.
        self.human_holds
            .retain(|n, _| self.open_prs.iter().any(|p| p.number == *n));
    }

    /// Toggle `user`'s `emoji` reaction on comment `id`; returns the updated
    /// comment (or `None` if no such comment).
    pub fn react_comment(&mut self, id: &str, user: &str, emoji: &str) -> Option<Comment> {
        let c = self.comments.iter_mut().find(|c| c.id == id)?;
        if let Some(r) = c.reactions.iter_mut().find(|r| r.emoji == emoji) {
            if let Some(pos) = r.users.iter().position(|u| u == user) {
                r.users.remove(pos);
            } else {
                r.users.push(user.to_owned());
            }
        } else {
            c.reactions.push(Reaction {
                emoji: emoji.to_owned(),
                users: vec![user.to_owned()],
            });
        }
        c.reactions.retain(|r| !r.users.is_empty());
        Some(c.clone())
    }

    /// Append a `#general` team-chat message from `user`.
    pub fn post_chat(&mut self, user: &str, body: &str) {
        self.post_chat_att(user, body, Vec::new());
    }

    /// Append a `#general` message with attachments.
    pub fn post_chat_att(&mut self, user: &str, body: &str, attachments: Vec<Attachment>) {
        self.post_chat_in(user, body, GENERAL_CHANNEL, attachments);
    }

    /// Messages in one room of this project's chat, oldest first.
    #[must_use]
    pub fn chat_in(&self, channel: &str) -> Vec<ChatMsg> {
        self.chat
            .iter()
            .filter(|m| m.channel == channel)
            .cloned()
            .collect()
    }

    /// Append a message to `channel`, trimming the oldest beyond [`MAX_CHAT`].
    pub fn post_chat_in(
        &mut self,
        user: &str,
        body: &str,
        channel: &str,
        attachments: Vec<Attachment>,
    ) {
        self.chat.push(ChatMsg {
            id: mint_id(),
            at: now_rfc3339(),
            user: user.to_owned(),
            body: body.to_owned(),
            edited: None,
            channel: channel.to_owned(),
            attachments,
            reactions: Vec::new(),
            thread_id: None,
            reply_count: 0,
            deleted: false,
        });
        let overflow = self.chat.len().saturating_sub(MAX_CHAT);
        if overflow > 0 {
            self.chat.drain(0..overflow);
        }
    }

    /// Create or update a documentation page. Matches on `id`; a new page is
    /// appended. Returns the stored page.
    pub fn upsert_doc(
        &mut self,
        id: &str,
        folder: &str,
        category: &str,
        title: &str,
        body: &str,
        author: &str,
    ) -> DocPage {
        let now = now_rfc3339();
        if let Some(p) = self.docs.iter_mut().find(|d| d.id == id) {
            folder.clone_into(&mut p.folder);
            category.clone_into(&mut p.category);
            title.clone_into(&mut p.title);
            body.clone_into(&mut p.body);
            p.updated_at = now;
            author.clone_into(&mut p.updated_by);
            return p.clone();
        }
        let page = DocPage {
            id: if id.is_empty() {
                mint_id()
            } else {
                id.to_owned()
            },
            folder: folder.to_owned(),
            category: category.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            updated_at: now,
            updated_by: author.to_owned(),
        };
        self.docs.push(page.clone());
        page
    }

    /// Number of open (not-done) architecture refactor chores (title starts with
    /// `Refactor:`). Drives entering/leaving the refactor sprint.
    #[must_use]
    pub fn open_refactor_count(&self) -> usize {
        self.tickets
            .iter()
            .filter(|t| {
                t.title().starts_with("Refactor:")
                    && !matches!(
                        t.status(),
                        coxagent_domain::Status::Done | coxagent_domain::Status::Documented
                    )
            })
            .count()
    }

    /// Record a retro lesson (deduped, newest last, capped at 12).
    pub fn add_lesson(&mut self, lesson: &str) {
        let lesson = lesson.trim();
        if lesson.is_empty() || self.lessons.iter().any(|l| l == lesson) {
            return;
        }
        self.lessons.push(lesson.to_owned());
        let overflow = self.lessons.len().saturating_sub(12);
        if overflow > 0 {
            self.lessons.drain(0..overflow);
        }
    }

    /// Record a durable team decision / convention (deduped, newest last, capped
    /// at 20). Trimmed to one line so it reads as an ADR entry.
    pub fn add_decision(&mut self, decision: &str) {
        let d = decision.trim().replace('\n', " ");
        let d = d.trim();
        if d.is_empty() || self.decisions.iter().any(|x| x == d) {
            return;
        }
        self.decisions.push(d.to_owned());
        let overflow = self.decisions.len().saturating_sub(20);
        if overflow > 0 {
            self.decisions.drain(0..overflow);
        }
    }

    /// Look up a documentation page by id.
    #[must_use]
    pub fn doc(&self, id: &str) -> Option<DocPage> {
        self.docs.iter().find(|d| d.id == id).cloned()
    }

    /// Remove a documentation page by id. Returns whether one was removed.
    pub fn remove_doc(&mut self, id: &str) -> bool {
        let before = self.docs.len();
        self.docs.retain(|d| d.id != id);
        self.docs.len() != before
    }

    /// Ensure the standard, role-owned Wiki spaces exist (idempotent), so the
    /// knowledge base has a sensible Confluence-like structure from the start
    /// rather than everything dumped into one folder. See [`STANDARD_DOC_FOLDERS`].
    pub fn ensure_standard_folders(&mut self) {
        for f in STANDARD_DOC_FOLDERS {
            self.add_doc_folder(f);
        }
    }

    /// Re-file docs backed by a ticket into the folder that matches the ticket's
    /// type, correcting legacy pages that were all dumped under "Features" (a
    /// merge chore is not a feature — it belongs in Engineering). Pages ids are
    /// `feat-<TICKET>`; unknown tickets are left where they are.
    pub fn normalize_doc_folders(&mut self) {
        // Snapshot the ticket type for each documented ticket first (avoids a
        // borrow conflict with the mutable docs iteration below).
        let routes: Vec<(String, String)> = self
            .docs
            .iter()
            .filter_map(|d| {
                let raw = d.id.strip_prefix("feat-")?;
                let tid = TicketId::new(raw).ok()?;
                let t = self.tickets.iter().find(|t| t.id() == &tid)?;
                Some((
                    d.id.clone(),
                    standard_doc_folder(t.ticket_type()).to_owned(),
                ))
            })
            .collect();
        for (id, folder) in routes {
            if let Some(p) = self.docs.iter_mut().find(|d| d.id == id) {
                if p.folder != folder {
                    p.folder = folder;
                }
            }
        }
    }

    /// Create a Wiki folder path (idempotent). Nested paths are `/`-separated.
    pub fn add_doc_folder(&mut self, path: &str) {
        let path = path.trim().trim_matches('/');
        if !path.is_empty() && !self.doc_folders.iter().any(|f| f == path) {
            self.doc_folders.push(path.to_owned());
        }
    }

    /// Delete a Wiki folder and everything under it — its subfolders and every
    /// page whose folder is at or below the path (Confluence: deleting a space
    /// takes its pages). Returns the ids of deleted pages.
    pub fn remove_doc_folder(&mut self, path: &str) -> Vec<String> {
        let path = path.trim().trim_matches('/').to_owned();
        if path.is_empty() {
            return Vec::new();
        }
        let prefix = format!("{path}/");
        let under = |f: &str| f == path || f.starts_with(&prefix);
        self.doc_folders.retain(|f| !under(f));
        let removed: Vec<String> = self
            .docs
            .iter()
            .filter(|d| under(&d.folder))
            .map(|d| d.id.clone())
            .collect();
        self.docs.retain(|d| !under(&d.folder));
        removed
    }

    /// Move a page to another folder path. Returns whether the page exists.
    pub fn move_doc(&mut self, id: &str, folder: &str) -> bool {
        let folder = folder.trim().trim_matches('/').to_owned();
        if let Some(p) = self.docs.iter_mut().find(|d| d.id == id) {
            p.folder = folder;
            p.updated_at = now_rfc3339();
            true
        } else {
            false
        }
    }

    /// Look up a channel by id (`#general` is synthesised on demand).
    #[must_use]
    pub fn channel(&self, id: &str) -> Option<Channel> {
        if id == GENERAL_CHANNEL {
            return Some(general_channel_record());
        }
        if id == AGENTS_CHANNEL {
            return Some(agents_channel_record());
        }
        self.channels.iter().find(|c| c.id == id).cloned()
    }

    /// All channels `user` can see: `#general` and `#agents` first, then their
    /// private ones.
    #[must_use]
    pub fn channels_for(&self, user: &str) -> Vec<Channel> {
        let mut out = vec![general_channel_record(), agents_channel_record()];
        out.extend(self.channels.iter().filter(|c| c.can_view(user)).cloned());
        out
    }

    /// Create a private channel named `name`, owned by `owner`. The owner is the
    /// first member. Returns the new channel, or an error string if the name is
    /// empty or collides with an existing channel.
    ///
    /// # Errors
    /// A human-readable message when the name is invalid or already taken.
    pub fn create_channel(&mut self, name: &str, owner: &str) -> Result<Channel, String> {
        self.create_channel_with_kind(name, owner, "private")
    }

    /// Create a channel with an explicit kind. Valid kinds: `"private"`, `"public"`.
    /// Create a sub-channel under `parent`: a room inside a room. It starts
    /// with the parent's members so the people already in the conversation can
    /// follow it without a second round of invites.
    ///
    /// # Errors
    /// The same reasons as [`Self::create_channel_with_kind`], plus an unknown
    /// parent.
    pub fn create_sub_channel(
        &mut self,
        parent_id: &str,
        name: &str,
        owner: &str,
        kind: &str,
    ) -> Result<Channel, String> {
        let Some(parent) = self.channels.iter().find(|c| c.id == parent_id).cloned() else {
            return Err(format!("no channel {parent_id}"));
        };
        if !parent.can_view(owner) {
            return Err("you are not in that channel".to_owned());
        }
        let ch = self.create_channel_with_kind(name, owner, kind)?;
        let id = ch.id.clone();
        let Some(created) = self.channels.iter_mut().find(|c| c.id == id) else {
            return Err("channel vanished".to_owned());
        };
        parent_id.clone_into(&mut created.parent);
        for m in &parent.members {
            if !created.members.iter().any(|x| x == m) {
                created.members.push(m.clone());
            }
        }
        Ok(created.clone())
    }

    /// Apply channel settings. `None` leaves a setting alone. Returns the
    /// updated channel.
    ///
    /// # Errors
    /// Unknown channel, or a privacy change on `#general`, which must stay the
    /// one room nobody can be shut out of.
    pub fn update_channel_settings(
        &mut self,
        id: &str,
        kind: Option<&str>,
        open_invite: Option<bool>,
        topic: Option<&str>,
    ) -> Result<Channel, String> {
        let Some(ch) = self.channels.iter_mut().find(|c| c.id == id) else {
            return Err(format!("no channel {id}"));
        };
        if let Some(k) = kind {
            if !ch.can_change_privacy() {
                return Err("#general cannot be made private".to_owned());
            }
            if !matches!(k, "private" | "public") {
                return Err(format!("unknown channel kind {k}"));
            }
            k.clone_into(&mut ch.kind);
        }
        if let Some(o) = open_invite {
            ch.open_invite = o;
        }
        if let Some(t) = topic {
            ch.topic = t.trim().chars().take(200).collect();
        }
        Ok(ch.clone())
    }

    /// Remove a member (and any invite delegation they held) from a channel.
    ///
    /// # Errors
    /// Unknown channel, or an attempt to remove its owner.
    pub fn remove_channel_member(&mut self, id: &str, user: &str) -> Result<Channel, String> {
        let Some(ch) = self.channels.iter_mut().find(|c| c.id == id) else {
            return Err(format!("no channel {id}"));
        };
        if ch.owner == user {
            return Err("the owner cannot be removed from their own channel".to_owned());
        }
        ch.members.retain(|m| m != user);
        ch.inviters.retain(|m| m != user);
        Ok(ch.clone())
    }

    /// Create a channel of an explicit `kind` (`"private"` or `"public"`) owned
    /// by `owner`, who becomes its first member.
    ///
    /// # Errors
    /// When `name` slugifies to nothing, the channel already exists, or `kind`
    /// is neither `"private"` nor `"public"`.
    pub fn create_channel_with_kind(
        &mut self,
        name: &str,
        owner: &str,
        kind: &str,
    ) -> Result<Channel, String> {
        let id = slugify(name);
        if id.is_empty() {
            return Err("channel name must contain letters or numbers".to_owned());
        }
        if id == GENERAL_CHANNEL || self.channels.iter().any(|c| c.id == id) {
            return Err(format!("channel #{id} already exists"));
        }
        if kind != "private" && kind != "public" {
            return Err("kind must be 'private' or 'public'".to_owned());
        }
        let ch = Channel {
            id,
            name: name.trim().to_owned(),
            owner: owner.to_owned(),
            members: vec![owner.to_owned()],
            inviters: Vec::new(),
            created_at: now_rfc3339(),
            kind: kind.to_owned(),
            parent: String::new(),
            open_invite: false,
            topic: String::new(),
            project: String::new(),
        };
        self.channels.push(ch.clone());
        Ok(ch)
    }

    /// Add `invitee` to `channel_id`. `actor` must be the owner or a delegated
    /// inviter. No-op if the invitee is already a member.
    ///
    /// # Errors
    /// When the channel doesn't exist or `actor` lacks permission.
    pub fn invite_to_channel(
        &mut self,
        channel_id: &str,
        actor: &str,
        invitee: &str,
    ) -> Result<(), String> {
        let ch = self
            .channels
            .iter_mut()
            .find(|c| c.id == channel_id)
            .ok_or("channel not found")?;
        if !ch.can_invite(actor) {
            return Err("you don't have permission to invite to this channel".to_owned());
        }
        let invitee = invitee.trim();
        if invitee.is_empty() {
            return Err("no user to invite".to_owned());
        }
        if ch.owner != invitee && !ch.members.iter().any(|m| m == invitee) {
            ch.members.push(invitee.to_owned());
        }
        Ok(())
    }

    /// Grant `grantee` invite permission on `channel_id`. Only the owner may
    /// delegate. Adds the grantee as a member too if they aren't one.
    ///
    /// # Errors
    /// When the channel doesn't exist or `actor` isn't the owner.
    pub fn delegate_invite(
        &mut self,
        channel_id: &str,
        actor: &str,
        grantee: &str,
    ) -> Result<(), String> {
        let ch = self
            .channels
            .iter_mut()
            .find(|c| c.id == channel_id)
            .ok_or("channel not found")?;
        if ch.owner != actor {
            return Err("only the channel owner can delegate invite permission".to_owned());
        }
        let grantee = grantee.trim();
        if grantee.is_empty() {
            return Err("no user to delegate to".to_owned());
        }
        if !ch.members.iter().any(|m| m == grantee) {
            ch.members.push(grantee.to_owned());
        }
        if !ch.inviters.iter().any(|m| m == grantee) {
            ch.inviters.push(grantee.to_owned());
        }
        Ok(())
    }
}

/// The synthetic record for the implicit `#general` channel.
fn agents_channel_record() -> Channel {
    Channel {
        id: AGENTS_CHANNEL.to_owned(),
        name: "agents".to_owned(),
        owner: String::new(),
        members: Vec::new(),
        inviters: Vec::new(),
        created_at: String::new(),
        kind: "general".to_owned(),
        parent: String::new(),
        open_invite: false,
        topic: String::new(),
        project: String::new(),
    }
}

fn general_channel_record() -> Channel {
    Channel {
        id: GENERAL_CHANNEL.to_owned(),
        name: "general".to_owned(),
        owner: String::new(),
        members: Vec::new(),
        inviters: Vec::new(),
        created_at: String::new(),
        kind: "general".to_owned(),
        parent: String::new(),
        open_invite: false,
        topic: String::new(),
        project: String::new(),
    }
}

/// Turn a display name into a URL-safe channel slug: lowercase, spaces and runs
/// of punctuation collapsed to single hyphens, trimmed. `"Design Review!"` →
/// `"design-review"`.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

pub fn mint_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}{seq:x}")
}

#[must_use]
pub fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Unix seconds now — the clock the outbox's retry deadlines and leases use.
#[must_use]
#[allow(clippy::cast_possible_wrap)] // seconds since UNIX_EPOCH fits i64 for a very long time
pub fn unix_now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Derive a short uppercase alias from a project name: its capital letters
/// (`CoXChat` -> `CXC`), else the first three alphanumerics uppercased.
#[must_use]
pub fn derive_alias(name: &str) -> String {
    let caps: String = name.chars().filter(char::is_ascii_uppercase).collect();
    if caps.len() >= 2 {
        return caps.chars().take(4).collect();
    }
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(3)
        .collect::<String>()
        .to_uppercase()
}

impl ProjectState {
    /// Find a ticket by id.
    #[must_use]
    pub fn ticket(&self, id: &TicketId) -> Option<&Ticket> {
        self.tickets.iter().find(|t| t.id() == id)
    }

    /// Find a ticket by id for mutation (guarded methods still apply).
    pub fn ticket_mut(&mut self, id: &TicketId) -> Option<&mut Ticket> {
        self.tickets.iter_mut().find(|t| t.id() == id)
    }

    /// Structural validation independent of transport: unique ids and every
    /// dependency referencing an existing ticket. Returns the offending detail.
    ///
    /// # Errors
    /// Returns a human-readable reason string when an invariant is violated.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for t in &self.tickets {
            if !seen.insert(t.id()) {
                return Err(format!("duplicate ticket id: {}", t.id()));
            }
        }
        for t in &self.tickets {
            for dep in t.depends_on() {
                if !seen.contains(dep) {
                    return Err(format!("ticket {} depends on unknown ticket {dep}", t.id()));
                }
            }
        }
        // A dependency cycle would deadlock the loop: no ticket in the cycle can
        // ever become workable because its dependencies never all reach `done`.
        if let Some(node) = self.first_dependency_cycle() {
            return Err(format!("dependency cycle involving ticket {node}"));
        }
        Ok(())
    }

    /// Detect a cycle in the `depends_on` graph, returning a node on the cycle.
    /// Iterative DFS with white/grey/black coloring; a grey→grey edge is a cycle.
    fn first_dependency_cycle(&self) -> Option<TicketId> {
        use std::collections::HashMap;
        #[derive(Clone, Copy, PartialEq)]
        enum Color {
            White,
            Grey,
            Black,
        }
        let mut color: HashMap<&TicketId, Color> = self
            .tickets
            .iter()
            .map(|t| (t.id(), Color::White))
            .collect();

        for start in self.tickets.iter().map(Ticket::id) {
            if color.get(start) != Some(&Color::White) {
                continue;
            }
            // Stack of (node, "entering" flag). Entering marks grey; on exit, black.
            let mut stack = vec![(start, false)];
            while let Some((node, exiting)) = stack.pop() {
                if exiting {
                    color.insert(node, Color::Black);
                    continue;
                }
                // A node can be queued more than once (shared dependency); only
                // enter it while still White.
                if color.get(node) != Some(&Color::White) {
                    continue;
                }
                color.insert(node, Color::Grey);
                stack.push((node, true));
                if let Some(t) = self.ticket(node) {
                    for dep in t.depends_on() {
                        match color.get(dep) {
                            Some(Color::Grey) => return Some(dep.clone()),
                            Some(Color::White) | None => stack.push((dep, false)),
                            Some(Color::Black) => {}
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod dependency_tests {
    use super::ProjectState;
    use coxagent_domain::{Complexity, Priority, Role, Ticket, TicketId, TicketType};

    fn feat(id: &str, deps: &[&str]) -> Ticket {
        let mut t = Ticket::new(
            TicketId::new(id).expect("id"),
            TicketType::Feature,
            "f",
            "",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .expect("ticket");
        for d in deps {
            t.add_dependency(Role::Sa, TicketId::new(*d).expect("dep"))
                .expect("dep");
        }
        t
    }

    #[test]
    fn acyclic_graph_validates() {
        let s = ProjectState {
            tickets: vec![
                feat("A-1", &["A-2"]),
                feat("A-2", &["A-3"]),
                feat("A-3", &[]),
            ],
            ..ProjectState::default()
        };
        assert!(s.validate().is_ok());
    }

    #[test]
    fn direct_cycle_is_rejected() {
        let s = ProjectState {
            tickets: vec![feat("A-1", &["A-2"]), feat("A-2", &["A-1"])],
            ..ProjectState::default()
        };
        assert!(s.validate().unwrap_err().contains("cycle"));
    }

    #[test]
    fn indirect_cycle_is_rejected() {
        let s = ProjectState {
            tickets: vec![
                feat("A-1", &["A-2"]),
                feat("A-2", &["A-3"]),
                feat("A-3", &["A-1"]),
            ],
            ..ProjectState::default()
        };
        assert!(s.validate().unwrap_err().contains("cycle"));
    }

    #[test]
    fn shared_dependency_is_not_a_cycle() {
        // A-1 and A-2 both depend on A-3 (diamond, no cycle).
        let s = ProjectState {
            tickets: vec![
                feat("A-1", &["A-3"]),
                feat("A-2", &["A-3"]),
                feat("A-3", &[]),
            ],
            ..ProjectState::default()
        };
        assert!(s.validate().is_ok());
    }
}

#[cfg(test)]
mod alias_tests {
    use super::derive_alias;

    #[test]
    fn derives_from_capitals() {
        assert_eq!(derive_alias("CoXChat"), "CXC");
        assert_eq!(derive_alias("CoXAgent"), "CXA");
    }

    #[test]
    fn falls_back_to_first_letters() {
        assert_eq!(derive_alias("quotes"), "QUO");
        assert_eq!(derive_alias("my app"), "MYA");
    }

    #[test]
    fn journal_note_bounded_and_capped() {
        let mut st = super::ProjectState::default();
        for i in 0..6 {
            st.journal_note("T-1", &format!("note {i} {}", "x".repeat(600)));
        }
        let notes = &st.ticket_journal["T-1"];
        assert_eq!(notes.len(), 4, "keeps only the last 4");
        assert!(notes[0].starts_with("note 2"), "oldest dropped");
        assert!(
            notes.iter().all(|n| n.chars().count() <= 500),
            "entries capped"
        );
        st.journal_note("T-1", "   ");
        assert_eq!(st.ticket_journal["T-1"].len(), 4, "blank notes ignored");
    }

    #[test]
    fn avg_role_cost_divides_by_runs() {
        let mut sp = super::Spend::default();
        assert!(sp.avg_role_cost("dev_feature").is_none());
        sp.by_role.insert("dev_feature".into(), 99.0); // historical total — ignored
        sp.metered_cost_by_role.insert("dev_feature".into(), 3.0);
        sp.runs_by_role.insert("dev_feature".into(), 4);
        let avg = sp.avg_role_cost("dev_feature").unwrap();
        assert!((avg - 0.75).abs() < 1e-9);
    }

    #[test]
    fn evidence_bounded_and_capped() {
        let mut st = super::ProjectState::default();
        for i in 0..8 {
            st.add_evidence("T-1", "api", &format!("proof {i}"), &"x".repeat(2000));
        }
        let ev = &st.ticket_evidence["T-1"];
        assert_eq!(ev.len(), 6, "keeps last 6");
        assert!(ev[0].label.contains("proof 2"), "oldest dropped");
        assert!(ev.iter().all(|e| e.detail.chars().count() <= 1200));
    }
}
