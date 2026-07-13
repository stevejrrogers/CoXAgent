//! `NotifierPort` — outbound event notifications (webhooks). The cycle emits a
//! small set of significant events; an adapter delivers them (HTTP POST, or a
//! no-op locally). Slack/Teams integrations are just adapters over this port.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A significant event worth notifying an operator or channel about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyEvent {
    /// Machine-readable kind, e.g. `deploy_ok`, `deploy_failed`,
    /// `budget_reached`, `policy_blocked`.
    pub kind: String,
    /// The project the event belongs to.
    pub project: String,
    /// Human-readable one-line summary.
    pub message: String,
}

/// Outbound notification sink.
#[async_trait]
pub trait NotifierPort: Send + Sync {
    /// Deliver one event. Best-effort: implementations must never fail the
    /// caller (a down webhook must not break the cycle).
    async fn notify(&self, event: NotifyEvent);
}

/// A notifier that drops everything — the default when no webhook is configured.
pub struct NullNotifier;

#[async_trait]
impl NotifierPort for NullNotifier {
    async fn notify(&self, _event: NotifyEvent) {}
}
