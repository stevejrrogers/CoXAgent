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
        })
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

    // --- Guarded mutations ---

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
}
