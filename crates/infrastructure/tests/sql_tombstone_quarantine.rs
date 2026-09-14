//! CXA-C023 — the durable delete tombstone and the durable quarantine ledger
//! for the shared Postgres store adapter:
//!
//!   * `delete()` arms `project_tombstone` in the SAME transaction as the
//!     CXA-B130 row purge and purges the project's `project_quarantine` rows,
//!     so the DB-level tombstone closes the cross-process late-write
//!     resurrection window the in-process `deleted` flag could only narrow
//!     (the hole `sql_store.rs` itself documented).
//!   * A store connected BEFORE the delete (a zombie in another process) is
//!     refused by the single guarded upsert and writes nothing.
//!   * A FRESH connect for the same id clears the tombstone — the recreation
//!     path stays unbroken.
//!   * A refusal quarantined on one instance is returned by `quarantined()` on
//!     a fresh instance — the trail no longer dies with the process.
//!
//! Runs against its own ephemeral Postgres claimed from the shared compose
//! fixture (`common::TestDb`, CXA-F327): no exported DSN is honored, an
//! unprovisionable database fails red naming the fixture, and a docker-less
//! environment skips explicitly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::{DocPage, ProjectState};
use coxagent_domain::{Complexity, Priority, SemVer, Ticket, TicketId, TicketType};
use coxagent_infrastructure::SqlStateStore;
use tokio_postgres::NoTls;

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

async fn connect_store(db: &common::TestDb, project: &str) -> SqlStateStore {
    // The fixture ran the schema-init migration alone at claim time, so
    // concurrent connects here never race the catalog.
    SqlStateStore::connect(&db.dsn(), project)
        .await
        .expect("connect + migrate against the ephemeral test database")
}

/// A state with two doc pages sharing one id — an integrity finding the write
/// boundary can NOT self-heal (which copy survives is a human decision), so a
/// save carrying it is refused and quarantined. Same fixture shape the F229
/// audit tests use.
fn state_with_duplicate_doc_id() -> ProjectState {
    let mut state = ProjectState {
        tickets: vec![sample_ticket("C023-001")],
        ..ProjectState::default()
    };
    for _ in 0..2 {
        state.docs.push(DocPage {
            id: "dup-doc".to_owned(),
            folder: String::new(),
            category: "product".to_owned(),
            title: "t".to_owned(),
            body: String::new(),
            updated_at: String::new(),
            updated_by: String::new(),
        });
    }
    state
}

/// Raw access for the row-level assertions the port API deliberately does not
/// expose (the tombstone arming and the ledger purge are adapter-internal).
async fn raw(dsn: &str) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(dsn, NoTls)
        .await
        .expect("raw connection to the ephemeral test database");
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("raw connection error: {e}");
        }
    });
    client
}

async fn count(client: &tokio_postgres::Client, sql: &str, pid: &str) -> i64 {
    client
        .query_one(sql, &[&pid])
        .await
        .expect("count query")
        .get::<_, i64>(0)
}

/// AC1: `delete` arms the tombstone atomically with the purge — the tombstone
/// row is present while the state, coordination and quarantine rows are all
/// gone, and a neighbour project in the same shared store is untouched.
#[tokio::test]
async fn delete_arms_the_tombstone_and_purges_every_footprint_atomically() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("c023-del-{}", std::process::id());
    let store = connect_store(&db, &pid).await;

    let state = ProjectState {
        current_version: SemVer::new(1, 0, 0),
        tickets: vec![sample_ticket("C023-DEL")],
        ..ProjectState::default()
    };
    store.save(&state).await.expect("save");

    // A neighbour project keeps its own aggregate AND its own ledger rows.
    let other = connect_store(&db, &format!("{pid}-other")).await;
    let other_state = ProjectState {
        tickets: vec![sample_ticket("C023-OTHER")],
        ..ProjectState::default()
    };
    other.save(&other_state).await.expect("save other");
    assert!(
        other.save(&state_with_duplicate_doc_id()).await.is_err(),
        "the neighbour's refused write seeds its durable quarantine ledger"
    );

    // The project under delete quarantines one refusal of its own — the row
    // the delete transaction must purge.
    assert!(
        store.save(&state_with_duplicate_doc_id()).await.is_err(),
        "the unhealable duplicate-doc payload is refused and quarantined"
    );
    assert_eq!(store.quarantined().await.len(), 1, "ledger holds the entry");

    store.delete().await.expect("delete");

    let client = raw(&db.dsn()).await;
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_tombstone WHERE project_id = $1",
            &pid
        )
        .await,
        1,
        "the delete tombstone is armed in the same commit as the purge"
    );
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_state WHERE project_id = $1",
            &pid
        )
        .await,
        0,
        "the aggregate row is purged (CXA-B130 footprint)"
    );
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_coord WHERE project_id = $1",
            &pid
        )
        .await,
        0,
        "the coordination rows are purged (CXA-B130 footprint)"
    );
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_quarantine WHERE project_id = $1",
            &pid
        )
        .await,
        0,
        "the deleted project's ledger rows are purged with it"
    );
    assert!(
        store.quarantined().await.is_empty(),
        "the purged ledger is visible through the port"
    );

    // Isolation: only THIS project's rows died.
    assert_eq!(
        other.load().await.expect("other load after delete"),
        other_state,
        "another project's state in the same shared store must be untouched"
    );
    assert_eq!(
        other.quarantined().await.len(),
        1,
        "another project's ledger rows must be untouched"
    );
}

/// AC2 — THE new guarantee: a store connected BEFORE the delete (a zombie
/// writer in another process, blind to the in-process `deleted` flag) is
/// refused by the DB-level tombstone after the delete, writes nothing, and
/// `load` stays the default — and even a corrupt-payload refusal on the zombie
/// leaks nothing into the purged id's durable ledger.
#[tokio::test]
async fn a_zombie_store_connected_before_the_delete_refuses_save_after_it() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("c023-zombie-{}", std::process::id());

    // Connected BEFORE the delete — it never learns the project died.
    let zombie = connect_store(&db, &pid).await;
    let seeded = ProjectState {
        current_version: SemVer::new(2, 0, 0),
        tickets: vec![sample_ticket("C023-Z1")],
        ..ProjectState::default()
    };
    zombie.save(&seeded).await.expect("seed rev 1");

    // Another process's store performs the delete (its connect cleared no
    // tombstone — none was armed yet — so this is the plain delete path).
    let killer = connect_store(&db, &pid).await;
    killer.delete().await.expect("delete");

    // The zombie's late phase-end save must be REFUSED at the database, not
    // silently re-INSERT the purged row.
    let late = ProjectState {
        current_version: SemVer::new(2, 1, 0),
        tickets: vec![sample_ticket("C023-Z2")],
        ..ProjectState::default()
    };
    match zombie.save(&late).await {
        Err(e) => assert!(
            e.to_string().contains("project was deleted"),
            "the refusal must reuse the write-refused envelope: {e}"
        ),
        Ok(()) => panic!("a zombie save after a cross-process delete must be refused"),
    }
    // The revision-guarded path hits the same tombstone guard.
    match zombie.save_expecting(&late, Some(1)).await {
        Err(e) => assert!(
            e.to_string().contains("project was deleted"),
            "save_expecting is refused by the same tombstone, not a conflict: {e}"
        ),
        Ok(()) => panic!("a zombie save_expecting after a cross-process delete must be refused"),
    }
    assert_eq!(
        zombie.load().await.expect("zombie load after delete"),
        ProjectState::default(),
        "the refused writes must have written nothing"
    );

    let client = raw(&db.dsn()).await;
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_state WHERE project_id = $1",
            &pid
        )
        .await,
        0,
        "no late write may resurrect the purged row"
    );

    // A zombie refusal on a CORRUPT payload must not leak into the purged
    // id's durable ledger either: the delete transaction purged its rows, and
    // a recreated id must not inherit the stale entry in its audit view.
    assert!(
        zombie.save(&state_with_duplicate_doc_id()).await.is_err(),
        "the unhealable payload stays refused for the zombie too"
    );
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_quarantine WHERE project_id = $1",
            &pid
        )
        .await,
        0,
        "a post-delete refusal must not write ledger rows for a purged id"
    );
    let recreated = connect_store(&db, &pid).await;
    assert!(
        recreated.quarantined().await.is_empty(),
        "the recreated id's audit view must not inherit the zombie's stale refusal"
    );
}

/// AC3: a fresh connect for the same id clears the tombstone and the recreated
/// project saves fresh — the recreation path is unbroken (the tombstone is
/// cleared ONLY at connect, the one moment an id legitimately comes back).
#[tokio::test]
async fn a_fresh_connect_clears_the_tombstone_and_recreation_saves_fresh() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("c023-recreate-{}", std::process::id());

    let original = connect_store(&db, &pid).await;
    let stale = ProjectState {
        tickets: vec![sample_ticket("C023-OLD")],
        ..ProjectState::default()
    };
    original.save(&stale).await.expect("seed the original team");
    connect_store(&db, &pid)
        .await
        .delete()
        .await
        .expect("delete arms the tombstone");

    // Recreation under the same id: a FRESH store (new connect) clears the
    // tombstone and starts from nothing.
    let recreated = connect_store(&db, &pid).await;
    assert_eq!(
        recreated.load().await.expect("load after recreate"),
        ProjectState::default(),
        "the recreated id must start from the default, not the deleted team"
    );
    let fresh = ProjectState {
        current_version: SemVer::new(3, 0, 0),
        tickets: vec![sample_ticket("C023-NEW")],
        ..ProjectState::default()
    };
    recreated
        .save(&fresh)
        .await
        .expect("recreated project saves");
    assert_eq!(
        recreated
            .load()
            .await
            .expect("reload after recreate")
            .tickets
            .len(),
        1
    );
    assert_eq!(
        recreated.current_version().await.expect("revision"),
        Some(1),
        "the recreated aggregate starts at the baseline revision"
    );

    let client = raw(&db.dsn()).await;
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_tombstone WHERE project_id = $1",
            &pid
        )
        .await,
        0,
        "the fresh connect cleared the tombstone"
    );
}

/// AC5: a refusal recorded on one instance is returned by `quarantined()` on a
/// fresh instance — the ledger is durable (the old memory-only ledger returned
/// an empty trail here) and stays bounded at the newest 50 per project.
#[tokio::test]
async fn a_refusal_recorded_on_one_instance_is_returned_by_a_fresh_instance() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("c023-quar-{}", std::process::id());
    let store = connect_store(&db, &pid).await;

    let bad = state_with_duplicate_doc_id();
    for _ in 0..55 {
        assert!(
            store.save(&bad).await.is_err(),
            "every unhealable payload is refused"
        );
    }

    // Bounded like the in-memory ledger: the newest 50, oldest first.
    let recent = store.quarantined().await;
    assert_eq!(recent.len(), 50, "the durable ledger stays bounded");
    assert_eq!(
        recent[0].rule_id, "duplicate_doc_id",
        "the recorded rule id survives the round-trip"
    );
    assert!(
        recent[0].payload.contains("dup-doc"),
        "the attempted payload itself is recorded"
    );

    // THE durability: a FRESH instance reads the same trail across the table.
    let fresh = connect_store(&db, &pid).await;
    let durable = fresh.quarantined().await;
    assert_eq!(
        durable.len(),
        50,
        "the trail survives the process that recorded it"
    );
    assert_eq!(durable[49].rule_id, "duplicate_doc_id");
    assert!(
        durable[49].payload.contains("dup-doc"),
        "newest last, same ordering contract as the in-memory ledger"
    );
}
