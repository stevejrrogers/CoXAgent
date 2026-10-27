//! `HttpEngine` — an [`AgentEnginePort`] over any OpenAI-compatible
//! `/chat/completions` HTTP gateway (OpenAI, Azure OpenAI, vLLM, LiteLLM,
//! OpenRouter, a local llama.cpp server, bizbrain, …). It issues ONE chat
//! completion and returns the assistant's text as the run's stdout.
//!
//! Scope (slice 1): this is a TEXT-completion engine. It produces a reply; it
//! does not read or edit files in the workspace. That makes it a first-class
//! fit for the text-producing roles — chat replies, BA/PO/SM/SA planning,
//! review narration, docs — and for pointing the hub at any gateway without a
//! vendor CLI on the host. It is NOT yet a coding engine: assigned to a
//! DEV-BUG/DEV-FEATURE role it will return prose and leave the tree unchanged,
//! so the cycle sees no diff and the ticket simply does not progress (a visible
//! no-op, never a bad merge). A follow-up slice adds the read/apply tool loop
//! that makes it edit code.
//!
//! Endpoint + credentials come from the environment, matching the hub's
//! existing convention: `COXAGENT_LLM_BASE_URL` (default
//! `https://api.openai.com/v1`) and `COXAGENT_LLM_API_KEY`. The key rides the
//! `Authorization: Bearer` header — never a URL or argv.

use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::ports::outbound::engine::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, Usage,
};
use coxagent_application::PortError;
use serde_json::json;

/// Default gateway when `COXAGENT_LLM_BASE_URL` is unset — public OpenAI.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// An OpenAI-compatible chat engine for one model selection.
pub struct HttpEngine {
    model: String,
    /// Gateway base, e.g. `https://api.openai.com/v1` — `/chat/completions` is
    /// appended. Trailing slash tolerated.
    base_url: String,
    /// Bearer token; empty means "unconfigured" and `run` fails clearly.
    api_key: String,
}

impl HttpEngine {
    #[must_use]
    pub fn new(
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
    }

    /// Build from the hub's env convention: `COXAGENT_LLM_BASE_URL` (default
    /// public OpenAI) + `COXAGENT_LLM_API_KEY`.
    #[must_use]
    pub fn from_env(model: impl Into<String>) -> Self {
        let base_url = std::env::var("COXAGENT_LLM_BASE_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        let api_key = std::env::var("COXAGENT_LLM_API_KEY").unwrap_or_default();
        Self::new(model, base_url, api_key)
    }

    /// The full chat-completions endpoint for the configured base.
    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// The request body for a single system+user chat completion.
fn chat_payload(model: &str, system_prompt: &str, task_prompt: &str) -> serde_json::Value {
    json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": task_prompt},
        ],
        "stream": false,
    })
}

/// What one completion yields: the assistant text plus token usage.
#[derive(Debug, PartialEq, Eq)]
struct Completion {
    content: String,
    input_tokens: u64,
    output_tokens: u64,
}

/// Parse an OpenAI-compatible `/chat/completions` response body. Tolerates a
/// missing `usage` block (some gateways omit it) but requires at least one
/// choice with assistant content.
fn parse_completion(body: &str) -> Result<Completion, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("bad JSON: {e}"))?;
    // A gateway error comes back as {"error": {"message": ...}} with 200 in
    // some proxies — surface it rather than reporting "no choices".
    if let Some(msg) = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(serde_json::Value::as_str)
    {
        return Err(format!("gateway error: {msg}"));
    }
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .ok_or("response had no choices[0].message.content")?
        .to_owned();
    let usage = v.get("usage");
    let tok = |field: &str| -> u64 {
        usage
            .and_then(|u| u.get(field))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    Ok(Completion {
        content,
        input_tokens: tok("prompt_tokens"),
        output_tokens: tok("completion_tokens"),
    })
}

#[async_trait]
impl AgentEnginePort for HttpEngine {
    fn id(&self) -> &'static str {
        "http"
    }

    fn sandbox_status(&self) -> SandboxStatus {
        // No subprocess and no local file writes, so there is nothing to
        // confine — confinement was never requested.
        SandboxStatus::NotRequested
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        if self.api_key.trim().is_empty() {
            return Err(PortError::Backend(
                "http engine: COXAGENT_LLM_API_KEY is not set".to_owned(),
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(request.timeout.max(Duration::from_secs(30)))
            .build()
            .map_err(|e| PortError::Backend(format!("http engine client: {e}")))?;
        let payload = chat_payload(&self.model, &request.system_prompt, &request.task_prompt);
        let resp = client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| PortError::Backend(format!("http engine request: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| PortError::Backend(format!("http engine read body: {e}")))?;
        if !status.is_success() {
            // Bodies can carry the key back in echoed request context on some
            // gateways; keep only the first line and cap it.
            let detail: String = body.lines().next().unwrap_or("").chars().take(200).collect();
            return Err(PortError::Backend(format!(
                "http engine: gateway returned {status}: {detail}"
            )));
        }
        let completion =
            parse_completion(&body).map_err(|e| PortError::Backend(format!("http engine: {e}")))?;
        Ok(AgentOutcome {
            stdout: completion.content,
            stderr: String::new(),
            exit_code: Some(0),
            usage: Some(Usage {
                input_tokens: completion.input_tokens,
                output_tokens: completion.output_tokens,
                cost_usd: 0.0,
            }),
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::NotRequested,
            engine: "http".to_owned(),
            model: self.model.clone(),
            attempts: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_appends_chat_completions_and_tolerates_trailing_slash() {
        let e = HttpEngine::new("m", "https://host/v1", "k");
        assert_eq!(e.endpoint(), "https://host/v1/chat/completions");
        let e2 = HttpEngine::new("m", "https://host/v1/", "k");
        assert_eq!(e2.endpoint(), "https://host/v1/chat/completions");
    }

    #[test]
    fn payload_carries_system_and_user_messages() {
        let p = chat_payload("gpt-x", "be terse", "say hi");
        assert_eq!(p["model"], "gpt-x");
        assert_eq!(p["messages"][0]["role"], "system");
        assert_eq!(p["messages"][0]["content"], "be terse");
        assert_eq!(p["messages"][1]["role"], "user");
        assert_eq!(p["messages"][1]["content"], "say hi");
        assert_eq!(p["stream"], false);
    }

    #[test]
    fn parses_a_normal_completion_with_usage() {
        let body = r#"{
            "choices": [{"message": {"role": "assistant", "content": "hello there"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 3}
        }"#;
        let c = parse_completion(body).unwrap();
        assert_eq!(c.content, "hello there");
        assert_eq!(c.input_tokens, 12);
        assert_eq!(c.output_tokens, 3);
    }

    #[test]
    fn tolerates_a_missing_usage_block() {
        let body = r#"{"choices": [{"message": {"content": "ok"}}]}"#;
        let c = parse_completion(body).unwrap();
        assert_eq!(c.content, "ok");
        assert_eq!(c.input_tokens, 0);
        assert_eq!(c.output_tokens, 0);
    }

    #[test]
    fn surfaces_a_gateway_error_body() {
        let body = r#"{"error": {"message": "model not found"}}"#;
        let err = parse_completion(body).unwrap_err();
        assert!(err.contains("model not found"), "{err}");
    }

    #[test]
    fn rejects_a_response_with_no_choices() {
        let err = parse_completion(r#"{"choices": []}"#).unwrap_err();
        assert!(err.contains("no choices"), "{err}");
    }
}
