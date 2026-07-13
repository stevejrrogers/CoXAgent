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
/// the teamwork surface the original workflow lacked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub at: String,
    /// Author: an agent role (e.g. `SM`, `PO`) or `USER`.
    pub author: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket: Option<String>,
}

/// Keep discussion threads bounded per project.
pub const MAX_COMMENTS: usize = 500;

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

/// The whole state of one managed project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectState {
    pub schema_version: u32,
    /// Short project alias prefixed onto every ticket id (e.g. `CXC`). Empty for
    /// backward compatibility (ids then read `FEAT-001` with no prefix).
    #[serde(default)]
    pub alias: String,
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
}

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            alias: String::new(),
            current_version: SemVer::default(),
            tickets: Vec::new(),
            history: Vec::new(),
            activity: Vec::new(),
            spend: Spend::default(),
            sprint: None,
            sprints: Vec::new(),
            deploy: None,
            comments: Vec::new(),
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

    /// Post a comment to a discussion thread, trimming to [`MAX_COMMENTS`].
    pub fn post_comment(&mut self, author: &str, body: &str, ticket: Option<String>) {
        self.comments.push(Comment {
            at: now_rfc3339(),
            author: author.to_owned(),
            body: body.to_owned(),
            ticket,
        });
        let overflow = self.comments.len().saturating_sub(MAX_COMMENTS);
        if overflow > 0 {
            self.comments.drain(0..overflow);
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
        Ok(())
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
        assert_eq!(s.comments.last().unwrap().body, format!("m{}", MAX_COMMENTS + 9));
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
