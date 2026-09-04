//! `MemoryArchiveStore` — an env-gated in-memory [`ArchiveStorePort`] for
//! dev and e2e runs that have no Mongo cold store behind them (CXA-F274).
//!
//! It writes nowhere persistent: the archive lives and dies with the process,
//! which is exactly right for a fixture server and wrong for production —
//! there the Mongo adapter (CXA-F272) takes the port's slot instead. Enabled
//! by `COXAGENT_ARCHIVE_MEMORY=1`; `COXAGENT_ARCHIVE_MEMORY_SEED` (optional)
//! names a JSON file of `{ "<project id>": [Ticket…] }` preloaded at boot so
//! an e2e run starts with a populated archive instead of an empty one.

use async_trait::async_trait;
use coxagent_application::error::PortError;
use coxagent_application::ports::outbound::ArchiveStorePort;
use coxagent_domain::ticket::Ticket;
use std::collections::HashMap;
use std::sync::Mutex;

type Projects = HashMap<String, HashMap<String, Ticket>>;

/// In-memory cold store, one id→ticket map per project.
pub struct MemoryArchiveStore {
    per_project: Mutex<Projects>,
}

impl Default for MemoryArchiveStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Idempotent upsert: an archive re-put of one ticket replaces its copy, so a
/// retried eviction can never duplicate a row (the crash-safety shape F273
/// relies on).
fn upsert(per_project: &mut Projects, project: &str, ticket: Ticket) {
    per_project
        .entry(project.to_owned())
        .or_default()
        .insert(ticket.id().as_str().to_owned(), ticket);
}

impl MemoryArchiveStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            per_project: Mutex::new(HashMap::new()),
        }
    }

    /// Build the store from the environment, or `None` when the gate is off:
    /// `COXAGENT_ARCHIVE_MEMORY=1` enables it, and a set
    /// `COXAGENT_ARCHIVE_MEMORY_SEED` additionally preloads the fixture
    /// (missing/unparsable seed files are refused — a silently empty archive
    /// would lie to the very e2e spec that set the variable).
    ///
    /// # Errors
    /// [`PortError::Backend`] when the seed file cannot be read or parsed.
    pub fn from_env() -> Result<Option<Self>, PortError> {
        let gate = std::env::var("COXAGENT_ARCHIVE_MEMORY").unwrap_or_default();
        let seed_path = std::env::var("COXAGENT_ARCHIVE_MEMORY_SEED").unwrap_or_default();
        if gate.trim().is_empty() && seed_path.trim().is_empty() {
            return Ok(None);
        }
        let mut store = Self::new();
        if !seed_path.trim().is_empty() {
            store.load_seed_file(&seed_path)?;
        }
        Ok(Some(store))
    }

    /// Preload archives from a seed file (`{ "<project id>": [Ticket…] }`).
    fn load_seed_file(&mut self, path: &str) -> Result<(), PortError> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| PortError::Backend(format!("archive seed read {path}: {e}")))?;
        let parsed: HashMap<String, Vec<Ticket>> = serde_json::from_str(&raw)
            .map_err(|e| PortError::Backend(format!("archive seed parse {path}: {e}")))?;
        let Ok(mut map) = self.per_project.lock() else {
            return Ok(()); // poisoned lock: nothing was readable anyway
        };
        for (project, tickets) in parsed {
            for ticket in tickets {
                upsert(&mut map, &project, ticket);
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Projects>, PortError> {
        self.per_project
            .lock()
            .map_err(|p| PortError::Backend(format!("archive lock: {p}")))
    }
}

#[async_trait]
impl ArchiveStorePort for MemoryArchiveStore {
    async fn put(&self, project: &str, ticket: &Ticket) -> Result<(), PortError> {
        let mut guard = self.lock()?;
        upsert(&mut guard, project, ticket.clone());
        Ok(())
    }

    async fn get(&self, project: &str, id: &str) -> Result<Option<Ticket>, PortError> {
        Ok(self
            .lock()?
            .get(project)
            .and_then(|tickets| tickets.get(id))
            .cloned())
    }

    async fn list(&self, project: &str) -> Result<Vec<Ticket>, PortError> {
        Ok(self
            .lock()?
            .get(project)
            .map(|tickets| tickets.values().cloned().collect())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use coxagent_domain::{Complexity, Priority, TicketId, TicketType};

    fn ticket(id: &str) -> Ticket {
        Ticket::new(
            TicketId::new(id).unwrap(),
            TicketType::Feature,
            format!("ticket {id}"),
            "body",
            Priority::Medium,
            Complexity::Small,
            false,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_round_trip_puts_gets_and_lists_by_project() {
        let store = MemoryArchiveStore::new();
        store.put("demo", &ticket("CXC-F001")).await.unwrap();
        store.put("demo", &ticket("CXC-B002")).await.unwrap();
        store.put("other", &ticket("CXC-F009")).await.unwrap();

        let got = store.get("demo", "CXC-F001").await.unwrap().unwrap();
        assert_eq!(got.id().as_str(), "CXC-F001");
        assert!(store.get("demo", "CXC-F404").await.unwrap().is_none());
        assert!(store.get("other", "CXC-F001").await.unwrap().is_none());

        let mut listed: Vec<String> = store
            .list("demo")
            .await
            .unwrap()
            .iter()
            .map(|t| t.id().as_str().to_owned())
            .collect();
        listed.sort();
        assert_eq!(listed, ["CXC-B002", "CXC-F001"]);
    }

    #[tokio::test]
    async fn a_re_put_is_idempotent_upsert() {
        let store = MemoryArchiveStore::new();
        store.put("demo", &ticket("CXC-F001")).await.unwrap();
        store.put("demo", &ticket("CXC-F001")).await.unwrap();
        let listed = store.list("demo").await.unwrap();
        assert_eq!(listed.len(), 1, "re-put replaces, never duplicates");
    }

    #[test]
    fn the_env_gate_keeps_the_adapter_off_by_default() {
        // No env set in the test process: the adapter must not silently turn
        // itself on — production boots stay archive-less until configured.
        let built = MemoryArchiveStore::from_env().unwrap();
        assert!(
            built.is_none(),
            "the memory archive must be env-gated, never implicit"
        );
    }

    #[tokio::test]
    async fn a_seed_file_preloads_the_named_projects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seed.json");
        // Built from the aggregate's own serde shape (round-trip), so the
        // fixture can never drift from the schema the store reads.
        let seeded = [ticket("CXC-F001"), ticket("CXC-B002")];
        let seed = serde_json::json!({ "demo": seeded });
        std::fs::write(&path, serde_json::to_string(&seed).unwrap()).unwrap();
        let mut store = MemoryArchiveStore::new();
        store.load_seed_file(path.to_str().unwrap()).unwrap();
        let mut listed: Vec<String> = store
            .list("demo")
            .await
            .unwrap()
            .iter()
            .map(|t| t.id().as_str().to_owned())
            .collect();
        listed.sort();
        assert_eq!(listed, ["CXC-B002", "CXC-F001"]);
        let got = store.get("demo", "CXC-B002").await.unwrap().unwrap();
        assert_eq!(got.title(), "ticket CXC-B002");
    }

    #[test]
    fn an_unreadable_seed_file_is_refused_not_swallowed() {
        let mut store = MemoryArchiveStore::new();
        let err = store
            .load_seed_file("/coxagent-definitely-not-here/seed.json")
            .unwrap_err();
        assert!(err.to_string().contains("archive seed read"));
    }
}
