//! CXA-F285 TDD — close out P5a: verify `/store` end-to-end with auth enabled.
//!
//! The enforcement itself landed in `store_rpc.rs::authorize_store_call` and
//! `auth_mw`; this suite pins the ticket's acceptance criteria end-to-end,
//! through the real runner adapter (`RestStateStore`) against real hubs booted
//! exactly as a deploy boots them (`serve_full`) — the same shape as
//! `rest_state_store_contract.rs`, whose harness module
//! (`rest_store_support`) supplies the fixtures reused here. Hubs bind
//! freshly allocated ports (the `auth_login_harvest` pattern) so two
//! worktrees running the suite never race for a fixed port.
//!
//! Encoded acceptance criteria (CXA-F285):
//! - AC1  With auth enabled on the hub, a POST /api/projects/:pid/store
//!   without any valid session or bearer token is rejected with 401 for
//!   every op (load, save, claim_ticket, heartbeat, ...), and a
//!   subsequent authenticated op=load shows the store state was not
//!   modified.
//! - AC2  With auth enabled, a caller that IS authenticated but lacks manage
//!   rights on the project is refused with 403 and no op reaches the
//!   store: a member-tier (write-tier) user, and a manage-tier user who
//!   is not a member of :pid (Super/Admin exempt), each cannot read or
//!   write that project's state via /store.
//! - AC3  With auth enabled, a runner pointed at the hub via
//!   COXAGENT_REMOTE_STORE_URL and a valid personal API token
//!   (COXAGENT_REMOTE_TOKEN minted by a manage-tier project member)
//!   completes a full store round-trip — load, version, save with
//!   revision, claim_ticket/claim_stage, heartbeat — without any
//!   401/403, matching its behaviour with auth disabled.
//! - AC4  With auth NOT configured (open mode), /store still accepts
//!   unauthenticated ops exactly as before P5a, so no existing open
//!   deployment regresses.
//!
//! (AC5, the caller inventory, is a repo-level deliverable documented in
//! `.claude/handoff-rest-runner.md`, section "/store caller inventory
//! (CXA-F285)".)
//!
//! The runner credential class is the personal API token presented as a
//! bearer, so every persona here authenticates the way a runner does. The
//! bearer-only restriction mirrors `rest_store_support::StubAuth` ("the
//! REST adapter authenticates with a bearer, never a session").

#![allow(clippy::unwrap_used, clippy::expect_used)]

// This suite reuses the contract harness's pure fixtures but not every
// helper in it (it boots its own personas/hubs); the unused remainder stays
// for `rest_state_store_contract.rs`, the module's primary consumer.
#[allow(dead_code)]
mod rest_store_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::{
    AgentEnginePort, AgentOutcome, AgentRequest, StateStorePort, WorkerCaps, WorkerEntry,
};
use coxagent_application::{PortError, ProjectState};
use coxagent_domain::{SemVer, TicketId};
use coxagent_infrastructure::{FileAuthService, JsonStateStore, RestStateStore};
use coxagent_presentation::{HubExtras, ProjectHandle};
use rest_store_support::{NOW, PID, now_rfc3339, rest_store, sample_bug, sample_ticket};

/// A second project: the outside personas' ONLY membership, so any access to
/// [`PID`] is cross-project by construction.
const OTHER_PID: &str = "elsewhere";

/// The Admin bearer: hub-wide manage tier AND a member of [`PID`] — seeds and
/// re-reads the state the refused callers must leave untouched.
const ADMIN_BEARER: &str = "f285-admin";
/// Member-tier (write-tier) bearer, member of [`PID`]: authenticated, may
/// work the project, but /store writes whole state snapshots so the manage
/// bar must refuse every op.
const MEMBER_BEARER: &str = "f285-member";
/// Manage-tier bearer (Manager) whose ONLY membership is [`OTHER_PID`]:
/// clears the manage bar but must trip the project-membership branch.
const LEAD_OUTSIDER_BEARER: &str = "f285-lead-outsider";
/// Super bearer with NO project memberships: the exemption AC2 pins — Super
/// is above the membership gate by design.
const SUPER_OUTSIDER_BEARER: &str = "f285-super-outsider";
/// Admin bearer with NO project memberships: the other exempt role.
const ADMIN_OUTSIDER_BEARER: &str = "f285-admin-outsider";

/// A free 127.0.0.1 port: bind :0, read the assigned port, release — so two
/// worktrees running this suite concurrently never collide on a fixed port
/// (the `auth_login_harvest` lesson).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral listener")
        .local_addr()
        .expect("local addr")
        .port()
}

struct UnreachableEngine;
#[async_trait]
impl AgentEnginePort for UnreachableEngine {
    fn id(&self) -> &'static str {
        "unreachable"
    }
    async fn run(&self, _rq: AgentRequest) -> Result<AgentOutcome, PortError> {
        unreachable!("the store gateway never runs an engine")
    }
}

/// One registered project (id [`PID`]) backed by `store`.
fn project_handle(store: Arc<dyn StateStorePort>) -> ProjectHandle {
    let dir = std::env::temp_dir();
    ProjectHandle {
        id: PID.to_owned(),
        name: "Demo".to_owned(),
        alias: "demo".to_owned(),
        store,
        runner: Arc::new(coxagent_application::use_cases::RunnerHandle::new()),
        outbox: None,
        config_path: dir.join("coxagent.json"),
        engine: Arc::new(UnreachableEngine),
        work_dir: dir.clone(),
        budget: Arc::new(Mutex::new(BudgetCaps::default())),
        context_path: dir.join("project_context.md"),
        forge: None,
        deploy: None,
        storage: None,
        files: None,
        deps_discovery: None,
    }
}

/// Boot one hub on a freshly allocated port with the given backend and
/// optional auth; waits until `/api/health` answers, retrying on a new port
/// if the bind was lost to a sibling process. Returns `(port, hub dir)`.
async fn boot(
    auth: Option<Arc<dyn AuthPort>>,
    store: Arc<dyn StateStorePort>,
) -> (u16, tempfile::TempDir) {
    let client = reqwest::Client::new();
    for _ in 0..3 {
        let port = free_port();
        let dir = tempfile::tempdir().expect("tempdir");
        let extras = HubExtras {
            auth: auth.clone(),
            hub_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
            Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
        tokio::spawn(coxagent_presentation::serve_full(
            vec![project_handle(store.clone())],
            port,
            audit,
            extras,
        ));
        let health = format!("http://127.0.0.1:{port}/api/health");
        for _ in 0..50 {
            if client.get(&health).send().await.is_ok() {
                return (port, dir);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    panic!("hub never answered /api/health on three freshly allocated ports");
}

/// The auth hub's bearer persona store: each test bearer resolves to one
/// principal; everything else (forged, expired, absent) resolves to none.
struct BearerPersonas;

fn persona(username: &str, role: AuthRole, projects: &[&str]) -> AuthUser {
    AuthUser {
        username: username.to_owned(),
        name: String::new(),
        email: String::new(),
        role,
        projects: projects.iter().map(|p| (*p).to_owned()).collect(),
    }
}

#[async_trait]
impl AuthPort for BearerPersonas {
    async fn login(&self, _u: &str, _p: &str, _t: Option<&str>) -> LoginResult {
        LoginResult::Denied
    }

    async fn user_for(&self, _token: &str) -> Option<AuthUser> {
        None // the REST adapter authenticates with a bearer, never a session
    }

    async fn logout(&self, _token: &str) {}

    async fn principal_for_bearer(&self, token: &str) -> Option<AuthUser> {
        match token {
            ADMIN_BEARER => Some(persona("ops-admin", AuthRole::Admin, &[PID])),
            MEMBER_BEARER => Some(persona("carol", AuthRole::Be, &[PID])),
            LEAD_OUTSIDER_BEARER => Some(persona("morgan", AuthRole::Manager, &[OTHER_PID])),
            SUPER_OUTSIDER_BEARER => Some(persona("saul", AuthRole::Super, &[])),
            ADMIN_OUTSIDER_BEARER => Some(persona("ada", AuthRole::Admin, &[])),
            _ => None,
        }
    }

    async fn create_token(&self, _label: &str, _role: AuthRole) -> Option<String> {
        None
    }

    async fn list_tokens(&self) -> Vec<TokenInfo> {
        Vec::new()
    }

    async fn revoke_token(&self, _label: &str) -> bool {
        false
    }

    async fn list_users(&self) -> Vec<AuthUser> {
        Vec::new()
    }

    async fn create_user(&self, _u: &str, _p: &str, _r: AuthRole) -> bool {
        false
    }

    async fn delete_user(&self, _username: &str) -> bool {
        false
    }

    async fn assign_project(&self, _username: &str, _pid: &str) -> bool {
        false
    }

    async fn unassign_project(&self, _username: &str, _pid: &str) -> bool {
        false
    }

    async fn enroll_2fa(&self, _username: &str) -> Option<(String, String)> {
        None
    }

    async fn enable_2fa(&self, _username: &str, _code: &str) -> bool {
        false
    }

    async fn disable_2fa(&self, _username: &str) -> bool {
        false
    }

    async fn has_2fa(&self, _username: &str) -> bool {
        false
    }

    async fn sessions_for(&self, _username: &str, _current: &str) -> Vec<SessionInfo> {
        Vec::new()
    }
}

/// In-memory backend with the two coordination surfaces a runner's full round
/// trip touches: optimistic-concurrency revisions (baseline 0, stale write ->
/// `Conflict`, like the production `SqlStateStore`) and the worker registry.
/// Pure double over the real port types; every other op keeps the port
/// default, and the gateway cannot tell it from a shared store.
struct VersionedRegistryStore {
    state: Mutex<ProjectState>,
    revision: Mutex<i64>,
    workers: Mutex<Vec<WorkerEntry>>,
}

impl VersionedRegistryStore {
    fn new() -> Self {
        Self {
            state: Mutex::new(ProjectState::default()),
            revision: Mutex::new(0),
            workers: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl StateStorePort for VersionedRegistryStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().expect("lock").clone())
    }

    async fn save(&self, state: &ProjectState) -> Result<(), PortError> {
        *self.state.lock().expect("lock") = state.clone();
        *self.revision.lock().expect("lock") += 1;
        Ok(())
    }

    async fn save_expecting(
        &self,
        state: &ProjectState,
        expected_revision: Option<i64>,
    ) -> Result<(), PortError> {
        let head = *self.revision.lock().expect("lock");
        if let Some(expected) = expected_revision {
            if expected != head {
                return Err(PortError::Conflict(format!(
                    "revision {expected} is stale, head is {head}"
                )));
            }
        }
        self.save(state).await
    }

    async fn current_version(&self) -> Result<Option<i64>, PortError> {
        Ok(Some(*self.revision.lock().expect("lock")))
    }

    async fn heartbeat_worker(
        &self,
        worker: &str,
        role: &str,
        ticket: &str,
        caps: &WorkerCaps,
        now: &str,
    ) -> Result<(), PortError> {
        self.workers.lock().expect("lock").push(WorkerEntry {
            worker: worker.to_owned(),
            role: role.to_owned(),
            ticket: ticket.to_owned(),
            at: now.to_owned(),
            engines: caps.engines.clone(),
            models: caps.models.clone(),
            git: caps.git.clone(),
            tooling: caps.tooling.clone(),
            version: caps.version.clone(),
        });
        Ok(())
    }

    async fn workers(&self) -> Result<Vec<WorkerEntry>, PortError> {
        Ok(self.workers.lock().expect("lock").clone())
    }
}

/// Drive EVERY operation the runner adapter implements — the full
/// `StateStorePort` surface the `/store` endpoint dispatches — and report per
/// op the error the gateway answered (`None` = the op succeeded). Pure over
/// the port types; no raw HTTP, no invented ops.
async fn every_op_result(store: &RestStateStore) -> OpResults {
    let id = TicketId::new("CXC-101").expect("valid id");
    let caps = WorkerCaps {
        engines: vec!["stub-engine".to_owned()],
        models: Vec::new(),
        git: None,
        tooling: None,
        version: "test".to_owned(),
    };
    let state = ProjectState {
        current_version: SemVer::new(1, 0, 0),
        tickets: vec![sample_ticket("F285-DRIVE")],
        ..ProjectState::default()
    };
    let mut out: Vec<(&'static str, Option<PortError>)> = Vec::new();
    out.push(("load", store.load().await.err()));
    out.push(("version", store.current_version().await.err()));
    out.push(("save", store.save(&state).await.err()));
    out.push((
        "save_expecting",
        store.save_expecting(&state, Some(0)).await.err(),
    ));
    out.push((
        "claim_ticket",
        store.claim_ticket(&id, "worker@mac", NOW).await.err(),
    ));
    out.push((
        "acquire_leader",
        store.acquire_leader("worker@mac", NOW).await.err(),
    ));
    out.push((
        "claim_stage",
        store.claim_stage(&id, "sa", "worker@mac", NOW).await.err(),
    ));
    out.push((
        "release_stage",
        store.release_stage(&id, "sa", "worker@mac").await.err(),
    ));
    out.push((
        "heartbeat",
        store
            .heartbeat_worker("worker@mac", "be", "CXC-101", &caps, NOW)
            .await
            .err(),
    ));
    out.push(("workers", store.workers().await.err()));
    out.push((
        "set_desired",
        store.set_desired("operator", true).await.err(),
    ));
    out.push(("get_desired", store.get_desired("operator").await.err()));
    out.push((
        "acquire_operator",
        store.acquire_operator("operator", "instance-1").await.err(),
    ));
    out
}

type OpResults = Vec<(&'static str, Option<PortError>)>;

/// The known state the auth hubs are seeded with through the admin bearer,
/// so every later assertion can prove refused ops modified nothing.
fn seeded_state() -> ProjectState {
    ProjectState {
        current_version: SemVer::new(7, 0, 0),
        tickets: vec![sample_ticket("F285-SEED")],
        ..ProjectState::default()
    }
}

/// AC1: without any valid session or bearer token, EVERY op is refused with
/// 401, and a subsequent authenticated op=load shows the store state was not
/// modified. A bearer the hub does not know is exactly as good as none.
#[tokio::test]
async fn ac1_without_credentials_every_store_op_is_refused_401_and_state_survives() {
    let backend = Arc::new(VersionedRegistryStore::new());
    let (port, _dir) = boot(Some(Arc::new(BearerPersonas)), backend).await;
    let admin = rest_store(port, Some(ADMIN_BEARER));
    let seeded = seeded_state();
    admin.save(&seeded).await.expect("seed the known state");

    for (op, refused) in every_op_result(&rest_store(port, None)).await {
        let err =
            refused.unwrap_or_else(|| panic!("op {op} must be refused without any credential"));
        assert!(
            err.to_string().contains("401"),
            "unauthenticated op {op} must answer 401 — got: {err}"
        );
    }
    for (op, refused) in every_op_result(&rest_store(port, Some("bearer-forged"))).await {
        let err = refused.unwrap_or_else(|| panic!("op {op} must be refused with a forged bearer"));
        assert!(
            err.to_string().contains("401"),
            "forged-bearer op {op} must answer 401 — got: {err}"
        );
    }

    let after = admin.load().await.expect("authenticated op=load");
    assert_eq!(
        after, seeded,
        "no refused op may have modified the store state"
    );
}

/// AC2: authenticated callers without manage rights on the project are
/// refused with 403 and no op reaches the store — the member-tier member via
/// the manage bar, the manage-tier non-member via the membership branch —
/// while Super/Admin stay exempt even with no project memberships.
#[tokio::test]
async fn ac2_authenticated_without_manage_rights_on_the_project_is_403_and_reaches_no_op() {
    let backend = Arc::new(VersionedRegistryStore::new());
    let (port, _dir) = boot(Some(Arc::new(BearerPersonas)), backend).await;
    let admin = rest_store(port, Some(ADMIN_BEARER));
    let seeded = seeded_state();
    admin.save(&seeded).await.expect("seed the known state");

    // (a) Member-tier (write-tier) user, member of :pid: authenticated, but
    // /store writes whole state snapshots — the manage bar refuses every op.
    for (op, refused) in every_op_result(&rest_store(port, Some(MEMBER_BEARER))).await {
        let err = refused.unwrap_or_else(|| panic!("member-tier op {op} must be refused with 403"));
        assert!(
            err.to_string().contains("403"),
            "member-tier op {op} must answer 403 — got: {err}"
        );
        assert!(
            err.to_string().contains("insufficient role"),
            "member-tier op {op} must be refused by the manage bar — got: {err}"
        );
    }

    // (b) Manage-tier user who is NOT a member of :pid: clears the manage
    // bar, trips the project-membership branch — still 403, still no op.
    for (op, refused) in every_op_result(&rest_store(port, Some(LEAD_OUTSIDER_BEARER))).await {
        let err =
            refused.unwrap_or_else(|| panic!("manage-tier non-member op {op} must be refused"));
        assert!(
            err.to_string().contains("403"),
            "manage-tier non-member op {op} must answer 403 — got: {err}"
        );
        assert!(
            err.to_string().contains("not a member of this project"),
            "manage-tier non-member op {op} must be refused by the membership branch — got: {err}"
        );
    }

    // (c) Super/Admin exempt — both load the project's state with no
    // membership at all.
    let super_outsider = rest_store(port, Some(SUPER_OUTSIDER_BEARER));
    assert_eq!(
        super_outsider.load().await.expect("super is exempt"),
        seeded,
        "Super must read the project's state without membership"
    );
    let admin_outsider = rest_store(port, Some(ADMIN_OUTSIDER_BEARER));
    assert_eq!(
        admin_outsider.load().await.expect("admin is exempt"),
        seeded,
        "Admin must read the project's state without membership"
    );

    // Every refused op stayed off the store: the authenticated read still
    // sees exactly the seeded state.
    let after = admin.load().await.expect("authenticated op=load");
    assert_eq!(
        after, seeded,
        "no refused op may have reached (let alone modified) the store"
    );
}

/// What one full runner round trip observed — the data AC3 compares between
/// the authed hub and the open hub. Plain data, `PartialEq`, no behaviour.
// The bools are the observation, not branching state: each records one step
// of the round trip (load/claim/lease/heartbeat) so the authed and open runs
// can be compared field for field.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, PartialEq)]
struct RoundTrip {
    load_is_default: bool,
    baseline_revision: Option<i64>,
    save_with_revision_ok: bool,
    revision_after_save: Option<i64>,
    claim_won: bool,
    stage_won: bool,
    heartbeat_ok: bool,
    workers_seen: usize,
}

/// The full store round trip AC3 names: load, version, save with revision,
/// claim_ticket, claim_stage, heartbeat. Every step must succeed — the
/// `expect`s surface the gateway's status line when one does not.
async fn full_round_trip(store: &RestStateStore) -> RoundTrip {
    let id = TicketId::new("CXC-101").expect("valid id");
    let caps = WorkerCaps {
        engines: vec!["stub-engine".to_owned()],
        models: Vec::new(),
        git: None,
        tooling: None,
        version: "test".to_owned(),
    };
    let state = ProjectState {
        current_version: SemVer::new(9, 0, 0),
        tickets: vec![sample_bug("CXC-101")],
        ..ProjectState::default()
    };
    let load_is_default = store.load().await.expect("load") == ProjectState::default();
    let baseline_revision = store.current_version().await.expect("version");
    let save_with_revision_ok = store
        .save_expecting(&state, baseline_revision)
        .await
        .is_ok();
    let revision_after_save = store.current_version().await.expect("version after save");
    let claim_won = store
        .claim_ticket(&id, "runner@mac", NOW)
        .await
        .expect("claim_ticket");
    let stage_won = store
        .claim_stage(&id, "sa", "runner@mac", NOW)
        .await
        .expect("claim_stage");
    let heartbeat_ok = store
        .heartbeat_worker("runner@mac", "be", "CXC-101", &caps, &now_rfc3339())
        .await
        .is_ok();
    let workers_seen = store.workers().await.expect("workers").len();
    RoundTrip {
        load_is_default,
        baseline_revision,
        save_with_revision_ok,
        revision_after_save,
        claim_won,
        stage_won,
        heartbeat_ok,
        workers_seen,
    }
}

/// AC3: a runner pointed at the hub with a personal API token minted by a
/// manage-tier PROJECT MEMBER completes the full round trip without any
/// 401/403, matching its behaviour with auth disabled. The token is minted on
/// the real `FileAuthService` exactly as `create_my_token_ep` mints it —
/// bound to the minting member's own role — and the member genuinely IS a
/// member of :pid (`assign_project`), so the only thing under test is what
/// the gateway does with that credential.
///
/// The member's role is Admin on purpose, not timidity: a bearer principal
/// resolves with NO project memberships by design (`FileAuthService` /
/// `SqlAuthService` `principal_for_bearer` both return `projects: []` —
/// service tokens are hub-wide for their role), so the membership branch of
/// the /store gate passes only Super/Admin-tier tokens. A lead-tier
/// (e.g. TechLead) member's token would be refused 403 "not a member of this
/// project" even though the account itself is a member — flagged in the
/// CXA-F285 caller inventory as a product decision for SA, not a bug fixed
/// here.
#[tokio::test]
async fn ac3_personal_token_of_a_manage_tier_member_completes_the_full_round_trip() {
    // The manage-tier project member and their personal token.
    let auth_dir = tempfile::tempdir().expect("auth dir");
    let auth = FileAuthService::open(&auth_dir.path().join("users.json")).expect("auth service");
    assert!(
        auth.create_user("ops-lead", "pw", AuthRole::Admin).await,
        "the manage-tier member must be creatable"
    );
    assert!(
        auth.assign_project("ops-lead", PID).await,
        "the member must be assignable to the project"
    );
    let token = auth
        .create_token("user:ops-lead:remote-store", AuthRole::Admin)
        .await
        .expect("mint the personal token");

    let (authed_port, _dir) = boot(
        Some(Arc::new(auth)),
        Arc::new(VersionedRegistryStore::new()),
    )
    .await;
    let runner = rest_store(authed_port, Some(&token));
    let with_auth = full_round_trip(&runner).await;

    // The same round trip with auth disabled — the parity half of the AC.
    let (open_port, _dir2) = boot(None, Arc::new(VersionedRegistryStore::new())).await;
    let open_runner = rest_store(open_port, None);
    let open = full_round_trip(&open_runner).await;

    assert_eq!(
        with_auth, open,
        "the authed round trip must match the open-mode round trip exactly"
    );
    assert!(
        with_auth.load_is_default
            && with_auth.baseline_revision == Some(0)
            && with_auth.save_with_revision_ok
            && with_auth.revision_after_save == Some(1)
            && with_auth.claim_won
            && with_auth.stage_won
            && with_auth.heartbeat_ok
            && with_auth.workers_seen == 1,
        "the full round trip must land every step: {with_auth:?}"
    );
}

/// AC4: with auth NOT configured, /store still accepts unauthenticated ops
/// exactly as before P5a — observed on the real `JsonStateStore` (the
/// adapter an open deployment actually runs), so the claim is about the real
/// backend, not a double.
#[tokio::test]
async fn ac4_open_mode_still_accepts_unauthenticated_ops_exactly_as_before_p5a() {
    let state_dir = tempfile::tempdir().expect("state dir");
    let json = JsonStateStore::new(state_dir.path().join("state")).expect("json store");
    let (port, _dir) = boot(None, Arc::new(json)).await;

    let anon = rest_store(port, None);
    assert_eq!(
        anon.load().await.expect("open load"),
        ProjectState::default(),
        "open mode must load the default state with no credential"
    );
    let state = ProjectState {
        current_version: SemVer::new(2, 0, 0),
        tickets: vec![sample_bug("CXC-OPEN")],
        ..ProjectState::default()
    };
    anon.save(&state).await.expect("open save");
    assert_eq!(anon.load().await.expect("open reload"), state);

    let id = TicketId::new("CXC-OPEN").expect("valid id");
    assert!(
        anon.claim_ticket(&id, "runner@mac", NOW)
            .await
            .expect("open claim"),
        "an open Bug must be claimable with no credential, as before P5a"
    );
    assert!(
        !anon
            .claim_ticket(&id, "rival@mac", NOW)
            .await
            .expect("open claim held"),
        "the claim must hold against a rival, as before P5a"
    );
    let caps = WorkerCaps {
        engines: vec!["stub-engine".to_owned()],
        models: Vec::new(),
        git: None,
        tooling: None,
        version: "test".to_owned(),
    };
    anon.heartbeat_worker("runner@mac", "be", "CXC-OPEN", &caps, &now_rfc3339())
        .await
        .expect("open heartbeat");
    assert_eq!(
        anon.workers().await.expect("open workers").len(),
        1,
        "the heartbeat must surface in the registry, as before P5a"
    );
}
