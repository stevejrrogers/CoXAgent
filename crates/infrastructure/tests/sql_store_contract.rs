//! `SqlStateStore` runs the same `StateStorePort` contract as the JSON store —
//! Liskov substitutability against a real Postgres. The tests are `#[ignore]`d
//! (ordinary CI without a database skips them explicitly) and run only through
//! the fail-closed guard in `tests/common/mod.rs`, which refuses any target
//! but an ephemeral `cxa_test*` Postgres — never a live hub, never an
//! unverified one (CXA-F326).
//!
//! Run with an ephemeral database (README → Integration test environment):
//!
//! ```sh
//! COXAGENT_TEST_PG_DSN='postgres://cox:test@localhost:55432/cxa_test' \
//!   cargo test -p coxagent-infrastructure --test sql_store_contract -- --ignored
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
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

/// The guard already ran the first migration alone (its probe serializes the
/// initial `CREATE TABLE` through a process-wide cell), so a plain connect
/// never races the catalog even though the contract tests run in parallel.
async fn connect_store(dsn: &str, pid: &str) -> SqlStateStore {
    SqlStateStore::connect(dsn, pid)
        .await
        .expect("connect + migrate")
}

#[tokio::test]
#[ignore = "skipped: needs an ephemeral cxa_test* Postgres — see README (Integration test environment)"]
async fn sql_store_satisfies_contract() {
    let pg = common::pg("sql_store_contract").await;
    let pid = pg.project("contract");
    let store = connect_store(&pg.dsn, &pid).await;

    // 1. Empty store loads the default state.
    assert_eq!(
        store.load().await.expect("load default"),
        ProjectState::default()
    );

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
    assert_eq!(
        store.load().await.expect("reload after bad").tickets.len(),
        2
    );

    // 5. Isolation: a different project id sees its own (default) state.
    let other = connect_store(&pg.dsn, &format!("{pid}-other")).await;
    assert_eq!(
        other.load().await.expect("other load"),
        ProjectState::default()
    );

    // Teardown: the namespaced fixtures must not outlive the test.
    store.delete().await.expect("teardown sweep");
}

/// CXA-F003 AC4: optimistic concurrency is stronger than "reload latest at
/// write time". A caller supplying a stale captured revision must get
/// [`PortError::Conflict`] rather than silently overwriting newer data, while
/// the current revision still commits — exactly how an optimistic retry
/// converges after seeing a conflict.
#[tokio::test]
#[ignore = "skipped: needs an ephemeral cxa_test* Postgres — see README (Integration test environment)"]
async fn sql_store_rejects_stale_revision_write_with_conflict() {
    let pg = common::pg("sql_store_contract").await;
    let pid = pg.project("occ");
    let store = connect_store(&pg.dsn, &pid).await;

    // An unwritten project exposes baseline revision 0.
    assert_eq!(
        store.current_version().await.expect("empty version"),
        Some(0),
        "an unwritten project exposes baseline revision 0"
    );

    // First writer captured 0 and saves against it -> lands at revision 1.
    let first = ProjectState {
        current_version: SemVer::new(2, 0, 0),
        tickets: vec![sample_ticket("F003-001")],
        ..ProjectState::default()
    };
    store
        .save_expecting(&first, Some(0))
        .await
        .expect("first save with expected 0 succeeds");
    assert_eq!(store.current_version().await.expect("after first"), Some(1));

    // A second writer captured rev 1 too and races ahead of us, bumping to 2.
    let theirs = ProjectState {
        current_version: SemVer::new(2, 1, 0),
        tickets: vec![sample_ticket("F003-002")],
        ..ProjectState::default()
    };
    store
        .save_expecting(&theirs, Some(1))
        .await
        .expect("concurrent writer saves against its own held rev");
    assert_eq!(
        store.current_version().await.expect("after concurrent"),
        Some(2)
    );

    // We still hold rev 1 but the row is now at 2. A blind save would clobber
    // their update; with the guard it must conflict and change nothing.
    let ours_stale = ProjectState {
        current_version: SemVer::new(2, 2, 0),
        tickets: vec![sample_ticket("F003-003")],
        ..ProjectState::default()
    };
    match store.save_expecting(&ours_stale, Some(1)).await {
        Err(PortError::Conflict(_)) => {}
        other => panic!("expected Conflict for stale rev-1 write over rev-2 row — got {other:?}"),
    }

    // The rejected write changed nothing — their winning state survives.
    let final_state = store.load().await.expect("reload after rejected write");
    assert_eq!(final_state.tickets.len(), 1);
    assert_eq!(final_state.tickets[0].id().as_str(), "F003-002");

    // Retrying with the CURRENT revision (reload-then-save) converges cleanly.
    store
        .save_expecting(&ours_stale, Some(2))
        .await
        .expect("retry with current revision succeeds");

    // Teardown: the namespaced fixtures must not outlive the test.
    store.delete().await.expect("teardown sweep");
}

/// CXA-B130: `delete` must purge the project's ENTIRE persisted footprint in
/// the shared store — the aggregate row AND its coordination rows — so a
/// project recreated under the same id starts fresh instead of silently
/// adopting the deleted team's tickets, spend and desired-run state.
#[tokio::test]
#[ignore = "skipped: needs an ephemeral cxa_test* Postgres — see README (Integration test environment)"]
async fn sql_store_delete_purges_state_and_coordination_for_the_project_only() {
    let pg = common::pg("sql_store_contract").await;
    let pid = pg.project("delete");
    let store = connect_store(&pg.dsn, &pid).await;

    // Seed everything a lived-in project leaves behind: the aggregate, the
    // operator's persistent desired-run state, and a worker-registry beat.
    let state = ProjectState {
        tickets: vec![sample_ticket("B130-001")],
        ..ProjectState::default()
    };
    store.save(&state).await.expect("save");
    store
        .set_desired("op@host", true)
        .await
        .expect("persist desired run state");
    store
        .heartbeat_worker(
            "dev@host",
            "worker",
            "B130-001",
            &coxagent_application::ports::outbound::WorkerCaps::default(),
            "2026-08-31T00:00:00Z",
        )
        .await
        .expect("heartbeat");

    // A neighbour project must be untouched by this project's delete.
    let other = connect_store(&pg.dsn, &format!("{pid}-other")).await;
    let other_state = ProjectState {
        tickets: vec![sample_ticket("B130-OTHER")],
        ..ProjectState::default()
    };
    other.save(&other_state).await.expect("save other");

    store.delete().await.expect("delete");

    // The store behaves as never-written: default aggregate, baseline revision.
    assert_eq!(
        store.load().await.expect("load after delete"),
        ProjectState::default(),
        "the deleted project's aggregate row must be gone"
    );
    assert_eq!(
        store.current_version().await.expect("version after delete"),
        Some(0),
        "the revision must be back at the baseline — a recreated id starts fresh"
    );
    assert_eq!(
        store
            .get_desired("op@host")
            .await
            .expect("desired after delete"),
        None,
        "the persistent desired-run state must be purged with the project, or \
         a recreated id auto-resumes the deleted project's runner"
    );
    assert!(
        store
            .workers()
            .await
            .expect("workers after delete")
            .is_empty(),
        "the worker registry rows must be purged with the project"
    );

    // Isolation: only THIS project's rows died.
    assert_eq!(
        other.load().await.expect("other load after delete"),
        other_state,
        "another project's state in the same shared store must be untouched"
    );

    // A runner cycle in flight at delete time checks `STOPPED` only at the
    // cycle boundary — its late phase-end save must be REFUSED, not silently
    // re-INSERT the deleted row (the resurrection would be invisible: the
    // caller believes it saved).
    match store.save(&state).await {
        Err(_) => {}
        Ok(()) => panic!("a save after delete must be refused, not resurrect the row"),
    }
    assert_eq!(
        store.load().await.expect("load after refused save"),
        ProjectState::default(),
        "the refused save must have written nothing"
    );
    assert!(
        !store
            .claim_ticket(&TicketId::new("B130-001").expect("id"), "dev@host", "now")
            .await
            .expect("claim after delete"),
        "a late claim must not win — and must not recreate the row"
    );
    assert_eq!(
        store.load().await.expect("load after refused claim"),
        ProjectState::default()
    );

    // Idempotent: deleting an already-purged project is a clean success.
    store.delete().await.expect("second delete");

    // Teardown: the neighbour's namespaced fixtures must not outlive the test.
    other
        .delete()
        .await
        .expect("teardown sweep for the neighbour");
}
