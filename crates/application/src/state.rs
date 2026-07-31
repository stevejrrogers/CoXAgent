//! `ProjectState` — the persisted aggregate the store loads and saves as a unit.
//!
//! Kept in the application layer because `schema_version` is a persistence
//! concern; the domain stays free of it.

use coxagent_domain::{SemVer, Ticket, TicketId};
use serde::{Deserialize, Serialize};

/// Current on-disk schema version. Bumped when the serialized shape changes;
/// the store refuses to silently load a newer version than it understands.
pub const SCHEMA_VERSION: u32 = 1;

/// One deployment: a version and the ticket that produced it. The changelog is
/// rendered from these — deterministic, zero-token, never out of sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecord {
    pub version: SemVer,
    pub ticket: TicketId,
    pub title: String,
    /// RFC3339 timestamp.
    pub at: String,
}

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

/// A file or image attached to a chat message or discussion comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Original filename shown to the user.
    pub name: String,
    /// Path to fetch it, e.g. `/api/projects/<pid>/media/<stored>`.
    pub url: String,
    /// MIME type (e.g. `image/png`), used to render images inline.
    pub mime: String,
    /// Size in bytes.
    pub size: u64,
}

/// One message on a discussion thread — an agent or the user commenting on a
/// ticket (`ticket = Some`) or on the team channel (`ticket = None`). This is
/// the teamwork surface the original workflow lacked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Stable id (minted on post) so reactions can target a specific comment.
    #[serde(default)]
    pub id: String,
    pub at: String,
    /// Author: an agent role (e.g. `SM`, `PO`) or `USER`.
    pub author: String,
    /// For an agent message, the worker identity (`operator@host`) that produced
    /// it — so you can tell whose DEV/SA/... posted, when several run in parallel.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// Files/images attached to the comment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Emoji reactions, each with the users who reacted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

/// Keep discussion threads bounded per project.
pub const MAX_COMMENTS: usize = 500;

/// One human-to-human message in the project's team chat channel. Unlike
/// [`Comment`] (which is dominated by agent scrum chatter), this is a plain
/// channel for the people on the project to talk to each other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMsg {
    /// Stable id (minted on post) so reactions can target a specific message.
    #[serde(default)]
    pub id: String,
    pub at: String,
    /// The authenticated username of the sender.
    pub user: String,
    pub body: String,
    /// Edited timestamp (set when message is edited)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited: Option<String>,
    /// The channel this message belongs to. Defaults to [`GENERAL_CHANNEL`] for
    /// messages written before channels existed.
    #[serde(default = "general_channel")]
    pub channel: String,
    /// Files/images attached to the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Emoji reactions, each with the users who reacted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
    /// Thread parent message id (absent for top-level messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Number of thread replies (populated for top-level messages)
    #[serde(default)]
    pub reply_count: u32,
    /// Whether this message was deleted (soft delete)
    #[serde(default)]
    pub deleted: bool,
}

/// One emoji reaction on a message and the users who added it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<String>,
}

impl ChatMsg {
    #[must_use]
    pub fn reply(user: &str, body: &str, channel: &str, thread_id: &str) -> Self {
        Self {
            id: mint_id(),
            at: now_rfc3339(),
            user: user.to_owned(),
            body: body.to_owned(),
            edited: None,
            channel: channel.to_owned(),
            attachments: Vec::new(),
            reactions: Vec::new(),
            thread_id: Some(thread_id.to_owned()),
            reply_count: 0,
            deleted: false,
        }
    }
}

/// Keep the team chat bounded per project.
pub const MAX_CHAT: usize = 500;

/// The id of the default channel every project has and everyone can see.
pub const GENERAL_CHANNEL: &str = "general";

/// The system feed channel: agent/bot notifications (PRs, deploys, digest,
/// previews) land here instead of spamming `#general`. Open to everyone.
pub const AGENTS_CHANNEL: &str = "agents";

fn general_channel() -> String {
    GENERAL_CHANNEL.to_owned()
}

/// A Slack-style chat channel. `#general` is implicit (open to everyone, no
/// owner); every other channel is private to its `members`, created and owned
/// by one person who may delegate invite rights to others via `inviters`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    /// URL-safe slug used as the stable id (e.g. `design-review`).
    pub id: String,
    /// Human display name.
    pub name: String,
    /// Username of the owner. Empty for the system `#general` channel.
    pub owner: String,
    /// Members who can see and post. Empty for `#general` (everyone).
    #[serde(default)]
    pub members: Vec<String>,
    /// Members the owner delegated invite permission to (owner always can).
    #[serde(default)]
    pub inviters: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    /// Channel kind: `"general"` (everyone), `"project"` (auto-mirrors a
    /// project's membership, id = `#<alias>`), or `"private"` (owner-created).
    #[serde(default = "chan_kind_private")]
    pub kind: String,
    /// For a `"project"` channel, the project id it mirrors. Empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub project: String,
    /// Optional channel topic/description
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// Parent channel id when this is a sub-channel, empty at the top level.
    /// A sub-channel is a room inside a room — same members by default, its own
    /// thread of conversation — so a project channel does not have to carry
    /// every side discussion.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
    /// Whether any member may invite others. Off by default: a private channel
    /// anyone can add people to is a privacy surprise, so this is the setting a
    /// team turns ON deliberately, not one they discover.
    #[serde(default)]
    pub open_invite: bool,
}

fn chan_kind_private() -> String {
    "private".to_owned()
}

impl Channel {
    /// The open, everyone-can-see `#general` channel.
    #[must_use]
    pub fn is_general(&self) -> bool {
        self.id == GENERAL_CHANNEL
    }

    /// An open system channel everyone can read (`#general`, `#agents`).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.id == GENERAL_CHANNEL || self.id == AGENTS_CHANNEL
    }

    /// Whether `user` may see and read this channel.
    #[must_use]
    pub fn can_view(&self, user: &str) -> bool {
        self.is_open() || self.owner == user || self.members.iter().any(|m| m == user)
    }

    /// Whether `user` may invite others. With `open_invite` any member can;
    /// otherwise it is the owner and whoever they delegated it to. Admins are
    /// handled above this, at the endpoint: their authority does not depend on
    /// a channel's settings.
    #[must_use]
    pub fn can_invite(&self, user: &str) -> bool {
        if self.is_open() {
            return true;
        }
        if self.owner == user || self.inviters.iter().any(|m| m == user) {
            return true;
        }
        self.open_invite && self.members.iter().any(|m| m == user)
    }

    /// Whether `user` may remove members. Never the whole membership — losing
    /// someone from a room is not something a room-mate should be able to do
    /// to another on a whim.
    #[must_use]
    pub fn can_kick(&self, user: &str) -> bool {
        !self.is_open() && (self.owner == user || self.inviters.iter().any(|m| m == user))
    }

    /// Whether this channel may be made private. `#general` may not: a team
    /// needs one room nobody can be shut out of.
    #[must_use]
    pub fn can_change_privacy(&self) -> bool {
        self.id != GENERAL_CHANNEL
    }
}

/// The project-level design system authored once by PD. Injected into DEV
/// prompts for UI tickets so implementation is visually consistent — the
/// design analogue of architecture governance (proactive, in-prompt).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignSystem {
    /// The overall design language / principles in prose.
    pub principles: String,
    /// Color tokens, e.g. `primary: cyan #0891B2`.
    pub palette: Vec<String>,
    /// Typography guidance (families, scale, weights).
    pub typography: String,
    /// Component conventions, e.g. `buttons: 8px radius, filled primary`.
    pub components: Vec<String>,
}

impl DesignSystem {
    /// Whether any field carries content (an authored system, not a blank one).
    #[must_use]
    pub fn is_populated(&self) -> bool {
        !self.principles.is_empty()
            || !self.palette.is_empty()
            || !self.typography.is_empty()
            || !self.components.is_empty()
    }
}

/// The standard, role-owned Wiki spaces — a Confluence-like structure so each
/// role's knowledge has an obvious home instead of everything landing in one
/// folder. Ordered as they should appear in the tree.
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

/// The standard Wiki space a ticket's documentation belongs in, by type: a
/// feature is product knowledge; a chore (merge/rebase/maintenance) or a bug fix
/// is engineering, not a feature.
#[must_use]
pub fn standard_doc_folder(ticket_type: coxagent_domain::TicketType) -> &'static str {
    use coxagent_domain::TicketType;
    match ticket_type {
        TicketType::Feature => "Product",
        TicketType::Chore | TicketType::Bug => "Engineering",
    }
}

/// A live engine-infrastructure problem: expired auth, a model the provider
/// rejected, a quota wall. These are not ticket failures and not the team's
/// fault, but they stop everything — so they are surfaced as an open incident
/// with the reason, the role that hit it, and when, and cleared the moment a
/// run of that engine succeeds again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineIncident {
    /// Engine id (`claude`, `opencode`).
    pub engine: String,
    /// The decisive line from the failure, already trimmed.
    pub reason: String,
    /// Role label that hit it first (`DEV-BUG`).
    pub role: String,
    pub since: String,
    /// How many runs have failed this way since `since`.
    pub hits: u32,
}

/// One agent asking another a question it must not guess the answer to.
///
/// A developer who cannot tell what the requirement means, or a BA who does
/// not know what the product already does, has exactly one correct move: ask
/// the person who knows. Without this the only options were to guess and fail
/// a gate, or stall — which is how a ticket burned three attempts on the same
/// misunderstanding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQuestion {
    /// Stable id (`<ticket>#<n>` or `open#<n>` when not about one ticket).
    pub id: String,
    /// Ticket the question is about; empty for a product-level question.
    pub ticket: String,
    /// Role label that asked (`DEV-BUG`, `BA`).
    pub from: String,
    /// Role label expected to answer (`BA`, `SA`).
    pub to: String,
    /// The question, as asked.
    pub body: String,
    /// The answer; empty while unanswered.
    #[serde(default)]
    pub answer: String,
    pub asked_at: String,
    #[serde(default)]
    pub answered_at: String,
    /// Whether this question has already been handed to the other role once.
    /// A second forward would be two roles passing it back and forth.
    #[serde(default)]
    pub forwarded: bool,
}

impl AgentQuestion {
    /// Whether this question is still waiting for an answer.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.answer.trim().is_empty()
    }
}

/// Which wiki space a ticket's page belongs in. The ticket TYPE alone gets
/// this wrong: an infrastructure feature (sandboxing, a deploy gate) is a
/// Feature ticket and would file under Product, which is how a product space
/// ends up holding nothing a product person would read. The subject decides,
/// with the type as the tie-breaker.
#[must_use]
pub fn doc_space_for(
    ticket_type: coxagent_domain::TicketType,
    title: &str,
    description: &str,
) -> &'static str {
    use coxagent_domain::TicketType;
    const ENGINEERING: &[&str] = &[
        "docker",
        "compose",
        "ci ",
        "pipeline",
        "clippy",
        "lint",
        "sandbox",
        "seatbelt",
        "bwrap",
        "deploy gate",
        "health check",
        "rollback",
        "refactor",
        "migration",
        "schema",
        "runner",
        "cargo",
        "build fails",
        "compile",
    ];
    let text = format!("{title} {description}").to_lowercase();
    if ENGINEERING.iter().any(|k| text.contains(k)) {
        return "Engineering";
    }
    match ticket_type {
        TicketType::Feature => "Product",
        TicketType::Chore | TicketType::Bug => "Engineering",
    }
}

/// The colour/category bucket for a Wiki folder, keyed off its top-level space.
/// Keeps DOCS-written pages consistent with the UI's folder colouring.
#[must_use]
pub fn doc_category_of(folder: &str) -> &'static str {
    match folder
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "architecture" | "technical" | "engineering" => "technical",
        "design" | "flows" => "flows",
        "qa" | "testing" | "test" | "tests" => "qa",
        "operations" | "ops" | "release notes" | "releases" => "ops",
        "product" | "features" => "product",
        // An unrecognised space is not silently "product": mislabelling a
        // team/ops page as product colours it wrongly in the wiki and skews
        // every filter built on the category.
        _ => "general",
    }
}

/// One living documentation page. `category` is `"product"` or `"technical"`;
/// `body` is Markdown. Pages are written by the DOCS agent and editable by
/// humans, and are structured so both people and agents can read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocPage {
    pub id: String,
    /// Folder path the page lives under, `/`-separated for nesting
    /// (e.g. `"Technical/Architecture"`). Empty = root.
    #[serde(default)]
    pub folder: String,
    /// Coarse colour bucket for the tag: `product`/`technical`/`flows`/`qa`/`ops`.
    pub category: String,
    pub title: String,
    /// Markdown body.
    pub body: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub updated_by: String,
}

/// Accumulated engine spend — the FinOps view of the autonomous team.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub total_cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub runs: u64,
    /// Cost attributed per agent role (e.g. `dev_feature`).
    #[serde(default)]
    pub by_role: std::collections::BTreeMap<String, f64>,
    /// Engine runs per role — divides `by_role` into an average cost per run,
    /// the basis of the pre-claim cost estimate for the approval gate.
    #[serde(default)]
    pub runs_by_role: std::collections::BTreeMap<String, u64>,
    /// Cost metered over the SAME window as `runs_by_role` (both started
    /// together) — `by_role` holds all-time totals from before run counting
    /// existed, so dividing THAT by runs inflates the estimate wildly.
    #[serde(default)]
    pub metered_cost_by_role: std::collections::BTreeMap<String, f64>,
    /// Usage attributed per operator (`account@host`) — the SaaS per-user view,
    /// so each user's token spend is measurable even though they share a project.
    #[serde(default)]
    pub by_operator: std::collections::BTreeMap<String, OperatorSpend>,
    /// Runs whose file writes were actually confined (Seatbelt/bwrap).
    #[serde(default)]
    pub confined_runs: u64,
    /// Runs where `workflow.sandbox` was on but confinement was unavailable on
    /// this host, so the run executed unconfined.
    #[serde(default)]
    pub unconfined_requested_runs: u64,
    /// Human-readable status of the most recent run's confinement (e.g.
    /// `"confined via bwrap"`, `"unavailable: bwrap not found on PATH"`),
    /// surfaced on the dashboard.
    #[serde(default)]
    pub last_sandbox_status: String,
}

impl Spend {
    /// Average observed cost of one engine run for `role_key` (e.g.
    /// `dev_feature`), or `None` before any metered run of that role.
    #[must_use]
    pub fn avg_role_cost(&self, role_key: &str) -> Option<f64> {
        let runs = *self.runs_by_role.get(role_key)?;
        if runs == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        Some(
            self.metered_cost_by_role
                .get(role_key)
                .copied()
                .unwrap_or(0.0)
                / runs as f64,
        )
    }
}

/// One operator's slice of the spend, for per-user token accounting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OperatorSpend {
    pub cost_usd: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub runs: u64,
}

/// The last deployment outcome, surfaced on the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployStatus {
    pub at: String,
    pub ok: bool,
    pub summary: String,
    /// The commit sha this deploy attempt built/ran (absent when git isn't
    /// wired up), so "what's currently live" is provable rather than assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    /// Result of the post-deploy health-endpoint probe for this attempt
    /// (COX-F005). `None` until the health check runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check: Option<HealthCheckResult>,
}

/// Outcome of a single health-endpoint probe (COX-F005): the app's health
/// endpoint on the deployed port, checked with a bounded timeout before a
/// deploy can be marked successful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheckResult {
    /// Whether the endpoint answered healthy within the bound.
    pub passed: bool,
    /// HTTP status code returned, if the endpoint was reachable at all.
    pub http_status: Option<u16>,
    /// Round-trip time of the probe, in milliseconds.
    pub response_time_ms: Option<u64>,
}

/// The most recent deploy that passed both `deploy()` and `run_tests()` —
/// auto-rollback's target. Backed by the durable `refs/coxagent/last-good`
/// git ref, which survives ticket-branch deletion after a squash-merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownGoodDeploy {
    pub sha: String,
    /// RFC3339 timestamp.
    pub at: String,
    /// Ordinal of this success — the Nth deploy to pass both gates.
    pub deploy_index: u64,
    pub summary: String,
}

/// Outcome of the most recent auto-rollback attempt, surfaced on the
/// dashboard distinctly from a normal deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackStatus {
    /// RFC3339 timestamp.
    pub at: String,
    /// What triggered it, e.g. `"deploy failed"` / `"tests failed"`.
    pub reason: String,
    /// The commit sha rolled back to.
    pub to_sha: String,
    pub ok: bool,
    pub summary: String,
    /// The known-good deploy was older than `max_rollback_age_secs` — rollback
    /// was skipped (not attempted), not just unlucky.
    #[serde(default)]
    pub stale: bool,
    /// Code touching `migration_detection_paths` shipped since the known-good
    /// deploy — rolling the app back without the DB schema could be unsafe,
    /// so rollback was skipped (not attempted).
    #[serde(default)]
    pub migration_blocked: bool,
}

/// One queued execution job (control plane → runner). The hub NEVER executes
/// these itself when a live runner exists — execution stays on the execution
/// plane. Claim-and-remove is atomic via `mutate_state`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingJob {
    pub id: String,
    /// "force_merge" today; the kind namespace is owned by `contracts::JobSpec`.
    pub kind: String,
    #[serde(default)]
    pub args: serde_json::Value,
    pub queued_at: String,
    #[serde(default)]
    pub queued_by: String,
}

/// A sprint (scrum mode): a fixed window of cycles with a goal and a committed
/// set of tickets. Kanban mode leaves this `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sprint {
    pub number: u32,
    pub goal: String,
    pub started_cycle: u64,
    pub length_cycles: u64,
    pub committed: Vec<TicketId>,
}

/// A closed sprint's outcome — the velocity history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SprintRecord {
    pub number: u32,
    pub goal: String,
    pub committed: usize,
    pub done: usize,
    pub at: String,
}

/// The SA agent's latest review verdict on an open pull request — surfaced in
/// the Review tab so the user sees the assessment before merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReview {
    pub number: u64,
    /// `"approve"` or `"request_changes"`.
    pub decision: String,
    pub summary: String,
    pub at: String,
}

/// A product milestone — a named delivery target that one or more sprints work
/// toward. `target_version` is the release that marks it reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Milestone {
    pub name: String,
    pub goal: String,
    /// Release version that completes this milestone, e.g. "0.5.0".
    pub target_version: String,
}

/// Why one attempt at a ticket failed, in a form later agents can reason over
/// instead of pattern-matching prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptFailure {
    /// 1-based attempt number.
    pub attempt: u32,
    /// Which layer the work died at.
    pub layer: FailureLayer,
    /// The gate or step that rejected it (`clippy`, `regression-test`,
    /// `tests`, `engine`), for routing and for the human digest.
    pub gate: String,
    /// The decisive detail, already trimmed (a lint line, an assertion).
    pub detail: String,
    /// Repo-relative files implicated, when the gate knows them.
    #[serde(default)]
    pub files: Vec<String>,
}

/// The layer an attempt died at — the thing that decides WHO can unstick it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureLayer {
    /// The requirement could not be built against (BA's problem).
    Spec,
    /// A mechanical quality gate rejected otherwise-sound work.
    Gate,
    /// The approach itself does not work (SA's problem).
    Design,
    /// Auth, network, capacity — nobody's fault, never counted.
    Infra,
}

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
    #[serde(default)]
    pub activity: Vec<ActivityEntry>,
    #[serde(default)]
    pub spend: Spend,
    #[serde(default)]
    pub sprint: Option<Sprint>,
    #[serde(default)]
    pub sprints: Vec<SprintRecord>,
    #[serde(default)]
    pub deploy: Option<DeployStatus>,
    /// Discussion threads: per-ticket and team-channel comments.
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// The SA agent's latest review verdict per open PR (by number).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviews: Vec<PrReview>,
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
    /// Living documentation pages (product + technical) written by agents/humans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<DocPage>,
    /// Explicit Wiki folder paths (`/`-separated, nested), so a folder can exist
    /// and nest even before it holds a page — Confluence-style spaces/pages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doc_folders: Vec<String>,
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
    /// Engine conversation id of the last fix run per PR — the next fix round
    /// RESUMES that conversation (the agent still has the branch, the feedback
    /// and its own changes in context) instead of starting cold. Dropped with
    /// `pr_fix_attempts` when the PR closes.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub pr_sessions: std::collections::BTreeMap<u64, String>,
    /// Tickets held for HUMAN cost approval: estimated run cost exceeded
    /// `workflow.approve_over_usd`. Value = the estimate shown to the human.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub cost_holds: std::collections::BTreeMap<String, f64>,
    /// Lint (clippy) error baseline: a DEV change may never ADD errors; an
    /// improvement lowers the bar for everyone after. `None` until first
    /// measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clippy_baseline: Option<u64>,
    /// Merged PR numbers already synced into ticket state (human merges on
    /// the forge must reflect back exactly once).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub seen_merged_prs: std::collections::BTreeSet<u64>,
    /// Closed-without-merge PR numbers already processed into lessons, so a
    /// human rejection is learned from exactly once.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub seen_closed_prs: std::collections::BTreeSet<u64>,
    /// Tickets a human approved to run despite the cost estimate.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub cost_approved: std::collections::BTreeSet<String>,
    /// Self-tuning knobs the orchestrator sets FROM the evals — the loop
    /// reacts to its own health instead of waiting for a human to read a
    /// dashboard. All deterministic; SM announces every change.
    #[serde(default, skip_serializing_if = "Tuning::is_default")]
    pub tuning: Tuning,
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
}

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
            activity: Vec::new(),
            spend: Spend::default(),
            sprint: None,
            sprints: Vec::new(),
            deploy: None,
            comments: Vec::new(),
            reviews: Vec::new(),
            chat: Vec::new(),
            channels: Vec::new(),
            design_system: None,
            milestones: Vec::new(),
            docs: Vec::new(),
            doc_folders: Vec::new(),
            lessons: Vec::new(),
            decisions: Vec::new(),
            refactor_mode: false,
            sprint_cycle: 0,
            sprint_goal: String::new(),
            last_digest_day: String::new(),
            pr_fix_attempts: std::collections::BTreeMap::new(),
            pr_sessions: std::collections::BTreeMap::new(),
            cost_holds: std::collections::BTreeMap::new(),
            clippy_baseline: None,
            seen_merged_prs: std::collections::BTreeSet::new(),
            seen_closed_prs: std::collections::BTreeSet::new(),
            cost_approved: std::collections::BTreeSet::new(),
            tuning: Tuning::default(),
            ticket_evidence: std::collections::BTreeMap::new(),
            drain_notice_sprint: 0,
            ticket_fail_attempts: std::collections::BTreeMap::new(),
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
        }
    }
}

/// One piece of Definition-of-Done evidence attached to a ticket: proof the
/// change actually works in its own context (UI → a real screenshot; API → a
/// real request/response; or an explicit waiver when the host can't collect).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// `screenshot` | `api` | `test` | `waived`
    pub kind: String,
    pub label: String,
    /// Screenshot: repo-relative path. API: capped request/response text.
    pub detail: String,
    pub at: String,
}

/// Orchestrator self-tuning state, derived from the evals each day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tuning {
    /// Quality brake: retry churn per shipped ticket ran hot, so DEV-FEATURE
    /// pauses and the team burns down bugs until churn recovers.
    #[serde(default)]
    pub bugs_first: bool,
    /// Intake brake: the backlog outgrew throughput, so BA proposals pause
    /// until the queue drains.
    #[serde(default)]
    pub skip_ba: bool,
    /// The day (`YYYY-MM-DD`) tuning was last evaluated.
    #[serde(default)]
    pub last_eval_day: String,
}

impl Tuning {
    #[must_use]
    pub fn is_default(&self) -> bool {
        !self.bugs_first && !self.skip_ba && self.last_eval_day.is_empty()
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
    pub fn upsert_review(&mut self, number: u64, decision: &str, summary: &str) {
        let review = PrReview {
            number,
            decision: decision.to_owned(),
            summary: summary.to_owned(),
            at: now_rfc3339(),
        };
        if let Some(r) = self.reviews.iter_mut().find(|r| r.number == number) {
            *r = review;
        } else {
            self.reviews.push(review);
        }
        // Keep the list bounded to recent PRs.
        let overflow = self.reviews.len().saturating_sub(50);
        if overflow > 0 {
            self.reviews.drain(0..overflow);
        }
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

#[cfg(test)]
mod channel_tests {
    use super::{slugify, ProjectState, AGENTS_CHANNEL, GENERAL_CHANNEL};

    #[test]
    fn slugify_makes_safe_ids() {
        assert_eq!(slugify("Design Review!"), "design-review");
        assert_eq!(slugify("  Q4   Planning  "), "q4-planning");
        assert_eq!(slugify("###"), "");
    }

    #[test]
    fn create_and_view_permissions() {
        let mut s = ProjectState::default();
        let ch = s.create_channel("Design Review", "alice").expect("create");
        assert_eq!(ch.id, "design-review");
        assert!(ch.can_view("alice"));
        assert!(!ch.can_view("bob"));
        // general + agents are always visible; alice additionally sees hers.
        assert_eq!(s.channels_for("alice").len(), 3);
        assert_eq!(s.channels_for("bob").len(), 2);
        assert_eq!(s.channels_for("bob")[0].id, GENERAL_CHANNEL);
        assert_eq!(s.channels_for("bob")[1].id, AGENTS_CHANNEL);
    }

    #[test]
    fn duplicate_channel_rejected() {
        let mut s = ProjectState::default();
        s.create_channel("Design", "alice").expect("first");
        assert!(s.create_channel("design", "bob").is_err());
        assert!(s.create_channel("general", "bob").is_err());
    }

    #[test]
    fn invite_requires_permission_then_grants_view() {
        let mut s = ProjectState::default();
        s.create_channel("Secret", "alice").expect("create");
        // bob can't invite; alice can.
        assert!(s.invite_to_channel("secret", "bob", "carol").is_err());
        s.invite_to_channel("secret", "alice", "bob")
            .expect("invite");
        assert!(s.channel("secret").expect("ch").can_view("bob"));
        // bob still can't invite (not delegated).
        assert!(s.invite_to_channel("secret", "bob", "carol").is_err());
    }

    #[test]
    fn delegation_lets_grantee_invite() {
        let mut s = ProjectState::default();
        s.create_channel("Secret", "alice").expect("create");
        assert!(s.delegate_invite("secret", "bob", "carol").is_err());
        s.delegate_invite("secret", "alice", "bob")
            .expect("delegate");
        s.invite_to_channel("secret", "bob", "carol")
            .expect("bob invites");
        assert!(s.channel("secret").expect("ch").can_view("carol"));
    }
}

/// A short, collision-free message id (nanos + a process-local counter).
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
mod comment_tests {
    use super::{ProjectState, MAX_COMMENTS};

    #[test]
    fn posts_and_bounds_the_thread() {
        let mut s = ProjectState::default();
        s.post_comment("SM", "hello", None);
        s.post_comment("USER", "hi", Some("CXC-F001".to_owned()));
        assert_eq!(s.comments.len(), 2);
        assert_eq!(s.comments[0].author, "SM");
        assert_eq!(s.comments[1].ticket.as_deref(), Some("CXC-F001"));
        for i in 0..MAX_COMMENTS + 10 {
            s.post_comment("BA", &format!("m{i}"), None);
        }
        assert_eq!(s.comments.len(), MAX_COMMENTS);
        // Oldest were dropped; the very latest survives.
        assert_eq!(
            s.comments.last().unwrap().body,
            format!("m{}", MAX_COMMENTS + 9)
        );
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

#[cfg(test)]
mod attempt_failure_tests {
    use super::{AttemptFailure, FailureLayer, ProjectState};

    fn failure(attempt: u32, gate: &str) -> AttemptFailure {
        AttemptFailure {
            attempt,
            layer: FailureLayer::Gate,
            gate: gate.to_owned(),
            detail: "d".to_owned(),
            files: vec!["crates/app/src/lib.rs".to_owned()],
        }
    }

    #[test]
    fn failures_are_kept_per_ticket_and_bounded() {
        let mut s = ProjectState::default();
        for n in 1..=9 {
            s.record_attempt_failure("COX-B006", failure(n, "clippy"));
        }
        s.record_attempt_failure("COX-B007", failure(1, "tests"));
        let log = s.attempt_failures("COX-B006");
        assert_eq!(log.len(), 6, "old attempts age out");
        assert_eq!(log[0].attempt, 4, "the oldest kept is the 4th");
        assert_eq!(s.attempt_failures("COX-B007").len(), 1);
        assert!(s.attempt_failures("COX-NONE").is_empty());
    }

    #[test]
    fn state_written_before_this_field_existed_still_loads() {
        // Projects on disk predate the structured log; a missing key must not
        // fail the load and strand a whole project.
        let mut doc = serde_json::to_value(ProjectState::default()).expect("serialize");
        doc.as_object_mut()
            .expect("object")
            .remove("ticket_failures");
        let back: ProjectState = serde_json::from_value(doc).expect("load legacy state");
        assert!(back.ticket_failures.is_empty());
    }
}

#[cfg(test)]
mod question_tests {
    use super::ProjectState;

    #[test]
    fn one_open_question_per_ticket_and_answers_are_readable_back() {
        let mut s = ProjectState::default();
        assert!(s.ask_question("COX-B1", "DEV-BUG", "BA", "what does archive mean?"));
        // A second unanswered question means the first was not the blocker.
        assert!(
            !s.ask_question("COX-B1", "DEV-BUG", "BA", "and what about purge?"),
            "a ticket may hold only one open question"
        );
        // A different ticket is unaffected.
        assert!(s.ask_question("COX-B2", "BA", "SA", "does the product already export?"));
        let open = s.open_question("COX-B1").expect("open");
        assert_eq!((open.from.as_str(), open.to.as_str()), ("DEV-BUG", "BA"));
        assert!(s.answered_questions("COX-B1").is_empty());

        let id = open.id.clone();
        assert!(s.answer_question(&id, "  soft-delete: the row stays, hidden  "));
        assert!(
            !s.answer_question(&id, "   "),
            "a blank answer is no answer"
        );
        assert!(s.open_question("COX-B1").is_none(), "no longer waiting");
        let answered = s.answered_questions("COX-B1");
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0].answer, "soft-delete: the row stays, hidden");
        // Asking again is allowed once the first is answered.
        assert!(s.ask_question("COX-B1", "DEV-BUG", "BA", "and what about purge?"));
    }

    #[test]
    fn a_question_may_be_handed_over_once_then_must_be_answered() {
        let mut s = ProjectState::default();
        assert!(s.ask_question("COX-B1", "DEV-BUG", "BA", "is archive a soft delete?"));
        let id = s.open_question("COX-B1").expect("open").id.clone();
        // The BA reads it as a systems question and hands it to the SA.
        assert!(s.forward_question(&id, "SA"));
        assert_eq!(s.open_question("COX-B1").expect("open").to, "SA");
        // A second hand-off would be the two roles passing it back and forth.
        assert!(
            !s.forward_question(&id, "BA"),
            "one hop only — after that someone has to read the code and answer"
        );
        // Handing it to the role that already holds it is not a hand-off.
        assert!(!s.forward_question(&id, "SA"));
        assert!(s.answer_question(&id, "soft delete; rows stay, hidden by a flag"));
        assert!(
            !s.forward_question(&id, "BA"),
            "answered questions do not move"
        );
    }

    #[test]
    fn empty_questions_are_not_recorded() {
        let mut s = ProjectState::default();
        assert!(!s.ask_question("COX-B1", "DEV-BUG", "BA", "   "));
        assert!(s.questions.is_empty());
    }
}

#[cfg(test)]
mod engine_incident_tests {
    use super::ProjectState;

    #[test]
    fn an_outage_is_raised_once_counted_and_closes_with_its_history() {
        let mut s = ProjectState::default();
        s.open_engine_incident("claude", "DevBug", "Failed to authenticate: OAuth expired");
        s.open_engine_incident("claude", "Test", "Failed to authenticate: OAuth expired");
        assert_eq!(s.engine_incidents.len(), 1, "one outage, not one per run");
        assert_eq!(s.engine_incidents[0].hits, 2);
        assert_eq!(s.engine_incidents[0].role, "DevBug", "who hit it first");

        // A second engine is its own incident.
        s.open_engine_incident("opencode", "DevFeature", "model not found");
        assert_eq!(s.engine_incidents.len(), 2);

        let closed = s.close_engine_incident("claude").expect("was open");
        assert_eq!(closed.hits, 2, "the caller can say how long it lasted");
        assert!(s.engine_incidents.iter().all(|i| i.engine != "claude"));
        assert!(
            s.close_engine_incident("claude").is_none(),
            "closing twice is not an event"
        );
    }
}

#[cfg(test)]
mod daily_job_tests {
    use super::ProjectState;

    #[test]
    fn a_daily_job_is_remembered_per_day_and_per_job() {
        let mut s = ProjectState::default();
        s.daily_jobs
            .insert("standup".to_owned(), "2026-07-29".to_owned());
        // Same job, same day: already done. Same day, other job: not.
        assert_eq!(
            s.daily_jobs.get("standup").map(String::as_str),
            Some("2026-07-29")
        );
        assert!(!s.daily_jobs.contains_key("po-milestones"));
        // A new day replaces the stamp rather than accumulating entries.
        s.daily_jobs
            .insert("standup".to_owned(), "2026-07-30".to_owned());
        assert_eq!(s.daily_jobs.len(), 1);
        assert_eq!(
            s.daily_jobs.get("standup").map(String::as_str),
            Some("2026-07-30")
        );
    }

    #[test]
    fn state_without_the_field_still_loads() {
        let mut doc = serde_json::to_value(ProjectState::default()).expect("serialize");
        doc.as_object_mut().expect("object").remove("daily_jobs");
        doc.as_object_mut()
            .expect("object")
            .remove("engine_incidents");
        let back: ProjectState = serde_json::from_value(doc).expect("legacy state loads");
        assert!(back.daily_jobs.is_empty() && back.engine_incidents.is_empty());
    }
}

#[cfg(test)]
mod channel_settings_tests {
    use super::{ProjectState, GENERAL_CHANNEL};

    #[test]
    fn a_sub_channel_inherits_the_room_it_was_opened_inside() {
        let mut s = ProjectState::default();
        let parent = s.create_channel("Design", "alice").expect("parent");
        s.invite_to_channel(&parent.id, "alice", "bob")
            .expect("invite");
        let sub = s
            .create_sub_channel(&parent.id, "Icons", "alice", "private")
            .expect("sub");
        assert_eq!(sub.parent, parent.id);
        assert!(
            sub.members.iter().any(|m| m == "bob"),
            "the people already in the conversation follow it without a second invite"
        );
        // Someone outside the parent cannot open a room inside it.
        assert!(s
            .create_sub_channel(&parent.id, "Nope", "mallory", "private")
            .is_err());
    }

    #[test]
    fn general_may_never_be_made_private() {
        let mut s = ProjectState::default();
        s.channels.push(super::Channel {
            id: GENERAL_CHANNEL.to_owned(),
            name: "general".to_owned(),
            owner: String::new(),
            members: Vec::new(),
            inviters: Vec::new(),
            created_at: String::new(),
            kind: "general".to_owned(),
            project: String::new(),
            topic: String::new(),
            parent: String::new(),
            open_invite: false,
        });
        let err = s
            .update_channel_settings(GENERAL_CHANNEL, Some("private"), None, None)
            .expect_err("must refuse");
        assert!(err.contains("#general"), "{err}");
        // Its other settings still move.
        assert!(s
            .update_channel_settings(GENERAL_CHANNEL, None, None, Some("say hi"))
            .is_ok());
    }

    #[test]
    fn closing_invites_narrows_who_can_add_people() {
        let mut s = ProjectState::default();
        let ch = s.create_channel("Ops", "alice").expect("channel");
        s.invite_to_channel(&ch.id, "alice", "bob").expect("invite");
        // Closed by default: a plain member cannot bring someone in.
        assert!(!s.channels[0].can_invite("bob"));
        s.update_channel_settings(&ch.id, None, Some(true), None)
            .expect("open invites");
        assert!(
            s.channels[0].can_invite("bob"),
            "turning it on is what lets members invite"
        );
        s.update_channel_settings(&ch.id, None, Some(false), None)
            .expect("close again");
        let ch = &s.channels[0];
        assert!(!ch.can_invite("bob"), "a plain member no longer can");
        assert!(ch.can_invite("alice"), "the owner always can");
        assert!(!ch.can_kick("bob"), "and cannot remove anyone");
        assert!(ch.can_kick("alice"));
    }

    #[test]
    fn the_owner_cannot_be_removed_from_their_own_channel() {
        let mut s = ProjectState::default();
        let ch = s.create_channel("Ops", "alice").expect("channel");
        s.invite_to_channel(&ch.id, "alice", "bob").expect("invite");
        assert!(s.remove_channel_member(&ch.id, "alice").is_err());
        let after = s.remove_channel_member(&ch.id, "bob").expect("removed");
        assert!(!after.members.iter().any(|m| m == "bob"));
    }
}
