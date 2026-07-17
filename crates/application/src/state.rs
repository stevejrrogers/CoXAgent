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
}

/// One emoji reaction on a message and the users who added it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<String>,
}

/// Keep the team chat bounded per project.
pub const MAX_CHAT: usize = 500;

/// The id of the default channel every project has and everyone can see.
pub const GENERAL_CHANNEL: &str = "general";

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

    /// Whether `user` may see and read this channel.
    #[must_use]
    pub fn can_view(&self, user: &str) -> bool {
        self.is_general() || self.owner == user || self.members.iter().any(|m| m == user)
    }

    /// Whether `user` may invite others (owner, or a delegated inviter).
    #[must_use]
    pub fn can_invite(&self, user: &str) -> bool {
        self.is_general() || self.owner == user || self.inviters.iter().any(|m| m == user)
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
        _ => "product",
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
    /// Usage attributed per operator (`account@host`) — the SaaS per-user view,
    /// so each user's token spend is measurable even though they share a project.
    #[serde(default)]
    pub by_operator: std::collections::BTreeMap<String, OperatorSpend>,
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

/// The whole state of one managed project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Spend accumulated on the current calendar day (UTC), for the daily budget
    /// policy. Resets when the day rolls over.
    #[serde(default)]
    pub spend_today_usd: f64,
    /// The UTC date (`YYYY-MM-DD`) `spend_today_usd` is counting.
    #[serde(default)]
    pub spend_day: String,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
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
            spend_today_usd: 0.0,
            spend_day: String::new(),
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

    /// Add `usd` to today's spend, rolling the counter over when the UTC date
    /// changes. Returns the new same-day total.
    pub fn add_daily_spend(&mut self, usd: f64) -> f64 {
        let today = now_rfc3339().get(..10).unwrap_or_default().to_owned();
        if self.spend_day != today {
            self.spend_day = today;
            self.spend_today_usd = 0.0;
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
            channel: channel.to_owned(),
            attachments,
            reactions: Vec::new(),
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
        self.channels.iter().find(|c| c.id == id).cloned()
    }

    /// All channels `user` can see: `#general` first, then their private ones.
    #[must_use]
    pub fn channels_for(&self, user: &str) -> Vec<Channel> {
        let mut out = vec![general_channel_record()];
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
        let id = slugify(name);
        if id.is_empty() {
            return Err("channel name must contain letters or numbers".to_owned());
        }
        if id == GENERAL_CHANNEL || self.channels.iter().any(|c| c.id == id) {
            return Err(format!("channel #{id} already exists"));
        }
        let ch = Channel {
            id,
            name: name.trim().to_owned(),
            owner: owner.to_owned(),
            members: vec![owner.to_owned()],
            inviters: Vec::new(),
            created_at: now_rfc3339(),
            kind: "private".to_owned(),
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
fn general_channel_record() -> Channel {
    Channel {
        id: GENERAL_CHANNEL.to_owned(),
        name: "general".to_owned(),
        owner: String::new(),
        members: Vec::new(),
        inviters: Vec::new(),
        created_at: String::new(),
        kind: "general".to_owned(),
        project: String::new(),
    }
}

/// Turn a display name into a URL-safe channel slug: lowercase, spaces and runs
/// of punctuation collapsed to single hyphens, trimmed. `"Design Review!"` →
/// `"design-review"`.
pub(crate) fn slugify(name: &str) -> String {
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
    use super::{slugify, ProjectState, GENERAL_CHANNEL};

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
        // general is always visible; alice sees general + hers, bob only general.
        assert_eq!(s.channels_for("alice").len(), 2);
        assert_eq!(s.channels_for("bob").len(), 1);
        assert_eq!(s.channels_for("bob")[0].id, GENERAL_CHANNEL);
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
pub(crate) fn mint_id() -> String {
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
}
