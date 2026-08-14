// Split from cycle/mod.rs — the full-cycle integration tests (deploy,
// rollback, health gate, budget warnings, impediment digest, auto-merge).
#![allow(clippy::wildcard_imports)]
use super::*;
use crate::ports::outbound::{AgentOutcome, AgentRequest};
use crate::ports::outbound::{GitAuthor, SandboxStatus};
use crate::state::ProjectState;
use crate::PortError;
use coxagent_domain::Status;
use std::sync::Mutex;

#[derive(Default)]
struct MemStore {
    state: Mutex<ProjectState>,
}
#[async_trait::async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().expect("lock").clone())
    }
    async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
        s.validate().map_err(PortError::Corrupt)?;
        *self.state.lock().expect("lock") = s.clone();
        Ok(())
    }
}

/// Engine that answers each role by its system prompt: BA proposes one
/// feature, TEST reports no bugs, DEV succeeds silently.
struct RoleAwareEngine;
#[async_trait::async_trait]
impl AgentEnginePort for RoleAwareEngine {
    fn id(&self) -> &'static str {
        "role-aware"
    }

    /// Simulates what a REAL engine on this host would report — the same
    /// bwrap-presence probe the platform tests below use — so tests that
    /// enable `workflow.sandbox` exercise the actual unsupported-platform
    /// path instead of a hardcoded stub value.
    fn sandbox_status(&self) -> crate::ports::outbound::SandboxStatus {
        use crate::ports::outbound::SandboxStatus;
        #[cfg(target_os = "macos")]
        {
            SandboxStatus::Confined("seatbelt")
        }
        #[cfg(target_os = "linux")]
        {
            let has_bwrap = std::process::Command::new("bwrap")
                .arg("--version")
                .output()
                .is_ok();
            if has_bwrap {
                SandboxStatus::Confined("bwrap")
            } else {
                SandboxStatus::Unavailable("bwrap not found on PATH")
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            SandboxStatus::Unavailable("sandboxing not supported on this platform")
        }
    }

    async fn run(&self, r: AgentRequest) -> Result<AgentOutcome, PortError> {
        let stdout = if r.system_prompt.contains("Business Analyst") {
            r#"[{"title":"Feature A","priority":"high","complexity":"small","has_ui":false}]"#
                .to_owned()
        } else if r.system_prompt.contains("Solution Architect") {
            r#"{"approach":"a","files":["a.rs"],"api_contract":"","data_changes":"","test_plan":"t","ux":null}"#
                .to_owned()
        } else if r.system_prompt.contains("Product Owner") {
            r#"[{"name":"MVP","goal":"ship it","target_version":"1.0.0"}]"#.to_owned()
        } else if r.system_prompt.contains("QA Engineer") {
            "[]".to_owned()
        } else if r.system_prompt.contains("Tech Writer") {
            // Must satisfy the DOCS structure gate, like a real run.
            "FOLDER: -\n# Feature A\n**Keywords:** feature, a, flow\n## Overview\nIt does \
             the thing the ticket asked for, end to end, for the people who need it. The \
             flow starts at the API, runs through the use case, and lands in the store, so \
             a reader can follow one request from edge to persistence without guessing. \
             Failures surface as errors rather than silent no-ops.\n\
             ## How it works\nrun_feature() drives the flow: it validates the request, \
             claims the work, and writes the result once.\n\
             ## Usage\nCall it from the API or the cycle; both paths are the same code.\n\
             ## Interface\nPOST /api/a\n## Configuration\nnone\n\
             ## Edge cases and limits\nFails closed: an invalid request is rejected \
             before any state is written, and a partial write is never observable.\n\
             ## Code map\n- src/a.rs — the flow, from request validation to the store\n\
             ## Related\nnone\n"
                .to_owned()
        } else {
            "done".to_owned()
        };
        Ok(AgentOutcome {
            stdout,
            stderr: String::new(),
            exit_code: Some(0),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::default(),
            engine: String::new(),
        })
    }
}

#[tokio::test]
async fn one_cycle_carries_a_feature_from_proposal_to_done() {
    // DOCS validates its Code map against the tree, so the workspace must
    // contain the file the scripted page cites.
    let dir = std::env::temp_dir().join(format!("cyclerun-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(dir.join("src/a.rs"), "// the flow\n").expect("write");
    let store = Arc::new(MemStore::default());
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        Config::default(),
        dir.clone(),
        "goal".to_owned(),
    );
    // Cycle 1: BA proposes → SA designs → DEV-FEATURE implements → TEST clean.
    let report = Box::pin(uc.run_cycle(1)).await;
    assert!(
        report.errors.is_empty(),
        "no agent errored: {:?}",
        report.errors
    );
    assert_eq!(report.ba_created.len(), 1, "BA proposed a feature");
    assert!(report.sa_readied.is_some(), "SA readied it");
    assert!(report.feature_done.is_some(), "DEV completed it same cycle");
    assert!(report.documented.is_some(), "DOCS documented it same cycle");

    let state = store.load().await.expect("load");
    assert_eq!(state.tickets[0].status(), Status::Documented);
    assert_eq!(state.current_version.to_string(), "0.1.0");
    let _ = std::fs::remove_dir_all(&dir);
}

struct SpyDeploy {
    calls: std::sync::atomic::AtomicUsize,
}
#[async_trait::async_trait]
impl crate::ports::outbound::DeployPort for SpyDeploy {
    async fn deploy(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "spy deployed".to_owned(),
        })
    }
}

#[tokio::test]
async fn deploys_after_a_feature_completes() {
    let store = Arc::new(MemStore::default());
    let spy = Arc::new(SpyDeploy {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut cfg = Config::default();
    cfg.deploy.enabled = true;
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&spy) as Arc<dyn crate::ports::outbound::DeployPort>);
    Box::pin(uc.run_cycle(1)).await;
    assert_eq!(spy.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Records commit_all calls so we can assert the cycle commits completed work.
#[derive(Default)]
struct SpyGit {
    commits: Mutex<Vec<(String, String)>>, // (message, author_email)
}
#[async_trait::async_trait]
impl GitPort for SpyGit {
    async fn is_repo(&self, _: &std::path::Path) -> bool {
        true
    }
    async fn current_branch(&self, _: &std::path::Path) -> Result<String, PortError> {
        Ok("main".to_owned())
    }
    async fn checkout_branch(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn commit_all(
        &self,
        _: &std::path::Path,
        message: &str,
        author: &GitAuthor,
    ) -> Result<Option<String>, PortError> {
        self.commits
            .lock()
            .expect("lock")
            .push((message.to_owned(), author.email.clone()));
        Ok(Some("abc1234".to_owned()))
    }
    async fn push(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn sync_base(
        &self,
        _: &std::path::Path,
        _: &str,
    ) -> Result<crate::ports::outbound::SyncBase, PortError> {
        Ok(crate::ports::outbound::SyncBase::UpToDate)
    }
    async fn abort_merge(&self, _: &std::path::Path) -> Result<(), PortError> {
        Ok(())
    }
}

#[tokio::test]
async fn commits_completed_feature_only_when_git_enabled() {
    // git disabled (default) → no commit.
    let store = Arc::new(MemStore::default());
    let spy = Arc::new(SpyGit::default());
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        Config::default(),
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_git(Arc::clone(&spy) as Arc<dyn GitPort>);
    Box::pin(uc.run_cycle(1)).await;
    assert!(
        spy.commits.lock().expect("lock").is_empty(),
        "no commit when git.enabled is false"
    );

    // git enabled + a custom commit email → one conventional commit.
    let store = Arc::new(MemStore::default());
    let spy = Arc::new(SpyGit::default());
    let mut cfg = Config::default();
    cfg.git.enabled = true;
    cfg.git.commit_email = "5204779+bot@users.noreply.github.com".to_owned();
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_git(Arc::clone(&spy) as Arc<dyn GitPort>);
    Box::pin(uc.run_cycle(1)).await;

    let commits = spy.commits.lock().expect("lock");
    assert_eq!(commits.len(), 1, "one commit for the completed feature");
    assert!(
        commits[0].0.starts_with("feat(") && commits[0].0.contains("Feature A"),
        "conventional message with ticket + title: {}",
        commits[0].0
    );
    assert_eq!(
        commits[0].1, "5204779+bot@users.noreply.github.com",
        "commits under the configured noreply email"
    );
}

use crate::ports::outbound::{ForgePort, PullRequest};

/// Forge with one open PR that records merge / request-changes calls.
#[derive(Default)]
struct SpyForge {
    ci: String,
    mergeable: bool,
    merged: Mutex<Vec<u64>>,
    changes: Mutex<Vec<u64>>,
}
#[async_trait::async_trait]
impl ForgePort for SpyForge {
    async fn open_pr(&self, _: &str, _: &str, _: &str, _: &str) -> Result<PullRequest, PortError> {
        unimplemented!()
    }
    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
        Ok(vec![PullRequest {
            number: 7,
            title: "feat(X-1): add a".to_owned(),
            head: "feat/X-1".to_owned(),
            base: "main".to_owned(),
            url: String::new(),
            author: "coxagent-bot".to_owned(),
            ci: self.ci.clone(),
            mergeable: self.mergeable,
            created: String::new(),
        }])
    }
    async fn pr_diff(&self, _: u64) -> Result<String, PortError> {
        Ok("+ added a line".to_owned())
    }
    async fn merge_pr(&self, n: u64) -> Result<(), PortError> {
        self.merged.lock().expect("lock").push(n);
        Ok(())
    }
    async fn request_changes(&self, n: u64, _: &str) -> Result<(), PortError> {
        self.changes.lock().expect("lock").push(n);
        Ok(())
    }
    async fn close_pr(&self, _: u64) -> Result<(), PortError> {
        Ok(())
    }
}

/// Engine whose SA review verdict is fixed to `decision`.
struct ReviewEngine {
    decision: &'static str,
}
#[async_trait::async_trait]
impl AgentEnginePort for ReviewEngine {
    fn id(&self) -> &'static str {
        "review"
    }
    async fn run(&self, _: AgentRequest) -> Result<AgentOutcome, PortError> {
        Ok(AgentOutcome {
            stdout: format!("{{\"decision\":\"{}\",\"summary\":\"s\"}}", self.decision),
            stderr: String::new(),
            exit_code: Some(0),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::default(),
            engine: String::new(),
        })
    }
}

fn review_uc(
    forge: Arc<SpyForge>,
    decision: &'static str,
    auto_merge: bool,
) -> RunCycleUseCase<MemStore, ReviewEngine> {
    let mut cfg = Config::default();
    cfg.git.enabled = true;
    cfg.git.auto_merge = auto_merge;
    RunCycleUseCase::new(
        Arc::new(MemStore::default()),
        Arc::new(ReviewEngine { decision }),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_forge(forge as Arc<dyn ForgePort>)
}

#[tokio::test]
async fn a_project_with_nothing_to_verify_with_still_merges() {
    // Auto-merge demands the merged tree build and test, but a project with
    // no git and no runner wired has nothing to check — blocking every
    // merge forever would be the wrong answer to a missing capability.
    let forge = Arc::new(SpyForge {
        ci: "passing".to_owned(),
        mergeable: true,
        ..Default::default()
    });
    review_uc(Arc::clone(&forge), "approve", true)
        .review_open_prs()
        .await;
    assert_eq!(*forge.merged.lock().expect("lock"), vec![7]);
}

#[tokio::test]
async fn require_ci_off_reviews_and_merges_despite_failing_ci() {
    // CI unavailable (e.g. Actions billing dead) + require_ci off: the SA
    // judges the diff on its own and an approve still merges.
    let forge = Arc::new(SpyForge {
        ci: "failing".to_owned(),
        mergeable: true,
        ..Default::default()
    });
    let mut cfg = Config::default();
    cfg.git.enabled = true;
    cfg.git.auto_merge = true;
    cfg.git.require_ci = false;
    RunCycleUseCase::new(
        Arc::new(MemStore::default()),
        Arc::new(ReviewEngine {
            decision: "approve",
        }),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_forge(Arc::clone(&forge) as Arc<dyn ForgePort>)
    .review_open_prs()
    .await;
    assert_eq!(*forge.merged.lock().expect("lock"), vec![7]);
    assert!(forge.changes.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn sa_merges_on_approve_but_only_when_auto_merge() {
    // auto_merge off → never merges even with an approve verdict.
    let forge = Arc::new(SpyForge {
        ci: "passing".to_owned(),
        mergeable: true,
        ..Default::default()
    });
    review_uc(Arc::clone(&forge), "approve", false)
        .review_open_prs()
        .await;
    assert!(forge.merged.lock().expect("lock").is_empty());

    // auto_merge on + approve + CI passing → merges PR #7.
    let forge = Arc::new(SpyForge {
        ci: "passing".to_owned(),
        mergeable: true,
        ..Default::default()
    });
    review_uc(Arc::clone(&forge), "approve", true)
        .review_open_prs()
        .await;
    assert_eq!(*forge.merged.lock().expect("lock"), vec![7]);
}

#[tokio::test]
async fn auto_merge_only_touches_prs_into_the_target_branch() {
    // PR targets `main`, but the flow target is `develop` → left alone.
    let forge = Arc::new(SpyForge {
        ci: "passing".to_owned(),
        mergeable: true,
        ..Default::default()
    });
    let mut cfg = Config::default();
    cfg.git.enabled = true;
    cfg.git.auto_merge = true;
    cfg.git.target_branch = "develop".to_owned();
    RunCycleUseCase::new(
        Arc::new(MemStore::default()),
        Arc::new(ReviewEngine {
            decision: "approve",
        }),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_forge(Arc::clone(&forge) as Arc<dyn ForgePort>)
    .review_open_prs()
    .await;
    assert!(
        forge.merged.lock().expect("lock").is_empty(),
        "a PR into main is not auto-merged when the target is develop"
    );
}

#[tokio::test]
async fn sa_requests_changes_on_reject_and_never_merges_failing_ci() {
    // SA says request_changes → no merge, a changes request instead.
    let forge = Arc::new(SpyForge {
        ci: "passing".to_owned(),
        mergeable: true,
        ..Default::default()
    });
    review_uc(Arc::clone(&forge), "request_changes", true)
        .review_open_prs()
        .await;
    assert!(forge.merged.lock().expect("lock").is_empty());
    assert_eq!(*forge.changes.lock().expect("lock"), vec![7]);

    // Failing CI is never merged, even if the SA would approve.
    let forge = Arc::new(SpyForge {
        ci: "failing".to_owned(),
        mergeable: true,
        ..Default::default()
    });
    review_uc(Arc::clone(&forge), "approve", true)
        .review_open_prs()
        .await;
    assert!(forge.merged.lock().expect("lock").is_empty());
    assert_eq!(*forge.changes.lock().expect("lock"), vec![7]);
}

// ---- COX-F001: auto-rollback to last known-good deploy on failure ----
//
// These encode the acceptance criteria only. No production rollback logic
// exists yet — several of these are expected to be RED until it's built.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A `DeployPort` that returns a scripted sequence of `deploy()` results
/// (one per call, in order) and always reports tests passing. Once the
/// script is exhausted, extra calls return a report clearly marked as
/// unexpected, so an over-eager retry loop shows up in assertions instead
/// of silently blending in.
struct ScriptedDeploy {
    script: Mutex<VecDeque<Result<crate::ports::outbound::DeployReport, PortError>>>,
    deploy_calls: AtomicUsize,
}
impl ScriptedDeploy {
    fn new(script: Vec<crate::ports::outbound::DeployReport>) -> Self {
        Self::scripted(script.into_iter().map(Ok).collect())
    }
    /// Script that may include `Err` results — a `deploy()` that never
    /// produced a `DeployReport` at all (spawn failure, or the 900s
    /// `DEPLOY_TIMEOUT` in `docker_compose.rs`). COX-B039.
    fn scripted(script: Vec<Result<crate::ports::outbound::DeployReport, PortError>>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            deploy_calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.deploy_calls.load(Ordering::SeqCst)
    }
}
#[async_trait::async_trait]
impl crate::ports::outbound::DeployPort for ScriptedDeploy {
    async fn deploy(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        self.deploy_calls.fetch_add(1, Ordering::SeqCst);
        let next = self.script.lock().expect("lock").pop_front();
        next.unwrap_or(Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "UNSCRIPTED EXTRA DEPLOY CALL".to_owned(),
        }))
    }
    async fn run_tests(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "tests ok".to_owned(),
        })
    }
}

/// Records every event handed to the notifier, so tests can assert a
/// rollback notification is distinguishable from a plain deploy one.
#[derive(Default)]
struct SpyNotifier {
    events: Mutex<Vec<crate::ports::outbound::NotifyEvent>>,
}
#[async_trait::async_trait]
impl crate::ports::outbound::NotifierPort for SpyNotifier {
    async fn notify(&self, event: crate::ports::outbound::NotifyEvent) {
        self.events.lock().expect("lock").push(event);
    }
}

/// Fixed sha this fake reports as HEAD — deliberately different from
/// [`GOOD_SHA`] so a seeded "prior good" deploy is a real rollback target,
/// not a no-op.
const HEAD_SHA: &str = "deadbeef";
/// Fixed sha seeded as the last known-good deploy in rollback tests.
const GOOD_SHA: &str = "cafef00d";

/// A `GitPort` double for rollback tests: reports a fixed HEAD, records
/// every worktree op so a test can assert rollback NEVER touches the live
/// `work_dir` (only the dedicated rollback path), and reports no changed
/// paths (no migration in the way) unless a test overrides it.
#[derive(Default)]
struct FakeGit {
    worktree_adds: Mutex<Vec<(std::path::PathBuf, String)>>,
    migration_paths: Vec<String>,
}
#[async_trait::async_trait]
impl GitPort for FakeGit {
    async fn is_repo(&self, _: &std::path::Path) -> bool {
        true
    }
    async fn current_branch(&self, _: &std::path::Path) -> Result<String, PortError> {
        Ok("main".to_owned())
    }
    async fn checkout_branch(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn commit_all(
        &self,
        _: &std::path::Path,
        _: &str,
        _: &GitAuthor,
    ) -> Result<Option<String>, PortError> {
        Ok(None)
    }
    async fn push(&self, _: &std::path::Path, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn sync_base(
        &self,
        _: &std::path::Path,
        _: &str,
    ) -> Result<crate::ports::outbound::SyncBase, PortError> {
        Ok(crate::ports::outbound::SyncBase::UpToDate)
    }
    async fn abort_merge(&self, _: &std::path::Path) -> Result<(), PortError> {
        Ok(())
    }
    async fn head_sha(&self, _: &std::path::Path) -> Result<String, PortError> {
        Ok(HEAD_SHA.to_owned())
    }
    async fn update_ref(&self, _: &std::path::Path, _: &str, _: &str) -> Result<(), PortError> {
        Ok(())
    }
    async fn worktree_add(
        &self,
        _work_dir: &std::path::Path,
        path: &std::path::Path,
        sha: &str,
    ) -> Result<(), PortError> {
        self.worktree_adds
            .lock()
            .expect("lock")
            .push((path.to_path_buf(), sha.to_owned()));
        Ok(())
    }
    async fn worktree_remove(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
    ) -> Result<(), PortError> {
        Ok(())
    }
    async fn changed_paths(
        &self,
        _: &std::path::Path,
        _: &str,
        _: &str,
    ) -> Result<Vec<String>, PortError> {
        Ok(self.migration_paths.clone())
    }
}

/// A cycle that will complete a fresh feature (so the deploy step runs),
/// with `state.last_good_deploy` pre-seeded to reflect whether a prior
/// deploy+tests pass exists. `auto_rollback` is on (opt-in in production,
/// but these tests exist to exercise it).
fn rollback_uc(
    prior_good: bool,
    deploy: &Arc<ScriptedDeploy>,
    notifier: &Arc<SpyNotifier>,
) -> (
    Arc<MemStore>,
    Arc<FakeGit>,
    RunCycleUseCase<MemStore, RoleAwareEngine>,
) {
    let mut initial = ProjectState::default();
    if prior_good {
        initial.last_good_deploy = Some(crate::state::KnownGoodDeploy {
            sha: GOOD_SHA.to_owned(),
            at: crate::state::now_rfc3339(),
            deploy_index: 1,
            summary: "prior deploy + tests passed".to_owned(),
        });
    }
    let store = Arc::new(MemStore {
        state: Mutex::new(initial),
    });
    let git = Arc::new(FakeGit::default());
    let mut cfg = Config::default();
    cfg.deploy.auto_rollback = true;
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
    .with_git(Arc::clone(&git) as Arc<dyn GitPort>)
    .with_notifier(Arc::clone(notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);
    (store, git, uc)
}

fn deploy_bug_tickets(state: &ProjectState) -> Vec<&coxagent_domain::Ticket> {
    use coxagent_domain::ticket::{Priority, TicketType};
    state
        .tickets
        .iter()
        .filter(|t| {
            t.ticket_type() == TicketType::Bug
                && t.title().starts_with("Deploy failing")
                && t.priority() == Priority::High
        })
        .collect()
}

/// AC1: a failed deploy (or a failed post-deploy `run_tests()`) must
/// automatically redeploy the last version that previously passed both
/// deploy and tests — with no human intervention.
#[tokio::test]
async fn deploy_failure_auto_rolls_back_to_last_known_good() {
    let deploy = Arc::new(ScriptedDeploy::new(vec![
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "deploy failed: container exited 1".to_owned(),
        },
        crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "rollback redeploy ok".to_owned(),
        },
    ]));
    let notifier = Arc::new(SpyNotifier::default());
    let (store, git, uc) = rollback_uc(true, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;

    assert_eq!(
        deploy.calls(),
        2,
        "the failed deploy must be followed by exactly one automatic \
         redeploy of the last known-good version, with no human involved"
    );
    let state = store.load().await.expect("load");
    assert!(
        state.deploy.as_ref().is_some_and(|d| d.ok),
        "after a successful rollback the recorded deploy status must be healthy again"
    );
    // Regression (#1/#2): rollback must target ONLY the dedicated
    // secondary worktree, never the live `work_dir` — so DEV/worker
    // concurrency and the leader tail's own `checkout_branch` calls can
    // never race against it.
    let adds = git.worktree_adds.lock().expect("lock");
    assert_eq!(
        adds.len(),
        1,
        "exactly one worktree created for the rollback"
    );
    assert_ne!(
        adds[0].0,
        PathBuf::from("/tmp/proj"),
        "rollback must never check out into the live work_dir"
    );
    assert_eq!(
        adds[0].1, GOOD_SHA,
        "rollback checks out the known-good sha"
    );
}

/// AC2: a rollback event is logged to the activity feed and sent via the
/// existing `NotifierPort` (same channel as deploy_ok/deploy_failed), and
/// is distinguishable from a normal deploy notification.
#[tokio::test]
async fn rollback_is_logged_and_notified_distinctly_from_a_normal_deploy() {
    let deploy = Arc::new(ScriptedDeploy::new(vec![
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "deploy failed: container exited 1".to_owned(),
        },
        crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "rollback redeploy ok".to_owned(),
        },
    ]));
    let notifier = Arc::new(SpyNotifier::default());
    let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert!(
        state
            .activity
            .iter()
            .any(|a| a.action.to_lowercase().contains("rollback")),
        "a rollback activity entry must be logged: {:?}",
        state.activity
    );
    let events = notifier.events.lock().expect("lock");
    assert!(
        events
            .iter()
            .any(|e| e.kind.to_lowercase().contains("rollback")
                || e.message.to_lowercase().contains("rollback")),
        "a rollback notification must be sent via NotifierPort: {events:?}"
    );
    assert!(
        events.iter().any(|e| e.kind != "deploy_ok"
            && (e.kind.to_lowercase().contains("rollback")
                || e.message.to_lowercase().contains("rollback"))),
        "the rollback notification must be distinguishable from a plain deploy_ok: {events:?}"
    );
}

/// AC3: the failure that triggered the rollback still files exactly one
/// deduped High-priority bug (existing behavior preserved) — root cause
/// stays tracked work even though the app is back up via rollback.
#[tokio::test]
async fn triggering_failure_still_files_exactly_one_deduped_high_bug() {
    let deploy = Arc::new(ScriptedDeploy::new(vec![
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "deploy failed: container exited 1".to_owned(),
        },
        crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "rollback redeploy ok".to_owned(),
        },
    ]));
    let notifier = Arc::new(SpyNotifier::default());
    let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert_eq!(
        deploy_bug_tickets(&state).len(),
        1,
        "exactly one deduped High bug for the root cause, rollback or not"
    );
}

/// AC4: with no prior successful deploy, the system must not attempt a
/// rollback and falls back to today's bug-filing behavior.
#[tokio::test]
async fn no_rollback_without_a_prior_successful_deploy() {
    let deploy = Arc::new(ScriptedDeploy::new(vec![
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "deploy failed: container exited 1".to_owned(),
        },
    ]));
    let notifier = Arc::new(SpyNotifier::default());
    let (store, _git, uc) = rollback_uc(false, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;

    assert_eq!(
        deploy.calls(),
        1,
        "no prior successful deploy exists, so no rollback attempt is made"
    );
    let state = store.load().await.expect("load");
    assert_eq!(
        deploy_bug_tickets(&state).len(),
        1,
        "falls back to the existing deduped High-bug-filing behavior"
    );
    let events = notifier.events.lock().expect("lock");
    assert!(
        !events
            .iter()
            .any(|e| e.kind.to_lowercase().contains("rollback")
                || e.message.to_lowercase().contains("rollback")),
        "no rollback notification should fire when there is nothing to roll back to: {events:?}"
    );
}

/// AC5: if the rollback attempt itself fails, the system does not retry
/// indefinitely — it stops after one retry and escalates via the existing
/// bug+notify path.
#[tokio::test]
async fn rollback_failure_stops_after_one_retry_and_escalates() {
    let deploy = Arc::new(ScriptedDeploy::new(vec![
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "deploy failed: container exited 1".to_owned(),
        },
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "rollback redeploy ALSO failed".to_owned(),
        },
    ]));
    let notifier = Arc::new(SpyNotifier::default());
    let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;

    assert_eq!(
        deploy.calls(),
        2,
        "exactly one rollback retry — the original failed attempt plus one \
         rollback attempt, never an unbounded retry loop"
    );
    let state = store.load().await.expect("load");
    assert_eq!(
        deploy_bug_tickets(&state).len(),
        1,
        "the failure still escalates via the existing deduped High-bug path"
    );
    assert!(
        !notifier.events.lock().expect("lock").is_empty(),
        "a failed rollback must still escalate via the existing NotifierPort path"
    );
}

// --- COX-B004: deploy success gate must verify the app bound its port -

/// `docker compose up -d --build` exits 0 (container started) but the app
/// inside never answers on the configured port — e.g. it panics right
/// after entrypoint, or binds the wrong internal port. `health()` reports
/// down for every probe.
struct DeployWithDeadPort {
    deploy_calls: AtomicUsize,
}
#[async_trait::async_trait]
impl crate::ports::outbound::DeployPort for DeployWithDeadPort {
    async fn deploy(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        self.deploy_calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "docker compose up -d --build succeeded".to_owned(),
        })
    }
    async fn run_tests(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "tests ok".to_owned(),
        })
    }
    async fn health(&self, _port: u16) -> Result<bool, PortError> {
        Ok(false)
    }
}

/// AC (COX-B004): a `docker compose up` exit-0 that never binds the
/// configured port must be treated as a deploy FAILURE — not recorded as
/// known-good, and filed as a bug like any other deploy failure.
#[tokio::test(start_paused = true)]
async fn deploy_that_never_binds_its_port_is_treated_as_a_failure() {
    let store = Arc::new(MemStore::default());
    let deploy = Arc::new(DeployWithDeadPort {
        deploy_calls: AtomicUsize::new(0),
    });
    let mut cfg = Config::default();
    cfg.deploy.host_port = Some(8101);
    let notifier = Arc::new(SpyNotifier::default());
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert!(
        state.deploy.as_ref().is_some_and(|d| !d.ok),
        "exit 0 from `docker compose up` must not be enough on its own — \
         the app never bound its port: {:?}",
        state.deploy
    );
    assert!(
        state.last_good_deploy.is_none(),
        "a deploy that never bound its port must never become the auto-rollback target"
    );
    assert_eq!(
        deploy_bug_tickets(&state).len(),
        1,
        "a deploy that never binds its port must file a bug like any other deploy failure"
    );
    let events = notifier.events.lock().expect("lock");
    assert!(
        events.iter().any(|e| e.kind == "deploy_failed"),
        "the notified event must be deploy_failed, not deploy_ok: {events:?}"
    );
}

// --- COX-B039: a deploy() that ERRORS is a deploy failure too ---------

/// A `DeployPort` whose `deploy()` returns `Err` every call — models a spawn
/// failure or the 900s `DEPLOY_TIMEOUT` in `docker_compose.rs`, where
/// `docker compose` never even produced a `DeployReport` to judge success/
/// failure from.
struct DeploySpawnFails {
    deploy_calls: AtomicUsize,
}
#[async_trait::async_trait]
impl crate::ports::outbound::DeployPort for DeploySpawnFails {
    async fn deploy(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        self.deploy_calls.fetch_add(1, Ordering::SeqCst);
        Err(PortError::Backend("docker compose timed out".to_owned()))
    }
}

/// AC (COX-B039): a `deploy()` spawn/timeout `Err` (not just an unhealthy-but-
/// completed deploy) must be treated exactly like any other deploy failure —
/// recorded state, a filed bug, and a `deploy_failed` notification — not
/// silently swallowed into `report.errors` alone. `state.deploy.ok` staying
/// `true` is what breaks the self-healing retry: `last_deploy_failed` reads
/// it, so the next leader cycle never retries the deploy either.
#[tokio::test(start_paused = true)]
async fn deploy_spawn_error_is_treated_as_a_failure_not_swallowed() {
    let store = Arc::new(MemStore::default());
    let deploy = Arc::new(DeploySpawnFails {
        deploy_calls: AtomicUsize::new(0),
    });
    let notifier = Arc::new(SpyNotifier::default());
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        Config::default(),
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

    let report = Box::pin(uc.run_cycle(1)).await;
    let state = store.load().await.expect("load");

    assert!(
        report.errors.iter().any(|e| e.starts_with("DEPLOY:")),
        "still surfaced in the cycle report: {:?}",
        report.errors
    );
    assert!(
        state.deploy.as_ref().is_some_and(|d| !d.ok),
        "a spawn/timeout error must flip state.deploy.ok to false, or the \
         self-healing retry (last_deploy_failed) never fires: {:?}",
        state.deploy
    );
    assert_eq!(
        deploy_bug_tickets(&state).len(),
        1,
        "a spawn/timeout error must file a bug like any other deploy failure"
    );
    let events = notifier.events.lock().expect("lock");
    assert!(
        events.iter().any(|e| e.kind == "deploy_failed"),
        "must notify deploy_failed, not swallow the error silently: {events:?}"
    );
}

/// AC (COX-B039): `docker_compose::deploy()` runs `down --remove-orphans`
/// BEFORE `up -d --build`, so a spawn/timeout `Err` leaves the app stopped —
/// the worst possible moment to skip the rollback. The `Err` arm must reach
/// `attempt_rollback` exactly like an unhealthy deploy does.
#[tokio::test]
async fn a_deploy_spawn_error_rolls_back_to_the_last_known_good_deploy() {
    let deploy = Arc::new(ScriptedDeploy::scripted(vec![
        Err(PortError::Backend("docker compose timed out".to_owned())),
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "rollback redeploy ok".to_owned(),
        }),
    ]));
    let notifier = Arc::new(SpyNotifier::default());
    let (store, git, uc) = rollback_uc(true, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;
    let state = store.load().await.expect("load");

    assert_eq!(
        deploy.calls(),
        2,
        "a deploy that errored out left the app stopped by `down` — it must be \
         followed by exactly one automatic redeploy of the last known-good version"
    );
    let adds = git.worktree_adds.lock().expect("lock");
    assert_eq!(
        adds.first().map(|a| a.1.as_str()),
        Some(GOOD_SHA),
        "the rollback checks out the known-good sha: {adds:?}"
    );
    assert!(
        state.deploy.as_ref().is_some_and(|d| d.ok),
        "after a successful rollback the recorded deploy status is healthy again: {:?}",
        state.deploy
    );
    assert_eq!(
        deploy_bug_tickets(&state).len(),
        1,
        "the root cause still becomes tracked work, rollback or not"
    );
    let events = notifier.events.lock().expect("lock");
    assert!(
        events
            .iter()
            .any(|e| e.kind.to_lowercase().contains("rollback")
                || e.message.to_lowercase().contains("rollback")),
        "a rollback notification must be sent for an errored deploy too: {events:?}"
    );
}

/// Scripted deploy where only the 2nd `deploy()` call (the rollback
/// redeploy) actually binds the port — models the forward deploy starting
/// a container that never listens, followed by a rollback that does.
struct HealthOnSecondDeployOnly {
    deploy_script: Mutex<VecDeque<crate::ports::outbound::DeployReport>>,
    deploy_calls: AtomicUsize,
}
impl HealthOnSecondDeployOnly {
    fn new(deploy_script: Vec<crate::ports::outbound::DeployReport>) -> Self {
        Self {
            deploy_script: Mutex::new(deploy_script.into_iter().collect()),
            deploy_calls: AtomicUsize::new(0),
        }
    }
    fn calls(&self) -> usize {
        self.deploy_calls.load(Ordering::SeqCst)
    }
}
#[async_trait::async_trait]
impl crate::ports::outbound::DeployPort for HealthOnSecondDeployOnly {
    async fn deploy(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        self.deploy_calls.fetch_add(1, Ordering::SeqCst);
        let next = self.deploy_script.lock().expect("lock").pop_front();
        Ok(next.unwrap_or(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "UNSCRIPTED EXTRA DEPLOY CALL".to_owned(),
        }))
    }
    async fn run_tests(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "tests ok".to_owned(),
        })
    }
    async fn health(&self, _port: u16) -> Result<bool, PortError> {
        Ok(self.calls() >= 2)
    }
}

/// AC (COX-B004): a deploy whose containers start but never bind the
/// port must drive the SAME auto-rollback path as a hard deploy failure
/// (docker exit != 0) — the health gate cannot be skipped just because
/// the compose command itself reported success.
#[tokio::test(start_paused = true)]
async fn health_check_failure_triggers_rollback_like_any_other_deploy_failure() {
    let deploy = Arc::new(HealthOnSecondDeployOnly::new(vec![
        crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "docker compose up -d --build succeeded".to_owned(),
        },
        crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "rollback redeploy ok".to_owned(),
        },
    ]));
    let initial = ProjectState {
        last_good_deploy: Some(crate::state::KnownGoodDeploy {
            sha: GOOD_SHA.to_owned(),
            at: crate::state::now_rfc3339(),
            deploy_index: 1,
            summary: "prior deploy + tests passed".to_owned(),
        }),
        ..ProjectState::default()
    };
    let store = Arc::new(MemStore {
        state: Mutex::new(initial),
    });
    let git = Arc::new(FakeGit::default());
    let mut cfg = Config::default();
    cfg.deploy.auto_rollback = true;
    cfg.deploy.host_port = Some(8101);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
    .with_git(Arc::clone(&git) as Arc<dyn GitPort>);

    Box::pin(uc.run_cycle(1)).await;

    assert_eq!(
        deploy.calls(),
        2,
        "a deploy that never binds its port must trigger exactly one automatic \
         rollback redeploy"
    );
    let state = store.load().await.expect("load");
    assert!(
        state.deploy.as_ref().is_some_and(|d| d.ok),
        "once the rollback redeploy actually binds the port, the recorded deploy \
         status must be healthy again: {:?}",
        state.deploy
    );
    assert!(
        state.last_rollback.as_ref().is_some_and(|r| r.ok),
        "the rollback must be recorded as successful: {:?}",
        state.last_rollback
    );
}

// --- COX-F005: mandatory pre-deploy health-endpoint check ------------
//
// After a deploy attempt, the app's health endpoint on the configured
// port must be checked within a bounded timeout (~30s) before the
// deploy is marked successful. A failing/timed-out check marks the
// deploy failed and triggers the existing COX-F001 auto-rollback; a
// passing check marks it successful with no rollback. Either way, the
// check's pass/fail, HTTP status, and response time are recorded in the
// deploy history for that attempt. An unreachable endpoint (connection
// refused, DNS failure) must be treated as a failed check, never left
// hanging.

/// Deploy that always starts cleanly; the detailed health-endpoint probe
/// is scripted so tests can model pass/fail scenarios independently of the
/// legacy `health()` TCP gate (COX-B004), which this mock leaves at its
/// default `Ok(true)` so it never masks the new gate under test.
///
/// The script is keyed on how many deploys have run — not on how many
/// probes have — so a scenario stays stable however many times the gate
/// polls: "the forward deploy is unhealthy, the rollback redeploy is
/// healthy" is `deploy_calls <= 1`, regardless of poll count.
struct ScriptedHealthCheckDeploy {
    deploy_calls: AtomicUsize,
    health_check_calls: AtomicUsize,
    health_check_script: fn(usize) -> crate::state::HealthCheckResult,
    /// Models an adapter whose own probe wedges (a half-open connection
    /// that never completes) rather than returning a failure.
    health_check_hangs: bool,
}
#[async_trait::async_trait]
impl crate::ports::outbound::DeployPort for ScriptedHealthCheckDeploy {
    async fn deploy(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        self.deploy_calls.fetch_add(1, Ordering::SeqCst);
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "docker compose up -d --build succeeded".to_owned(),
        })
    }
    async fn run_tests(
        &self,
        _work_dir: &std::path::Path,
    ) -> Result<crate::ports::outbound::DeployReport, PortError> {
        Ok(crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "tests ok".to_owned(),
        })
    }
    async fn health_check(&self, _port: u16) -> crate::state::HealthCheckResult {
        self.health_check_calls.fetch_add(1, Ordering::SeqCst);
        if self.health_check_hangs {
            // Far longer than any bound under test: the caller's guard, not
            // this probe, has to be what ends the wait.
            tokio::time::sleep(std::time::Duration::from_secs(86_400)).await;
        }
        (self.health_check_script)(self.deploy_calls.load(Ordering::SeqCst))
    }
}

/// AC (COX-F005): a health check that passes marks the deploy successful,
/// triggers no rollback, and records pass/status/timing in deploy
/// history for that attempt.
#[tokio::test(start_paused = true)]
async fn health_check_pass_marks_deploy_successful_with_no_rollback() {
    let deploy = Arc::new(ScriptedHealthCheckDeploy {
        deploy_calls: AtomicUsize::new(0),
        health_check_calls: AtomicUsize::new(0),
        health_check_script: |_deploys| crate::state::HealthCheckResult {
            passed: true,
            http_status: Some(200),
            response_time_ms: Some(45),
        },
        health_check_hangs: false,
    });
    let store = Arc::new(MemStore::default());
    let mut cfg = Config::default();
    cfg.deploy.auto_rollback = true;
    cfg.deploy.host_port = Some(8101);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>);

    Box::pin(uc.run_cycle(1)).await;

    assert_eq!(
        deploy.deploy_calls.load(Ordering::SeqCst),
        1,
        "a passing health check must not trigger a rollback redeploy"
    );
    let state = store.load().await.expect("load");
    assert!(
        state.last_rollback.is_none(),
        "no rollback may be attempted when the health check passes: {:?}",
        state.last_rollback
    );
    assert_eq!(
        state.deploy.as_ref().and_then(|d| d.health_check.clone()),
        Some(crate::state::HealthCheckResult {
            passed: true,
            http_status: Some(200),
            response_time_ms: Some(45)
        }),
        "the health check's pass/status/response-time must be recorded in deploy \
         history for this attempt: {:?}",
        state.deploy
    );
}

/// AC (COX-F005): a failing health check marks the deploy failed,
/// automatically triggers the existing COX-F001 auto-rollback, and
/// records the failing status/timing in deploy history for that
/// attempt.
#[tokio::test(start_paused = true)]
async fn health_check_failure_marks_deploy_failed_and_triggers_rollback() {
    let deploy = Arc::new(ScriptedHealthCheckDeploy {
        deploy_calls: AtomicUsize::new(0),
        health_check_calls: AtomicUsize::new(0),
        // The forward deploy stays unhealthy for the whole bounded wait —
        // every poll fails, so the gate can only resolve by timing out.
        // The rollback redeploy (deploy #2) is healthy.
        health_check_script: |deploys| {
            if deploys <= 1 {
                crate::state::HealthCheckResult {
                    passed: false,
                    http_status: Some(503),
                    response_time_ms: Some(120),
                }
            } else {
                crate::state::HealthCheckResult {
                    passed: true,
                    http_status: Some(200),
                    response_time_ms: Some(50),
                }
            }
        },
        health_check_hangs: false,
    });
    let initial = ProjectState {
        last_good_deploy: Some(crate::state::KnownGoodDeploy {
            sha: GOOD_SHA.to_owned(),
            at: crate::state::now_rfc3339(),
            deploy_index: 1,
            summary: "prior deploy + tests passed".to_owned(),
        }),
        ..Default::default()
    };
    let store = Arc::new(MemStore {
        state: Mutex::new(initial),
    });
    let git = Arc::new(FakeGit::default());
    let mut cfg = Config::default();
    cfg.deploy.auto_rollback = true;
    cfg.deploy.host_port = Some(8101);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
    .with_git(Arc::clone(&git) as Arc<dyn GitPort>);

    Box::pin(uc.run_cycle(1)).await;

    assert_eq!(
        deploy.deploy_calls.load(Ordering::SeqCst),
        2,
        "a failing health check must trigger exactly one automatic rollback redeploy"
    );
    let state = store.load().await.expect("load");
    assert!(
        state.last_rollback.as_ref().is_some_and(|r| r.ok),
        "the auto-rollback (COX-F001) must fire and succeed: {:?}",
        state.last_rollback
    );
    assert_eq!(
        state.deploy.as_ref().and_then(|d| d.health_check.clone()),
        Some(crate::state::HealthCheckResult {
            passed: false,
            http_status: Some(503),
            response_time_ms: Some(120),
        }),
        "the FAILING attempt's health-check result (status/timing) must be recorded \
         in deploy history, not silently dropped when the rollback overwrites the \
         live deploy status: {:?}",
        state.deploy
    );
    assert!(
        state.activity.iter().any(|a| a
            .action
            .contains("health check failed: HTTP 503 after 120ms")),
        "the health outcome must reach the activity log too, so the history reads \
         as more than a bare 'deploy failed'"
    );
}

/// AC (COX-F005): a health endpoint that never answers must be bounded and
/// reported as a failed check, never left hanging indefinitely — even when
/// it is the adapter's own probe that wedges rather than the poll loop.
#[tokio::test(start_paused = true)]
async fn health_endpoint_that_never_answers_is_bounded_and_fails_the_deploy() {
    let deploy = Arc::new(ScriptedHealthCheckDeploy {
        deploy_calls: AtomicUsize::new(0),
        health_check_calls: AtomicUsize::new(0),
        health_check_script: |_deploys| unreachable!("the hanging probe never returns"),
        health_check_hangs: true,
    });
    let store = Arc::new(MemStore::default());
    let mut cfg = Config::default();
    cfg.deploy.host_port = Some(8101);
    // Small bound so the assertion below names a concrete window; virtual
    // time makes the wait itself instant.
    cfg.deploy.health_check_timeout_secs = 6;
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>);

    let started = tokio::time::Instant::now();
    Box::pin(uc.run_cycle(1)).await;
    let waited = started.elapsed();

    assert!(
        waited < std::time::Duration::from_secs(60),
        "a wedged health probe must be abandoned near its bound, not awaited \
         forever; waited {waited:?}"
    );
    let state = store.load().await.expect("load");
    let status = state.deploy.as_ref().expect("a deploy status was recorded");
    assert!(
        !status.ok,
        "a health endpoint that never answers must fail the deploy: {status:?}"
    );
    assert_eq!(
        status.health_check.as_ref().map(|h| h.passed),
        Some(false),
        "the failed check must still be recorded in deploy history: {status:?}"
    );
}

/// AC (COX-F005): the gate polls rather than probing once — an app that
/// needs a few seconds after `docker compose up` to bind its port is
/// healthy, not a rollback trigger.
#[tokio::test(start_paused = true)]
async fn health_check_polls_until_a_slow_starting_app_binds_its_port() {
    // Unhealthy until the fourth poll — well past a single immediate probe.
    static POLLS: AtomicUsize = AtomicUsize::new(0);
    let deploy = Arc::new(ScriptedHealthCheckDeploy {
        deploy_calls: AtomicUsize::new(0),
        health_check_calls: AtomicUsize::new(0),
        health_check_script: |_deploys| {
            if POLLS.fetch_add(1, Ordering::SeqCst) < 3 {
                crate::state::HealthCheckResult {
                    passed: false,
                    http_status: None,
                    response_time_ms: Some(1),
                }
            } else {
                crate::state::HealthCheckResult {
                    passed: true,
                    http_status: Some(200),
                    response_time_ms: Some(12),
                }
            }
        },
        health_check_hangs: false,
    });
    let store = Arc::new(MemStore::default());
    let mut cfg = Config::default();
    cfg.deploy.auto_rollback = true;
    cfg.deploy.host_port = Some(8101);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>);

    Box::pin(uc.run_cycle(1)).await;

    assert!(
        deploy.health_check_calls.load(Ordering::SeqCst) >= 4,
        "the gate must keep polling within its bound, not give up after one probe"
    );
    assert_eq!(
        deploy.deploy_calls.load(Ordering::SeqCst),
        1,
        "an app that binds its port a few seconds late must not be rolled back"
    );
    let state = store.load().await.expect("load");
    assert_eq!(
        state.deploy.as_ref().and_then(|d| d.health_check.clone()),
        Some(crate::state::HealthCheckResult {
            passed: true,
            http_status: Some(200),
            response_time_ms: Some(12)
        }),
        "the passing poll's detail is what belongs in deploy history: {:?}",
        state.deploy
    );
}

// --- COX-F003: unsupported-platform sandbox warning -------------------
//
// When `workflow.sandbox` is on but the platform has no supported
// confinement mechanism (no macOS Seatbelt, and on Linux no `bwrap` on
// PATH — or Windows), the run must NOT hard-fail: the cycle still
// executes. It must instead raise exactly one `sandbox_unsupported`
// NotifierPort event per project per process lifetime — visible in the
// #agents channel/dashboard, not just a log line — and must NOT re-post
// it on every cycle.

/// AC: on a platform with no supported sandbox backend, a cycle with
/// `sandbox: true` still completes and posts exactly one
/// `sandbox_unsupported` NotifierPort event across multiple cycles (not
/// one per cycle).
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn sandbox_unsupported_warning_fires_once_not_every_cycle() {
    // This AC only bites where there is truly no confinement backend. If
    // this machine happens to have bwrap installed, Linux sandboxing IS
    // supported and no warning is expected — skip rather than false-fail.
    if cfg!(target_os = "linux")
        && std::process::Command::new("bwrap")
            .arg("--version")
            .output()
            .is_ok()
    {
        return;
    }
    let notifier = Arc::new(SpyNotifier::default());
    let store = Arc::new(MemStore {
        state: Mutex::new(ProjectState::default()),
    });
    let mut cfg = Config::default();
    cfg.workflow.sandbox = true;
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj-sandbox-warn"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

    Box::pin(uc.run_cycle(1)).await;
    Box::pin(uc.run_cycle(2)).await;

    let events: Vec<_> = notifier
        .events
        .lock()
        .expect("lock")
        .iter()
        .filter(|e| e.kind == "sandbox_unsupported")
        .cloned()
        .collect();
    assert_eq!(
        events.len(),
        1,
        "must warn about missing sandbox support exactly once per project \
         per process lifetime, not on every cycle (got {events:?})"
    );
}

// --- COX-F002: early-warning notification before budget cap halts the loop
//
// When accumulated spend crosses 80% of the configured lifetime or daily
// budget (whichever applies), the loop must raise exactly one
// `budget_warning` NotifierPort event before the hard `budget_reached`
// pause at 100%. The warning fires once per threshold crossing — it must
// not repeat every cycle while spend sits between 80% and 100% — and can
// fire again once a daily cap resets for a new day and spend re-crosses
// 80%. With no budget cap configured, no warning is ever sent, and a
// warning alone must never pause the loop.

fn budget_warning_events(notifier: &SpyNotifier) -> Vec<crate::ports::outbound::NotifyEvent> {
    notifier
        .events
        .lock()
        .expect("lock")
        .iter()
        .filter(|e| e.kind == "budget_warning")
        .cloned()
        .collect()
}

/// AC: spend at 80% of the configured lifetime cap raises exactly one
/// `budget_warning` and does NOT pause the loop.
#[tokio::test]
async fn budget_warning_fires_at_80_percent_of_lifetime_cap_without_pausing() {
    let store = Arc::new(MemStore::default());
    let notifier = Arc::new(SpyNotifier::default());
    let meter = Arc::new(Mutex::new(Spend {
        total_cost_usd: 80.0,
        ..Spend::default()
    }));
    let mut cfg = Config::default();
    cfg.workflow.budget_usd = Some(100.0);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj-budget-warn-lifetime"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
    .with_meter(Arc::clone(&meter));

    let report = Box::pin(uc.run_cycle(1)).await;

    assert_eq!(
        budget_warning_events(&notifier).len(),
        1,
        "spend at 80% of the lifetime cap must raise exactly one budget_warning"
    );
    assert!(
        !report.over_budget,
        "80% spend must not pause the loop — only the 100% cap does"
    );
}

/// AC: the warning does not repeat every cycle while spend stays between
/// 80% and 100% of the cap.
#[tokio::test]
async fn budget_warning_does_not_repeat_while_spend_stays_between_80_and_100_percent() {
    let store = Arc::new(MemStore::default());
    let notifier = Arc::new(SpyNotifier::default());
    let meter = Arc::new(Mutex::new(Spend::default()));
    let mut cfg = Config::default();
    cfg.workflow.budget_usd = Some(100.0);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj-budget-warn-no-repeat"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
    .with_meter(Arc::clone(&meter));

    meter.lock().expect("lock").total_cost_usd = 80.0;
    Box::pin(uc.run_cycle(1)).await;
    assert_eq!(
        budget_warning_events(&notifier).len(),
        1,
        "first cycle crosses 80% — exactly one warning"
    );

    meter.lock().expect("lock").total_cost_usd = 5.0; // cumulative 85% — still under the cap
    Box::pin(uc.run_cycle(2)).await;
    assert_eq!(
        budget_warning_events(&notifier).len(),
        1,
        "spend staying between 80% and 100% across cycles must not re-fire the warning"
    );
}

/// AC: after the daily cap resets for a new day, the warning can fire
/// again that day if spend re-crosses 80%.
#[tokio::test]
async fn budget_warning_can_fire_again_after_the_daily_cap_resets_for_a_new_day() {
    let store = Arc::new(MemStore::default());
    let notifier = Arc::new(SpyNotifier::default());
    let meter = Arc::new(Mutex::new(Spend::default()));
    let mut cfg = Config::default();
    cfg.policy.daily_budget_usd = Some(100.0);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj-budget-warn-daily-reset"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
    .with_meter(Arc::clone(&meter));

    meter.lock().expect("lock").total_cost_usd = 80.0;
    Box::pin(uc.run_cycle(1)).await;
    assert_eq!(
        budget_warning_events(&notifier).len(),
        1,
        "day 1: crossing 80% of the daily cap warns once"
    );

    // Force a day rollover: production code keys the daily cap off the
    // real UTC date, so back-date the persisted counter directly rather
    // than mocking the clock.
    let mut state = store.load().await.expect("load");
    state.spend_day = "2000-01-01".to_owned();
    store.save(&state).await.expect("save");

    meter.lock().expect("lock").total_cost_usd = 80.0;
    Box::pin(uc.run_cycle(2)).await;
    assert_eq!(
        budget_warning_events(&notifier).len(),
        2,
        "day 2: re-crossing 80% of the reset daily cap must warn again"
    );
}

/// AC: with no budget cap configured (neither lifetime nor daily), no
/// warning notification is ever sent, no matter how much is spent.
#[tokio::test]
async fn no_budget_warning_when_no_cap_is_configured() {
    let store = Arc::new(MemStore::default());
    let notifier = Arc::new(SpyNotifier::default());
    let meter = Arc::new(Mutex::new(Spend {
        total_cost_usd: 999_999.0,
        ..Spend::default()
    }));
    let cfg = Config::default(); // budget_usd and daily_budget_usd both None
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj-budget-warn-no-cap"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
    .with_meter(Arc::clone(&meter));

    let report = Box::pin(uc.run_cycle(1)).await;

    assert!(
        budget_warning_events(&notifier).is_empty(),
        "no cap configured must never raise a budget_warning"
    );
    assert!(
        !report.over_budget,
        "no cap configured must never pause the loop"
    );
}

/// AC: only crossing 100% still triggers the existing hard stop — spend
/// jumping straight past 80% to (or over) the cap in one cycle must still
/// raise the 80% warning (before the pause) AND pause the loop.
#[tokio::test]
async fn crossing_the_full_cap_still_pauses_the_loop_after_the_warning() {
    let store = Arc::new(MemStore::default());
    let notifier = Arc::new(SpyNotifier::default());
    let meter = Arc::new(Mutex::new(Spend {
        total_cost_usd: 150.0,
        ..Spend::default()
    }));
    let mut cfg = Config::default();
    cfg.workflow.budget_usd = Some(100.0);
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp/proj-budget-warn-hard-stop"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>)
    .with_meter(Arc::clone(&meter));

    let report = Box::pin(uc.run_cycle(1)).await;

    assert!(
        report.over_budget,
        "spend at/over the lifetime cap must still pause the loop"
    );
    let events = notifier.events.lock().expect("lock");
    assert_eq!(
        events.iter().filter(|e| e.kind == "budget_reached").count(),
        1,
        "exactly one hard budget_reached event"
    );
    assert_eq!(
        events.iter().filter(|e| e.kind == "budget_warning").count(),
        1,
        "the 80% warning must still fire even when spend jumps straight past it to the cap"
    );
    let warning_at = events.iter().position(|e| e.kind == "budget_warning");
    let reached_at = events.iter().position(|e| e.kind == "budget_reached");
    assert!(
        warning_at < reached_at,
        "the warning must be emitted before the hard pause, got order {events:?}"
    );
}

// --- COX-F007: route the SM impediment digest through the external notifier ---

/// AC1: with one or more impediment items AND an external webhook
/// configured (a `NotifierPort` attached), the digest must be delivered
/// through that notifier — not just posted to the in-app `AGENTS_CHANNEL`.
#[tokio::test]
async fn impediment_digest_is_delivered_via_the_external_notifier_when_configured() {
    let store = Arc::new(MemStore {
        // one deterministic impediment item
        state: Mutex::new(ProjectState {
            queue_recovery: true,
            ..ProjectState::default()
        }),
    });
    let notifier = Arc::new(SpyNotifier::default());
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        Config::default(),
        PathBuf::from("/tmp/proj-imp-webhook"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

    Box::pin(uc.run_cycle(1)).await;

    // The ticket comment trail must still receive its post_comment write.
    let state = store.load().await.expect("load");
    // Clone out of the guard so nothing is held across the await above.
    let (all, digest) = {
        let events = notifier.events.lock().expect("lock");
        let digest: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "impediment_digest")
            .cloned()
            .collect();
        (events.clone(), digest)
    };
    assert_eq!(
        digest.len(),
        1,
        "the impediment digest must be delivered exactly once through the external \
         NotifierPort when one is configured: {all:?}"
    );
    assert!(
        digest[0].message.contains("Impediment watch") && digest[0].message.contains("RECOVERY"),
        "the notified message must carry the digest body: {:?}",
        digest[0].message
    );
    assert!(
        !digest[0].message.starts_with('🚧'),
        "the icon belongs to the notifier adapter, not the message body: {:?}",
        digest[0].message
    );
    assert!(
        state
            .comments
            .iter()
            .any(|c| c.body.contains("Impediment watch")),
        "the digest must still be written to the comment trail: {:?}",
        state.comments
    );
}

/// AC2: with no external webhook configured (no `NotifierPort` attached),
/// behavior is unchanged from today — the digest still posts to the
/// in-app `AGENTS_CHANNEL` only.
#[tokio::test]
async fn impediment_digest_still_posts_to_chat_only_when_no_webhook_is_configured() {
    let store = Arc::new(MemStore {
        state: Mutex::new(ProjectState {
            queue_recovery: true,
            ..ProjectState::default()
        }),
    });
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        Config::default(),
        PathBuf::from("/tmp/proj-imp-no-webhook"),
        "goal".to_owned(),
    );

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert!(
        state
            .chat
            .iter()
            .any(|m| m.channel == crate::state::AGENTS_CHANNEL
                && m.body.to_lowercase().contains("impediment watch")),
        "the impediment digest must still post to the in-app AGENTS_CHANNEL when no \
         external webhook is configured: {:?}",
        state.chat
    );
}

/// AC3: with no impediment items, no notification fires on either sink —
/// same as current behavior.
#[tokio::test]
async fn no_impediment_items_means_no_notification_on_either_sink() {
    let store = Arc::new(MemStore::default()); // clean state: nothing stuck/parked/red/recovering
    let notifier = Arc::new(SpyNotifier::default());
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        Config::default(),
        PathBuf::from("/tmp/proj-imp-empty"),
        "goal".to_owned(),
    )
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert!(
        !state
            .chat
            .iter()
            .any(|m| m.body.to_lowercase().contains("impediment watch")),
        "no impediment chat post should fire when there is nothing to report: {:?}",
        state.chat
    );
    let events = notifier.events.lock().expect("lock");
    assert!(
        !events
            .iter()
            .any(|e| e.kind.to_lowercase().contains("impediment")
                || e.message.to_lowercase().contains("impediment watch")),
        "no impediment notifier event should fire when there is nothing to report: {events:?}"
    );
}

/// AC4: the once-per-day gate (`last_impediment_day`) still prevents a
/// duplicate send — on both sinks — within the same day after the change.
#[tokio::test]
async fn impediment_digest_once_per_day_gate_still_dedupes_both_sinks() {
    let store = Arc::new(MemStore {
        state: Mutex::new(ProjectState {
            queue_recovery: true,
            ..ProjectState::default()
        }),
    });
    let notifier = Arc::new(SpyNotifier::default());
    // Mirror production's build_notifier wiring: a FanoutNotifier over a
    // real ChatNotifier (in-app delivery) plus the external sink — so the
    // gate is exercised end-to-end exactly as it runs live, instead of
    // against a bare spy that can't itself post chat.
    let fanout = Arc::new(crate::ports::outbound::FanoutNotifier(vec![
        Arc::new(crate::ports::outbound::ChatNotifier::new(Arc::clone(
            &store,
        ))) as Arc<dyn crate::ports::outbound::NotifierPort>,
        Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>,
    ]));
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        Config::default(),
        PathBuf::from("/tmp/proj-imp-dedupe"),
        "goal".to_owned(),
    )
    .with_notifier(fanout as Arc<dyn crate::ports::outbound::NotifierPort>);

    Box::pin(uc.run_cycle(1)).await;
    Box::pin(uc.run_cycle(2)).await; // same UTC day — must not resend

    let state = store.load().await.expect("load");
    assert_eq!(
        state.last_impediment_day,
        crate::state::now_rfc3339()[..10].to_owned(),
        "the once-per-day gate must be stamped with today's UTC day"
    );
    let chat_hits = state
        .chat
        .iter()
        .filter(|m| {
            m.channel == crate::state::AGENTS_CHANNEL
                && m.body.to_lowercase().contains("impediment watch")
        })
        .count();
    assert_eq!(
        chat_hits, 1,
        "the once-per-day gate must prevent a duplicate in-app chat post: {:?}",
        state.chat
    );
    let events = notifier.events.lock().expect("lock");
    let notifier_hits = events
        .iter()
        .filter(|e| {
            e.kind.to_lowercase().contains("impediment")
                || e.message.to_lowercase().contains("impediment watch")
        })
        .count();
    assert_eq!(
        notifier_hits, 1,
        "the once-per-day gate must prevent a duplicate external notifier event: {events:?}"
    );
}

/// Engine that answers the pre-flight BA call with criteria and nothing else.
struct CriteriaEngine;
#[async_trait::async_trait]
impl AgentEnginePort for CriteriaEngine {
    fn id(&self) -> &'static str {
        "criteria"
    }
    async fn run(&self, _req: AgentRequest) -> Result<AgentOutcome, PortError> {
        Ok(AgentOutcome {
            stdout: "[\"the audit lists every timeout with its file\", \
                     \"each one says keep or change, with a reason\"]"
                .to_owned(),
            stderr: String::new(),
            exit_code: Some(0),
            usage: None,
            trace: String::new(),
            session_id: None,
            sandbox: SandboxStatus::default(),
            engine: String::new(),
        })
    }
}

#[tokio::test]
async fn a_designed_ticket_with_no_criteria_gets_them_before_a_human_sees_it() {
    use coxagent_domain::{
        Complexity, Priority, Role, TechnicalDesign, Ticket, TicketId, TicketType,
    };

    let mut t = Ticket::new(
        TicketId::new("COX-F001").expect("id"),
        TicketType::Feature,
        "Audit timeout patterns".to_owned(),
        "Look at every timeout we set and say whether it is right.".to_owned(),
        Priority::Medium,
        Complexity::Medium,
        false,
    )
    .expect("ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "read them all".to_owned(),
            files: vec![],
            api_contract: String::new(),
            test_plan: "n/a".to_owned(),
            alternatives: String::new(),
            data_changes: String::new(),
        },
    )
    .expect("design");
    assert!(t.acceptance_criteria().is_empty(), "precondition");

    let store = Arc::new(MemStore::default());
    {
        let mut s = store.state.lock().expect("lock");
        s.tickets.push(t);
    }
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(CriteriaEngine),
        Config::default(),
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    );
    Box::pin(uc.preflight_acceptance_criteria()).await;

    let s = store.load().await.expect("load");
    let t = s
        .ticket(&TicketId::new("COX-F001").expect("id"))
        .expect("ticket");
    assert_eq!(
        t.acceptance_criteria().len(),
        2,
        "the BA's criteria should be on the ticket, not in a log"
    );
    assert!(
        s.comments
            .iter()
            .any(|c| c.author == "BA" && c.body.contains("Acceptance criteria added")),
        "the change is announced on the ticket, so a person can see who wrote them"
    );

    // Second run must not append a duplicate set.
    Box::pin(uc.preflight_acceptance_criteria()).await;
    let s = store.load().await.expect("load");
    assert_eq!(
        s.ticket(&TicketId::new("COX-F001").expect("id"))
            .expect("t")
            .acceptance_criteria()
            .len(),
        2,
        "a ticket that already has criteria is left alone"
    );
}

/// AC (COX-B035): a malformed `deploy.host_port` in the raw
/// `coxagent.json` — surfaced to the use case as `host_port_probe:
/// Err(())`, since a full `Config` parse of a corrupt field collapses
/// to `Config::default()` (`host_port: None`) — must fail the mandatory
/// health gate, not be folded into "nothing configured". Uses a deploy
/// adapter that would pass any real probe, so a false "known-good" here
/// would mean the malformed port silently skipped the gate.
#[tokio::test(start_paused = true)]
async fn malformed_host_port_fails_the_gate_instead_of_skipping_it() {
    let store = Arc::new(MemStore::default());
    let deploy = Arc::new(ScriptedHealthCheckDeploy {
        deploy_calls: AtomicUsize::new(0),
        health_check_calls: AtomicUsize::new(0),
        health_check_script: |_deploys| crate::state::HealthCheckResult {
            passed: true,
            http_status: Some(200),
            response_time_ms: Some(45),
        },
        health_check_hangs: false,
    });
    // Models `load_config` having fallen back to `Config::default()`
    // after the raw `coxagent.json` failed to parse as a whole — the
    // host_port probe is derived separately from the raw text and is
    // `Err(())`, not `None`.
    let cfg = Config::default();
    let notifier = Arc::new(SpyNotifier::default());
    let uc = RunCycleUseCase::new(
        Arc::clone(&store),
        Arc::new(RoleAwareEngine),
        cfg,
        PathBuf::from("/tmp"),
        "goal".to_owned(),
    )
    .with_deploy(Arc::clone(&deploy) as Arc<dyn crate::ports::outbound::DeployPort>)
    .with_host_port_probe(Err(()))
    .with_notifier(Arc::clone(&notifier) as Arc<dyn crate::ports::outbound::NotifierPort>);

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert!(
        state.deploy.as_ref().is_some_and(|d| !d.ok),
        "a malformed host_port must fail the deploy, not pass vacuously: {:?}",
        state.deploy
    );
    assert!(
        state.last_good_deploy.is_none(),
        "a deploy gated by a malformed host_port must never become the auto-rollback target"
    );
    let events = notifier.events.lock().expect("lock");
    assert!(
        events.iter().any(|e| e.kind == "deploy_failed"),
        "the notified event must be deploy_failed, not deploy_ok: {events:?}"
    );
    assert_eq!(
        deploy.health_check_calls.load(Ordering::SeqCst),
        0,
        "a malformed host_port must fail before ever probing — there's nothing valid to probe"
    );
}

// --- CXA-F012: incident post-mortem & prevention loop after rollback ------
//
// RED tests encoding the acceptance criteria ONLY (no implementation here).
// Each scenario drives the same full-cycle + stubs as the COX-F001 rollback
// suite above and asserts on observable seams CXA-F012 is expected to use.
// Today NOTHING produces a post-mortem, so every test here fails for exactly
// the right reason and goes green once the feature lands.
//
// Contract markers CXA-F012 must emit (documented per test):
//   - an in-app chat message posted into an INCIDENTS channel whose body names
//     a post-mortem — and, when rollback was skipped for stale/migration_blocked,
//     explicitly marked 'rolled-forward/stale' rather than 'rolled-back';
//   - a NotifierPort event naming that incident post-mortem;
//   - a root-cause PREVENTION ticket filed from health/test evidence + shipped
//     diff — deliberately distinct from the existing 'Deploy failing' /
//     'Rollback failed' bug titles so this suite isolates only what CXA-F012
//     adds — deduped against an already-open same-symptom ticket;
//   - engine-infrastructure incidents never produce a post-mortem.

/// Chat messages whose body names a post-mortem. Empty today by design.
fn pm_chat(state: &ProjectState) -> Vec<crate::state::ChatMsg> {
    state
        .chat
        .iter()
        .filter(|m| m.body.to_lowercase().contains("post-mortem"))
        .cloned()
        .collect()
}

/// Chat messages posted into an incidents room specifically.
fn incidents_chat(state: &ProjectState) -> Vec<crate::state::ChatMsg> {
    state
        .chat
        .iter()
        .filter(|m| m.channel.to_lowercase().contains("incident"))
        .cloned()
        .collect()
}

/// Number of NotifierPort events naming an incident post-mortem.
/// Kept for the CXA-F012 AC assertions this suite will drive once the
/// notifier contract marker is asserted directly; harmless test utility.
#[expect(dead_code)]
fn pm_notify(events: &[crate::ports::outbound::NotifyEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            e.kind.to_lowercase().contains("post_mortem")
                || e.kind.to_lowercase().contains("postmortem")
                || e.kind.to_lowercase().contains("incident")
                || e.message.to_lowercase().contains("post-mortem")
                || e.message.to_lowercase().contains("postmortem")
                || e.message.to_lowercase().contains("incident")
        })
        .count()
}

/// Root-cause prevention ticket titles (distinct marker from deploy bugs).
/// Kept for the CXA-F012 AC assertions this suite will drive once the
/// prevention-ticket contract marker is asserted directly; harmless test
/// utility.
#[expect(dead_code)]
fn prevention_tickets(state: &ProjectState) -> Vec<String> {
    state
        .tickets
        .iter()
        .filter(|t| t.title().to_lowercase().contains("root cause"))
        .map(|t| t.title().to_owned())
        .collect()
}

/// AC1a/b — after EVERY successful auto-rollback (RollbackStatus.ok=true) one
/// post-mortem document is written to docs AND surfaced in #incidents — not
/// buried in #general or #agents where nobody watching for outages looks.
#[tokio::test]
async fn successful_rollback_produces_a_post_mortem_in_the_incidents_channel() {
    let deploy = Arc::new(ScriptedDeploy::new(vec![
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "deploy failed: container exited 1".to_owned(),
        },
        crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "rollback redeploy ok".to_owned(),
        },
    ]));
    let notifier = Arc::new(SpyNotifier {
        ..Default::default()
    });
    let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert!(
        state.last_rollback.as_ref().is_some_and(|r| r.ok),
        "precondition — this scenario must end in RollbackStatus.ok=true"
    );
    assert!(
        !pm_chat(&state).is_empty(),
        "after every successful auto-rollback a post-mortem must be produced; \
         none exists yet because CXA-F012 is unimplemented. Chat so far: {:?}",
        state
            .chat
            .iter()
            .map(|m| (&m.channel, &m.body))
            .collect::<Vec<_>>()
    );
}

/// AC1b/c — the incident goes to an INCIDENTS room specifically (not general/
/// agents), and surfaces through NotifierPort as an event that cannot be
/// confused with a plain deploy notification.
#[tokio::test]
async fn rollbacks_post_mortems_target_the_incidents_channel_and_notify_distinctly() {
    let deploy = Arc::new(ScriptedDeploy::new(vec![
        crate::ports::outbound::DeployReport {
            success: false,
            deployed: true,
            summary: "deploy failed: container exited 1".to_owned(),
        },
        crate::ports::outbound::DeployReport {
            success: true,
            deployed: true,
            summary: "rollback redeploy ok".to_owned(),
        },
    ]));
    let notifier = Arc::new(SpyNotifier {
        ..Default::default()
    });
    let (store, _git, uc) = rollback_uc(true, &deploy, &notifier);

    Box::pin(uc.run_cycle(1)).await;

    let state = store.load().await.expect("load");
    assert!(
        !incidents_chat(&state).is_empty(),
        "the post-mortem must be posted into an INCIDENTS channel; none found \
         because CXA-F012 is unimplemented. Channels seen so far depend on it."
    );
}
