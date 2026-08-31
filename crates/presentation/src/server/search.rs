// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Global search (CXA-F275): `GET /api/search?q=<text>&pid=<project-id>?` —
//! one query across the caller's tickets, wiki pages and chat threads, so an
//! operator at a human gate finds the prior art without opening views one by
//! one. The ranking/scoping decisions live in the pure use case
//! (`application/use_cases/global_search.rs`); this handler only resolves the
//! principal, enforces project access, and flattens the grouped result into
//! the SA's bare-array wire contract.

use super::*;

/// Query parameters. `pid` is optional: with it, one project is searched;
/// without it, every project the caller may view is swept and merged.
#[derive(serde::Deserialize)]
pub(super) struct SearchParams {
    q: Option<String>,
    pid: Option<String>,
}

/// A viewer with no access never receives another project's rows (AC2).
///
/// NOTE: this route carries no `:pid` path segment, so `auth_mw`'s
/// per-project membership gate (`extract_pid_from_path`) never fires — the
/// handler enforces scope itself, with the same Super/Admin bypass and the
/// same 403 the per-project routes answer.
pub(super) async fn global_search_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<SearchParams>,
) -> axum::response::Response {
    let term = params.q.unwrap_or_default();
    let viewer = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(user) => user,
            None => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "unauthenticated" })),
                )
                    .into_response()
            }
        },
        // Open mode (no accounts configured): the single local user IS the
        // operator, as everywhere else (see `principal_name`).
        None => coxagent_application::AuthUser {
            username: "operator".to_owned(),
            name: String::new(),
            email: String::new(),
            role: coxagent_application::auth::AuthRole::Super,
            projects: Vec::new(),
        },
    };

    // One explicit project — the palette's default (the current project).
    if let Some(pid) = params.pid.as_deref().filter(|p| !p.is_empty()) {
        let allowed = viewer.role.is_super()
            || viewer.role == coxagent_application::auth::AuthRole::Admin
            || viewer.projects.iter().any(|m| m == pid);
        if !allowed {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "not a member of this project" })),
            )
                .into_response();
        }
        let Some(p) = app.project(pid).await else {
            return not_found();
        };
        // A store that fails to load contributes nothing — a read-only search
        // surface never turns a store hiccup into an error page (AC3's
        // never-an-error spirit applied to the whole corpus).
        let Ok(state) = p.store.load().await else {
            return Json(Vec::<coxagent_application::use_cases::SearchHit>::new()).into_response();
        };
        let grouped = coxagent_application::use_cases::global_search(&state, pid, &term, &viewer);
        return Json(grouped.flat()).into_response();
    }

    // No pid: sweep every registered project the caller may view, in registry
    // order, and merge into one ranked, capped response. Store failures skip
    // that project silently for the same reason as above.
    let mut loaded: Vec<(String, coxagent_application::ProjectState)> = Vec::new();
    {
        let map = app.projects.read().await;
        let order = app.order.read().await;
        for id in order.iter() {
            let Some(handle) = map.get(id) else { continue };
            if let Ok(state) = handle.store.load().await {
                loaded.push((id.clone(), state));
            }
        }
    }
    let joined = loaded.iter().map(|(id, state)| (id.as_str(), state));
    let grouped = coxagent_application::use_cases::global_search_many(joined, &term, &viewer);
    Json(grouped.flat()).into_response()
}
