//! Executable half of CXA-F246's AC2: the evidence collectors record the
//! resolved live link under `state.repro_urls[ticket]` inside the SAME
//! mutation that attaches the ticket's DoD evidence — and record nothing
//! when the deploy gate is off. The source-scan guards in
//! `live_repro_url_f246_tdd.rs` pin that the writer exists; these prove what
//! it writes, over the real `MemStore` + `RunCycleUseCase` harness from
//! [`super::cycle_tests`] — no host, no network port, no spawn.

use super::RunCycleUseCase;
use crate::config::Config;
use crate::ports::outbound::StateStorePort;
use crate::state::ProjectState;
use crate::use_cases::cycle::cycle_tests::{MemStore, RoleAwareEngine};
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A Bug driven to `Fixed` through the REAL transition table — the
/// shipped, awaiting-verification shape evidence collection keys on.
fn fixed_bug(id: &str, has_ui: bool) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Bug,
        "Login returns 500 on an empty email",
        "POST /api/login with an empty email crashes the handler.",
        Priority::High,
        Complexity::Small,
        has_ui,
    )
    .expect("valid ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("bug claim is a legal edge");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("DEV completes the fix");
    t
}

/// A use case over the in-memory store with `deploy.host_port` set but NO
/// shot/probe/storage wired — both collectors take their waived arm, which
/// is exactly the "capture failed, link still recorded" path.
fn evidence_uc(
    state: ProjectState,
    host_port: Option<u16>,
) -> (Arc<MemStore>, RunCycleUseCase<MemStore, RoleAwareEngine>) {
    let mut cfg = Config::default();
    cfg.deploy.host_port = host_port;
    let store = Arc::new(MemStore {
        state: Mutex::new(state),
    });
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    );
    (store, uc)
}

#[tokio::test]
async fn collecting_api_evidence_records_the_resolved_live_link() {
    let id = TicketId::new("CXC-246-api").expect("valid ticket id");
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug("CXC-246-api", false));
    let (store, uc) = evidence_uc(state, Some(8101));

    uc.collect_evidence(&id).await;

    let s = store.load().await.expect("load");
    assert_eq!(
        s.repro_urls.get("CXC-246-api").map(String::as_str),
        Some("http://127.0.0.1:8101/"),
        "the collector records the config-derived live link under the ticket id"
    );
    assert!(
        s.ticket_evidence.contains_key("CXC-246-api"),
        "the link lands in the same mutation as the evidence attach, never alone"
    );
}

#[tokio::test]
async fn collecting_ui_evidence_records_the_resolved_live_link() {
    let id = TicketId::new("CXC-246-ui").expect("valid ticket id");
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug("CXC-246-ui", true));
    let (store, uc) = evidence_uc(state, Some(8101));

    uc.collect_evidence(&id).await;

    let s = store.load().await.expect("load");
    assert_eq!(
        s.repro_urls.get("CXC-246-ui").map(String::as_str),
        Some("http://127.0.0.1:8101/"),
        "the UI collector records the link even when the capture itself waives"
    );
    assert!(
        s.ticket_evidence.contains_key("CXC-246-ui"),
        "waived screenshot evidence is attached by the same mutation"
    );
}

#[tokio::test]
async fn a_project_with_no_deploy_port_records_no_link_and_no_evidence() {
    let id = TicketId::new("CXC-246-off").expect("valid ticket id");
    let mut state = ProjectState::default();
    state.tickets.push(fixed_bug("CXC-246-off", false));
    let (store, uc) = evidence_uc(state, None);

    uc.collect_evidence(&id).await;

    let s = store.load().await.expect("load");
    assert!(
        s.repro_urls.is_empty(),
        "the gate is off — an unresolvable link is absent, never fabricated"
    );
    assert!(
        !s.ticket_evidence.contains_key("CXC-246-off"),
        "no deploy port means nothing to prove against: no evidence either"
    );
}
