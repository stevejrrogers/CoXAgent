//! Contract test suite for `StateStorePort`. Any adapter (JSON now, remote
//! later) must pass this same suite — this is how Liskov substitutability is
//! verified in practice, not asserted in prose.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::ProjectState;
use coxagent_domain::{Complexity, Priority, SemVer, Ticket, TicketId, TicketType};
use coxagent_infrastructure::JsonStateStore;

fn sample_ticket(id: &str) -> Ticket {
    Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Feature,
        "A feature",
        "desc",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

/// The reusable contract. Adapters call this from their own integration test.
async fn state_store_contract<S: StateStorePort>(store: S) {
    // 1. Empty store loads the default state.
    let loaded = store.load().await.expect("load default");
    assert_eq!(loaded, ProjectState::default());

    // 2. Round-trip: save then load yields an equal aggregate.
    let state = ProjectState {
        current_version: SemVer::new(1, 2, 3),
        tickets: vec![sample_ticket("FEAT-001")],
        ..ProjectState::default()
    };
    store.save(&state).await.expect("save");
    let back = store.load().await.expect("reload");
    assert_eq!(back, state);

    // 3. Overwrite: a second save replaces, not appends.
    let mut state2 = state.clone();
    state2.tickets.push(sample_ticket("FEAT-002"));
    store.save(&state2).await.expect("save 2");
    let back2 = store.load().await.expect("reload 2");
    assert_eq!(back2.tickets.len(), 2);

    // 4. Validation: invalid state (dup ids) is rejected, not persisted.
    let mut bad = ProjectState::default();
    bad.tickets.push(sample_ticket("DUP-1"));
    bad.tickets.push(sample_ticket("DUP-1"));
    assert!(store.save(&bad).await.is_err());
    // Previous good state is untouched.
    let after_bad = store.load().await.expect("reload after bad");
    assert_eq!(after_bad.tickets.len(), 2);
}

#[tokio::test]
async fn json_store_satisfies_contract() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonStateStore::new(dir.path()).expect("store");
    state_store_contract(store).await;
}

#[tokio::test]
async fn json_store_recovers_corrupt_state_from_backup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonStateStore::new(dir.path()).expect("store");

    // First save (no backup yet), then a second save which backs up the first.
    let mut a = ProjectState::default();
    a.tickets.push(sample_ticket("FEAT-001"));
    store.save(&a).await.expect("save a");
    let mut b = a.clone();
    b.tickets.push(sample_ticket("FEAT-002"));
    store.save(&b).await.expect("save b");

    // Corrupt the live state file.
    std::fs::write(dir.path().join("state.json"), b"{ not json").expect("corrupt");

    // Load recovers the newest good backup (state A, one ticket).
    let recovered = store.load().await.expect("recover");
    assert_eq!(recovered.tickets.len(), 1);
    assert_eq!(recovered.tickets[0].id().as_str(), "FEAT-001");
}

#[tokio::test]
async fn json_store_creates_backup_on_overwrite() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonStateStore::new(dir.path()).expect("store");

    let mut state = ProjectState::default();
    state.tickets.push(sample_ticket("FEAT-001"));
    store.save(&state).await.expect("first save");
    state.tickets.push(sample_ticket("FEAT-002"));
    store.save(&state).await.expect("second save");

    let backups = dir.path().join(".backups");
    let count = std::fs::read_dir(&backups).expect("backup dir").count();
    assert_eq!(count, 1, "one backup written on the overwrite");
}
