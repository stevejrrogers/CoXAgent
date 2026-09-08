//! `SqlStateStore` runs the same `StateStorePort` contract as the JSON store —
//! Liskov substitutability against a real Postgres. Every test claims its own
//! ephemeral database from the shared compose fixture (`common::TestDb`,
//! CXA-F327): no exported DSN is honored, an unprovisionable database fails
//! red naming the fixture, and a docker-less environment skips explicitly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::{
    ChatMsg, DocPage, ProjectState, ShardData, ShardKind, SocialShard, StateShard, GENERAL_CHANNEL,
    SCHEMA_VERSION,
};
use coxagent_application::PortError;
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

#[tokio::test]
async fn sql_store_satisfies_contract() {
    // `None` is the docker-absent explicit skip — the only lawful green
    // non-run, with its reason already printed by the fixture.
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    // Unique project id per run so repeated runs against the same DB are clean.
    let pid = format!("test-{}", std::process::id());
    let store = connect_store(&db, &pid).await;

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
    let other = connect_store(&db, &format!("{pid}-other")).await;
    assert_eq!(
        other.load().await.expect("other load"),
        ProjectState::default()
    );
}

/// CXA-F003 AC4: optimistic concurrency is stronger than "reload latest at
/// write time". A caller supplying a stale captured revision must get
/// [`PortError::Conflict`] rather than silently overwriting newer data, while
/// the current revision still commits — exactly how an optimistic retry
/// converges after seeing a conflict.
#[tokio::test]
async fn sql_store_rejects_stale_revision_write_with_conflict() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("occ-test-{}", std::process::id());
    let store = connect_store(&db, &pid).await;

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
}

/// CXA-B130: `delete` must purge the project's ENTIRE persisted footprint in
/// the shared store — the aggregate row AND its coordination rows — so a
/// project recreated under the same id starts fresh instead of silently
/// adopting the deleted team's tickets, spend and desired-run state.
#[tokio::test]
async fn sql_store_delete_purges_state_and_coordination_for_the_project_only() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("del-test-{}", std::process::id());
    let store = connect_store(&db, &pid).await;

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
    let other = connect_store(&db, &format!("{pid}-other")).await;
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
}

// ── CXA-C019b: shard-native reads and writes over the SQL adapter ──────────

/// One populated field per bounded-context family, so every shard column
/// carries non-default data and "only this shard changed" is observable.
fn full_state() -> ProjectState {
    let mut state = ProjectState {
        current_version: SemVer::new(9, 9, 9),
        tickets: vec![sample_ticket("C019B-001")],
        ..ProjectState::default()
    };
    state.chat = vec![chat_msg("m1", "seeded social")];
    state.docs = vec![DocPage {
        id: "d1".to_owned(),
        folder: "Technical/Architecture".to_owned(),
        category: "technical".to_owned(),
        title: "State shards".to_owned(),
        body: "Five bounded contexts.".to_owned(),
        updated_at: "2026-09-07T00:00:00Z".to_owned(),
        updated_by: "SA".to_owned(),
    }];
    state.lessons = vec!["seeded governance".to_owned()];
    state.ops_down = true;
    state
}

fn chat_msg(id: &str, body: &str) -> ChatMsg {
    ChatMsg {
        id: id.to_owned(),
        at: "2026-09-07T00:00:00Z".to_owned(),
        user: "operator".to_owned(),
        body: body.to_owned(),
        edited: None,
        channel: GENERAL_CHANNEL.to_owned(),
        attachments: Vec::new(),
        reactions: Vec::new(),
        thread_id: None,
        reply_count: 0,
        deleted: false,
    }
}

/// A Social shard whose only content is one chat message with `body` — the
/// payload the shard-save tests merge in.
fn social_shard_with(body: &str) -> StateShard {
    let mut social = SocialShard::default();
    social.chat.push(chat_msg("m1", body));
    StateShard::new(ShardData::Social(social))
}

/// Raw access for the row-level assertions the port API deliberately does not
/// expose (shard-column contents are adapter-internal) — the same pattern the
/// tombstone suite uses.
async fn raw(dsn: &str) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(dsn, NoTls)
        .await
        .expect("raw connection to the ephemeral test database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// Projection: a never-written project serves fresh slices (the Work shard at
/// the CURRENT schema version, never 0), and after a full save every shard
/// read equals that aggregate's own slice of it.
#[tokio::test]
async fn sql_store_shard_reads_serve_fresh_slices_then_each_saved_shard() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let store = connect_store(&db, "c019b-projection").await;

    for kind in ShardKind::ALL {
        assert_eq!(
            store.load_shard(kind).await.expect("fresh shard"),
            ProjectState::default().shard(kind),
            "a never-written project's {kind:?} must read as a fresh slice"
        );
    }
    let ShardData::Work(fresh_work) = store.load_shard(ShardKind::Work).await.expect("work").data
    else {
        panic!("the Work kind must serve a Work payload");
    };
    assert_eq!(
        fresh_work.schema_version, SCHEMA_VERSION,
        "a fresh Work shard starts at the current schema version, never 0 — \
         a shard-native writer must produce a document the stores accept"
    );

    let state = full_state();
    store.save(&state).await.expect("seed save");
    for kind in ShardKind::ALL {
        assert_eq!(
            store.load_shard(kind).await.expect("load_shard"),
            state.shard(kind),
            "shard {kind:?} must project the saved aggregate"
        );
    }
}

/// Native-path proof: a payload that exists ONLY in the shard column (the
/// envelope was deliberately left behind) is what `load_shard` serves — the
/// read costs one column, not the whole document.
#[tokio::test]
async fn sql_store_load_shard_reads_the_native_column_not_the_envelope() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = "c019b-native";
    let store = connect_store(&db, pid).await;
    store.save(&full_state()).await.expect("seed save");

    let mut native_only = SocialShard::default();
    native_only.chat.push(chat_msg("m9", "native column only"));
    let payload = serde_json::to_value(&native_only).expect("social payload");
    raw(&db.dsn())
        .await
        .execute(
            "UPDATE project_state SET shard_social = $1 WHERE project_id = $2",
            &[&payload, &pid],
        )
        .await
        .expect("write the native-only column");

    let shard = store
        .load_shard(ShardKind::Social)
        .await
        .expect("native read");
    assert_eq!(
        shard.data,
        ShardData::Social(native_only.clone()),
        "the native column is the read path"
    );
    let envelope = store.load().await.expect("envelope read");
    assert!(
        !envelope.chat.iter().any(|m| m.id == "m9"),
        "the envelope must not contain the native-only write — the two paths are distinct"
    );
}

/// Rollback path: an unbackfilled (NULL) shard column falls back to
/// projecting the legacy `data` envelope, so pre-shard rows — and an abandoned
/// column — keep serving honest shard reads.
#[tokio::test]
async fn sql_store_load_shard_falls_back_to_the_envelope_when_the_column_is_unbackfilled() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = "c019b-fallback";
    let store = connect_store(&db, pid).await;
    let state = full_state();
    store.save(&state).await.expect("seed save");

    raw(&db.dsn())
        .await
        .execute(
            "UPDATE project_state SET shard_governance = NULL WHERE project_id = $1",
            &[&pid],
        )
        .await
        .expect("unbackfill the governance column");

    assert_eq!(
        store
            .load_shard(ShardKind::Governance)
            .await
            .expect("fallback read"),
        state.shard(ShardKind::Governance),
        "a NULL shard column must project the envelope, never serve defaults"
    );
}

/// Isolation + envelope coherence: a shard-scoped save lands only that
/// shard's fields, leaves every other shard byte-identical, and the legacy
/// envelope reflects the merge — so envelope readers never miss a shard
/// write during the dual-write phase.
#[tokio::test]
async fn sql_store_save_shard_merges_only_its_own_shard_and_keeps_the_envelope_coherent() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let store = connect_store(&db, "c019b-isolation").await;
    let before = full_state();
    store.save(&before).await.expect("seed save");

    // Mutate ONLY the social payload of the seeded aggregate.
    let mut social = match before.shard(ShardKind::Social).data {
        ShardData::Social(social) => social,
        other => panic!("the Social kind must serve a Social payload, got {other:?}"),
    };
    social.chat.push(chat_msg("m2", "shard-scoped write"));
    let social_save = StateShard::new(ShardData::Social(social));
    store.save_shard(&social_save).await.expect("save_shard");

    assert_eq!(
        store.load_shard(ShardKind::Social).await.expect("social"),
        social_save,
        "the shard's own fields landed"
    );
    for kind in [
        ShardKind::Work,
        ShardKind::Docs,
        ShardKind::Governance,
        ShardKind::Ops,
    ] {
        assert_eq!(
            store.load_shard(kind).await.expect("other shard"),
            before.shard(kind),
            "shard {kind:?} must survive a Social save untouched"
        );
    }

    let mut expected = before.clone();
    expected.with_shard(social_save);
    assert_eq!(
        store.load().await.expect("envelope reload"),
        expected,
        "the envelope must reflect the shard merge — no lost or defaulted fields"
    );

    // The merged document still passes the write-boundary integrity audit as
    // a whole: a full save of it round-trips.
    let merged = store.load().await.expect("merged");
    store
        .save(&merged)
        .await
        .expect("full save of merged state");
}

/// Optimistic concurrency: `save_shard_expecting` CASes the ENVELOPE
/// revision — a stale caller conflicts and changes nothing (no shard column,
/// no envelope, no revision), a fresh caller converges, and the unguarded
/// `save_shard` merge path still works.
#[tokio::test]
async fn sql_store_save_shard_expecting_conflicts_on_a_stale_revision_and_changes_nothing() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let store = connect_store(&db, "c019b-cas").await;
    store.save(&full_state()).await.expect("seed save");
    let captured = store.current_version().await.expect("version after seed");

    store
        .save_shard_expecting(&social_shard_with("first"), captured)
        .await
        .expect("a write against the captured revision wins");
    let after_first = store
        .current_version()
        .await
        .expect("version after first shard save");

    let refused = store
        .save_shard_expecting(&social_shard_with("stale"), captured)
        .await;
    assert!(
        matches!(refused, Err(PortError::Conflict(_))),
        "the now-stale captured revision must conflict, got {refused:?}"
    );
    assert_eq!(
        store.load_shard(ShardKind::Social).await.expect("social"),
        social_shard_with("first"),
        "the refused write must not touch the shard column"
    );
    assert_eq!(
        store.load().await.expect("envelope").chat[0].body,
        "first",
        "the refused write must not touch the envelope either"
    );
    assert_eq!(
        store
            .current_version()
            .await
            .expect("version after refusal"),
        after_first,
        "the refused write must not advance the revision"
    );

    store
        .save_shard_expecting(&social_shard_with("fresh"), after_first)
        .await
        .expect("a write against the current revision converges");
    store
        .save_shard(&social_shard_with("scoped"))
        .await
        .expect("the unguarded merge path still works");
    assert_eq!(
        store.load_shard(ShardKind::Social).await.expect("social"),
        social_shard_with("scoped")
    );
    assert_eq!(
        store.load().await.expect("envelope").chat[0].body,
        "scoped",
        "the unguarded merge is coherent in the envelope too"
    );
}
