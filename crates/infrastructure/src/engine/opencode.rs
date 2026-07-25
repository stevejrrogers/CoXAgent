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
    /// This project's CoXAgent MCP endpoint, when reachable — see [`crate::engine::McpAccess`].
    mcp: Option<crate::engine::McpAccess>,
}

impl OpencodeEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: "opencode".to_owned(),
            mcp: None,
        }
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
fn ensure_opencode_mcp_config(work_dir: &std::path::Path, mcp: &crate::engine::McpAccess) {
    let path = work_dir.join("opencode.json");
    let existed_before = path.exists();
    let mut doc: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !doc.is_object() {
        return; // don't clobber a malformed/non-object config
    }
    let mut entry = serde_json::json!({ "type": "remote", "url": mcp.url });
    if let Some(token) = &mcp.token {
        entry["headers"] = serde_json::json!({ "Authorization": format!("Bearer {token}") });
    }
    if doc.pointer("/mcp/coxagent") == Some(&entry) {
        return; // already up to date
    }
    doc.as_object_mut()
        .expect("checked is_object above")
        .entry("mcp")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(mcp_obj) = doc.get_mut("mcp").and_then(serde_json::Value::as_object_mut) {
        mcp_obj.insert("coxagent".to_owned(), entry);
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
    // We're the one creating this file (it didn't exist before this call),
    // so it's ours to keep out of git — a bare `git add -A` in this project
    // must never pick up a live token. If the project already had its own
    // opencode.json (existed_before), leave .gitignore alone: it may have
    // other, intentionally-tracked settings we don't own an opinion on.
    if !existed_before {
        ensure_gitignored(work_dir, "opencode.json");
    }
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
            None => format!("{}\n\n---\n\n{}", request.system_prompt, request.task_prompt),
        };

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

    fn mcp(token: Option<&str>) -> crate::engine::McpAccess {
        crate::engine::McpAccess {
            url: "http://127.0.0.1:4000/api/mcp".to_owned(),
            token: token.map(str::to_owned),
            project: "cxc".to_owned(),
        }
    }

    #[test]
    fn ensure_opencode_mcp_config_creates_file_when_absent() {
        let dir = std::env::temp_dir().join(format!("oc-mcp-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.json")).expect("read"))
                .expect("valid json");
        assert_eq!(doc["mcp"]["coxagent"]["type"], "remote");
        assert_eq!(
            doc["mcp"]["coxagent"]["headers"]["Authorization"],
            "Bearer tok"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_opencode_mcp_config_preserves_existing_keys() {
        let dir = std::env::temp_dir().join(format!("oc-mcp-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("opencode.json"),
            r#"{"theme":"dark","mcp":{"other":{"type":"local","command":["x"]}}}"#,
        )
        .expect("write");
        ensure_opencode_mcp_config(&dir, &mcp(None));
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.json")).expect("read"))
                .expect("valid json");
        assert_eq!(doc["theme"], "dark", "unrelated top-level key preserved");
        assert_eq!(doc["mcp"]["other"]["type"], "local", "other server preserved");
        assert_eq!(doc["mcp"]["coxagent"]["type"], "remote");
        assert!(doc["mcp"]["coxagent"].get("headers").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end through the real `AgentEnginePort::run()` — not just the
    /// leaf `ensure_opencode_mcp_config` helper — using a fake `opencode`
    /// binary (a shell script) so no real LLM call happens. Proves the config
    /// file actually gets written by the real spawn path, not just when
    /// called directly.
    #[tokio::test]
    async fn run_writes_opencode_json_before_spawning() {
        use coxagent_application::ports::outbound::{AgentEnginePort, AgentRequest};
        use coxagent_domain::Role;

        let dir = std::env::temp_dir().join(format!("oc-mcp-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");

        // A fake `opencode` that ignores its args and prints one valid event.
        let fake_bin = dir.join("fake-opencode.sh");
        std::fs::write(
            &fake_bin,
            "#!/bin/sh\necho '{\"type\":\"text\",\"part\":{\"text\":\"ok\"}}'\n",
        )
        .expect("write fake bin");
        #[cfg(unix)]
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
                system_prompt: "sys".to_owned(),
                task_prompt: "task".to_owned(),
                work_dir: dir.clone(),
                timeout: std::time::Duration::from_secs(10),
            })
            .await
            .expect("fake binary run succeeds");
        assert!(outcome.succeeded());

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("opencode.json")).expect("config written"))
                .expect("valid json");
        assert_eq!(doc["mcp"]["coxagent"]["type"], "remote");
        assert_eq!(doc["mcp"]["coxagent"]["url"], "http://127.0.0.1:4000/api/mcp");
        assert_eq!(
            doc["mcp"]["coxagent"]["headers"]["Authorization"],
            "Bearer tok"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_opencode_mcp_config_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("oc-mcp-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let first = std::fs::read_to_string(dir.join("opencode.json")).expect("read");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let second = std::fs::read_to_string(dir.join("opencode.json")).expect("read");
        assert_eq!(first, second, "second call is a no-op byte-for-byte");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn ensure_opencode_mcp_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("oc-mcp-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let mode = std::fs::metadata(dir.join("opencode.json"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "opencode.json holds a live token — owner-only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_opencode_mcp_config_gitignores_the_file_it_creates() {
        let dir = std::env::temp_dir().join(format!("oc-mcp-gi-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        let gitignore = std::fs::read_to_string(dir.join(".gitignore")).expect("read");
        assert!(
            gitignore.lines().any(|l| l.trim() == "opencode.json"),
            ".gitignore was:\n{gitignore}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_opencode_mcp_config_leaves_gitignore_alone_for_a_pre_existing_file() {
        let dir = std::env::temp_dir().join(format!("oc-mcp-gi-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        // The project already tracks its own opencode.json before we ever touch it.
        std::fs::write(dir.join("opencode.json"), r#"{"theme":"dark"}"#).expect("write");
        ensure_opencode_mcp_config(&dir, &mcp(Some("tok")));
        assert!(
            !dir.join(".gitignore").exists(),
            "must not silently change tracking for a file we didn't create"
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
        assert!(text.lines().any(|l| l.trim() == "node_modules"), "existing entry preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
