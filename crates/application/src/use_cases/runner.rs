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
}

/// A reporter the cycle calls as it enters/leaves each agent phase.
pub type PhaseReporter = std::sync::Arc<dyn Fn(Option<(String, String)>) + Send + Sync>;

/// Drive the cycle loop under the handle's control until stopped. Waits while
/// paused; runs one cycle per `step`; sleeps `sleep` between cycles when running.
pub async fn run_forever<S: StateStorePort, E: AgentEnginePort>(
    handle: std::sync::Arc<RunnerHandle>,
    mut cycle_uc: RunCycleUseCase<S, E>,
    sleep: Duration,
) {
    // Let the cycle report which agent is running, live.
    let h = std::sync::Arc::clone(&handle);
    cycle_uc.set_phase_reporter(std::sync::Arc::new(move |info| match info {
        Some((role, note)) => h.set_active(&role, &note),
        None => h.clear_active(),
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
