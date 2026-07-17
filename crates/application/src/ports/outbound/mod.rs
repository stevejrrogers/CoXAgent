//! Outbound ports — interfaces the application needs the outside world to fulfil.

pub mod audit;
pub mod deploy;
pub mod doc_store;
pub mod engine;
pub mod forge;
pub mod git;
pub mod kv_doc;
pub mod notify;
pub mod state_store;
pub mod storage;

pub use audit::{AuditPort, AuditRecord};
pub use deploy::{DeployPort, DeployReport};
pub use doc_store::DocStorePort;
pub use engine::{AgentEnginePort, AgentOutcome, AgentRequest, Usage};
pub use forge::{ForgePort, PullRequest};
pub use git::{GitAuthor, GitPort};
pub use kv_doc::KvDocPort;
pub use notify::{ChatNotifier, FanoutNotifier, NotifierPort, NotifyEvent, NullNotifier};
pub use state_store::{mutate_state, StateStorePort, WorkerEntry};
pub use storage::StoragePort;
