//! State-store adapters.

pub mod any_store;
pub mod json_store;
pub mod redis_coord;
pub mod sql_store;

#[cfg(feature = "rest-store")]
mod rest_store;

pub use any_store::AnyStateStore;
pub use json_store::JsonStateStore;
pub use redis_coord::RedisCoord;
#[cfg(feature = "rest-store")]
pub use rest_store::{RestConfig, RestStateStore};
pub use sql_store::SqlStateStore;
