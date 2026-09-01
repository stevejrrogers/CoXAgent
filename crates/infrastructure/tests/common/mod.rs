//! Shared guard for the DSN-gated Postgres integration tests.
#![allow(dead_code)]

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_infrastructure::SqlStateStore;

/// Refuse to run destructive integration tests against a database that
/// already holds a real hub project. A dedicated test database has no `cxa`
/// row; a live hub's `cxa` row always carries revision > 0. This closed a
/// real incident: an exported COXAGENT_TEST_PG_DSN pointing at the
/// production store filled it with `test-<pid>` project rows.
pub async fn is_live_hub_db(dsn: &str) -> bool {
    let Ok(probe) = SqlStateStore::connect(dsn, "cxa").await else {
        return false;
    };
    matches!(probe.current_version().await, Ok(Some(v)) if v > 0)
}
