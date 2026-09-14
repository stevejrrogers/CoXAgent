//! `ClaudeEngine` — runs the `claude` CLI (Claude Code) in headless print mode
//! as the agent engine. The second engine behind `AgentEnginePort`, proving the
//! Strategy boundary: swapping opencode for claude touches no use case.
//!
//! Invocation: `claude -p <prompt> --model <model> --dangerously-skip-permissions`
//! with the working directory set to the managed codebase.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, Usage,
};
use coxagent_application::PortError;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

// Shared with every streaming engine so all live logs land where the
// dashboard reads them (including the per-slot-worktree hop) — see engine::live.
use crate::engine::live::{append_live, live_path};

/// Adapter over the `claude` binary for one model selection.
pub struct ClaudeEngine {
    /// Model alias or full name (e.g. `sonnet`, `opus`, `claude-sonnet-4-6`).
    model: String,
    binary: String,
    /// This project's CoXAgent MCP endpoint, when reachable — see [`crate::engine::McpAccess`].
    mcp: Option<crate::engine::McpAccess>,
    /// Confine file writes to the project workspace (see `proc::agent_command`).
    sandbox: bool,
    /// Retry escalation ladder: models for `escalation_level` 1, 2, … (last
    /// entry repeats). Default `["opus"]` — a failed attempt retries on the
    /// strongest Claude tier.
    escalation: Vec<String>,
}

impl ClaudeEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: crate::engine::resolve_engine_binary("claude"),
            mcp: None,
            escalation: vec!["opus".to_owned()],
            sandbox: false,
        }
    }

    /// Confine agent file writes to the workspace + tool caches (macOS).
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Override the retry escalation ladder (empty keeps the default).
    #[must_use]
    pub fn with_escalation(mut self, ladder: Vec<String>) -> Self {
        if !ladder.is_empty() {
            self.escalation = ladder;
        }
        self
    }

    /// The model for a given escalation level: 0 = the configured model;
    /// n ≥ 1 = ladder rung n (clamped to the last rung).
    fn model_for(&self, level: u8) -> &str {
        if level == 0 || self.escalation.is_empty() {
            return &self.model;
        }
        let idx = usize::from(level - 1).min(self.escalation.len() - 1);
        &self.escalation[idx]
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.binary = binary.into();
        self
    }

    /// Give this engine live access to the project's own code-graph MCP tools.
    #[must_use]
    pub fn with_mcp(mut self, mcp: Option<crate::engine::McpAccess>) -> Self {
        self.mcp = mcp;
        self
    }
}

/// Build the `--mcp-config` JSON blob wiring a single HTTP MCP server named
/// `coxagent` at `mcp.url`, with a bearer header when the hub has auth.
fn mcp_config_json(mcp: &crate::engine::McpAccess) -> String {
    let mut server = serde_json::json!({ "type": "http", "url": mcp.url });
    if let Some(token) = &mcp.token {
        server["headers"] = serde_json::json!({ "Authorization": format!("Bearer {token}") });
    }
    serde_json::json!({ "mcpServers": { "coxagent": server } }).to_string()
}

/// A `--mcp-config` file on disk, deleted on drop. `claude` accepts either
/// inline JSON or a file path for this flag — a file is used here (not the
/// inline JSON `mcp_config_json` builds) specifically so the bearer token
/// never appears in `ps`/`/proc/<pid>/cmdline`, which any local user on a
/// shared machine can read. Written with `0600` (owner-only) and placed
/// outside the managed codebase so it's never at risk of `git add -A`.
struct McpConfigFile(PathBuf);

impl McpConfigFile {
    fn write(mcp: &crate::engine::McpAccess) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join("coxagent-mcp-config");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}-{}.json", std::process::id(), fastrand_hex()));
        std::fs::write(&path, mcp_config_json(mcp))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self(path))
    }
}

impl Drop for McpConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A short random hex suffix so two runs starting in the same process/PID
/// tick (or after a PID wraps) never collide on the same config file path.
/// Not a security boundary — the directory + `0600` perms are.
fn fastrand_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    format!("{nanos:x}")
}

/// A short system-prompt addendum telling the agent its project id (needed on
/// every MCP tool call) and nudging it toward the code-graph tools before
/// blind exploration.
fn mcp_prompt_hint(mcp: &crate::engine::McpAccess) -> String {
    format!(
        "\n\nThis project's id is `{}`. You have MCP tools `search_symbols` and \
         `symbol_refs` (project=\"{}\") backed by the native code graph — use \
         them to locate code and check blast radius before reading files blind.",
        mcp.project, mcp.project
    )
}

#[async_trait]
impl AgentEnginePort for ClaudeEngine {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn sandbox_status(&self) -> SandboxStatus {
        crate::proc::sandbox_status(self.sandbox)
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        // nice(+10) + optional write-confinement (see proc::agent_command) —
        // same as resume_run, so a sandboxed run is actually sandboxed on its
        // FIRST call, not just on follow-ups.
        let (mut cmd, sandbox) =
            crate::proc::agent_command(&self.binary, &request.work_dir, self.sandbox);
        // The role/system text goes through --append-system-prompt, NOT folded
        // into -p: it joins the CLI's cached system block, so the stable prefix
        // (base + standards + role) gets prompt-cache READ hits across
        // back-to-back agent runs instead of being re-billed every call. The
        // MCP hint is appended here too — it's static per project, so it rides
        // the same cached block instead of busting the cache like -p would.
        let system_prompt = match &self.mcp {
            Some(mcp) => format!("{}{}", request.system_prompt, mcp_prompt_hint(mcp)),
            None => request.system_prompt.clone(),
        };
        // The model THIS run executes on — the escalation ladder swaps models
        // inside this adapter, so the provenance stamp must be the actual
        // choice, not the configured one (CXA-F257).
        let model = self.model_for(request.escalation_level).to_owned();
        cmd.arg("-p")
            .arg(&request.task_prompt)
            .arg("--append-system-prompt")
            .arg(&system_prompt)
            .arg("--model")
            .arg(&model)
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
        // Held until this function returns (after the child has exited) so the
        // file survives the whole run; deleted on drop either way — including
        // on every early `?` return below.
        let _mcp_config_file = match &self.mcp {
            Some(mcp) => match McpConfigFile::write(mcp) {
                Ok(f) => {
                    cmd.arg("--mcp-config").arg(&f.0);
                    Some(f)
                }
                Err(e) => {
                    tracing::warn!("could not write MCP config file, running without MCP: {e}");
                    None
                }
            },
            None => None,
        };
        crate::engine::apply_shim_path(&mut cmd);
        let role = crate::engine::role_key(request.role);
        // _mcp_config_file must outlive the child process (deleted on drop).
        let outcome = self
            .exec(
                cmd,
                &role,
                request.label.as_deref(),
                &request.work_dir,
                request.timeout,
                sandbox,
                &model,
            )
            .await;
        drop(_mcp_config_file);
        outcome
    }

    async fn resume_run(
        &self,
        _role: coxagent_domain::Role,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        let (mut cmd, sandbox) = crate::proc::agent_command(&self.binary, work_dir, self.sandbox);
        cmd.arg("-p")
            .arg(follow_up)
            .arg("--resume")
            .arg(session_id)
            .arg("--model")
            .arg(&self.model)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--dangerously-skip-permissions")
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        crate::engine::apply_shim_path(&mut cmd);
        self.exec(cmd, "resume", None, work_dir, timeout, sandbox, &self.model)
            .await
    }
}

impl ClaudeEngine {
    /// Spawn `cmd`, stream its NDJSON stdout into the live log, and parse the
    /// final outcome (including the conversation `session_id`, so callers can
    /// continue this run later). Shared by `run` and `resume_run`. `model` is
    /// the model id passed on THIS command's `--model` — stamped onto the
    /// outcome so provenance records the model that actually executed, not
    /// the one config named (CXA-F257).
    #[allow(clippy::too_many_arguments)] // one linear spawn recipe, private
    async fn exec(
        &self,
        mut cmd: Command,
        role: &str,
        label: Option<&str>,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
        sandbox: SandboxStatus,
        model: &str,
    ) -> Result<AgentOutcome, PortError> {
        // Stream stdout line-by-line: render each event to the live log as it
        // arrives (so the UI can tail it), while accumulating the raw NDJSON for
        // the final parse. Reset the live file at the start of the run.
        let live = live_path(work_dir, role, label);
        if let Some(p) = &live {
            let _ = std::fs::write(p, format!("# {role} — live @ run start\n"));
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // The status comes BACK from the spawn: a host whose Seatbelt refused
        // the profile on every attempt downgrades it to `Denied`, so the
        // outcome reports a run that never happened instead of a confined one
        // (COX-B016).
        let (mut child, sandbox) = crate::proc::spawn_confined(&mut cmd, sandbox)
            .await
            .map_err(|e| PortError::Backend(format!("spawn claude: {e}")))?;
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
        let leader_pid = child_pid;
        let Ok(read) = tokio::time::timeout(timeout, read).await else {
            // Reap the WHOLE process tree, not just the CLI: a timed-out
            // agent may have left builds/dev-servers running.
            if let Some(pid) = leader_pid {
                crate::proc::kill_group(pid);
            }
            return Err(PortError::Backend("claude timed out".to_owned()));
        };
        let (raw, status) = read?;
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
            session_id: extract_session(&raw),
            sandbox,
            engine: "claude".to_owned(),
            model: model.to_owned(),
            attempts: Vec::new(),
        })
    }
}

/// First `session_id` seen in the NDJSON stream (the init event carries it) —
/// the handle for `--resume`.
fn extract_session(raw: &str) -> Option<String> {
    raw.lines().find_map(|l| {
        serde_json::from_str::<serde_json::Value>(l.trim())
            .ok()?
            .get("session_id")?
            .as_str()
            .map(str::to_owned)
    })
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
                        let text = tool_result_text(block.get("content"));
                        let _ = write!(out, "\n   ↳ {}", summarize_tool_result(&text));
                        // Show the actual output, not just its shape: a capped
                        // preview the dashboard can reveal on click. "13 lines"
                        // told a reader nothing about WHAT the 13 lines were.
                        for l in preview_lines(&text) {
                            let _ = write!(out, "\n   ┆ {l}");
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out.trim_start_matches('\n').to_owned()
}

/// The plain text of a tool_result block, whether it arrived as a bare string
/// or as an array of `{type:"text", text:…}` parts.
fn tool_result_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One human line for a tool's output, so the work log reads as an outcome
/// ("✓ 220 passed", "clippy clean", "6 matches") instead of "result (396
/// chars)" — the length told a reader nothing. Recognises the tools agents run
/// constantly; anything else shows its first real line, capped.
#[must_use]
pub(crate) fn summarize_tool_result(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return "done (no output)".to_owned();
    }
    let low = t.to_lowercase();
    // cargo test: the "test result:" line is the verdict.
    if let Some(line) = t.lines().find(|l| l.contains("test result:")) {
        if line.contains("FAILED") || line.contains("failed;") && !line.contains("0 failed") {
            // e.g. "test result: FAILED. 2 passed; 1 failed"
            if let Some(fail) = line.split("passed;").nth(1) {
                let n = fail.split("failed").next().unwrap_or("").trim();
                return format!("✗ {n} failed");
            }
            return "✗ tests failed".to_owned();
        }
        if let Some(pass) = line.split("result: ok.").nth(1) {
            let n = pass.split("passed").next().unwrap_or("").trim();
            return format!("✓ {n} passed");
        }
    }
    // Compiler/cargo errors — recognised by the diagnostic SHAPE, not the bare
    // word "error". `git show` of Rust source is full of `.map_err`, `Error`,
    // "error:" in strings; counting those as failures showed "✗ 1 errors" for a
    // clean file dump. Real cargo output carries `error[EXXXX]`, a line that
    // starts with "error:", or "could not compile".
    let compiler_err = low.contains("could not compile")
        || t.contains("error[")
        || t.lines().any(|l| l.trim_start().starts_with("error:"));
    if compiler_err {
        let n = t
            .lines()
            .filter(|l| {
                let l = l.trim_start();
                l.starts_with("error[") || l.starts_with("error:")
            })
            .count();
        return format!("✗ {} error{}", n.max(1), if n == 1 { "" } else { "s" });
    }
    // A git/shell fatal is a failure too, shown as itself.
    if let Some(fatal) = t.lines().find(|l| l.trim_start().starts_with("fatal:")) {
        return format!("✗ {}", fatal.trim().chars().take(76).collect::<String>());
    }
    if low.contains("warning:") {
        let n = t.lines().filter(|l| l.contains("warning:")).count();
        return format!("⚠ {n} warning{}", if n == 1 { "" } else { "s" });
    }
    // grep -n / -c: line count is the answer.
    let lines = t.lines().filter(|l| !l.trim().is_empty()).count();
    if lines > 1 {
        return format!("{lines} lines");
    }
    // A short single-line result IS the answer — show it.
    let first = t.lines().next().unwrap_or("").trim();
    if first.chars().count() <= 80 {
        first.to_owned()
    } else {
        format!("{}…", first.chars().take(80).collect::<String>())
    }
}

/// A capped preview of a tool's real output for the work log: up to 14 lines,
/// each ≤ 200 chars, total ≤ 1200 — enough to SEE what happened, bounded so a
/// giant dump can't bloat the log. A final "… (+N more)" marks truncation.
fn preview_lines(text: &str) -> Vec<String> {
    const MAX_LINES: usize = 14;
    const MAX_CHARS: usize = 1200;
    let all: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if all.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut budget = MAX_CHARS;
    for l in all.iter().take(MAX_LINES) {
        let line: String = l.chars().take(200).collect();
        if line.len() > budget {
            break;
        }
        budget -= line.len();
        out.push(line);
    }
    let shown = out.len();
    if all.len() > shown {
        out.push(format!("… (+{} more)", all.len() - shown));
    }
    out
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

/// Sum every input-side token field claude reports. With prompt caching on —
/// which it is, by default — the bulk of the prompt lands in
/// `cache_read_input_tokens` / `cache_creation_input_tokens`, and only the
/// uncached delta in `input_tokens`. Counting just the last field made the
/// Cost tab report a fraction of the real input (223K in vs 10.5M out — the
/// wrong way round). Cost itself is unaffected: `total_cost_usd` from the CLI
/// already prices every tier.
fn input_tokens_all(u: &serde_json::Value) -> u64 {
    let get = |k: &str| u.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
    get("input_tokens") + get("cache_read_input_tokens") + get("cache_creation_input_tokens")
}

/// Extract usage/cost from a stream `result` event.
fn parse_usage(v: &serde_json::Value) -> Option<Usage> {
    let u = v.get("usage")?;
    Some(Usage {
        input_tokens: input_tokens_all(u),
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
        input_tokens: input_tokens_all(u),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::McpAccess;

    #[test]
    fn tool_result_summary_reads_as_an_outcome_not_a_byte_count() {
        assert_eq!(
            summarize_tool_result(
                "   Compiling…\ntest result: ok. 220 passed; 0 failed; 0 ignored"
            ),
            "✓ 220 passed"
        );
        assert_eq!(
            summarize_tool_result("test result: FAILED. 2 passed; 1 failed; 0 ignored"),
            "✗ 1 failed"
        );
        assert!(
            summarize_tool_result("error[E0433]: cannot find `x`\nerror: aborting")
                .starts_with("✗ 2 error")
        );
        // A source dump full of `.map_err`/`Error` must NOT read as failures.
        assert_eq!(
            summarize_tool_result("fn f() -> Result<(), Error> { x.map_err(|e| e)?; Ok(()) }"),
            "fn f() -> Result<(), Error> { x.map_err(|e| e)?; Ok(()) }"
        );
        // A git fatal is shown as itself.
        assert!(summarize_tool_result("fatal: path 'x.rs' does not exist").starts_with("✗ fatal:"));
        assert_eq!(
            summarize_tool_result("warning: unused variable `y`"),
            "⚠ 1 warning"
        );
        assert_eq!(summarize_tool_result("a\nb\nc"), "3 lines");
        assert_eq!(
            summarize_tool_result("crates/app/src/lib.rs:42"),
            "crates/app/src/lib.rs:42"
        );
        assert_eq!(summarize_tool_result("   "), "done (no output)");
    }

    #[test]
    fn input_tokens_include_the_cached_tiers() {
        // With caching on, most of the prompt is billed as cache reads. The
        // Cost tab was undercounting input by ignoring the two cache fields.
        let raw = r#"{"result":"ok","total_cost_usd":0.5,"usage":{"input_tokens":100,"cache_read_input_tokens":9000,"cache_creation_input_tokens":900,"output_tokens":50}}"#;
        let (_text, usage) = parse_json_output(raw);
        let u = usage.expect("usage");
        assert_eq!(u.input_tokens, 10_000, "100 + 9000 + 900");
        assert_eq!(u.output_tokens, 50);
        assert!((u.cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn mcp_config_json_includes_auth_header_when_token_present() {
        let mcp = McpAccess {
            url: "http://127.0.0.1:4000/api/mcp".to_owned(),
            token: Some("secret123".to_owned()),
            project: "cxc".to_owned(),
        };
        let v: serde_json::Value =
            serde_json::from_str(&mcp_config_json(&mcp)).expect("valid json");
        assert_eq!(v["mcpServers"]["coxagent"]["type"], "http");
        assert_eq!(
            v["mcpServers"]["coxagent"]["url"],
            "http://127.0.0.1:4000/api/mcp"
        );
        assert_eq!(
            v["mcpServers"]["coxagent"]["headers"]["Authorization"],
            "Bearer secret123"
        );
    }

    #[test]
    fn mcp_config_json_omits_headers_when_no_token() {
        let mcp = McpAccess {
            url: "http://127.0.0.1:4000/api/mcp".to_owned(),
            token: None,
            project: "cxc".to_owned(),
        };
        let v: serde_json::Value =
            serde_json::from_str(&mcp_config_json(&mcp)).expect("valid json");
        assert!(v["mcpServers"]["coxagent"].get("headers").is_none());
    }

    /// End-to-end through the real `AgentEnginePort::run()` — a fake `claude`
    /// binary (a shell script) captures its own argv instead of calling a
    /// real LLM, so this proves `--mcp-config` is actually on the command
    /// line the real spawn path builds, not just in the leaf JSON builder.
    #[tokio::test]
    async fn run_passes_mcp_config_flag_to_the_real_spawn() {
        use coxagent_application::ports::outbound::{AgentEnginePort, AgentRequest};
        use coxagent_domain::Role;

        let dir = std::env::temp_dir().join(format!("claude-mcp-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");

        let argv_capture = dir.join("argv.txt");
        let mcp_config_capture = dir.join("mcp-config-seen.json");
        let fake_bin = dir.join("fake-claude.sh");
        // Also copies out whatever file --mcp-config points at, while it
        // still exists (McpConfigFile deletes it once run() returns) — that's
        // how this test can assert on its contents without re-exposing the
        // token on the command line itself (which is the whole point of the
        // fix: the URL/token live in the FILE, not argv).
        std::fs::write(
            &fake_bin,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' \"$@\" > {argv_capture:?}\n\
                 prev=\"\"\n\
                 for a in \"$@\"; do\n\
                 \x20\x20if [ \"$prev\" = \"--mcp-config\" ]; then cp \"$a\" {mcp_config_capture:?}; fi\n\
                 \x20\x20prev=\"$a\"\n\
                 done\n\
                 echo '{{\"type\":\"result\",\"result\":\"ok\",\"usage\":{{\"input_tokens\":1,\"output_tokens\":1}},\"total_cost_usd\":0.0}}'\n"
            ),
        )
        .expect("write fake bin");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_bin).expect("meta").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_bin, perms).expect("chmod");
        }

        let engine = ClaudeEngine::new("test-model")
            .with_binary(fake_bin.to_string_lossy())
            .with_mcp(Some(McpAccess {
                url: "http://127.0.0.1:4000/api/mcp".to_owned(),
                token: Some("tok".to_owned()),
                project: "cxc".to_owned(),
            }));

        let outcome = engine
            .run(AgentRequest {
                role: Role::DevFeature,
                system_prompt: "sys".to_owned(),
                task_prompt: "task".to_owned(),
                work_dir: dir.clone(),
                // Generous on purpose (CXA-B041): this spawns a real child via
                // the production path, and a one-line echo can still outlast a
                // tight wall-clock budget when CI is heavily loaded. The test
                // asserts argv/env plumbing only — latency is irrelevant.
                timeout: std::time::Duration::from_secs(120),
                escalation_level: 0,
                label: None,
            })
            .await
            .expect("fake binary run succeeds");
        assert!(outcome.succeeded());

        let argv = std::fs::read_to_string(&argv_capture).expect("argv captured");
        assert!(argv.contains("--mcp-config"), "argv was:\n{argv}");
        // The token must NOT be reachable from argv/ps — only from the file
        // --mcp-config points at.
        assert!(
            !argv.contains("tok") && !argv.contains("Bearer"),
            "token/header leaked into argv:\n{argv}"
        );
        assert!(
            argv.contains("cxc"),
            "system prompt hint should carry the project id"
        );

        let mcp_config: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&mcp_config_capture).expect("--mcp-config file was readable"),
        )
        .expect("valid json");
        assert_eq!(
            mcp_config["mcpServers"]["coxagent"]["url"],
            "http://127.0.0.1:4000/api/mcp"
        );
        assert_eq!(
            mcp_config["mcpServers"]["coxagent"]["headers"]["Authorization"],
            "Bearer tok"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mcp_config_file_is_private_and_cleaned_up_on_drop() {
        let mcp = McpAccess {
            url: "http://127.0.0.1:4000/api/mcp".to_owned(),
            token: Some("tok".to_owned()),
            project: "cxc".to_owned(),
        };
        let path = {
            let f = McpConfigFile::write(&mcp).expect("write");
            let path = f.0.clone();
            assert!(path.exists());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "config file must be owner-only");
            }
            path
            // `f` drops here
        };
        assert!(
            !path.exists(),
            "config file must be removed once the engine is done with it"
        );
    }

    #[test]
    fn mcp_prompt_hint_carries_the_project_id() {
        let mcp = McpAccess {
            url: "http://127.0.0.1:4000/api/mcp".to_owned(),
            token: None,
            project: "cxc".to_owned(),
        };
        let hint = mcp_prompt_hint(&mcp);
        assert!(hint.contains("cxc"));
        assert!(hint.contains("search_symbols"));
    }

    #[test]
    fn extracts_session_id_from_stream() {
        let raw = concat!(
            "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"abc-123\"}\n",
            "{\"type\":\"result\",\"session_id\":\"abc-123\"}\n",
        );
        assert_eq!(super::extract_session(raw).as_deref(), Some("abc-123"));
        assert_eq!(super::extract_session("not json\n{}"), None);
    }

    #[test]
    fn escalation_ladder_picks_stronger_models_on_retries() {
        let e = super::ClaudeEngine::new("sonnet");
        assert_eq!(e.model_for(0), "sonnet");
        assert_eq!(e.model_for(1), "opus", "default ladder escalates to opus");
        assert_eq!(e.model_for(9), "opus", "clamps to the last rung");
        let e = super::ClaudeEngine::new("sonnet")
            .with_escalation(vec!["opus".into(), "opus-max".into()]);
        assert_eq!(e.model_for(1), "opus");
        assert_eq!(e.model_for(2), "opus-max");
        assert_eq!(e.model_for(3), "opus-max");
    }
}
