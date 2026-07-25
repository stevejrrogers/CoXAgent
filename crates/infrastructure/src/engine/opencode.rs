//! `OpencodeEngine` — runs the `opencode` CLI as the agent engine.
//!
//! Mirrors the reference workflow: `opencode run --model provider/model
//! --dangerously-skip-permissions --dir <workdir> --format json <prompt>`.
//! Streams NDJSON events line-by-line so the dashboard's live log updates
//! in real-time. Aggregates `tokens` and `cost` from `step_finish` events.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::PortError;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// Adapter over the `opencode` binary for one engine/model selection.
pub struct OpencodeEngine {
    /// Full `provider/model` string passed to `--model`.
    model: String,
    /// Binary name or path (defaults to `opencode`; overridable for tests).
    binary: String,
}

impl OpencodeEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: "opencode".to_owned(),
        }
    }

    /// Override the binary path (used by discovery / tests).
    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }
}

/// The live-log file for a run: `<workspace>/logs/live/<role>.log`, derived
/// from the codebase work-dir (`<workspace>/codebase`). Same layout as the
/// claude engine so the dashboard's `agent-log` endpoint finds it.
fn live_path(work_dir: &Path, role: &str) -> Option<PathBuf> {
    let dir = work_dir.parent()?.join("logs").join("live");
    std::fs::create_dir_all(&dir).ok()?;
    let suffix = std::env::var("COXAGENT_OPERATOR")
        .ok()
        .map(|o| {
            o.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .map_or_else(String::new, |s| format!("__{s}"));
    Some(dir.join(format!("{role}{suffix}.log")))
}

fn append_live(path: &Path, line: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{}", line.trim_end());
    }
}

#[async_trait]
impl AgentEnginePort for OpencodeEngine {
    fn id(&self) -> &'static str {
        "opencode"
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let prompt = format!(
            "{}\n\n---\n\n{}",
            request.system_prompt, request.task_prompt
        );

        let role = crate::engine::role_key(request.role);
        let live = live_path(&request.work_dir, &role);
        if let Some(p) = &live {
            let _ = std::fs::write(p, format!("# {role} — live @ run start\n"));
        }

        let mut cmd = Command::new(&self.binary);
        cmd.arg("run")
            .arg("--model")
            .arg(&self.model)
            .arg("--dangerously-skip-permissions")
            .arg("--dir")
            .arg(&request.work_dir)
            .arg("--format")
            .arg("json")
            .arg(prompt)
            .current_dir(&request.work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Clear env vars that tell opencode it's running inside a parent
        // session — inherited when the hub itself is launched from inside an
        // opencode session. Without this, the child tries to attach to the
        // parent instead of starting fresh and fails with "Not logged in".
        cmd.env_remove("OPENCODE");
        cmd.env_remove("OPENCODE_PID");
        crate::engine::apply_shim_path(&mut cmd);

        let mut child = cmd
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn opencode: {e}")))?;
        let out = child
            .stdout
            .take()
            .ok_or_else(|| PortError::Backend("no stdout".to_owned()))?;
        let mut err = child.stderr.take();
        let err_task = tokio::spawn(async move {
            let mut s = String::new();
            if let Some(e) = err.as_mut() {
                let _ = BufReader::new(e).read_to_string(&mut s).await;
            }
            s
        });

        let live2 = live.clone();
        let read = async move {
            let mut raw = String::new();
            let mut lines = BufReader::new(out).lines();
            while let Some(line) = lines
                .next_line()
                .await
                .map_err(|e| PortError::Backend(format!("read opencode: {e}")))?
            {
                raw.push_str(&line);
                raw.push('\n');
                // Stream visible text to the live log in real-time.
                if let Some(p) = &live2 {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                        let step = render_event(&v);
                        if !step.is_empty() {
                            append_live(p, &step);
                        }
                    }
                }
            }
            let status = child
                .wait()
                .await
                .map_err(|e| PortError::Backend(format!("opencode wait: {e}")))?;
            Ok::<_, PortError>((raw, status))
        };

        let (raw, status) = tokio::time::timeout(request.timeout, read)
            .await
            .map_err(|_| PortError::Backend("opencode timed out".to_owned()))??;
        let stderr = err_task.await.unwrap_or_default();

        if let Some(p) = &live {
            append_live(p, "\n— run finished —");
        }

        let (text, usage) = parse_json_stream(&raw);

        Ok(AgentOutcome {
            stdout: text,
            stderr,
            exit_code: status.code(),
            usage: Some(usage),
            trace: String::new(),
        })
    }
}

/// Render one NDJSON event into a readable line for the live log.
/// Empty for non-visible events (step_start, etc.).
fn render_event(v: &serde_json::Value) -> String {
    match v.get("type").and_then(serde_json::Value::as_str) {
        Some("text") => v
            .pointer("/part/text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        _ => String::new(),
    }
}

/// Parse the newline-delimited JSON event stream from `opencode run --format
/// json`. Aggregates `tokens` and `cost` from every `step_finish` event and
/// concatenates `text` parts into the final stdout the caller expects.
///
/// When the output is not valid JSON events (e.g. an error message), falls back
/// to returning the raw text with a rough token estimate.
fn parse_json_stream(raw: &str) -> (String, coxagent_application::ports::outbound::engine::Usage) {
    use coxagent_application::ports::outbound::engine::Usage;

    let mut text_parts: Vec<String> = Vec::new();
    let mut input_tokens: u64 = 0;
    let mut output_tokens: u64 = 0;
    let mut cost_usd: f64 = 0.0;
    let mut saw_json = false;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        saw_json = true;
        match v.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(t) = v.pointer("/part/text").and_then(serde_json::Value::as_str) {
                    text_parts.push(t.to_owned());
                }
            }
            Some("step_finish") => {
                if let Some(tokens) = v.pointer("/part/tokens") {
                    input_tokens = input_tokens.saturating_add(
                        tokens
                            .get("input")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                    );
                    output_tokens = output_tokens.saturating_add(
                        tokens
                            .get("output")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                    );
                }
                if let Some(c) = v.pointer("/part/cost").and_then(serde_json::Value::as_f64) {
                    cost_usd += c;
                }
            }
            _ => {}
        }
    }

    if !saw_json {
        return (
            raw.to_owned(),
            Usage {
                input_tokens: estimate_tokens_raw(raw.len()),
                output_tokens: estimate_tokens_raw(raw.len()),
                cost_usd: 0.0,
            },
        );
    }

    let text = text_parts.join("");
    let usage = Usage {
        input_tokens,
        output_tokens,
        cost_usd,
    };
    (text, usage)
}

/// Rough token estimate (~3.8 chars per token).
fn estimate_tokens_raw(len: usize) -> u64 {
    if len == 0 {
        return 0;
    }
    (len as f64 / 3.8).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_stream_aggregates_tokens_and_text() {
        let stream = r#"{"type":"step_start","part":{"type":"step-start"}}
{"type":"text","part":{"text":"Hello"}}
{"type":"step_finish","part":{"tokens":{"input":100,"output":5},"cost":0.01}}
{"type":"text","part":{"text":" world"}}
{"type":"step_finish","part":{"tokens":{"input":50,"output":3},"cost":0.005}}"#;
        let (text, usage) = parse_json_stream(stream);
        assert_eq!(text, "Hello world");
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 8);
        assert!((usage.cost_usd - 0.015).abs() < 1e-9);
    }

    #[test]
    fn parse_json_stream_falls_back_on_plain_text() {
        let (text, usage) = parse_json_stream("not json at all");
        assert_eq!(text, "not json at all");
        assert!(usage.input_tokens > 0);
        assert_eq!(usage.cost_usd, 0.0);
    }

    #[test]
    fn render_event_extracts_text() {
        let v = serde_json::json!({"type":"text","part":{"text":"hello world"}});
        assert_eq!(render_event(&v), "hello world");
    }

    #[test]
    fn render_event_empty_for_non_text() {
        let v = serde_json::json!({"type":"step_start","part":{}});
        assert_eq!(render_event(&v), "");
    }
}
