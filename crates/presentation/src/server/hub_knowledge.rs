// CXA-F017 hub knowledge dashboard surface.
#![allow(clippy::wildcard_imports)]
use super::{principal_name as require_principal, AppState};
use axum::{response::IntoResponse as _, Json};
use std::{collections::BTreeMap, sync::Arc};

const CURATION_KEY: &str = "hub_knowledge";

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct CurationDoc {
    hidden: BTreeMap<String, bool>,
}

static MEM_CACHE: std::sync::OnceLock<Arc<std::sync::Mutex<CurationDoc>>> =
    std::sync::OnceLock::new();
fn mem_cache() -> Arc<std::sync::Mutex<CurationDoc>> {
    MEM_CACHE
        .get_or_init(|| Arc::new(std::sync::Mutex::new(CurationDoc::default())))
        .clone()
}

async fn load_doc(app: &AppState) -> CurationDoc {
    if let Some(kv) = &app.hub_knowledge_kv {
        if let Ok(Some(txt)) = kv.load(CURATION_KEY).await {
            if let Ok(d) = serde_json::from_str(&txt) {
                return d;
            }
        }
        return CurationDoc::default();
    }
    mem_cache().lock().map(|g| g.clone()).unwrap_or_default()
}

async fn save_doc(app: &AppState, doc: &CurationDoc) {
    if let Some(kv) = &app.hub_knowledge_kv {
        let txt = serde_json::to_string(doc).unwrap_or_default();
        let _ = kv.save(CURATION_KEY, &txt).await;
        return;
    }
    if let Ok(mut g) = mem_cache().lock() {
        *g = doc.clone();
    }
}

fn push_row(rows: &mut Vec<serde_json::Value>, e: &coxagent_domain::KnowledgeEntry, hidden: bool) {
    rows.push(serde_json ::json !({
        "entry_id": e .entry_id ,
        "project_id": e.project_id,
        "kind": e.kind.as_str(),
        "title": e.title,
        "curated":{"hidden_in_briefs":hidden},
    }));
}

async fn index_rows(app: &AppState) -> Vec<serde_json::Value> {
    let doc = load_doc(app).await;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let order = app.order.read().await.clone();
    let map = app.projects.read().await.clone();
    for pid in &order {
        let Some(p) = map.get(pid) else { continue };
        match p.store.load().await {
            Ok(st) => {
                for e in coxagent_application::hub_knowledge::entries_from_project(
                    &p.id,
                    &st.docs,
                    &st.tickets,
                ) {
                    let hidden = doc.hidden.contains_key(&e.entry_id);
                    push_row(&mut rows, &e, hidden);
                }
            }
            Err(_) => {}
        }
    }
    if let Ok(text) = std::fs::read_to_string(coxagent_application::prompts::hub_lessons_path()) {
        for e in coxagent_application::hub_knowledge::lessons_from_text(&text) {
            let hidden = doc.hidden.contains_key(&e.entry_id);
            push_row(&mut rows, &e, hidden);
        }
    }
    rows.sort_by_key(|r| r["entry_id"].as_str().unwrap_or("").to_owned());
    rows
}

/// GET /api/hub-knowledge/index?q=<terms> — every sibling's sharable knowledge
/// plus hub lessons, each row flagged with its persisted curation state.
pub(super) async fn hub_knowledge_index_ep(
    axum::extract::State(app): axum::extract::State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if app.auth.is_some() {
        if require_principal(&app, &headers).await.is_none() {
            return (axum::http::StatusCode::UNAUTHORIZED, "sign-in required").into_response();
        }
    }
    let q = params.get("q").cloned().unwrap_or_default();
    let rows = index_rows(&app).await;
    let lower = q.to_lowercase();
    let filtered: Vec<_> = rows
        .into_iter()
        .filter(|r| {
            q.is_empty()
                || serde_json::to_string(r).is_ok_and(|s| s.to_lowercase().contains(&lower))
        })
        .collect();
    let project_count = app.order.read().await.len();
    Json(serde_json::json !({"project_count":project_count,"entries":filtered})).into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct CurateReq {
    entry_id: String,
    hidden: bool,
}

/// POST /api/hub-knowledge/curate — hide/unshare an entry for brief surfaces.
pub(super) async fn hub_knowledge_curate_ep(
    axum::extract::State(app): axum::extract::State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<CurateReq>,
) -> axum::response::Response {
    if req.entry_id.trim().is_empty() {
        return (axum::http::StatusCode::BAD_REQUEST, "empty entry_id").into_response();
    }
    if app.auth.is_some() {
        if require_principal(&app, &headers).await.is_none() {
            return (axum::http::StatusCode::UNAUTHORIZED, "sign-in required").into_response();
        }
    }
    let mut doc = load_doc(&app).await;
    if req.hidden {
        doc.hidden.insert(req.entry_id.clone(), true);
    } else {
        doc.hidden.remove(&req.entry_id);
    }
    save_doc(&app, &doc).await;
    let hidden_in_briefs = req.hidden;
    let entry_id = req.entry_id;
    Json(serde_json::json !({"ok":true,"entry_id":entry_id,"hidden_in_briefs":hidden_in_briefs}))
        .into_response()
}
