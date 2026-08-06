//! Gateway-backed [`StateStorePort`]: the runner reaches state through REST,
//! never by opening Postgres or Redis itself.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{StateStorePort, WorkerCaps, WorkerEntry};
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
use coxagent_domain::TicketId;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Point this adapter at one project on a control-plane gateway.
#[derive(Clone)]
pub struct RestConfig {
    /// Gateway origin without a trailing slash, e.g. `http://127.0.0.1:4000`.
    pub base_url: String,
    /// Project id used in path routing (`.../projects/<id>/store`).
    pub project_id: String,
    /// Optional bearer token for RBAC-enabled gateways; empty for open mode.
    pub token: Option<String>,
}

impl RestConfig {
    fn endpoint(&self) -> String {
        format!(
            "{}/api/projects/{}/store",
            self.base_url.trim_end_matches('/'),
            self.project_id
        )
    }
}

/// One operation sent to the gateway `/store?op=...`, carrying only that op's
/// arguments. All fields optional; unused ones are omitted.
#[derive(Default, Serialize)]
struct Body {
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    now: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<String>,
}

impl Body {
    fn new() -> Self {
        Self {
            revision: None,
            data: None,
            id: None,
            worker: None,
            now: None,
            stage: None,
        }
    }
}

/// Gateway-backed state store; each method round-trips to the control plane.
pub struct RestStateStore {
    cfg: RestConfig,
}

impl RestStateStore {
    /// Create an adapter pointed at a gateway project.
    ///
    /// # Errors
    /// [`PortError::Backend`] if `base_url` is empty.
    pub fn new(cfg: RestConfig) -> Result<Self, PortError> {
        if cfg.base_url.is_empty() {
            return Err(PortError::Backend("RestStateStore needs a base_url".into()));
        }
        Ok(Self { cfg })
    }

    async fn post(&self, op: &str, body: Body) -> Result<reqwest::Response, PortError> {
        let url = format!("{}?op={op}", self.cfg.endpoint());
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| PortError::Backend(format!("http build: {e}")))?;
        let mut rb = client.post(url).json(&body);
        if let Some(tok) = &self.cfg.token {
            rb = rb.bearer_auth(tok);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::CONFLICT {
            return Err(PortError::Conflict("remote store conflict".into()));
        }
        if !resp.status().is_success() {
            let code = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(PortError::Backend(format!(
                "remote store {op}: {code} - {text}"
            )));
        }
        Ok(resp)
    }
}

#[async_trait]
impl StateStorePort for RestStateStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        let r = self.post("load", Body::new()).await?;
        r.json()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        let data = serde_json::to_string(state)
            .map_err(|e| PortError::Backend(format!("encode state: {e}")))?;
        let mut body = Body::new();
        body.data = Some(data);
        self.post("save", body).await?;
        Ok(())
    }

    async fn claim_ticket(
        &self,
        id: &TicketId,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        let mut body = Body::new();
        body.id = Some(id.to_string());
        body.worker = Some(worker.to_owned());
        body.now = Some(now.to_owned());
        self.won("claim_ticket", body).await
    }

    async fn acquire_leader(&self, worker: &str, now: &str) -> Result<bool, PortError> {
        let mut body = Body::new();
        body.worker = Some(worker.to_owned());
        body.now = Some(now.to_owned());
        self.won("acquire_leader", body).await
    }

    async fn claim_stage(
        &self,
        id: &TicketId,
        stage: &str,
        worker: &str,
        now: &str,
    ) -> Result<bool, PortError> {
        let mut body = Body::new();
        body.id = Some(id.to_string());
        body.stage = Some(stage.to_owned());
        body.worker = Some(worker.to_owned());
        body.now = Some(now.to_owned());
        self.won("claim_stage", body).await
    }

    async fn release_stage(
        &self,
        id: &TicketId,
        stage: &str,
        worker: &str,
    ) -> Result<(), PortError> {
        let mut body = Body::new();
        body.id = Some(id.to_string());
        body.stage = Some(stage.to_owned());
        body.worker = Some(worker.to_owned());
        self.post("release_stage", body).await?;
        Ok(())
    }

    async fn heartbeat_worker(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        caps: &WorkerCaps,
        now: &str,
    ) -> Result<(), PortError> {
        let mut body = Body::new();
        body.worker = Some(worker.to_owned());
        body.data = Some(serde_json::json!({
            "role": role,
            "ticket": ticket,
            "engines": caps.engines,
            "models": caps.models,
            "git": caps.git.as_ref().map(|g| serde_json::to_string(g).unwrap_or_default()),
            "tooling": caps.tooling.as_ref().map(|t| serde_json::to_string(t).unwrap_or_default()),
            "now": now
        }).to_string());
        self.post("heartbeat", body).await?;
        Ok(())
    }

    async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        let r = self.post("workers", Body::new()).await?;
        r.json()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))
    }

    async fn set_desired(&self, operator: &str, running: bool) -> Result<(), PortError> {
        let mut body = Body::new();
        body.worker = Some(operator.to_owned());
        body.data = Some(running.to_string());
        self.post("set_desired", body).await?;
        Ok(())
    }

    async fn get_desired(&self, operator: &str) -> Result<Option<bool>, PortError> {
        #[derive(Deserialize)]
        struct DesiredValue {
            value: Option<bool>,
        }
        let mut body = Body::new();
        body.worker = Some(operator.to_owned());
        let r = self.post("get_desired", body).await?;
        let d: DesiredValue = r
            .json()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(d.value)
    }

    async fn acquire_operator(&self, operator: &str, instance: &str) -> Result<bool, PortError> {
        let mut body = Body::new();
        body.worker = Some(operator.to_owned());
        body.now = Some(instance.to_owned());
        self.won("acquire_operator", body).await
    }
}

impl RestStateStore {
    /// POST an operation that answers `{ "won": bool }`.
    async fn won(&self, op: &str, body: Body) -> Result<bool, PortError> {
        #[derive(Deserialize)]
        struct Won {
            #[serde(default)]
            won: bool,
        }
        let r = self.post(op, body).await?;
        let w: Won = r
            .json()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(w.won)
    }
}
