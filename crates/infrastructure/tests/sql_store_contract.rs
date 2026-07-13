//! `SqlStateStore` runs the same `StateStorePort` contract as the JSON store —
//! Liskov substitutability against a real Postgres. Skipped unless
//! `COXAGENT_TEST_PG_DSN` is set (no database in ordinary CI), so it is a no-op
//! by default and a full integration check when a DSN is provided.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::ProjectState;
use coxagent_domain::{Complexity, Priority, SemVer, Ticket, TicketId, TicketType};
use coxagent_infrastructure::SqlStateStore;

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

#[tokio::test]
async fn sql_store_satisfies_contract() {
    let Ok(dsn) = std::env::var("COXAGENT_TEST_PG_DSN") else {
        eprintln!("COXAGENT_TEST_PG_DSN unset — skipping Postgres contract test");
        return;
    };
    // Unique project id per run so repeated runs against the same DB are clean.
    let pid = format!("test-{}", std::process::id());
    let store = SqlStateStore::connect(&dsn, &pid)
        .await
        .expect("connect + migrate");

    // 1. Empty store loads the default state.
    assert_eq!(store.load().await.expect("load default"), ProjectState::default());

    // 2. Round-trip.
    let state = ProjectState {
        current_version: SemVer::new(1, 2, 3),
        tickets: vec![sample_ticket("FEAT-001")],
        ..ProjectState::default()
    };
    store.save(&state).await.expect("save");
    assert_eq!(store.load().await.expect("reload"), state);

    // 3. Overwrite replaces, not appends.
    let mut state2 = state.clone();
    state2.tickets.push(sample_ticket("FEAT-002"));
    store.save(&state2).await.expect("save 2");
    assert_eq!(store.load().await.expect("reload 2").tickets.len(), 2);

    // 4. Invalid state (dup ids) is rejected and does not clobber good state.
    let mut bad = ProjectState::default();
    bad.tickets.push(sample_ticket("DUP-1"));
    bad.tickets.push(sample_ticket("DUP-1"));
    assert!(store.save(&bad).await.is_err());
    assert_eq!(store.load().await.expect("reload after bad").tickets.len(), 2);

    // 5. Isolation: a different project id sees its own (default) state.
    let other = SqlStateStore::connect(&dsn, format!("{pid}-other"))
        .await
        .expect("connect other");
    assert_eq!(other.load().await.expect("other load"), ProjectState::default());
}
