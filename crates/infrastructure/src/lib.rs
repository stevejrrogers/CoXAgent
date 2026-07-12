//! CoXAgent infrastructure layer — outbound adapters implementing application
//! ports (state stores, engines, deploy, git, event bus). M0 ships the JSON
//! state store.

pub mod engine;
pub mod state;

pub use engine::{discover, AnyEngine, ClaudeEngine, DetectedEngine, MockEngine, OpencodeEngine};
pub use state::JsonStateStore;
