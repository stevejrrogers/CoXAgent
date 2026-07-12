//! CoXAgent application layer — use cases and the ports they depend on.
//! Depends only on the domain crate; infrastructure implements the outbound
//! ports, presentation drives the inbound ones.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod ports;
pub mod state;
pub mod use_cases;

pub use error::{AppError, PortError};
pub use state::{ProjectState, SCHEMA_VERSION};
