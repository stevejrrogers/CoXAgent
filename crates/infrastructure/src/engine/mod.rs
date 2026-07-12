//! Agent-engine adapters and discovery.

pub mod any;
pub mod claude;
pub mod metering;
pub mod mock;
pub mod opencode;
pub mod registry;
pub mod scripted;
pub mod transcript;

pub use any::AnyEngine;
pub use claude::ClaudeEngine;
pub use metering::{Meter, MeteringEngine};
pub use mock::MockEngine;
pub use opencode::OpencodeEngine;
pub use registry::{discover, discover_in, DetectedEngine};
pub use scripted::ScriptedEngine;
pub use transcript::TranscriptEngine;
