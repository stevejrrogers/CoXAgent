//! Outbound ports — interfaces the application needs the outside world to fulfil.

pub mod audit;
pub mod deploy;
pub mod doc_store;
pub mod engine;
pub mod forge;
pub mod git;
pub mod janitor;
pub mod kv_doc;
pub mod notify;
pub mod probe;
pub mod screenshot;
pub mod state_store;
pub mod storage;
pub mod workspace;

pub use audit::{AuditPort, AuditRecord};
pub use deploy::{
    parse_deploy_host_port, verify_deploy_health, CrossCheck, DeployPort, DeployReport,
    LintReport,
};
pub use doc_store::DocStorePort;
pub use engine::{AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, Usage};
pub use forge::{ForgePort, PrFeedback, PullRequest};
pub use git::{GitAuthor, GitPort, SyncBase, WorkingTreeDiff};
pub use janitor::ProcessJanitorPort;
pub use kv_doc::KvDocPort;
pub use notify::{ChatNotifier, FanoutNotifier, NotifierPort, NotifyEvent, NullNotifier};
pub use probe::{ApiProbePort, ApiProof};
pub use screenshot::ScreenshotPort;
pub use state_store::{mutate_state, StateStorePort, WorkerEntry};
pub use storage::StoragePort;
pub use workspace::{FileMeta, WorkspaceFilesPort};
