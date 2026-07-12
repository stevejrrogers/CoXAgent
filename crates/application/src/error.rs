//! Application-layer errors: wraps domain errors and port failures.

use coxagent_domain::DomainError;

/// Error returned by outbound ports (adapters implement them).
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("state is corrupt: {0}")]
    Corrupt(String),

    #[error("backend failure: {0}")]
    Backend(String),
}

/// Error returned by use cases.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Port(#[from] PortError),
}
