//! `NotifierPort` — outbound event notifications (webhooks). The cycle emits a
//! small set of significant events; an adapter delivers them (HTTP POST, or a
//! no-op locally). Slack/Teams integrations are just adapters over this port.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A significant event worth notifying an operator or channel about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyEvent {
    /// Machine-readable kind, e.g. `deploy_ok`, `deploy_failed`,
    /// `budget_reached`, `budget_warning`, `policy_blocked`. New kinds are
    /// additive — consumers (including [`FanoutNotifier`]/webhooks) must treat
    /// an unrecognized kind as forward-compatible rather than dropping it.
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

/// Delivers events into the project's `#agents` channel as a bot post,
/// so notifications live where the user already is — and the app's existing
/// chat push-notification path raises a native banner for them. Writes through
/// the state store, so it works from the hub AND from headless operators.
pub struct ChatNotifier<S: super::StateStorePort + ?Sized> {
    store: std::sync::Arc<S>,
}

impl<S: super::StateStorePort + ?Sized> ChatNotifier<S> {
    pub fn new(store: std::sync::Arc<S>) -> Self {
        Self { store }
    }
}

/// A leading emoji per event kind so alerts scan at a glance in the chat.
fn kind_icon(kind: &str) -> &'static str {
    match kind {
        k if k.contains("deploy_failed") || k.contains("fail") => "❌",
        k if k.contains("deploy") => "🚀",
        k if k.contains("budget_warning") => "⚠️",
        k if k.contains("budget") => "💰",
        k if k.contains("quota") => "⛔",
        k if k.contains("pr") => "🔀",
        k if k.contains("sprint") => "🏁",
        k if k.contains("impediment") => "🚧",
        k if k.contains("digest") => "📰",
        _ => "🔔",
    }
}

#[async_trait]
impl<S: super::StateStorePort + ?Sized> NotifierPort for ChatNotifier<S> {
    async fn notify(&self, event: NotifyEvent) {
        let body = format!("{} {}", kind_icon(&event.kind), event.message);
        let _ = super::mutate_state(self.store.as_ref(), |s| {
            s.post_chat_in("COX", &body, crate::state::AGENTS_CHANNEL, Vec::new());
            Ok(())
        })
        .await;
    }
}

/// Fans one event out to several sinks (e.g. team chat + an external webhook).
pub struct FanoutNotifier(pub Vec<std::sync::Arc<dyn NotifierPort>>);

#[async_trait]
impl NotifierPort for FanoutNotifier {
    async fn notify(&self, event: NotifyEvent) {
        for n in &self.0 {
            n.notify(event.clone()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::kind_icon;

    #[test]
    fn budget_warning_gets_its_own_amber_icon_distinct_from_the_hard_stop() {
        assert_eq!(kind_icon("budget_warning"), "⚠️");
        assert_eq!(kind_icon("budget_reached"), "💰");
        assert_ne!(kind_icon("budget_warning"), kind_icon("budget_reached"));
    }

    #[test]
    fn impediment_digest_gets_the_construction_icon_not_the_generic_digest_one() {
        assert_eq!(kind_icon("impediment_digest"), "🚧");
        assert_ne!(kind_icon("impediment_digest"), "📰");
    }
}
