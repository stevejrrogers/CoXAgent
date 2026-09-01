// CXA-F303 endpoint tests: the approval-policy surface (GET + the two
// operator flips) behind the REAL `auth_mw`, answered in-process via
// `tower::ServiceExt::oneshot` — no TCP port, no engine spawn, the same
// discipline as the /store guard tests (deterministic CXA-B037 rule).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::server::approval_policy::{
    approval_policy_ask_again_ep, approval_policy_ep, approval_policy_release_ep,
};
use axum::body::Body;
use coxagent_application::state::ProjectState;
use std::sync::Arc;
use store_rpc_test_support::{
    app_with_config, body_text, CountingStore, StubAuth, INSIDE_SESSION, MEMBER_SESSION, PID,
};
use tower::ServiceExt;

/// The project config under test: the adaptive gate ON, ready gate on,
/// documented defaults for the dials. The workflow fields beside `human`
/// are the ones WorkflowConfig requires (no serde default) — a real
/// coxagent.json always carries them.
const GATE_ON: &str = r#"{"workflow":{"ba_every_n_cycles":4,"feature_dev_enabled":true,"sleep_seconds":30,"human":{"gate_ready":true,"adaptive":{"enabled":true,"learn_after_samples":8,"undo_window_minutes":30,"max_auto_per_cycle":3}}}}"#;
/// Same, with the adaptive gate explicitly OFF (AC5).
const GATE_OFF: &str = r#"{"workflow":{"ba_every_n_cycles":4,"feature_dev_enabled":true,"sleep_seconds":30,"human":{"gate_ready":true,"adaptive":{"enabled":false}}}}"#;

/// A pending designed ticket — the undo-window rows attribute per shape via
/// `shape_key`, so the fixture must be a real aggregate.
fn pending_designed(id: &str, title: &str) -> coxagent_domain::Ticket {
    use coxagent_domain::{
        Complexity, Priority, Role, TechnicalDesign, Ticket, TicketId, TicketType,
    };
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid id"),
        TicketType::Chore,
        title.to_owned(),
        "the policy panel lists me".to_owned(),
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            files: vec!["crates/app/tests/policy_fixture.rs".to_owned()],
            ..TechnicalDesign::default()
        },
    )
    .expect("SA owns the design");
    t
}

/// A state with eight consistent approvals of `test/small` by luffy — the
/// exact threshold the config's `learn_after_samples` names.
fn learned_state() -> ProjectState {
    let mut s = ProjectState::default();
    for _ in 0..8 {
        s.approval_samples.push(
            coxagent_application::use_cases::approval_memory::ApprovalSample {
                shape: "test/small".to_owned(),
                decision: "approve".to_owned(),
                by: "luffy".to_owned(),
                reason: String::new(),
                at: "2026-08-31T09:00:00Z".to_owned(),
            },
        );
    }
    s
}

/// The three policy routes behind `auth_mw`, exactly as `serve_full` layers
/// them (route_layer + with_state).
fn policy_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/projects/:pid/approval-policy",
            get(approval_policy_ep),
        )
        .route(
            "/api/projects/:pid/approval-policy/ask-again",
            post(approval_policy_ask_again_ep),
        )
        .route(
            "/api/projects/:pid/approval-policy/release",
            post(approval_policy_release_ep),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))
        .with_state(state)
}

async fn get_policy(router: Router, pid: &str, session: Option<&str>) -> axum::response::Response {
    let mut builder = Request::builder().uri(format!("/api/projects/{pid}/approval-policy"));
    if let Some(token) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    router
        .oneshot(builder.body(Body::empty()).expect("well-formed request"))
        .await
        .expect("in-process request")
}

async fn flip(
    router: Router,
    segment: &str,
    shape: &str,
    session: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/projects/{PID}/approval-policy/{segment}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = session {
        builder = builder.header(header::COOKIE, format!("{SESSION_COOKIE}={token}"));
    }
    router
        .oneshot(
            builder
                .body(Body::from(
                    serde_json::json!({ "shape": shape }).to_string(),
                ))
                .expect("well-formed request"),
        )
        .await
        .expect("in-process request")
}

/// A hub with auth configured, the given project config on disk, and `state`
/// behind a counting store. The returned TempDir backs the config file — the
/// caller keeps it alive for the whole test.
async fn hub(config: &str, state: ProjectState) -> (tempfile::TempDir, Arc<CountingStore>, Router) {
    let store = Arc::new(CountingStore::seeded(state));
    let (dir, app) = app_with_config(
        Some(Arc::new(StubAuth) as Arc<dyn AuthPort>),
        store.clone() as Arc<dyn StateStorePort>,
        config,
    )
    .await;
    (dir, store, policy_router(app))
}

// --- AC1: the policy view is served -------------------------------------------

#[tokio::test]
async fn policy_view_lists_the_learned_rule_samples_and_deciders() {
    let (_dir, _store, router) = hub(GATE_ON, learned_state()).await;
    let resp = get_policy(router, PID, Some(INSIDE_SESSION)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).expect("policy JSON");

    assert_eq!(v["enabled"], serde_json::json!(true));
    assert_eq!(v["undo_window_minutes"], serde_json::json!(30));
    assert_eq!(v["learn_after_samples"], serde_json::json!(8));
    let shapes = v["shapes"].as_array().expect("shapes array");
    let row = shapes
        .iter()
        .find(|s| s["shape"] == "test/small")
        .expect("the approved shape is listed");
    assert_eq!(
        row["learned"],
        serde_json::json!({ "auto_approve": { "by": "luffy", "samples": 8 } }),
        "the rule exactly as the learner holds it"
    );
    assert_eq!(row["effective"], "auto");
    assert_eq!(row["samples"]["approve"], 8);
    assert_eq!(row["deciders"], serde_json::json!(["luffy"]));
    assert_eq!(row["last_decision_at"], "2026-08-31T09:00:00Z");
}

// --- AC2: the flip, and back ----------------------------------------------------

#[tokio::test]
async fn ask_again_flip_is_idempotent_persisted_and_reversible() {
    let (_dir, store, router) = hub(GATE_ON, learned_state()).await;

    // Force the shape back to always-ask.
    let resp = flip(
        router.clone(),
        "ask-again",
        "test/small",
        Some(INSIDE_SESSION),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["changed"], true);

    // Idempotent: the same flip again changes nothing.
    let resp = flip(
        router.clone(),
        "ask-again",
        "test/small",
        Some(INSIDE_SESSION),
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();
    assert_eq!(v["changed"], false, "the flip is idempotent");
    assert_eq!(
        store.snapshot().ask_again_shapes,
        vec!["test/small".to_owned()],
        "exactly one override, persisted in state"
    );

    // The read side now shows the override outranking the learned rule.
    let resp = get_policy(router.clone(), PID, Some(INSIDE_SESSION)).await;
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();
    let row = v["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["shape"] == "test/small")
        .unwrap();
    assert_eq!(row["overridden"], true);
    assert_eq!(row["effective"], "ask", "the pass skips overridden shapes");

    // Flip BACK: the override is removed, the learned rule re-applies — and
    // the recorded history is NOT zeroed (re-learns, never resets).
    let resp = flip(
        router.clone(),
        "release",
        "test/small",
        Some(INSIDE_SESSION),
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();
    assert_eq!(v["changed"], true);
    let resp = flip(
        router.clone(),
        "release",
        "test/small",
        Some(INSIDE_SESSION),
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();
    assert_eq!(v["changed"], false, "release is idempotent too");

    let snap = store.snapshot();
    assert!(snap.ask_again_shapes.is_empty());
    assert_eq!(
        snap.approval_samples.len(),
        8,
        "history survives the release"
    );

    let resp = get_policy(router, PID, Some(INSIDE_SESSION)).await;
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();
    let row = v["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["shape"] == "test/small")
        .unwrap();
    assert_eq!(row["overridden"], false);
    assert_eq!(row["effective"], "auto");
}

// --- validation & roles ---------------------------------------------------------

#[tokio::test]
async fn malformed_or_unknown_shapes_are_refused_with_the_vocabulary() {
    let (_dir, _store, router) = hub(GATE_ON, learned_state()).await;
    for bad in ["test/", "/small", "tests/small", "test/huge", ""] {
        let resp = flip(router.clone(), "ask-again", bad, Some(INSIDE_SESSION)).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad:?} must 400");
        let body = body_text(resp).await;
        assert!(
            body.contains("test, docs, bug, chore, feature"),
            "the 400 names the valid kind vocabulary: {body}"
        );
        assert!(body.contains("small, medium, large"), "{body}");
    }
}

#[tokio::test]
async fn a_member_role_may_read_the_policy_but_never_flip_it() {
    let (_dir, _store, router) = hub(GATE_ON, learned_state()).await;
    // Reading is for everyone signed in — transparency is the point.
    let resp = get_policy(router.clone(), PID, Some(MEMBER_SESSION)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    // A flip retires a learned gate rule — the same weight as undoing an
    // auto-approval, so a BE (no gate rights) is refused.
    let resp = flip(router, "ask-again", "test/small", Some(MEMBER_SESSION)).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// --- AC5: gate off --------------------------------------------------------------

#[tokio::test]
async fn a_gate_disabled_in_config_lists_no_applicable_rules() {
    let mut state = learned_state();
    state.ask_again_shapes.push("test/small".to_owned());
    let (_dir, _store, router) = hub(GATE_OFF, state).await;
    let resp = get_policy(router, PID, Some(INSIDE_SESSION)).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();
    assert_eq!(v["enabled"], false);
    assert_eq!(v["gate_off"], true);
    assert_eq!(
        v["shapes"],
        serde_json::json!([]),
        "no applicable rules while the gate is off"
    );
}

// --- AC4: undo window + expired history ------------------------------------------

#[tokio::test]
async fn undo_window_items_carry_minutes_left_and_expired_ones_become_history() {
    let mut state = learned_state();
    state
        .tickets
        .push(pending_designed("CXC-F303-1", "Test coverage: policy view"));
    state.tickets.push(pending_designed(
        "CXC-F303-2",
        "Test coverage: undo listing",
    ));
    state.auto_approved_at.insert(
        "CXC-F303-1".to_owned(),
        coxagent_application::state::now_rfc3339(),
    );
    state
        .auto_approved_at
        .insert("CXC-F303-2".to_owned(), "2020-01-01T00:00:00Z".to_owned());
    let (_dir, _store, router) = hub(GATE_ON, state).await;

    let resp = get_policy(router, PID, Some(INSIDE_SESSION)).await;
    let v: serde_json::Value = serde_json::from_str(&body_text(resp).await).unwrap();

    let undoable = v["undoable"].as_array().unwrap();
    assert_eq!(
        undoable.len(),
        1,
        "only the fresh auto-approval is undoable"
    );
    assert_eq!(undoable[0]["ticket"], "CXC-F303-1");
    assert_eq!(undoable[0]["shape"], "test/small", "attributed per shape");
    let minutes_left = undoable[0]["minutes_left"].as_u64().unwrap();
    assert!(minutes_left > 0 && minutes_left <= 30, "{minutes_left}");

    let expired = v["expired"].as_array().unwrap();
    assert_eq!(expired.len(), 1, "the closed window becomes history");
    assert_eq!(expired[0]["ticket"], "CXC-F303-2");
    assert_eq!(expired[0]["shape"], "test/small");
    assert_eq!(expired[0]["approved_at"], "2020-01-01T00:00:00Z");
    assert!(expired[0]["minutes_ago"].as_u64().unwrap_or(0) > 30);
}
