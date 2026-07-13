//! `WebhookNotifier` — the HTTP [`NotifierPort`]: POSTs each event as JSON to a
//! configured URL. Best-effort with a short timeout, so a slow or down receiver
//! never blocks or fails the cycle. Slack/Teams are just specific URLs.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{NotifierPort, NotifyEvent};
use std::time::Duration;

/// Posts events to a webhook URL.
pub struct WebhookNotifier {
    url: String,
    client: reqwest::Client,
}

impl WebhookNotifier {
    /// Create a notifier posting to `url`.
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            url: url.into(),
            client,
        }
    }
}

#[async_trait]
impl NotifierPort for WebhookNotifier {
    async fn notify(&self, event: NotifyEvent) {
        // Fire-and-forget; a webhook failure must not affect the cycle.
        let _ = self.client.post(&self.url).json(&event).send().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[tokio::test]
    async fn posts_event_json_to_the_url() {
        // A tiny blocking HTTP listener that captures one request body.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            req
        });

        let notifier = WebhookNotifier::new(format!("http://127.0.0.1:{port}/hook"));
        notifier
            .notify(NotifyEvent {
                kind: "deploy_ok".to_owned(),
                project: "demo".to_owned(),
                message: "shipped v1.2.3".to_owned(),
            })
            .await;

        let req = handle.join().unwrap();
        assert!(req.starts_with("POST /hook"), "got: {req}");
        assert!(req.contains("\"kind\":\"deploy_ok\""));
        assert!(req.contains("\"project\":\"demo\""));
        assert!(req.contains("shipped v1.2.3"));
    }
}
