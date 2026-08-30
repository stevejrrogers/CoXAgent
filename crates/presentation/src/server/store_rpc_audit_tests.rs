// CXA-F229 tests — the structural-integrity audit surface:
//
//   * `GET /api/projects/:pid/store?op=audit`  — read-only findings +
//     quarantine ledger, behind the exact auth layering the POST surface has.
//   * `POST …/store?op=heal` — opt-in self-heal of dangling ticket-keyed map
//     entries (audit-first-then-fix, through the port's guarded save path).
//   * Write-back refusal: a mutation whose post-state fails the audit is
//     refused and its payload quarantined (AC4), visible via the audit.
//
// Pure in-process verification (harness in `store_rpc_test_support`): requests
// drive the real handler and the real `auth_mw` layering via
// `tower::ServiceExt::oneshot`; the store is a real `JsonStateStore` over a
// tempdir so the quarantine ledger is the real one.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::wildcard_imports)]
use super::*;
use coxagent_application::state::ProjectState;
use coxagent_infrastructure::JsonStateStore;
use std::sync::Arc;
use store_rpc_test_support::{
    app_with, body_text, deployed_router, get_store_at, handler_router, post_store, CountingStore,
    StubAuth, INSIDE_BEARER, OUTSIDE_SESSION, PID,
};

/// A ticket-bearing healthy state.
fn healthy_state() -> ProjectState {
    let mut state = ProjectState::default();
    state.tickets.push(
        coxagent_domain::Ticket::new(
            coxagent_domain::TicketId::new("CXA-F001").expect("id"),
            coxagent_domain::TicketType::Feature,
            "t",
            "",
            coxagent_domain::Priority::Medium,
            coxagent_domain::Complexity::Small,
            false,
        )
        .expect("ticket"),
    );
    state.add_evidence("CXA-F001", "test", "proof", "detail");
    state
}

/// A state whose evidence map references a ticket that does not exist — the
/// corrupted shape the auditor must refuse (AC3 fixture: dangling evidence).
fn state_with_dangling_evidence() -> ProjectState {
    let mut state = healthy_state();
    state
        .ticket_evidence
        .insert("CXA-F999".to_owned(), Vec::new());
    state
}

/// A real JsonStateStore over a tempdir, pre-seeded by writing `state.json`
/// directly — the only way to place a corrupted document behind a real store,
/// since the write path now refuses such payloads.
fn store_seeded_with(state: &ProjectState) -> (tempfile::TempDir, Arc<JsonStateStore>) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("state.json"),
        serde_json::to_string_pretty(state).expect("serialize state"),
    )
    .expect("write state.json");
    let store = Arc::new(JsonStateStore::new(dir.path()).expect("store"));
    (dir, store)
}

// ---------------------------------------------------------------------------
// The audit read endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_of_a_healthy_project_is_200_and_healthy() {
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(None, store).await;
    let resp = get_store_at(handler_router(app), PID, Some("audit"), None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_text(resp).await;
    let doc: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(doc["healthy"], serde_json::json!(true));
    assert_eq!(doc["findings"], serde_json::json!([]));
    assert_eq!(doc["quarantined"], serde_json::json!([]));
}

#[tokio::test]
async fn audit_of_a_corrupted_project_names_the_violation() {
    let (_dir, store) = store_seeded_with(&state_with_dangling_evidence());
    let app = app_with(None, store).await;
    let resp = get_store_at(handler_router(app), PID, Some("audit"), None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["healthy"], serde_json::json!(false));
    let findings = doc["findings"].as_array().expect("findings array");
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0]["rule_id"],
        serde_json::json!("dangling_ticket_reference")
    );
    assert_eq!(findings[0]["ticket_id"], serde_json::json!("CXA-F999"));
    assert!(findings[0]["detail"]
        .as_str()
        .expect("detail")
        .contains("ticket_evidence"));
}

#[tokio::test]
async fn audit_rejects_an_unknown_op_with_the_store_error_envelope() {
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(None, store).await;
    let resp = get_store_at(handler_router(app), PID, Some("bogus"), None, None).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert!(doc["error"]
        .as_str()
        .expect("error")
        .contains("unknown store op"));
}

// ---------------------------------------------------------------------------
// Auth: the GET audit sits behind the same gates as the POST surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_without_credentials_is_refused_before_the_store_is_touched() {
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = deployed_router(app);
    let resp = get_store_at(router, PID, Some("audit"), None, None).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(store.calls(), 0, "a refused audit never reaches the store");
}

#[tokio::test]
async fn audit_for_a_non_member_is_refused_by_the_handler_itself() {
    // Mallory is authenticated but holds no membership in PID — the in-handler
    // defense-in-depth gate refuses even though auth_mw's read path passed.
    let store = Arc::new(CountingStore::seeded(ProjectState::default()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = handler_router(app);
    let resp = get_store_at(router, PID, Some("audit"), None, Some(OUTSIDE_SESSION)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(store.calls(), 0);
}

#[tokio::test]
async fn audit_with_a_member_bearer_is_allowed() {
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(Some(Arc::new(StubAuth)), store).await;
    let resp = get_store_at(
        deployed_router(app),
        PID,
        Some("audit"),
        Some(&format!("Bearer {INSIDE_BEARER}")),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["healthy"], serde_json::json!(true));
}

#[tokio::test]
async fn audit_defaults_to_the_audit_op_when_the_query_is_omitted() {
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(None, store).await;
    let resp = get_store_at(handler_router(app), PID, None, None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["healthy"], serde_json::json!(true));
}

// The path's guard philosophy (store_rpc_guard_tests): EVERY op needs a
// principal, and a refused call never reaches the store adapter. The two new
// POST ops — one of them mutating — sit under the same rule.

#[tokio::test]
async fn unauthenticated_post_audit_and_heal_are_refused_before_the_store() {
    let store = Arc::new(CountingStore::seeded(healthy_state()));
    let app = app_with(Some(Arc::new(StubAuth)), store.clone()).await;
    let router = handler_router(app);

    for op in ["audit", "heal"] {
        let resp = post_store(router.clone(), op, serde_json::json!({}), None, None).await;
        let status = resp.status();
        let body = body_text(resp).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "unauthenticated /store?op={op} must be refused — got: {body}"
        );
    }
    assert_eq!(
        store.calls(),
        0,
        "a refused audit or heal must never reach the store adapter"
    );
}

// ---------------------------------------------------------------------------
// Write-back refusal + quarantine (AC4)
// ---------------------------------------------------------------------------

/// A state with two doc pages sharing one id — an audit finding the write
/// boundary can NOT self-heal (which copy survives is a human decision), so a
/// save carrying it is refused and quarantined (AC4 fixture). It passes the
/// older schema-level `validate` (which only sees tickets/dependencies), so it
/// exercises specifically the integrity-audit refusal.
fn state_with_duplicate_doc_id() -> ProjectState {
    let mut state = healthy_state();
    for _ in 0..2 {
        state.docs.push(coxagent_application::state::DocPage {
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

/// A healthy state whose deploy ordinal rolled back behind the recorded
/// last-good deploy — an unhealable audit violation (which ordinal is real
/// needs a human), so the write boundary must refuse and quarantine it.
fn state_with_rolled_back_deploy_index() -> ProjectState {
    let mut state = healthy_state();
    state.deploy_index = 1;
    state.last_good_deploy = Some(coxagent_application::state::KnownGoodDeploy {
        sha: "abc".to_owned(),
        at: String::new(),
        deploy_index: 3,
        summary: "s".to_owned(),
    });
    state
}

// The write boundary (gate_save) HEALS the one safely-repairable corruption
// class — dangling ticket-keyed entries — and still REFUSES everything a
// human must decide, quarantining the payload. The tests below partition
// that contract through the real wire shape: one healable class and two
// unhealable ones (duplicated doc ids, a rolled-back deploy ordinal).
#[tokio::test]
async fn a_save_whose_post_state_fails_the_audit_is_refused_and_quarantined() {
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(None, store).await;
    let router = handler_router(app);

    // The corrupted snapshot rides the exact wire shape a runner uses
    // (`op=save` with a full ProjectState in `data`). The corruption is one
    // the boundary cannot decide: a duplicate doc id (which copy survives?) —
    // the dangling-reference class is healed at the boundary instead (see the
    // heal test below), so refusal is reserved for unhealable findings.
    let payload = serde_json::to_string(&state_with_duplicate_doc_id()).expect("serialize");
    let resp = post_store(
        router.clone(),
        "save",
        serde_json::json!({ "data": payload }),
        None,
        None,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the audit refusal surfaces through the same error envelope as every store failure"
    );
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert!(doc["error"]
        .as_str()
        .expect("error")
        .contains("structural integrity audit"));

    // The audit now reports the quarantined payload alongside the (healthy)
    // persisted state — the corruption is inspectable, not silent.
    let resp = get_store_at(router, PID, Some("audit"), None, None).await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(
        doc["healthy"],
        serde_json::json!(true),
        "persisted state untouched"
    );
    let quarantined = doc["quarantined"].as_array().expect("quarantine ledger");
    assert_eq!(quarantined.len(), 1, "the refused payload was quarantined");
    assert_eq!(
        quarantined[0]["rule_id"],
        serde_json::json!("duplicate_doc_id")
    );
    assert!(
        quarantined[0]["payload"]
            .as_str()
            .expect("payload")
            .contains("dup-doc"),
        "the attempted payload itself is recorded"
    );
}

/// A payload with dangling ticket-keyed references is no longer REFUSED at
/// the write boundary: refusing bricked the hub on FIRST deploy (decades of
/// legacy keys failed every save — see `gate_save` in the quarantine module),
/// so the one safely-repairable class heals in place and the healed shape is
/// what persists. The corruption must still never reach disk, and a healed
/// save is not quarantined.
#[tokio::test]
async fn a_save_with_dangling_references_is_healed_at_the_write_boundary() {
    // Since the write boundary heals the dangling-reference class (refusing
    // it bricked every save on first deploy), a save carrying orphaned
    // evidence succeeds: the entry is dropped, the persisted state audits
    // clean, and nothing is quarantined.
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(None, store).await;
    let router = handler_router(app);

    let payload = serde_json::to_string(&state_with_dangling_evidence()).expect("serialize");
    let resp = post_store(
        router.clone(),
        "save",
        serde_json::json!({ "data": payload }),
        None,
        None,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the dangling reference is healed, not refused"
    );
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["ok"], serde_json::json!(true));

    // The persisted state is the healed shape: the orphaned key is gone.
    let resp = post_store(router.clone(), "load", serde_json::json!({}), None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let state: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("state");
    assert!(
        state["ticket_evidence"].get("CXA-F999").is_none(),
        "the dangling evidence entry must not survive the save"
    );

    let resp = get_store_at(router, PID, Some("audit"), None, None).await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(
        doc["healthy"],
        serde_json::json!(true),
        "persisted state is the healed shape"
    );
    assert_eq!(doc["findings"], serde_json::json!([]));
    assert_eq!(
        doc["quarantined"],
        serde_json::json!([]),
        "a healed save is not quarantined"
    );
}

#[tokio::test]
async fn a_save_with_a_rolled_back_deploy_index_is_refused_and_quarantined() {
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(None, store).await;
    let router = handler_router(app);

    // The rolled-back ordinal rides the exact wire shape a runner uses
    // (`op=save` with a full ProjectState in `data`).
    let payload = serde_json::to_string(&state_with_rolled_back_deploy_index()).expect("serialize");
    let resp = post_store(
        router.clone(),
        "save",
        serde_json::json!({ "data": payload }),
        None,
        None,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "the audit refusal surfaces through the same error envelope as every store failure"
    );
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert!(doc["error"]
        .as_str()
        .expect("error")
        .contains("structural integrity audit"));

    // The audit now reports the quarantined payload alongside the (healthy)
    // persisted state — the corruption is inspectable, not silent.
    let resp = get_store_at(router, PID, Some("audit"), None, None).await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(
        doc["healthy"],
        serde_json::json!(true),
        "persisted state untouched"
    );
    let quarantined = doc["quarantined"].as_array().expect("quarantine ledger");
    assert_eq!(quarantined.len(), 1, "the refused payload was quarantined");
    assert_eq!(
        quarantined[0]["rule_id"],
        serde_json::json!("deploy_index_regression")
    );
    assert!(
        quarantined[0]["payload"]
            .as_str()
            .expect("payload")
            .contains("deploy_index"),
        "the attempted payload itself is recorded"
    );
}

#[tokio::test]
async fn a_healthy_save_is_never_refused_or_quarantined() {
    let (_dir, store) = store_seeded_with(&healthy_state());
    let app = app_with(None, store).await;
    let router = handler_router(app);
    let payload = serde_json::to_string(&healthy_state()).expect("serialize");
    let resp = post_store(
        router.clone(),
        "save",
        serde_json::json!({ "data": payload }),
        None,
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = get_store_at(router, PID, Some("audit"), None, None).await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["healthy"], serde_json::json!(true));
    assert_eq!(doc["quarantined"], serde_json::json!([]));
}

// ---------------------------------------------------------------------------
// Opt-in self-heal (POST op=heal)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn heal_drops_dangling_entries_and_the_project_audits_clean_again() {
    let (_dir, store) = store_seeded_with(&state_with_dangling_evidence());
    let app = app_with(None, store).await;
    let router = handler_router(app);

    let resp = post_store(router.clone(), "heal", serde_json::json!({}), None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["healed"], serde_json::json!(1));
    assert_eq!(doc["healthy"], serde_json::json!(true));

    // The persisted state is actually healed, not just the response.
    let resp = get_store_at(router, PID, Some("audit"), None, None).await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["healthy"], serde_json::json!(true));
    assert_eq!(doc["findings"], serde_json::json!([]));
}

#[tokio::test]
async fn heal_refuses_a_corruption_it_cannot_decide() {
    // A duplicate ticket id needs a human (which copy survives?) — the heal
    // must refuse and leave the state untouched.
    let mut state = state_with_dangling_evidence();
    state.tickets.push(
        coxagent_domain::Ticket::new(
            coxagent_domain::TicketId::new("CXA-F001").expect("id"),
            coxagent_domain::TicketType::Feature,
            "t",
            "",
            coxagent_domain::Priority::Medium,
            coxagent_domain::Complexity::Small,
            false,
        )
        .expect("ticket"),
    );
    let (_dir, store) = store_seeded_with(&state);
    let app = app_with(None, store).await;
    let router = handler_router(app);

    let resp = post_store(router.clone(), "heal", serde_json::json!({}), None, None).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert!(doc["error"]
        .as_str()
        .expect("error")
        .contains("duplicate_ticket_id"));

    // Nothing was touched: the dangling entry (and the duplicate) survive.
    let resp = get_store_at(router, PID, Some("audit"), None, None).await;
    let doc: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("json body");
    assert_eq!(doc["healthy"], serde_json::json!(false));
    assert_eq!(doc["findings"].as_array().expect("findings").len(), 2);
}
