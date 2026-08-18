//! `RunnerHandle` + `run_forever` — a controllable cycle loop that the server
//! hosts in the background so the dashboard can drive it (resume / pause / step)
//! and observe it live. Starts paused, so hosting the loop never burns engine
//! calls until a human resumes it.

use crate::ports::outbound::{AgentEnginePort, StateStorePort};
use crate::use_cases::RunCycleUseCase;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

const PAUSED: u8 = 0;
const RUNNING: u8 = 1;
const STOPPED: u8 = 2;

/// A live, serializable view of the runner for the dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct RunnerSnapshot {
    pub mode: &'static str,
    pub cycle: u64,
    pub last_summary: String,
    /// The agent currently executing (e.g. `"DEV-FEATURE"`), or `None` between
    /// phases. Drives the live "working now" indicator in the dashboard.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_role: Option<String>,
    /// A short note on what the active agent is doing (e.g. a ticket id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_note: Option<String>,
    /// The account that resumed this runner (who the agents are working for).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// The host this runner executes on — the "machine" the agents run on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Shared control + status handle. Cloneable across the server and the loop via
/// `Arc`. All methods are non-blocking.
pub struct RunnerHandle {
    mode: AtomicU8,
    step: AtomicBool,
    resume: Notify,
    status: Mutex<RunnerSnapshot>,
    /// Agent CLIs found on THIS machine's PATH, injected by the composition root
    /// (detection is an infrastructure concern). Reported on every heartbeat so
    /// a hub that will never have an agent CLI of its own can still tell the
    /// dashboard which engines the team can actually run, which models its
    /// opencode reaches, and whether its git/forge credentials really work.
    caps: crate::ports::outbound::WorkerCaps,
}

impl Default for RunnerHandle {
    fn default() -> Self {
        Self {
            mode: AtomicU8::new(PAUSED),
            step: AtomicBool::new(false),
            resume: Notify::new(),
            status: Mutex::new(RunnerSnapshot {
                mode: "paused",
                cycle: 0,
                last_summary: "idle".to_owned(),
                active_role: None,
                active_note: None,
                operator: None,
                host: None,
            }),
            caps: crate::ports::outbound::WorkerCaps::default(),
        }
    }
}

impl RunnerHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare what this machine can run (composition root only): the agent CLIs
    /// on its PATH and the `provider/model` pairs its opencode can reach.
    #[must_use]
    pub fn with_capabilities(mut self, caps: crate::ports::outbound::WorkerCaps) -> Self {
        self.caps = caps;
        self
    }

    /// What this machine can do, as reported on every heartbeat.
    #[must_use]
    pub fn caps(&self) -> &crate::ports::outbound::WorkerCaps {
        &self.caps
    }

    /// Resume continuous running.
    pub fn resume(&self) {
        self.mode.store(RUNNING, Ordering::SeqCst);
        self.set_mode_label("running");
        self.resume.notify_waiters();
    }

    /// Whether the runner is currently paused — polled by the cycle BETWEEN
    /// phases so a user's Pause takes effect at the next phase boundary (after
    /// the in-flight engine call), not after the whole multi-agent cycle.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.mode.load(Ordering::SeqCst) == PAUSED
    }

    /// Pause: no new phases start; the in-flight engine call finishes first.
    pub fn pause(&self) {
        self.mode.store(PAUSED, Ordering::SeqCst);
        self.set_mode_label("paused");
        // The phase label outlives the cycle it belonged to: pausing mid-cycle
        // left "SA · reviewing PRs" on the dashboard beside a paused runner,
        // which reads as "you asked it to stop and it ignored you".
        self.clear_active();
    }

    /// Run exactly one cycle, then pause.
    pub fn step(&self) {
        self.step.store(true, Ordering::SeqCst);
        self.resume.notify_waiters();
    }

    /// Stop the loop for good.
    pub fn stop(&self) {
        self.mode.store(STOPPED, Ordering::SeqCst);
        self.set_mode_label("stopped");
        self.resume.notify_waiters();
    }

    /// Current status snapshot.
    #[must_use]
    pub fn snapshot(&self) -> RunnerSnapshot {
        match self.status.lock() {
            Ok(s) => s.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set_mode_label(&self, label: &'static str) {
        if let Ok(mut s) = self.status.lock() {
            s.mode = label;
        }
    }

    fn update(&self, cycle: u64, summary: String) {
        if let Ok(mut s) = self.status.lock() {
            s.cycle = cycle;
            s.last_summary = summary;
        }
    }

    /// Mark the agent currently executing (live "working now" signal).
    pub fn set_active(&self, role: &str, note: &str) {
        if let Ok(mut s) = self.status.lock() {
            s.active_role = Some(role.to_owned());
            s.active_note = (!note.is_empty()).then(|| note.to_owned());
        }
    }

    /// Clear the active agent (between phases / cycle end).
    pub fn clear_active(&self) {
        if let Ok(mut s) = self.status.lock() {
            s.active_role = None;
            s.active_note = None;
        }
    }

    /// Record who resumed this runner and on which host. Drives the per-card
    /// "account@host" attribution and the ticket claim owner.
    pub fn set_operator(&self, account: &str, host: &str) {
        if let Ok(mut s) = self.status.lock() {
            s.operator = (!account.is_empty()).then(|| account.to_owned());
            s.host = (!host.is_empty()).then(|| host.to_owned());
        }
    }

    /// This runner's claim identity, `account@host` (falls back to `host` alone,
    /// then `"local"`), for stamping ticket claims.
    #[must_use]
    pub fn worker_id(&self) -> String {
        let s = self.snapshot();
        match (s.operator, s.host) {
            (Some(a), Some(h)) => format!("{a}@{h}"),
            (Some(a), None) => a,
            (None, Some(h)) => h,
            (None, None) => "local".to_owned(),
        }
    }
}

/// A reporter the cycle calls as it enters/leaves each agent phase.
pub type PhaseReporter = std::sync::Arc<dyn Fn(Option<(String, String)>) + Send + Sync>;

/// Drive the cycle loop under the handle's control until stopped. Waits while
/// paused; runs one cycle per `step`; sleeps `sleep` between cycles when running.
#[allow(clippy::too_many_lines)] // one linear supervision loop; splitting hurts readability
pub async fn run_forever<S: StateStorePort + 'static, E: AgentEnginePort>(
    handle: std::sync::Arc<RunnerHandle>,
    mut cycle_uc: RunCycleUseCase<S, E>,
    sleep: Duration,
) {
    // Let the cycle report which agent is running, live — both to the local
    // snapshot AND to the shared worker registry (with the real role + ticket),
    // so every dashboard, on any machine, shows which account is running which
    // agent on which ticket.
    let h = std::sync::Arc::clone(&handle);
    let store = cycle_uc.store();
    // The live phase, shared with a keepalive task. A single engine call can run
    // for tens of minutes; the registry TTL is a few minutes, so without a
    // mid-phase refresh a busy operator would drop off every dashboard and look
    // dead. The keepalive re-beats the current phase periodically to fix that.
    let phase: std::sync::Arc<std::sync::Mutex<(String, String)>> =
        std::sync::Arc::new(std::sync::Mutex::new(("idle".to_owned(), String::new())));
    {
        let (store, h, phase) = (
            std::sync::Arc::clone(&store),
            std::sync::Arc::clone(&h),
            std::sync::Arc::clone(&phase),
        );
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(45)).await;
                let (role, note) = phase
                    .lock()
                    .map_or_else(|_| ("idle".to_owned(), String::new()), |p| p.clone());
                // Only a runner actually mid-phase belongs in the registry from
                // here. This loop also backs the hub's own built-in runner,
                // which sits paused by default — beating while idle filled the
                // registry with a phantom worker (no engines, no work) on every
                // hub. A headless operator advertises itself from `run_loop`
                // instead, which is the process that really has the agent CLIs.
                if role != "idle" {
                    let now = crate::state::now_rfc3339();
                    let _ = store
                        .heartbeat_worker(&h.worker_id(), &role, &note, h.caps(), &now)
                        .await;
                }
            }
        });
    }
    cycle_uc.set_phase_reporter(std::sync::Arc::new(move |info| {
        // `Some` = entering a phase (role + ticket); `None` = idle between phases.
        // Beat both so the registry reflects reality and never shows stale work.
        let (role, note) = match &info {
            Some((role, note)) => (role.clone(), note.clone()),
            None => ("idle".to_owned(), String::new()),
        };
        if let Ok(mut p) = phase.lock() {
            *p = (role.clone(), note.clone());
        }
        match info {
            Some((role, note)) => h.set_active(&role, &note),
            None => h.clear_active(),
        }
        let (store, worker) = (std::sync::Arc::clone(&store), h.worker_id());
        let caps = h.caps().clone();
        tokio::spawn(async move {
            let now = crate::state::now_rfc3339();
            let _ = store
                .heartbeat_worker(&worker, &role, &note, &caps, &now)
                .await;
        });
    }));
    {
        let h = std::sync::Arc::clone(&handle);
        cycle_uc.set_pause_check(std::sync::Arc::new(move || h.is_paused()));
    }
    let breaker_store = cycle_uc.store();
    let mut cycle = 0u64;
    // Circuit breaker: engine-infrastructure outages (revoked auth, network
    // down) make every cycle fail fast with zero progress. Instead of spinning
    // forever — 12 empty cycles during a real 401 outage — three consecutive
    // no-progress cycles whose errors look infrastructural pause the runner
    // and tell the humans, exactly like the budget cap does.
    let mut infra_streak = 0u32;
    loop {
        // Gate: wait until running or a step is requested; exit if stopped.
        let stepping = loop {
            match handle.mode.load(Ordering::SeqCst) {
                STOPPED => return,
                RUNNING => break false,
                _ => {
                    if handle.step.swap(false, Ordering::SeqCst) {
                        break true;
                    }
                    handle.resume.notified().await;
                }
            }
        };

        cycle += 1;
        // Hot-reload the engine/config at the cycle boundary when coxagent.json
        // changed (a Settings save) — this is the HUB's runner path, so without
        // this only headless `coxagent run` picked up edits without a restart.
        // On a reload, drop the per-role observed-engine records: they describe
        // the OLD stack, and the dashboard kept showing "copilot" on cards long
        // after the user had switched everything to claude.
        if cycle_uc.maybe_reload() {
            let store = cycle_uc.store();
            let _ = crate::ports::outbound::mutate_state(store.as_ref(), |s| {
                s.spend.engine_by_role.clear();
                s.spend.operator_by_role.clear();
                Ok(())
            })
            .await;
        }
        // Stamp the live operator identity so ticket claims are owned by whoever
        // resumed this runner, on this host.
        cycle_uc.set_worker(handle.worker_id());
        // Publish the cycle as it STARTS, not only when it ends. A cycle runs
        // for many minutes; until now the dashboard kept showing the previous
        // number (0 after a restart) with no active role, so a working team
        // looked dead — the single most common "are the agents running?"
        // question.
        handle.update(cycle, format!("cycle {cycle} — running"));
        // One cycle drives every role; boxing keeps that 16KB future off the
        // loop's own stack frame.
        let report = Box::pin(cycle_uc.run_cycle(cycle)).await;
        handle.update(cycle, report.summary());
        handle.clear_active();
        for e in &report.errors {
            tracing::warn!("{e}");
        }

        if report.over_budget {
            tracing::warn!("budget cap reached — pausing loop");
            cycle_uc
                .notify("loop_paused", "loop paused: spend cap reached".to_owned())
                .await;
            handle.pause();
            continue;
        }
        let progressed = !report.ba_created.is_empty()
            || report.sa_readied.is_some()
            || report.feature_done.is_some()
            || report.bug_fixed.is_some()
            || report.documented.is_some();
        let infra_faults: Vec<&String> = report
            .errors
            .iter()
            .filter(|e| crate::faults::is_infra_fault(e))
            .collect();
        let infra_errors = infra_faults.len();
        // Raise the outage where EVERY role passes through. The first version
        // of this listened only inside the developer's failure path, so a
        // revoked token that failed the whole team left engine_incidents empty
        // and the dashboard clean while nothing worked at all.
        {
            let engine = cycle_uc.engine_id().to_owned();
            let first = infra_faults.first().map(|e| (*e).clone());
            let fault_count = infra_errors;
            // Webhook mirror of the chat announcements below: fires only on the
            // open/close EDGE, so a night-long outage is one message, not one
            // per cycle.
            let was_open = breaker_store
                .load()
                .await
                .is_ok_and(|s| s.engine_incidents.iter().any(|i| i.engine == engine));
            if let Some(detail) = &first {
                // Auth deaths alarm on FIRST sight: revoked credentials never
                // heal without a person, so waiting to count cycles just burns
                // quiet hours. Everything else keeps the >=2 debounce.
                let urgent = crate::faults::is_auth_death(detail);
                if !was_open && (fault_count >= 2 || urgent) {
                    let hint = if urgent {
                        " — credentials are dead; re-login the engine CLI (e.g. `claude login`) and the loop resumes"
                    } else {
                        ""
                    };
                    cycle_uc
                        .notify(
                            "engine_incident",
                            format!(
                                "{engine} failed {fault_count} run(s) this cycle: {detail}{hint}"
                            ),
                        )
                        .await;
                }
            }
            // Life is EVIDENCE, not absence of failure: a cycle that shipped
            // something, or whose failures were all task-shaped (the engine
            // answered and was wrong), proves the engine lives. A SILENT cycle
            // — nothing ran, nothing failed — proves nothing: the 2026-08-17
            // OAuth death opened an incident at 01:33 and the next empty cycle
            // closed it again, so the hub spent the night dead with a clean
            // dashboard and no webhook.
            let engine_alive = progressed || (infra_errors == 0 && !report.errors.is_empty());
            if first.is_none() && was_open && engine_alive {
                cycle_uc
                    .notify(
                        "engine_recovered",
                        format!("{engine} is answering again — work resumes"),
                    )
                    .await;
            }
            let _ = crate::ports::outbound::mutate_state(breaker_store.as_ref(), move |s| {
                if let Some(detail) = &first {
                    let already = s.engine_incidents.iter().any(|i| i.engine == engine);
                    s.open_engine_incident(&engine, "CYCLE", detail);
                    // Announce once, and only when the whole cycle went down —
                    // a single dropped connection is a blip the retry handles,
                    // and shouting about it teaches people to ignore the alert
                    // that matters.
                    if !already && (fault_count >= 2 || crate::faults::is_auth_death(detail)) {
                        let msg = format!(
                            "🔌 {engine} failed {fault_count} run(s) this cycle: {detail}. If it \
                             keeps up, the loop pauses itself — fix the credentials or the model \
                             and this clears."
                        );
                        s.post_chat_in("SYSTEM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                    }
                } else if engine_alive {
                    // The engine PROVABLY answered this cycle (work landed, or
                    // failures were task-shaped). Only that closes the banner —
                    // an idle cycle with zero runs closes nothing, or an
                    // overnight outage clears its own alarm (2026-08-17).
                    if let Some(inc) = s.close_engine_incident(&engine) {
                        let msg = format!(
                            "✅ {engine} is answering again after {} failed run(s) — resolved, \
                             work resumes.",
                            inc.hits
                        );
                        s.post_chat_in("SYSTEM", &msg, crate::state::AGENTS_CHANNEL, Vec::new());
                    }
                }
                Ok(())
            })
            .await;
        }
        // Streak bookkeeping. Reset only on REAL signal that the engine lives:
        // work shipped, or runs that failed for non-infra reasons (the engine
        // answered, the task was wrong). A SILENT cycle — nothing claimed,
        // nothing failed — keeps the streak: it used to reset it, and with the
        // leader lease rotating across three runners no process ever saw three
        // loud cycles in a row, so the loop spun all night against a dead
        // provider without ever tripping this breaker.
        // ONE infra fault in a no-progress cycle counts: overnight the dead
        // engine produced exactly one fault per cycle (a single DEV attempt),
        // so a >=2 threshold meant the streak never grew and the loop spun
        // dead until morning (2026-08-17). Three consecutive such cycles are
        // still required before the pause — a lone transient blip cycle gets
        // reset by the next alive cycle.
        if !progressed && infra_errors >= 1 {
            infra_streak += 1;
        } else if progressed || (infra_errors == 0 && !report.errors.is_empty()) {
            infra_streak = 0;
        }
        if infra_streak >= 3 {
            tracing::warn!(
                "engine infrastructure appears DOWN (3 consecutive no-progress cycles with \
                 infra-looking failures) — pausing the loop; resume once auth/network is back"
            );
            let _ = crate::ports::outbound::mutate_state(breaker_store.as_ref(), |s| {
                let msg = "🔌 Engine infrastructure looks DOWN (auth/network) — loop paused \
                           after 3 empty cycles. Fix the outage, then Resume.";
                s.post_comment("SM", msg, None);
                s.post_chat_in("SM", msg, crate::state::AGENTS_CHANNEL, Vec::new());
                Ok(())
            })
            .await;
            infra_streak = 0;
            cycle_uc
                .notify(
                    "loop_paused",
                    "loop paused: engine infrastructure looks DOWN (auth/network) after 3 \
                     empty cycles — fix the outage, then Resume"
                        .to_owned(),
                )
                .await;
            handle.pause();
            continue;
        }
        if stepping {
            handle.pause();
        } else {
            tokio::select! {
                () = tokio::time::sleep(sleep) => {}
                () = handle.resume.notified() => {}
            }
        }
    }
}
