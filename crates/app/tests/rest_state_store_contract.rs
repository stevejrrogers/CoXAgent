//! CXA-F033: `RestStateStore` runs the real `/api/projects/:pid/store` gateway.
//!
//! The runner-side REST adapter (crates/infrastructure/src/state/rest_store.rs)
//! must satisfy the same `StateStorePort` contract the JSON and Postgres
//! adapters satisfy, over the wire, against a hub booted exactly as a deploy
//! boots it (`serve_full`). Four hubs, each on its own fixed port so the tests
//! below (and the crate's other hub-booting tests) never race:
//!
//! 1. JSON-backed hub — the shared state-store contract (Liskov parity with
//!    `state_store_contract` and `sql_store_contract`).
//! 2. Second JSON-backed hub — every coordination op, proving the full
//!    twelve-op surface dispatches through the single `/store` endpoint.
//! 3. Revision-tracking hub — a stale `save_expecting` write comes back as
//!    HTTP 409 and lands on the caller as [`PortError::Conflict`] (CXA-F003
//!    optimistic concurrency surviving the adapter boundary).
//! 4. Auth-enabled hub — the bearer a runner configures travels on the wire,
//!    clears the middleware + handler gates for a manage-tier member, and an
//!    unauthenticated runner is refused with 401 (P5a, through the client).
//!
//! Fixtures and boot helpers live in `rest_store_support/`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod rest_store_support;

use std::sync::Arc;

use coxagent_application::ports::outbound::{StateStorePort, WorkerCaps};
use coxagent_application::{PortError, ProjectState};
use coxagent_domain::{SemVer, TicketId};
use coxagent_infrastructure::JsonStateStore;
use rest_store_support::{
    boot, handle, now_rfc3339, rest_store, sample_bug, sample_ticket, StubAuth, VersionedStore,
    ADMIN_BEARER, AUTH_PORT, COORD_PORT, JSON_PORT, MEMBER_BEARER, NOW, OCC_PORT,
};

/// AC1 + AC5, state half: a runner fronted at the gateway satisfies the very
/// contract the JSON store is tested against — empty load, round-trip,
/// overwrite, validation, and claim persistence — over real HTTP.
#[tokio::test]
async fn rest_store_satisfies_state_store_contract_over_the_gateway() {
    let state_dir = tempfile::tempdir().expect("tempdir");
    let json = JsonStateStore::new(state_dir.path()).expect("store");
    let _hub = boot(JSON_PORT, vec![handle(Arc::new(json))], None).await;
    let store = rest_store(JSON_PORT, None);

    // 1. Empty store loads the default state.
    assert_eq!(
        store.load().await.expect("load default"),
        ProjectState::default()
    );

    // 2. Round-trip: save then load yields an equal aggregate.
    let state = ProjectState {
        current_version: SemVer::new(1, 2, 3),
        tickets: vec![sample_ticket("FEAT-001")],
        ..ProjectState::default()
    };
    store.save(&state).await.expect("save");
    assert_eq!(store.load().await.expect("reload"), state);

    // 3. Overwrite: a second save replaces, not appends.
    let mut state2 = state.clone();
    state2.tickets.push(sample_ticket("FEAT-002"));
    store.save(&state2).await.expect("save 2");
    assert_eq!(store.load().await.expect("reload 2").tickets.len(), 2);

    // 4. Validation: invalid state (dup ids) is rejected, not persisted.
    let mut bad = ProjectState::default();
    bad.tickets.push(sample_ticket("DUP-1"));
    bad.tickets.push(sample_ticket("DUP-1"));
    assert!(store.save(&bad).await.is_err());
    assert_eq!(
        store.load().await.expect("reload after bad").tickets.len(),
        2
    );

    // 5. Ticket claims: a claimable Bug (Open) is won once, then held; a
    // missing ticket loses. The claim persists for the next reader.
    let mut with_bug = store.load().await.expect("reload with bug");
    with_bug.tickets.push(sample_bug("CXC-101"));
    store.save(&with_bug).await.expect("save bug");
    let id = TicketId::new("CXC-101").expect("id");
    assert!(store
        .claim_ticket(&id, "dev@mac", NOW)
        .await
        .expect("claim"));
    assert!(
        !store
            .claim_ticket(&id, "other@mac", NOW)
            .await
            .expect("second claim loses"),
        "an already-claimed ticket must not be re-won"
    );
    let missing = TicketId::new("CXC-999").expect("id");
    assert!(!store
        .claim_ticket(&missing, "dev@mac", NOW)
        .await
        .expect("missing"));
    let claimed = store.load().await.expect("reload claimed");
    let bug = claimed
        .tickets
        .iter()
        .find(|t| t.id().as_str() == "CXC-101")
        .expect("bug persisted");
    assert_eq!(bug.claimed_by(), Some("dev@mac"));
}

/// AC5, coordination half: leadership, stage leases, the worker registry, and
/// operator state all dispatch through the single `/store` endpoint. Runs on
/// its own hub — these ops touch only the coord file, no saved state needed.
#[tokio::test]
async fn rest_store_drives_coordination_ops_through_the_gateway() {
    let state_dir = tempfile::tempdir().expect("tempdir");
    let json = JsonStateStore::new(state_dir.path()).expect("store");
    let _hub = boot(COORD_PORT, vec![handle(Arc::new(json))], None).await;
    let store = rest_store(COORD_PORT, None);
    let id = TicketId::new("CXC-101").expect("id");

    // 1. Leadership: won, held against a rival, renewable by the holder.
    assert!(store.acquire_leader("dev@mac", NOW).await.expect("lead"));
    assert!(
        !store
            .acquire_leader("rival@mac", NOW)
            .await
            .expect("rival denied"),
        "a fresh lease must shut out another worker"
    );
    assert!(store.acquire_leader("dev@mac", NOW).await.expect("renew"));

    // 2. Stage leases: exclusive per (ticket, stage), independent across
    // stages.
    assert!(store
        .claim_stage(&id, "sa", "dev@mac", NOW)
        .await
        .expect("stage claim"));
    assert!(!store
        .claim_stage(&id, "sa", "rival@mac", NOW)
        .await
        .expect("rival stage denied"));
    assert!(store
        .claim_stage(&id, "pd", "rival@mac", NOW)
        .await
        .expect("other stage independent"));
    // 3. Release dispatches through the gateway and succeeds. (Every backend
    // today keeps `release_stage` at the port's no-op default — the lease
    // frees via TTL, not release — so a freed-then-reclaimed assertion here
    // would test behaviour no adapter implements.)
    store
        .release_stage(&id, "sa", "dev@mac")
        .await
        .expect("release");

    // 4. Worker registry: a heartbeat surfaces in `workers()` with what the
    // runner reported.
    let caps = WorkerCaps {
        engines: vec!["stub-engine".to_owned()],
        models: Vec::new(),
        git: None,
        tooling: None,
        version: "test".to_owned(),
    };
    let beat_at = now_rfc3339();
    store
        .heartbeat_worker("dev@mac", "be", "CXC-101", &caps, &beat_at)
        .await
        .expect("heartbeat");
    let workers = store.workers().await.expect("workers");
    assert_eq!(workers.len(), 1, "exactly the heartbeat just sent");
    assert_eq!(workers[0].worker, "dev@mac");
    assert_eq!(workers[0].role, "be");
    assert_eq!(workers[0].ticket, "CXC-101");
    assert_eq!(workers[0].at, beat_at);

    // 5. Operator state + single-instance lock: dispatched over the gateway.
    store
        .set_desired("operator", true)
        .await
        .expect("set_desired");
    // The JSON backend keeps desired-state at the port default (no-op/None);
    // the round-trip pins the wire shape both directions.
    assert_eq!(
        store.get_desired("operator").await.expect("get_desired"),
        None
    );
    assert!(store
        .acquire_operator("operator", "instance-1")
        .await
        .expect("acquire_operator"));

    // 6. The JSON backend tracks no revisions: `current_version` stays at
    // the port default (`None`), same answer the in-process adapter gives.
    assert_eq!(store.current_version().await.expect("version"), None);
}

/// AC4: optimistic concurrency survives the REST boundary. A runner that
/// captured a revision another writer has since advanced must see
/// [`PortError::Conflict`] — never a silent clobber — and converge by
/// retrying with the head revision. Mirrors
/// `sql_store_rejects_stale_revision_write_with_conflict`.
#[tokio::test]
async fn rest_store_rejects_stale_revision_write_with_conflict() {
    let versioned = Arc::new(VersionedStore::new());
    let backend: Arc<dyn StateStorePort> = versioned.clone();
    let _hub = boot(OCC_PORT, vec![handle(backend)], None).await;
    let store = rest_store(OCC_PORT, None);

    // An unwritten project exposes baseline revision 0.
    assert_eq!(
        store.current_version().await.expect("empty version"),
        Some(0)
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

    // A second writer captured rev 1 too and races ahead, bumping head to 2.
    let theirs = ProjectState {
        current_version: SemVer::new(2, 1, 0),
        tickets: vec![sample_ticket("F003-002")],
        ..ProjectState::default()
    };
    store
        .save_expecting(&theirs, Some(1))
        .await
        .expect("concurrent writer saves against its own held rev");
    assert_eq!(store.current_version().await.expect("after race"), Some(2));

    // We still hold rev 1 but the head is 2: the gateway must answer 409 and
    // the adapter must surface it as Conflict, not clobber their update.
    let ours_stale = ProjectState {
        current_version: SemVer::new(2, 2, 0),
        tickets: vec![sample_ticket("F003-003")],
        ..ProjectState::default()
    };
    match store.save_expecting(&ours_stale, Some(1)).await {
        Err(PortError::Conflict(_)) => {}
        other => {
            panic!("expected Conflict for a stale rev-1 write over a rev-2 head — got {other:?}")
        }
    }
    // The stale revision we captured is what actually crossed the wire.
    assert_eq!(
        versioned.last_expected(),
        Some(1),
        "the caller's expected revision must reach the store adapter"
    );

    // The rejected write changed nothing — their winning state survives.
    let final_state = store.load().await.expect("reload after rejection");
    assert_eq!(final_state.tickets.len(), 1);
    assert_eq!(final_state.tickets[0].id().as_str(), "F003-002");

    // Retrying with the CURRENT revision converges cleanly.
    store
        .save_expecting(&ours_stale, Some(2))
        .await
        .expect("retry with current revision succeeds");
}

/// AC3: with auth configured on the gateway, the runner's bearer travels on
/// the wire and clears the gates for a manage-tier member; unauthenticated or
/// wrong-credential runners are refused with 401, and a merely member-tier
/// principal with 403 — before any state is touched.
#[tokio::test]
async fn rest_store_presents_bearer_token_and_is_gated_without_it() {
    let state_dir = tempfile::tempdir().expect("tempdir");
    let json = JsonStateStore::new(state_dir.path()).expect("store");
    let _hub = boot(
        AUTH_PORT,
        vec![handle(Arc::new(json))],
        Some(Arc::new(StubAuth)),
    )
    .await;

    // The manage-tier member the gateway trusts: every op flows.
    let store = rest_store(AUTH_PORT, Some(ADMIN_BEARER));
    assert_eq!(
        store.load().await.expect("authed load"),
        ProjectState::default()
    );
    let state = ProjectState {
        current_version: SemVer::new(3, 0, 0),
        tickets: vec![sample_ticket("FEAT-001")],
        ..ProjectState::default()
    };
    store.save(&state).await.expect("authed save");
    assert_eq!(store.load().await.expect("authed reload"), state);

    // Member tier is authenticated AND a member of :pid, but /store writes
    // whole state snapshots: the manage bar must refuse it with 403.
    let member = rest_store(AUTH_PORT, Some(MEMBER_BEARER));
    let err = member
        .save(&state)
        .await
        .expect_err("member-tier write must be refused");
    assert!(err.to_string().contains("403"), "got: {err}");

    // No credential at all: 401 before the handler runs.
    let anonymous = rest_store(AUTH_PORT, None);
    let err = anonymous
        .load()
        .await
        .expect_err("unauthenticated load must be refused");
    assert!(err.to_string().contains("401"), "got: {err}");
    assert!(err.to_string().contains("unauthenticated"), "got: {err}");

    // A bearer the gateway does not know is exactly as good as none.
    let impostor = rest_store(AUTH_PORT, Some("bearer-forged"));
    let err = impostor
        .load()
        .await
        .expect_err("forged bearer must be refused");
    assert!(err.to_string().contains("401"), "got: {err}");
}
