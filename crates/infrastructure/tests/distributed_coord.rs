#![allow(clippy::unwrap_used, clippy::expect_used, clippy::map_unwrap_or)]
//! Live integration test for the distributed coordination path: Postgres for
//! durable state + transactional ticket claims, Redis for leader/stage leases.
//!
//! Skipped unless both are provided:
//!   COXAGENT_TEST_PG_DSN=postgres://cox:test@localhost:55432/coxagent \
//!   COXAGENT_TEST_REDIS_URL=redis://localhost:56379 \
//!   cargo test -p coxagent-infrastructure --test distributed_coord -- --nocapture

use coxagent_application::ports::outbound::{GitCheck, StateStorePort, WorkerCaps};

mod common;
use coxagent_application::state::ProjectState;
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};
use coxagent_infrastructure::state::SqlStateStore;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

/// The tests connect in parallel; on a FRESH database their concurrent
/// `CREATE TABLE IF NOT EXISTS` races the catalog and one loses with
/// "migrate: db error". Run the first migration once, alone.
static MIGRATED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn store(project: &str) -> SqlStateStore {
    let dsn = env("COXAGENT_TEST_PG_DSN").expect("dsn");
    let redis = env("COXAGENT_TEST_REDIS_URL").expect("redis");
    MIGRATED
        .get_or_init(|| async {
            SqlStateStore::connect(&dsn, "schema-init")
                .await
                .expect("initial migrate");
        })
        .await;
    SqlStateStore::connect(&dsn, project)
        .await
        .expect("connect pg")
        .with_redis(&redis)
        .expect("attach redis")
}

fn ready_feature(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("id"),
        TicketType::Feature,
        "f",
        "",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("t");
    t.set_technical_design(Role::Sa, TechnicalDesign::default())
        .expect("design");
    t.transition_to(Role::Sa, Status::Ready).expect("ready");
    t
}

#[tokio::test]
async fn distributed_coordination_across_two_hubs() {
    if env("COXAGENT_TEST_PG_DSN").is_none() || env("COXAGENT_TEST_REDIS_URL").is_none() {
        eprintln!("skipping: set COXAGENT_TEST_PG_DSN + COXAGENT_TEST_REDIS_URL");
        return;
    }
    if common::is_live_hub_db(&env("COXAGENT_TEST_PG_DSN").expect("dsn")).await {
        eprintln!(
            "COXAGENT_TEST_PG_DSN points at a LIVE hub database — refusing the coordination test"
        );
        return;
    }
    // Unique project id per run so reruns start clean.
    let project = format!(
        "test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    // Two stores on the same PG + Redis = two machines/hubs.
    let hub_a = store(&project).await;
    let hub_b = store(&project).await;
    let now = "2026-07-15T00:00:00Z";

    // Seed two ready features into the shared state.
    hub_a
        .save(&ProjectState {
            tickets: vec![ready_feature("CXC-F001"), ready_feature("CXC-F002")],
            ..ProjectState::default()
        })
        .await
        .expect("seed");

    // Leader election (Redis): exactly one hub leads.
    let a_leads = hub_a
        .acquire_leader("chopper@a", now)
        .await
        .expect("a lead");
    let b_leads = hub_b.acquire_leader("luffy@b", now).await.expect("b lead");
    assert!(a_leads, "first caller becomes leader");
    assert!(!b_leads, "second caller cannot lead while lease is fresh");

    // Stage lease (Redis): exclusive per (ticket, stage).
    assert!(hub_a
        .claim_stage(&TicketId::new("CXC-F001").unwrap(), "sa", "chopper@a", now)
        .await
        .unwrap());
    assert!(!hub_b
        .claim_stage(&TicketId::new("CXC-F001").unwrap(), "sa", "luffy@b", now)
        .await
        .unwrap());

    // Ticket claim (Postgres transaction): two hubs race the same ticket, one wins.
    let id = TicketId::new("CXC-F001").unwrap();
    let a_won = hub_a.claim_ticket(&id, "chopper@a", now).await.unwrap();
    let b_won = hub_b.claim_ticket(&id, "luffy@b", now).await.unwrap();
    assert!(a_won ^ b_won, "exactly one hub wins the same ticket");
    let winner = if a_won { "chopper@a" } else { "luffy@b" };
    let state = hub_a.load().await.unwrap();
    let t = state.tickets.iter().find(|t| t.id() == &id).unwrap();
    assert_eq!(t.status(), Status::InProgress);
    assert_eq!(t.claimed_by(), Some(winner));

    // The *other* ticket is still free — the losing hub can grab a different one.
    let id2 = TicketId::new("CXC-F002").unwrap();
    assert!(hub_b.claim_ticket(&id2, "luffy@b", now).await.unwrap());

    worker_registry_carries_machine_capabilities(&hub_a, &hub_b, now).await;
}

/// CXA-B130: a project's delete must also sweep its Redis keyspace. The
/// leases are TTL'd and would expire on their own, but the operator's
/// desired-run state is a PERSISTENT key — leaving it behind would
/// auto-resume the deleted project's runner the moment a new project reuses
/// the id.
#[tokio::test]
async fn delete_sweeps_the_project_redis_keyspace_including_desired_state() {
    if env("COXAGENT_TEST_PG_DSN").is_none() || env("COXAGENT_TEST_REDIS_URL").is_none() {
        eprintln!("skipping: set COXAGENT_TEST_PG_DSN + COXAGENT_TEST_REDIS_URL");
        return;
    }
    if common::is_live_hub_db(&env("COXAGENT_TEST_PG_DSN").expect("dsn")).await {
        eprintln!(
            "COXAGENT_TEST_PG_DSN points at a LIVE hub database — refusing the coordination test"
        );
        return;
    }
    let project = format!(
        "del-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let store = store(&project).await;

    store
        .set_desired("op@host", true)
        .await
        .expect("persist desired run state in Redis");
    assert_eq!(
        store.get_desired("op@host").await.expect("desired"),
        Some(true),
        "fixture: the desired state must be readable before the delete"
    );

    store.delete().await.expect("delete");

    assert_eq!(
        store
            .get_desired("op@host")
            .await
            .expect("desired after delete"),
        None,
        "the deleted project's persistent Redis keys must be swept, or a \
         recreated id auto-resumes its runner"
    );
}

/// The registry is how a hub with no CLI, no key and no checkout of its own —
/// the container serving the dashboard — learns what each machine can do.
async fn worker_registry_carries_machine_capabilities(
    hub_a: &SqlStateStore,
    hub_b: &SqlStateStore,
    now: &str,
) {
    hub_a
        .heartbeat_worker(
            "chopper@a",
            "leader",
            "CXC-F001",
            &WorkerCaps {
                engines: vec!["claude".to_owned(), "opencode".to_owned()],
                // A CUSTOM provider: it exists only in this user's opencode
                // config, so no built-in list and no other machine can know it.
                models: vec!["bizbrain/DeepSeek-V4-Pro".to_owned()],
                // Push works, pull requests do not — the split that a hub-side
                // probe cannot see, because it holds neither credential.
                git: Some(GitCheck {
                    account: "kyroc3".to_owned(),
                    api_ok: false,
                    push_ok: true,
                    remedy: "gh is signed in as 'kyroc3', which cannot see the repo".to_owned(),
                    ..GitCheck::default()
                }),
                // The machine's OS, which the hub cannot infer: asking itself
                // would answer with the container's.
                tooling: Some(serde_json::json!({ "os": "macos", "has_brew": true })),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    hub_b
        .heartbeat_worker(
            "luffy@b",
            "worker",
            "CXC-F002",
            &WorkerCaps {
                engines: vec!["gemini".to_owned()],
                ..WorkerCaps::default()
            },
            now,
        )
        .await
        .unwrap();
    let workers = hub_a.workers().await.unwrap();
    assert_eq!(workers.len(), 2, "both teams show online: {workers:?}");

    let engines_of = |who: &str| {
        workers
            .iter()
            .find(|w| w.worker == who)
            .map(|w| w.engines.clone())
            .unwrap_or_default()
    };
    assert_eq!(
        engines_of("chopper@a"),
        vec!["claude".to_owned(), "opencode".to_owned()],
        "a runner's own engines survive the round trip"
    );
    assert_eq!(
        engines_of("luffy@b"),
        vec!["gemini".to_owned()],
        "each runner reports only what IT has, not a merged list"
    );
    assert_eq!(
        workers
            .iter()
            .find(|w| w.worker == "chopper@a")
            .map(|w| w.models.clone())
            .unwrap_or_default(),
        vec!["bizbrain/DeepSeek-V4-Pro".to_owned()],
        "a custom opencode provider reaches the hub only through its own runner"
    );
    let git = workers
        .iter()
        .find(|w| w.worker == "chopper@a")
        .and_then(|w| w.git.clone())
        .expect("the runner's own git probe reaches the hub");
    assert!(git.push_ok, "push works on that machine");
    assert!(
        !git.api_ok,
        "and pull requests do not — the two are separate credentials"
    );
    assert_eq!(git.account, "kyroc3");
    assert_eq!(
        workers
            .iter()
            .find(|w| w.worker == "chopper@a")
            .and_then(|w| w.tooling.clone())
            .and_then(|t| t.get("os").and_then(|o| o.as_str().map(ToOwned::to_owned))),
        Some("macos".to_owned()),
        "the OS comes from the machine that has the tools, not from whoever asks"
    );
    assert!(
        workers
            .iter()
            .find(|w| w.worker == "luffy@b")
            .and_then(|w| w.git.clone())
            .is_none(),
        "a runner that has not probed reports nothing rather than a false pass"
    );
}
