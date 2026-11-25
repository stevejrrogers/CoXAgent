// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Per-user LLM keys (BYOK, CXA-F410): a signed-in user can voluntarily add
//! their own OpenAI-compatible provider key so their work stops competing for
//! the project pool's parallel slots.
//!
//! UX rules this module enforces server-side:
//! - **Probe-first**: a key is saved only after a live 1-token completion
//!   against the given base URL succeeds; failures come back as a
//!   human-readable reason (wrong key / wrong URL / throttled / unreachable).
//! - **Masked forever**: the raw key is write-only. Every read answers the
//!   provider label, base URL, model and the key's last four characters.
//! - **Self-service only**: each user sees and manages exclusively their own
//!   keys; there is no admin listing of raw keys.

use super::*;

/// One stored user key. `key` never leaves the server — see [`UserKeyView`].
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct UserKey {
    pub(super) id: String,
    /// Display label, defaulting to the base URL's host.
    #[serde(default)]
    pub(super) label: String,
    pub(super) base_url: String,
    /// Model to route this user's requests to (also the probe target).
    pub(super) model: String,
    pub(super) key: String,
    #[serde(default)]
    pub(super) created_at: String,
    /// Result of the most recent probe: "ok" or the failure reason.
    #[serde(default)]
    pub(super) probe: String,
    #[serde(default)]
    pub(super) probe_at: String,
}

/// The masked, client-facing shape of a [`UserKey`].
#[derive(serde::Serialize)]
struct UserKeyView {
    id: String,
    label: String,
    base_url: String,
    model: String,
    key_last4: String,
    created_at: String,
    probe: String,
    probe_at: String,
}

impl UserKey {
    fn view(&self) -> UserKeyView {
        let tail: String = self
            .key
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        UserKeyView {
            id: self.id.clone(),
            label: self.label.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            key_last4: tail,
            created_at: self.created_at.clone(),
            probe: self.probe.clone(),
            probe_at: self.probe_at.clone(),
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct UserKeysDoc {
    /// username → that user's keys.
    pub(super) keys: std::collections::HashMap<String, Vec<UserKey>>,
}

/// Key store: shared KV (`app_kv` key `user-llm-keys`) when configured, else a
/// local `user-llm-keys.json` under the hub dir — same shape as [`Pf`].
#[derive(Clone)]
pub(super) struct Uk {
    pub(super) inner: Arc<tokio::sync::Mutex<UserKeysDoc>>,
    pub(super) path: PathBuf,
    pub(super) store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
}

impl Uk {
    pub(super) async fn load(
        dir: &std::path::Path,
        store: Option<Arc<dyn coxagent_application::ports::outbound::KvDocPort>>,
    ) -> Self {
        let path = dir.join("user-llm-keys.json");
        let text = if let Some(s) = &store {
            s.load("user-llm-keys").await.ok().flatten()
        } else {
            std::fs::read_to_string(&path).ok()
        };
        let inner = text
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(inner)),
            path,
            store,
        }
    }

    pub(super) async fn save(&self) {
        let json = { serde_json::to_string(&*self.inner.lock().await).unwrap_or_default() };
        if let Some(s) = &self.store {
            if let Err(e) = s.save("user-llm-keys", &json).await {
                tracing::warn!("user-llm-keys save failed: {e}");
            }
        } else if let Err(e) = std::fs::write(&self.path, json) {
            tracing::warn!("user-llm-keys save failed: {e}");
        }
    }
}

/// Live probe: one 1-token completion against `base_url`. `Ok(())` means the
/// provider answered a completion for this key+model. The error string is the
/// user-facing reason — keep it actionable, never dump raw bodies.
async fn probe_key(base_url: &str, key: &str, model: &str) -> Result<(), String> {
    let base = base_url.trim_end_matches('/');
    // Accept both "…/v1" bases and bare origins.
    let url = if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    };
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("probe client: {e}"))?;
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
    });
    let resp = client
        .post(&url)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "provider did not answer within 30s — check the base URL".to_owned()
            } else {
                "provider unreachable — check the base URL".to_owned()
            }
        })?;
    match resp.status().as_u16() {
        200 => Ok(()),
        401 | 403 => Err("the provider rejected this key (401/403) — check the key".to_owned()),
        404 => Err("no chat-completions endpoint at this base URL (404) — check the URL".to_owned()),
        429 => Err("key works but is currently rate-limited (429) — saved checks may pass later; try again".to_owned()),
        s if s >= 500 => Err(format!("provider error (HTTP {s}) — try again shortly")),
        s => Err(format!("unexpected provider answer (HTTP {s})")),
    }
}

#[derive(serde::Deserialize)]
pub(super) struct AddKeyBody {
    #[serde(default)]
    label: String,
    base_url: String,
    api_key: String,
    model: String,
}

/// `GET /api/me/llm-keys` — the caller's keys, masked.
pub(super) async fn my_keys_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let me = resolve_username(&app, &headers).await;
    let doc = app.user_keys.inner.lock().await;
    let views: Vec<UserKeyView> = doc
        .keys
        .get(&me)
        .map(|v| v.iter().map(UserKey::view).collect())
        .unwrap_or_default();
    Json(serde_json::json!({ "keys": views })).into_response()
}

/// `POST /api/me/llm-keys` — probe-first add. A key that fails the live probe
/// is NOT stored; the reason comes back as `{"error": …}` with 422.
pub(super) async fn add_key_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AddKeyBody>,
) -> axum::response::Response {
    let me = resolve_username(&app, &headers).await;
    let base_url = body.base_url.trim().to_owned();
    let api_key = body.api_key.trim().to_owned();
    let model = body.model.trim().to_owned();
    if base_url.is_empty() || api_key.is_empty() || model.is_empty() {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "base_url, api_key and model are all required"})),
        )
            .into_response();
    }
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "base_url must start with http:// or https://"})),
        )
            .into_response();
    }
    if let Err(reason) = probe_key(&base_url, &api_key, &model).await {
        return (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": reason})),
        )
            .into_response();
    }
    let now = now_rfc3339();
    let label = if body.label.trim().is_empty() {
        url_host(&base_url)
    } else {
        body.label.trim().to_owned()
    };
    let entry = UserKey {
        id: format!("k{}", uuid::Uuid::new_v4().simple()),
        label,
        base_url,
        model,
        key: api_key,
        created_at: now.clone(),
        probe: "ok".to_owned(),
        probe_at: now,
    };
    let view = entry.view();
    {
        let mut doc = app.user_keys.inner.lock().await;
        doc.keys.entry(me.clone()).or_default().push(entry);
    }
    app.user_keys.save().await;
    audit_push(&app.audit, &me, format!("added LLM key {}", view.id), 200).await;
    Json(serde_json::json!({ "key": view })).into_response()
}

/// `POST /api/me/llm-keys/:id/retest` — re-run the live probe on a stored key
/// and record the outcome on the entry.
pub(super) async fn retest_key_ep(
    State(app): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let me = resolve_username(&app, &headers).await;
    let target = {
        let doc = app.user_keys.inner.lock().await;
        doc.keys
            .get(&me)
            .and_then(|v| v.iter().find(|k| k.id == id))
            .map(|k| (k.base_url.clone(), k.key.clone(), k.model.clone()))
    };
    let Some((base_url, key, model)) = target else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let outcome = probe_key(&base_url, &key, &model).await;
    let (probe, status) = match &outcome {
        Ok(()) => ("ok".to_owned(), axum::http::StatusCode::OK),
        Err(reason) => (reason.clone(), axum::http::StatusCode::UNPROCESSABLE_ENTITY),
    };
    let view = {
        let mut doc = app.user_keys.inner.lock().await;
        let entry = doc
            .keys
            .get_mut(&me)
            .and_then(|v| v.iter_mut().find(|k| k.id == id));
        entry.map(|k| {
            k.probe.clone_from(&probe);
            k.probe_at = now_rfc3339();
            k.view()
        })
    };
    app.user_keys.save().await;
    match view {
        Some(v) => (status, Json(serde_json::json!({ "key": v }))).into_response(),
        None => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

/// `DELETE /api/me/llm-keys/:id`.
pub(super) async fn delete_key_ep(
    State(app): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let me = resolve_username(&app, &headers).await;
    let removed = {
        let mut doc = app.user_keys.inner.lock().await;
        match doc.keys.get_mut(&me) {
            Some(v) => {
                let before = v.len();
                v.retain(|k| k.id != id);
                before != v.len()
            }
            None => false,
        }
    };
    if !removed {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    }
    app.user_keys.save().await;
    audit_push(&app.audit, &me, format!("deleted LLM key {id}"), 204).await;
    axum::http::StatusCode::NO_CONTENT.into_response()
}

fn url_host(u: &str) -> String {
    u.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("provider")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_masks_the_key_to_last_four() {
        let k = UserKey {
            id: "k1".into(),
            label: "p".into(),
            base_url: "https://x".into(),
            model: "m".into(),
            key: "sk-secret-abcdef".into(),
            created_at: String::new(),
            probe: "ok".into(),
            probe_at: String::new(),
        };
        let v = k.view();
        assert_eq!(v.key_last4, "cdef");
        // The serialized view must not contain the raw key anywhere.
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("sk-secret"));
    }

    #[test]
    fn short_keys_mask_without_panicking() {
        let k = UserKey {
            id: "k1".into(),
            label: String::new(),
            base_url: String::new(),
            model: String::new(),
            key: "ab".into(),
            created_at: String::new(),
            probe: String::new(),
            probe_at: String::new(),
        };
        assert_eq!(k.view().key_last4, "ab");
    }

    #[test]
    fn host_label_fallback() {
        assert_eq!(
            url_host("https://console.example.app/v1"),
            "console.example.app"
        );
        assert_eq!(url_host("nonsense"), "nonsense");
    }
}
