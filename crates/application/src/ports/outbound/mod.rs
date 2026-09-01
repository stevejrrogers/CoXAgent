//! Outbound ports — interfaces the application needs the outside world to fulfil.

pub mod audit;
pub mod backup;
pub mod deploy;
pub mod deps;
pub mod doc_store;
pub mod engine;
pub mod forge;
pub mod git;
pub mod janitor;
pub mod kv_doc;
pub mod notify;
pub mod outbox;
pub mod pr_report;
pub mod probe;
pub mod screenshot;
pub mod state_store;
pub mod storage;
pub mod workspace;

pub use audit::{AuditPort, AuditRecord};
pub use backup::{
    archive_sha256, hex_decode, hex_encode, is_safe_archive_path, sha256_hex, validate_archive,
    ArchiveFile, ArchiveManifest, ArchiveRoot, BackupArchivePort, SecretChoice, WorkspaceArchive,
    ARCHIVE_SCHEMA_VERSION,
};
pub use deploy::{
    is_publishable_host_port, parse_deploy_host_port, verify_deploy_health,
    verify_deploy_health_probe, CrossCheck, DeployPort, DeployReport, LintReport,
};
pub use deps::{DependencyDiscoveryPort, Lockfile};
pub use doc_store::DocStorePort;
pub use engine::{AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, Usage};
pub use forge::{ForgePort, PrFeedback, PullRequest};
pub use git::{GitAuthor, GitPort, SyncBase, WorkingTreeDiff};
pub use janitor::ProcessJanitorPort;
pub use kv_doc::KvDocPort;
pub use notify::{ChatNotifier, FanoutNotifier, NotifierPort, NotifyEvent, NullNotifier};
pub use outbox::{MemoryOutboxStore, OutboxStorePort};
pub use pr_report::{NullPrReporter, PrOpen, PrReporterPort, StorePrReporter};
pub use probe::{ApiProbePort, ApiProof};
pub use screenshot::ScreenshotPort;
pub use state_store::{
    mutate_state, GitCheck, QuarantineEntry, StateStorePort, WorkerCaps, WorkerEntry,
};
pub use storage::StoragePort;
pub use workspace::{FileMeta, WorkspaceFilesPort};
