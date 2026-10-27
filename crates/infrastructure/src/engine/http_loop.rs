//! The `http` engine's agentic chat↔tool loop: drive an OpenAI-compatible
//! model through [`super::http_tools`] actions until it edits the workspace and
//! declares `done`. The loop is decoupled from HTTP — it takes an injected
//! `chat` callback — so it is unit-testable with a scripted fake model, with no
//! network and no live gateway.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::json;

use std::time::Duration;

use super::http_tools::{apply_tool, run_command, tool_call_from_content, ToolCall};

/// Hard per-command timeout for the `run` tool.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// System-prompt addendum that teaches the model the tool protocol. Appended to
/// the role's own system prompt.
pub(super) const TOOLS_INSTRUCTIONS: &str = "\n\n\
You are running in an automated coding loop with tools. To act, reply with a \
SINGLE fenced JSON block and nothing else:\n\
```json\n{\"tool\": \"read_file\", \"path\": \"relative/path\"}\n```\n\
Available tools:\n\
- read_file {path}: return the file's text.\n\
- list_dir {path}: list a directory (\".\" for the workspace root).\n\
- write_file {path, content}: create/overwrite a file with the FULL new content.\n\
- patch_file {path, find, replace}: replace the UNIQUE occurrence of `find` with \
`replace` in an existing file — prefer this for edits to large files. `find` \
must match exactly and appear once; add surrounding lines to disambiguate.\n\
- run {cmd}: run a shell command in the workspace (e.g. build or tests) and see \
its output + exit code; use it to verify your change.\n\
- done {summary}: finish, summarising what you changed.\n\
Paths are relative to the workspace; `..` and absolute paths are rejected. \
Inspect before you edit, make the smallest change that implements the task, \
run the build/tests to check it, then call done.";

/// One model turn's reply, as the injected chat callback returns it.
pub(super) struct TurnReply {
    pub content: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The result of running the loop.
pub(super) struct LoopOutcome {
    /// Final summary (the `done` summary, or a capped/stalled note).
    pub summary: String,
    /// Full transcript of turns + tool results, for `AgentOutcome.trace`.
    pub transcript: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Whether at least one `write_file` succeeded — lets the caller tell a real
    /// edit from pure chatter.
    pub wrote_any: bool,
    /// Whether the model called `done` within the cap.
    pub done: bool,
}

/// Drive the loop. `chat(messages)` sends the running conversation and returns
/// the assistant's next turn. Stops at `done` or after `max_iters` turns.
pub(super) async fn run_tool_loop<F, Fut>(
    mut chat: F,
    work_dir: &Path,
    system_prompt: &str,
    task_prompt: &str,
    max_iters: usize,
) -> LoopOutcome
where
    F: FnMut(Vec<serde_json::Value>) -> Fut,
    Fut: std::future::Future<Output = Result<TurnReply, String>>,
{
    let mut messages = vec![
        json!({"role": "system", "content": format!("{system_prompt}{TOOLS_INSTRUCTIONS}")}),
        json!({"role": "user", "content": task_prompt}),
    ];
    let mut transcript = String::new();
    let (mut input_tokens, mut output_tokens) = (0u64, 0u64);
    let mut wrote_any = false;

    for turn in 0..max_iters {
        let reply = match chat(messages.clone()).await {
            Ok(r) => r,
            Err(e) => {
                let _ = write!(transcript, "\n[turn {turn}] chat error: {e}\n");
                return LoopOutcome {
                    summary: format!("http loop aborted: {e}"),
                    transcript,
                    input_tokens,
                    output_tokens,
                    wrote_any,
                    done: false,
                };
            }
        };
        input_tokens = input_tokens.saturating_add(reply.input_tokens);
        output_tokens = output_tokens.saturating_add(reply.output_tokens);
        let _ = write!(transcript, "\n[assistant {turn}]\n{}\n", reply.content);
        messages.push(json!({"role": "assistant", "content": reply.content.clone()}));

        let Some(call) = tool_call_from_content(&reply.content) else {
            // No actionable tool call — nudge once toward the protocol.
            messages.push(json!({
                "role": "user",
                "content": "No tool action found. Reply with exactly one ```json {\"tool\": …}``` block, or {\"tool\": \"done\", \"summary\": …} to finish."
            }));
            continue;
        };

        if let ToolCall::Done { summary } = &call {
            let _ = writeln!(transcript, "[done] {summary}");
            return LoopOutcome {
                summary: summary.clone(),
                transcript,
                input_tokens,
                output_tokens,
                wrote_any,
                done: true,
            };
        }

        let result = match &call {
            ToolCall::Run { cmd } => run_command(work_dir, cmd, RUN_TIMEOUT).await,
            other => apply_tool(work_dir, other),
        };
        if matches!(call, ToolCall::WriteFile { .. } | ToolCall::PatchFile { .. }) && result.ok {
            wrote_any = true;
        }
        let _ = writeln!(
            transcript,
            "[tool {}] {}",
            if result.ok { "ok" } else { "err" },
            result.content.chars().take(200).collect::<String>()
        );
        messages.push(json!({
            "role": "user",
            "content": format!(
                "Tool result ({}):\n{}",
                if result.ok { "ok" } else { "error" },
                result.content
            ),
        }));
    }

    LoopOutcome {
        summary: format!(
            "http loop hit the {max_iters}-turn cap without calling done ({})",
            if wrote_any { "some files were written" } else { "no files written" }
        ),
        transcript,
        input_tokens,
        output_tokens,
        wrote_any,
        done: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A scripted fake model: returns the queued replies in order.
    fn scripted(replies: Vec<&str>) -> impl FnMut(Vec<serde_json::Value>) -> std::future::Ready<Result<TurnReply, String>> {
        let q = RefCell::new(replies.into_iter().map(str::to_owned).collect::<Vec<_>>());
        move |_messages| {
            let next = if q.borrow().is_empty() {
                "(no more scripted replies)".to_owned()
            } else {
                q.borrow_mut().remove(0)
            };
            std::future::ready(Ok(TurnReply { content: next, input_tokens: 5, output_tokens: 7 }))
        }
    }

    #[tokio::test]
    async fn loop_writes_a_file_then_finishes_on_done() {
        let dir = std::env::temp_dir().join(format!("cox-http-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let out = run_tool_loop(
            scripted(vec![
                r#"```json
{"tool": "write_file", "path": "out.txt", "content": "done by model"}
```"#,
                r#"```json
{"tool": "done", "summary": "wrote out.txt"}
```"#,
            ]),
            &dir,
            "you are a coder",
            "create out.txt",
            12,
        )
        .await;

        assert!(out.done);
        assert!(out.wrote_any);
        assert_eq!(out.summary, "wrote out.txt");
        assert_eq!(out.input_tokens, 10); // two turns × 5
        assert_eq!(out.output_tokens, 14);
        assert_eq!(
            std::fs::read_to_string(dir.join("out.txt")).unwrap(),
            "done by model"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loop_stops_at_cap_when_model_never_finishes() {
        let dir = std::env::temp_dir().join(format!("cox-http-loop-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Model keeps chatting with no tool call.
        let out = run_tool_loop(scripted(vec!["thinking…", "still thinking…"]), &dir, "s", "t", 2).await;
        assert!(!out.done);
        assert!(!out.wrote_any);
        assert!(out.summary.contains("cap"), "{}", out.summary);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loop_runs_a_command_and_feeds_the_output_back() {
        let dir = std::env::temp_dir().join(format!("cox-http-loop-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = run_tool_loop(
            scripted(vec![
                "```json\n{\"tool\": \"run\", \"cmd\": \"echo built-ok\"}\n```",
                "```json\n{\"tool\": \"done\", \"summary\": \"verified\"}\n```",
            ]),
            &dir,
            "s",
            "t",
            12,
        )
        .await;
        assert!(out.done);
        assert!(out.transcript.contains("built-ok"), "{}", out.transcript);
        assert!(out.transcript.contains("[tool ok]"), "{}", out.transcript);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn loop_reports_a_write_that_escapes_as_a_tool_error_not_a_write() {
        let dir = std::env::temp_dir().join(format!("cox-http-loop-esc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = run_tool_loop(
            scripted(vec![
                r#"```json
{"tool": "write_file", "path": "../escape.txt", "content": "x"}
```"#,
                r#"```json
{"tool": "done", "summary": "tried"}
```"#,
            ]),
            &dir,
            "s",
            "t",
            12,
        )
        .await;
        assert!(out.done);
        assert!(!out.wrote_any, "an escaping write must not count as a write");
        assert!(out.transcript.contains("[tool err]"), "{}", out.transcript);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
