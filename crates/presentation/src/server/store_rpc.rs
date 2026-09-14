// Server half of RestStateStore: one endpoint that dispatches every
// StateStorePort operation sent by a REST-fronted runner onto this hub's own
// store adapter (Postgres+Redis or JSON). Keeps atomicity on the control plane.
#![allow(clippy::wildcard_imports)]

use crate::ProjectHandle;
use axum::{
    extract::{Path, Query},
    response::{IntoResponse, Response},
};

/// Query parameter selecting which operation to run.
#[derive(serde::Deserialize)]
pub(super) struct OpQ {
    op: String,
}

/// Query for the read-only audit surface (`GET …/store?op=audit`): the op is
/// part of the documented contract; omitting it defaults to the audit.
#[derive(serde::Deserialize)]
pub(super) struct AuditQ {
    op: Option<String>,
}

/// Optional arguments carried for each operation.
#[derive(Default, serde::Deserialize)]
pub(super) struct Args {
    revision: Option<i64>,
    data: Option<String>,
    id: Option<String>,
    worker: Option<String>,
    now: Option<String>,
    stage: Option<String>,
}

/// Serialize any value as a 200 JSON response.
fn ok<T: serde::Serialize>(v: T) -> Response {
    axum::Json(v).into_response()
}

/// Map a store error to an HTTP response; conflicts become 409 so the
/// REST-fronted runner can retry its read-modify-write.
fn err(e: coxagent_application::PortError) -> Response {
    match e {
        coxagent_application::PortError::Conflict(msg) => (
            axum::http::StatusCode::CONFLICT,
            axum::Json(serde_json::json!({ "conflict": msg })),
        )
            .into_response(),
        other => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": other.to_string() })),
        )
            .into_response(),
    }
}

/// Full project state read for a REST-fronted runner.
async fn op_load(p: &ProjectHandle) -> Response {
    match p.store.load().await {
        Ok(s) => ok(s),
        Err(e) => err(e),
    }
}

/// The current optimistic-concurrency revision, so a REST-fronted runner can
/// capture what it saw and hand it back on [`op_save`] to get stale-write
/// protection (`Some(rev)`), or learn none is tracked (`None`).
async fn op_version(p: &ProjectHandle) -> Response {
    match p.store.current_version().await {
        Ok(rev) => ok(serde_json::json!({ "revision": rev })),
        Err(e) => err(e),
    }
}

/// Persist a full state snapshot on behalf of a REST-fronted runner.
async fn op_save(p: &ProjectHandle, args: &Args) -> Response {
    let Some(Ok(state)) = args
        .data
        .as_deref()
        .map(serde_json::from_str::<coxagent_application::state::ProjectState>)
    else {
        return err(coxagent_application::PortError::Corrupt(
            "save needs data".into(),
        ));
    };
    // Forward the caller-captured revision so a stale write is rejected with a
    // Conflict instead of silently clobbering newer data. Older clients omitting
    // the field pass None and keep today's behaviour.
    match p.store.save_expecting(&state, args.revision).await {
        Ok(()) => ok(serde_json::json!({ "ok": true })),
        Err(e) => err(e),
    }
}

/// Atomically claim a ticket on the control plane.
async fn op_claim_ticket(p: &ProjectHandle, args: &Args) -> Response {
    let Some(id) = args
        .id
        .as_deref()
        .and_then(|s| coxagent_domain::TicketId::new(s.to_string()).ok())
    else {
        return err(coxagent_application::PortError::Corrupt("need id".into()));
    };
    let (Some(worker), Some(now)) = (args.worker.as_deref(), args.now.as_deref()) else {
        return err(coxagent_application::PortError::Corrupt(
            "need worker/now".into(),
        ));
    };
    match p.store.claim_ticket(&id, worker, now).await {
        Ok(won) => ok(serde_json::json!({ "won": won })),
        Err(e) => err(e),
    }
}

/// Acquire or renew project leadership for a singleton phase.
async fn op_acquire_leader(p: &ProjectHandle, args: &Args) -> Response {
    let (Some(worker), Some(now)) = (args.worker.as_deref(), args.now.as_deref()) else {
        return err(coxagent_application::PortError::Corrupt(
            "need worker/now".into(),
        ));
    };
    match p.store.acquire_leader(worker, now).await {
        Ok(won) => ok(serde_json::json!({ "won": won })),
        Err(e) => err(e),
    }
}

/// Claim a per-ticket stage lease.
async fn op_claim_stage(p: &ProjectHandle, args: &Args) -> Response {
    let Some(id) = args
        .id
        .as_deref()
        .and_then(|s| coxagent_domain::TicketId::new(s.to_string()).ok())
    else {
        return err(coxagent_application::PortError::Corrupt("need id".into()));
    };
    let (Some(stage), Some(worker), Some(now)) = (
        args.stage.as_deref(),
        args.worker.as_deref(),
        args.now.as_deref(),
    ) else {
        return err(coxagent_application::PortError::Corrupt(
            "need stage/worker/now".into(),
        ));
    };
    match p.store.claim_stage(&id, stage, worker, now).await {
        Ok(won) => ok(serde_json::json!({ "won": won })),
        Err(e) => err(e),
    }
}

/// Release a per-ticket stage lease.
async fn op_release_stage(p: &ProjectHandle, args: &Args) -> Response {
    let Some(id) = args
        .id
        .as_deref()
        .and_then(|s| coxagent_domain::TicketId::new(s.to_string()).ok())
    else {
        return err(coxagent_application::PortError::Corrupt("need id".into()));
    };
    let (Some(stage), Some(worker)) = (args.stage.as_deref(), args.worker.as_deref()) else {
        return err(coxagent_application::PortError::Corrupt(
            "need stage/worker".into(),
        ));
    };
    match p.store.release_stage(&id, stage, worker).await {
        Ok(()) => ok(serde_json::json!({ "ok": true })),
        Err(e) => err(e),
    }
}

/// Relay this runner's presence to the shared worker registry.
async fn op_heartbeat(p: &ProjectHandle, args: &Args) -> Response {
    #[derive(serde::Deserialize)]
    struct Beat {
        role: String,
        ticket: Option<String>,
        engines: Option<Vec<String>>,
        models: Option<Vec<String>>,
        git: Option<String>,
        tooling: Option<String>,
        version: Option<String>,
        now: String,
    }
    let Some(b) = args
        .data
        .as_deref()
        .and_then(|d| serde_json::from_str::<Beat>(d).ok())
    else {
        return err(coxagent_application::PortError::Corrupt(
            "need heartbeat data".into(),
        ));
    };
    let caps = coxagent_application::ports::outbound::WorkerCaps {
        engines: b.engines.unwrap_or_default(),
        models: b.models.unwrap_or_default(),
        git: b.git.as_deref().and_then(|s| serde_json::from_str(s).ok()),
        tooling: b
            .tooling
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        version: b.version.unwrap_or_default(),
    };
    let worker = args.worker.as_deref().unwrap_or("");
    match p
        .store
        .heartbeat_worker(
            worker,
            &b.role,
            b.ticket.as_deref().unwrap_or(""),
            &caps,
            &b.now,
        )
        .await
    {
        Ok(()) => ok(serde_json::json!({ "ok": true })),
        Err(e) => err(e),
    }
}

/// List workers currently online (from the shared registry).
async fn op_workers(p: &ProjectHandle) -> Response {
    match p.store.workers().await {
        Ok(w) => ok(w),
        Err(e) => err(e),
    }
}

/// Persist an operator's desired run state (data carries "true"/"false").
async fn op_set_desired(p: &ProjectHandle, args: &Args) -> Response {
    let running = args
        .data
        .as_deref()
        .and_then(|s| s.parse::<bool>().ok())
        .unwrap_or(false);
    match p
        .store
        .set_desired(args.worker.as_deref().unwrap_or(""), running)
        .await
    {
        Ok(()) => ok(serde_json::json!({ "ok": true })),
        Err(e) => err(e),
    }
}

/// Read an operator's persisted desired run state.
async fn op_get_desired(p: &ProjectHandle, args: &Args) -> Response {
    match p
        .store
        .get_desired(args.worker.as_deref().unwrap_or(""))
        .await
    {
        Ok(v) => ok(serde_json::json!({ "value": v })),
        Err(e) => err(e),
    }
}

/// Acquire/renew the single-instance lock for an operator (worker=operator,
/// now=instance).
async fn op_acquire_operator(p: &ProjectHandle, args: &Args) -> Response {
    let (Some(operator), Some(instance)) = (args.worker.as_deref(), args.now.as_deref()) else {
        return err(coxagent_application::PortError::Corrupt(
            "need worker/now".into(),
        ));
    };
    match p.store.acquire_operator(operator, instance).await {
        Ok(won) => ok(serde_json::json!({ "won": won })),
        Err(e) => err(e),
    }
}

/// P5a defense-in-depth: when hub auth is configured every /store call needs
/// a manage-tier principal who is a member of :pid (Super/Admin exempt) — the
/// exact policy `auth_mw` enforces for this path (write gate -> `can_manage`,
/// plus per-project membership). The middleware already rejects traffic while
/// /store sits under it; enforcing the same decision HERE keeps the adapter
/// closed even if /store ever moves out from under that middleware — an
/// authenticated-but-outsider session must be refused by the endpoint itself
/// instead of being forwarded to the store adapter. Shared by the POST
/// runner surface and the GET audit surface so both stay in lockstep.
#[allow(clippy::result_large_err)] // axum Response<Body> is ~128B; boxing it would ripple every handler return site
async fn authorize_store_call(
    app: &super::AppState,
    pid: &str,
    headers: &axum::http::HeaderMap,
) -> Result<(), axum::response::Response> {
    let Some(auth) = &app.auth else {
        return Ok(());
    };
    let Some(user) = super::resolve_principal(auth, headers).await else {
        return Err((axum::http::StatusCode::UNAUTHORIZED, "sign in first").into_response());
    };
    if !user.role.can_manage() {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "management role required"
            })),
        )
            .into_response());
    }
    let is_super_or_admin = user.role == coxagent_application::auth::AuthRole::Super
        || user.role == coxagent_application::auth::AuthRole::Admin;
    if !is_super_or_admin && !user.projects.iter().any(|p| p == pid) {
        return Err((
            axum::http::StatusCode::FORBIDDEN,
            axum::Json(serde_json::json!({
                "error": "not a member of this project"
            })),
        )
            .into_response());
    }
    Ok(())
}

/// The read-only structural-integrity audit for one project's persisted
/// state (CXA-F229): findings plus the quarantine ledger of write-backs the
/// gate already refused. The findings are computed purely from the loaded
/// snapshot; the handler adds no business logic beyond marshalling.
async fn op_audit(p: &ProjectHandle) -> Response {
    match p.store.load().await {
        Ok(state) => {
            let findings = state.audit_structural_integrity();
            let healthy = findings.is_empty();
            let quarantined = p.store.quarantined().await;
            ok(serde_json::json!({
                "findings": findings,
                "healthy": healthy,
                "quarantined": quarantined,
            }))
        }
        Err(e) => err(e),
    }
}

/// Opt-in self-heal (CXA-F229): audit first, then drop exactly the dangling
/// ticket-keyed map entries the audit reported — never anything else; every
/// other finding class needs a human decision and refuses the heal. The
/// mutation goes through `mutate_state`, so it inherits the port's
/// revision/concurrency guards and the write-time validation + audit gate.
async fn op_heal(p: &ProjectHandle) -> Response {
    let healed = std::sync::atomic::AtomicUsize::new(0);
    let outcome =
        coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |state| match state
            .heal_dangling_references()
        {
            Ok(n) => {
                healed.store(n, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            Err(violation) => Err(coxagent_application::PortError::Corrupt(
                violation.to_string(),
            )),
        })
        .await;
    match outcome {
        Ok(()) => {
            // Report against the state that actually persisted: a concurrent
            // writer could have changed things after the heal landed.
            match p.store.load().await {
                Ok(state) => {
                    let findings = state.audit_structural_integrity();
                    ok(serde_json::json!({
                        "healed": healed.load(std::sync::atomic::Ordering::Relaxed),
                        "healthy": findings.is_empty(),
                        "findings": findings,
                    }))
                }
                Err(e) => err(e),
            }
        }
        Err(e) => err(e),
    }
}

/// Purge this project's persisted state on the control plane — the REST
/// counterpart of deregistering a project (CXA-B130). Sits behind the same
/// manage-tier + membership gate as every other store write, and the hub's
/// own delete flow calls the adapter directly, so no runner invokes this
/// today; it exists so a REST-fronted `delete()` can never silently no-op.
async fn op_delete(p: &ProjectHandle) -> Response {
    match p.store.delete().await {
        Ok(()) => ok(serde_json::json!({ "ok": true })),
        Err(e) => err(e),
    }
}

/// Entry point routing every runner operation onto this project's store.
pub(super) async fn store_rpc_ep(
    axum::extract::State(app): axum::extract::State<super::AppState>,
    Path(pid): Path<String>,
    Query(q): Query<OpQ>,
    headers: axum::http::HeaderMap,
    axum::Json(args): axum::Json<Args>,
) -> Response {
    let Some(p) = app.project(&pid).await else {
        return super::not_found();
    };
    if let Err(refused) = authorize_store_call(&app, &pid, &headers).await {
        return refused;
    }
    match q.op.as_str() {
        "load" => op_load(&p).await,
        "version" => op_version(&p).await,
        "save" => op_save(&p, &args).await,
        "claim_ticket" => op_claim_ticket(&p, &args).await,
        "acquire_leader" => op_acquire_leader(&p, &args).await,
        "claim_stage" => op_claim_stage(&p, &args).await,
        "release_stage" => op_release_stage(&p, &args).await,
        "heartbeat" => op_heartbeat(&p, &args).await,
        "workers" => op_workers(&p).await,
        "set_desired" => op_set_desired(&p, &args).await,
        "get_desired" => op_get_desired(&p, &args).await,
        "acquire_operator" => op_acquire_operator(&p, &args).await,
        "audit" => op_audit(&p).await,
        "heal" => op_heal(&p).await,
        "delete" => op_delete(&p).await,
        other => err(coxagent_application::PortError::Backend(format!(
            "unknown store op: {other}"
        ))),
    }
}

/// Read-only integrity audit: `GET /api/projects/:pid/store?op=audit`
/// (CXA-F229). Same `auth_mw` layering as the POST surface plus the same
/// in-handler defense-in-depth gate, so the audit of a project's state is
/// exactly as protected as writing it.
pub(super) async fn store_audit_ep(
    axum::extract::State(app): axum::extract::State<super::AppState>,
    Path(pid): Path<String>,
    Query(q): Query<AuditQ>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(p) = app.project(&pid).await else {
        return super::not_found();
    };
    if let Err(refused) = authorize_store_call(&app, &pid, &headers).await {
        return refused;
    }
    match q.op.as_deref() {
        None | Some("audit") => op_audit(&p).await,
        Some(other) => err(coxagent_application::PortError::Backend(format!(
            "unknown store op: {other}"
        ))),
    }
}
