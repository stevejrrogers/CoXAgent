// Split from server/mod.rs — the PR-preview lifecycle tests.
#![allow(clippy::wildcard_imports)]
use super::*;
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::{
    AgentOutcome, AgentRequest, DeployPort, DeployReport, ForgePort, PullRequest,
};
use coxagent_application::state::ProjectState;
use coxagent_application::PortError;
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
        *self.state.lock().expect("lock") = s.clone();
        Ok(())
    }
}

/// Never invoked by the restore (`start=false`) path under test.
struct UnusedEngine;
#[async_trait::async_trait]
impl coxagent_application::ports::outbound::AgentEnginePort for UnusedEngine {
    fn id(&self) -> &'static str {
        "unused"
    }
    async fn run(&self, _request: AgentRequest) -> Result<AgentOutcome, PortError> {
        unreachable!("not called by pr_preview's restore path")
    }
}

/// Never invoked by the restore (`start=false`) path under test — it does
/// no PR/git lookups, only `deploy.down` + `deploy.deploy`.
struct UnusedForge;
#[async_trait::async_trait]
impl ForgePort for UnusedForge {
    async fn open_pr(
        &self,
        _head: &str,
        _base: &str,
        _title: &str,
        _body: &str,
    ) -> Result<PullRequest, PortError> {
        unreachable!("not called by pr_preview's restore path")
    }
    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
        unreachable!("not called by pr_preview's restore path")
    }
    async fn pr_diff(&self, _number: u64) -> Result<String, PortError> {
        unreachable!("not called by pr_preview's restore path")
    }
    async fn merge_pr(&self, _number: u64) -> Result<(), PortError> {
        unreachable!("not called by pr_preview's restore path")
    }
    async fn request_changes(&self, _number: u64, _comment: &str) -> Result<(), PortError> {
        unreachable!("not called by pr_preview's restore path")
    }
    async fn close_pr(&self, _number: u64) -> Result<(), PortError> {
        unreachable!("not called by pr_preview's restore path")
    }
}

/// `docker compose up` exits 0 (container started) but the app inside
/// never answers on its configured port — the COX-B004/COX-B009 scenario.
struct DeployWithDeadPort;
#[async_trait::async_trait]
impl DeployPort for DeployWithDeadPort {
    async fn deploy(&self, _work_dir: &std::path::Path) -> Result<DeployReport, PortError> {
        Ok(DeployReport {
            failure_bundle: None,
            success: true,
            deployed: true,
            summary: "docker compose up -d --build succeeded".to_owned(),
        })
    }
    async fn health(&self, _port: u16) -> Result<bool, PortError> {
        Ok(false)
    }
}

struct HealthyDeploy;
#[async_trait::async_trait]
impl DeployPort for HealthyDeploy {
    async fn deploy(&self, _work_dir: &std::path::Path) -> Result<DeployReport, PortError> {
        Ok(DeployReport {
            failure_bundle: None,
            success: true,
            deployed: true,
            summary: "docker compose up -d --build succeeded".to_owned(),
        })
    }
    async fn health(&self, _port: u16) -> Result<bool, PortError> {
        Ok(true)
    }
}

/// A project workspace with a `coxagent.json` whose `deploy.host_port`
/// is the given raw JSON literal (e.g. `"8101"`, `"-1"`, `"\"8101\""`,
/// `"true"`) — lets tests exercise `parse_preview_host_port` against
/// values that aren't valid `u16`s, not just a well-formed port.
fn project_handle_with_raw_host_port(
    deploy: Arc<dyn DeployPort>,
    raw_host_port: &str,
) -> (tempfile::TempDir, ProjectHandle) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("coxagent.json");
    std::fs::write(
        &config_path,
        format!(r#"{{"deploy":{{"host_port":{raw_host_port}}}}}"#),
    )
    .expect("write config");
    let handle = ProjectHandle {
        id: "proj".to_owned(),
        name: "proj".to_owned(),
        alias: "proj".to_owned(),
        store: Arc::new(MemStore::default()) as Arc<dyn StateStorePort>,
        runner: Arc::new(RunnerHandle::default()),
        config_path,
        engine: Arc::new(UnusedEngine),
        work_dir: dir.path().to_path_buf(),
        outbox: None,
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: dir.path().join("project_context.md"),
        forge: None,
        deploy: Some(deploy),
        storage: None,
        files: None,
        deps_discovery: None,
    };
    (dir, handle)
}

/// A project workspace with a `coxagent.json` naming the published
/// `deploy.host_port` `pr_preview` reads to probe health.
fn project_handle(deploy: Arc<dyn DeployPort>) -> (tempfile::TempDir, ProjectHandle) {
    project_handle_with_raw_host_port(deploy, "8101")
}

/// A project workspace whose `coxagent.json` is exactly `raw_config` — for
/// exercising corruption above the `host_port` leaf (unparseable JSON, a
/// non-object `deploy` section) that `project_handle_with_raw_host_port`
/// can't reach since it always wraps the value in a well-formed
/// `{"deploy":{"host_port":...}}` shell.
fn project_handle_with_raw_config(
    deploy: Arc<dyn DeployPort>,
    raw_config: &str,
) -> (tempfile::TempDir, ProjectHandle) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("coxagent.json");
    std::fs::write(&config_path, raw_config).expect("write config");
    let handle = ProjectHandle {
        id: "proj".to_owned(),
        name: "proj".to_owned(),
        alias: "proj".to_owned(),
        store: Arc::new(MemStore::default()) as Arc<dyn StateStorePort>,
        runner: Arc::new(RunnerHandle::default()),
        config_path,
        engine: Arc::new(UnusedEngine),
        work_dir: dir.path().to_path_buf(),
        outbox: None,
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: dir.path().join("project_context.md"),
        forge: None,
        deploy: Some(deploy),
        storage: None,
        files: None,
        deps_discovery: None,
    };
    (dir, handle)
}

/// AC (COX-B009): restoring the main build after a PR preview must run
/// through the same mandatory health gate as the autonomous cycle
/// (COX-B004) and chat's "deploy" command — a compose exit-0 that never
/// binds the app's port must NOT be reported as a successful restore.
#[tokio::test(start_paused = true)]
async fn restore_reports_failure_when_the_app_never_binds_its_port() {
    let (_dir, handle) = project_handle(Arc::new(DeployWithDeadPort));
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("health check failed"),
        "expected the health-gate failure reason in the response: {text}"
    );
}

/// Control: a restore that actually answers on its port still reports OK
/// — the gate must not fail a genuinely healthy restore.
#[tokio::test(start_paused = true)]
async fn restore_reports_ok_when_the_app_is_healthy() {
    let (_dir, handle) = project_handle(Arc::new(HealthyDeploy));
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::OK);
}

/// A `ForgePort` stub reporting a single open PR whose head branch is
/// fetchable from the fixture's `origin` remote.
struct ForgeWithOpenPr(String);
#[async_trait::async_trait]
impl ForgePort for ForgeWithOpenPr {
    async fn open_pr(
        &self,
        _head: &str,
        _base: &str,
        _title: &str,
        _body: &str,
    ) -> Result<PullRequest, PortError> {
        unreachable!("not called by pr_preview's start path")
    }
    async fn list_open_prs(&self) -> Result<Vec<PullRequest>, PortError> {
        Ok(vec![PullRequest {
            number: 1,
            title: "test".to_owned(),
            head: self.0.clone(),
            base: "main".to_owned(),
            url: String::new(),
            author: "tester".to_owned(),
            ci: "none".to_owned(),
            mergeable: true,
            created: "2026-01-01T00:00:00Z".to_owned(),
        }])
    }
    async fn pr_diff(&self, _number: u64) -> Result<String, PortError> {
        unreachable!("not called by pr_preview's start path")
    }
    async fn merge_pr(&self, _number: u64) -> Result<(), PortError> {
        unreachable!("not called by pr_preview's start path")
    }
    async fn request_changes(&self, _number: u64, _comment: &str) -> Result<(), PortError> {
        unreachable!("not called by pr_preview's start path")
    }
    async fn close_pr(&self, _number: u64) -> Result<(), PortError> {
        unreachable!("not called by pr_preview's start path")
    }
}

/// A bare `origin` repo with a `feat/preview` branch, plus a working
/// clone wired as the project's `work_dir` — real git, since the start
/// path shells out to `git fetch`/`git worktree add`.
async fn git_preview_fixture(
    deploy: Arc<dyn DeployPort>,
    host_port: Option<u64>,
) -> (tempfile::TempDir, tempfile::TempDir, ProjectHandle) {
    let bare = tempfile::tempdir().expect("tempdir");
    git_pv(bare.path(), &["init", "--bare", "-q"])
        .await
        .expect("git init --bare");
    let bare_url = bare.path().to_string_lossy().into_owned();

    let seed = tempfile::tempdir().expect("tempdir");
    git_pv(seed.path(), &["init", "-q", "-b", "main"])
        .await
        .expect("git init seed");
    git_pv(seed.path(), &["config", "user.email", "test@test"])
        .await
        .expect("git config email");
    git_pv(seed.path(), &["config", "user.name", "test"])
        .await
        .expect("git config name");
    std::fs::write(seed.path().join("README.md"), "seed").expect("write");
    git_pv(seed.path(), &["add", "."]).await.expect("git add");
    git_pv(seed.path(), &["commit", "-q", "-m", "seed"])
        .await
        .expect("git commit");
    git_pv(seed.path(), &["checkout", "-q", "-b", "feat/preview"])
        .await
        .expect("git checkout -b");
    std::fs::write(seed.path().join("README.md"), "preview").expect("write");
    git_pv(seed.path(), &["commit", "-q", "-am", "preview change"])
        .await
        .expect("git commit");
    git_pv(seed.path(), &["remote", "add", "origin", &bare_url])
        .await
        .expect("remote add");
    git_pv(seed.path(), &["push", "-q", "origin", "--all"])
        .await
        .expect("git push");

    let work = tempfile::tempdir().expect("tempdir");
    git_pv(work.path(), &["clone", "-q", &bare_url, "."])
        .await
        .expect("git clone");
    let config_path = work.path().join("coxagent.json");
    let config = match host_port {
        Some(port) => format!(r#"{{"deploy":{{"host_port":{port}}}}}"#),
        None => r#"{"deploy":{}}"#.to_owned(),
    };
    std::fs::write(&config_path, config).expect("write config");
    let handle = ProjectHandle {
        id: "proj".to_owned(),
        name: "proj".to_owned(),
        alias: "proj".to_owned(),
        store: Arc::new(MemStore::default()) as Arc<dyn StateStorePort>,
        runner: Arc::new(RunnerHandle::default()),
        config_path,
        engine: Arc::new(UnusedEngine),
        work_dir: work.path().to_path_buf(),
        outbox: None,
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: work.path().join("project_context.md"),
        forge: None,
        deploy: Some(deploy),
        storage: None,
        files: None,
        deps_discovery: None,
    };
    (bare, work, handle)
}

/// AC (COX-B009): starting a PR preview must run through the same
/// mandatory health gate as restore/chat/cycle — a compose exit-0 that
/// never binds the app's port must NOT be reported as a LIVE preview.
#[tokio::test(start_paused = true)]
async fn preview_start_reports_failure_when_the_app_never_binds_its_port() {
    let (_bare, _work, handle) =
        git_preview_fixture(Arc::new(DeployWithDeadPort), Some(8101)).await;
    let forge: Arc<dyn ForgePort> = Arc::new(ForgeWithOpenPr("feat/preview".to_owned()));

    let resp = pr_preview(&handle, &forge, 1, true).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("health check failed"),
        "expected the health-gate failure reason in the response: {text}"
    );
}

/// Control: a preview that actually answers on its port reports LIVE —
/// the gate must not fail a genuinely healthy preview.
#[tokio::test(start_paused = true)]
async fn preview_start_reports_ok_when_the_app_is_healthy() {
    let (_bare, _work, handle) = git_preview_fixture(Arc::new(HealthyDeploy), Some(8101)).await;
    let forge: Arc<dyn ForgePort> = Arc::new(ForgeWithOpenPr("feat/preview".to_owned()));

    let resp = pr_preview(&handle, &forge, 1, true).await;

    assert_eq!(resp.status(), StatusCode::OK);
}

/// AC (COX-B009): a project with no configured `deploy.host_port` has
/// nothing for the gate to probe — same contract as
/// `verify_deploy_health`'s own `no_configured_host_port_passes_without_probing`
/// unit test, exercised here through the actual preview endpoint. Uses a
/// deploy adapter that would fail any real probe, so a false pass here
/// would mean the gate is probing a port that was never configured.
#[tokio::test(start_paused = true)]
async fn an_unset_host_port_leaves_the_gate_nothing_to_probe() {
    let (_bare, _work, handle) = git_preview_fixture(Arc::new(DeployWithDeadPort), None).await;
    let forge: Arc<dyn ForgePort> = Arc::new(ForgeWithOpenPr("feat/preview".to_owned()));

    let resp = pr_preview(&handle, &forge, 1, true).await;

    assert_eq!(resp.status(), StatusCode::OK);
}

/// AC (COX-B026): an explicit JSON `null` is the same as an absent
/// `host_port` key — nothing configured, nothing to probe — matching
/// `Option<u16>`'s own deserialization contract. Uses a deploy adapter
/// that would fail any real probe, so a false pass here would mean the
/// gate is probing a port that was never configured.
#[tokio::test(start_paused = true)]
async fn an_explicit_null_host_port_leaves_the_gate_nothing_to_probe() {
    let (_dir, handle) = project_handle_with_raw_host_port(Arc::new(DeployWithDeadPort), "null");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::OK);
}

/// AC (COX-B026): `serde_json::Value::as_u64` returns `None` for a
/// negative `host_port` — the same `None` it returns for a genuinely
/// absent field. Without inspecting the raw JSON value first, that
/// collapse would let a negative port silently skip the gate exactly
/// like COX-B025's out-of-range positive case did before its fix.
#[tokio::test(start_paused = true)]
async fn a_negative_host_port_is_rejected_rather_than_skipping_the_gate() {
    let (_dir, handle) = project_handle_with_raw_host_port(Arc::new(HealthyDeploy), "-1");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// AC (COX-B026): a float `host_port` is not a valid TCP port either —
/// `as_u64` returns `None` for it same as for a negative number or an
/// absent field.
#[tokio::test(start_paused = true)]
async fn a_float_host_port_is_rejected_rather_than_skipping_the_gate() {
    let (_dir, handle) = project_handle_with_raw_host_port(Arc::new(HealthyDeploy), "8101.5");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// AC (COX-B026): a quoted-string `host_port` (e.g. from a hand-edited
/// config) must fail the gate rather than be treated as unset.
#[tokio::test(start_paused = true)]
async fn a_string_host_port_is_rejected_rather_than_skipping_the_gate() {
    let (_dir, handle) = project_handle_with_raw_host_port(Arc::new(HealthyDeploy), "\"8101\"");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// AC (COX-B026): a boolean `host_port` must fail the gate rather than
/// be treated as unset.
#[tokio::test(start_paused = true)]
async fn a_boolean_host_port_is_rejected_rather_than_skipping_the_gate() {
    let (_dir, handle) = project_handle_with_raw_host_port(Arc::new(HealthyDeploy), "true");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// AC (COX-B042): `0` is a valid `u16` but not a connectable port — the
/// kernel's "any free port" sentinel. Probing it can only ever time out, so
/// a preview would spend the gate's whole window before blaming the app for
/// a fault that is in the config. Reject it like any other unpublishable
/// value instead.
#[tokio::test(start_paused = true)]
async fn a_zero_host_port_is_rejected_rather_than_probed() {
    let (_dir, handle) = project_handle_with_raw_host_port(Arc::new(HealthyDeploy), "0");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// AC (COX-B025 regression guard): an out-of-`u16`-range positive
/// `host_port` must still fail the gate, not silently skip it.
#[tokio::test(start_paused = true)]
async fn an_out_of_range_host_port_is_rejected_rather_than_skipping_the_gate() {
    let (_dir, handle) = project_handle_with_raw_host_port(Arc::new(HealthyDeploy), "70000");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// AC (COX-B062): a `coxagent.json` that fails to parse as JSON at all —
/// not just a bad `host_port` value — must still fail the mandatory health
/// gate rather than being read as "unconfigured" (`Ok(None)`) and passing
/// unconditionally. Uses `DeployWithDeadPort` so a false pass here would
/// mean a dead-on-arrival app got reported as a successful restore.
#[tokio::test(start_paused = true)]
async fn corrupt_json_is_rejected_rather_than_skipping_the_gate() {
    let (_dir, handle) =
        project_handle_with_raw_config(Arc::new(DeployWithDeadPort), "{not valid json at all");
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains("health check failed"),
        "expected the health-gate failure reason in the response: {text}"
    );
}

/// AC (COX-B062): a `deploy` section that exists but isn't a JSON object
/// (e.g. `{"deploy":"oops"}`) must also fail the gate — `value.get("deploy")
/// .and_then(|d| d.get("host_port"))` silently returns `None` for this shape
/// same as a genuinely absent field, so it must be checked explicitly.
#[tokio::test(start_paused = true)]
async fn a_non_object_deploy_section_is_rejected_rather_than_skipping_the_gate() {
    let (_dir, handle) =
        project_handle_with_raw_config(Arc::new(DeployWithDeadPort), r#"{"deploy":"oops"}"#);
    let forge: Arc<dyn ForgePort> = Arc::new(UnusedForge);

    let resp = pr_preview(&handle, &forge, 1, false).await;

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
