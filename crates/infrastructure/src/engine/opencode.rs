//! `OpencodeEngine` — runs the `opencode` CLI as the agent engine.
//!
//! Mirrors the reference workflow: `opencode run --model provider/model
//! --dangerously-skip-permissions --dir <workdir> --format json <prompt>`.
//! Streams NDJSON events line-by-line so the dashboard's live log updates
//! in real-time. Aggregates `tokens` and `cost` from `step_finish` events.

use async_trait::async_trait;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus,
};
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
    /// This project's CoXAgent MCP endpoint, when reachable — see [`crate::engine::McpAccess`].
    mcp: Option<crate::engine::McpAccess>,
    /// Confine file writes to the project workspace (see `proc::agent_command`).
    sandbox: bool,
    /// Retry escalation ladder (see `with_escalation` / `model_for`).
    escalation: Vec<String>,
}

impl OpencodeEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: "opencode".to_owned(),
            mcp: None,
            escalation: Vec::new(),
            sandbox: false,
        }
    }

    /// Confine agent file writes to the workspace + tool caches (macOS).
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Override the retry escalation ladder. Empty = auto-detect from the
    /// opencode config at run time (custom providers first, then built-ins).
    #[must_use]
    pub fn with_escalation(mut self, ladder: Vec<String>) -> Self {
        self.escalation = ladder;
        self
    }

    /// The model for an escalation level: 0 = configured model; n ≥ 1 walks
    /// the ladder (configured, else detected from opencode's own config —
    /// CUSTOM providers take priority over built-in ones, per house policy).
    fn model_for(&self, level: u8, work_dir: &std::path::Path) -> String {
        if level == 0 {
            return self.model.clone();
        }
        let ladder = if self.escalation.is_empty() {
            detected_escalation(work_dir, &self.model)
        } else {
            self.escalation.clone()
        };
        if ladder.is_empty() {
            return self.model.clone();
        }
        let idx = usize::from(level - 1).min(ladder.len() - 1);
        ladder[idx].clone()
    }

    /// Override the binary path (used by discovery / tests).
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

/// Ensure `<work_dir>/opencode.json` has a `mcp.coxagent` entry pointing at
/// `mcp.url` (opencode's `remote` MCP server type — a plain HTTP JSON-RPC
/// endpoint, same shape as `~/.config/opencode/opencode.json` uses for other
/// remote servers). Additive: merges into whatever's already there instead of
/// overwriting the project's own config, and no-ops if the desired entry is
/// already present so it doesn't dirty the managed repo's git status on every
/// run. Best-effort — a write failure just means this run has no live MCP.
/// Our own config file inside the workspace. Named with a `cox-` prefix so it
/// can NEVER collide with the project's real `opencode.json` — we previously
/// wrote into that file directly, which fought the user's own opencode setup.
/// The engine points opencode at this file via the `OPENCODE_CONFIG` env var.
pub const COX_OPENCODE_CONFIG: &str = "cox-opencode.json";

fn ensure_opencode_mcp_config(work_dir: &std::path::Path, mcp: &crate::engine::McpAccess) {
    migrate_legacy_opencode_config(work_dir);
    let path = work_dir.join(COX_OPENCODE_CONFIG);
    let mut entry = serde_json::json!({ "type": "remote", "url": mcp.url });
    if let Some(token) = &mcp.token {
        entry["headers"] = serde_json::json!({ "Authorization": format!("Bearer {token}") });
    }
    let doc = serde_json::json!({ "mcp": { "coxagent": entry } });
    let existing: Option<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    if existing.as_ref() == Some(&doc) {
        return; // already up to date
    }
    let Ok(text) = serde_json::to_string_pretty(&doc) else {
        return;
    };
    if std::fs::write(&path, text).is_err() {
        return;
    }
    // Owner-only: this file carries a live bearer token when auth is
    // configured, and it lives inside the managed (git-tracked) codebase.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    // Always ours, always token-bearing — never let `git add -A` pick it up.
    ensure_gitignored(work_dir, COX_OPENCODE_CONFIG);
}

/// One-time cleanup of the OLD behaviour, which wrote into the project's own
/// `opencode.json`:
/// - a file that is PURELY our injection (only `mcp.coxagent`) is renamed to
///   the new `cox-opencode.json`;
/// - a MIXED file (user settings + our injected `mcp.coxagent`) gets ONLY our
///   key surgically removed, restoring the user's config;
/// - either way the exact `opencode.json` .gitignore line we used to add is
///   dropped, so the user's real config doesn't stay silently git-ignored.
/// Files without our marker are untouched — they were never ours.
fn migrate_legacy_opencode_config(work_dir: &std::path::Path) {
    let legacy = work_dir.join("opencode.json");
    let Ok(text) = std::fs::read_to_string(&legacy) else {
        return;
    };
    let Ok(mut doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    if doc.pointer("/mcp/coxagent").is_none() {
        return; // not our injection — hands off
    }
    let purely_ours = doc.as_object().is_some_and(|o| {
        o.len() == 1
            && o.get("mcp")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|m| m.len() == 1 && m.contains_key("coxagent"))
    });
    if purely_ours {
        let _ = std::fs::rename(&legacy, work_dir.join(COX_OPENCODE_CONFIG));
    } else {
        // Surgical: remove only our key (and an mcp object left empty by it).
        if let Some(m) = doc
            .get_mut("mcp")
            .and_then(serde_json::Value::as_object_mut)
        {
            m.remove("coxagent");
        }
        if doc
            .get("mcp")
            .and_then(serde_json::Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
        {
            if let Some(o) = doc.as_object_mut() {
                o.remove("mcp");
            }
        }
        if let Ok(clean) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::write(&legacy, clean);
        }
    }
    remove_gitignore_line(work_dir, "opencode.json");
}

/// Drop an EXACT line from `.gitignore` (best-effort). Only used to undo the
/// line this codebase itself used to add.
fn remove_gitignore_line(work_dir: &std::path::Path, entry: &str) {
    let path = work_dir.join(".gitignore");
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return;
    };
    if !existing.lines().any(|l| l.trim() == entry) {
        return;
    }
    let kept: Vec<&str> = existing.lines().filter(|l| l.trim() != entry).collect();
    let _ = std::fs::write(&path, kept.join("\n") + "\n");
}

/// Append `entry` to `<work_dir>/.gitignore` if no existing line already
/// covers it (an exact `entry` line, or a broader pattern the caller can't
/// know about — we only dedupe the exact line, so this is best-effort, not a
/// full gitignore-pattern matcher). Creates the file if absent. Best-effort:
/// a write failure just means the file goes untracked-but-not-git-ignored,
/// same as before this function existed.
fn ensure_gitignored(work_dir: &std::path::Path, entry: &str) {
    let path = work_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == entry) {
        return;
    }
    use std::io::Write as _;
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let sep = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let _ = writeln!(f, "{sep}{entry}");
}

/// Same project-id + tool nudge as the claude adapter (see `claude.rs`'s
/// `mcp_prompt_hint`) — opencode has no `--append-system-prompt` cache-friendly
/// slot, so this is folded into the same concatenated prompt string as
/// everything else.
fn mcp_prompt_hint(mcp: &crate::engine::McpAccess) -> String {
    format!(
        "\n\nThis project's id is `{}`. You have MCP tools `search_symbols` and \
         `symbol_refs` (project=\"{}\") backed by the native code graph — use \
         them to locate code and check blast radius before reading files blind.",
        mcp.project, mcp.project
    )
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

    fn sandbox_status(&self) -> SandboxStatus {
        crate::proc::sandbox_status(self.sandbox)
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let prompt = match &self.mcp {
            Some(mcp) => {
                ensure_opencode_mcp_config(&request.work_dir, mcp);
                format!(
                    "{}{}\n\n---\n\n{}",
                    request.system_prompt,
                    mcp_prompt_hint(mcp),
                    request.task_prompt
                )
            }
            None => format!(
                "{}\n\n---\n\n{}",
                request.system_prompt, request.task_prompt
            ),
        };

        let role = crate::engine::role_key(request.role);
        let live = live_path(&request.work_dir, &role);
        if let Some(p) = &live {
            let _ = std::fs::write(p, format!("# {role} — live @ run start\n"));
        }

        // nice(+10) + optional write-confinement (see proc::agent_command).
        let (mut cmd, sandbox) =
            crate::proc::agent_command(&self.binary, &request.work_dir, self.sandbox);
        cmd.arg("run")
            .arg("--model")
            .arg(self.model_for(request.escalation_level, &request.work_dir))
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
        if self.mcp.is_some() {
            // Our MCP wiring lives in cox-opencode.json (never the project's
            // own opencode.json); point opencode at it explicitly.
            cmd.env(
                "OPENCODE_CONFIG",
                request.work_dir.join(COX_OPENCODE_CONFIG),
            );
        }
        crate::engine::apply_shim_path(&mut cmd);

        self.exec(cmd, live, request.timeout, sandbox).await
    }

    async fn resume_run(
        &self,
        session_id: &str,
        follow_up: &str,
        work_dir: &std::path::Path,
        timeout: std::time::Duration,
    ) -> Result<AgentOutcome, PortError> {
        let live = live_path(work_dir, "resume");
        let (mut cmd, sandbox) = crate::proc::agent_command(&self.binary, work_dir, self.sandbox);
        cmd.arg("run")
            .arg("--model")
            .arg(&self.model)
            .arg("--dangerously-skip-permissions")
            .arg("--dir")
            .arg(work_dir)
            .arg("--format")
            .arg("json")
            .arg("--session")
            .arg(session_id)
            .arg(follow_up)
            .current_dir(work_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        cmd.env_remove("OPENCODE");
        cmd.env_remove("OPENCODE_PID");
        if self.mcp.is_some() {
            cmd.env("OPENCODE_CONFIG", work_dir.join(COX_OPENCODE_CONFIG));
        }
        crate::engine::apply_shim_path(&mut cmd);
        self.exec(cmd, live, timeout, sandbox).await
    }
}

impl OpencodeEngine {
    /// Spawn `cmd`, stream NDJSON stdout to the live log, parse the outcome.
    async fn exec(
        &self,
        cmd: Command,
        live: Option<std::path::PathBuf>,
        timeout: std::time::Duration,
        sandbox: SandboxStatus,
    ) -> Result<AgentOutcome, PortError> {
        let mut cmd = cmd;
        let mut child = crate::proc::spawn_confined(&mut cmd, sandbox)
            .await
            .map_err(|e| PortError::Backend(format!("spawn opencode: {e}")))?;
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

        let leader_pid = child_pid;
        let (raw, status) = match tokio::time::timeout(timeout, read).await {
            Ok(r) => r?,
            Err(_) => {
                if let Some(pid) = leader_pid {
                    crate::proc::kill_group(pid);
                }
                return Err(PortError::Backend("opencode timed out".to_owned()));
            }
        };
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
            session_id: extract_session(&raw),
            sandbox,
        })
    }
}

/// First session id seen in the NDJSON stream (`sessionID` on opencode
/// events) — the handle for `run --session`.
fn extract_session(raw: &str) -> Option<String> {
    raw.lines().find_map(|l| {
        let v = serde_json::from_str::<serde_json::Value>(l.trim()).ok()?;
        ["sessionID", "session_id"].iter().find_map(|k| {
            v.get(k)
                .or_else(|| v.pointer(&format!("/part/{k}")))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
    })
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

/// Provider ids opencode ships with. Anything else defined under `provider`
/// in an opencode config is a CUSTOM provider (own endpoint/npm package) and,
/// per house policy, custom providers are the FIRST escalation choice.
const BUILTIN_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "azure",
    "amazon-bedrock",
    "openrouter",
    "github-copilot",
    "opencode",
    "xai",
    "groq",
    "mistral",
    "deepseek",
];

/// Escalation candidates detected from opencode's own config files
/// (`<work_dir>/opencode.json`, then `~/.config/opencode/opencode.json`):
/// every configured `provider/model`, CUSTOM providers first, built-ins after,
/// preserving config order; the currently-selected model is excluded.
fn detected_escalation(work_dir: &std::path::Path, current: &str) -> Vec<String> {
    let mut configs = Vec::new();
    for path in [
        work_dir.join("opencode.json"),
        dirs_config().join("opencode").join("opencode.json"),
    ] {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                configs.push(v);
            }
        }
    }
    escalation_from_configs(&configs, current)
}

fn dirs_config() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default()
                .join(".config")
        },
        std::path::PathBuf::from,
    )
}

/// Pure ordering: custom-provider models first, then built-in-provider models.
fn escalation_from_configs(configs: &[serde_json::Value], current: &str) -> Vec<String> {
    let mut custom = Vec::new();
    let mut builtin = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for cfg in configs {
        let Some(providers) = cfg.get("provider").and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (pid, pdef) in providers {
            let Some(models) = pdef.get("models").and_then(serde_json::Value::as_object) else {
                continue;
            };
            for model in models.keys() {
                let full = format!("{pid}/{model}");
                if full == current || !seen.insert(full.clone()) {
                    continue;
                }
                if BUILTIN_PROVIDERS.contains(&pid.as_str()) {
                    builtin.push(full);
                } else {
                    custom.push(full);
                }
            }
        }
    }
    custom.extend(builtin);
    custom
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

    fn mcp(token: Option<&str>) -> crate::engine::McpAccess {
        crate::engine::McpAccess {
            url: "http://127.0.0.1:4000/api/mcp".to_owned(),
            token: token.map(str::to_owned),
            project: "cxc".to_owned(),
        }
    }

    #[test]
    fn writes_cox_config_never_touching_the_projects_opencode_json() {
        let dir = std::env::temp_dir().join(format!("oc-cox-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("opencode.json"), r#"{"theme":"dark"}"#).expect("user cfg");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(COX_OPENCODE_CONFIG)).expect("cox cfg"),
        )
        .expect("json");
        assert_eq!(
            doc.pointer("/mcp/coxagent/headers/Authorization")
                .and_then(|v| v.as_str()),
            Some("Bearer tok")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("opencode.json")).expect("user cfg"),
            r#"{"theme":"dark"}"#,
            "the user's own opencode.json must be untouched"
        );
        let gi = std::fs::read_to_string(dir.join(".gitignore")).unwrap_or_default();
        assert!(gi.lines().any(|l| l.trim() == COX_OPENCODE_CONFIG));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cox_config_is_idempotent_and_owner_only() {
        let dir = std::env::temp_dir().join(format!("oc-cox-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let first = std::fs::read_to_string(dir.join(COX_OPENCODE_CONFIG)).expect("read");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let second = std::fs::read_to_string(dir.join(COX_OPENCODE_CONFIG)).expect("read");
        assert_eq!(first, second);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(COX_OPENCODE_CONFIG))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "token-bearing file must be 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_a_purely_ours_legacy_file_by_rename() {
        let dir = std::env::temp_dir().join(format!("oc-mig-ours-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("opencode.json"),
            r#"{"mcp":{"coxagent":{"type":"remote","url":"http://x"}}}"#,
        )
        .expect("legacy");
        std::fs::write(
            dir.join(".gitignore"),
            "opencode.json
",
        )
        .expect("gi");
        ensure_opencode_mcp_config(&dir, &mcp(None));
        assert!(
            !dir.join("opencode.json").exists(),
            "purely-ours legacy file is renamed away"
        );
        assert!(dir.join(COX_OPENCODE_CONFIG).exists());
        let gi = std::fs::read_to_string(dir.join(".gitignore")).expect("gi");
        assert!(
            !gi.lines().any(|l| l.trim() == "opencode.json"),
            "our old ignore line must go, or the user's future config is silently ignored"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_a_mixed_legacy_file_by_surgical_removal() {
        let dir = std::env::temp_dir().join(format!("oc-mig-mixed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("opencode.json"),
            r#"{"theme":"dark","mcp":{"coxagent":{"type":"remote","url":"http://x"},"other":{"type":"local"}}}"#,
        )
        .expect("legacy");
        ensure_opencode_mcp_config(&dir, &mcp(None));
        let user: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("opencode.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(
            user.pointer("/theme").and_then(|v| v.as_str()),
            Some("dark")
        );
        assert!(user.pointer("/mcp/coxagent").is_none(), "our key removed");
        assert!(
            user.pointer("/mcp/other").is_some(),
            "the user's own mcp entry survives"
        );
        assert!(dir.join(COX_OPENCODE_CONFIG).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn untouched_when_legacy_file_is_not_ours() {
        let dir = std::env::temp_dir().join(format!("oc-mig-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let original = r#"{"theme":"dark","mcp":{"other":{"type":"local"}}}"#;
        std::fs::write(dir.join("opencode.json"), original).expect("cfg");
        ensure_opencode_mcp_config(&dir, &mcp(None));
        assert_eq!(
            std::fs::read_to_string(dir.join("opencode.json")).expect("read"),
            original,
            "a config without our marker is never modified"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_gitignored_is_idempotent_and_appends_a_newline() {
        let dir = std::env::temp_dir().join(format!("oc-gi-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(".gitignore"), "node_modules").expect("write"); // no trailing \n
        ensure_gitignored(&dir, "opencode.json");
        ensure_gitignored(&dir, "opencode.json"); // second call: no duplicate line
        let text = std::fs::read_to_string(dir.join(".gitignore")).expect("read");
        assert_eq!(
            text.lines().filter(|l| l.trim() == "opencode.json").count(),
            1,
            ".gitignore was:\n{text}"
        );
        assert!(
            text.lines().any(|l| l.trim() == "node_modules"),
            "existing entry preserved"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracts_session_id_from_events() {
        let raw = "{\"type\":\"text\",\"sessionID\":\"ses_9\",\"part\":{\"text\":\"hi\"}}\n";
        assert_eq!(super::extract_session(raw).as_deref(), Some("ses_9"));
        let nested = "{\"type\":\"text\",\"part\":{\"sessionID\":\"ses_n\"}}\n";
        assert_eq!(super::extract_session(nested).as_deref(), Some("ses_n"));
        assert_eq!(super::extract_session("{}"), None);
    }

    #[test]
    fn escalation_prefers_custom_providers_over_builtins() {
        let cfg: serde_json::Value = serde_json::json!({
            "provider": {
                "anthropic": {"models": {"claude-opus": {}}},
                "bizbrain": {
                    "options": {"baseURL": "https://llm.bizbrain.local/v1"},
                    "models": {"DeepSeek-V4-Pro": {}, "Qwen3.6-35B-A3B-thinking": {}}
                }
            }
        });
        let ladder = super::escalation_from_configs(&[cfg], "bizbrain/Qwen3.6-35B-A3B-thinking");
        assert_eq!(
            ladder,
            vec!["bizbrain/DeepSeek-V4-Pro", "anthropic/claude-opus"],
            "custom provider first, built-in after, current model excluded"
        );
    }

    #[test]
    fn escalation_empty_when_no_config() {
        assert!(super::escalation_from_configs(&[], "x/y").is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_writes_cox_config_and_exports_opencode_config_env() {
        use coxagent_application::ports::outbound::{AgentEnginePort, AgentRequest};
        use coxagent_domain::Role;

        let dir = std::env::temp_dir().join(format!("oc-cox-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");

        // Fake `opencode` echoes the env var back as a text event, so the test
        // observes what the REAL child process would see.
        let fake_bin = dir.join("fake-opencode.sh");
        std::fs::write(
            &fake_bin,
            "#!/bin/sh\nprintf '{\"type\":\"text\",\"part\":{\"text\":\"cfg=%s\"}}\\n' \"$OPENCODE_CONFIG\"\n",
        )
        .expect("write fake bin");
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_bin).expect("meta").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_bin, perms).expect("chmod");
        }

        let engine = OpencodeEngine::new("test/model")
            .with_binary(fake_bin.to_string_lossy())
            .with_mcp(Some(mcp(Some("tok"))));
        let outcome = engine
            .run(AgentRequest {
                role: Role::DevFeature,
                system_prompt: "s".into(),
                task_prompt: "t".into(),
                work_dir: dir.clone(),
                timeout: std::time::Duration::from_secs(20),
                escalation_level: 0,
            })
            .await
            .expect("run");
        assert!(outcome.succeeded(), "stderr: {}", outcome.stderr);
        assert!(
            dir.join(COX_OPENCODE_CONFIG).exists(),
            "cox config written before spawn"
        );
        let expected = dir.join(COX_OPENCODE_CONFIG);
        assert!(
            outcome
                .stdout
                .contains(&format!("cfg={}", expected.display())),
            "child saw OPENCODE_CONFIG={}; stdout: {}",
            expected.display(),
            outcome.stdout
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
