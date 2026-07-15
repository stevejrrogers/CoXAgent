//! CoXAgent application layer — use cases and the ports they depend on.
//! Depends only on the domain crate; infrastructure implements the outbound
//! ports, presentation drives the inbound ones.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod auth;
pub mod codegraph;
pub mod config;
pub mod conformance;
pub mod error;
pub mod metrics;
pub mod parsing;
pub mod policy;
pub mod ports;
pub mod prompts;
pub mod selection;
pub mod sprint;
pub mod state;
pub mod system_chat;
pub mod tokens;
pub mod use_cases;

pub use auth::{AuthPort, AuthRole, AuthUser, LoginResult, TokenInfo};
pub use config::{
    BudgetCaps, Config, DeployConfig, EngineChoice, EngineKind, EngineMapping, LiveBudget, Mode,
    PolicyConfig, WorkflowConfig,
};
pub use error::{AppError, PortError};
pub use state::{
    Attachment, Channel, ChatMsg, Comment, DesignSystem, DocPage, Milestone, PrReview,
    ProjectState, Reaction, Spend, Sprint, GENERAL_CHANNEL, SCHEMA_VERSION,
};
pub use system_chat::{ChatContext, ProjectRef, SystemChat, UserRef, Webhook};
