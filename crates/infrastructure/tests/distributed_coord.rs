#![allow(clippy::unwrap_used, clippy::expect_used, clippy::map_unwrap_or)]
//! Live integration test for the distributed coordination path: Postgres for
//! durable state + transactional ticket claims, Redis for leader/stage leases.
//!
//! Skipped unless both are provided:
//!   COXAGENT_TEST_PG_DSN=postgres://cox:test@localhost:55432/coxagent \
//!   COXAGENT_TEST_REDIS_URL=redis://localhost:56379 \
//!   cargo test -p coxagent-infrastructure --test distributed_coord -- --nocapture

use coxagent_application::ports::outbound::{GitCheck, StateStorePort, WorkerCaps};
use coxagent_application::state::ProjectState;
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};
use coxagent_infrastructure::state::SqlStateStore;

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

async fn store(project: &str) -> SqlStateStore {
    let dsn = env("COXAGENT_TEST_PG_DSN").expect("dsn");
    let redis = env("COXAGENT_TEST_REDIS_URL").expect("redis");
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
    assert!(
        workers
            .iter()
            .find(|w| w.worker == "luffy@b")
            .and_then(|w| w.git.clone())
            .is_none(),
        "a runner that has not probed reports nothing rather than a false pass"
    );
}
