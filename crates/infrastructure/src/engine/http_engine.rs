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
use coxagent_domain::Role;
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

    /// POST one chat-completions request and parse the reply. The single HTTP
    /// path shared by the text completion and every turn of the tool loop.
    async fn post_chat(
        &self,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<Completion, PortError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| PortError::Backend(format!("http engine client: {e}")))?;
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
            // Bodies can echo request context (incl. the key) on some gateways;
            // keep only the first line and cap it.
            let detail: String = body.lines().next().unwrap_or("").chars().take(200).collect();
            return Err(PortError::Backend(format!(
                "http engine: gateway returned {status}: {detail}"
            )));
        }
        parse_completion(&body).map_err(|e| PortError::Backend(format!("http engine: {e}")))
    }

    /// Single-shot text completion — the engine's behaviour for text-producing
    /// roles (chat/planning/review/docs).
    async fn run_text(&self, req: &AgentRequest, timeout: Duration) -> Result<AgentOutcome, PortError> {
        let payload = chat_payload(&self.model, &req.system_prompt, &req.task_prompt);
        let c = self.post_chat(payload, timeout).await?;
        Ok(self.outcome(c.content, c.input_tokens, c.output_tokens, String::new(), Some(0)))
    }

    /// Agentic coding run — drive the chat↔tool loop so the model inspects and
    /// edits the workspace, then summarises.
    async fn run_coding(&self, req: &AgentRequest, timeout: Duration) -> Result<AgentOutcome, PortError> {
        let chat = |messages: Vec<serde_json::Value>| {
            let payload = messages_payload(&self.model, &messages);
            async move {
                self.post_chat(payload, timeout)
                    .await
                    .map(|c| crate::engine::http_loop::TurnReply {
                        content: c.content,
                        input_tokens: c.input_tokens,
                        output_tokens: c.output_tokens,
                    })
                    .map_err(|e| e.to_string())
            }
        };
        let out = crate::engine::http_loop::run_tool_loop(
            chat,
            &req.work_dir,
            &req.system_prompt,
            &req.task_prompt,
            MAX_TOOL_ITERS,
        )
        .await;
        // Success for a coding run = finished cleanly; the cycle still decides
        // whether the resulting diff is acceptable. Flag a clean finish that
        // touched no files so a no-op run is not mistaken for real work.
        let exit = if out.done { Some(0) } else { Some(1) };
        let stdout = if out.done && !out.wrote_any {
            format!("{} (no files were modified)", out.summary)
        } else {
            out.summary
        };
        Ok(self.outcome(stdout, out.input_tokens, out.output_tokens, out.transcript, exit))
    }

    /// Assemble an [`AgentOutcome`] from this engine's common fields.
    fn outcome(
        &self,
        stdout: String,
        input_tokens: u64,
        output_tokens: u64,
        trace: String,
        exit_code: Option<i32>,
    ) -> AgentOutcome {
        AgentOutcome {
            stdout,
            stderr: String::new(),
            exit_code,
            usage: Some(Usage { input_tokens, output_tokens, cost_usd: 0.0 }),
            trace,
            session_id: None,
            sandbox: SandboxStatus::NotRequested,
            engine: "http".to_owned(),
            model: self.model.clone(),
            attempts: Vec::new(),
        }
    }
}

/// How many chat↔tool turns a coding run may take before giving up.
const MAX_TOOL_ITERS: usize = 12;

/// The request body for a conversation of `messages`.
fn messages_payload(model: &str, messages: &[serde_json::Value]) -> serde_json::Value {
    json!({ "model": model, "messages": messages, "stream": false })
}

/// The request body for a single system+user chat completion.
fn chat_payload(model: &str, system_prompt: &str, task_prompt: &str) -> serde_json::Value {
    messages_payload(
        model,
        &[
            json!({"role": "system", "content": system_prompt}),
            json!({"role": "user", "content": task_prompt}),
        ],
    )
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
    // Require a message object, but tolerate a null/absent `content`: a gateway
    // returns `content: null` when the model was cut off at max_tokens
    // (finish_reason "length"), tripped a content filter, or is a reasoning
    // model that carries its text elsewhere. Treating that as a hard error
    // aborted the whole run — and killed a coding loop on a single truncated
    // turn — when the gateway had in fact succeeded. Fall back to empty content
    // and annotate it with finish_reason so an empty reply stays diagnosable.
    let message = v
        .pointer("/choices/0/message")
        .ok_or("response had no choices[0].message")?;
    let content = message.get("content").and_then(serde_json::Value::as_str).map_or_else(
        || {
            let reason = v
                .pointer("/choices/0/finish_reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            format!("(empty response from gateway; finish_reason: {reason})")
        },
        str::to_owned,
    );
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
        let timeout = request.timeout.max(Duration::from_secs(30));
        // Coding roles get the agentic tool loop (inspect + edit the workspace);
        // every other role gets a single text completion.
        match request.role {
            Role::DevBug | Role::DevFeature => self.run_coding(&request, timeout).await,
            _ => self.run_text(&request, timeout).await,
        }
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

    #[test]
    fn tolerates_null_content_from_a_truncated_turn() {
        // Gateway cut the model off at max_tokens: content is null, not a hard
        // error. Must parse to a non-empty, diagnosable placeholder carrying the
        // finish_reason — never abort the run.
        let body = r#"{"choices": [{"message": {"role": "assistant", "content": null}, "finish_reason": "length"}]}"#;
        let c = parse_completion(body).unwrap();
        assert!(c.content.contains("length"), "{}", c.content);
        assert!(c.content.contains("empty response"), "{}", c.content);
    }

    #[test]
    fn tolerates_absent_content_field() {
        let body = r#"{"choices": [{"message": {"role": "assistant"}}]}"#;
        let c = parse_completion(body).unwrap();
        assert!(c.content.contains("empty response"), "{}", c.content);
    }

    #[test]
    fn still_errors_when_the_message_object_is_missing() {
        let err = parse_completion(r#"{"choices": [{"finish_reason": "stop"}]}"#).unwrap_err();
        assert!(err.contains("no choices[0].message"), "{err}");
    }
}
