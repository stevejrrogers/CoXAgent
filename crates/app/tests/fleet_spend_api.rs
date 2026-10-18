//! CXA-F278 endpoint contract: /api/fleet/spend and /api/fleet/ceiling.
//!
//! A hub boots with three live projects backed by in-memory stores (one over
//! its lifetime cap, one inside the warning band, one uncapped), one broken
//! registration, and one space — so real HTTP behaviour pins the whole AC set:
//! fleet totals reconcile with the per-project state, the broken project is
//! flagged with zero spend instead of silently dropped, uncapped projects show
//! null headroom and never alert, the ceiling round-trips into the hub's
//! workspace file, and only a super admin (or open mode) gets past the gate.

#![allow(clippy::unwrap_used, clippy::float_cmp)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use coxagent_application::auth::{
    AuthPort, AuthRole, AuthUser, LoginResult, SessionInfo, TokenInfo,
};
use coxagent_application::config::BudgetCaps;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::ports::outbound::{AgentEnginePort, AgentOutcome, AgentRequest};
use coxagent_application::state::SpendDay;
use coxagent_application::{PortError, ProjectState};
use coxagent_presentation::{BrokenProject, HubExtras, ProjectHandle};

/// Fixed, high, and unique to this test file so it does not race the other
/// hub-booting tests in this crate for a port.
const PORT: u16 = 47_860;
/// Second hub instance (open mode) — also fixed and unique within this file.
const OPEN_PORT: u16 = 47_861;

/// Today in the hub's own wire format (`YYYY-MM-DD`), so seeded daily numbers
/// are counted as "today" no matter when the suite runs.
fn today() -> String {
    time::OffsetDateTime::now_utc().date().to_string()
}

fn yesterday() -> String {
    (time::OffsetDateTime::now_utc().date() - time::Duration::days(1)).to_string()
}

struct MemStore {
    state: Mutex<ProjectState>,
}

#[async_trait]
impl StateStorePort for MemStore {
    async fn load(&self) -> Result<ProjectState, PortError> {
        Ok(self.state.lock().unwrap().clone())
    }
    async fn save(&self, s: &ProjectState) -> Result<(), PortError> {
        *self.state.lock().unwrap() = s.clone();
        Ok(())
    }
}

struct StubEngine;
#[async_trait]
impl AgentEnginePort for StubEngine {
    fn id(&self) -> &'static str {
        "stub"
    }
    async fn run(&self, _rq: AgentRequest) -> Result<AgentOutcome, PortError> {
        unreachable!("fleet endpoints never run an engine")
    }
}

/// Session store with no I/O: a fixed token per role. Mirrors the double in
/// `store_rpc_auth_gate.rs`.
struct StubAuth;

impl StubAuth {
    fn token(role: AuthRole) -> String {
        format!("session-{}", role.as_str())
    }
}

#[async_trait]
impl AuthPort for StubAuth {
    async fn login(&self, _u: &str, _p: &str, _t: Option<&str>) -> LoginResult {
        LoginResult::Denied
    }
    async fn user_for(&self, token: &str) -> Option<AuthUser> {
        let role = token
            .strip_prefix("session-")
            .map(AuthRole::from_str_lenient)?;
        Some(AuthUser {
            username: format!("user-{}", role.as_str()),
            name: String::new(),
            email: String::new(),
            role,
            projects: Vec::new(),
        })
    }
    async fn logout(&self, _token: &str) {}
    async fn principal_for_bearer(&self, _token: &str) -> Option<AuthUser> {
        None
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

fn handle(id: &str, caps: BudgetCaps, state: ProjectState) -> ProjectHandle {
    let dir = std::env::temp_dir();
    ProjectHandle {
        id: id.to_owned(),
        name: id.to_owned(),
        alias: id.to_owned(),
        store: Arc::new(MemStore {
            state: Mutex::new(state),
        }),
        runner: Arc::new(coxagent_application::use_cases::RunnerHandle::new()),
        outbox: None,
        config_path: dir.join("coxagent.json"),
        engine: Arc::new(StubEngine),
        work_dir: dir.clone(),
        budget: Arc::new(Mutex::new(caps)),
        context_path: dir.join("context.md"),
        forge: None,
        deploy: None,
        files: None,
        deps_discovery: None,
        storage: None,
    }
}

/// `over`: 12 of a 10 lifetime cap (headroom 0, status "over"), today 3.0 of a
/// 4.0 daily cap, and a closed 2.5 day inside the trailing window.
fn over_state() -> ProjectState {
    let mut s = ProjectState::default();
    s.spend.total_cost_usd = 12.0;
    s.spend_today_usd = 3.0;
    s.spend_day = today();
    s.spend_history.push(SpendDay {
        day: yesterday(),
        usd: 2.5,
    });
    s
}

/// `approaching`: 8.5 of a 10 cap — inside the 80% warning band.
fn approaching_state() -> ProjectState {
    let mut s = ProjectState::default();
    s.spend.total_cost_usd = 8.5;
    s.spend_today_usd = 2.0;
    s.spend_day = today();
    s
}

/// `uncapped`: no caps at all, still counts toward totals.
fn uncapped_state() -> ProjectState {
    let mut s = ProjectState::default();
    s.spend.total_cost_usd = 1.0;
    s.spend_today_usd = 0.5;
    s.spend_day = today();
    s
}

/// Boot a hub on `port` with the full fixture fleet; `auth` `None` runs open
/// mode. Returns the hub dir so the ceiling round-trip can inspect the
/// persisted workspace file.
async fn boot(auth: Option<Arc<dyn AuthPort>>, port: u16) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // One space grouping the two capped projects, seeded pre-boot the same way
    // a real hub dir is laid out (spaces.json under the hub dir).
    std::fs::write(
        dir.path().join("spaces.json"),
        serde_json::json!({
            "spaces": [{
                "id": "s1", "name": "Alpha", "projects": ["over", "approaching"],
                "budget_usd": 9.0, "admins": [], "members": [],
                "created_by": "", "created_at": ""
            }]
        })
        .to_string(),
    )
    .unwrap();
    let extras = HubExtras {
        auth,
        hub_dir: Some(dir.path().to_path_buf()),
        broken: vec![BrokenProject {
            id: "broken".to_owned(),
            config_path: std::path::PathBuf::from("/w/broken/coxagent.json"),
            error: "invalid config".to_owned(),
        }],
        ..Default::default()
    };
    let audit: Arc<dyn coxagent_application::ports::outbound::AuditPort> =
        Arc::new(coxagent_infrastructure::MemoryAuditSink::default());
    tokio::spawn(coxagent_presentation::serve_full(
        vec![
            handle(
                "over",
                BudgetCaps {
                    lifetime_usd: Some(10.0),
                    daily_usd: Some(4.0),
                },
                over_state(),
            ),
            handle(
                "approaching",
                BudgetCaps {
                    lifetime_usd: Some(10.0),
                    daily_usd: None,
                },
                approaching_state(),
            ),
            handle("uncapped", BudgetCaps::default(), uncapped_state()),
        ],
        port,
        audit,
        extras,
    ));
    let client = reqwest::Client::new();
    let health = format!("http://127.0.0.1:{port}/api/health");
    for _ in 0..50 {
        if client.get(&health).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    dir
}

/// GET the fleet payload as `role` (`None` = anonymous), returning
/// `(status, body)`.
async fn get_fleet(port: u16, role: Option<AuthRole>) -> (u16, serde_json::Value) {
    let mut builder =
        reqwest::Client::new().get(format!("http://127.0.0.1:{port}/api/fleet/spend"));
    if let Some(role) = role {
        builder = builder.header(
            reqwest::header::COOKIE,
            format!("cox_session={}", StubAuth::token(role)),
        );
    }
    let resp = builder.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or_default())
}

async fn put_ceiling(
    port: u16,
    role: Option<AuthRole>,
    ceiling: serde_json::Value,
) -> (u16, serde_json::Value) {
    let mut builder = reqwest::Client::new()
        .put(format!("http://127.0.0.1:{port}/api/fleet/ceiling"))
        .json(&ceiling);
    if let Some(role) = role {
        builder = builder.header(
            reqwest::header::COOKIE,
            format!("cox_session={}", StubAuth::token(role)),
        );
    }
    let resp = builder.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or_default())
}

fn row<'v>(v: &'v serde_json::Value, id: &str) -> &'v serde_json::Value {
    v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("row {id} missing from the fleet payload"))
}

/// One test per hub instance: the assertions share a process-wide TCP bind, so
/// they live in a single `#[tokio::test]` rather than racing each other. The
/// phases are extracted into helpers purely to keep each function readable.
#[tokio::test]
async fn fleet_spend_aggregates_flags_and_reconciles_for_a_super_admin() {
    let hub = boot(Some(Arc::new(StubAuth)), PORT).await;
    assert_gate_admits_only_the_super_admin(PORT).await;
    let body = assert_fleet_shape_totals_rows_and_spaces(PORT).await;
    assert_rows_reconcile_with_project_state(PORT, &body).await;
    assert_ceiling_round_trips_and_persists(PORT, hub.path()).await;
}

/// Gate: ordinary roles and anonymous do not get past the fleet endpoint.
async fn assert_gate_admits_only_the_super_admin(port: u16) {
    let (status, _) = get_fleet(port, Some(AuthRole::Fe)).await;
    assert_eq!(status, 403, "member-tier must be refused");
    let (status, _) = get_fleet(port, None).await;
    assert_eq!(
        status, 401,
        "anonymous must be refused by the auth middleware"
    );
}

/// Wire shape, totals, per-project rows and the space rollup for a super
/// admin's GET. Returns the body so the reconciliation phase can reuse it.
async fn assert_fleet_shape_totals_rows_and_spaces(port: u16) -> serde_json::Value {
    let (status, body) = get_fleet(port, Some(AuthRole::Super)).await;
    assert_eq!(status, 200);

    // Wire shape: warn pct + uncapped-by-default ceiling.
    assert_eq!(body["hub_warn_pct"], 0.8);
    assert_eq!(body["hub_ceiling_usd"], 0.0, "fresh hub = uncapped");
    assert!(body["totals"]["hub_headroom_usd"].is_null());

    // Totals reconcile with the seeded per-project state: 12 + 8.5 + 1.
    assert_eq!(body["totals"]["spend_usd"], 21.5);
    assert_eq!(body["totals"]["today_usd"], 5.5);
    // Trailing 7 days = today's 5.5 + "over"'s closed 2.5 day.
    assert_eq!(body["totals"]["spend_7d_usd"], 8.0);
    // Four projects (3 live + 1 broken), flagged.
    assert_eq!(body["totals"]["projects"], 4);
    assert_eq!(body["totals"]["over"], 1);
    assert_eq!(body["totals"]["approaching"], 1);
    assert_eq!(
        body["totals"]["broken"], 1,
        "the broken registration is flagged, not dropped"
    );

    // Sorted by burn, highest first.
    let ids: Vec<&str> = body["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["over", "approaching", "uncapped", "broken"]);

    // Over-cap row: headroom saturated at 0, daily headroom 1.0.
    let over = row(&body, "over");
    assert_eq!(over["status"], "over");
    assert_eq!(over["spend_usd"], 12.0);
    assert_eq!(over["lifetime_cap_usd"], 10.0);
    assert_eq!(over["headroom_usd"], 0.0);
    assert_eq!(over["daily_cap_usd"], 4.0);
    assert_eq!(over["headroom_today_usd"], 1.0);
    assert_eq!(over["space_id"], "s1");
    assert_eq!(over["broken"], false);

    // Approaching row: inside the 80% band.
    let approaching = row(&body, "approaching");
    assert_eq!(approaching["status"], "approaching");
    assert_eq!(approaching["headroom_usd"], 1.5);

    // Uncapped row: null caps/headroom, status ok, still counted.
    let uncapped = row(&body, "uncapped");
    assert_eq!(uncapped["status"], "ok");
    assert!(uncapped["lifetime_cap_usd"].is_null());
    assert!(uncapped["headroom_usd"].is_null());
    assert!(uncapped["headroom_today_usd"].is_null());

    // Broken row: zero spend, explicit flag, no caps.
    let broken = row(&body, "broken");
    assert_eq!(broken["broken"], true);
    assert_eq!(broken["spend_usd"], 0.0);
    assert_eq!(broken["today_usd"], 0.0);
    assert!(broken["headroom_usd"].is_null());
    assert_eq!(broken["status"], "ok");

    // Space rollup: s1 groups over + approaching (12 + 8.5 = 20.5 vs 9 cap → over).
    assert_eq!(body["spaces"].as_array().unwrap().len(), 1);
    assert_eq!(body["spaces"][0]["id"], "s1");
    assert_eq!(body["spaces"][0]["spend_usd"], 20.5);
    assert_eq!(body["spaces"][0]["budget_usd"], 9.0);
    assert_eq!(body["spaces"][0]["status"], "over");

    body
}

/// AC5 — reconciliation: every fleet row number equals the SAME fields the
/// per-project state endpoint publishes for the same saved state.
async fn assert_rows_reconcile_with_project_state(port: u16, body: &serde_json::Value) {
    let client = reqwest::Client::new();
    for pid in ["over", "approaching", "uncapped"] {
        let state: serde_json::Value = client
            .get(format!("http://127.0.0.1:{port}/api/projects/{pid}/state"))
            .header(
                reqwest::header::COOKIE,
                format!("cox_session={}", StubAuth::token(AuthRole::Super)),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let r = row(body, pid);
        assert_eq!(
            r["spend_usd"], state["spend"]["total_cost_usd"],
            "{pid}: fleet total must equal the project's own meter"
        );
        assert_eq!(
            r["today_usd"], state["spend_today_usd"],
            "{pid}: today's burn must match"
        );
    }
}

/// Ceiling round-trip: the gate refuses a member, a negative ceiling is
/// refused, the super admin's PUT persists into the hub workspace file and
/// clearing (null) returns to uncapped.
async fn assert_ceiling_round_trips_and_persists(port: u16, hub_dir: &std::path::Path) {
    // Ceiling round-trip: a member cannot set it…
    let (status, _) = put_ceiling(
        port,
        Some(AuthRole::Fe),
        serde_json::json!({"ceiling_usd": 50.0}),
    )
    .await;
    assert_eq!(status, 403);
    // …a negative ceiling is refused…
    let (status, _) = put_ceiling(
        port,
        Some(AuthRole::Super),
        serde_json::json!({"ceiling_usd": -1.0}),
    )
    .await;
    assert_eq!(status, 400);
    // …and the super admin's PUT persists and shows up in GET.
    let (status, set) = put_ceiling(
        port,
        Some(AuthRole::Super),
        serde_json::json!({"ceiling_usd": 50.0}),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(set["ceiling_usd"], 50.0);
    let (_, body) = get_fleet(port, Some(AuthRole::Super)).await;
    assert_eq!(body["hub_ceiling_usd"], 50.0);
    assert_eq!(body["totals"]["hub_headroom_usd"], 44.5, "50 - today's 5.5");
    // Persisted to the hub-dir workspace file (survives restarts).
    let ws = std::fs::read_to_string(hub_dir.join("workspace.json")).unwrap();
    let ws: serde_json::Value = serde_json::from_str(&ws).unwrap();
    assert_eq!(ws["fleet_ceiling_usd"], 50.0);
    // Clearing (null) returns to uncapped.
    let (status, _) = put_ceiling(
        port,
        Some(AuthRole::Super),
        serde_json::json!({"ceiling_usd": null}),
    )
    .await;
    assert_eq!(status, 200);
    let (_, body) = get_fleet(port, Some(AuthRole::Super)).await;
    assert_eq!(body["hub_ceiling_usd"], 0.0);
    assert!(body["totals"]["hub_headroom_usd"].is_null());
}

/// Open mode (no auth configured) — the same surface the single-operator
/// local hub runs — must reach the cockpit without a session.
#[tokio::test]
async fn open_mode_sees_the_fleet_and_sets_the_ceiling() {
    let _hub = boot(None, OPEN_PORT).await;
    let (status, body) = get_fleet(OPEN_PORT, None).await;
    assert_eq!(status, 200);
    assert_eq!(body["totals"]["projects"], 4);
    let (status, set) = put_ceiling(OPEN_PORT, None, serde_json::json!({"ceiling_usd": 6.0})).await;
    assert_eq!(status, 200);
    assert_eq!(set["ceiling_usd"], 6.0);
    let (_, body) = get_fleet(OPEN_PORT, None).await;
    assert_eq!(body["hub_ceiling_usd"], 6.0);
}
