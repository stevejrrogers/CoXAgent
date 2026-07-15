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
        }
    }
}

impl RunnerHandle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Resume continuous running.
    pub fn resume(&self) {
        self.mode.store(RUNNING, Ordering::SeqCst);
        self.set_mode_label("running");
        self.resume.notify_waiters();
    }

    /// Pause after the current cycle.
    pub fn pause(&self) {
        self.mode.store(PAUSED, Ordering::SeqCst);
        self.set_mode_label("paused");
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
    cycle_uc.set_phase_reporter(std::sync::Arc::new(move |info| {
        // `Some` = entering a phase (role + ticket); `None` = idle between phases.
        // Beat both so the registry reflects reality and never shows stale work.
        let (role, note) = match &info {
            Some((role, note)) => (role.clone(), note.clone()),
            None => ("idle".to_owned(), String::new()),
        };
        match info {
            Some((role, note)) => h.set_active(&role, &note),
            None => h.clear_active(),
        }
        let (store, worker) = (std::sync::Arc::clone(&store), h.worker_id());
        tokio::spawn(async move {
            let now = crate::state::now_rfc3339();
            let _ = store.heartbeat_worker(&worker, &role, &note, &now).await;
        });
    }));
    let mut cycle = 0u64;
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
        // Stamp the live operator identity so ticket claims are owned by whoever
        // resumed this runner, on this host.
        cycle_uc.set_worker(handle.worker_id());
        let report = cycle_uc.run_cycle(cycle).await;
        handle.update(cycle, report.summary());
        handle.clear_active();
        for e in &report.errors {
            tracing::warn!("{e}");
        }

        if report.over_budget {
            tracing::warn!("budget cap reached — pausing loop");
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
