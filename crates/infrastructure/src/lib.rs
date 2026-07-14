//! CoXAgent infrastructure layer — outbound adapters implementing application
//! ports (state stores, engines, deploy, git, event bus). M0 ships the JSON
//! state store.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit_sink;
pub mod auth;
pub mod deploy;
pub mod engine;
pub mod forge;
pub mod git;
pub mod notifier;
pub mod sql_auth;
pub mod state;
pub mod totp;

pub use audit_sink::{MemoryAuditSink, SqlAuditSink};
pub use auth::FileAuthService;
pub use deploy::DockerComposeDeploy;
pub use engine::{discover, AnyEngine, ClaudeEngine, DetectedEngine, MockEngine, OpencodeEngine};
pub use forge::{GhForge, GlForge};
pub use git::SystemGit;
pub use notifier::WebhookNotifier;
pub use sql_auth::SqlAuthService;
pub use state::{AnyStateStore, JsonStateStore, SqlStateStore};
