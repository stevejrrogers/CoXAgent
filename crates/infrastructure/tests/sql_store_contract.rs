//! `SqlStateStore` runs the same `StateStorePort` contract as the JSON store —
//! Liskov substitutability against a real Postgres. Every test claims its own
//! ephemeral database from the shared compose fixture (`common::TestDb`,
//! CXA-F327): no exported DSN is honored, an unprovisionable database fails
//! red naming the fixture, and a docker-less environment skips explicitly.
//!
//! CXA-C019b adds the sharded-rows contract: per-shard JSONB storage with a
//! head-revision row, legacy single-blob migration, shard-diff writes and the
//! shard-scoped `claim_ticket` lock. The schema the tests inspect is
//! discovered from the adapter's own `INIT_SQL` via
//! `common::shard_schema_f300` — no invented identifiers.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::shard_schema_f300 as schema;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::{
    ChatMsg, DocPage, ProjectState, ShardData, ShardKind, SocialShard, StateShard, GENERAL_CHANNEL,
    SCHEMA_VERSION,
};
use coxagent_application::PortError;
use coxagent_domain::{
    Complexity, Priority, Role, SemVer, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};
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

// --- CXA-C019b: sharded rows --------------------------------------------------

use coxagent_application::state::DeployRecord;

/// A claimable ticket: the `distributed_coord` fixture shape (designed then
/// ready), since `Ticket::claim` requires a legal transition to InProgress.
fn ready_feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Feature,
        "A feature",
        "desc",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(Role::Sa, TechnicalDesign::default())
        .expect("design");
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t
}

/// A state with content in EVERY bounded-context shard, so the per-shard
/// assertions below each have a payload to observe.
fn lived_in_state() -> ProjectState {
    let mut state = ProjectState {
        tickets: vec![ready_feature("CXC-F300")],
        ..ProjectState::default()
    };
    state.post_comment_by("SM", "sm@host", "cycle kickoff", None);
    state.docs.push(coxagent_application::state::DocPage {
        id: "f300-doc".to_owned(),
        folder: String::new(),
        category: "product".to_owned(),
        title: "F300 doc".to_owned(),
        body: "shard fixture".to_owned(),
        updated_at: String::new(),
        updated_by: String::new(),
    });
    state.decisions.push("keep shards disjoint".to_owned());
    state.history.push(DeployRecord {
        version: SemVer::new(1, 0, 0),
        ticket: TicketId::new("CXC-F300").expect("valid id"),
        title: "first deploy".to_owned(),
        at: "2026-09-07T00:00:00Z".to_owned(),
    });
    state
}

// ── CXA-C019b: shard-native reads and writes over the SQL adapter ──────────

/// One populated field per bounded-context family, so every shard's
/// payload carries non-default data and "only this shard changed" is
/// observable.
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
/// expose (the physical shard layout IS adapter-internal) — the same pattern
/// the tombstone suite uses.
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

/// The `shard`-kind column of the discovered shard table, e.g. `shard` — used
/// to build the raw row queries from the adapter's own schema.
fn kind_column() -> String {
    schema::shard_kind_column()
}

/// (label -> (write counter, updated_at)) of every shard row of one project.
async fn shard_row_stamps(
    client: &tokio_postgres::Client,
    pid: &str,
) -> std::collections::BTreeMap<String, (i64, std::time::SystemTime)> {
    let sql = format!(
        "SELECT {kind}, revision, updated_at FROM {table} WHERE project_id = $1",
        kind = kind_column(),
        table = schema::shard_table_name()
    );
    let rows = client.query(&sql, &[&pid]).await.expect("shard rows");
    rows.into_iter()
        .map(|r| {
            (
                r.get::<_, String>(0),
                (r.get::<_, i64>(1), r.get::<_, std::time::SystemTime>(2)),
            )
        })
        .collect()
}

async fn head_revision(client: &tokio_postgres::Client, pid: &str) -> Option<i64> {
    client
        .query_opt(
            "SELECT revision FROM project_state_head WHERE project_id = $1",
            &[&pid],
        )
        .await
        .expect("head row")
        .map(|r| r.get::<_, i64>(0))
}

/// AC1: a project saved by the OLD code path (legacy single-blob row) loads
/// correctly, keeps its revision visible, and migrates to shards on its next
/// save — with the data identical across the migration and the original
/// document frozen for recovery.
#[tokio::test]
async fn f300_legacy_row_loads_and_migrates_to_shards_on_next_save() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("f300-legacy-{}", std::process::id());
    let store = connect_store(&db, &pid).await;

    // The old code path's exact artifact: one full-aggregate JSONB row in
    // `project_state` (this is serde of a real ProjectState, revision 7).
    let legacy_state = lived_in_state();
    let legacy_json = serde_json::to_value(&legacy_state).expect("serialize aggregate");
    raw(&db.dsn())
        .await
        .execute(
            "INSERT INTO project_state (project_id, schema_version, revision, data)
             VALUES ($1, $2, $3, $4)",
            &[
                &pid,
                &i32::try_from(SCHEMA_VERSION).expect("schema version fits i32"),
                &7_i64,
                &legacy_json,
            ],
        )
        .await
        .expect("seed the legacy single-blob row");

    // Pre-migration: the legacy row loads whole, with its revision.
    assert_eq!(
        store.load().await.expect("legacy load"),
        legacy_state,
        "a project saved by the old code path must load correctly before any \
         migration runs"
    );
    assert_eq!(
        store.current_version().await.expect("legacy version"),
        Some(7),
        "the legacy row's revision is the truth until the migration runs"
    );

    // The next save migrates: shards written, data preserved, revision kept.
    let mut next = store.load().await.expect("load before save");
    next.tickets.push(ready_feature("CXC-F301"));
    store.save(&next).await.expect("the migrating save");

    let client = raw(&db.dsn()).await;
    let shard_table = schema::shard_table_name();
    let kind = kind_column();
    assert_eq!(
        count(
            &client,
            "SELECT count(*) FROM project_state WHERE project_id = $1",
            &pid
        )
        .await,
        0,
        "the legacy single-blob row is tombstoned once the shards are written"
    );
    assert_eq!(
        count(
            &client,
            &format!(
                "SELECT count(*) FROM {shard_table} WHERE project_id = $1 AND {kind} = '_legacy'"
            ),
            &pid
        )
        .await,
        1,
        "the original full JSON is preserved frozen as shard='_legacy'"
    );
    assert_eq!(
        count(
            &client,
            &format!("SELECT count(*) FROM {shard_table} WHERE project_id = $1"),
            &pid
        )
        .await,
        6,
        "one row per bounded-context shard plus the frozen legacy row"
    );
    let frozen: serde_json::Value = client
        .query_one(
            &format!("SELECT data FROM {shard_table} WHERE project_id = $1 AND {kind} = '_legacy'"),
            &[&pid],
        )
        .await
        .expect("legacy freeze row")
        .get(0);
    assert_eq!(
        frozen, legacy_json,
        "the frozen document must be byte-identical JSON to what the old code wrote"
    );
    assert_eq!(
        head_revision(&client, &pid).await,
        Some(8),
        "the head revision carries the legacy row's revision forward (7) plus \
         the save's own bump — never rewound to 1"
    );

    // Data identical pre/post migration, plus the mutation.
    assert_eq!(
        store.load().await.expect("post-migration load"),
        next,
        "load composes the shards back into the same aggregate"
    );
    assert_eq!(
        store
            .current_version()
            .await
            .expect("post-migration version"),
        Some(8)
    );
}

/// AC5: a save rewrites only the shards whose payload changed — the write
/// amplification CXA-C019b removes, observed at the row level (per-shard write
/// counter and updated_at unchanged for untouched shards).
#[tokio::test]
async fn f300_a_save_rewrites_only_the_shards_whose_payload_changed() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("f300-diff-{}", std::process::id());
    let store = connect_store(&db, &pid).await;
    let state = lived_in_state();
    store.save(&state).await.expect("seed");

    let client = raw(&db.dsn()).await;
    let before = shard_row_stamps(&client, &pid).await;
    assert_eq!(before.len(), 5, "one row per bounded-context shard");
    let rev_before = head_revision(&client, &pid).await.expect("head row");

    // Change ONLY the social shard; every other field stays as loaded.
    let mut social_only = store.load().await.expect("load");
    social_only.post_comment_by("DEV", "dev@host", "a social-only write", None);
    store.save(&social_only).await.expect("social-only save");

    let after = shard_row_stamps(&client, &pid).await;
    assert_eq!(
        head_revision(&client, &pid).await,
        Some(rev_before + 1),
        "the head revision bumps on every successful write"
    );
    for (label, (revision, updated_at)) in before {
        let Some((new_revision, new_updated_at)) = after.get(&label) else {
            panic!("shard row {label} vanished");
        };
        if label == schema::shard_label(ShardKind::Social) {
            assert_eq!(
                *new_revision,
                revision + 1,
                "the social shard was rewritten: its write counter moves"
            );
            assert!(
                *new_updated_at >= updated_at,
                "the rewritten shard's updated_at moves forward"
            );
        } else {
            assert_eq!(
                *new_revision, revision,
                "shard {label} was NOT part of this save — its write counter \
                 must not move (no whole-aggregate rewrite)"
            );
            assert_eq!(
                *new_updated_at, updated_at,
                "shard {label} was NOT part of this save — its row must not be \
                 touched (updated_at unchanged)"
            );
        }
    }
    assert_eq!(
        store.load().await.expect("reload"),
        social_only,
        "the composed aggregate reflects the save"
    );
}

/// AC3: two sequential claims cannot both win, and a claim does NOT take the
/// whole-aggregate lock — while another connection holds a DIFFERENT shard's
/// row lock, the claim still wins (under the old whole-row lock it would block
/// until that lock released).
#[tokio::test]
async fn f300_claim_scopes_its_lock_to_the_tickets_shard_row() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("f300-claim-{}", std::process::id());
    let first = connect_store(&db, &pid).await;
    let state = lived_in_state();
    first.save(&state).await.expect("seed");
    let id = TicketId::new("CXC-F300").expect("valid id");
    let now = "2026-09-07T00:00:00Z";

    // Two sequential claims on the same ticket: exactly one wins.
    assert!(
        first
            .claim_ticket(&id, "dev-a@host", now)
            .await
            .expect("claim"),
        "the first claim wins"
    );
    let second = connect_store(&db, &pid).await;
    assert!(
        !second
            .claim_ticket(&id, "dev-b@host", now)
            .await
            .expect("second claim"),
        "two sequential claims cannot both win the same ticket"
    );

    // A successful claim bumps the revision the REST ops observe.
    let client = raw(&db.dsn()).await;
    let rev_after_claims = head_revision(&client, &pid).await.expect("head row");
    assert_eq!(
        first.current_version().await.expect("version after claims"),
        Some(rev_after_claims),
        "claim_ticket bumps the head revision (REST op=version keeps observing claims)"
    );

    // A second, still-free ticket for the lock-scope claim below.
    let mut with_second = second.load().await.expect("load");
    with_second.tickets.push(ready_feature("CXC-F302"));
    second
        .save(&with_second)
        .await
        .expect("seed the free ticket");

    // Lock scope: hold ONLY the social shard row on another connection. The
    // claim must still win — it locks the Tickets (work) shard row, not the
    // whole aggregate. (Bounded so a regression to the whole-row lock fails
    // red instead of hanging the suite.)
    let mut holder = raw(&db.dsn()).await;
    let tx = holder.transaction().await.expect("holder tx");
    let social_label = schema::shard_label(ShardKind::Social);
    tx.query_opt(
        &format!(
            "SELECT data FROM {} WHERE project_id = $1 AND {} = $2 FOR UPDATE",
            schema::shard_table_name(),
            kind_column()
        ),
        &[&pid, &social_label],
    )
    .await
    .expect("lock the social shard row");
    let won = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        second.claim_ticket(
            &TicketId::new("CXC-F302").expect("valid id"),
            "dev-b@host",
            now,
        ),
    )
    .await
    .expect("the claim must not block behind an unrelated shard row's lock")
    .expect("claim while another shard row is locked");
    assert!(
        won,
        "the claim on a free ticket wins without the whole-row lock"
    );
    tx.rollback().await.expect("release the social lock");
}

/// AC5: a Conflict on a stale revision changes NOTHING — the winning state's
/// shard rows and the head revision are exactly as the winner left them.
#[tokio::test]
async fn f300_a_stale_write_conflicts_and_writes_no_shard_rows() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("f300-cas-{}", std::process::id());
    let store = connect_store(&db, &pid).await;
    let first = lived_in_state();
    store.save(&first).await.expect("first save lands at rev 1");

    let client = raw(&db.dsn()).await;
    let before = shard_row_stamps(&client, &pid).await;

    // A writer that captured revision 0 must conflict, not clobber.
    let mut stale = first.clone();
    stale.tickets.push(ready_feature("CXC-F303"));
    match store.save_expecting(&stale, Some(0)).await {
        Err(PortError::Conflict(_)) => {}
        other => panic!("expected Conflict for a stale rev-0 write — got {other:?}"),
    }
    let after = shard_row_stamps(&client, &pid).await;
    assert_eq!(
        before, after,
        "the losing writer must not rewrite any shard row"
    );
    assert_eq!(
        head_revision(&client, &pid).await,
        Some(1),
        "the head revision is untouched by the losing write"
    );

    // Retrying against the current revision converges.
    store
        .save_expecting(&stale, Some(1))
        .await
        .expect("retry with the current revision");
    assert_eq!(
        store.load().await.expect("reload").tickets.len(),
        2,
        "the retried write is visible through the composed load"
    );
}

/// AC4: a `gate_save` refusal (CXA-F229 structural-integrity audit) returns
/// Err BEFORE any row is written — no shard row appears, moves or bumps.
#[tokio::test]
async fn f300_a_gate_refusal_writes_no_shard_rows() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = format!("f300-gate-{}", std::process::id());
    let store = connect_store(&db, &pid).await;
    store.save(&lived_in_state()).await.expect("seed");

    let client = raw(&db.dsn()).await;
    let before = shard_row_stamps(&client, &pid).await;

    // Two doc pages sharing one id — the unhealable finding the F229 audit
    // refuses (same fixture shape the C023 suite uses).
    let mut bad = ProjectState {
        tickets: vec![ready_feature("CXC-F304")],
        ..ProjectState::default()
    };
    for _ in 0..2 {
        bad.docs.push(coxagent_application::state::DocPage {
            id: "dup-doc".to_owned(),
            folder: String::new(),
            category: "product".to_owned(),
            title: "t".to_owned(),
            body: String::new(),
            updated_at: String::new(),
            updated_by: String::new(),
        });
    }
    assert!(
        store.save(&bad).await.is_err(),
        "the refused payload must not save"
    );

    assert_eq!(
        shard_row_stamps(&client, &pid).await,
        before,
        "no shard row may appear, move or bump on a refused write"
    );
    assert_eq!(
        head_revision(&client, &pid).await,
        Some(1),
        "the head revision is untouched by the refused write"
    );
    assert_eq!(
        store.quarantined().await.len(),
        1,
        "the refusal is quarantined (CXA-F229 behaviour unchanged)"
    );
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

/// Native-path proof: a payload written straight into the shard ROW is what
/// `load_shard` serves — the read costs one row, not a whole-aggregate
/// decode.
#[tokio::test]
async fn sql_store_load_shard_reads_the_native_row_payload() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = "c019b-native";
    let store = connect_store(&db, pid).await;
    store.save(&full_state()).await.expect("seed save");

    let mut native_only = SocialShard::default();
    native_only.chat.push(chat_msg("m9", "native row only"));
    let payload = serde_json::to_value(ShardData::Social(native_only.clone()))
        .expect("tagged social payload");
    raw(&db.dsn())
        .await
        .execute(
            &format!(
                "UPDATE {} SET data = $1 WHERE project_id = $2 AND {} = 'social'",
                schema::shard_table_name(),
                kind_column()
            ),
            &[&payload, &pid],
        )
        .await
        .expect("write the native-only shard row");

    let shard = store
        .load_shard(ShardKind::Social)
        .await
        .expect("native read");
    assert_eq!(
        shard.data,
        ShardData::Social(native_only.clone()),
        "the native shard row is the read path"
    );
    assert_eq!(
        shard.kind,
        ShardKind::Social,
        "the served payload must agree with the requested kind"
    );
}

/// Rollback path: a project with NO shard row for the requested kind — the
/// pre-migration legacy single-blob layout — falls back to projecting the
/// slice from the composed load, so the legacy row keeps serving honest
/// shard reads until the migration runs.
#[tokio::test]
async fn sql_store_load_shard_falls_back_to_the_composed_load_when_no_shard_row_exists() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let pid = "c019b-fallback";
    let store = connect_store(&db, pid).await;

    // The old code path's exact artifact: one full-aggregate JSONB row, no
    // shard rows (seeded after connect, so the migration has nothing to do).
    let state = full_state();
    let legacy_json = serde_json::to_value(&state).expect("serialize aggregate");
    raw(&db.dsn())
        .await
        .execute(
            "INSERT INTO project_state (project_id, schema_version, revision, data)
             VALUES ($1, $2, $3, $4)",
            &[
                &pid,
                &i32::try_from(SCHEMA_VERSION).expect("schema version fits i32"),
                &3_i64,
                &legacy_json,
            ],
        )
        .await
        .expect("seed the legacy single-blob row");

    assert_eq!(
        store
            .load_shard(ShardKind::Governance)
            .await
            .expect("fallback read"),
        state.shard(ShardKind::Governance),
        "a missing shard row must project the composed (legacy) aggregate, \
         never serve defaults"
    );
}

/// Isolation: a shard-scoped save lands only that shard's ROW — every other
/// shard row's write counter and updated_at stay byte-identical — and the
/// composed load reflects the merge, so full-document readers never miss a
/// shard write.
#[tokio::test]
async fn sql_store_save_shard_merges_only_its_own_shard_row() {
    let Some(db) = common::claim_or_skip().await else {
        return;
    };
    let store = connect_store(&db, "c019b-isolation").await;
    let before = full_state();
    store.save(&before).await.expect("seed save");
    let client = raw(&db.dsn()).await;
    let stamps_before = shard_row_stamps(&client, "c019b-isolation").await;

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
    let stamps_after = shard_row_stamps(&client, "c019b-isolation").await;
    for (label, stamp) in &stamps_after {
        if label == "social" {
            continue;
        }
        assert_eq!(
            stamps_before.get(label),
            Some(stamp),
            "shard row '{label}' must survive a Social save untouched"
        );
    }

    let mut expected = before.clone();
    expected.with_shard(social_save);
    assert_eq!(
        store.load().await.expect("composed reload"),
        expected,
        "the composed load must reflect the shard merge — no lost or \
         defaulted fields"
    );

    // The merged document still passes the write-boundary integrity audit as
    // a whole: a full save of it round-trips.
    let merged = store.load().await.expect("merged");
    store
        .save(&merged)
        .await
        .expect("full save of merged state");
}

/// Optimistic concurrency: `save_shard_expecting` CASes the HEAD revision —
/// a stale caller conflicts and changes nothing (no shard row, no revision
/// bump), a fresh caller converges, and the unguarded `save_shard` merge
/// path still works.
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
        "the refused write must not touch the shard row"
    );
    assert_eq!(
        store.load().await.expect("composed load").chat[0].body,
        "first",
        "the refused write must not touch the composed aggregate either"
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
        store.load().await.expect("composed load").chat[0].body,
        "scoped",
        "the unguarded merge is coherent in the composed aggregate too"
    );
}
