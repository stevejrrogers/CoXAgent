//! HTTP probe adapter — captures one GET as API evidence.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{ApiProbePort, ApiProof};
use std::time::Duration;

pub struct HttpProbe;

#[async_trait]
impl ApiProbePort for HttpProbe {
    async fn get(&self, url: &str) -> Option<ApiProof> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .ok()?;
        let resp = client.get(url).send().await.ok()?;
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Some(ApiProof {
            status,
            body_snippet: body.chars().take(600).collect(),
        })
    }
}
