//! Graceful shutdown: flip a flag on SIGINT/SIGTERM so the loop finishes the
//! current cycle and exits cleanly instead of being killed mid-write.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// A shared shutdown signal driven by OS signals.
pub struct Shutdown {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl Shutdown {
    /// Start listening for SIGINT (and SIGTERM on unix). Returns immediately.
    #[must_use]
    pub fn listen() -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        spawn_listener(Arc::clone(&flag), Arc::clone(&notify));
        Self { flag, notify }
    }

    /// Whether a shutdown has been requested.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Sleep for `dur`, waking early if shutdown is requested.
    pub async fn sleep_or_shutdown(&self, dur: std::time::Duration) {
        tokio::select! {
            () = tokio::time::sleep(dur) => {}
            () = self.notify.notified() => {}
        }
    }
}

#[cfg(unix)]
fn spawn_listener(flag: Arc<AtomicBool>, notify: Arc<Notify>) {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async move {
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("cannot listen for SIGTERM: {e}");
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        tracing::info!("shutdown signal received");
        flag.store(true, Ordering::SeqCst);
        notify.notify_waiters();
    });
}

#[cfg(not(unix))]
fn spawn_listener(flag: Arc<AtomicBool>, notify: Arc<Notify>) {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        flag.store(true, Ordering::SeqCst);
        notify.notify_waiters();
    });
}
