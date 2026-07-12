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

/// The whole state of one managed project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectState {
    pub schema_version: u32,
    pub current_version: SemVer,
    pub tickets: Vec<Ticket>,
    #[serde(default)]
    pub history: Vec<DeployRecord>,
}

impl Default for ProjectState {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            current_version: SemVer::default(),
            tickets: Vec::new(),
            history: Vec::new(),
        }
    }
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
