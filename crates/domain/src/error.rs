//! Domain-level errors. No IO concerns here — only rule violations.

use crate::ticket::{Role, Status, TicketType};

/// Errors raised by the domain when an invariant or rule would be broken.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("empty {field}: must not be blank")]
    Empty { field: &'static str },

    #[error("invalid transition for {ticket_type:?}: {from:?} -> {to:?}")]
    InvalidTransition {
        ticket_type: TicketType,
        from: Status,
        to: Status,
    },

    #[error("role {role:?} is not allowed to change field `{field}`")]
    FieldNotPermitted { role: Role, field: &'static str },

    #[error("role {role:?} is not allowed to perform transition {from:?} -> {to:?}")]
    TransitionNotPermitted {
        role: Role,
        from: Status,
        to: Status,
    },

    #[error("ticket not ready: missing {missing}")]
    NotReady { missing: &'static str },

    #[error("invalid version string `{0}`")]
    InvalidVersion(String),

    #[error("ticket already claimed by `{by}`")]
    AlreadyClaimed { by: String },
}
