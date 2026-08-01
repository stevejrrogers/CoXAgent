// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Live wires: the terminal socket and the event stream.

use super::*;

/// Embedded terminal (IDE-style): a real PTY in the project's codebase dir,
/// bridged over this WebSocket. Arbitrary shell = full host access, so the
/// gate is hard: Admin/Super only, and every session start is audited.
/// Protocol: client sends JSON text frames {"input": "..."} and
/// {"resize": {"cols": N, "rows": N}}; server sends raw output as binary.
pub(super) async fn terminal_ws_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(resp) = role_guard(true) {
        return resp;
    }
    if !origin_ok(&headers) {
        return (StatusCode::FORBIDDEN, "cross-origin websocket rejected").into_response();
    }
    // Every working member gets a shell (like opening the OS terminal) — only
    // the read-only legacy Viewer is excluded. Each session start is audited.
    let user = match &app.auth {
        Some(auth) => match resolve_principal(auth, &headers).await {
            Some(u) if u.role.can_write() => u.username,
            Some(_) => {
                return (StatusCode::FORBIDDEN, "read-only role").into_response();
            }
            None => return (StatusCode::UNAUTHORIZED, "unauthenticated").into_response(),
        },
        None => "user".to_owned(),
    };
    let Some(p) = app.project(&pid).await else {
        return not_found();
    };
    // Execution-plane guard: a hardened control-plane deployment (K8s gateway)
    // sets COXAGENT_NO_INLINE_EXEC=1 — no shells in this process, ever.
    if std::env::var("COXAGENT_NO_INLINE_EXEC").is_ok_and(|v| v == "1") {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "terminals are disabled on the control plane — connect via a runner",
        )
            .into_response();
    }
    app.audit
        .record(coxagent_application::ports::outbound::AuditRecord {
            at: coxagent_application::state::now_rfc3339(),
            user: user.clone(),
            action: format!("TERMINAL open {pid}"),
            status: 101,
        })
        .await;
    let work_dir = p.work_dir.clone();
    ws.max_message_size(256 * 1024)
        .on_upgrade(move |socket| terminal_socket(socket, work_dir, user, pid))
}

pub(super) async fn terminal_socket(
    mut socket: WebSocket,
    work_dir: PathBuf,
    user: String,
    pid: String,
) {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    let pty = native_pty_system();
    let Ok(pair) = pty.openpty(PtySize {
        rows: 30,
        cols: 100,
        pixel_width: 0,
        pixel_height: 0,
    }) else {
        let _ = socket
            .send(Message::Text("\r\n[pty unavailable]\r\n".into()))
            .await;
        return;
    };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l");
    cmd.env("TERM", "xterm-256color");
    if work_dir.is_dir() {
        cmd.cwd(&work_dir);
    }
    let Ok(mut child) = pair.slave.spawn_command(cmd) else {
        let _ = socket
            .send(Message::Text("\r\n[shell spawn failed]\r\n".into()))
            .await;
        return;
    };
    drop(pair.slave);
    let Ok(mut reader) = pair.master.try_clone_reader() else {
        return;
    };
    let Ok(mut writer) = pair.master.take_writer() else {
        return;
    };
    tracing::info!("terminal session opened by {user} on {pid}");
    // Blocking PTY reads → channel → WS.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let master = pair.master;
    loop {
        tokio::select! {
            out = rx.recv() => {
                if let Some(bytes) = out {
                    if socket.send(Message::Binary(bytes)).await.is_err() { break; }
                } else { // shell exited
                    let _ = socket.send(Message::Text("\r\n[session ended]\r\n".into())).await;
                    break;
                }
            }
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                if let Message::Text(t) = msg {
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else { continue };
                    if let Some(input) = v.get("input").and_then(|x| x.as_str()) {
                        use std::io::Write;
                        if writer.write_all(input.as_bytes()).is_err() { break; }
                        let _ = writer.flush();
                    } else if let Some(r) = v.get("resize") {
                        let cols = r.get("cols").and_then(serde_json::Value::as_u64).unwrap_or(100);
                        let rows = r.get("rows").and_then(serde_json::Value::as_u64).unwrap_or(30);
                        #[allow(clippy::cast_possible_truncation)]
                        let _ = master.resize(PtySize {
                            rows: rows.clamp(4, 300) as u16,
                            cols: cols.clamp(20, 500) as u16,
                            pixel_width: 0, pixel_height: 0,
                        });
                    }
                }
            }
        }
    }
    let _ = child.kill();
    tracing::info!("terminal session closed ({user} on {pid})");
}

pub(super) async fn events_ep(
    State(app): State<AppState>,
    Path(pid): Path<String>,
    headers: axum::http::HeaderMap,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let handle = app.project(&pid).await;
    // Identify the viewer so distinct-user counts (not tab counts) are reported.
    let user = match &app.auth {
        Some(auth) => resolve_principal(auth, &headers)
            .await
            .map_or_else(|| "anonymous".to_owned(), |u| u.username),
        None => "local".to_owned(),
    };
    let guard = ViewerGuard::new(&app.viewers, user);
    let stream = IntervalStream::new(tokio::time::interval(STREAM_INTERVAL)).then(move |_| {
        // `guard` is owned by this closure, so the count drops when the stream ends.
        let count = guard.count();
        let online = guard.online_users();
        let handle = handle.clone();
        async move {
            let payload = match handle {
                Some(p) => serde_json::json!({
                    "state": p.store.load().await.ok().as_ref().map(lite_state_value),
                    "runner": p.runner.snapshot(),
                    "viewers": count,
                    "online": online,
                }),
                None => serde_json::json!({ "error": "no such project" }),
            };
            Ok(Event::default().data(payload.to_string()))
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
