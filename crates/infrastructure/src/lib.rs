//! CoXAgent infrastructure layer — outbound adapters implementing application
//! ports (state stores, engines, deploy, git, event bus). M0 ships the JSON
//! state store.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit_sink;
pub mod auth;
pub mod deploy;
pub mod docs_store;
pub mod engine;
pub mod forge;
pub mod git;
pub mod kv_doc;
pub mod notifier;
pub mod sql_auth;
pub mod state;
pub mod storage;
pub mod totp;

pub use audit_sink::{MemoryAuditSink, SqlAuditSink};
pub use auth::FileAuthService;
pub use deploy::DockerComposeDeploy;
pub use docs_store::MongoDocStore;
pub use engine::{
    discover, discover_tooling, AnyEngine, ClaudeEngine, DetectedEngine, DetectedTool, MockEngine,
    OpencodeEngine, Tooling,
};
pub use forge::{GhForge, GlForge};
pub use git::SystemGit;
pub use kv_doc::PgKvDoc;
pub use notifier::WebhookNotifier;
pub use sql_auth::SqlAuthService;
pub use state::{AnyStateStore, JsonStateStore, SqlStateStore};
pub use storage::{LocalStorage, S3Storage};
