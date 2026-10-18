// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The archive read-back (CXA-F274): serve cold-store tickets to the board UI
//! and back the detail endpoint's hot-miss fallback, so eviction (F273) never
//! makes work invisible. Read-only over [`ArchiveStorePort`]; the write path
//! is F272/F273's.

use super::*;
use coxagent_application::archive_read;
use coxagent_domain::ticket::Ticket;

/// `GET /api/projects/:pid/tickets/archive?limit=50&offset=0` — one page of
/// the project's archived tickets (id-descending) plus the archive's TOTAL
/// across pages. The board list shape (`design` stripped, same rule as the
/// 1 Hz snapshot), each row stamped `"archived":true`. Unknown project → 404;
/// unauthorized requests are refused by `auth_mw` like every sibling; a store
/// backend error → 500; no store configured or an empty archive → 200 with
/// `{"tickets":[],"total":0}` — byte-for-byte the shape an empty hot board
/// already renders, so the empty-archive UI is indistinguishable from today.
pub(super) async fn ticket_archive_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ArchiveQuery>,
) -> axum::response::Response {
    // Existence check only — the handle carries nothing this endpoint reads.
    if app.project(&pid).await.is_none() {
        return not_found();
    }
    // No cold store wired (the default local boot): the archive is empty by
    // definition, never an error — archival simply has not happened.
    let Some(store) = app.archive_store.as_deref() else {
        return Json(serde_json::json!({ "tickets": [], "total": 0 })).into_response();
    };
    let (offset, limit) = archive_read::clamp_window(q.offset, q.limit);
    match archive_read::page(store, &pid, offset, limit).await {
        Ok((tickets, total)) => Json(serde_json::json!({
            "tickets": tickets.iter().map(archive_lite).collect::<Vec<_>>(),
            "total": total,
        }))
        .into_response(),
        Err(e) => internal_error(&e.to_string()),
    }
}

/// One archived ticket as the board list shape: the aggregate serialized with
/// its heavy `design` specs stripped (they load on demand through the detail
/// fallback, exactly like hot tickets) and the `"archived"` stamp added.
fn archive_lite(t: &Ticket) -> serde_json::Value {
    let mut v = serde_json::to_value(t).unwrap_or_default();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("design");
        obj.insert("archived".into(), serde_json::json!(true));
    }
    v
}

/// Which record answers a detail lookup (CXA-F274): the hot state first — the
/// live board is the source of truth and a hot hit never consults the cold
/// store — then the archive on a miss. The bool stamps `"archived"` on the
/// payload; `Ok(None)` is an honest miss on BOTH stores (the caller answers
/// 404 exactly as before), and a backend error is a 500, never a silent 404 —
/// archival must not look like data loss.
pub(super) async fn resolve_detail(
    hot: &[Ticket],
    app: &AppState,
    pid: &str,
    id: &str,
) -> Result<Option<(Ticket, bool)>, coxagent_application::error::PortError> {
    if let Some(t) = hot.iter().find(|t| t.id().as_str() == id) {
        return Ok(Some((t.clone(), false)));
    }
    let Some(store) = app.archive_store.as_deref() else {
        return Ok(None);
    };
    Ok(store.get(pid, id).await?.map(|t| (t, true)))
}
