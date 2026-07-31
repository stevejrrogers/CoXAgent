// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! Long-running hub chores: watchdogs, backups, the bus bridge.

use super::*;

/// Post one COX budget notice into a project's #agents and log the same line to
/// its activity feed. Both the warning and the hard stop below report this way.
pub(super) async fn post_budget_notice(p: &ProjectHandle, msg: &str, activity: &str) {
    let _ = coxagent_application::ports::outbound::mutate_state(p.store.as_ref(), |s| {
        s.post_chat_in(
            "COX",
            msg,
            coxagent_application::state::AGENTS_CHANNEL,
            Vec::new(),
        );
        s.log_activity("COX", activity, None);
        Ok(())
    })
    .await;
}

/// Bridge the in-process chat broadcast onto Redis pub/sub (`cox:events`), so
/// every hub instance sees every event — the piece that makes the gateway
/// horizontally scalable. Loop safety: outbound frames carry this instance's
/// origin id (dropped by our own subscriber), and payloads just received from
/// the bus are remembered briefly so re-broadcasting them locally doesn't
/// publish an echo back.
pub(super) async fn redis_bus_bridge(app: AppState, url: String) {
    use coxagent_contracts::BusEnvelope;
    let origin = format!("hub-{}", std::process::id());
    let recent: Arc<std::sync::Mutex<std::collections::VecDeque<String>>> =
        Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let remember = |recent: &std::sync::Mutex<std::collections::VecDeque<String>>, s: &str| {
        if let Ok(mut q) = recent.lock() {
            q.push_back(s.to_owned());
            while q.len() > 256 {
                q.pop_front();
            }
        }
    };
    let seen = |recent: &std::sync::Mutex<std::collections::VecDeque<String>>, s: &str| {
        recent.lock().is_ok_and(|q| q.iter().any(|x| x == s))
    };
    // Outbound: local broadcast → Redis.
    {
        let url = url.clone();
        let origin = origin.clone();
        let recent = Arc::clone(&recent);
        let tx = app.syschat.tx.clone();
        tokio::spawn(async move {
            loop {
                let Ok(client) = redis::Client::open(url.as_str()) else {
                    return;
                };
                let Ok(mut conn) = client.get_multiplexed_async_connection().await else {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    continue;
                };
                let mut rx = tx.subscribe();
                while let Ok(payload) = rx.recv().await {
                    if seen(&recent, &payload) {
                        continue; // just came FROM the bus — don't echo it back
                    }
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else {
                        continue;
                    };
                    let env = BusEnvelope::new(&origin, "syschat", v);
                    if let Ok(frame) = serde_json::to_string(&env) {
                        let _: Result<(), _> =
                            redis::AsyncCommands::publish(&mut conn, "cox:events", frame).await;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }
    // Inbound: Redis → local broadcast.
    loop {
        let Ok(client) = redis::Client::open(url.as_str()) else {
            return;
        };
        let Ok(mut pubsub) = client.get_async_pubsub().await else {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        };
        if pubsub.subscribe("cox:events").await.is_err() {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            continue;
        }
        let mut stream = pubsub.on_message();
        while let Some(msg) = futures_util::StreamExt::next(&mut stream).await {
            let frame: String = match msg.get_payload() {
                Ok(f) => f,
                Err(_) => continue,
            };
            let Ok(env) = serde_json::from_str::<BusEnvelope>(&frame) else {
                continue;
            };
            if env.origin == origin || env.v != coxagent_contracts::CONTRACT_VERSION {
                continue;
            }
            if let Ok(payload) = serde_json::to_string(&env.payload) {
                remember(&recent, &payload);
                let _ = app.syschat.tx.send(payload);
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Disaster-recovery floor for the hub-level app_kv documents: once a day,
/// snapshot workspace + spaces + system chat as dated JSON under
/// `<hub_dir>/backups/YYYY-MM-DD/`, pruning snapshots older than 14 days.
/// Restore = copy a snapshot back over the store (documented in DEPLOYMENT.md).
pub(super) async fn nightly_backup(app: AppState, dir: PathBuf) {
    loop {
        let day = coxagent_application::state::now_rfc3339()[..10].to_owned();
        let dest = dir.join(&day);
        let done = dest.join("spaces.json").exists();
        if !done {
            let _ = std::fs::create_dir_all(&dest);
            let ws = app.workspace.inner.lock().await.clone();
            let sp = app.spaces.inner.lock().await.clone();
            let chat = app.syschat.inner.lock().await.clone();
            let dump = |name: &str, v: serde_json::Result<String>| {
                if let Ok(text) = v {
                    let _ = std::fs::write(dest.join(name), text);
                }
            };
            dump("workspace.json", serde_json::to_string_pretty(&ws));
            dump("spaces.json", serde_json::to_string_pretty(&sp));
            dump("system_chat.json", serde_json::to_string_pretty(&chat));
            tracing::info!("nightly backup written to {}", dest.display());
            // Prune snapshots older than 14 days (lexicographic = chronologic).
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let mut days: Vec<String> = entries
                    .filter_map(Result::ok)
                    .filter(|e| e.path().is_dir())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect();
                days.sort();
                while days.len() > 14 {
                    let old = days.remove(0);
                    let _ = std::fs::remove_dir_all(dir.join(old));
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
