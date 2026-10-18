// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The fleet spend cockpit (CXA-F278): hub-level cross-project cost
//! aggregation with per-project cap headroom and an OPTIONAL hub-level daily
//! soft ceiling. Visibility-only by design — it pauses nothing, alters no
//! cycle, and fires no per-project machinery; the only side effect is one
//! deduplicated system-chat alert when today's fleet total crosses the soft
//! ceiling's warning band. All the arithmetic lives in the pure core
//! ([`coxagent_application::fleet`]); this module is the thin adapter that
//! reads the stores and the live budget cells.

use super::*;

/// How often the soft-ceiling watchdog re-evaluates — same cadence as the
/// space budget watchdog, far faster than spend moves at.
const FLEET_WATCHDOG_INTERVAL: Duration = Duration::from_secs(300);

/// The `COX` author the machine's budget notices post under.
const COX: &str = "COX";

/// Map every registered project id (live or broken) to its space id, from the
/// spaces doc — the one place membership is written down.
async fn space_of(app: &AppState) -> HashMap<String, Option<String>> {
    let spaces = app.spaces.inner.lock().await.spaces.clone();
    let mut map: HashMap<String, Option<String>> = HashMap::new();
    for s in &spaces {
        for pid in &s.projects {
            map.insert(pid.clone(), Some(s.id.clone()));
        }
    }
    map
}

/// Snapshot the whole fleet for the cockpit: every registered project in
/// registration order plus every broken registration, zero-spend and flagged.
async fn fleet_snapshot(app: &AppState) -> coxagent_application::fleet::FleetSnapshot {
    use coxagent_application::fleet::{FleetProjectSpend, FleetSnapshot};
    let today = now_rfc3339().get(..10).unwrap_or("").to_owned();
    let space = space_of(app).await;
    // Handles are Arc-cheap: clone out under the registry read lock so the
    // per-project store loads below never hold it across an await (the same
    // rule the river follows — a create/delete or recovery admission must not
    // stall behind a chain of DB reads).
    let handles: Vec<ProjectHandle> = {
        let order = app.order.read().await;
        let map = app.projects.read().await;
        order
            .iter()
            .filter(|id| map.contains_key(*id))
            .filter_map(|pid| map.get(pid).cloned())
            .collect()
    };
    let mut projects = Vec::new();
    for h in &handles {
        // The LIVE budget cell the cycle loop honours — read it so the
        // cockpit stays correct right after a live budget edit. A poisoned
        // cell falls back to "no caps": the cockpit then shows the project
        // uncapped instead of guessing a cap it cannot read.
        let caps = h.budget.lock().map(|caps| *caps).unwrap_or_default();
        match h.store.load().await {
            Ok(state) => projects.push(FleetProjectSpend::live(
                &h.id,
                &h.name,
                space.get(&h.id).cloned().flatten(),
                &state,
                &today,
                caps,
            )),
            // A registered project whose store will not answer counts as
            // broken: zero spend, explicit flag — never silently dropped.
            Err(_) => {
                projects.push(FleetProjectSpend::broken(
                    &h.id,
                    space.get(&h.id).cloned().flatten(),
                ));
            }
        }
    }
    for b in app.broken.read().await.iter() {
        projects.push(FleetProjectSpend::broken(
            &b.id,
            space.get(&b.id).cloned().flatten(),
        ));
    }
    FleetSnapshot { projects }
}

/// Assemble the full cockpit payload (the GET body).
async fn build_fleet_cockpit(app: &AppState) -> coxagent_application::fleet::FleetCockpit {
    use coxagent_application::fleet::{build_cockpit, FleetSpaceInfo};
    let snapshot = fleet_snapshot(app).await;
    let spaces: Vec<FleetSpaceInfo> = app
        .spaces
        .inner
        .lock()
        .await
        .spaces
        .iter()
        .map(|s| FleetSpaceInfo {
            id: s.id.clone(),
            name: s.name.clone(),
            budget_usd: s.budget_usd,
        })
        .collect();
    let hub_ceiling = app.workspace.inner.lock().await.fleet_ceiling_usd;
    build_cockpit(&snapshot, &spaces, hub_ceiling, WARN_PCT)
}

/// GET /api/fleet/spend — the super-admin fleet cockpit payload. Open mode
/// (no auth configured) is allowed, exactly like `/api/manage/overview`.
pub(super) async fn fleet_spend_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "super admin required" })),
        )
            .into_response();
    }
    Json(build_fleet_cockpit(&app).await).into_response()
}

#[derive(serde::Deserialize)]
pub(super) struct FleetCeilingReq {
    /// `null` or `0` clears the ceiling (uncapped); a negative number is
    /// refused with 400 — a debt-collector ceiling is a typo, not a setting.
    ceiling_usd: Option<f64>,
}

/// PUT /api/fleet/ceiling — set/clear the hub-level daily soft ceiling.
/// Persisted to the workspace doc (survives restarts, shared across
/// replicas) and audited. Super admin only; open mode allowed.
pub(super) async fn fleet_ceiling_put_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(req): Json<FleetCeilingReq>,
) -> axum::response::Response {
    if !is_super(&app, &headers).await {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "super admin required" })),
        )
            .into_response();
    }
    let Some(value) = req.ceiling_usd else {
        return set_fleet_ceiling(&app, &headers, 0.0).await;
    };
    if !value.is_finite() || value < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "ceiling_usd must be a non-negative number or null" })),
        )
            .into_response();
    }
    // 0 = uncapped sentinel: a 0.5-cent ceiling is uncapped by any reading.
    let normalized = if value > 0.0 { value } else { 0.0 };
    set_fleet_ceiling(&app, &headers, normalized).await
}

/// Persist the (already validated) ceiling and answer the wire shape.
async fn set_fleet_ceiling(
    app: &AppState,
    headers: &axum::http::HeaderMap,
    ceiling: f64,
) -> axum::response::Response {
    {
        let mut ws = app.workspace.inner.lock().await;
        ws.fleet_ceiling_usd = ceiling;
    }
    app.workspace.save().await;
    let user = resolve_username(app, headers).await;
    let action = if ceiling > 0.0 {
        format!("hub fleet ceiling set to ${ceiling:.2}")
    } else {
        "hub fleet ceiling cleared".to_owned()
    };
    audit_push(&app.audit, &user, action, 200).await;
    Json(json!({ "ceiling_usd": ceiling })).into_response()
}

/// Today's fleet total plus each project's (id, today) pair — the watchdog's
/// inputs. The staleness filter (a counter from an earlier UTC day is not
/// today's burn) is the core's [`live_today_spend`]; the cockpit endpoint
/// deliberately keeps the raw counter for reconciliation. Live projects whose
/// store fails count zero (the cockpit endpoint is what flags them; the alert
/// is about burn, and a store that cannot be read cannot be shown to be
/// burning).
async fn fleet_today(app: &AppState, today: &str) -> (f64, Vec<(String, f64)>) {
    let mut total = 0.0;
    let mut per_project: Vec<(String, f64)> = Vec::new();
    // Clone the handles out under the read lock — same no-awaits-under-lock
    // rule as [`fleet_snapshot`].
    let handles: Vec<ProjectHandle> = {
        let order = app.order.read().await;
        let map = app.projects.read().await;
        order
            .iter()
            .filter(|id| map.contains_key(*id))
            .filter_map(|pid| map.get(pid).cloned())
            .collect()
    };
    for h in &handles {
        let today_spend = match h.store.load().await {
            Ok(state) => coxagent_application::fleet::live_today_spend(&state, today),
            Err(_) => 0.0,
        };
        total += today_spend;
        per_project.push((h.id.clone(), today_spend));
    }
    (total, per_project)
}

/// The hub-level daily soft-ceiling watchdog (CXA-F278). SOFT by design: it
/// only ever posts ONE system-chat notice into `#general` naming the projects
/// above their pro-rata share — no runner is paused and no cycle is altered
/// (the hard-stop pause behaviour already exists per-space). Dedupe state is
/// in-memory: after a restart it re-evaluates and re-notifies at most once,
/// and the UTC-midnight spend reset re-arms it naturally.
pub(super) async fn fleet_ceiling_watchdog(app: AppState) {
    use coxagent_application::fleet::{above_pro_rata, hub_soft_alert, HubAlert};
    let mut warned = false;
    loop {
        tokio::time::sleep(FLEET_WATCHDOG_INTERVAL).await;
        let ceiling = app.workspace.inner.lock().await.fleet_ceiling_usd;
        let today = now_rfc3339().get(..10).unwrap_or("").to_owned();
        let (total_today, per_project) = fleet_today(&app, &today).await;
        match hub_soft_alert(total_today, ceiling, warned, WARN_PCT) {
            HubAlert::Fire => {
                let burners = above_pro_rata(&per_project, ceiling, WARN_PCT);
                let pct = if ceiling > 0.0 {
                    total_today / ceiling * 100.0
                } else {
                    0.0
                };
                let names = burners
                    .iter()
                    .map(|(id, usd)| format!("{id} (${usd:.2})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let msg = format!(
                    "⚠️ FLEET BUDGET: the hub burned ${total_today:.2} today against its \
                     ${ceiling:.2} daily soft ceiling ({pct:.0}%). Above pro-rata share: \
                     {names}. Heads-up only — nothing is paused. Raise or clear it via \
                     Manage → Fleet spend.",
                );
                deliver_syschat(
                    &app,
                    COX,
                    &msg,
                    coxagent_application::GENERAL_CHANNEL,
                    Vec::new(),
                )
                .await;
                tracing::warn!(
                    "fleet daily soft ceiling crossed: ${total_today:.2} / ${ceiling:.2}"
                );
                warned = true;
            }
            // Left the band (UTC reset or ceiling raised): re-arm so the next
            // crossing fires again; ceiling removed: drop any stale arming.
            HubAlert::ReArm | HubAlert::Clear => warned = false,
            HubAlert::Hold => {}
        }
    }
}
