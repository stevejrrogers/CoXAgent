// One logical module split across files for merge-conflict surface, not an API
// boundary — see server/mod.rs.
#![allow(clippy::wildcard_imports)]
//! The cross-project fleet river (CXA-F233): one SSE stream of every agent's
//! live activity across ALL registered projects at once. Where `/api/projects/
//! :pid/events` streams one project's 1 Hz snapshot, this endpoint fans the
//! same shapes out hub-wide — each project's runner phase, its
//! [`lite_state_value`], and every new [`ActivityEntry`] as it lands — so an
//! operator can intercept a runaway loop at the moment it happens instead of
//! after the ledger compiles.
//!
//! Transport note (design deviation, deliberate): the SA design predated the
//! current layout and proposed bridging activity over the Redis `cox:events`
//! bus. Activity is PERSISTED project state (`ProjectState.activity`), unlike
//! the in-memory syschat the bus actually carries, so polling each store —
//! exactly what the single-project stream has always done — is correct for
//! every deployment topology the store itself supports (shared store = every
//! instance sees the same entries; local store = one instance owns them).
//! No new transport was invented.

use super::*;
use coxagent_application::state::{ActivityEntry, ProjectState};
use coxagent_application::use_cases::RunnerSnapshot;
use futures_util::StreamExt;

/// How far back a (re)connecting client is caught up. EventSource reconnects
/// are new connections by design; the backlog caps each one so a client that
/// was away for hours resumes with recent history plus liveness — never an
/// unbounded replay of the (60-entry, [`MAX_ACTIVITY`]-bounded) feed.
const RIVER_BACKLOG: usize = 20;

/// The friendly empty-state payload (AC2): an explicit, renderable prompt —
/// never an error status — for an empty fleet or a filter that excludes
/// everything.
const EMPTY_RIVER_MESSAGE: &str = "No agent activity is flowing for this view yet — the river starts as soon as a registered project matches.";

/// Query parameters (the SA contract's names, on the AC's path):
/// `?scope=hub|space&space=<id>&project_id=<pid>&phase=<PHASE>`.
#[derive(serde::Deserialize)]
pub(super) struct RiverQuery {
    /// Restrict the river to a single project id.
    project_id: Option<String>,
    /// Restrict to one agent phase: matches the runner's active role
    /// (`"DEV"` matches `"DEV-FEATURE"`) and activity rows by the same rule.
    phase: Option<String>,
    /// `hub` (default) = every visible project; `space` = only the named space.
    scope: Option<String>,
    /// Space id when `scope=space`.
    space: Option<String>,
}

/// The slice of a [`ProjectHandle`] the river streams — cloned once per
/// connection so the registry read lock is never held across an `await`.
#[derive(Clone)]
struct RiverProject {
    id: String,
    name: String,
    alias: String,
    store: Arc<dyn StateStorePort>,
    runner: Arc<RunnerHandle>,
}

impl RiverProject {
    fn of(h: &ProjectHandle) -> Self {
        Self {
            id: h.id.clone(),
            name: h.name.clone(),
            alias: h.alias.clone(),
            store: Arc::clone(&h.store),
            runner: Arc::clone(&h.runner),
        }
    }
}

/// Per-connection stream state, shared with each tick's future.
struct RiverState {
    projects: Vec<RiverProject>,
    /// Registered-but-unloadable projects (COX-B043): a label and a reason —
    /// no store, no runner — shown with their broken-reason marker instead of
    /// dropping silently out of the fleet view. Boot-time facts, so they are
    /// announced once per connection, never re-polled.
    broken: Vec<BrokenProject>,
    /// Last activity entry forwarded per project — the delta marker.
    seen: HashMap<String, Option<ActivityEntry>>,
    /// Monotonic per-project sequence, so a client can order/reconnect sanely.
    seq: HashMap<String, u64>,
    booted: bool,
}

/// Which registered projects this caller may see in the river. Super/Admin and
/// open mode (no auth configured) see the whole fleet; every other role sees
/// only its assigned projects — the SAME per-project membership `auth_mw`
/// enforces on `/api/projects/:pid/*`, so a hub-level stream never becomes a
/// cross-project leak.
fn river_scope(user: Option<&coxagent_application::AuthUser>, registered: &[String]) -> Vec<String> {
    let Some(u) = user else {
        return registered.to_vec();
    };
    let sees_all = matches!(
        u.role,
        coxagent_application::auth::AuthRole::Super | coxagent_application::auth::AuthRole::Admin
    );
    if sees_all {
        return registered.to_vec();
    }
    registered
        .iter()
        .filter(|id| u.projects.contains(id))
        .cloned()
        .collect()
}

/// Whether this caller may see a broken project's marker. Broken records are
/// deliberately NOT in the live registry, so membership is checked against the
/// caller's own assignment list — the same visibility rule as [`river_scope`],
/// applied to a project the hub could not load.
fn may_see_broken(user: Option<&coxagent_application::AuthUser>, id: &str) -> bool {
    match user {
        // Open mode (no auth configured): everything is visible.
        None => true,
        Some(u) => {
            matches!(
                u.role,
                coxagent_application::auth::AuthRole::Super | coxagent_application::auth::AuthRole::Admin
            ) || u.projects.iter().any(|p| p == id)
        }
    }
}

/// Narrow the visible set by the query filters, preserving registration order.
/// `None` filters pass everything through.
fn filter_projects(
    allowed: &[String],
    project_id: Option<&str>,
    space: Option<&[String]>,
) -> Vec<String> {
    allowed
        .iter()
        .filter(|id| {
            project_id.map_or(true, |pid| pid == id.as_str())
                && space.map_or(true, |sp| sp.contains(id))
        })
        .cloned()
        .collect()
}

/// Normalize a phase filter: trimmed, uppercase; empty string = no filter.
fn phase_norm(phase: Option<&str>) -> String {
    phase.map_or("", str::trim).to_ascii_uppercase()
}

/// Does this runner sit in the filtered phase? An empty filter matches all.
/// Role labels compose the phase with the kind of work (`DEV-FEATURE`), so the
/// bare phase matches as a prefix; the comparison is case-insensitive so the
/// matcher is safe to call with raw query input.
fn runner_in_phase(phase: &str, snap: &RunnerSnapshot) -> bool {
    let phase = phase.trim().to_ascii_uppercase();
    phase.is_empty()
        || snap
            .active_role
            .as_deref()
            .is_some_and(|r| r.to_ascii_uppercase().starts_with(&phase))
}

/// Does this activity row belong to the filtered phase? Same prefix rule as
/// [`runner_in_phase`], applied to the row's agent label.
fn entry_in_phase(phase: &str, agent: &str) -> bool {
    let phase = phase.trim().to_ascii_uppercase();
    phase.is_empty() || agent.to_ascii_uppercase().starts_with(&phase)
}

/// A project needs a human when the machine is holding work for one:
/// `ProjectState.human_holds` is the codebase's canonical "held for a person"
/// map (the inbox's `human_eyes` items come from it). The flag rides on every
/// river event so the view can mark those rows.
fn human_action_needed(state: &ProjectState) -> bool {
    !state.human_holds.is_empty()
}

/// Fewer than two completed cycles — the river cannot say anything meaningful
/// about the project's rhythm yet, and the view renders "(insufficient data)"
/// instead of pretending otherwise (AC5; trends uses three, the river is the
/// stricter edge).
fn insufficient_data(state: &ProjectState) -> bool {
    coxagent_application::metrics_health::completed_cycles(state) < 2
}

/// The entries newer than what the stream last forwarded. Bounded in every
/// branch: a client that reconnects (or a feed that rotated past its cap)
/// catches up on the newest `RIVER_BACKLOG` entries, never an unbounded replay.
/// Pure over the real [`ActivityEntry`] feed so the reconnect semantics are
/// testable without a server.
fn delta<'a>(
    seen: Option<&ActivityEntry>,
    activity: &'a [ActivityEntry],
    cap: usize,
) -> &'a [ActivityEntry] {
    let Some(last) = seen else {
        let start = activity.len().saturating_sub(cap);
        return &activity[start..];
    };
    match activity.iter().rposition(|e| e == last) {
        Some(i) if i + 1 < activity.len() => {
            let start = (i + 1).max(activity.len().saturating_sub(cap));
            &activity[start..]
        }
        Some(_) => &[],
        // The seen entry was trimmed away (feed rotated) — resend the newest tail.
        None => {
            let start = activity.len().saturating_sub(cap);
            &activity[start..]
        }
    }
}

/// The `agent_activity` payload: the design's wire shape —
/// `{"type":"agent_activity","project_id",...,"entry":{...},"seq":N}` — plus
/// the flags the view renders (AC3/AC5). Pure, so the wire contract is
/// assertable without a server.
fn agent_activity_payload(
    pid: &str,
    seq: u64,
    entry: &ActivityEntry,
    needs_human: bool,
    insufficient: bool,
) -> serde_json::Value {
    serde_json::json!({
        "type": "agent_activity",
        "project_id": pid,
        "seq": seq,
        "entry": entry,
        "needs_human": needs_human,
        "insufficient_data": insufficient,
    })
}

/// One `project_state` heartbeat: the per-project twin of the single-project
/// stream's payload (`runner` + `state` shaped like [`lite_state_value`]) with
/// the fleet-level extras the river view needs.
fn project_state_payload(
    p: &RiverProject,
    state: &ProjectState,
    viewers: usize,
    online: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "type": "project_state",
        "project_id": p.id,
        "name": p.name,
        "alias": p.alias,
        "runner": serde_json::to_value(p.runner.snapshot()).unwrap_or_default(),
        "state": lite_state_value(state),
        "viewers": viewers,
        "online": online,
        "needs_human": human_action_needed(state),
        "insufficient_data": insufficient_data(state),
    })
}

fn empty_payload() -> serde_json::Value {
    serde_json::json!({ "type": "empty", "message": EMPTY_RIVER_MESSAGE, "projects": [] })
}

/// First tick of a connection: hello (so the view can build its filters),
/// then the bounded backlog, then one immediate heartbeat per project so the
/// phase strip populates without waiting a full second. Returns bare JSON
/// payloads — SSE framing happens once, in the handler.
async fn river_boot(
    sh: &mut RiverState,
    phase: &str,
    viewers: usize,
    online: &[String],
) -> Vec<serde_json::Value> {
    // The fleet is only "empty" when nothing at all is registered — broken
    // projects still deserve their markers, not the empty-state prompt.
    if sh.projects.is_empty() && sh.broken.is_empty() {
        return vec![empty_payload()];
    }
    let mut out = vec![serde_json::json!({
        "type": "hello",
        "projects": sh.projects.iter().map(|p| serde_json::json!({
            "id": p.id, "name": p.name, "alias": p.alias,
        })).chain(sh.broken.iter().map(|b| serde_json::json!({
            "id": b.id, "name": b.id, "alias": "", "broken": true, "error": b.error,
        }))).collect::<Vec<_>>(),
        "viewers": viewers,
        "online": online,
    })];
    // Broken projects: one marker event each, then done — a config that
    // cannot parse has no store to poll and no runner to snapshot.
    for b in &sh.broken {
        out.push(serde_json::json!({
            "type": "project_broken",
            "project_id": b.id,
            "name": b.id,
            "error": b.error,
            "config_path": b.config_path.display().to_string(),
        }));
    }
    // Iterate an owned copy so `seen`/`seq` stay mutable inside the loop
    // (handles are Arc-cheap to clone).
    for p in sh.projects.clone() {
        let Some(state) = p.store.load().await.ok() else {
            sh.seen.insert(p.id.clone(), None);
            continue;
        };
        for entry in delta(None, &state.activity, RIVER_BACKLOG) {
            if !entry_in_phase(phase, &entry.agent) {
                continue;
            }
            let seq = {
                let s = sh.seq.entry(p.id.clone()).or_insert(0);
                *s += 1;
                *s
            };
            out.push(agent_activity_payload(
                &p.id,
                seq,
                entry,
                human_action_needed(&state),
                insufficient_data(&state),
            ));
        }
        sh.seen
            .insert(p.id.clone(), state.activity.last().cloned());
        let snap = p.runner.snapshot();
        if runner_in_phase(phase, &snap) {
            out.push(project_state_payload(&p, &state, viewers, online));
        }
    }
    out
}

/// One tick: heartbeats for in-phase projects plus the new activity rows since
/// the previous tick (deltas bounded by [`delta`]). Bare payloads, like
/// [`river_boot`].
async fn river_tick(
    shared: &tokio::sync::Mutex<RiverState>,
    phase: &str,
    viewers: usize,
    online: &[String],
) -> Vec<serde_json::Value> {
    let mut sh = shared.lock().await;
    if !sh.booted {
        sh.booted = true;
        return river_boot(&mut sh, phase, viewers, online).await;
    }
    let mut out = Vec::new();
    // Owned copy again: the loop body mutates the `seen`/`seq` maps.
    for p in sh.projects.clone() {
        let Some(state) = p.store.load().await.ok() else {
            continue;
        };
        // News first, then the state heartbeat — the same order as boot.
        let fresh: Vec<ActivityEntry> = {
            let seen = sh.seen.get(&p.id).and_then(|o| o.as_ref());
            delta(seen, &state.activity, RIVER_BACKLOG).to_vec()
        };
        for entry in &fresh {
            if !entry_in_phase(phase, &entry.agent) {
                continue;
            }
            let seq = {
                let s = sh.seq.entry(p.id.clone()).or_insert(0);
                *s += 1;
                *s
            };
            out.push(agent_activity_payload(
                &p.id,
                seq,
                entry,
                human_action_needed(&state),
                insufficient_data(&state),
            ));
        }
        let snap = p.runner.snapshot();
        if runner_in_phase(phase, &snap) {
            out.push(project_state_payload(&p, &state, viewers, online));
        }
        // The seen marker advances regardless of the phase filter, so a row
        // hidden now is never replayed when the filter is lifted.
        sh.seen
            .insert(p.id.clone(), state.activity.last().cloned());
    }
    out
}

/// GET /api/fleet/river — the cross-project live agent activity river.
/// Auth goes through the same session/bearer guard as every other route
/// (`auth_mw`); visibility is scoped per caller by [`river_scope`], and viewer
/// counts register through the shared per-user [`ViewerGuard`] registry, so a
/// person in five browser tabs is one online user — exactly like the
/// single-project stream.
pub(super) async fn fleet_river_ep(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<RiverQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let principal = match &app.auth {
        Some(auth) => resolve_principal(auth, &headers).await,
        None => None,
    };
    // Identify the viewer so distinct-user counts (not tab counts) are reported.
    let viewer =
        principal.as_ref().map_or_else(|| "local".to_owned(), |u| u.username.clone());
    let phase = phase_norm(q.phase.as_deref());
    // Registry order is the display order; the read lock is held only here.
    let (registered, space_projects) = {
        let order = app.order.read().await;
        let map = app.projects.read().await;
        let registered: Vec<String> = order
            .iter()
            .filter(|id| map.contains_key(*id))
            .cloned()
            .collect();
        // `scope=space` narrows to the named space's projects. A missing or
        // unknown space id behaves like any other filter that matches
        // nothing — the friendly empty payload — never a silent fallback to
        // the whole fleet, which is what a typo'd filter must not do.
        let space_projects = match q.scope.as_deref() {
            Some("space") => Some(match q.space.as_deref() {
                Some(sid) => app
                    .spaces
                    .inner
                    .lock()
                    .await
                    .spaces
                    .iter()
                    .find(|s| s.id == sid)
                    .map(|s| s.projects.clone())
                    .unwrap_or_default(),
                None => Vec::new(),
            }),
            _ => None,
        };
        (registered, space_projects)
    };
    let visible = river_scope(principal.as_ref(), &registered);
    let included = filter_projects(
        &visible,
        q.project_id.as_deref(),
        space_projects.as_deref(),
    );
    let projects: Vec<RiverProject> = {
        let map = app.projects.read().await;
        included.iter().filter_map(|id| map.get(id).map(RiverProject::of)).collect()
    };
    // A project filter also narrows the broken markers: naming a live project
    // hides unrelated broken ones; no filter shows every visible registration.
    // Markers obey the same membership rule as live projects — a broken
    // project still carries its config path, which is not other teams' business.
    let broken: Vec<BrokenProject> = {
        let broken = app.broken.read().await;
        broken
            .iter()
            .filter(|b| {
                may_see_broken(principal.as_ref(), &b.id)
                    && q.project_id.as_deref().map_or(true, |pid| pid == b.id)
            })
            .cloned()
            .collect()
    };
    let guard = ViewerGuard::new(&app.viewers, viewer);
    let shared = Arc::new(tokio::sync::Mutex::new(RiverState {
        projects,
        broken,
        seen: HashMap::new(),
        seq: HashMap::new(),
        booted: false,
    }));
    // `guard` is owned by this closure, so the count drops when the stream ends.
    // Each tick yields bare JSON payloads; SSE framing happens here, once.
    let stream = IntervalStream::new(tokio::time::interval(STREAM_INTERVAL))
        .then(move |_| {
            let shared = Arc::clone(&shared);
            let count = guard.count();
            let online = guard.online_users();
            let phase = phase.clone();
            async move { river_tick(&shared, &phase, count, &online).await }
        })
        .map(|payloads| {
            futures_util::stream::iter(
                payloads
                    .into_iter()
                    .map(|v| Ok::<_, Infallible>(Event::default().data(v.to_string()))),
            )
        })
        .flatten();
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod fleet_river_tests {
    use super::*;
    use coxagent_application::auth::{AuthRole, AuthUser};
    use coxagent_application::state::CycleScore;

    fn principal(role: AuthRole, projects: &[&str]) -> AuthUser {
        AuthUser {
            username: "op".to_owned(),
            name: String::new(),
            email: String::new(),
            role,
            projects: projects.iter().map(|p| (*p).to_owned()).collect(),
        }
    }

    fn ids(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    /// The hub-level river honours the same per-project membership `auth_mw`
    /// enforces on every `/api/projects/:pid/*` route — a member-tier user
    /// must not see other teams' agents in a fleet-wide stream.
    #[test]
    fn a_member_sees_only_assigned_projects_in_registration_order() {
        let registered = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let u = principal(AuthRole::Fe, &["c", "a"]);
        assert_eq!(ids(&river_scope(Some(&u), &registered)), vec!["a", "c"]);
    }

    #[test]
    fn super_admin_and_open_mode_see_the_whole_fleet() {
        let registered = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(ids(&river_scope(None, &registered)), vec!["a", "b"]);
        let u = principal(AuthRole::Super, &[]);
        assert_eq!(ids(&river_scope(Some(&u), &registered)), vec!["a", "b"]);
        let u = principal(AuthRole::Admin, &[]);
        assert_eq!(ids(&river_scope(Some(&u), &registered)), vec!["a", "b"]);
    }

    /// A broken registration is not in the live registry, so its marker's
    /// visibility is decided against the caller's assignment list alone —
    /// open mode and Super/Admin see every marker, a member only their own.
    #[test]
    fn broken_project_markers_follow_the_same_membership_rule() {
        assert!(may_see_broken(None, "broken"));
        let u = principal(AuthRole::Super, &[]);
        assert!(may_see_broken(Some(&u), "broken"));
        let u = principal(AuthRole::Fe, &["mine"]);
        assert!(may_see_broken(Some(&u), "mine"));
        assert!(!may_see_broken(Some(&u), "other-team"));
    }

    #[test]
    fn project_and_space_filters_intersect_visibility() {        let allowed = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(ids(&filter_projects(&allowed, Some("b"), None)), vec!["b"]);
        // A filter naming an invisible project yields an empty set — the
        // friendly empty-state payload, never another project's data.
        assert!(filter_projects(&allowed, Some("z"), None).is_empty());
        let space = vec!["c".to_owned(), "a".to_owned()];
        assert_eq!(ids(&filter_projects(&allowed, None, Some(&space))), vec!["a", "c"]);
        assert_eq!(ids(&filter_projects(&allowed, Some("a"), Some(&space))), vec!["a"]);
        assert_eq!(ids(&filter_projects(&allowed, Some("b"), Some(&space))), Vec::<String>::new());
        assert_eq!(ids(&filter_projects(&allowed, None, None)), vec!["a", "b", "c"]);
    }

    #[test]
    fn phase_matching_is_case_insensitive_and_prefix_aware() {
        assert_eq!(phase_norm(Some(" dev ")), "DEV");
        assert_eq!(phase_norm(None), "");
        let mut snap = coxagent_application::use_cases::RunnerHandle::new().snapshot();
        snap.active_role = Some("DEV-FEATURE".to_owned());
        assert!(runner_in_phase("DEV", &snap));
        assert!(runner_in_phase("dev-feature", &snap));
        assert!(!runner_in_phase("QA", &snap));
        // No filter matches everything, including an idle runner.
        assert!(runner_in_phase("", &snap));
        snap.active_role = None;
        assert!(!runner_in_phase("DEV", &snap));
        assert!(entry_in_phase("dev", "DEV-BUG"));
        assert!(entry_in_phase("SA", "SA"));
        assert!(!entry_in_phase("QA", "DEV-BUG"));
        assert!(entry_in_phase("", "anything"));
    }

    fn entry(at: &str, agent: &str, action: &str) -> ActivityEntry {
        ActivityEntry {
            at: at.to_owned(),
            agent: agent.to_owned(),
            action: action.to_owned(),
            ticket: None,
        }
    }

    fn feed() -> Vec<ActivityEntry> {
        (0..8)
            .map(|i| entry(&format!("2026-08-29T00:0{i}:00Z"), "DEV", &format!("act {i}")))
            .collect()
    }

    #[test]
    fn a_first_connection_catches_up_on_the_bounded_newest_tail() {
        let f = feed();
        assert_eq!(delta(None, &f, 20).len(), 8);
        assert_eq!(delta(None, &f, 3).len(), 3);
        assert_eq!(delta(None, &f, 3)[0].action, "act 5", "newest tail, oldest-first");
    }

    #[test]
    fn deltas_forward_only_the_rows_added_since_the_last_tick() {
        let f = feed();
        let seen = f[5].clone();
        assert_eq!(delta(Some(&seen), &f, 20).len(), 2);
        assert_eq!(delta(Some(&seen), &f, 20)[0].action, "act 6");
        assert!(delta(Some(f[7].clone()).as_ref(), &f, 20).is_empty(), "no news is empty");
    }

    #[test]
    fn a_rotated_or_rewritten_feed_resyncs_to_a_bounded_tail() {
        let mut f = feed();
        let seen = f[0].clone();
        // The 60-entry ring dropped the seen entry: the reconnecting client
        // gets recent history (bounded), not an error or an unbounded replay.
        f.drain(0..6);
        assert_eq!(delta(Some(&seen), &f, 20).len(), 2);
        // A burst larger than the cap in a single tick is bounded too.
        let seen = f[0].clone();
        assert_eq!(delta(Some(&seen), &f, 1).len(), 1);
        assert_eq!(delta(Some(&seen), &f, 1)[0].action, "act 7");
    }

    #[test]
    fn the_human_action_flag_is_derived_from_persisted_holds() {
        let mut s = ProjectState::default();
        assert!(!human_action_needed(&s));
        s.human_holds.insert(7, "needs human eyes".to_owned());
        assert!(human_action_needed(&s), "held-for-a-person work IS the flag");
    }

    fn state_with_cycles(n: usize) -> ProjectState {
        let mut s = ProjectState::default();
        for i in 0..n {
            let cs: CycleScore = serde_json::from_value(serde_json::json!({
                "cycle": (i as u64) + 1,
                "at": "2026-08-29T00:00:00Z",
                "runs": 1, "useful": 1, "cost_usd": 1.0,
                "shipped": 1, "incidents": 0, "errors": 0, "grade": "B"
            }))
            .expect("cycle score");
            s.cycle_scores.push(cs);
        }
        s
    }

    #[test]
    fn fewer_than_two_completed_cycles_is_insufficient_data() {
        assert!(insufficient_data(&state_with_cycles(0)));
        assert!(insufficient_data(&state_with_cycles(1)));
        assert!(!insufficient_data(&state_with_cycles(2)));
    }

    #[test]
    fn activity_events_carry_the_wire_shape_and_both_flags() {
        let mut s = state_with_cycles(1);
        s.human_holds.insert(7, "needs human eyes".to_owned());
        let e = entry("2026-08-29T00:00:00Z", "SA", "held a PR for human eyes");
        let v = agent_activity_payload("p1", 4, &e, human_action_needed(&s), insufficient_data(&s));
        assert_eq!(v["type"], "agent_activity");
        assert_eq!(v["project_id"], "p1");
        assert_eq!(v["seq"], 4);
        assert_eq!(v["entry"]["agent"], "SA");
        assert_eq!(v["entry"]["action"], "held a PR for human eyes");
        assert_eq!(v["needs_human"], true);
        assert_eq!(v["insufficient_data"], true);
    }
}

/// The boot/tick orchestration over a REAL `StateStorePort` double — the same
/// in-memory shape the application crate's own store tests use — so the stream
/// protocol (hello → bounded backlog → deltas, phase gating, broken markers,
/// failure recovery) is pinned without a server or a port.
#[cfg(test)]
mod fleet_river_stream_tests {
    use super::*;
    use async_trait::async_trait;
    use coxagent_application::auth::{AuthRole, AuthUser};
    use coxagent_application::PortError;
    use coxagent_application::use_cases::RunnerHandle;

    struct MemStore {
        state: std::sync::Mutex<ProjectState>,
        /// Interior-mutable because the same double is shared through `Arc`
        /// with the project under test (injected failure mode).
        fail: std::sync::atomic::AtomicBool,
    }

    impl MemStore {
        fn with(entries: Vec<ActivityEntry>) -> Arc<Self> {
            Arc::new(Self {
                state: std::sync::Mutex::new(ProjectState {
                    activity: entries,
                    ..ProjectState::default()
                }),
                fail: std::sync::atomic::AtomicBool::new(false),
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                state: std::sync::Mutex::new(ProjectState::default()),
                fail: std::sync::atomic::AtomicBool::new(true),
            })
        }

        fn push(&self, agent: &str, action: &str) {
            let mut s = self.state.lock().expect("state");
            s.log_activity(agent, action, None);
        }
    }

    #[async_trait]
    impl StateStorePort for MemStore {
        async fn load(&self) -> Result<ProjectState, PortError> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(PortError::Backend("injected".to_owned()));
            }
            Ok(self.state.lock().expect("state").clone())
        }

        async fn save(&self, _state: &ProjectState) -> Result<(), PortError> {
            Err(PortError::Backend("read-only double".to_owned()))
        }
    }

    fn entry(at: &str, agent: &str, action: &str) -> ActivityEntry {
        ActivityEntry {
            at: at.to_owned(),
            agent: agent.to_owned(),
            action: action.to_owned(),
            ticket: None,
        }
    }

    fn river_project(id: &str, store: Arc<MemStore>, runner: Arc<RunnerHandle>) -> RiverProject {
        RiverProject {
            id: id.to_owned(),
            name: id.to_owned(),
            alias: String::new(),
            store,
            runner,
        }
    }

    fn river_state(projects: Vec<RiverProject>, broken: Vec<BrokenProject>) -> RiverState {
        RiverState {
            projects,
            broken,
            seen: HashMap::new(),
            seq: HashMap::new(),
            booted: false,
        }
    }

    fn types(events: &[serde_json::Value]) -> Vec<&str> {
        events.iter().filter_map(|v| v["type"].as_str()).collect()
    }

    #[tokio::test]
    async fn boot_streams_hello_then_bounded_backlog_then_one_heartbeat() {
        let store = MemStore::with((0..3)
            .map(|i| entry(&format!("2026-08-29T00:0{i}:00Z"), "DEV", &format!("act {i}")))
            .collect());
        let mut sh = river_state(
            vec![river_project("p", store, Arc::new(RunnerHandle::new()))],
            Vec::new(),
        );
        let events = river_boot(&mut sh, "", 2, &["op".to_owned()]).await;

        assert_eq!(types(&events), vec!["hello", "agent_activity", "agent_activity", "agent_activity", "project_state"]);
        assert_eq!(events[0]["viewers"], 2, "hello carries the distinct-user count");
        assert_eq!(events[0]["online"], serde_json::json!(["op"]));
        // Backlog is oldest-first with a fresh per-project sequence.
        assert_eq!(events[1]["seq"], 1);
        assert_eq!(events[1]["entry"]["action"], "act 0");
        assert_eq!(events[3]["seq"], 3);
        // One cycle completed → the heartbeat says so explicitly (AC5).
        assert_eq!(events[4]["insufficient_data"], true);
        assert_eq!(events[4]["runner"]["mode"], "paused");
        assert!(events[4]["state"].is_object(), "state rides in lite_state_value shape");
    }

    #[tokio::test]
    async fn a_tick_with_no_news_emits_only_the_heartbeat() {
        let store = MemStore::with(vec![entry("2026-08-29T00:00:00Z", "DEV", "act 0")]);
        let shared = Arc::new(tokio::sync::Mutex::new(river_state(
            vec![river_project("p", Arc::clone(&store), Arc::new(RunnerHandle::new()))],
            Vec::new(),
        )));
        let boot = river_tick(&shared, "", 1, &[]).await;
        assert_eq!(types(&boot), vec!["hello", "agent_activity", "project_state"]);

        let tick = river_tick(&shared, "", 1, &[]).await;
        assert_eq!(types(&tick), vec!["project_state"], "no news: heartbeat only, no replay");

        store.push("DEV", "act 1");
        let tick = river_tick(&shared, "", 1, &[]).await;
        assert_eq!(types(&tick), vec!["agent_activity", "project_state"]);
        assert_eq!(tick[0]["seq"], 2, "the sequence continues the backlog's");
        assert_eq!(tick[0]["entry"]["action"], "act 1");
    }

    #[tokio::test]
    async fn the_phase_filter_gates_both_backlog_and_heartbeats() {
        let store = MemStore::with(vec![
            entry("2026-08-29T00:00:00Z", "DEV", "dev work"),
            entry("2026-08-29T00:00:01Z", "QA", "qa work"),
        ]);
        let runner = Arc::new(RunnerHandle::new());
        let shared = Arc::new(tokio::sync::Mutex::new(river_state(
            vec![river_project("p", Arc::clone(&store), Arc::clone(&runner))],
            Vec::new(),
        )));
        // The paused runner is in no phase: a QA-filtered boot carries the QA
        // backlog row but no heartbeat.
        let boot = river_tick(&shared, "qa", 1, &[]).await;
        assert_eq!(types(&boot), vec!["hello", "agent_activity"]);
        assert_eq!(boot[1]["entry"]["agent"], "QA");

        // The runner enters the QA phase: heartbeats start flowing.
        runner.resume();
        runner.set_active("QA-VERIFY", "CXC-F233");
        let tick = river_tick(&shared, "qa", 1, &[]).await;
        assert_eq!(types(&tick), vec!["project_state"]);
        assert_eq!(tick[0]["runner"]["active_role"], "QA-VERIFY");

        // A DEV row arriving now is gated out; the seen marker still advances,
        // so the row is never replayed when the filter is lifted.
        store.push("DEV", "dev work 2");
        let tick = river_tick(&shared, "qa", 1, &[]).await;
        assert_eq!(types(&tick), vec!["project_state"]);
        let tick = river_tick(&shared, "", 1, &[]).await;
        assert!(
            !types(&tick).contains(&"agent_activity"),
            "a row hidden by a lifted filter's earlier ticks must not replay"
        );
    }

    #[tokio::test]
    async fn an_empty_fleet_answers_with_the_friendly_payload_only() {
        let mut sh = river_state(Vec::new(), Vec::new());
        let events = river_boot(&mut sh, "", 1, &[]).await;
        assert_eq!(types(&events), vec!["empty"]);
        assert!(events[0]["message"].as_str().is_some_and(|m| !m.is_empty()));
    }

    #[tokio::test]
    async fn a_broken_only_fleet_shows_markers_instead_of_dropping_silently() {
        let broken = BrokenProject {
            id: "broken".to_owned(),
            config_path: std::path::PathBuf::from("/w/broken/coxagent.json"),
            error: "invalid config".to_owned(),
        };
        let mut sh = river_state(Vec::new(), vec![broken]);
        let events = river_boot(&mut sh, "", 1, &[]).await;

        assert_eq!(types(&events), vec!["hello", "project_broken"]);
        assert_eq!(events[0]["projects"][0]["broken"], true);
        assert_eq!(events[1]["project_id"], "broken");
        assert_eq!(events[1]["config_path"], "/w/broken/coxagent.json");
    }

    #[tokio::test]
    async fn a_failing_store_is_skipped_and_recovers_with_a_bounded_backlog() {
        let store = MemStore::failing();
        let shared = Arc::new(tokio::sync::Mutex::new(river_state(
            vec![river_project("p", Arc::clone(&store), Arc::new(RunnerHandle::new()))],
            Vec::new(),
        )));
        let boot = river_tick(&shared, "", 1, &[]).await;
        assert_eq!(types(&boot), vec!["hello"], "an unreadable project contributes nothing");

        // Heals: the catch-up is the bounded newest tail, not a replay-from-zero.
        store.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        for i in 0..3 {
            store.push("DEV", &format!("late {i}"));
        }
        let tick = river_tick(&shared, "", 1, &[]).await;
        assert_eq!(types(&tick), vec!["agent_activity", "agent_activity", "agent_activity", "project_state"]);
        assert_eq!(tick[0]["entry"]["action"], "late 0");
    }

    #[test]
    fn a_member_principal_cannot_reach_other_teams_projects_in_the_scope() {
        // Guards the wiring: the handler feeds river_scope the resolved
        // principal, so member-tier visibility is enforced at the endpoint.
        let u = AuthUser {
            username: "op".to_owned(),
            name: String::new(),
            email: String::new(),
            role: AuthRole::Fe,
            projects: vec!["mine".to_owned()],
        };
        let registered = vec!["mine".to_owned(), "theirs".to_owned()];
        assert_eq!(river_scope(Some(&u), &registered), vec!["mine".to_owned()]);
    }
}
