//! Outbound ports — interfaces the application needs the outside world to fulfil.

pub mod audit;
pub mod deploy;
pub mod engine;
pub mod forge;
pub mod git;
pub mod notify;
pub mod state_store;

pub use audit::{AuditPort, AuditRecord};
pub use deploy::{DeployPort, DeployReport};
pub use engine::{AgentEnginePort, AgentOutcome, AgentRequest, Usage};
pub use forge::{ForgePort, PullRequest};
pub use git::{GitAuthor, GitPort};
pub use notify::{NotifierPort, NotifyEvent, NullNotifier};
pub use state_store::StateStorePort;
