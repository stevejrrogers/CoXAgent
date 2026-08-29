//! CoXAgent infrastructure layer — outbound adapters implementing application
//! ports (state stores, engines, deploy, git, event bus). M0 ships the JSON
//! state store.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit_sink;
pub mod auth;
pub mod cleanup;
pub mod deps_discovery;
pub mod deploy;
pub mod docs_store;
pub mod engine;
pub mod forge;
pub mod git;
pub mod kv_doc;
pub mod notifier;
pub mod pr_report;
pub mod probe;
pub mod proc;
pub mod screenshot;
pub mod sql_auth;
pub mod state;
pub mod storage;
pub mod totp;
pub mod workspace_files;

pub use audit_sink::{MemoryAuditSink, SqlAuditSink};
pub use auth::FileAuthService;
pub use cleanup::OsProcessJanitor;
pub use deps_discovery::FsLockfileDiscovery;
pub use deploy::DockerComposeDeploy;
pub use docs_store::MongoDocStore;
pub use engine::{
    discover, discover_opencode_models, discover_tooling, AnyEngine, ClaudeEngine, DetectedEngine,
    DetectedTool, MockEngine, OpencodeEngine, Tooling,
};
pub use forge::{github_forge, probe_git_access, GhApiForge, GhForge, GlForge};
pub use git::SystemGit;
pub use kv_doc::PgKvDoc;
pub use notifier::WebhookNotifier;
pub use pr_report::HttpPrReporter;
pub use sql_auth::SqlAuthService;
pub use state::{AnyStateStore, JsonStateStore, RestConfig, RestStateStore, SqlStateStore};
pub use storage::{LocalStorage, S3Storage};
pub use workspace_files::FsWorkspaceFiles;
