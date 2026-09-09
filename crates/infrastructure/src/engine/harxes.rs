//! Adapter over the embedded `harxes-core` engine (CXA-F372) — the first
//! IN-PROCESS engine: no child process is spawned for the agent loop itself,
//! so the whole zombie class (a wedged CLI outliving its dropped caller)
//! cannot exist. Dropping the run future cancels the LLM call and kills the
//! process group of every Bash tool child (harxes-core's contract, covered by
//! its `drop_mid_run_leaves_no_process` test).
//!
//! Provider routing mirrors the hub's `provider/model` string convention:
//!   * `anthropic/<model>`  → Anthropic Messages API (`ANTHROPIC_API_KEY`,
//!     optional `ANTHROPIC_BASE_URL`);
//!   * `copilot/<model>`    → GitHub Copilot native API (`GH_TOKEN` /
//!     `GITHUB_TOKEN` OAuth token; harxes-core exchanges + refreshes the
//!     short-lived bearer itself);
//!   * anything else        → OpenAI-compatible (LiteLLM — the primary path):
//!     `COXAGENT_LLM_BASE_URL`/`COXAGENT_LLM_API_KEY`, falling back to
//!     `OPENAI_BASE_URL`/`OPENAI_API_KEY`. The full `provider/model` string is
//!     passed through as the model id, exactly as the opencode adapter does.
//!
//! Sandbox: the hub's confinement is injected as harxes-core's `ShellPort`
//! (`ConfinedShell`) — every Bash command the agent runs goes through
//! `proc::agent_command`, the same seatbelt/nice path the CLI engines use.
//! Permission mode is `AllowUnlessDenied` precisely because the shell IS
//! host-confined (the mode's documented precondition); parity with the CLI
//! engines' `--dangerously-skip-permissions`-inside-seatbelt posture.
//!
//! Error mapping preserves the F370 infra/task split without string-matching
//! on our side inventing anything: `EngineError::is_infra_fault()` decides,
//! and the message is rendered with the vocabulary `faults::is_infra_fault`
//! already recognizes ("timed out", "unavailable"), so the DEV failure
//! counter and the runner breaker classify harxes outcomes identically to
//! every other engine.

use async_trait::async_trait;
use coxagent_application::ports::outbound::engine::{
    AgentEnginePort, AgentOutcome, AgentRequest, SandboxStatus, Usage,
};
use coxagent_application::PortError;
use harxes_core::{
    CommandOutput, CommandPolicy, EngineConfig, EngineError, LoopLimits, PermissionMode,
    ProviderSpec, ReasoningEffort, RunEvent, RunOutcome, RunRequest, ShellError, ShellExitStatus,
    ShellPort,
};
use crate::engine::live::{append_live, live_path};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// In-process Harxes engine for one model selection.
pub struct HarxesEngine {
    /// Full `provider/model` selection string from config.
    model: String,
    sandbox: bool,
}

impl HarxesEngine {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            sandbox: false,
        }
    }

    /// Confine agent Bash commands to the workspace + tool caches.
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Resolve the provider from the hub's `provider/model` convention.
    fn provider_spec(&self) -> Result<ProviderSpec, PortError> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Some(rest) = self.model.strip_prefix("anthropic/") {
            let _ = rest;
            return Ok(ProviderSpec::Anthropic {
                base_url: env("ANTHROPIC_BASE_URL")
                    .unwrap_or_else(|| "https://api.anthropic.com/v1/messages".to_owned()),
                api_key: env("ANTHROPIC_API_KEY").ok_or_else(|| {
                    PortError::Backend("harxes: ANTHROPIC_API_KEY is not set".to_owned())
                })?,
            });
        }
        if self.model.strip_prefix("copilot/").is_some() {
            return Ok(ProviderSpec::Copilot {
                github_token: env("GH_TOKEN").or_else(|| env("GITHUB_TOKEN")).ok_or_else(
                    || {
                        PortError::Backend(
                            "harxes: GH_TOKEN/GITHUB_TOKEN is not set for the copilot provider"
                                .to_owned(),
                        )
                    },
                )?,
            });
        }
        let base_url = env("COXAGENT_LLM_BASE_URL")
            .or_else(|| env("OPENAI_BASE_URL"))
            .ok_or_else(|| {
                PortError::Backend(
                    "harxes: COXAGENT_LLM_BASE_URL/OPENAI_BASE_URL is not set".to_owned(),
                )
            })?;
        let api_key = env("COXAGENT_LLM_API_KEY")
            .or_else(|| env("OPENAI_API_KEY"))
            .unwrap_or_default();
        Ok(ProviderSpec::OpenAiCompatible { base_url, api_key })
    }

    /// The model id sent to the provider. Anthropic/Copilot get the bare
    /// model (their APIs know no `provider/` namespace); OpenAI-compatible
    /// keeps the full string — LiteLLM routes on it, same as opencode.
    fn provider_model(&self) -> String {
        for p in ["anthropic/", "copilot/"] {
            if let Some(rest) = self.model.strip_prefix(p) {
                return rest.to_owned();
            }
        }
        self.model.clone()
    }

    fn build_engine(
        &self,
        work_dir: &Path,
        role: coxagent_domain::Role,
    ) -> Result<harxes_core::HarxesEngine, PortError> {
        // The default cap (40 iters / 1.5M cumulative tokens) is dominated by
        // RE-SENT INPUT, not thinking: measured runs carry 1k-10k reasoning
        // tokens inside 1.5M totals (~40k context x 40 iterations). Deep-work
        // roles were dying at the cap mid-task with the budget spent on
        // context re-sends, so they get room to finish; the cap stays a
        // runaway backstop, not a working budget.
        let mut limits = LoopLimits::default();
        if matches!(effort_for(role), ReasoningEffort::High) {
            // With tool-output aging (harxes v0.4.2) an iteration costs
            // ~10-20k tokens, so iterations — not tokens — became the
            // binding cap: runs died at 60 iters holding only ~1.1-1.5M of
            // the 4M budget. 100 x ~12k stays well inside it.
            limits.max_iterations = 100;
            limits.max_total_tokens = 4_000_000;
        }
        let cfg = EngineConfig {
            provider: self.provider_spec()?,
            model: self.provider_model(),
            limits,
            command_policy: CommandPolicy::default(),
            // The shell below IS host-confined — the documented precondition
            // for this mode. With DenyUnlessAllowed and no allowlist the
            // agent could run nothing at all.
            permission: PermissionMode::AllowUnlessDenied,
            shell: Some(Arc::new(ConfinedShell {
                sandbox: self.sandbox,
                work_dir: work_dir.to_path_buf(),
            })),
            fs: None,
            // Per-run effort (below) wins; no engine-wide default.
            reasoning_effort: None,
            // Bizbrain rate-limits under 3 parallel workers; waiting out a
            // 429 (up to 2 min) keeps the run on the strong model instead of
            // bouncing it to the weak failover engine mid-task.
            rate_limit_patience: Some(std::time::Duration::from_secs(120)),
            // v0.4.4 distinct-key aging ended hot-file thrash (one file
            // re-read 29x had been eating a quarter of the iteration
            // budget). Deep-work roles keep a wider window of hot files.
            aging_keep_recent: if matches!(effort_for(role), ReasoningEffort::High) {
                Some(10)
            } else {
                None
            },
        };
        harxes_core::HarxesEngine::new(cfg)
            .map_err(|e| PortError::Backend(format!("harxes: engine config: {e}")))
    }
}

#[async_trait]
impl AgentEnginePort for HarxesEngine {
    fn id(&self) -> &'static str {
        "harxes"
    }

    fn sandbox_status(&self) -> SandboxStatus {
        crate::proc::sandbox_status(self.sandbox)
    }

    async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError> {
        let engine = self.build_engine(&request.work_dir, request.role)?;
        // Stream the work log where the dashboard's agent-log endpoint tails
        // it — an engine that only buffers into the outcome looks silent in
        // the live view even while it is plainly working (live.rs's warning).
        // The dashboard resolves live files by the role's serde key
        // (`dev_bug`), not its Debug name (`DevBug` -> "devbug") — a
        // mismatched name makes the whole run invisible in the UI.
        let role = {
            let dbg = format!("{:?}", request.role);
            let mut out = String::with_capacity(dbg.len() + 2);
            for (i, c) in dbg.chars().enumerate() {
                if c.is_ascii_uppercase() && i > 0 {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            }
            out
        };
        let live = live_path(&request.work_dir, &role, request.label.as_deref());
        if let Some(p) = &live {
            let _ = std::fs::write(
                p,
                format!("# {role} — harxes · {} — run start\n", self.model),
            );
        }
        let handle = engine.run(RunRequest {
            prompt: request.task_prompt.clone(),
            system_prompt: Some(request.system_prompt.clone()),
            history: Vec::new(),
            timeout: Some(request.timeout),
            model: None,
            // v0.3.2: anchors the engine-side FileSystemPort (Read/Write/
            // Glob/Grep walk from here, relative results) and is what the
            // engine hands our ConfinedShell as `working_dir`. We inject
            // shell (confined) but NOT fs, so the engine builds its rooted
            // host FS automatically — exactly the supported combination.
            working_dir: Some(request.work_dir.clone()),
            // Spend deep thinking where it pays (design, code, verification)
            // and stop the paperwork roles from ruminating a million tokens
            // per run. Steve's rule: "cái nào cần suy nghĩ kỹ thì vẫn phải
            // suy nghĩ kỹ" — engineering keeps High.
            reasoning_effort: Some(effort_for(request.role)),
        });
        let (mut events, mut driver) = handle.split();

        // Drain the live event stream into the work-log trace while driving
        // the run. Dropping `driver` (this future being cancelled from above)
        // cancels the whole run — harxes-core's contract. Text/Reasoning
        // events are STREAMING DELTAS (token fragments) — a coalescer buffers
        // them into whole lines so the live log reads as prose, not one
        // fragment per line.
        let mut trace = String::new();
        let mut co = Coalescer::default();
        let outcome = loop {
            tokio::select! {
                ev = events.recv() => {
                    if let Some(ev) = ev {
                        co.feed(&ev, live.as_deref(), &mut trace);
                    }
                }
                done = &mut driver => break done,
            }
        };
        // Flush any events that raced the driver's completion.
        while let Ok(ev) = events.try_recv() {
            co.feed(&ev, live.as_deref(), &mut trace);
        }
        co.flush(live.as_deref(), &mut trace);

        match outcome {
            Ok(out) => Ok(finish(out, trace, self)),
            Err(e) => Err(PortError::Backend(render_engine_error(&e))),
        }
    }
}

/// Map a completed run onto the port's outcome shape.
fn finish(out: RunOutcome, mut trace: String, engine: &HarxesEngine) -> AgentOutcome {
    // Final accounting line: how much of the spend was deliberation. This is
    // the number the reasoning_effort mapping is judged by.
    if let Some(r) = out.reasoning_tokens {
        use std::fmt::Write as _;
        let _ = writeln!(
            trace,
            "⏱ run total · {} tokens ({} reasoning)",
            fmt_tokens(out.total_tokens),
            fmt_tokens(r)
        );
    }
    let guardrail = matches!(out.stop_reason, harxes_core::StopReason::Guardrail);
    AgentOutcome {
        stdout: out.final_text,
        stderr: if guardrail {
            format!(
                "harxes guardrail: run stopped at iteration/token cap ({} iterations, {} tokens)",
                out.iterations, out.total_tokens
            )
        } else {
            String::new()
        },
        // Guardrail = the loop was cut before the agent finished — a failed
        // attempt (task-side: the budget spent proves the engine worked).
        exit_code: Some(i32::from(guardrail)),
        usage: Some(Usage {
            input_tokens: out.input_tokens,
            output_tokens: out.output_tokens,
            // Pricing is provider-specific config the hub owns; the honest
            // number here is 0, like the copilot adapter (never invent cost).
            cost_usd: 0.0,
        }),
        trace,
        session_id: None,
        sandbox: crate::proc::sandbox_status(engine.sandbox),
        engine: "harxes".to_owned(),
        model: engine.model.clone(),
        attempts: Vec::new(),
    }
}

/// Render a typed engine error in the vocabulary `faults::is_infra_fault`
/// already classifies, so harxes infra faults are recognized without adding
/// engine-specific patterns. The typed source of truth is
/// `EngineError::is_infra_fault()`; the words merely carry it across the
/// string boundary of `PortError::Backend`.
fn render_engine_error(e: &EngineError) -> String {
    match e {
        EngineError::Timeout => "harxes timed out".to_owned(),
        EngineError::ProviderUnavailable(m) => {
            format!("harxes provider service unavailable: {m}")
        }
        EngineError::AuthDead => {
            "harxes provider auth dead (credentials rejected) — service unavailable until re-auth"
                .to_owned()
        }
        EngineError::TaskFailed(m) => format!("harxes task failed: {m}"),
        EngineError::Config(m) => format!("harxes config error: {m}"),
    }
}

/// Reasoning depth by role: engineering thinks hard, coordination thinks
/// briefly. PD sits in the middle — design judgement, but not proofs.
fn effort_for(role: coxagent_domain::Role) -> ReasoningEffort {
    use coxagent_domain::Role;
    match role {
        Role::DevFeature | Role::DevBug | Role::Sa | Role::Test => ReasoningEffort::High,
        Role::Pd => ReasoningEffort::Medium,
        _ => ReasoningEffort::Low,
    }
}

/// A tool duration a person can read: "480ms", "12.4s", "3m05s".
fn fmt_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        {
        #[allow(clippy::cast_precision_loss)] // sub-minute durations fit easily
        let secs = ms as f64 / 1000.0;
        format!("{secs:.1}s")
    }
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Cumulative token counts a person can read: "481k", "1.5M".
fn fmt_tokens(n: u64) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else {
        {
        #[allow(clippy::cast_precision_loss)] // token counts are far below 2^52
        let m = n as f64 / 1_000_000.0;
        format!("{m:.1}M")
    }
    }
}

/// Buffers streaming Text/Reasoning deltas into whole lines; tool events and
/// retries flush the buffer first so ordering is preserved.
///
/// Lines are written in the dashboard's shared work-log line protocol
/// (`parseWorklog` in shell.js): `🧠` thinking, `💬` message start (plain
/// continuation lines attach to the open message), `🔧 name(args)` tool call,
/// `↳` tool result with `┆`-indented preview, `↻` provider retry.
#[derive(Default)]
struct Coalescer {
    /// Pending partial line and whether it is reasoning (true) or text.
    buf: String,
    reasoning: bool,
    /// A `💬` message is open: further plain text lines are continuations.
    msg_open: bool,
}

impl Coalescer {
    fn feed(&mut self, ev: &RunEvent, live_file: Option<&Path>, trace: &mut String) {
        match ev {
            RunEvent::Text(t) | RunEvent::Reasoning(t) => {
                let is_reasoning = matches!(ev, RunEvent::Reasoning(_));
                if self.reasoning != is_reasoning && !self.buf.is_empty() {
                    self.flush(live_file, trace);
                }
                self.reasoning = is_reasoning;
                self.buf.push_str(t);
                while let Some(nl) = self.buf.find('\n') {
                    let line: String = self.buf.drain(..=nl).collect();
                    self.emit(line.trim_end(), live_file, trace);
                }
            }
            RunEvent::ToolStart { name, summary } => {
                self.close_msg(live_file, trace);
                Self::emit_raw(&format!("🔧 {name}({summary})"), live_file, trace);
            }
            RunEvent::ToolEnd {
                name: _,
                summary,
                duration_ms,
                ok,
            } => {
                self.close_msg(live_file, trace);
                // Multi-line result: first line is the inline summary, the
                // rest becomes the click-to-open preview body.
                let mut lines = summary.lines();
                let head = lines.next().unwrap_or_default();
                let mark = if *ok { '✓' } else { '✗' };
                Self::emit_raw(
                    &format!("↳ {mark} {head} · {}", fmt_duration(*duration_ms)),
                    live_file,
                    trace,
                );
                for l in lines {
                    Self::emit_raw(&format!("┆ {l}"), live_file, trace);
                }
            }
            // Cumulative loop progress: the trail that shows how close a run
            // is to its guardrail cap BEFORE it dies there.
            RunEvent::Iteration {
                n,
                input_tokens,
                output_tokens,
                reasoning_tokens,
                context_tokens,
            } => {
                self.close_msg(live_file, trace);
                let reasoning = reasoning_tokens
                    .map(|r| format!(" ({} reasoning)", fmt_tokens(r)))
                    .unwrap_or_default();
                // ctx = this call's context size. Flat/shrinking across
                // iterations proves tool-output aging is working; linear
                // growth means something isn't aging — report it upstream.
                Self::emit_raw(
                    &format!(
                        "⏱ iteration {n} · ctx {} · {} total{reasoning}",
                        fmt_tokens(*context_tokens),
                        fmt_tokens(input_tokens + output_tokens)
                    ),
                    live_file,
                    trace,
                );
            }
            RunEvent::Retry { wait_secs } => {
                self.close_msg(live_file, trace);
                Self::emit_raw(&format!("↻ provider retry in {wait_secs}s"), live_file, trace);
            }
            // RunEvent is #[non_exhaustive]: future variants stream past the
            // live log rather than breaking the build.
            _ => {}
        }
    }

    /// Flush pending text and end the open message block, so the next text
    /// line starts a fresh `💬` bubble instead of gluing onto the old one.
    fn close_msg(&mut self, live_file: Option<&Path>, trace: &mut String) {
        self.flush(live_file, trace);
        self.msg_open = false;
    }

    fn flush(&mut self, live_file: Option<&Path>, trace: &mut String) {
        if self.buf.trim().is_empty() {
            self.buf.clear();
            return;
        }
        let line = std::mem::take(&mut self.buf);
        self.emit(line.trim_end(), live_file, trace);
    }

    fn emit(&mut self, line: &str, live_file: Option<&Path>, trace: &mut String) {
        if line.is_empty() {
            return;
        }
        let rendered = if self.reasoning {
            self.msg_open = false;
            format!("🧠 {line}")
        } else if self.msg_open {
            line.to_owned()
        } else {
            self.msg_open = true;
            format!("💬 {line}")
        };
        Self::emit_raw(&rendered, live_file, trace);
    }

    fn emit_raw(line: &str, live_file: Option<&Path>, trace: &mut String) {
        if let Some(p) = live_file {
            append_live(p, line);
        }
        trace.push_str(line);
        trace.push('\n');
    }
}

/// harxes-core `ShellPort` backed by the hub's own confinement: every Bash
/// command the agent runs is spawned through `proc::agent_command` — the
/// exact seatbelt/nice path the CLI engines get — in its own process group
/// with kill-on-drop, so a cancelled run reaps its tool children.
struct ConfinedShell {
    sandbox: bool,
    /// The workspace this run is confined to. The per-call `working_dir` from
    /// harxes-core is honored as the cwd, but confinement is always anchored
    /// to the run's workspace.
    work_dir: PathBuf,
}

#[async_trait]
impl ShellPort for ConfinedShell {
    async fn run_command(
        &self,
        working_dir: &str,
        cmd: &str,
    ) -> Result<CommandOutput, ShellError> {
        // harxes-core passes "." for "the project" — in-process that would be
        // the HUB's cwd, not this run's workspace. Anchor every relative dir
        // (and any path outside the workspace) to the run's work_dir.
        let wd = std::path::Path::new(working_dir);
        let cwd = if wd.is_relative() {
            if working_dir == "." || working_dir.is_empty() {
                self.work_dir.clone()
            } else {
                self.work_dir.join(wd)
            }
        } else if wd.starts_with(&self.work_dir) {
            wd.to_path_buf()
        } else {
            self.work_dir.clone()
        };
        let (mut command, sandbox) =
            crate::proc::agent_command("/bin/bash", &self.work_dir, self.sandbox);
        command
            .arg("-lc")
            .arg(cmd)
            .current_dir(&cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Spawn through the COX-B013 retry wrapper, not `.spawn()`: macOS
        // Seatbelt's sandbox_apply() fails transiently, and a raw spawn turns
        // that into a silently denied in-workspace write. The exhausted-retry
        // case is already warned by the mechanism itself; ShellPort's
        // CommandOutput has no confinement channel, so the (possibly
        // downgraded) status is not re-surfaced here.
        let (child, _sandbox) = crate::proc::spawn_confined(&mut command, sandbox)
            .await
            .map_err(|e| ShellError::Spawn(e.to_string()))?;
        let pid = child.id();
        // Guard the whole group: if this future is dropped mid-await (run
        // cancelled), kill_on_drop only takes the leader — the group signal
        // reaps grandchildren too (same rationale as proc::kill_group).
        let guard = GroupGuard { pid };
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| ShellError::Io(e.to_string()))?;
        std::mem::forget(guard); // completed normally — nothing to reap
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit_status: match out.status.code() {
                Some(0) => ShellExitStatus::Success,
                Some(c) => ShellExitStatus::Failure(c),
                None => ShellExitStatus::Failure(-1),
            },
        })
    }
}

struct GroupGuard {
    pid: Option<u32>,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            crate::proc::kill_group(pid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_routing_follows_the_model_prefix() {
        let e = HarxesEngine::new("anthropic/claude-sonnet-5");
        assert_eq!(e.provider_model(), "claude-sonnet-5");
        let e = HarxesEngine::new("copilot/gpt-4o");
        assert_eq!(e.provider_model(), "gpt-4o");
        // LiteLLM keeps the full provider/model string — it routes on it.
        let e = HarxesEngine::new("bizbrain/GLM-5.3");
        assert_eq!(e.provider_model(), "bizbrain/GLM-5.3");
    }

    #[test]
    fn infra_faults_render_in_the_classifier_vocabulary() {
        // The words must stay inside faults::is_infra_fault's pattern set —
        // that is the whole contract of render_engine_error.
        for (err, needle) in [
            (EngineError::Timeout, "timed out"),
            (
                EngineError::ProviderUnavailable("502".to_owned()),
                "unavailable",
            ),
            (EngineError::AuthDead, "unavailable"),
        ] {
            let msg = render_engine_error(&err).to_lowercase();
            assert!(msg.contains(needle), "{msg} must contain {needle}");
        }
        // Task-side failures must NOT look like infra.
        let msg = render_engine_error(&EngineError::TaskFailed("tests failed".to_owned()))
            .to_lowercase();
        assert!(!msg.contains("timed out") && !msg.contains("unavailable"));
    }

    #[test]
    fn coalescer_joins_streaming_deltas_into_lines() {
        let mut trace = String::new();
        let mut co = Coalescer::default();
        for d in ["Let", " me", " begin", ".\n", "Next"] {
            co.feed(&RunEvent::Reasoning((*d).to_owned()), None, &mut trace);
        }
        co.feed(
            &RunEvent::ToolStart {
                name: "Bash".to_owned(),
                summary: "ls".to_owned(),
            },
            None,
            &mut trace,
        );
        assert_eq!(
            trace,
            "🧠 Let me begin.\n🧠 Next\n🔧 Bash(ls)\n",
            "deltas coalesce into whole lines; tool events flush first"
        );
    }

    #[test]
    fn engineering_roles_keep_deep_reasoning() {
        use coxagent_domain::Role;
        for r in [Role::DevFeature, Role::DevBug, Role::Sa, Role::Test] {
            assert!(matches!(effort_for(r), ReasoningEffort::High));
        }
        for r in [Role::Docs, Role::Ba, Role::Po, Role::Sm] {
            assert!(matches!(effort_for(r), ReasoningEffort::Low));
        }
        assert!(matches!(effort_for(Role::Pd), ReasoningEffort::Medium));
    }

    #[test]
    fn coalescer_speaks_the_worklog_line_protocol() {
        let mut trace = String::new();
        let mut co = Coalescer::default();
        co.feed(&RunEvent::Text("Done. Summary:\nAll tests pass.\n".to_owned()), None, &mut trace);
        co.feed(
            &RunEvent::ToolEnd {
                name: "Bash".to_owned(),
                summary: "exit=0\n220 passed".to_owned(),
                duration_ms: 12_400,
                ok: true,
            },
            None,
            &mut trace,
        );
        co.feed(&RunEvent::Text("Next step.\n".to_owned()), None, &mut trace);
        co.feed(&RunEvent::Retry { wait_secs: 3 }, None, &mut trace);
        assert_eq!(
            trace,
            "\u{1f4ac} Done. Summary:\nAll tests pass.\n\u{21b3} \u{2713} exit=0 \u{b7} 12.4s\n\u{2506} 220 passed\n\u{1f4ac} Next step.\n\u{21bb} provider retry in 3s\n",
            "first text line opens a \u{1f4ac} bubble, continuations stay plain, tool end splits into summary + preview"
        );
    }

    #[test]
    fn guardrail_maps_to_a_failed_attempt() {
        let out = RunOutcome {
            final_text: "partial".to_owned(),
            transcript: Vec::new(),
            iterations: 40,
            input_tokens: 1,
            output_tokens: 2,
            total_tokens: 3,
            stop_reason: harxes_core::StopReason::Guardrail,
            reasoning_tokens: Some(900_000),
        };
        let mapped = finish(out, String::new(), &HarxesEngine::new("bizbrain/GLM-5.3"));
        assert_eq!(mapped.exit_code, Some(1));
        assert!(mapped.stderr.contains("guardrail"));
        assert!(!mapped.succeeded());
    }
}
