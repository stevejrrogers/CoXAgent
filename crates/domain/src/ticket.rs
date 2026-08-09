//! The `Ticket` aggregate root and its value objects.
//!
//! Invariants are enforced at construction (parse-don't-validate) and every
//! mutation goes through a method that checks the transition table and
//! field-level role permissions. A ticket in an invalid state cannot exist.

use crate::error::DomainError;
use crate::ids::TicketId;
use crate::transitions::{can_transition, field_permitted, transition_allowed};
use serde::{Deserialize, Serialize};

/// The single ticket kind, discriminated by `type` (anti-Jira: one entity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketType {
    Feature,
    Bug,
    Chore,
}

/// Three-level priority — deliberately coarse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
}

/// Coarse sizing used for the SA design gate (small can auto-pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Small,
    Medium,
    Large,
}

/// Lifecycle status. Feature/chore and bug share `InProgress` and `Rejected`;
/// the transition table keeps the two lifecycles distinct per [`TicketType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    // Feature / chore lifecycle
    Pending,
    Ready,
    InProgress,
    Done,
    Documented,
    Rejected,
    // Bug lifecycle
    Open,
    Fixed,
    Verified,
}

/// The nine team roles plus `User` (super-PO) and `System` (automated bookkeeping).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Ba,
    Po,
    Sm,
    Sa,
    Pd,
    DevBug,
    DevFeature,
    Test,
    Docs,
    User,
    System,
}

/// Technical design authored by SA. Presence gates `Pending -> Ready`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TechnicalDesign {
    pub approach: String,
    pub files: Vec<String>,
    pub api_contract: String,
    pub data_changes: String,
    pub test_plan: String,
    /// Alternatives considered and why they were rejected — the difference
    /// between a design and the first idea that compiled.
    #[serde(default)]
    pub alternatives: String,
}

/// UX design authored by PD. Mandatory when `has_ui` is true.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UxDesign {
    pub user_flow: String,
    pub screens: Vec<String>,
    pub component_states: Vec<String>,
    pub responsive_notes: String,
}

/// Combined design attached to a ticket.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Design {
    pub technical: Option<TechnicalDesign>,
    pub ux: Option<UxDesign>,
}

/// The ticket aggregate root. Fields are private; all access is via methods so
/// no code path can produce an inconsistent ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    id: TicketId,
    #[serde(rename = "type")]
    kind: TicketType,
    title: String,
    description: String,
    priority: Priority,
    complexity: Complexity,
    status: Status,
    has_ui: bool,
    design: Design,
    parent_id: Option<TicketId>,
    depends_on: Vec<TicketId>,
    /// Up to 5 acceptance criteria — the checklist that defines "done".
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    /// Worker that holds the in-progress claim (`account@host`), or `None` when
    /// unclaimed. Set atomically when the ticket enters `InProgress`; cleared on
    /// completion or release. Lets concurrent runners on a shared backlog avoid
    /// working the same ticket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claimed_by: Option<String>,
    /// RFC3339 time the claim was taken — a lease timestamp so recovery can tell
    /// a fresh claim from one orphaned by a crashed worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claimed_at: Option<String>,
    /// A HUMAN this ticket is routed to (username), or `None` for the agent
    /// pool. Human-assigned tickets are invisible to the DEV agents' candidate
    /// selection — the person owns it end to end; they hand it back by
    /// clearing the assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee: Option<String>,
}

impl Ticket {
    /// Create a fresh ticket in its initial status (`Pending` for feature/chore,
    /// `Open` for bug), validating required text fields.
    ///
    /// # Errors
    /// Returns [`DomainError::Empty`] when `title` is blank.
    pub fn new(
        id: TicketId,
        ticket_type: TicketType,
        title: impl Into<String>,
        description: impl Into<String>,
        priority: Priority,
        complexity: Complexity,
        has_ui: bool,
    ) -> Result<Self, DomainError> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(DomainError::Empty { field: "title" });
        }
        let status = match ticket_type {
            TicketType::Bug => Status::Open,
            TicketType::Feature | TicketType::Chore => Status::Pending,
        };
        Ok(Self {
            id,
            kind: ticket_type,
            title,
            description: description.into(),
            priority,
            complexity,
            status,
            has_ui,
            design: Design::default(),
            parent_id: None,
            depends_on: Vec::new(),
            acceptance_criteria: Vec::new(),
            claimed_by: None,
            claimed_at: None,
            assignee: None,
        })
    }

    /// The human description of the work (what & why).
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The acceptance criteria checklist (at most 5 entries).
    #[must_use]
    pub fn acceptance_criteria(&self) -> &[String] {
        &self.acceptance_criteria
    }

    /// Replace the acceptance criteria, trimming blanks and capping at 5.
    pub fn set_acceptance_criteria(&mut self, criteria: Vec<String>) {
        self.acceptance_criteria = criteria
            .into_iter()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .take(5)
            .collect();
    }

    // --- Accessors ---

    #[must_use]
    pub fn id(&self) -> &TicketId {
        &self.id
    }

    #[must_use]
    pub fn ticket_type(&self) -> TicketType {
        self.kind
    }

    #[must_use]
    pub fn status(&self) -> Status {
        self.status
    }

    #[must_use]
    pub fn priority(&self) -> Priority {
        self.priority
    }

    #[must_use]
    pub fn has_ui(&self) -> bool {
        self.has_ui
    }

    #[must_use]
    pub fn complexity(&self) -> Complexity {
        self.complexity
    }

    #[must_use]
    pub fn design(&self) -> &Design {
        &self.design
    }

    #[must_use]
    pub fn depends_on(&self) -> &[TicketId] {
        &self.depends_on
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The worker holding the in-progress claim (`account@host`), or `None`.
    #[must_use]
    pub fn claimed_by(&self) -> Option<&str> {
        self.claimed_by.as_deref()
    }

    /// The human this ticket is routed to, or `None` for the agent pool.
    #[must_use]
    pub fn assignee(&self) -> Option<&str> {
        self.assignee.as_deref()
    }

    /// Route this ticket to a human (empty clears back to the agent pool).
    pub fn assign_to_human(&mut self, username: &str) {
        let u = username.trim();
        self.assignee = if u.is_empty() {
            None
        } else {
            Some(u.to_owned())
        };
    }

    /// RFC3339 time the current claim was taken, or `None` when unclaimed.
    #[must_use]
    pub fn claimed_at(&self) -> Option<&str> {
        self.claimed_at.as_deref()
    }

    // --- Guarded mutations ---

    /// Edit the human text (title + description). Only PO or a `User` acting as
    /// super-PO may — the same authority that owns scope.
    ///
    /// # Errors
    /// - [`DomainError::FieldNotPermitted`] if `actor` may not edit scope text.
    /// - [`DomainError::Empty`] if the new title is blank.
    pub fn edit(
        &mut self,
        actor: Role,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<(), DomainError> {
        if !matches!(actor, Role::Po | Role::User) {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "title",
            });
        }
        let title = title.into();
        if title.trim().is_empty() {
            return Err(DomainError::Empty { field: "title" });
        }
        self.title = title;
        self.description = description.into();
        Ok(())
    }

    /// Restate the requirement after a rescue — the BA rewriting a ticket the
    /// developers could not build against. Unlike [`Ticket::edit`] the title is
    /// left alone: the ask has not changed, only how clearly it is stated.
    ///
    /// # Errors
    /// - [`DomainError::FieldNotPermitted`] if `actor` may not restate scope.
    /// - [`DomainError::Empty`] if the new description is blank.
    pub fn clarify(&mut self, actor: Role, description: &str) -> Result<(), DomainError> {
        if !matches!(actor, Role::Ba | Role::Po | Role::User) {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "description",
            });
        }
        if description.trim().is_empty() {
            return Err(DomainError::Empty {
                field: "description",
            });
        }
        self.description.clear();
        self.description.push_str(description.trim());
        Ok(())
    }

    /// Atomically claim the ticket for `worker` (`account@host`) by moving it
    /// into `InProgress` and stamping ownership. Fails if the ticket is already
    /// claimed or the transition is not legal — so two concurrent runners racing
    /// on a shared backlog cannot both win the same ticket.
    ///
    /// # Errors
    /// - [`DomainError::AlreadyClaimed`] if another worker already holds it.
    /// - Any error from [`Ticket::transition_to`] for an illegal claim.
    pub fn claim(
        &mut self,
        actor: Role,
        worker: impl Into<String>,
        now: impl Into<String>,
    ) -> Result<(), DomainError> {
        if let Some(holder) = &self.claimed_by {
            return Err(DomainError::AlreadyClaimed { by: holder.clone() });
        }
        self.transition_to(actor, Status::InProgress)?;
        self.claimed_by = Some(worker.into());
        self.claimed_at = Some(now.into());
        Ok(())
    }

    /// Change priority. Only PO (or a `User` acting as super-PO) may do so.
    ///
    /// # Errors
    /// [`DomainError::FieldNotPermitted`] if `actor` lacks priority authority.
    pub fn set_priority(&mut self, actor: Role, priority: Priority) -> Result<(), DomainError> {
        if !field_permitted(actor, "priority") {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "priority",
            });
        }
        self.priority = priority;
        Ok(())
    }

    /// Attach or replace the technical design. Only SA may write it.
    ///
    /// # Errors
    /// [`DomainError::FieldNotPermitted`] if `actor` is not SA.
    pub fn set_technical_design(
        &mut self,
        actor: Role,
        design: TechnicalDesign,
    ) -> Result<(), DomainError> {
        if !field_permitted(actor, "design.technical") {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "design.technical",
            });
        }
        self.design.technical = Some(design);
        Ok(())
    }

    /// Attach or replace the UX design. Only PD (or SA when PD is disabled) writes it.
    ///
    /// # Errors
    /// [`DomainError::FieldNotPermitted`] if `actor` is not PD/SA.
    pub fn set_ux_design(&mut self, actor: Role, design: UxDesign) -> Result<(), DomainError> {
        if !field_permitted(actor, "design.ux") {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "design.ux",
            });
        }
        self.design.ux = Some(design);
        Ok(())
    }

    /// Declare a dependency on another ticket (SA during design/split).
    ///
    /// # Errors
    /// [`DomainError::FieldNotPermitted`] if `actor` may not edit dependencies.
    pub fn add_dependency(&mut self, actor: Role, on: TicketId) -> Result<(), DomainError> {
        if !field_permitted(actor, "depends_on") {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "depends_on",
            });
        }
        if !self.depends_on.contains(&on) {
            self.depends_on.push(on);
        }
        Ok(())
    }

    /// Attempt a status transition performed by `actor`.
    ///
    /// Enforces three things in order: the transition is valid for this ticket
    /// type, the actor's role is allowed to perform it, and any status-specific
    /// precondition (e.g. `Ready` requires a design) holds.
    ///
    /// # Errors
    /// - [`DomainError::InvalidTransition`] — not a legal edge for this type.
    /// - [`DomainError::TransitionNotPermitted`] — role not allowed.
    /// - [`DomainError::NotReady`] — precondition for the target status unmet.
    pub fn transition_to(&mut self, actor: Role, to: Status) -> Result<(), DomainError> {
        let from = self.status;
        if !transition_allowed(self.kind, from, to) {
            return Err(DomainError::InvalidTransition {
                ticket_type: self.kind,
                from,
                to,
            });
        }
        if !can_transition(actor, from, to) {
            return Err(DomainError::TransitionNotPermitted {
                role: actor,
                from,
                to,
            });
        }
        if to == Status::Ready {
            self.check_ready()?;
        }
        self.status = to;
        // Leaving InProgress (completion or reject) frees the claim.
        if to != Status::InProgress {
            self.claimed_by = None;
            self.claimed_at = None;
        }
        Ok(())
    }

    /// Release an orphaned claim back to the work queue during recovery.
    ///
    /// Only `System` (the orchestrator at startup) may do this: a ticket left
    /// `InProgress` by a crashed run is returned to `Ready` (feature/chore) or
    /// `Open` (bug) so it can be picked up again. A dedicated method rather than
    /// a table edge, so normal agents can never "un-claim" work.
    ///
    /// # Errors
    /// - [`DomainError::FieldNotPermitted`] if `actor` is not `System`.
    /// - [`DomainError::InvalidTransition`] if the ticket is not `InProgress`.
    pub fn release_claim(&mut self, actor: Role) -> Result<(), DomainError> {
        if actor != Role::System {
            return Err(DomainError::FieldNotPermitted {
                role: actor,
                field: "claim",
            });
        }
        if self.status != Status::InProgress {
            return Err(DomainError::InvalidTransition {
                ticket_type: self.kind,
                from: self.status,
                to: self.status,
            });
        }
        self.status = match self.kind {
            TicketType::Bug => Status::Open,
            TicketType::Feature | TicketType::Chore => Status::Ready,
        };
        self.claimed_by = None;
        self.claimed_at = None;
        Ok(())
    }

    /// Definition of Ready: technical design present, and UX design present when
    /// the ticket has UI.
    fn check_ready(&self) -> Result<(), DomainError> {
        if self.design.technical.is_none() {
            return Err(DomainError::NotReady {
                missing: "design.technical",
            });
        }
        if self.has_ui && self.design.ux.is_none() {
            return Err(DomainError::NotReady {
                missing: "design.ux",
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(has_ui: bool) -> Ticket {
        Ticket::new(
            TicketId::new("FEAT-001").expect("id"),
            TicketType::Feature,
            "A feature",
            "desc",
            Priority::Medium,
            Complexity::Medium,
            has_ui,
        )
        .expect("ticket")
    }

    fn tech_design() -> TechnicalDesign {
        TechnicalDesign {
            approach: "do it".to_owned(),
            ..TechnicalDesign::default()
        }
    }

    #[test]
    fn new_feature_starts_pending_new_bug_starts_open() {
        assert_eq!(feature(false).status(), Status::Pending);
        let bug = Ticket::new(
            TicketId::new("BUG-1").expect("id"),
            TicketType::Bug,
            "A bug",
            "",
            Priority::High,
            Complexity::Small,
            false,
        )
        .expect("bug");
        assert_eq!(bug.status(), Status::Open);
    }

    #[test]
    fn ready_requires_technical_design() {
        let mut t = feature(false);
        // No design yet -> NotReady.
        assert_eq!(
            t.transition_to(Role::Sa, Status::Ready),
            Err(DomainError::NotReady {
                missing: "design.technical"
            })
        );
        t.set_technical_design(Role::Sa, tech_design())
            .expect("set");
        assert!(t.transition_to(Role::Sa, Status::Ready).is_ok());
        assert_eq!(t.status(), Status::Ready);
    }

    #[test]
    fn ui_feature_also_requires_ux_design() {
        let mut t = feature(true);
        t.set_technical_design(Role::Sa, tech_design())
            .expect("set");
        assert_eq!(
            t.transition_to(Role::Sa, Status::Ready),
            Err(DomainError::NotReady {
                missing: "design.ux"
            })
        );
        t.set_ux_design(Role::Pd, UxDesign::default()).expect("ux");
        assert!(t.transition_to(Role::Sa, Status::Ready).is_ok());
    }

    #[test]
    fn dev_cannot_open_design_gate() {
        let mut t = feature(false);
        t.set_technical_design(Role::Sa, tech_design())
            .expect("set");
        assert!(matches!(
            t.transition_to(Role::DevFeature, Status::Ready),
            Err(DomainError::TransitionNotPermitted { .. })
        ));
    }

    #[test]
    fn illegal_edge_is_rejected_before_role_check() {
        let mut t = feature(false);
        assert!(matches!(
            t.transition_to(Role::System, Status::Done),
            Err(DomainError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn dev_cannot_change_priority_but_po_can() {
        let mut t = feature(false);
        assert!(t.set_priority(Role::DevFeature, Priority::High).is_err());
        assert!(t.set_priority(Role::Po, Priority::High).is_ok());
        assert_eq!(t.priority(), Priority::High);
    }

    #[test]
    fn system_releases_orphaned_claim_to_ready() {
        let mut t = feature(false);
        t.set_technical_design(Role::Sa, tech_design())
            .expect("set");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.transition_to(Role::DevFeature, Status::InProgress)
            .expect("claim");
        // Crash happens here; recovery releases the claim.
        t.release_claim(Role::System).expect("release");
        assert_eq!(t.status(), Status::Ready);
    }

    #[test]
    fn claim_stamps_owner_and_rejects_second_claimer() {
        let mut t = feature(false);
        t.set_technical_design(Role::Sa, tech_design())
            .expect("set");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.claim(Role::DevFeature, "alice@mac", "2026-07-15T00:00:00Z")
            .expect("first claim wins");
        assert_eq!(t.status(), Status::InProgress);
        assert_eq!(t.claimed_by(), Some("alice@mac"));
        // A second worker cannot steal an in-progress claim.
        assert!(matches!(
            t.claim(Role::DevFeature, "bob@pc", "2026-07-15T00:01:00Z"),
            Err(DomainError::AlreadyClaimed { .. })
        ));
        assert_eq!(t.claimed_by(), Some("alice@mac"));
    }

    #[test]
    fn completing_clears_the_claim() {
        let mut t = feature(false);
        t.set_technical_design(Role::Sa, tech_design())
            .expect("set");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.claim(Role::DevFeature, "alice@mac", "2026-07-15T00:00:00Z")
            .expect("claim");
        t.transition_to(Role::DevFeature, Status::Done)
            .expect("done");
        assert_eq!(t.claimed_by(), None);
    }

    #[test]
    fn non_system_cannot_release_claim() {
        let mut t = feature(false);
        t.set_technical_design(Role::Sa, tech_design())
            .expect("set");
        t.transition_to(Role::Sa, Status::Ready).expect("ready");
        t.transition_to(Role::DevFeature, Status::InProgress)
            .expect("claim");
        assert!(t.release_claim(Role::DevFeature).is_err());
        assert_eq!(t.status(), Status::InProgress);
    }

    #[test]
    fn clarify_is_the_bas_to_make_and_keeps_the_title() {
        let mut t = feature(false);
        assert!(
            t.clarify(Role::DevBug, "restated").is_err(),
            "not DEV's call"
        );
        t.clarify(Role::Ba, "  Repro: run x, observe y.  ")
            .expect("BA may restate");
        assert_eq!(t.description(), "Repro: run x, observe y.");
        assert_eq!(t.title(), "A feature");
        assert!(
            t.clarify(Role::Ba, "   ").is_err(),
            "blank is not a clarification"
        );
    }
}
