//! Adapter over the GitHub Copilot CLI (`copilot`) for one model selection.
//!
//! Copilot's agentic CLI is close to claude/opencode: `-p` runs it
//! non-interactively, `--model auto` lets it route to a model itself, and
//! `--output-format json` emits JSONL — one event object per line. We drive it
//! with `--allow-all-tools` (required for non-interactive use) and `--add-dir`
//! the workspace so it can read and edit the project.
//!
//! Parsing: the answer is the concatenation of `assistant.message` `content`;
//! tool calls and the final answer become the work-log trace; `outputTokens`
//! per message is summed for the Cost tab. Copilot bills "premium requests",
//! not USD or input tokens, so `input_tokens`/`cost_usd` stay 0 — the token
//! count we can report honestly, the rest we do not invent.

use async_trait::async_trait;
use coxagent_application::ports::outbound::engine::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, Usage,
};
use coxagent_application::PortError;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

/// Adapter over the `copilot` binary for one model selection.
pub struct CopilotEngine {
    model: String,
    binary: String,
    sandbox: bool,
}

impl CopilotEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        let m = model.into();
        Self {
            // Empty/"default" means let Copilot route: `auto`.
            model: if m.trim().is_empty() || m == "default" {
                "auto".to_owned()
            } else {
                m
            },
            binary: crate::engine::resolve_engine_binary("copilot"),
            sandbox: false,
        }
    }

    /// Confine agent file writes to the workspace + tool caches (macOS/Linux).
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Spawn `cmd`, stream its JSONL stdout to the live log line-by-line, and
    /// return the raw stdout, exit code, and stderr. Same shape as the opencode
    /// adapter's exec so both feed the dashboard's live view identically.
    async fn exec(
        &self,
        mut cmd: Command,
        live: Option<PathBuf>,
        timeout: std::time::Duration,
        sandbox: SandboxStatus,
    ) -> Result<StreamedRun, PortError> {
        // `sandbox` is re-bound to what the OS ACTUALLY applied: a Seatbelt
        // profile it refused to apply leaves the outcome reporting `Refused`,
        // never a confinement that never happened (COX-B016).
        let (mut child, sandbox) = crate::proc::spawn_confined(&mut cmd, sandbox)
            .await
            .map_err(|e| PortError::Backend(format!("spawn copilot: {e}")))?;
        let out = child
            .stdout
            .take()
            .ok_or_else(|| PortError::Backend("no stdout".to_owned()))?;
        let mut err = child.stderr.take();
        let child_pid = child.id();
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
                .map_err(|e| PortError::Backend(format!("read copilot: {e}")))?
            {
                raw.push_str(&line);
                raw.push('\n');
                if let Some(p) = &live2 {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                        let step = render_event(&v);
                        if !step.is_empty() {
                            crate::engine::live::append_live(p, &step);
                        }
                    }
                }
            }
            let status = child
                .wait()
                .await
                .map_err(|e| PortError::Backend(format!("copilot wait: {e}")))?;
            Ok::<_, PortError>((raw, status))
        };

        let Ok(read) = tokio::time::timeout(timeout, read).await else {
            if let Some(pid) = child_pid {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("copilot timed out".to_owned()));
        };
        let (raw, status) = read?;
        let stderr = err_task.await.unwrap_or_default();
        if let Some(p) = &live {
            crate::engine::live::append_live(p, "\n— run finished —");
        }
        Ok(StreamedRun {
            stdout: raw,
            code: status.code(),
            stderr,
            sandbox,
        })
    }
}

/// One streamed Copilot run: its raw stdout, exit code, stderr — and the write
/// confinement that ACTUALLY applied, which `spawn_confined` downgrades when
/// the OS refused to apply the profile (COX-B016). Carrying it out of `exec`
/// is what stops the caller stamping the REQUESTED confinement onto an outcome
/// the sandbox never produced.
struct StreamedRun {
    stdout: String,
    code: Option<i32>,
    stderr: String,
    sandbox: SandboxStatus,
}

/// Render ONE Copilot JSONL event as a work-log line for the live view — the
/// streaming twin of `parse_jsonl`'s trace (which needs the whole stream to
/// settle the answer and token count). Unknown events render empty.
fn render_event(v: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let ty = v.get("type").and_then(serde_json::Value::as_str).unwrap_or("");
    let data = v.get("data");
    let mut out = String::new();
    match ty {
        "session.auto_mode_resolved" => {
            if let Some(m) = data
                .and_then(|d| d.get("chosenModel"))
                .and_then(serde_json::Value::as_str)
            {
                let _ = write!(out, "# model: {m} (auto)");
            }
        }
        "assistant.message" => {
            if let Some(d) = data {
                if let Some(c) = d.get("content").and_then(serde_json::Value::as_str) {
                    let c = c.trim();
                    if !c.is_empty() {
                        let _ = write!(out, "💬 {c}");
                    }
                }
                if let Some(reqs) = d.get("toolRequests").and_then(serde_json::Value::as_array) {
                    for r in reqs {
                        let name = r
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("tool");
                        let arg = r
                            .get("arguments")
                            .map(std::string::ToString::to_string)
                            .unwrap_or_default();
                        let arg: String = arg.chars().take(160).collect();
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        let _ = write!(out, "🔧 {name}({arg})");
                    }
                }
            }
        }
        "session.error" => {
            let msg = data
                .and_then(|d| d.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("session error");
            let _ = write!(out, "✗ {msg}");
        }
        "result" => {
            let _ = write!(out, "— run finished");
        }
        _ => {}
    }
    out
}

#[async_trait]
impl AgentEnginePort for CopilotEngine {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn sandbox_status(&self) -> SandboxStatus {
        crate::proc::sandbox_status(self.sandbox)
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let prompt = format!("{}\n\n---\n\n{}", request.system_prompt, request.task_prompt);
        let work = request.work_dir.display().to_string();

        let (mut cmd, sandbox) =
            crate::proc::agent_command(&self.binary, &request.work_dir, self.sandbox);
        cmd.arg("-p")
            .arg(&prompt)
            .arg("--model")
            .arg(&self.model)
            .arg("--output-format")
            .arg("json")
            .arg("--allow-all-tools")
            .arg("--add-dir")
            .arg(&work)
            .arg("--log-level")
            .arg("none")
            .arg("--no-color")
            .current_dir(&request.work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::engine::apply_shim_path(&mut cmd);

        // Stream the JSONL to the live log as it arrives, so the dashboard shows
        // this agent working in real time instead of a blank pane until the end.
        let role = crate::engine::role_key(request.role);
        let live =
            crate::engine::live::live_path(&request.work_dir, &role, request.label.as_deref());
        // Truncate to a fresh header at run start (as claude/opencode do), so the
        // live view shows THIS run — not a wall of every past run's "run
        // finished" appended to the same file.
        if let Some(p) = &live {
            let _ = std::fs::write(p, format!("# {role} — live @ run start\n"));
        }
        let StreamedRun {
            stdout,
            code,
            stderr,
            sandbox,
        } = self.exec(cmd, live, request.timeout, sandbox).await?;
        let parsed = parse_jsonl(&stdout);
        // A session.error means the run produced nothing useful even though the
        // CLI exits 0 — report it as the failure it is, with the message in
        // stderr so the failover layer can recognize a quota wall and move on.
        let (code, stderr) = match &parsed.error {
            Some(e) => (Some(1), format!("{e}\n{stderr}")),
            None => (code, stderr),
        };
        Ok(AgentOutcome {
            stdout: if parsed.answer.is_empty() {
                stdout.clone()
            } else {
                parsed.answer
            },
            stderr,
            exit_code: code,
            usage: Some(Usage {
                input_tokens: 0,
                output_tokens: parsed.output_tokens,
                cost_usd: 0.0,
            }),
            trace: parsed.trace,
            session_id: parsed.session_id,
            sandbox,
            engine: "copilot".to_owned(),
        })
    }

    /// Resume a prior Copilot session by id (`--resume=<id>`) with a follow-up
    /// prompt, so the agent keeps the context it already built on this ticket
    /// instead of re-reading the code cold. No system prompt: the resumed session
    /// already carries the role framing from its first turn.
    async fn resume_run(
        &self,
        role: coxagent_domain::Role,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        let work = work_dir.display().to_string();
        let (mut cmd, sandbox) = crate::proc::agent_command(&self.binary, work_dir, self.sandbox);
        cmd.arg("-p")
            .arg(follow_up)
            .arg(format!("--resume={session_id}"))
            .arg("--model")
            .arg(&self.model)
            .arg("--output-format")
            .arg("json")
            .arg("--allow-all-tools")
            .arg("--add-dir")
            .arg(&work)
            .arg("--log-level")
            .arg("none")
            .arg("--no-color")
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::engine::apply_shim_path(&mut cmd);

        let live = crate::engine::live::live_path(work_dir, &crate::engine::role_key(role), None);
        let StreamedRun {
            stdout,
            code,
            stderr,
            sandbox,
        } = self.exec(cmd, live, timeout, sandbox).await?;
        let parsed = parse_jsonl(&stdout);
        let (code, stderr) = match &parsed.error {
            Some(e) => (Some(1), format!("{e}\n{stderr}")),
            None => (code, stderr),
        };
        Ok(AgentOutcome {
            stdout: if parsed.answer.is_empty() {
                stdout.clone()
            } else {
                parsed.answer
            },
            stderr,
            exit_code: code,
            usage: Some(Usage {
                input_tokens: 0,
                output_tokens: parsed.output_tokens,
                cost_usd: 0.0,
            }),
            trace: parsed.trace,
            session_id: parsed.session_id.or_else(|| Some(session_id.to_owned())),
            sandbox,
            engine: "copilot".to_owned(),
        })
    }
}

/// What we pull out of Copilot's JSONL stream.
struct Parsed {
    answer: String,
    trace: String,
    output_tokens: u64,
    session_id: Option<String>,
    /// A `session.error` the CLI reported (quota exhaustion, auth failure…).
    /// Copilot exits 0 even on these, so without surfacing it the run looked
    /// like a SUCCESS with empty output — no failover fired and every
    /// copilot-routed role silently produced nothing for hours.
    error: Option<String>,
}

/// Parse Copilot's `--output-format json` (JSONL). Falls back gracefully: any
/// line that is not valid JSON, or an unknown event type, is ignored — the
/// answer and token count come only from the events we understand.
fn parse_jsonl(raw: &str) -> Parsed {
    use std::fmt::Write as _;
    let mut answer = String::new();
    let mut trace = String::new();
    let mut output_tokens: u64 = 0;
    let mut session_id = None;
    let mut error = None;

    for line in raw.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ty = v.get("type").and_then(serde_json::Value::as_str).unwrap_or("");
        let data = v.get("data");
        match ty {
            // `auto` resolved to a concrete model — note it at the top of the log.
            "session.auto_mode_resolved" => {
                if let Some(m) = data
                    .and_then(|d| d.get("chosenModel"))
                    .and_then(serde_json::Value::as_str)
                {
                    let _ = writeln!(trace, "# model: {m} (auto)");
                }
            }
            // The assistant's turn: its text is the answer, its tool requests
            // are steps. Deltas are skipped — only the settled message counts.
            "assistant.message" => {
                if let Some(d) = data {
                    if let Some(c) = d.get("content").and_then(serde_json::Value::as_str) {
                        let c = c.trim();
                        if !c.is_empty() {
                            if !answer.is_empty() {
                                answer.push('\n');
                            }
                            answer.push_str(c);
                            let _ = writeln!(trace, "💬 {c}");
                        }
                    }
                    output_tokens = output_tokens
                        .saturating_add(d.get("outputTokens").and_then(serde_json::Value::as_u64).unwrap_or(0));
                    if let Some(reqs) = d.get("toolRequests").and_then(serde_json::Value::as_array) {
                        for r in reqs {
                            let name = r.get("name").and_then(serde_json::Value::as_str).unwrap_or("tool");
                            let arg = r
                                .get("arguments")
                                .map(std::string::ToString::to_string)
                                .unwrap_or_default();
                            let arg: String = arg.chars().take(160).collect();
                            let _ = writeln!(trace, "🔧 {name}({arg})");
                        }
                    }
                }
            }
            // The CLI's own failure report (quota exhaustion, auth death…).
            // Captured verbatim: the process still EXITS 0 on these, so this
            // event is the only truth that the run produced nothing.
            "session.error" => {
                let msg = data
                    .and_then(|d| d.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("copilot session error");
                let etype = data
                    .and_then(|d| d.get("errorType"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("error");
                let full = format!("copilot {etype}: {msg}");
                let _ = writeln!(trace, "✗ {full}");
                error = Some(full);
            }
            "result" => {
                session_id = v
                    .get("sessionId")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned);
                let _ = writeln!(trace, "   — run finished");
            }
            _ => {}
        }
    }
    Parsed {
        answer,
        trace: trace.trim_end().to_owned(),
        output_tokens,
        session_id,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_error_is_surfaced_not_swallowed() {
        // The exact shape captured live when the monthly quota ran out: the CLI
        // still EXITS 0, so this event is the only signal the run failed. It
        // must surface as an error (→ failover), never a successful empty run.
        let raw = r#"{"type":"session.error","data":{"errorType":"quota","message":"You have exceeded your monthly quota (Request ID: X)","statusCode":402}}
{"type":"result","sessionId":"s1"}"#;
        let p = parse_jsonl(raw);
        let err = p.error.expect("session.error must be captured");
        assert!(err.contains("quota"), "{err}");
        assert!(err.contains("exceeded your monthly quota"), "{err}");
        // And the failover layer must classify it as a quota wall.
        assert!(crate::engine::failover::is_quota_wall(&err));
    }

    #[test]
    fn empty_or_default_model_becomes_auto() {
        assert_eq!(CopilotEngine::new("").model, "auto");
        assert_eq!(CopilotEngine::new("default").model, "auto");
        assert_eq!(CopilotEngine::new("gpt-5.4-mini").model, "gpt-5.4-mini");
    }

    #[test]
    fn parses_answer_tokens_and_model_from_the_jsonl_stream() {
        let raw = concat!(
            r#"{"type":"session.auto_mode_resolved","data":{"chosenModel":"claude-haiku-4.5"}}"#,
            "\n",
            r#"{"type":"assistant.message","data":{"content":"PONG","outputTokens":45,"toolRequests":[]}}"#,
            "\n",
            r#"{"type":"result","sessionId":"ea7","exitCode":0,"usage":{"premiumRequests":0.33}}"#,
            "\n",
        );
        let p = parse_jsonl(raw);
        assert_eq!(p.answer, "PONG");
        assert_eq!(p.output_tokens, 45);
        assert_eq!(p.session_id.as_deref(), Some("ea7"));
        assert!(p.trace.contains("model: claude-haiku-4.5 (auto)"));
        assert!(p.trace.contains("💬 PONG"));
        assert!(p.trace.contains("run finished"));
    }

    #[test]
    fn a_tool_call_becomes_a_trace_step() {
        let raw = r#"{"type":"assistant.message","data":{"content":"","outputTokens":10,"toolRequests":[{"name":"bash","arguments":{"command":"ls"}}]}}"#;
        let p = parse_jsonl(raw);
        assert!(p.trace.contains("🔧 bash("), "{}", p.trace);
        assert!(p.answer.is_empty());
    }

    #[test]
    fn sandbox_status_reflects_the_setting() {
        assert_eq!(
            CopilotEngine::new("auto").sandbox_status(),
            SandboxStatus::NotRequested
        );
        assert_ne!(
            CopilotEngine::new("auto").with_sandbox(true).sandbox_status(),
            SandboxStatus::NotRequested
        );
    }
}
