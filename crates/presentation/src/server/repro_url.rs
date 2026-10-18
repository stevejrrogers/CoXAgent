// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
//! Live reproduction for the human verify gate (CXA-F243): resolve where a
//! reviewer can open the fix RUNNING, so the verify decision rests on the
//! app itself rather than static evidence alone.

use super::*;

/// GET /api/projects/:pid/ticket/:id/reproduction-url — the optional live
/// reproduction base URL for a verify-pending (`Fixed`) ticket, plus what
/// occupies the port behind it (`reason`: `preview` | `main` | `none`).
///
/// Authorization follows the P5a recipe: a principal is required ONLY when
/// the gateway has auth enabled (open-mode hubs stay open). It is a
/// read-only, team-visible surface — like the inbox that offers the verify
/// decision itself — so any signed-in principal may resolve it; only
/// anonymous callers are refused.
///
/// Nothing here is UI logic: the snapshot is assembled from real port reads
/// (raw config parse, store load) and the resolution + health judgement live
/// in the application use case behind the deploy port.
pub(super) async fn ticket_reproduction_url_ep(
    State(app): State<AppState>,
    Path((pid, id)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if let Some(auth) = &app.auth {
        if super::resolve_principal(auth, &headers).await.is_none() {
            return (axum::http::StatusCode::UNAUTHORIZED, "sign in first").into_response();
        }
    }
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let Ok(state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(t) = state.tickets.iter().find(|t| t.id().as_str() == id) else {
        return not_found();
    };
    // The same parse every deploy call site runs (COX-B035): the raw config
    // text, never a defaulted `Config` a corrupt `host_port` could drift
    // through. A corrupt config resolves to nothing — there is no honest URL
    // to hand a reviewer — same as an unconfigured one.
    let host_port = std::fs::read_to_string(&p.config_path)
        .ok()
        .map_or(Ok(None), |text| {
            coxagent_application::ports::outbound::parse_deploy_host_port(&text)
        })
        .unwrap_or_default();
    let snapshot = coxagent_application::use_cases::ReproUrlSnapshot {
        host_port,
        // Nothing durably tracks preview state yet (a preview lives only as
        // `.preview/` worktrees + compose projects), so the resolver reports
        // the base-rate truth — what normally occupies the port is the main
        // build. The snapshot seam stays: a preview tracker feeds `true`
        // here without touching the resolver again.
        preview_live: false,
        // Verify-pending = `Fixed` and not yet `Verified` — the bug
        // lifecycle's human verify gate; there is no separate status.
        ticket_verify_pending: t.status() == coxagent_domain::Status::Fixed,
    };
    let resolved = match &p.deploy {
        Some(deploy) => {
            coxagent_application::use_cases::ResolveReproUrlUseCase::new(Arc::clone(deploy))
                .execute(&snapshot)
                .await
        }
        // No deploy adapter configured: nothing can be judged healthy, so
        // nothing may be linked.
        None => None,
    };
    let (url, reason) = match resolved {
        Some(r) => (Some(r.url), r.source.key()),
        None => (None, "none"),
    };
    Json(serde_json::json!({ "url": url, "reason": reason })).into_response()
}
