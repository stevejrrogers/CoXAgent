//! Agent-engine adapters and discovery.

pub mod mock;
pub mod opencode;
pub mod registry;

pub use mock::MockEngine;
pub use opencode::OpencodeEngine;
pub use registry::{discover, discover_in, DetectedEngine};
