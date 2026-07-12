//! Domain events. Every meaningful state change emits one; the event bus turns
//! these into SSE for the team board, so realtime is a consequence of the model.

use crate::ids::TicketId;
use crate::ticket::{Role, Status};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// A domain event with the actor and time that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub actor: Role,
    pub kind: EventKind,
}

/// The kinds of change the domain reports. Append here as the model grows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    TicketCreated {
        ticket: TicketId,
    },
    TicketTransitioned {
        ticket: TicketId,
        from: Status,
        to: Status,
    },
    PriorityChanged {
        ticket: TicketId,
    },
    DesignAttached {
        ticket: TicketId,
        part: DesignPart,
    },
}

/// Which part of a ticket's design was attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignPart {
    Technical,
    Ux,
}

impl DomainEvent {
    #[must_use]
    pub fn new(at: OffsetDateTime, actor: Role, kind: EventKind) -> Self {
        Self { at, actor, kind }
    }
}
