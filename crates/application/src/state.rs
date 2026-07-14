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

/// One message on a discussion thread — an agent or the user commenting on a
/// ticket (`ticket = Some`) or on the team channel (`ticket = None`). This is
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

/// the teamwork surface the original workflow lacked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub at: String,
    /// Author: an agent role (e.g. `SM`, `PO`) or `USER`.
    pub author: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
    /// Files/images attached to the comment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// Keep discussion threads bounded per project.
pub const MAX_COMMENTS: usize = 500;

/// One human-to-human message in the project's team chat channel. Unlike
/// [`Comment`] (which is dominated by agent scrum chatter), this is a plain
/// channel for the people on the project to talk to each other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMsg {
    pub at: String,
    /// The authenticated username of the sender.
    pub user: String,
    pub body: String,
    /// Files/images attached to the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

/// Keep the team chat bounded per project.
pub const MAX_CHAT: usize = 500;

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
    /// Team chat: human-to-human messages among the people on the project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chat: Vec<ChatMsg>,
    /// Project-level design system authored by PD (absent until a UI ticket
    /// prompts PD to create it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_system: Option<DesignSystem>,
    /// Product milestones the sprints work toward (authored once by the PO).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub milestones: Vec<Milestone>,
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
            chat: Vec::new(),
            design_system: None,
            milestones: Vec::new(),
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

    /// Post a comment with attachments.
    pub fn post_comment_att(
        &mut self,
        author: &str,
        body: &str,
        ticket: Option<String>,
        attachments: Vec<Attachment>,
    ) {
        self.comments.push(Comment {
            at: now_rfc3339(),
            author: author.to_owned(),
            body: body.to_owned(),
            ticket,
            attachments,
        });
        let overflow = self.comments.len().saturating_sub(MAX_COMMENTS);
        if overflow > 0 {
            self.comments.drain(0..overflow);
        }
    }

    /// Append a team-chat message from `user`, trimming the oldest beyond
    /// [`MAX_CHAT`].
    pub fn post_chat(&mut self, user: &str, body: &str) {
        self.post_chat_att(user, body, Vec::new());
    }

    /// Append a team-chat message with attachments.
    pub fn post_chat_att(&mut self, user: &str, body: &str, attachments: Vec<Attachment>) {
        self.chat.push(ChatMsg {
            at: now_rfc3339(),
            user: user.to_owned(),
            body: body.to_owned(),
            attachments,
        });
        let overflow = self.chat.len().saturating_sub(MAX_CHAT);
        if overflow > 0 {
            self.chat.drain(0..overflow);
        }
    }
}

pub(crate) fn now_rfc3339() -> String {
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
