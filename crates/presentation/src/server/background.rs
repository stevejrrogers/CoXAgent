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

// --- Loop-liveness watchdog (CXA-F259) --------------------------------------

use coxagent_application::ports::outbound::WorkerEntry;

/// How often the liveness sweep runs. One minute: tight enough to name a
/// stall "within minutes", slow enough to be free.
const LIVENESS_SWEEP_SECS: u64 = 60;

/// Resolved watchdog knobs for one project, read from its `coxagent.json`
/// (`workflow.stall_*`) plus the budget caps the non-stall exclusion needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct LivenessKnobs {
    enabled: bool,
    timeout_secs: u64,
    escalate_secs: u64,
    budget_lifetime_usd: Option<f64>,
    budget_daily_usd: Option<f64>,
}

impl Default for LivenessKnobs {
    fn default() -> Self {
        Self {
            enabled: true,
            timeout_secs: 3600,
            escalate_secs: 14_400,
            budget_lifetime_usd: None,
            budget_daily_usd: None,
        }
    }
}

/// The e2e horizon (`COXAGENT_LIVENESS_HORIZON_SECS`): shortens the effective
/// threshold so a test can raise a real stall in seconds instead of an hour.
fn liveness_horizon_env() -> Option<i64> {
    std::env::var("COXAGENT_LIVENESS_HORIZON_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|s| *s > 0)
}

/// Sweep cadence: the horizon tightens it so an e2e run sees the chip quickly.
fn liveness_sweep_secs(horizon: Option<i64>) -> u64 {
    horizon.map_or(LIVENESS_SWEEP_SECS, |h| {
        u64::try_from(h.clamp(2, 60)).unwrap_or(LIVENESS_SWEEP_SECS)
    })
}

/// Knobs for one project. An unreadable/unparseable config falls back to the
/// defaults rather than going dark: this is a read-only safety net, and a
/// broken config is already named loudly at boot and by the readiness
/// surface — silencing the watchdog too would hide the one alert that says
/// "everything else is quietly dead".
fn liveness_knobs(p: &ProjectHandle, horizon: Option<i64>) -> LivenessKnobs {
    let mut knobs = LivenessKnobs::default();
    if let Ok(text) = std::fs::read_to_string(&p.config_path) {
        if let Ok(cfg) = coxagent_application::config_parse::parse_config(&text) {
            knobs.enabled = cfg.workflow.stall_watchdog;
            knobs.timeout_secs = coxagent_application::liveness::stall_threshold(&cfg.workflow);
            knobs.escalate_secs = coxagent_application::liveness::stall_escalation(&cfg.workflow);
            knobs.budget_lifetime_usd = cfg.workflow.budget_usd;
            knobs.budget_daily_usd = cfg.policy.daily_budget_usd;
        }
    }
    if let Some(h) = horizon {
        knobs.timeout_secs = knobs.timeout_secs.min(u64::try_from(h).unwrap_or(u64::MAX));
    }
    knobs
}

/// The freshest registry entry, ordered by parsed heartbeat INSTANT — never
/// by string: store-RPC heartbeats may carry non-UTC offsets whose string
/// order lies (`…T11:00:00+02:00` sorts after `…T10:00:00Z` but is OLDER).
/// Unparseable stamps sort oldest; an empty registry means the worker
/// process is gone (the store prunes stale heartbeats — absence IS the
/// staleness).
fn freshest_worker(workers: Vec<WorkerEntry>) -> Option<WorkerEntry> {
    workers
        .into_iter()
        .max_by_key(|w| coxagent_application::liveness::rfc3339_secs(&w.at).unwrap_or(i64::MIN))
}

/// The liveness checker's hub-side loop (AC4): detection lives OUTSIDE the
/// unit that can hang — the runner's cycle is awaited inline by its own
/// process, so a wedged pipeline can never run this sweep. Fires even when
/// the worker process is entirely gone: the persisted activity trail governs
/// the trigger, and the registry (`workers()`) is the only other evidence
/// read — no process probe. Scope note: "desired-run" here is the hub-hosted
/// runner's live state (running, not paused, not STOPPED — a user's Stop must
/// never read as a stall) — the deployment whose state the hub can actually
/// read; a remote headless worker's intent lives behind a per-operator flag
/// the JSON hub store cannot enumerate, so it is deliberately not guessed at.
pub(super) async fn liveness_watchdog(app: AppState) {
    let horizon = liveness_horizon_env();
    let sweep = liveness_sweep_secs(horizon);
    loop {
        tokio::time::sleep(Duration::from_secs(sweep)).await;
        let handles: Vec<ProjectHandle> = app.projects.read().await.values().cloned().collect();
        for p in &handles {
            let knobs = liveness_knobs(p, horizon);
            liveness_pass(&p.store, &p.runner, &p.id, knobs, p.outbox.as_ref()).await;
        }
    }
}

/// One sweep over one project: read the store once, decide purely, act once.
/// The alert goes through the existing channels — the `ChatNotifier`
/// (`NotifierPort`) #agents feed and, when wired, the durable webhook spool
/// (`OutboxStorePort`) — and the episode persists for dedupe and self-clear.
/// Best-effort end to end: a store hiccup skips this sweep, it does not fail
/// the hub.
pub(super) async fn liveness_pass(
    store: &std::sync::Arc<dyn StateStorePort>,
    runner: &RunnerHandle,
    project: &str,
    knobs: LivenessKnobs,
    outbox: Option<&std::sync::Arc<dyn coxagent_application::ports::outbound::OutboxStorePort>>,
) {
    use coxagent_application::liveness::{stall_verdict, LivenessSnapshot, LivenessVerdict};
    use coxagent_application::ports::outbound::{ChatNotifier, NotifierPort, NotifyEvent};
    use coxagent_application::state::{unix_now_secs, OutboxEntry};

    if !knobs.enabled {
        // workflow.stall_watchdog=false rolls the behaviour back: clear any
        // open episode so the dashboard chip and the next sweep agree.
        if store.load().await.is_ok_and(|s| s.liveness.is_some()) {
            let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
                s.liveness = None;
                Ok(())
            })
            .await;
        }
        return;
    }
    let Ok(state) = store.load().await else {
        return;
    };
    let workers = store.workers().await.unwrap_or_default();
    // Quota exhaustion announces itself on the trail and nothing newer has
    // landed since — the exact documented non-stall line (cycle/mod.rs).
    let quota_exhausted = state
        .activity
        .last()
        .is_some_and(|a| a.action == coxagent_application::liveness::QUOTA_PAUSE_LINE);
    let over_budget = knobs
        .budget_lifetime_usd
        .is_some_and(|cap| state.spend.total_cost_usd >= cap)
        || knobs
            .budget_daily_usd
            .is_some_and(|cap| state.spend_today_usd >= cap);
    // Desired-run is the runner's LIVE mode: running = on; paused AND stopped
    // are both off (the Stop button stops the handle without pausing it —
    // `is_paused()` alone would read a stopped team as desired-run=true and
    // false-alert on it).
    let desired_run = runner.snapshot().mode == "running";
    let snap = LivenessSnapshot {
        worker: freshest_worker(workers),
        activity: state.activity.clone(),
        desired_run,
        paused: runner.is_paused(),
        over_budget,
        quota_exhausted,
        backlog_empty: coxagent_application::liveness::backlog_is_empty(&state.tickets),
        // u64 -> i64: a seconds threshold past i64::MAX is not a real config,
        // saturate instead of wrapping.
        threshold_secs: i64::try_from(knobs.timeout_secs).unwrap_or(i64::MAX),
        escalate_after_secs: i64::try_from(knobs.escalate_secs).unwrap_or(i64::MAX),
        open_stall: state.liveness.clone(),
        now: coxagent_application::state::now_rfc3339(),
    };
    match stall_verdict(&snap) {
        LivenessVerdict::Quiet => {}
        LivenessVerdict::Stall(alert) | LivenessVerdict::Escalate(alert) => {
            // One alert per episode: the #agents feed post (the channel every
            // other loop alert uses) plus the durable webhook spool. Chat
            // posts never touch the activity trail, so the alert cannot wake
            // the very silence it is reporting.
            ChatNotifier::new(std::sync::Arc::clone(store))
                .notify(NotifyEvent {
                    kind: "cycle_stalled".to_owned(),
                    project: project.to_owned(),
                    message: alert.message.clone(),
                })
                .await;
            if let Some(outbox) = outbox {
                outbox
                    .enqueue(OutboxEntry::pending(
                        "cycle_stalled",
                        project,
                        &alert.message,
                        unix_now_secs(),
                    ))
                    .await;
            }
            let episode = alert.episode;
            let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), move |s| {
                s.liveness = Some(episode.clone());
                Ok(())
            })
            .await;
        }
        LivenessVerdict::Resumed => {
            // New activity landed (or a documented pause took over): announce
            // the recovery once and self-clear the episode.
            let message = "loop resumed — new activity landed; the stall alert is cleared";
            ChatNotifier::new(std::sync::Arc::clone(store))
                .notify(NotifyEvent {
                    kind: "cycle_resumed".to_owned(),
                    project: project.to_owned(),
                    message: message.to_owned(),
                })
                .await;
            if let Some(outbox) = outbox {
                outbox
                    .enqueue(OutboxEntry::pending(
                        "cycle_resumed",
                        project,
                        message,
                        unix_now_secs(),
                    ))
                    .await;
            }
            let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
                s.liveness = None;
                Ok(())
            })
            .await;
        }
    }
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    use coxagent_application::ports::outbound::{MemoryOutboxStore, OutboxStorePort};
    use coxagent_application::state::{ActivityEntry, StallEpisode, AGENTS_CHANNEL};
    use coxagent_infrastructure::state::JsonStateStore;

    fn knobs() -> LivenessKnobs {
        LivenessKnobs {
            enabled: true,
            timeout_secs: 60,
            escalate_secs: 3_600,
            budget_lifetime_usd: None,
            budget_daily_usd: None,
        }
    }

    fn rfc3339_ago(secs_ago: i64) -> String {
        (time::OffsetDateTime::now_utc() - time::Duration::seconds(secs_ago))
            .format(&time::format_description::well_known::Rfc3339)
            .expect("rfc3339")
    }

    /// The `TempDir` is returned alongside the store ON PURPOSE: dropping it
    /// deletes the store's files out from under the test.
    async fn store_with_old_activity() -> (tempfile::TempDir, std::sync::Arc<dyn StateStorePort>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: std::sync::Arc<dyn StateStorePort> =
            std::sync::Arc::new(JsonStateStore::new(dir.path()).expect("store"));
        let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
            s.activity.push(ActivityEntry {
                at: rfc3339_ago(7_200),
                agent: "SM".to_owned(),
                action: "daily digest posted".to_owned(),
                ticket: None,
            });
            // One actionable ticket: an empty backlog is a documented
            // non-stall reason, so the stall fixture must have work.
            s.tickets.push(
                coxagent_domain::Ticket::new(
                    coxagent_domain::TicketId::new("CXC-259".to_owned()).expect("id"),
                    coxagent_domain::TicketType::Feature,
                    "the work the loop went silent on",
                    "fixture",
                    coxagent_domain::Priority::High,
                    coxagent_domain::Complexity::Small,
                    false,
                )
                .expect("valid ticket"),
            );
            Ok(())
        })
        .await;
        (dir, store)
    }

    /// AC1+AC3, executable: a running loop gone silent opens ONE alert
    /// (chat feed + webhook spool + persisted episode), the next sweep
    /// dedupes, and new activity self-clears the episode and announces the
    /// resume. Pure store + handle — no server, no network port.
    #[tokio::test]
    async fn a_silent_running_loop_alerts_once_dedupes_then_clears_on_activity() {
        let (_dir, store) = store_with_old_activity().await;
        let runner = std::sync::Arc::new(RunnerHandle::default());
        runner.resume(); // desired-run on
        let outbox: std::sync::Arc<dyn OutboxStorePort> =
            std::sync::Arc::new(MemoryOutboxStore::new());

        liveness_pass(&store, &runner, "demo", knobs(), Some(&outbox)).await;
        let s = store.load().await.expect("state");
        assert!(s.liveness.is_some(), "the open episode persists for dedupe");
        assert!(
            s.chat_in(AGENTS_CHANNEL)
                .iter()
                .any(|m| m.body.contains("LOOP STALLED")),
            "the #agents feed carries the stall alert"
        );
        assert_eq!(
            outbox.recent(10).await.len(),
            1,
            "one spooled webhook event"
        );

        // The next sweep inside the escalation window dedupes: no flood.
        liveness_pass(&store, &runner, "demo", knobs(), Some(&outbox)).await;
        let s = store.load().await.expect("state");
        let stalled = s
            .chat_in(AGENTS_CHANNEL)
            .iter()
            .filter(|m| m.body.contains("LOOP STALLED"))
            .count();
        assert_eq!(stalled, 1, "the same episode never alerts twice");
        assert_eq!(outbox.recent(10).await.len(), 1);

        // Progress tick: new activity lands, the episode self-clears.
        let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
            s.log_activity("DEV-BUG", "fixed the login 500", None);
            Ok(())
        })
        .await;
        liveness_pass(&store, &runner, "demo", knobs(), Some(&outbox)).await;
        let s = store.load().await.expect("state");
        assert!(s.liveness.is_none(), "the episode self-clears on activity");
        assert!(
            s.chat_in(AGENTS_CHANNEL)
                .iter()
                .any(|m| m.body.contains("resumed")),
            "the recovery is announced"
        );
        assert_eq!(outbox.recent(10).await.len(), 2, "stall + resumed events");
    }

    /// The escalation horizon: an episode silent far past it re-alerts once,
    /// with the counter advanced and the episode's `since` preserved.
    #[tokio::test]
    async fn an_episode_past_the_escalation_horizon_re_alerts_with_a_counter() {
        let (_dir, store) = store_with_old_activity().await;
        let runner = std::sync::Arc::new(RunnerHandle::default());
        runner.resume();
        let outbox: std::sync::Arc<dyn OutboxStorePort> =
            std::sync::Arc::new(MemoryOutboxStore::new());

        liveness_pass(&store, &runner, "demo", knobs(), Some(&outbox)).await;
        let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
            if let Some(ep) = s.liveness.as_mut() {
                ep.last_alert_at = rfc3339_ago(20_000); // ~5.5 h since the first alert
            }
            Ok(())
        })
        .await;

        liveness_pass(&store, &runner, "demo", knobs(), Some(&outbox)).await;
        let s = store.load().await.expect("state");
        let open = s.liveness.as_ref().expect("episode still open");
        assert_eq!(open.escalations, 1, "one escalation counted");
        let escalations = s
            .chat_in(AGENTS_CHANNEL)
            .iter()
            .filter(|m| m.body.contains("ESCALATION 1"))
            .count();
        assert_eq!(escalations, 1, "the escalation re-alerted exactly once");
        assert_eq!(outbox.recent(10).await.len(), 2);
    }

    /// AC2, executable: a PAUSED loop and a STOPPED loop (the Stop button
    /// stops the handle without pausing it) are both user-intent states —
    /// neither may ever alert — and a user pause clears an open episode.
    #[tokio::test]
    async fn a_paused_or_stopped_loop_never_alerts() {
        let (_dir, store) = store_with_old_activity().await;
        let outbox: std::sync::Arc<dyn OutboxStorePort> =
            std::sync::Arc::new(MemoryOutboxStore::new());

        // Default handle starts paused…
        let runner = std::sync::Arc::new(RunnerHandle::default());
        liveness_pass(&store, &runner, "demo", knobs(), Some(&outbox)).await;
        assert!(store.load().await.expect("state").liveness.is_none());
        assert!(
            outbox.recent(10).await.is_empty(),
            "no alert for a paused loop"
        );

        // …and a STOPPED handle must read the same way (regression: stop()
        // does not pause, so a naive is_paused()-only gate false-alerted on a
        // deliberately stopped team).
        let stopped = std::sync::Arc::new(RunnerHandle::default());
        stopped.resume();
        stopped.stop();
        liveness_pass(&store, &stopped, "demo", knobs(), Some(&outbox)).await;
        assert!(store.load().await.expect("state").liveness.is_none());
        assert!(
            outbox.recent(10).await.is_empty(),
            "no alert for a stopped loop"
        );
    }

    /// The rollback switch: workflow.stall_watchdog=false clears its own open
    /// episode and never alerts.
    #[tokio::test]
    async fn a_disabled_watchdog_clears_its_open_episode() {
        let (_dir, store) = store_with_old_activity().await;
        let runner = std::sync::Arc::new(RunnerHandle::default());
        runner.resume();
        let outbox: std::sync::Arc<dyn OutboxStorePort> =
            std::sync::Arc::new(MemoryOutboxStore::new());
        let _ = coxagent_application::ports::outbound::mutate_state(store.as_ref(), |s| {
            s.liveness = Some(StallEpisode {
                since: rfc3339_ago(7_200),
                worker: "luton@macbook".to_owned(),
                last_activity_at: rfc3339_ago(7_200),
                last_alert_at: rfc3339_ago(7_200),
                escalations: 0,
            });
            Ok(())
        })
        .await;
        let off = LivenessKnobs {
            enabled: false,
            ..knobs()
        };
        liveness_pass(&store, &runner, "demo", off, Some(&outbox)).await;
        assert!(
            store.load().await.expect("state").liveness.is_none(),
            "disabling the watchdog clears its open episode"
        );
        assert!(outbox.recent(10).await.is_empty());
    }

    /// The freshest-worker pick orders by parsed instant: a non-UTC offset
    /// stamp that string-sorts later must not beat a truly fresher Z stamp.
    #[test]
    fn the_freshest_worker_is_picked_by_instant_not_string_order() {
        let mk = |at: &str| WorkerEntry {
            worker: "w@h".to_owned(),
            role: "DEV-FEATURE".to_owned(),
            ticket: String::new(),
            at: at.to_owned(),
            engines: Vec::new(),
            models: Vec::new(),
            git: None,
            tooling: None,
            version: String::new(),
        };
        let fresher = mk("2026-08-31T10:00:00Z");
        let string_lies = mk("2026-08-31T11:00:00+02:00"); // = 09:00Z, OLDER
        assert!(
            string_lies.at > fresher.at,
            "precondition: string order lies"
        );
        // WorkerEntry carries no PartialEq — compare by the heartbeat stamp.
        let picked = freshest_worker(vec![string_lies, fresher.clone()]).map(|w| w.at);
        assert_eq!(picked.as_deref(), Some(fresher.at.as_str()));
        assert_eq!(freshest_worker(Vec::new()).map(|w| w.at), None);
        // Unparseable stamps sort oldest, never panic.
        let picked = freshest_worker(vec![mk("garbage"), fresher.clone()]).map(|w| w.at);
        assert_eq!(picked.as_deref(), Some(fresher.at.as_str()));
    }
}
