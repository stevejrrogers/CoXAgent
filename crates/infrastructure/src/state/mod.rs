//! State-store adapters.

pub mod any_store;
pub mod json_store;
mod quarantine;
pub mod redis_coord;
mod rest_store;
pub mod sql_store;

pub use any_store::AnyStateStore;
pub use json_store::JsonStateStore;
pub use redis_coord::RedisCoord;
pub use rest_store::{RestConfig, RestStateStore};
pub use sql_store::SqlStateStore;
