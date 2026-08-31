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

/// The part of a lowercased live-log filename that follows the role, or `None`
/// when it isn't this role's file. The writer names files
/// `<role_key>__<ticket>__<operator>.log` (role_key lowercase snake, e.g.
/// `dev_bug`); the reader must match the role PREFIX and no more — `dev_feature`
/// must not match role `dev`. The bug this pins: the old reader guessed
/// `<role>__<account>.log`, matched nothing, and served a stale `<role>.log`.
fn live_role_suffix<'a>(name: &'a str, role_want: &str) -> Option<&'a str> {
    let stem = name.strip_suffix(".log")?;
    let after = stem.strip_prefix(role_want)?;
    // Fields are joined by `__` (double underscore); role keys carry single
    // underscores inside them (`dev_bug`). So a match ends the role exactly at
    // stem-end or at a `__` boundary — `dev` must NOT swallow `dev_feature`,
    // whose leftover is a single-underscore `_feature…`.
    if after.is_empty() || after.starts_with("__") {
        Some(after)
    } else {
        None
    }
}

/// Resolve the newest live-log file for a role (optionally pinned to one
/// operator), shared by the snapshot endpoint and the SSE stream endpoint.
/// The writer keys a live file `<role_key>__<ticket>__<operator>.log` and every
/// streaming engine (opencode, copilot, claude, …) lands here via `append_live`,
/// so this resolver is engine-agnostic: whatever engine wrote the file, the tail
/// machinery below just follows it.
fn resolve_live_file(p: &ProjectHandle, role: &str, worker: &str) -> PathBuf {
    let base = p.config_path.parent().unwrap_or(&p.config_path);
    let live_dir = base.join("logs").join("live");
    let want = role.to_ascii_lowercase().replace('-', "_");
    let want_op = worker.to_ascii_lowercase();
    std::fs::read_dir(&live_dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            let after = live_role_suffix(&name, &want)?;
            let op_ok = want_op.is_empty() || after.contains(&want_op);
            let mtime = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((op_ok, mtime, e.path()))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)))
        .map_or_else(|| live_dir.join(format!("{role}.log")), |(_, _, path)| path)
}

/// Read a live log's current content plus its tail byte-offset, so a stream
/// client can snapshot then resume exactly where it left off without re-reading
/// the whole file on every push.
fn read_live_upto(path: &std::path::Path, after: u64) -> (String, u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (String::new(), 0);
    };
    let len = meta.len();
    let start = after.min(len);
    let mut tail = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        use std::io::{Read as _, Seek as _};
        let _ = f.seek(std::io::SeekFrom::Start(start));
        let _ = f.read_to_string(&mut tail);
        // If the file shrank (recreated/rotated) we couldn't have meant a
        // non-zero start into the new empty file — restart from the top and
        // let the client re-render.
        if after > len && start != 0 {
            tail.clear();
        }
    }
    (tail, len)
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
    let live = resolve_live_file(&p, &role, &account);
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

/// The `after` byte-offset the SSE stream uses to resume. If a client connects
/// without one we treat it as a fresh open: emit the full snapshot up front as
/// an `init` event carrying the file's tail offset, then follow new appends.
#[derive(serde::Deserialize, Debug)]
pub(super) struct AgentLogStreamQuery {
    role: String,
    /// Optional operator pin (comma-joined), forwarded to `resolve_live_file`.
    #[serde(default)]
    worker: String,
    /// Optional starting byte offset; 0 = snapshot from the top then stream.
    #[serde(default)]
    after: u64,
}

/// Real-time server-sent live log. The engine (opencode / copilot / claude)
/// writes lines to `<workspace>/logs/live/<role>...log` via `append_live`, local
/// on the machine, so the tail below is just a cheap local read every few
/// hundred ms — no engine-specific protocol needed. The hub then presses those
/// bytes down to the browser over SSE as `text/event-stream`, which replaces
/// the old 1.5 s polling with push.
///
/// Events:
///   * `init`   — `{ offset, role, live }` snapshot kick-off, sent on EVERY
///     fresh open (live:false when the log file is absent/empty — the
///     client's terminal empty state depends on it)
///   * `line`   — one or more new bytes, JSON `{ text }` batched per poll
///   * `done`   — file gone / ended, client should close
///   * comment  — `: ping` heartbeat every ~15 s to keep proxies alive
pub(super) async fn agent_log_stream_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    axum::extract::Query(q): axum::extract::Query<AgentLogStreamQuery>,
) -> axum::response::Response {
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    let role: String = q
        .role
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if role.is_empty() {
        return (StatusCode::BAD_REQUEST, "role required").into_response();
    }
    let account: String = q
        .worker
        .split('@')
        .next()
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();

    let live = resolve_live_file(&p, &role, &account);
    let (snapshot, offset) = read_live_upto(&live, 0);
    let live_flag = snapshot.trim().len() >= 20 && live.exists();

    let mut after = q.after.min(offset);
    // CXA-B128: a fresh open (after == 0) ALWAYS gets the `init` kick-off —
    // even when the live log does not exist yet. Withholding init used to
    // leave the client's work-log panel on its indefinite "loading…"
    // placeholder forever: no init → no renderAgentLog → no empty state, and
    // a healthy connection fires no error either. The empty-file init carries
    // live:false so the client paints its terminal "hasn't run yet" state.
    let fresh = q.after == 0;
    let mut pending = if fresh {
        // Fresh open: send the snapshot immediately.
        after = offset;
        snapshot
    } else {
        // Resume: only send what we haven't delivered yet (fetch now).
        let (tail, _) = read_live_upto(&live, q.after);
        tail
    };

    // Tail loop → channel → SSE stream. The engine writes locally so each poll
    // is a cheap `stat` + read; no engine-specific protocol involved.
    let offset_init = offset;
    let (tx, rx) = tokio::sync::mpsc::channel::<
        Result<axum::response::sse::Event, std::convert::Infallible>,
    >(64);
    let send_init = fresh || !pending.is_empty();
    let has_snapshot = !pending.is_empty();
    std::thread::spawn(move || {
        let send = |tx: &tokio::sync::mpsc::Sender<_>, e: axum::response::sse::Event| {
            tx.blocking_send(Ok(e)).is_err()
        };
        if send_init {
            if send(
                &tx,
                axum::response::sse::Event::default().event("init").data(
                    serde_json::json!({
                        "offset": offset_init,
                        "role": role,
                        "live": live_flag,
                    })
                    .to_string(),
                ),
            ) {
                return;
            }
            // Only a non-empty snapshot becomes a `line`; the init event above
            // already told the client there is nothing to show yet.
            if has_snapshot
                && send(
                    &tx,
                    axum::response::sse::Event::default()
                        .event("line")
                        .data(serde_json::json!({ "text": std::mem::take(&mut pending) }).to_string()),
                )
            {
                return;
            }
        }
        loop {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let now = std::fs::metadata(&live).map_or(0, |m| m.len());
            if now < after {
                // File truncated (rotated) — restart from the top.
                after = 0;
            }
            if now > after {
                let (text, o) = read_live_upto(&live, after);
                after = o;
                if !text.is_empty()
                    && send(
                        &tx,
                        axum::response::sse::Event::default()
                            .event("line")
                            .data(serde_json::json!({ "text": text }).to_string()),
                    )
                {
                    break;
                }
            }
        }
    });

    axum::response::sse::Sse::new(tokio_stream::wrappers::ReceiverStream::new(rx))
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(std::time::Duration::from_secs(15))
                .text(": ping"),
        )
        .into_response()
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

#[cfg(test)]
mod live_log_tests {
    use super::live_role_suffix;

    // Names the writer actually produces: `<role_key>__<ticket>__<operator>.log`.
    #[test]
    fn matches_the_writers_per_ticket_name() {
        assert_eq!(
            live_role_suffix("dev_bug__cox-b043__root.log", "dev_bug"),
            Some("__cox-b043__root")
        );
        // Bare role file (the shared fallback) still matches.
        assert_eq!(live_role_suffix("dev_bug.log", "dev_bug"), Some(""));
    }

    // The prefix must not swallow a longer role — the bug a naive `contains`
    // would have: role `dev` picking up `dev_feature`'s live log.
    #[test]
    fn role_prefix_does_not_bleed_into_a_longer_role() {
        assert_eq!(
            live_role_suffix("dev_feature__cox-f01__root.log", "dev"),
            None
        );
        assert_eq!(live_role_suffix("developer.log", "dev"), None);
    }

    // The operator suffix is what the caller filters on to prefer one worker.
    #[test]
    fn operator_is_findable_in_the_suffix() {
        let after = live_role_suffix("sa__cox-f10__alice.log", "sa").unwrap();
        assert!(after.contains("alice"));
        assert!(!after.contains("bob"));
    }

    #[test]
    fn a_non_log_or_foreign_file_is_rejected() {
        assert_eq!(live_role_suffix("dev_bug.txt", "dev_bug"), None);
        assert_eq!(live_role_suffix("qa__cox-b01.log", "sa"), None);
    }
}
