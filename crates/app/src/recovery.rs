//! Recovery of hub projects that failed to load at boot (CXA-B114).
//!
//! `build_project` connects a project's state store (Postgres when
//! configured) exactly once per process. When that connect failed at boot —
//! the database still starting, or auth rejecting until an operator fixes it —
//! the project was parked in the hub's broken list FOREVER: `/api/projects`
//! answered `broken:true`, every project route 404'd, and a database made
//! healthy again was never retried; recovery required a restart.
//!
//! This module closes that. Boot loads get a short retry window; whatever
//! still fails is (a) labelled broken for the dashboard exactly as before
//! (COX-B043) and (b) rebuilt in the background until its store recovers, at
//! which point the live handle is handed to the hub through the `admit`
//! channel and the broken label is cleared — no restart.

use super::*;
use coxagent_presentation::{BrokenProject, ProjectHandle};
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc::Sender;

use super::retry::{
    boot_backoff, recovery_backoff, retrying, retrying_until_recovered, Backoff, BOOT_ATTEMPTS,
};

/// One entry of the hub registry JSON array.
#[derive(serde::Deserialize)]
pub(crate) struct Entry {
    id: String,
    path: PathBuf,
}

/// Error type of a project build — the same boxed error `build_project`
/// returns, so the shared builder is the real wiring, not a reduced view.
pub(crate) type BuildError = Box<dyn std::error::Error>;

/// Assemble one live project handle (store connect, config, engine, runner).
/// Shared by the boot loop and every background retry task so both phases
/// exercise the SAME rebuild path; boxed because the builder is handed to
/// tasks that outlive the boot frame.
pub(crate) type ProjectBuilder = Arc<
    dyn for<'a> Fn(
            &'a str,
            &'a Path,
            PathBuf,
        )
            -> Pin<Box<dyn Future<Output = Result<ProjectHandle, BuildError>> + Send + 'a>>
        + Send
        + Sync,
>;

/// Retry budget for the two phases — parameters rather than constants so the
/// orchestration is testable with zero delays.
#[derive(Clone, Copy)]
pub(crate) struct RecoveryPolicy {
    /// Attempts during boot before an entry is labelled broken.
    pub(crate) boot_attempts: u32,
    /// Delay schedule between boot attempts.
    pub(crate) boot: Backoff,
    /// Delay schedule between background-recovery attempts.
    pub(crate) recovery: Backoff,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        Self {
            boot_attempts: BOOT_ATTEMPTS,
            boot: boot_backoff,
            recovery: recovery_backoff,
        }
    }
}

/// A registry entry whose boot build failed, kept so the background retry
/// rebuilds exactly what boot could not load.
struct FailedProject {
    id: String,
    state_dir: PathBuf,
    work_dir: PathBuf,
}

/// Build every registry entry — the hub's boot registration.
///
/// Each entry gets a short boot retry (a database seconds from ready must not
/// ship a broken project); whatever still fails is labelled broken for the
/// dashboard AND handed to [`spawn_retry`], which keeps rebuilding it until
/// its store recovers and admits the live handle. Returns the live handles
/// and the broken labels.
pub(crate) async fn build_registry(
    entries: Vec<Entry>,
    admit: Sender<ProjectHandle>,
    build: ProjectBuilder,
    policy: RecoveryPolicy,
) -> (Vec<ProjectHandle>, Vec<BrokenProject>) {
    let mut projects = Vec::new();
    let mut broken = Vec::new();
    for e in entries {
        let state_dir = e.path.join("state");
        let work_dir = e.path.join("codebase");
        let what = format!("[{}] project load", e.id);
        match retrying(&what, policy.boot_attempts, policy.boot, || {
            build(&e.id, &state_dir, work_dir.clone())
        })
        .await
        {
            Ok(p) => {
                tracing::info!("hub: registered project '{}'", p.id);
                projects.push(p);
            }
            Err(err) => {
                // Loud, and carried into the dashboard: a project that fails to
                // load has no handle to serve, so without this record it would
                // simply be absent from /api/projects (COX-B043) — and since
                // CXA-B114 it is not the END of the story either.
                tracing::error!(
                    "hub: '{}' failed to load — serving it as broken and retrying in the \
                     background until its store recovers: {err}",
                    e.id
                );
                broken.push(BrokenProject {
                    id: e.id.clone(),
                    config_path: e.path.join("coxagent.json"),
                    error: err.to_string(),
                });
                spawn_retry(
                    FailedProject {
                        id: e.id.clone(),
                        state_dir,
                        work_dir,
                    },
                    policy,
                    Arc::clone(&build),
                    admit.clone(),
                );
            }
        }
    }
    (projects, broken)
}

/// Keep rebuilding one failed project until its store recovers, then hand the
/// live handle to the hub through `admit` (CXA-B114). Runs for as long as the
/// hub does: a store that comes back minutes or hours later still un-breaks
/// its project, no restart.
fn spawn_retry(
    task: FailedProject,
    policy: RecoveryPolicy,
    build: ProjectBuilder,
    admit: Sender<ProjectHandle>,
) {
    tokio::spawn(async move {
        let what = format!("[{}] project load", task.id);
        let handle = retrying_until_recovered(&what, policy.recovery, || {
            build(&task.id, &task.state_dir, task.work_dir.clone())
        })
        .await;
        if admit.send(handle).await.is_err() {
            // The hub server is gone (shutdown): nothing left to register with.
            tracing::info!("[{}] store recovered after the hub stopped", task.id);
        } else {
            tracing::info!("[{}] store recovered — project re-registered live", task.id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
    use coxagent_application::PortError;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    struct StubEngine;

    #[async_trait::async_trait]
    impl AgentEnginePort for StubEngine {
        fn id(&self) -> &'static str {
            "stub"
        }
        async fn run(&self, _request: AgentRequest) -> Result<AgentOutcome, PortError> {
            unreachable!("no agent runs during a recovery test")
        }
    }

    /// A live-looking handle for a recovered project: real JSON store over a
    /// throwaway dir, everything else inert.
    fn stub_handle(id: &str) -> ProjectHandle {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        ProjectHandle {
            id: id.to_owned(),
            name: id.to_owned(),
            alias: String::new(),
            store: Arc::new(coxagent_infrastructure::JsonStateStore::new(&dir).expect("store")),
            runner: Arc::new(coxagent_application::use_cases::RunnerHandle::default()),
            config_path: dir.join("coxagent.json"),
            engine: Arc::new(StubEngine),
            work_dir: dir.clone(),
            budget: Arc::new(std::sync::Mutex::new(
                coxagent_application::BudgetCaps::default(),
            )),
            context_path: dir.join("project_context.md"),
            forge: None,
            deploy: None,
            outbox: None,
            storage: None,
            files: None,
            deps_discovery: None,
        }
    }

    /// A builder that fails the first `failures` attempts and every attempt
    /// while the store is down, plus a call counter — the "database comes
    /// back" story, observable.
    fn stub_builder(failures: u32) -> (ProjectBuilder, Arc<AtomicU32>, Arc<AtomicBool>) {
        let calls = Arc::new(AtomicU32::new(0));
        let up = Arc::new(AtomicBool::new(false));
        let build: ProjectBuilder = {
            let calls = Arc::clone(&calls);
            let up = Arc::clone(&up);
            Arc::new(move |id: &str, _: &Path, _: PathBuf| {
                let calls = Arc::clone(&calls);
                let up = Arc::clone(&up);
                Box::pin(async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if up.load(Ordering::SeqCst) || n > failures {
                        Ok(stub_handle(id))
                    } else {
                        Err::<ProjectHandle, BuildError>(BuildError::from("db error"))
                    }
                })
            })
        };
        (build, calls, up)
    }

    fn zero_policy() -> RecoveryPolicy {
        RecoveryPolicy {
            boot_attempts: 3,
            boot: |_| Duration::ZERO,
            recovery: |_| Duration::ZERO,
        }
    }

    fn one_entry(id: &str) -> Vec<Entry> {
        vec![Entry {
            id: id.to_owned(),
            path: PathBuf::from("/w").join(id),
        }]
    }

    /// A store that is only seconds late never reaches the dashboard as
    /// broken: the boot window absorbs it.
    #[tokio::test]
    async fn a_transient_store_failure_at_boot_never_ships_a_broken_project() {
        let (build, calls, _up) = stub_builder(1); // the DB comes up after one failure
        let (admit, mut rx) = tokio::sync::mpsc::channel(4);

        let (projects, broken) =
            build_registry(one_entry("cxa"), admit, build, zero_policy()).await;

        assert_eq!(projects.len(), 1, "the second attempt succeeded");
        assert!(broken.is_empty(), "no broken label for a transient failure");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(rx.try_recv().is_err(), "nothing needed the background loop");
    }

    /// The ticket's repro, end to end: every boot attempt fails, the project
    /// is labelled broken, the background loop keeps rebuilding, and the live
    /// handle arrives on the admit channel the moment the store recovers.
    #[tokio::test]
    async fn a_project_broken_at_boot_is_re_registered_once_its_store_recovers() {
        let (build, _calls, up) = stub_builder(u32::MAX); // store down for the whole boot window
        let (admit, mut rx) = tokio::sync::mpsc::channel(4);
        let (projects, broken) =
            build_registry(one_entry("late"), admit, build, zero_policy()).await;
        assert!(projects.is_empty());
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].id, "late");
        assert_eq!(broken[0].error, "db error");
        assert!(rx.try_recv().is_err(), "not yet — the store is still down");

        // ...the operator fixes the database...
        up.store(true, Ordering::SeqCst);

        // ...and the very next background attempt re-registers the project.
        let recovered = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("background retry must keep running")
            .expect("a recovered handle");
        assert_eq!(recovered.id, "late");
    }
}
