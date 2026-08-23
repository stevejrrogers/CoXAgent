FOLDER: Performance Analytics

# Agent Cycle Performance Dashboard

**Keywords:** velocity, burn rate, win rate, sprint burndown, pattern recognition, failure modes, cost by outcome type, agent cycle metrics, overview dashboard, daily spend trend

## Overview

CXA-F018 adds a cycle-performance health overlay to the project dashboard's Overview panel. It turns already-persisted state — archived sprint records (`SprintRecord`s), per-cycle scorecards (`CycleScore`s), per-ticket structured failure logs (`ticket_failures`), deploy history (`history`), and metered spend (`spend`) — into three read-outs: **velocity** (committed vs done tickets per sprint), **burn rate** (overall win rate plus a USD spend trend with anomaly warnings), and **pattern recognition** (which gates fail most often, which layers they die at — spec/gate/design/infra — and which tickets recur). It is for any operator who wants to see whether agent cycles are shipping work for their money instead of burning it on churn. Everything is pure state math over `ProjectState`, so every consumer reads identical numbers at zero extra token cost.

## How it works

All analytics live in one pure module whose functions take `&ProjectState` and return serializable summaries; none of them perform IO. The two HTTP handlers each resolve the project through auth middleware once and load state once via an injected async store port.

- **Velocity** — `compute_velocity(state)` at crates/application/src/metrics_health.rs:138 walks archived `state.sprints` plus the running `state.sprint`. For closed sprints it reads committed/done straight off each archive row; for the running sprint it counts committed tickets whose current status is Done/Documented/Verified via an inline lookup against `state.tickets`. Rows sort ascending by sprint number.
- **Burn rate** — `compute_burn(state)` at :178 computes win rate = cycles that shipped ≥1 ticket divided by all completed cycles (`state.cycle_scores.len()`); lifetime USD comes from `state.spend.total_cost_usd`. It also buckets each cycle scorecard by its calendar day into a 14-day daily-spend series.
- **Cost by outcome** — `compute_cost_by_outcome(state)` at :214 splits spend between feature work (role key contains "feature") and bug work (role key contains "bug") using role-key attribution in the spend ledger's per-role map.
- **Pattern recognition** — `compute_patterns(state)` at :247 aggregates every entry in state.ticket_failures: top-3 most-failed gates by frequency (ties broken ascending by gate id); a failure-layer distribution across Spec/Gate/Design/Infra; recurring tickets with ≥2 recorded failures ranked top-5 descending; and total structured failure volume.
- **Burn warning** — `detect_burn_warning(state)` at :301 compares each of the last up-to-5 completed cycles against their own mean; any cycle costing more than 2× that mean surfaces as a warning naming its day.
- **Trend series** — compute_trends(&State,N,&day) at :359 emits an oldest-first day-by-day series combining deploys-per-day (from history) as velocity and spend-per-day (from cycle scores) as USD. If fewer than three completed cycles exist it returns empty points with insufficient_data = true so charts render "(insufficient data)" rather than misleading lines.
- Full panel assembly is delegated to compute_cycle_perf(&State,&day) at :328; both endpoints inject today's UTC date as the first 10 chars of RFC3339 so tests are deterministic.

Two axum GET endpoints expose this surface: `/api/projects/:pid/metrics/summary` → summary payload for the Overview panel; `/api/projects/:pid/metrics/trends?days=N` → time-series for line charts. A missing project returns HTTP not-found like every other project-scoped route; a store load error maps to internal-error.

## Usage

Fetch the Overview health overlay:

```http
GET /api/projects/<pid>/metrics/summary
```

Response body keys mirror struct names exactly:

```json
{
  "velocity_by_sprint":   [ { "sprint":    1,
                              "committed": 8,
                              "done":      6 } ],
  "per_sprint_summary":   { "shipped_total":     42 },
  "burn_rate":            { "win_rate":          0.8,
                            "total_cost_usd":    31.40 },
  "daily_spend_last14d": [ { "day":"2026-08-01",
                             "cycles":4,
                             "cost_usd":         7.0 } ],
  "cost_by_outcome_type": { "feature_usd":       22.0,
                            "bug_usd":           9.4 }
}
```

The full set of summary fields appears under Interface below. Run these two requests verbatim against a hub:

```bash
curl -s 'http://127.0.0.1:<host_port>/api/projects/demo/metrics/summary'
curl -s 'http://127.0.0.1:<host_port>/api/projects/demo/metrics/trends?days=7'
```

Missing project answers with HTTP status exactly like every `/api/projects/:pid/*` route:

```text
HTTP StatusCode::NOT_FOUND     # body produced by not_found()
```

See Edge cases below for what trends returns before three completed cycles exist.

## Interface

Routes registered inside crates/presentation/src/server/mod.rs (~line 721) within serve_full(...):

| Method | Path | Handler | Produces |
|---|---|---|---|
| GET | `/api/projects/:pid/metrics/summary` | metrics_summary_ep | compute_cycle_perf output |
| GET | `/api/projects/:pid/metrics/trends?days=N` | metrics_trends_ep | compute_trends output |

Handlers live in crates/presentation/src/server/status.rs beside the existing metrics handler. Both take `State<AppState>` + `Path<String>`; the trends handler additionally reads an optional `Query` map and parses `days` as usize, defaulting to 14 when absent or unparsable. On project lookup failure they return not_found(); on store load error they return internal_error().

Public pure functions exported from coxagent_application::metrics_health (all take state, none do IO):

| Function | Returns |
|---|---|
| compute_velocity(&ProjectState) -> Vec<SprintVelocity> | closed + running sprint committed-vs-done rows |
| compute_burn(&ProjectState) -> (BurnRate, Vec<DailySpend>) | win rate + lifetime USD; 14-day daily spend |
| compute_cost_by_outcome(&ProjectState) -> CostByOutcomeType | feature vs bug USD split |
| compute_patterns(&ProjectState) -> PatternSummary | top gates / layer dist / recurring tickets / total failures |
| detect_burn_warning(&ProjectState) -> Option<BurnWarning> | >2× mean anomaly within last ≤5 cycles |
| compute_trends(&ProjectState, days, now_day) -> CycleTrendsResponse | day series or insufficient-data flag |
| compute_cycle_perf(&ProjectState, now_day) -> CyclePerfSummary | full summary payload |

Serialized payload structs and their exact fields:

- CyclePerfSummary: velocity_by_sprint Vec<SprintVelocity>, per_sprint_summary PerSprintSummary{shipped_total}, burn_rate BurnRate{win_rate f64, total_cost_usd f64}, daily_spend_last14d Vec<DailySpend{day String, cycles usize, cost_usd f64}>, cost_by_outcome_type CostByOutcomeType{feature_usd f64, bug_usd f64}, patterns PatternSummary, deploy_7d usize, burn_warning Option<BurnWarning>.
- SprintVelocity: { sprint u32, committed usize, done usize }.
- PatternSummary: most_failed_gates_top3 Vec<GateCount{gate String, count usize}>, failure_layer_dist BTreeMap<String, usize> (layer label -> count), recurring_tickets_top5 Vec<TicketRecurrence{id String, fail_count u32}>, total_failures usize.
- BurnWarning: { day String ("YYYY-MM-DD"), cost_usd f64, avg_cost_usd f64 }.
- CycleTrendsResponse: { insufficient_data bool, points Vec<TrendPoint{day String "YYYY-MM-DD", velocity f64 (deploys that day), spend_usd f64}> }.

A single visibility change shipped alongside: days_back was widened to pub(crate) in crates/application/src/metrics.rs so the trends math reuses the same exact civil-calendar arithmetic instead of duplicating it.

## Configuration

CXA-F018 introduces no new config keys, CLI flags, or environment variables. It is deliberately passive: it reads what other subsystems already persist (`cycle_scores`, `ticket_failures`, `sprints`/`SprintRecord`, `history`, `spend`) and never writes any of them. The only behavioural knob is request-time state on the trends route — the query parameter `days` (usize), which bounds how many trailing calendar days the series spans; absent or unparsable values fall back to 14.

The two endpoints are registered unconditionally in serve_full(...) for any project whose store can serve a ProjectState.

## Edge cases and limits

- **Front-end not wired yet.** The commit adds an inert placeholder container `<div id="ov-cycle-perf"></div>` in crates/presentation/src/web/index.html but ships no JavaScript renderer that fetches `/metrics/summary` into it. The analytics are fully available via the API; the Overview panel itself does not display them until that JS lands.
- **Insufficient data** — compute_trends returns empty points with insufficient_data=true whenever fewer than three completed cycles exist, so callers must render "(insufficient data)" rather than a fabricated chart. It never invents zero-filled history.
- **Burn warning needs ≥2 cycles** — detect_burn_warning returns None until at least two completed cycle scorecards exist; with a non-positive window average it also stays quiet (no division by zero).
- **Determinism is date-injected** — "last 14 days", "last 7 days", and day bucketing all depend on the now_day passed by the handler (today's UTC date), which is what keeps tests deterministic. Time-of-day of a record does not matter, only its calendar day.
- **Missing project → HTTP 404**, matching every other project-scoped route; a store load error → internal-error response.
- **Cost attribution is heuristic** — cost-by-outcome splits purely on role-key substrings ("feature"/"bug"); roles with neither keyword are excluded from both buckets, so feature_usd + bug_usd may be less than total spend.
- Nothing here writes state or calls an engine; these endpoints never consume tokens or mutate the store.

## Code map

- crates/application/src/metrics_health.rs — the entire analytics module (new in CXA-F018): compute_cycle_perf / compute_velocity / compute_burn / compute_cost_by_outcome / compute_patterns / detect_burn_warning / compute_trends, all payload structs, and inline unit tests. ~600 lines.
- crates/presentation/src/server/mod.rs — route registration for `/api/projects/:pid/metrics/summary` and `/api/projects/:pid/metrics/trends` inside serve_full(...) (~line 721).
- crates/presentation/src/server/status.rs — handlers metrics_summary_ep and metrics_trends_ep (auth surface shared with metricsEP).
- crates/app/tests/agent_cycle_metrics_dashboard.rs — endpoint contract test: boot a hub on an in-memory store, assert summary shape + burn_warning null + pattern total_failures, trends 404 on missing project, and insufficient_data edge.
- crates/application/src/lib.rs — `pub mod metrics_health;` export (re-exported as coxagent_application::metrics_health so handlers can call it).
- crates/presentation/src/web/index.html — added `<div id="ov-cycle-perf"></div>` Overview placeholder (no renderer yet; see Edge cases).

Data-source types this feature reads (pre-existing, not part of F018):
- crates/application/src/state/work.rs — CycleScore, SprintRecord, AttemptFailure, FailureLayer.
- crates/application/src/state/mod.rs — ProjectState fields cycle_scores (:134), ticket_failures (:293), sprints/history/spend.

## Related

- CXA-F018 lives on the `feat/CXA-F018` branch (commit 4efbccf) and was not yet merged into main at write time. The existing `/api/projects/:pid/metrics` (metricsEP) and `/api/projects/:pid/agent-evals` (agent_evals_ep) endpoints in crates/presentation/src/server are the sibling surfaces this overlay extends; the core compute() / agent_evals() analytics live in crates/application/src/metrics.rs.
- The cycle-scorecard grading that feeds burn rate and trends is produced by CycleScore::grade_of and recorded each cycle by the scrum wiring (crates/application/src/sprint.rs and use_cases/cycle/scrum.rs).
- Failure-mode data consumed here is written by the attempt-failure recording in crates/application/src/faults.rs and stored per ticket as AttemptFailure / FailureLayer entries in state.
- Sprint velocity history is archived at rollover / close by sprint::roll_over in crates/application/src/sprint.rs, whose docs note that a single archive shape keeps the velocity chart comparable across timed rollovers and early closes.

