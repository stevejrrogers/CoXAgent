//! HTTP adapter for [`PrReporterPort`] — the runner talks to its hub over a
//! plain REST API (never to the database), so the web dashboard can read what
//! the forge-authenticated runner learned even when it runs in a container with
//! no `gh` and no token.
//!
//! The hub authenticates the runner with an internally-minted bearer token
//! (the same pattern `/api/mcp` uses), so no forge secret passes through config.
//!
//! Endpoints used (project is carried in the body, keeping the path clear of
//! the per-project / PR-review auth gates):
//! - `POST /api/pr-report`          — report an opened PR or a review verdict.
//! - `GET  /api/pr-report/reviews`  — read back persisted reviews.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{PrOpen, PrReporterPort};
use coxagent_application::state::PrReview;
use std::time::Duration;

/// Posts PR/review events to a hub over HTTP.
pub struct HttpPrReporter {
    project: String,
    base_url: String,
    token: String,
    client: reqwest::Client,
}

impl HttpPrReporter {
    /// Build a reporter for `project`, posting to `base_url` (e.g.
    /// `http://localhost:4000`) authenticated with `token` (an internal bearer
    /// token minted for this runner by the hub's own auth store).
    #[must_use]
    pub fn new(
        project: impl Into<String>,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            project: project.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            token: token.into(),
            client,
        }
    }
}

#[async_trait]
impl PrReporterPort for HttpPrReporter {
    async fn report_pr(&self, pr: PrOpen) {
        let url = format!("{}/api/pr-report", self.base_url);
        let body = serde_json::json!({ "project": self.project, "pr": pr });
        let _ = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await;
    }

    async fn report_review(&self, number: u64, decision: &str, summary: &str, head_sha: &str) {
        let url = format!("{}/api/pr-report", self.base_url);
        let body = serde_json::json!({
            "project": self.project,
            "review": { "number": number, "decision": decision, "summary": summary, "head_sha": head_sha }
        });
        let _ = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await;
    }

    async fn report_hold(&self, number: u64, reason: &str) {
        let url = format!("{}/api/pr-report", self.base_url);
        let body = serde_json::json!({
            "project": self.project,
            "hold": { "number": number, "reason": reason }
        });
        let _ = self
            .client
            .post(&url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await;
    }

    async fn fetch_reviews(&self) -> Vec<PrReview> {
        let url = format!("{}/api/pr-report/reviews", self.base_url);
        match self
            .client
            .get(&url)
            .bearer_auth(&self.token)
            .query(&[("project", &self.project)])
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                resp.json::<Vec<PrReview>>().await.unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }
}
