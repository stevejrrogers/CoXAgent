//! CoXAgent infrastructure layer — outbound adapters implementing application
//! ports (state stores, engines, deploy, git, event bus). M0 ships the JSON
//! state store.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod auth;
pub mod deploy;
pub mod engine;
pub mod state;

pub use auth::FileAuthService;
pub use deploy::DockerComposeDeploy;
pub use engine::{discover, AnyEngine, ClaudeEngine, DetectedEngine, MockEngine, OpencodeEngine};
pub use state::JsonStateStore;
