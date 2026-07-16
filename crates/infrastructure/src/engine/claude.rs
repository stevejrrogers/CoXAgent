//! `ClaudeEngine` — runs the `claude` CLI (Claude Code) in headless print mode
//! as the agent engine. The second engine behind `AgentEnginePort`, proving the
//! Strategy boundary: swapping opencode for claude touches no use case.
//!
//! Invocation: `claude -p <prompt> --model <model> --dangerously-skip-permissions`
//! with the working directory set to the managed codebase.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest, Usage};
use coxagent_application::PortError;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// The live-log file for a run: `<workspace>/logs/live/<role>.log`, derived from
/// the codebase work-dir (`<workspace>/codebase`). Streamed to during the run so
/// the UI can tail it live.
fn live_path(work_dir: &Path, role: &str) -> Option<PathBuf> {
    let dir = work_dir.parent()?.join("logs").join("live");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{role}.log")))
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

/// Adapter over the `claude` binary for one model selection.
pub struct ClaudeEngine {
    /// Model alias or full name (e.g. `sonnet`, `opus`, `claude-sonnet-4-6`).
    model: String,
    binary: String,
}

impl ClaudeEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: "claude".to_owned(),
        }
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }
}

#[async_trait]
impl AgentEnginePort for ClaudeEngine {
    fn id(&self) -> &'static str {
        "claude"
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let prompt = format!(
            "{}\n\n---\n\n{}",
            request.system_prompt, request.task_prompt
        );

        let mut cmd = Command::new(&self.binary);
        cmd.arg("-p")
            .arg(prompt)
            .arg("--model")
            .arg(&self.model)
            // Stream-json + verbose emits every step (assistant text, tool_use,
            // tool_result) plus a final result carrying usage/cost — so we can
            // show the detailed work log, not just the answer.
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--dangerously-skip-permissions")
            .current_dir(&request.work_dir)
            // No stdin: claude -p otherwise waits for piped input and warns/exits.
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        crate::engine::apply_shim_path(&mut cmd);

        // Stream stdout line-by-line: render each event to the live log as it
        // arrives (so the UI can tail it), while accumulating the raw NDJSON for
        // the final parse. Reset the live file at the start of the run.
        let role = crate::engine::role_key(request.role);
        let live = live_path(&request.work_dir, &role);
        if let Some(p) = &live {
            let _ = std::fs::write(p, format!("# {role} — live @ run start\n"));
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| PortError::Backend(format!("spawn claude: {e}")))?;
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
                .map_err(|e| PortError::Backend(format!("read claude: {e}")))?
            {
                raw.push_str(&line);
                raw.push('\n');
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
                .map_err(|e| PortError::Backend(format!("claude wait: {e}")))?;
            Ok::<_, PortError>((raw, status))
        };
        let (raw, status) = tokio::time::timeout(request.timeout, read)
            .await
            .map_err(|_| PortError::Backend("claude timed out".to_owned()))??;
        let stderr = err_task.await.unwrap_or_default();

        // Prefer the streamed events; fall back to the old single-object JSON.
        let (stdout, usage, trace) = if let Some(v) = parse_stream(&raw) {
            v
        } else {
            let (s, u) = parse_json_output(&raw);
            (s, u, String::new())
        };
        if let Some(p) = &live {
            append_live(p, "\n— run finished —");
        }

        Ok(AgentOutcome {
            stdout,
            stderr,
            exit_code: status.code(),
            usage,
            trace,
        })
    }
}

/// Render one stream event (assistant text / tool_use / tool_result) into a
/// readable line for the trace and the live log. Empty for non-visible events.
fn render_event(v: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    match v.get("type").and_then(serde_json::Value::as_str) {
        Some("assistant") => {
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(serde_json::Value::as_array)
            {
                for block in content {
                    match block.get("type").and_then(serde_json::Value::as_str) {
                        Some("text") => {
                            let t = block
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("")
                                .trim();
                            if !t.is_empty() {
                                let _ = write!(out, "\n💬 {t}");
                            }
                        }
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("tool");
                            let input: String = block
                                .get("input")
                                .map(serde_json::Value::to_string)
                                .unwrap_or_default()
                                .chars()
                                .take(160)
                                .collect();
                            let _ = write!(out, "\n🔧 {name}({input})");
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("user") => {
            if let Some(content) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(serde_json::Value::as_array)
            {
                for block in content {
                    if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
                    {
                        let n = block
                            .get("content")
                            .map(std::string::ToString::to_string)
                            .unwrap_or_default()
                            .chars()
                            .count();
                        let _ = write!(out, "\n   ↳ result ({n} chars)");
                    }
                }
            }
        }
        _ => {}
    }
    out.trim_start_matches('\n').to_owned()
}

/// Parse `claude --output-format stream-json` (NDJSON): return the final result
/// text, usage, and a rendered step-by-step work log. `None` if no result event
/// is present (so the caller falls back to plain-JSON parsing).
fn parse_stream(raw: &str) -> Option<(String, Option<Usage>, String)> {
    let mut result: Option<String> = None;
    let mut usage: Option<Usage> = None;
    let mut trace = String::new();
    for line in raw.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(serde_json::Value::as_str) == Some("result") {
            result = v
                .get("result")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            usage = parse_usage(&v);
            continue;
        }
        let step = render_event(&v);
        if !step.is_empty() {
            trace.push_str(&step);
            trace.push('\n');
        }
    }
    result.map(|r| (r, usage, trace.trim().to_owned()))
}

/// Extract usage/cost from a stream `result` event.
fn parse_usage(v: &serde_json::Value) -> Option<Usage> {
    let u = v.get("usage")?;
    Some(Usage {
        input_tokens: u
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        output_tokens: u
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cost_usd: v
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    })
}

/// Parse `claude --output-format json`: `{ result, total_cost_usd, usage:
/// {input_tokens, output_tokens} }`. Falls back to the raw text if it isn't the
/// expected JSON (e.g. an error line).
fn parse_json_output(raw: &str) -> (String, Option<Usage>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
        return (raw.to_owned(), None);
    };
    let text = v
        .get("result")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(raw)
        .to_owned();
    let usage = v.get("usage").map(|u| Usage {
        input_tokens: u
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        output_tokens: u
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        cost_usd: v
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    });
    (text, usage)
}
