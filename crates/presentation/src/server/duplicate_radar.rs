// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Cross-project duplicate proposal/ticket radar (CXA-F253): the HTTP adapter
//! around the pure [`coxagent_application::use_cases::duplicate_radar`]
//! decision core. Each project's BA dedupes only against its own board, so
//! the same generic feature gets independently invented in every project;
//! this surface runs the existing similarity predicate across registered
//! projects and hands the matches to a human — redirect / reject / allow —
//! never auto-suppressing (a legitimately-shared infrastructure ticket must
//! stay allowed).
//!
//! Content isolation: only title + scope metadata crosses projects, and only
//! between projects the requesting principal may see (the same visibility
//! rule `river_scope` enforces for the fleet river).

use super::*;
use coxagent_application::use_cases::duplicate_radar::{
    find_cross_project_duplicates, pair_key, DuplicatePair, TicketSnapshot,
};

/// GET /api/workspace/duplicates — every cross-project duplicate pair among
/// the caller's visible projects. Auth is `auth_mw`'s session/bearer gate
/// (like `/api/fleet/river`); pair visibility is scoped per principal, so a
/// member never learns another team's ticket titles.
pub(super) async fn duplicates_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let principal = match &app.auth {
        Some(auth) => resolve_principal(auth, &headers).await,
        // Open mode (no accounts configured): the operator sees everything.
        None => None,
    };
    let (registered, allowed_keys) = {
        let order = app.order.read().await;
        let map = app.projects.read().await;
        let registered: Vec<String> = order
            .iter()
            .filter(|id| map.contains_key(*id))
            .cloned()
            .collect();
        let allowed: Vec<String> = app
            .workspace
            .inner
            .lock()
            .await
            .dupe_allowlist
            .iter()
            .map(|a| a.key.clone())
            .collect();
        (registered, allowed)
    };
    let visible = river_scope(principal.as_ref(), &registered);
    let mut snapshots: Vec<TicketSnapshot> = Vec::new();
    {
        let map = app.projects.read().await;
        for pid in &visible {
            let Some(p) = map.get(pid) else { continue };
            // One store that fails to load drops that project from the run —
            // never the whole radar.
            let Ok(state) = p.store.load().await else {
                continue;
            };
            for t in &state.tickets {
                if !radar_active(t.status()) {
                    continue;
                }
                snapshots.push(TicketSnapshot {
                    project_id: p.id.clone(),
                    project_name: p.name.clone(),
                    ticket_id: t.id().as_str().to_owned(),
                    title: t.title().to_owned(),
                    scope: scope_snippet(t.description()),
                    service_tag: t.service_tag().map(ToOwned::to_owned),
                });
            }
        }
    }
    let pairs = find_cross_project_duplicates(&snapshots, &allowed_keys);
    Json(json!({
        "crossProjectDuplicates": pairs.iter().map(pair_json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// One radar entry for the view: home side flattened (the SA contract's named
/// keys) plus every duplicate of it, each carrying id, project, scope and the
/// computed similarity score (AC2).
fn pair_json(e: &DuplicatePair) -> serde_json::Value {
    json!({
        "homeProjectId": e.home.project_id,
        "homeProjectName": e.home.project_name,
        "homeTicketId": e.home.ticket_id,
        "homeTicketTitle": e.home.title,
        "homeTicketScope": e.home.scope,
        "normalizedTitle": e.normalized_title,
        "dupTitle": e.dups.first().map(|d| d.title.clone()).unwrap_or_default(),
        "duplicates": e.dups.iter().map(|d| json!({
            "projectId": d.project_id,
            "projectName": d.project_name,
            "ticketId": d.ticket_id,
            "title": d.title,
            "scope": d.scope,
            "score": d.score,
        })).collect::<Vec<_>>(),
    })
}

/// The radar's active pool — the same resolved set `add_ticket`'s duplicate
/// gate uses: shipped-complete or rejected tickets are out of the pool ON
/// THEIR SIDE ONLY, so a redirect/reject verdict naturally retires the pair.
fn radar_active(status: coxagent_domain::Status) -> bool {
    !matches!(
        status,
        coxagent_domain::Status::Done
            | coxagent_domain::Status::Documented
            | coxagent_domain::Status::Verified
            | coxagent_domain::Status::Rejected
    )
}

/// Scope metadata that may cross projects: the ticket's own description,
/// trimmed and capped. Raw documents never do.
fn scope_snippet(desc: &str) -> String {
    const MAX_CHARS: usize = 200;
    let d = desc.trim();
    if d.chars().count() <= MAX_CHARS {
        d.to_owned()
    } else {
        let mut out: String = d.chars().take(MAX_CHARS).collect();
        out.push('…');
        out
    }
}

/// POST /api/workspace/duplicates/action — the human's explicit verdict on
/// one reported pair (AC3). `allow` persists the pair in the hub-wide
/// allowlist so future radar runs exclude it; `redirect`/`reject` retire the
/// duplicate through the SAME guarded domain transition the per-project
/// reject route uses (a `User` acts as super-PO), with an activity + audit
/// trail naming the home ticket.
pub(super) async fn duplicates_action_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<DupeActionReq>,
) -> axum::response::Response {
    // A qualified person in front of the decision — Viewer (read-only by
    // definition) must not resolve pairs, same gate as the inbox verdicts.
    let Some(me) = gate_principal(
        &app,
        &headers,
        coxagent_application::auth::AuthRole::can_approve_ready,
    )
    .await
    else {
        return (
            StatusCode::FORBIDDEN,
            "your role may not take this decision",
        )
            .into_response();
    };
    // Both sides of the pair must be visible to this principal — a member
    // must not resolve (or even name) a pair reaching into another team's
    // project they cannot see.
    let principal = match &app.auth {
        Some(auth) => resolve_principal(auth, &headers).await,
        None => None,
    };
    let registered: Vec<String> = {
        let order = app.order.read().await;
        let map = app.projects.read().await;
        order
            .iter()
            .filter(|id| map.contains_key(*id))
            .cloned()
            .collect()
    };
    let visible = river_scope(principal.as_ref(), &registered);
    if !visible.contains(&req.home_project_id) || !visible.contains(&req.dup_project_id) {
        return (
            StatusCode::FORBIDDEN,
            "one side of this pair is not visible to you",
        )
            .into_response();
    }
    let response = match req.action.as_str() {
        "allow" => allow_pair(&app, &req, &me).await,
        "redirect" | "reject" => retire_duplicate(&app, &req, &me, &req.action).await,
        other => {
            return (StatusCode::BAD_REQUEST, format!("unknown action: {other}")).into_response()
        }
    };
    response
}

#[derive(serde::Deserialize)]
pub(super) struct DupeActionReq {
    /// `allow` | `redirect` | `reject`.
    pub action: String,
    #[serde(default)]
    pub home_project_id: String,
    #[serde(default)]
    pub home_ticket_id: String,
    #[serde(default)]
    pub dup_project_id: String,
    #[serde(default)]
    pub dup_ticket_id: String,
}

/// Allow-and-keep both (AC3): persist the pair so the radar never reports it
/// again. Keyed by the order-independent pair key — free-text title keys
/// break on rename (SA ruling).
async fn allow_pair(app: &AppState, req: &DupeActionReq, by: &str) -> axum::response::Response {
    let key = pair_key(
        (&req.home_project_id, &req.home_ticket_id),
        (&req.dup_project_id, &req.dup_ticket_id),
    );
    {
        let mut doc = app.workspace.inner.lock().await;
        if !doc.dupe_allowlist.iter().any(|a| a.key == key) {
            doc.dupe_allowlist.push(super::hub_docs::DupeAllow {
                key: key.clone(),
                by: by.to_owned(),
                at: now_rfc3339(),
            });
        }
    }
    app.workspace.save().await;
    Json(json!({ "ok": true, "key": key })).into_response()
}

/// Retire the duplicate side through the guarded domain transition. Redirect
/// and reject differ only in the trail they write: both move the duplicate
/// ticket to `Rejected` in ITS OWN project, which removes the pair from
/// future radar runs with no extra bookkeeping.
async fn retire_duplicate(
    app: &AppState,
    req: &DupeActionReq,
    by: &str,
    action: &str,
) -> axum::response::Response {
    let Some(p) = app.project(&req.dup_project_id).await else {
        return not_found();
    };
    let Ok(mut state) = p.store.load().await else {
        return internal_error("load failed");
    };
    let Some(t) = state
        .tickets
        .iter_mut()
        .find(|t| t.id().as_str() == req.dup_ticket_id)
    else {
        return (StatusCode::NOT_FOUND, "no such ticket").into_response();
    };
    // The aggregate enforces who may reject from which status — a duplicate
    // already InProgress is a 409 here, not a silent force-close.
    if let Err(e) = t.transition_to(
        coxagent_domain::Role::User,
        coxagent_domain::Status::Rejected,
    ) {
        return (StatusCode::CONFLICT, e.to_string()).into_response();
    }
    let home = format!("{}/{}", req.home_project_id, req.home_ticket_id);
    let msg = if action == "redirect" {
        format!("redirected to {home} (cross-project duplicate radar)")
    } else {
        format!("rejected as cross-project duplicate of {home}")
    };
    state.log_activity("USER", &msg, Some(req.dup_ticket_id.clone()));
    audit_push(
        &app.audit,
        by,
        format!(
            "dupe-radar {action} {}/{}",
            req.dup_project_id, req.dup_ticket_id
        ),
        200,
    )
    .await;
    match p.store.save(&state).await {
        Ok(()) => Json(json!({ "ok": true, "action": action, "ticket": req.dup_ticket_id }))
            .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_pool_matches_the_add_ticket_gate() {
        use coxagent_domain::Status;
        // Out on their side: shipped complete or rejected.
        for status in [
            Status::Done,
            Status::Documented,
            Status::Verified,
            Status::Rejected,
        ] {
            assert!(!radar_active(status), "{status:?} must be out of the pool");
        }
        // Live work stays in.
        for status in [
            Status::Pending,
            Status::Ready,
            Status::InProgress,
            Status::OnHold,
            Status::Open,
            Status::Fixed,
        ] {
            assert!(radar_active(status), "{status:?} stays in the pool");
        }
    }

    #[test]
    fn scope_snippet_trims_and_caps_but_never_truncates_mid_nothing() {
        assert_eq!(scope_snippet("  hello  "), "hello");
        let long = "x".repeat(500);
        let cut = scope_snippet(&long);
        assert_eq!(cut.chars().count(), 201, "200 chars + the ellipsis");
        assert!(cut.ends_with('…'));
        assert_eq!(scope_snippet(""), "");
    }

    #[test]
    fn a_workspace_doc_predating_the_radar_loads_with_an_empty_allowlist() {
        let doc: super::hub_docs::WorkspaceDoc =
            serde_json::from_str(r#"{"name":"Acme","tagline":"t"}"#).expect("legacy doc");
        assert!(doc.dupe_allowlist.is_empty(), "serde default, no migration");
        // …and a recorded verdict round-trips.
        let doc: super::hub_docs::WorkspaceDoc = serde_json::from_str(
            r#"{"name":"Acme","dupe_allowlist":[{"key":"p1/T-1|p2/T-2","by":"op","at":"2026-08-31T00:00:00Z"}]}"#,
        )
        .expect("doc with verdict");
        assert_eq!(doc.dupe_allowlist.len(), 1);
        assert_eq!(doc.dupe_allowlist[0].key, "p1/T-1|p2/T-2");
    }

    #[test]
    fn the_pair_payload_carries_ids_projects_scopes_and_score() {
        let e = DuplicatePair {
            home: coxagent_application::use_cases::duplicate_radar::RadarTicket {
                project_id: "p1".to_owned(),
                project_name: "Alpha".to_owned(),
                ticket_id: "T-9".to_owned(),
                title: "Fix flaky login".to_owned(),
                scope: "s".to_owned(),
                score: None,
            },
            dups: vec![
                coxagent_application::use_cases::duplicate_radar::RadarTicket {
                    project_id: "p2".to_owned(),
                    project_name: "Beta".to_owned(),
                    ticket_id: "T-2".to_owned(),
                    title: "fix flaky login".to_owned(),
                    scope: "s2".to_owned(),
                    score: Some(1.0),
                },
            ],
            normalized_title: "fix flaky login".to_owned(),
        };
        let v = pair_json(&e);
        assert_eq!(v["homeProjectId"], "p1");
        assert_eq!(v["homeProjectName"], "Alpha");
        assert_eq!(v["homeTicketId"], "T-9");
        assert_eq!(v["homeTicketTitle"], "Fix flaky login");
        assert_eq!(v["dupTitle"], "fix flaky login");
        let d = &v["duplicates"][0];
        assert_eq!(d["projectId"], "p2");
        assert_eq!(d["ticketId"], "T-2");
        assert_eq!(d["scope"], "s2");
        assert_eq!(d["score"], 1.0);
    }
}
