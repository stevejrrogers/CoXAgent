//! Durable webhook delivery (CXA-F235).
//!
//! [`WebhookNotifier`] used to POST each event once, fire-and-forget: a
//! webhook that was down or slow at exactly the moment a deploy failed or a
//! budget cap hit silently ate the alert. Now `notify` only **spools** the
//! event into the [`OutboxStorePort`] (a quick local write that never waits on
//! the webhook), and an independent background flusher drains the spool:
//! claims due entries, POSTs each with its idempotency key, and records the
//! outcome — delivered on 2xx, else exponential backoff via
//! `advance_after_failure` until delivered or dead (replayable in the UI).
//!
//! Slack/Teams are just specific URLs, as before. The event kinds stay
//! forward-compatible: an unknown kind spools and delivers like any other.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{NotifierPort, NotifyEvent, OutboxStorePort};
use coxagent_application::state::{
    advance_after_failure, unix_now_secs, OutboxEntry, OutboxStatus,
};
use std::sync::Arc;
use std::time::Duration;

/// How long the flusher waits between polls when nothing is due. Backoff
/// deadlines are second-granular, so this keeps retry latency tight without
/// spinning.
const FLUSH_POLL_SECS: u64 = 5;

/// Entries claimed per drain cycle — bounds one flush pass under a burst.
const CLAIM_BATCH: u32 = 16;

/// Spools events to the durable outbox for an independent flusher to deliver.
#[derive(Clone)]
pub struct WebhookNotifier {
    outbox: Arc<dyn OutboxStorePort>,
}

impl WebhookNotifier {
    /// A notifier spooling into `outbox`; the flusher (see
    /// [`spawn_outbox_flusher`]) drains it to the configured URL.
    #[must_use]
    pub fn new(outbox: Arc<dyn OutboxStorePort>) -> Self {
        Self { outbox }
    }
}

#[async_trait]
impl NotifierPort for WebhookNotifier {
    async fn notify(&self, event: NotifyEvent) {
        // Best-effort by contract: a spool hiccup must never fail the cycle.
        // The entry is immediately due; the flusher owns everything after.
        self.outbox
            .enqueue(OutboxEntry::pending(
                &event.kind,
                &event.project,
                &event.message,
                unix_now_secs(),
            ))
            .await;
    }
}

/// Start the background flusher draining `outbox` to `url`. Independent of the
/// cycle loop: a webhook that stays down slows nothing but its own retries.
/// Entries claimed but unacknowledged when the process exits are re-claimed
/// after the lease TTL on the next start — at-least-once, keyed by entry id.
pub fn spawn_outbox_flusher(outbox: Arc<dyn OutboxStorePort>, url: String) {
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        loop {
            let progressed = flush_once(outbox.as_ref(), &url, &client).await;
            if !progressed {
                tokio::time::sleep(Duration::from_secs(FLUSH_POLL_SECS)).await;
            }
        }
    });
}

/// One drain pass: claim due entries, deliver each, record the outcome.
/// Returns whether anything was claimed (so the caller can drain again
/// immediately instead of polling).
pub(crate) async fn flush_once(
    outbox: &dyn OutboxStorePort,
    url: &str,
    client: &reqwest::Client,
) -> bool {
    let due = outbox.claim_due(CLAIM_BATCH).await;
    for entry in &due {
        match post_event(client, url, entry).await {
            Ok(()) => outbox.mark_delivered(entry.id).await,
            Err(()) => match advance_after_failure(unix_now_secs(), entry) {
                failed if failed.status == OutboxStatus::Dead => {
                    tracing::warn!(
                        "alert {} ({}) dead after {} attempt(s) — replayable in the dashboard",
                        entry.id,
                        entry.kind,
                        failed.attempts
                    );
                    outbox.mark_dead(entry.id).await;
                }
                failed => {
                    outbox.mark_retry(entry.id, failed.next_attempt_at).await;
                }
            },
        }
    }
    // A full batch may have left more due entries; anything less means the
    // drain caught up and the caller can go back to polling.
    !due.is_empty()
}

/// POST one spooled event. The body is the same `NotifyEvent` JSON the old
/// fire-and-forget notifier sent; the entry id rides as `Idempotency-Key` so
/// an at-least-once receiver can dedupe retries without any call-site change.
/// Any non-2xx — or a transport error/timeout — is a failed attempt.
async fn post_event(client: &reqwest::Client, url: &str, entry: &OutboxEntry) -> Result<(), ()> {
    let event = NotifyEvent {
        kind: entry.kind.clone(),
        project: entry.project.clone(),
        message: entry.message.clone(),
    };
    let response = client
        .post(url)
        .header("Idempotency-Key", entry.id.to_string())
        .json(&event)
        .send()
        .await
        .map_err(|_| ())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbox::FileOutboxStore;
    use coxagent_application::ports::outbound::MemoryOutboxStore;
    use coxagent_application::state::OUTBOX_BASE_BACKOFF_SECS;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;

    fn spool_dir() -> PathBuf {
        tempfile::tempdir().expect("tempdir").keep()
    }

    fn event() -> NotifyEvent {
        NotifyEvent {
            kind: "deploy_failed".to_owned(),
            project: "demo".to_owned(),
            message: "shipped v1.2.3 failed".to_owned(),
        }
    }

    /// A tiny blocking HTTP listener that answers `status` and captures the
    /// first request (head + body) — the same local-socket pattern the old
    /// fire-and-forget test used, kept for the wire-format assertions.
    fn one_shot_listener(status: u16) -> (std::thread::JoinHandle<String>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let status_line = format!("HTTP/1.1 {status} T\r\nContent-Length: 0\r\n\r\n");
            let _ = stream.write_all(status_line.as_bytes());
            req
        });
        (handle, format!("http://127.0.0.1:{port}/hook"))
    }

    #[tokio::test]
    async fn notify_spools_the_event_without_any_http_call() {
        // The old notifier POSTed inline; now notify only enqueues — a dead
        // webhook cannot even slow this down.
        let spool: Arc<dyn OutboxStorePort> = Arc::new(MemoryOutboxStore::new());
        WebhookNotifier::new(Arc::clone(&spool))
            .notify(event())
            .await;
        let recent = spool.recent(10).await;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].kind, "deploy_failed");
        assert_eq!(recent[0].project, "demo");
        assert!(recent[0].id > 0, "the spool assigns the idempotency id");
    }

    #[tokio::test]
    async fn flush_delivers_the_event_json_with_the_idempotency_key() {
        let (handle, url) = one_shot_listener(200);
        let spool: Arc<dyn OutboxStorePort> =
            Arc::new(FileOutboxStore::new(spool_dir()).expect("spool"));
        WebhookNotifier::new(Arc::clone(&spool))
            .notify(event())
            .await;
        let id = spool.recent(10).await[0].id;

        assert!(flush_once(spool.as_ref(), &url, &client()).await);

        let req = handle.join().expect("join");
        assert!(req.starts_with("POST /hook"), "got: {req}");
        assert!(req.contains("\"kind\":\"deploy_failed\""));
        assert!(req.contains("\"project\":\"demo\""));
        assert!(req.contains("shipped v1.2.3"));
        // The receiver dedupes retries off this header — it must carry the
        // spool identity, unchanged across attempts.
        let key = req
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("idempotency-key")
                    .then(|| value.trim().to_owned())
            })
            .expect("idempotency key header present");
        assert_eq!(key, id.to_string());
    }

    #[tokio::test]
    async fn a_non_2xx_response_schedules_a_backoff_retry() {
        let (handle, url) = one_shot_listener(500);
        let spool: Arc<dyn OutboxStorePort> =
            Arc::new(FileOutboxStore::new(spool_dir()).expect("spool"));
        WebhookNotifier::new(Arc::clone(&spool))
            .notify(event())
            .await;

        flush_once(spool.as_ref(), &url, &client()).await;
        handle.join().expect("join");

        let recent = spool.recent(10).await;
        assert_eq!(recent[0].status, OutboxStatus::Pending, "still delivering");
        assert_eq!(recent[0].attempts, 1);
        assert!(recent[0].next_attempt_at >= unix_now_secs() + OUTBOX_BASE_BACKOFF_SECS - 1);
        assert!(spool.claim_due(10).await.is_empty(), "backing off, not due");
    }

    #[tokio::test]
    async fn exhausting_attempts_across_flushes_parks_the_entry_dead() {
        // Fail, fail, fail… until the entry flips dead and becomes replayable
        // instead of retried forever. The intermediate retries are forced due
        // via the store (past deadlines, as if the backoff had elapsed); the
        // final decision — dead at the max attempt — is the flusher's.
        let spool: Arc<dyn OutboxStorePort> = Arc::new(
            FileOutboxStore::new(spool_dir())
                .expect("spool")
                .with_lease_ttl_secs(0), // every claim re-due immediately
        );
        WebhookNotifier::new(Arc::clone(&spool))
            .notify(event())
            .await;
        let id = spool.recent(1).await[0].id;

        let (_h, url) = one_shot_listener(500);
        let _ = flush_once(spool.as_ref(), &url, &client()).await; // attempt 1
        for _ in 1..coxagent_application::state::OUTBOX_MAX_ATTEMPTS {
            // Back off (as the flusher would), then let the deadline pass.
            spool.mark_retry(id, unix_now_secs() - 1).await;
        }
        let (_h2, url2) = one_shot_listener(500);
        let _ = flush_once(spool.as_ref(), &url2, &client()).await; // final attempt

        let dead = &spool.recent(10).await[0];
        assert_eq!(dead.id, id);
        assert_eq!(dead.status, OutboxStatus::Dead);
        assert_eq!(
            dead.attempts,
            coxagent_application::state::OUTBOX_MAX_ATTEMPTS
        );
        assert!(
            spool.claim_due(10).await.is_empty(),
            "dead entries are parked"
        );
        assert!(spool.replay(id).await, "a dead alert is replayable");
        assert_eq!(spool.recent(1).await[0].status, OutboxStatus::Pending);
    }

    #[tokio::test]
    async fn flusher_spawns_and_drains_in_the_background() {
        // End-to-end through the public seam: spawn + notify, then wait for
        // the background flusher to deliver and record the acknowledgement.
        let (handle, url) = one_shot_listener(200);
        let spool: Arc<dyn OutboxStorePort> =
            Arc::new(FileOutboxStore::new(spool_dir()).expect("spool"));
        spawn_outbox_flusher(Arc::clone(&spool), url);
        WebhookNotifier::new(Arc::clone(&spool))
            .notify(event())
            .await;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if spool
                .recent(10)
                .await
                .first()
                .is_some_and(|e| e.status == OutboxStatus::Delivered)
            {
                break;
            }
        }
        let _ = handle.join();
        assert_eq!(
            spool.recent(10).await[0].status,
            OutboxStatus::Delivered,
            "background flusher delivered the spooled alert"
        );
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client")
    }
}
