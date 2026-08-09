// Part of the server module split by concern — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Agent transcripts and live logs: the tail endpoint the dashboard's live
//! log view polls, and the per-run transcript listing/download.

use super::*;

pub(super) fn safe_under(root: &std::path::Path, rel: &str) -> Option<PathBuf> {
    // Reject absolute paths and any `..` component outright.
    let candidate = root.join(rel.trim_start_matches('/'));
    let root_c = root.canonicalize().ok()?;
    let cand_c = candidate.canonicalize().ok()?;
    cand_c.starts_with(&root_c).then_some(cand_c)
}

/// The transcript directory for a project: `<workspace>/logs/transcripts`.
pub(super) fn transcripts_dir(p: &ProjectHandle) -> PathBuf {
    p.config_path
        .parent()
        .unwrap_or(&p.config_path)
        .join("logs")
        .join("transcripts")
}

#[derive(serde::Deserialize)]
pub(super) struct AgentLogQuery {
    role: String,
    /// Optional operator (`account@host` or just the account) to view that
    /// specific worker's live log when several run the same role.
    #[serde(default)]
    worker: String,
}

/// Live agent log for a role: the streamed `<workspace>/logs/live/<role>.log`
/// (updated during the run), falling back to the newest completed transcript
/// for that role. Powers the full-screen live agent view.
pub(super) async fn agent_log_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<AgentLogQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Sanitize the role to a filename token (no path traversal).
    let role: String = q
        .role
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if role.is_empty() {
        return (StatusCode::BAD_REQUEST, "role required").into_response();
    }
    // A specific operator's log is `<role>__<account>.log`; without a worker (or
    // when that file is absent) fall back to the shared `<role>.log`.
    let account: String = q
        .worker
        .split('@')
        .next()
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let base = p.config_path.parent().unwrap_or(&p.config_path);
    let live_dir = base.join("logs").join("live");
    // The writer keys a live file `<role_key>__<ticket>__<operator>.log`
    // (role_key is lowercase snake, e.g. `dev_bug`; the ticket/operator
    // suffixes vary per run). The old read path guessed `<role>__<account>.log`
    // and never matched — so the fresh per-ticket log was orphaned and a stale
    // `<role>.log` was served instead (its results had no output preview).
    // Match by the role PREFIX, case-insensitively, and serve the newest.
    let want = role.to_ascii_lowercase().replace('-', "_");
    let want_op = account.to_ascii_lowercase();
    let live = std::fs::read_dir(&live_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            let stem = name.strip_suffix(".log")?;
            // `dev_bug`, `dev_bug__cox-b043`, `dev_bug__cox-b043__root` all match
            // role `dev_bug`; `dev_feature` must NOT match `dev` — require the
            // next char after the prefix to be `_` or end of stem.
            let after = stem.strip_prefix(&want)?;
            if !after.is_empty() && !after.starts_with('_') {
                return None;
            }
            // When an operator is named, prefer that operator's own log.
            let op_ok = want_op.is_empty() || after.contains(&want_op);
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((op_ok, mtime, e.path()))
        })
        // Operator-matched files win; then newest mtime.
        .max_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
        .map_or_else(|| live_dir.join(format!("{role}.log")), |(_, _, path)| path);
    // Local live file first (this machine's operators). If empty/absent, try
    // shared storage (MinIO) where remote operators mirror their live logs, so
    // the central hub can show an operator running on another machine.
    let local = std::fs::read_to_string(&live)
        .ok()
        .filter(|s| s.trim().len() > 20);
    let remote = if local.is_some() {
        None
    } else {
        let name = if account.is_empty() {
            format!("{role}.log")
        } else {
            format!("{role}__{account}.log")
        };
        app.storage
            .get(&format!("agentlogs/{pid}/{name}"))
            .await
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| s.trim().len() > 20)
    };
    let (body, live_flag) = if let Some(s) = local.or(remote) {
        (s, true)
    } else {
        // Fall back to the latest transcript for this role.
        let dir = transcripts_dir(&p);
        let latest = std::fs::read_dir(&dir).ok().and_then(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains(&role))
                .max_by_key(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                })
                .map(|e| e.path())
        });
        let text = latest
            .and_then(|pth| std::fs::read_to_string(pth).ok())
            .unwrap_or_default();
        (text, false)
    };
    Json(serde_json::json!({ "role": role, "live": live_flag, "log": body })).into_response()
}

/// List transcript files (name + size + modified), newest first.
pub(super) async fn list_transcripts(
    State(app): State<AppState>,
    Path(pid): Path<String>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let dir = transcripts_dir(&p);
    let mut items: Vec<serde_json::Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = entries.flatten().collect();
        files.sort_by_key(|e| {
            std::cmp::Reverse(
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            )
        });
        for e in files.into_iter().take(200) {
            let name = e.file_name().to_string_lossy().into_owned();
            let size = e.metadata().map_or(0, |m| m.len());
            items.push(serde_json::json!({ "name": name, "size": size }));
        }
    }
    Json(items).into_response()
}

/// Return one transcript's content. The name is validated to prevent traversal.
pub(super) async fn get_transcript(
    State(app): State<AppState>,
    Path((pid, name)): Path<(String, String)>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Reject any path separators / traversal — only a bare filename is allowed.
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let path = transcripts_dir(&p).join(&name);
    match std::fs::read_to_string(&path) {
        Ok(body) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such transcript").into_response(),
    }
}
