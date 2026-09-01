//! Gateway-backed [`StateStorePort`]: the runner reaches state through REST,
//! never by opening Postgres or Redis itself.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{StateStorePort, WorkerCaps, WorkerEntry};
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
use coxagent_domain::TicketId;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Per-request deadline the REST client enforces by default, identical to the
/// value this adapter hard-coded before CXA-F029 bug #2 — existing deployments
/// behave byte-for-byte the same unless the operator overrides it.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Point this adapter at one project on a control-plane gateway.
#[derive(Clone)]
pub struct RestConfig {
    /// Gateway origin without a trailing slash, e.g. `http://127.0.0.1:4000`.
    pub base_url: String,
    /// Project id used in path routing (`.../projects/<id>/store`).
    pub project_id: String,
    /// Optional bearer token for RBAC-enabled gateways; empty for open mode.
    pub token: Option<String>,
    /// Client-side deadline for one store request. Defaults to
    /// [`DEFAULT_TIMEOUT_SECS`] via [`RestConfig::timeout_from_env`]; operators
    /// of slow gateways raise it with `COXAGENT_REMOTE_STORE_TIMEOUT_SECS`.
    pub timeout: Duration,
}

impl RestConfig {
    fn endpoint(&self) -> String {
        format!(
            "{}/api/projects/{}/store",
            self.base_url.trim_end_matches('/'),
            self.project_id
        )
    }

    /// The timeout [`RestStateStore`] should be built with: whole seconds from
    /// `COXAGENT_REMOTE_STORE_TIMEOUT_SECS`, or [`DEFAULT_TIMEOUT_SECS`] when
    /// unset, unparsable, or zero (a zero deadline would fail every call).
    pub fn timeout_from_env() -> Duration {
        std::env::var("COXAGENT_REMOTE_STORE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&secs| secs > 0)
            .map_or(
                Duration::from_secs(DEFAULT_TIMEOUT_SECS),
                Duration::from_secs,
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

    /// The remedy for a gateway auth rejection, or `None` for any other
    /// status. Pure so the failure-path wording is unit-testable without a
    /// live gateway (CXA-F029 bug #3: a bare `401 - ` told the operator
    /// nothing about the bearer they were missing).
    fn credential_hint(code: reqwest::StatusCode) -> Option<&'static str> {
        match code {
            reqwest::StatusCode::UNAUTHORIZED => Some(
                "the gateway requires a bearer: set COXAGENT_REMOTE_TOKEN to a \
                 personal API token (minted automatically at hub sign-in, or \
                 create one under API tokens in the hub UI)",
            ),
            reqwest::StatusCode::FORBIDDEN => Some(
                "the presented credential is valid but lacks rights for this \
                 project: sign in as the operator that owns it, or present a \
                 token for a manage-tier member",
            ),
            _ => None,
        }
    }

    async fn post(&self, op: &str, body: Body) -> Result<reqwest::Response, PortError> {
        let url = format!("{}?op={op}", self.cfg.endpoint());
        let client = reqwest::Client::builder()
            .timeout(self.cfg.timeout)
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
            let hint = Self::credential_hint(code)
                .map(|h| format!(" ({h})"))
                .unwrap_or_default();
            return Err(PortError::Backend(format!(
                "remote store {op}: {code} - {text}{hint}"
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

    async fn save_expecting(
        &self,
        state: &ProjectState,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        // Carry the caller-captured revision so the gateway rejects a stale
        // write with a 409 Conflict instead of silently overwriting newer data.
        // `None` (older clients, or backends without version tracking) omits the
        // field and keeps today's behaviour.
        let data = serde_json::to_string(state)
            .map_err(|e| PortError::Backend(format!("encode state: {e}")))?;
        let mut body = Body::new();
        body.data = Some(data);
        if expected_revision.is_some() {
            body.revision = expected_revision;
            self.post("save", body).await?;
            return Ok(());
        }
        self.save(state).await
    }

    async fn current_version(&self) -> Result<Option<i64>, PortError> {
        #[derive(Deserialize)]
        struct Versioned {
            #[serde(default)]
            revision: Option<i64>,
        }
        let r = self.post("version", Body::new()).await?;
        let v: Versioned = r
            .json()
            .await
            .map_err(|e| PortError::Backend(e.to_string()))?;
        Ok(v.revision)
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
            "version": caps.version,
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

    /// Ask the gateway to purge this project's persisted footprint. The
    /// control plane decides what "everything" means for its backend; a
    /// silent no-op here would resurrect the deleted project's state on the
    /// hub that owns the row (CXA-B130), so failures propagate.
    async fn delete(&self) -> Result<(), PortError> {
        self.post("delete", Body::new()).await?;
        Ok(())
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

#[cfg(test)]
mod rest_config_tests {
    use super::*;

    /// `COXAGENT_REMOTE_STORE_TIMEOUT_SECS` is process-global; serialize the
    /// tests that touch it so parallel test threads cannot clobber each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn credential_hint_names_the_bearer_remedy_on_auth_failures() {
        let hint = RestStateStore::credential_hint(reqwest::StatusCode::UNAUTHORIZED)
            .expect("401 must carry a remedy");
        assert!(
            hint.contains("COXAGENT_REMOTE_TOKEN"),
            "the 401 remedy must name the env var the operator can set: {hint}"
        );

        let hint = RestStateStore::credential_hint(reqwest::StatusCode::FORBIDDEN)
            .expect("403 must carry a remedy");
        assert!(
            hint.contains("manage-tier") || hint.contains("rights"),
            "the 403 remedy must point at the rights gap, not the token: {hint}"
        );
    }

    #[test]
    fn credential_hint_is_silent_for_non_auth_failures() {
        for code in [
            reqwest::StatusCode::NOT_FOUND,
            reqwest::StatusCode::BAD_REQUEST,
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(
                RestStateStore::credential_hint(code).is_none(),
                "{code} is not a credential problem — no remedy hint"
            );
        }
    }

    #[test]
    fn timeout_defaults_to_the_previously_hardcoded_deadline() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::remove_var("COXAGENT_REMOTE_STORE_TIMEOUT_SECS");
        assert_eq!(
            RestConfig::timeout_from_env(),
            Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            "no env override must reproduce the old hard-coded 120 s exactly"
        );
    }

    #[test]
    fn timeout_env_override_wins_and_garbage_falls_back() {
        let _guard = ENV_LOCK.lock().expect("env lock");

        std::env::set_var("COXAGENT_REMOTE_STORE_TIMEOUT_SECS", "300");
        assert_eq!(
            RestConfig::timeout_from_env(),
            Duration::from_secs(300),
            "a valid operator override must win"
        );

        for garbage in ["not-a-number", "0", "-5", ""] {
            std::env::set_var("COXAGENT_REMOTE_STORE_TIMEOUT_SECS", garbage);
            assert_eq!(
                RestConfig::timeout_from_env(),
                Duration::from_secs(DEFAULT_TIMEOUT_SECS),
                "unparsable/zero override {garbage:?} must keep the safe default"
            );
        }

        std::env::remove_var("COXAGENT_REMOTE_STORE_TIMEOUT_SECS");
    }
}
