//! CXA-B129/CXA-B138/CXA-B139 regression: `POST /api/projects` maps the
//! injected factory's failure CLASS to the right status — an expected client
//! conflict (the target workspace already holds tickets, e.g. after
//! delete-then-recreate against a surviving store row) is 409, invalid client
//! input (a bad git URL scheme, a path-traversing alias) is 400, both with the
//! same JSON error shape, never a 500.
//! Pure in-process: requests answered via `tower::ServiceExt::oneshot`, no
//! hub process, no host harness, no TCP port.

use super::*;
use axum::body::Body;
use coxagent_infrastructure::MemoryAuditSink;
use std::sync::Arc;
use store_rpc_test_support::{CountingStore, UnusedEngine, PID};
use tower::ServiceExt;

/// A hub with no registered projects and the given factory; the exact shape
/// `create_project` needs (empty space registry → no space required).
async fn hub_with_factory(factory: ProjectFactory) -> AppState {
    let dir = tempfile::tempdir().expect("tempdir");
    build_state(
        Vec::new(),
        Arc::new(MemoryAuditSink::default()),
        HubExtras {
            factory: Some(factory),
            hub_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
}

/// A factory stub that answers every create with the same classified result.
fn factory_returning(result: Result<ProjectHandle, FactoryError>) -> ProjectFactory {
    Arc::new(move |_req| {
        let result = result.clone();
        Box::pin(async move { result })
    })
}

async fn post_create(state: AppState) -> axum::response::Response {
    post_create_with_body(state, r#"{"name":"QA-B126-Conflict"}"#).await
}

async fn post_create_with_body(state: AppState, body: &str) -> axum::response::Response {
    Router::new()
        .route("/api/projects", post(super::projects::create_project))
        .with_state(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_owned()))
                .expect("well-formed request"),
        )
        .await
        .expect("in-process request")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// The repro class: the derived id collides with a store row that survived a
/// delete (companion cleanup bug), so greenfield refuses to re-onboard. That
/// is the CLIENT asking for something already true — 409, not 500.
#[tokio::test]
async fn a_workspace_conflict_is_mapped_to_409_with_the_error_json() {
    let app = hub_with_factory(factory_returning(Err(FactoryError::conflict(
        "workspace already has tickets; refusing to re-onboard",
    ))))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "workspace already has tickets; refusing to re-onboard",
        "the operator-facing message must survive the status mapping"
    );
}

/// Any other factory failure stays a genuine server fault: 500.
#[tokio::test]
async fn an_unclassified_factory_failure_stays_a_500() {
    let app = hub_with_factory(factory_returning(Err(FactoryError::internal(
        "store unreachable",
    ))))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// CXA-B139 regression: invalid client input classified by the factory —
/// e.g. the git-URL scheme check on a `ftp://` import — must reach the client
/// as 400 with the same JSON error shape, never a 500 that invites a retry
/// which can never succeed.
#[tokio::test]
async fn a_bad_request_failure_is_mapped_to_400_with_the_error_json() {
    let app = hub_with_factory(factory_returning(Err(FactoryError::bad_request(
        "git URL must start with git@, https:// or http://",
    ))))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "git URL must start with git@, https:// or http://",
        "the operator-facing message must survive the status mapping"
    );
}

/// CXA-B138: a path-traversing alias would be `base.join`-ed into a workspace
/// OUTSIDE the hub's workspace base (the docker deploy escapes to container
/// root, and DELETE then `rm -rf`s it). The route must refuse it with 400
/// BEFORE the factory is ever called — the factory below panics to prove it.
#[tokio::test]
async fn a_path_traversing_alias_is_refused_with_400_before_the_factory_runs() {
    let factory_was_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&factory_was_called);
    let app = hub_with_factory(Arc::new(move |_req| {
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Err::<ProjectHandle, FactoryError>(FactoryError::internal("must never run"))
        })
    }))
    .await;

    for alias in [
        "../qatrav-esc", // the ticket's repro
        "..\\qatrav",
        "qa/../trav",
        "..",
        ".",
        ".hidden",
        "a/b",
    ] {
        let body = format!(
            r#"{{"name":"QA Trav","alias":"{}"}}"#,
            alias.replace('\\', "\\\\")
        );
        let resp = post_create_with_body(app.clone(), &body).await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "alias {alias:?} must be refused with 400"
        );
        let body = body_json(resp).await;
        assert!(
            body["error"].as_str().unwrap_or_default().contains("alias"),
            "the refusal must say why: {body}"
        );
    }
    assert!(
        !factory_was_called.load(std::sync::atomic::Ordering::SeqCst),
        "the factory must never see a traversing alias"
    );
}

/// CXA-B140: a control character in the alias (the ticket's raw newline and
/// JSON-escaped backspace repros) lands in the project id, its workspace
/// directory name and registry.json, mangles every listing, and is
/// non-obviously deletable — DELETE needs the byte percent-encoded to match.
/// The route must refuse it with 400 BEFORE the factory is ever called — the
/// factory below records every call, and the flag must stay false.
#[tokio::test]
async fn a_control_character_alias_is_refused_with_400_before_the_factory_runs() {
    let factory_was_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&factory_was_called);
    let app = hub_with_factory(Arc::new(move |_req| {
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Err::<ProjectHandle, FactoryError>(FactoryError::internal("must never run"))
        })
    }))
    .await;

    for alias in [
        "bad\nid", // the ticket's newline repro
        "a\u{8}",  // the ticket's backspace repro (JSON "\b")
        "bad\rid",
        "bad\tid",
        "trailing\n",
    ] {
        let body = serde_json::to_string(&serde_json::json!({
            "name": "NL Probe",
            "alias": alias,
        }))
        .expect("json body");
        let resp = post_create_with_body(app.clone(), &body).await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "alias {alias:?} must be refused with 400"
        );
        let body = body_json(resp).await;
        assert!(
            body["error"].as_str().unwrap_or_default().contains("alias"),
            "the refusal must say why: {body}"
        );
    }
    assert!(
        !factory_was_called.load(std::sync::atomic::Ordering::SeqCst),
        "the factory must never see a control-character alias"
    );
}

/// The honest counterpart: a plain alias still reaches the factory untouched.
#[tokio::test]
async fn a_plain_alias_still_reaches_the_factory() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = Arc::clone(&seen);
    let app = hub_with_factory(Arc::new(move |req| {
        *recorder.lock().expect("poisoned") = Some(req.alias);
        Box::pin(async {
            Err::<ProjectHandle, FactoryError>(FactoryError::internal("stop after capture"))
        })
    }))
    .await;

    let resp = post_create_with_body(app, r#"{"name":"QA Trav","alias":"QATRAV"}"#).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        *seen.lock().expect("poisoned"),
        Some(Some("QATRAV".to_owned()))
    );
}

/// Pin the empty-alias contract: "" is normalized to ABSENT like every other
/// optional field (CXA-B146 — trim, blank→absent), so it is never a 400 and
/// the factory sees `None` — the port then derives the alias from the name.
#[tokio::test]
async fn an_empty_alias_reaches_the_factory_as_absent() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = Arc::clone(&seen);
    let app = hub_with_factory(Arc::new(move |req| {
        *recorder.lock().expect("poisoned") = Some(req.alias);
        Box::pin(async {
            Err::<ProjectHandle, FactoryError>(FactoryError::internal("stop after capture"))
        })
    }))
    .await;

    let resp = post_create_with_body(app, r#"{"name":"QA Trav","alias":""}"#).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(*seen.lock().expect("poisoned"), Some(None));
}

/// CXA-B146 regression: a whitespace-only alias used to pass validation raw,
/// become the project id and scaffold a workspace directory named `"   "` —
/// deletable only via percent-encoded DELETE. It is trimmed like every other
/// input: the factory sees `None` (absent → derive-from-name), never the
/// spaces, and nothing is refused (a blank optional is absent, not an error).
#[tokio::test]
async fn a_whitespace_only_alias_reaches_the_factory_as_absent() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = Arc::clone(&seen);
    let app = hub_with_factory(Arc::new(move |req| {
        *recorder.lock().expect("poisoned") = Some(req.alias);
        Box::pin(async {
            Err::<ProjectHandle, FactoryError>(FactoryError::internal("stop after capture"))
        })
    }))
    .await;

    // NBSP pins the Unicode-whitespace boundary. A blank made of CONTROL
    // characters (e.g. " \t\n") is deliberately NOT here: the B140 guard runs
    // on the raw alias before the trim, so that shape is refused with 400.
    for alias in ["   ", "\u{a0}"] {
        let body = serde_json::to_string(&serde_json::json!({
            "name": "QA Space",
            "alias": alias,
        }))
        .expect("json body");
        let resp = post_create_with_body(app.clone(), &body).await;
        assert_eq!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a blank alias is absent, not refused: {alias:?}"
        );
        assert_eq!(
            *seen.lock().expect("poisoned"),
            Some(None),
            "the factory must never see the whitespace-only alias {alias:?}"
        );
    }
}

/// CXA-B146: a padded alias is trimmed before the factory sees it, so its
/// spaces can never become part of the workspace id.
#[tokio::test]
async fn a_padded_alias_is_trimmed_before_the_factory_sees_it() {
    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = Arc::clone(&seen);
    let app = hub_with_factory(Arc::new(move |req| {
        *recorder.lock().expect("poisoned") = Some(req.alias);
        Box::pin(async {
            Err::<ProjectHandle, FactoryError>(FactoryError::internal("stop after capture"))
        })
    }))
    .await;

    let resp = post_create_with_body(app, r#"{"name":"QA Trav","alias":"  QATRAV  "}"#).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        *seen.lock().expect("poisoned"),
        Some(Some("QATRAV".to_owned()))
    );
}

/// A hub with one registered project (`default`) whose working tree is
/// `work_dir`, plus the given factory — the CXA-B145 repro shape. The caller
/// keeps `work_dir`'s TempDir alive: the ownership check canonicalises both
/// sides, so the tree must exist.
async fn hub_with_live_project(work_dir: &std::path::Path, factory: ProjectFactory) -> AppState {
    let dir = tempfile::tempdir().expect("tempdir");
    build_state(
        vec![ProjectHandle {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            alias: "DEFAULT".to_owned(),
            store: Arc::new(CountingStore::seeded(
                coxagent_application::state::ProjectState::default(),
            )),
            runner: Arc::new(RunnerHandle::default()),
            config_path: work_dir.join("coxagent.json"),
            engine: Arc::new(UnusedEngine),
            work_dir: work_dir.to_path_buf(),
            outbox: None,
            budget: Arc::new(std::sync::Mutex::new(
                coxagent_application::config::BudgetCaps::default(),
            )),
            context_path: work_dir.join("project_context.md"),
            forge: None,
            deploy: None,
            storage: None,
            files: None,
            deps_discovery: None,
        }],
        Arc::new(MemoryAuditSink::default()),
        HubExtras {
            factory: Some(factory),
            hub_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await
}

/// A factory stub that records that it ran — every CXA-B145 refusal must
/// happen BEFORE the factory touches the filesystem.
fn never_run_factory(flag: &Arc<std::sync::atomic::AtomicBool>) -> ProjectFactory {
    let seen = Arc::clone(flag);
    Arc::new(move |_req| {
        seen.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Err::<ProjectHandle, FactoryError>(FactoryError::internal("must never run"))
        })
    })
}

/// CXA-B145 repro: `POST /api/projects` adopting the working tree of a live
/// registered project (`existing` = default's codebase) must be refused with
/// 409 BEFORE the factory runs — two runners doing branch-per-ticket git,
/// deploys and version bumps over one tree corrupt each other.
#[tokio::test]
async fn adopting_another_registered_projects_codebase_is_refused_with_409() {
    let tree = tempfile::tempdir().expect("codebase dir");
    let factory_was_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let app = hub_with_live_project(tree.path(), never_run_factory(&factory_was_called)).await;

    let body = serde_json::to_string(&serde_json::json!({
        "name": "QA Reonboard",
        "alias": "qareonb",
        "existing": tree.path().to_str().expect("utf8 path"),
    }))
    .expect("json body");
    let resp = post_create_with_body(app, &body).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "the adoption must be a client conflict, not a 500"
    );
    let body = body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("default"),
        "the refusal must name the owning project: {body}"
    );
    assert!(
        !factory_was_called.load(std::sync::atomic::Ordering::SeqCst),
        "the factory must never adopt an owned codebase"
    );
}

/// The owned tree reached through a symlink alias refuses too: both sides are
/// canonicalised, so a link pointing into a live project's codebase is the
/// same adoption (CXA-B145).
#[cfg(unix)]
#[tokio::test]
async fn a_symlink_into_a_registered_codebase_is_refused_with_409() {
    let tree = tempfile::tempdir().expect("codebase dir");
    let alias_dir = tempfile::tempdir().expect("alias dir");
    std::os::unix::fs::symlink(tree.path(), alias_dir.path().join("alias"))
        .expect("create symlink");
    let factory_was_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let app = hub_with_live_project(tree.path(), never_run_factory(&factory_was_called)).await;

    let body = serde_json::to_string(&serde_json::json!({
        "name": "QA Reonboard",
        "existing": alias_dir.path().join("alias").to_str().expect("utf8 path"),
    }))
    .expect("json body");
    let resp = post_create_with_body(app, &body).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert!(
        !factory_was_called.load(std::sync::atomic::Ordering::SeqCst),
        "the factory must never adopt an owned codebase through a link"
    );
}

/// A path that cannot be resolved to a real directory (here: a ghost entry
/// under the owned tree) is NOT the ownership guard's call — it falls through
/// to the factory, whose honest "codebase path does not exist" refusal (and
/// CXA-B136 cleanup) owns that case. The stub's 500 proves the guard stayed
/// silent instead of inventing a verdict about a path it cannot see.
#[tokio::test]
async fn an_unresolvable_import_path_falls_through_to_the_factory() {
    let tree = tempfile::tempdir().expect("default's codebase dir");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = Arc::clone(&seen);
    let app = hub_with_live_project(
        tree.path(),
        Arc::new(move |req| {
            *recorder.lock().expect("poisoned") = Some(req.existing.clone());
            Box::pin(async {
                Err::<ProjectHandle, FactoryError>(FactoryError::internal("stop after capture"))
            })
        }),
    )
    .await;

    let body = serde_json::to_string(&serde_json::json!({
        "name": "Ghost Adopt",
        "existing": tree.path().join("ghost").to_str().expect("utf8 path"),
    }))
    .expect("json body");
    let resp = post_create_with_body(app, &body).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        *seen.lock().expect("poisoned"),
        Some(Some(tree.path().join("ghost")))
    );
}

/// The honest counterpart: a codebase no registered project owns still
/// reaches the factory with the path intact — the refusal must not over-fire.
#[tokio::test]
async fn an_unowned_codebase_still_reaches_the_factory() {
    let tree = tempfile::tempdir().expect("default's codebase dir");
    let other = tempfile::tempdir().expect("unrelated codebase dir");
    let seen = Arc::new(std::sync::Mutex::new(None));
    let recorder = Arc::clone(&seen);
    let app = hub_with_live_project(
        tree.path(),
        Arc::new(move |req| {
            *recorder.lock().expect("poisoned") = Some(req.existing.clone());
            Box::pin(async {
                Err::<ProjectHandle, FactoryError>(FactoryError::internal("stop after capture"))
            })
        }),
    )
    .await;

    let body = serde_json::to_string(&serde_json::json!({
        "name": "Fresh Adopt",
        "existing": other.path().to_str().expect("utf8 path"),
    }))
    .expect("json body");
    let resp = post_create_with_body(app, &body).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        *seen.lock().expect("poisoned"),
        Some(Some(other.path().to_path_buf()))
    );
}

/// The port may also refuse a request itself (CXA-B138): a factory-classified
/// bad request reaches the client as 400 with the JSON error shape, never 500.
#[tokio::test]
async fn a_factory_classified_bad_request_is_mapped_to_400() {
    let app = hub_with_factory(factory_returning(Err(FactoryError::bad_request(
        "alias \"../x\" must not contain '/', '\\', '..' or leading dots",
    ))))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body["error"].as_str().unwrap_or_default().contains("alias"),
        "the operator-facing message must survive the status mapping: {body}"
    );
}

/// Sanity pin on the harness: the route under test is the real one, and a
/// successful factory still lands `{"ok":true,"id":...}`.
#[tokio::test]
async fn a_successful_factory_still_returns_200_with_the_id() {
    let app = hub_with_factory(factory_returning(Ok(ProjectHandle {
        id: PID.to_owned(),
        name: "QA-B126-Conflict".to_owned(),
        alias: "QABC".to_owned(),
        store: Arc::new(CountingStore::seeded(
            coxagent_application::state::ProjectState::default(),
        )),
        runner: Arc::new(RunnerHandle::default()),
        config_path: std::path::PathBuf::from("/tmp/qa/coxagent.json"),
        engine: Arc::new(UnusedEngine),
        work_dir: std::path::PathBuf::from("/tmp/qa"),
        outbox: None,
        budget: Arc::new(std::sync::Mutex::new(
            coxagent_application::config::BudgetCaps::default(),
        )),
        context_path: std::path::PathBuf::from("/tmp/qa/project_context.md"),
        forge: None,
        deploy: None,
        storage: None,
        files: None,
        deps_discovery: None,
    })))
    .await;

    let resp = post_create(app).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], serde_json::json!(true));
    assert_eq!(body["id"], PID);
}
