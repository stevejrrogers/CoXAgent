//! Tool protocol for the `http` engine's (forthcoming) agentic file-editing
//! loop. This module is the PURE, testable core: the tool vocabulary a model
//! may call, how to parse a model turn into a [`ToolCall`], and how to apply
//! one against a workspace safely. The chat↔tool loop that drives it lives in a
//! follow-up slice; keeping the protocol here lets every parsing and
//! path-safety rule be unit-tested without a network or a live model.
//!
//! Safety: a tool path is always workspace-relative. [`safe_join`] rejects an
//! absolute path or any `..` component *lexically* (before touching disk, so it
//! also guards a not-yet-created write target), so a model can never read or
//! write outside `work_dir`. File I/O lives here in infrastructure, never in
//! application (the hexagonal gate forbids it there).

use std::path::{Component, Path, PathBuf};

/// One action the model asked for. The vocabulary is deliberately tiny — enough
/// to inspect and edit a workspace, plus `Done` to end the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    ReadFile { path: String },
    WriteFile { path: String, content: String },
    ListDir { path: String },
    Done { summary: String },
}

/// The outcome of applying a [`ToolCall`], fed back to the model as the tool
/// result for the next turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// Whether the op succeeded; a failure is reported to the model, not fatal.
    pub ok: bool,
    /// Human/model-readable content (file text, listing, confirmation, error).
    pub content: String,
}

impl ToolResult {
    fn ok(content: impl Into<String>) -> Self {
        Self { ok: true, content: content.into() }
    }
    fn err(content: impl Into<String>) -> Self {
        Self { ok: false, content: content.into() }
    }
}

/// Resolve a workspace-relative tool path under `work_dir`, rejecting anything
/// that would escape it. Purely lexical: no canonicalize, so it is correct for
/// a write to a path that does not exist yet.
///
/// # Errors
/// A message naming why the path was refused (absolute, empty, or `..`).
pub fn safe_join(work_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.trim().is_empty() {
        return Err("empty path".to_owned());
    }
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(format!("absolute paths are not allowed: {rel}"));
    }
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                return Err(format!("path escapes the workspace (`..`): {rel}"));
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(format!("rooted paths are not allowed: {rel}"));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(work_dir.join(p))
}

/// Parse one normalized tool object — `{"tool": "...", ...}` — into a
/// [`ToolCall`]. This is the shape both the OpenAI `tool_calls` arguments and
/// the fenced-JSON fallback reduce to.
#[must_use]
pub fn tool_call_from_json(v: &serde_json::Value) -> Option<ToolCall> {
    let tool = v.get("tool").and_then(serde_json::Value::as_str)?;
    let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_owned);
    match tool {
        "read_file" => Some(ToolCall::ReadFile { path: s("path")? }),
        "write_file" => Some(ToolCall::WriteFile {
            path: s("path")?,
            content: s("content").unwrap_or_default(),
        }),
        "list_dir" => Some(ToolCall::ListDir {
            path: s("path").unwrap_or_else(|| ".".to_owned()),
        }),
        "done" => Some(ToolCall::Done {
            summary: s("summary").unwrap_or_default(),
        }),
        _ => None,
    }
}

/// Extract a tool call from a model's assistant message content when it did not
/// use native `tool_calls`: find the first ```json fenced block (or fall back
/// to the first `{…}` object) and parse it. Lets the engine drive gateways that
/// lack OpenAI function-calling.
#[must_use]
pub fn tool_call_from_content(content: &str) -> Option<ToolCall> {
    let json = extract_json_block(content)?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    tool_call_from_json(&v)
}

/// The first fenced ```json … ``` block, else the first balanced `{…}` span.
fn extract_json_block(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = &text[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_owned());
        }
    }
    // Fallback: first `{` to its matching `}` (depth-counted, string-aware).
    let bytes = text.as_bytes();
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

/// Apply a [`ToolCall`] against `work_dir`, returning what to report back. Never
/// panics and never escapes the workspace; a bad path or I/O error is a failed
/// [`ToolResult`], not an error return.
#[must_use]
pub fn apply_tool(work_dir: &Path, call: &ToolCall) -> ToolResult {
    match call {
        ToolCall::ReadFile { path } => match safe_join(work_dir, path) {
            Ok(p) => match std::fs::read_to_string(&p) {
                Ok(text) => ToolResult::ok(text),
                Err(e) => ToolResult::err(format!("read {path}: {e}")),
            },
            Err(e) => ToolResult::err(e),
        },
        ToolCall::WriteFile { path, content } => match safe_join(work_dir, path) {
            Ok(p) => {
                if let Some(parent) = p.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return ToolResult::err(format!("mkdir for {path}: {e}"));
                    }
                }
                match std::fs::write(&p, content) {
                    Ok(()) => ToolResult::ok(format!("wrote {} bytes to {path}", content.len())),
                    Err(e) => ToolResult::err(format!("write {path}: {e}")),
                }
            }
            Err(e) => ToolResult::err(e),
        },
        ToolCall::ListDir { path } => match safe_join(work_dir, path) {
            Ok(p) => match std::fs::read_dir(&p) {
                Ok(entries) => {
                    let mut names: Vec<String> = entries
                        .flatten()
                        .map(|e| {
                            let n = e.file_name().to_string_lossy().into_owned();
                            if e.path().is_dir() {
                                format!("{n}/")
                            } else {
                                n
                            }
                        })
                        .collect();
                    names.sort();
                    ToolResult::ok(names.join("\n"))
                }
                Err(e) => ToolResult::err(format!("list {path}: {e}")),
            },
            Err(e) => ToolResult::err(e),
        },
        ToolCall::Done { summary } => ToolResult::ok(summary.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_accepts_a_workspace_relative_path() {
        let wd = Path::new("/work");
        assert_eq!(safe_join(wd, "src/main.rs").unwrap(), Path::new("/work/src/main.rs"));
        assert_eq!(safe_join(wd, "./a/b").unwrap(), Path::new("/work/a/b"));
    }

    #[test]
    fn safe_join_rejects_escape_and_absolute() {
        let wd = Path::new("/work");
        assert!(safe_join(wd, "../etc/passwd").is_err());
        assert!(safe_join(wd, "a/../../b").is_err());
        assert!(safe_join(wd, "/etc/passwd").is_err());
        assert!(safe_join(wd, "").is_err());
    }

    #[test]
    fn parses_each_tool_from_json() {
        let r = tool_call_from_json(&serde_json::json!({"tool":"read_file","path":"x.rs"}));
        assert_eq!(r, Some(ToolCall::ReadFile { path: "x.rs".to_owned() }));
        let w = tool_call_from_json(&serde_json::json!({"tool":"write_file","path":"x","content":"hi"}));
        assert_eq!(w, Some(ToolCall::WriteFile { path: "x".to_owned(), content: "hi".to_owned() }));
        let l = tool_call_from_json(&serde_json::json!({"tool":"list_dir"}));
        assert_eq!(l, Some(ToolCall::ListDir { path: ".".to_owned() }));
        let d = tool_call_from_json(&serde_json::json!({"tool":"done","summary":"ok"}));
        assert_eq!(d, Some(ToolCall::Done { summary: "ok".to_owned() }));
        assert_eq!(tool_call_from_json(&serde_json::json!({"tool":"nope"})), None);
    }

    #[test]
    fn extracts_a_fenced_json_action_from_prose() {
        let msg = "Sure, I'll read it.\n```json\n{\"tool\": \"read_file\", \"path\": \"a.txt\"}\n```\nthen continue.";
        assert_eq!(
            tool_call_from_content(msg),
            Some(ToolCall::ReadFile { path: "a.txt".to_owned() })
        );
    }

    #[test]
    fn extracts_a_bare_json_object_when_unfenced() {
        let msg = r#"here: {"tool":"write_file","path":"f","content":"x"} done"#;
        assert_eq!(
            tool_call_from_content(msg),
            Some(ToolCall::WriteFile { path: "f".to_owned(), content: "x".to_owned() })
        );
    }

    #[test]
    fn write_then_read_round_trips_in_a_tempdir() {
        let dir = std::env::temp_dir().join(format!("cox-http-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let w = apply_tool(&dir, &ToolCall::WriteFile {
            path: "sub/hello.txt".to_owned(),
            content: "hi there".to_owned(),
        });
        assert!(w.ok, "{w:?}");

        let r = apply_tool(&dir, &ToolCall::ReadFile { path: "sub/hello.txt".to_owned() });
        assert!(r.ok);
        assert_eq!(r.content, "hi there");

        let l = apply_tool(&dir, &ToolCall::ListDir { path: "sub".to_owned() });
        assert!(l.ok);
        assert_eq!(l.content, "hello.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_refuses_to_escape_the_workspace() {
        let dir = std::env::temp_dir();
        let r = apply_tool(&dir, &ToolCall::ReadFile { path: "../../etc/passwd".to_owned() });
        assert!(!r.ok);
        assert!(r.content.contains("escapes"), "{}", r.content);
    }
}
