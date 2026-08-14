FOLDER: Analytics
# Agent Cycle Performance Dashboard

**Keywords:** velocity, burndown, burn rate, throughput, sprint capacity, agent evals, cost per ship, churn per ship, pattern recognition, self-tuning

## Overview

The Agent Cycle Performance Dashboard turns persisted project state into live team-health telemetry for anyone watching the web UI or reading team chat. It reports what shipped vs committed (**velocity**), how fast work is draining and at what dollar cost (**burn rate**), and surfaces recurring **patterns**: high retry churn flips DEV to bugs-first; a backlog outgrowing throughput pauses BA intake; ritual/duplicate tickets collapse into one slot. It is read-only — it derives numbers from state and never mutates it.

## How it works

All metrics are **pure functions over state**, so the dashboard, reports and SM retro read identical numbers. The two computation entry points live in `crates/application/src/metrics.rs`:

- [`metrics::compute(state)`](crates/application/src/metrics.rs) → [`Metrics`](crates/application/src/metrics.rs): status distribution (`by_status`), `features_shipped`, `features_in_flight`, `bugs_open`, `bugs_verified`, `releases` (= length of deploy history), `deploys_by_day: Vec<DayCount>` (a burndown source) and `feature_ratio_pct` (via [`is_feature_id()`](crates/application/src/metrics.rs)).
- [`metrics::agent_evals(state)`](crates/application/src/metrics.rs) → [`AgentEvals`](crates/application/src/metrics.rs): per-role runs/average cost ([`RoleEval`](crates/application/src/metrics.rs)), `shipped_total`, `shipped_7d` (computed with [`days_back()`](crates/application/src/metrics.rs)), `parked` (tickets with ≥3 failed attempts in `state.ticket_fail_attempts`), retry **churn per ship**, PRs stuck in the fix ladder (`state.pr_fix_attempts ≥ 2`) and **cost per ship**.

Velocity comes from the sprint archive written by [`roll_over()`](crates/application/src/sprint.rs): each closed sprint pushes a [`SprintRecord { number, goal, committed, done }`](crates/application/src/state/work.rs) onto `state.sprints`. Next-sprint capacity is inferred from that history by [`sprint_capacity()`](crates/application/src/sprint.rs) — average of the last 5 done-counts plus half-stretch (floor 3). Live burndown uses [`done_count()`](crates/application/src/sprint.rs).

Pattern recognition is three mechanisms:
1. **Self-tuning brakes** —[`metrics::decide_tuning(evals,&backlog,&current)`](crates/application/src/metrics.rs) returns a new `Tuning` using hysteresis: churn-per-ship >1.5 → bugs-first on / <0.8 off; backlog >25 → skip-BA on / <12 off.
2. **Ritual / duplicate detection** — [`is_backlog_meta(title)`](crates/application/src/parsing.rs) flags ceremony titles ("burndown", "triage", "stabilize") so only one ritual ticket stays open; `duplicates_existing(title, &existing)` (same file, line ~254) dedupes near-paraphrases via Jaccard ≥0.6.
3. **UI duplicate warning** — shell.js flags actionable tickets sharing a normalised title.

Serving happens over HTTP in the presentation crate; rendering lives in classic scripts under `web/js/*`: home.js draws the sprint card + burndown + velocity; core.js draws charts; shell.js draws the Agents evals panel.

## Usage

Open any project's web UI home tab to see KPIs plus:
- Sprint card: Committed / In progress / Shipped / Remaining / Cycles left / Velocity avg + an SVG burndown (`renderSprintPanel`, home.js).
- Velocity bar chart (shipped cyan vs committed grey per closed sprint, last 10) — [`velocityHtml(sprints)`].
- Throughput sparkline (last 14 days shipped/day) + ticket-status stacked bar — [`chartsHtml(s)`].
The Agents panel shows shipped total · shipped 7d · cost/ship · retry churn/ship · parked · PRs stuck ([shell.js]).

Post an on-demand digest into team chat:

```bash
curl -X POST -H "Authorization: Bearer $TOKEN" \
  http://localhost:<host_port>/api/projects/<pid>/digest
# => {"ok":true,"digest":"**Daily digest** … Shipped (24h): N … Sprint #k: d/c … Spend to date: $x.yz"}
```

Example metrics payload:

```json
GET /api/projects/<pid>/metrics
{
  "version": "0.4.0",
  "total_tickets": 41,
  "by_status": {"pending":12,"in_progress":3,"done":9},
  "features_shipped":8,
  "features_in_flight":2,
  "bugs_open":4,
  "bugs_verified":1,
  "releases":11,
  "deploys_by_day":[{"day":"2026-08-13","count":2}],
  "feature_ratio_pct":72
}
```

Example evals payload:

```json
GET /api/projects/<pid>/agent-evals
{
  "per_role":[{"role":"dev_feature","runs":14,"cost_usd":6.2,"avg_cost_usd":0.44}],
  "shipped_total":11,"shipped_7d":3,"parked":1,"failed_attempts":4,
  "churn_per_ship":0.36,"prs_stuck":0,"cost_per_ship_usd":1.83
}
```

## Interface

HTTP endpoints registered in [`crates/presentation/src/server/mod.rs`](crates/presentation/src/server/mod.rs), all scoped `/api/projects/:pid`, auth required:

| Method | Path | Handler | Returns |
|--------|------|---------|---------|
| GET | `/metrics` | [`status::metrics_ep`](crates/presentation/src/server/status.rs) | JSON [`Metrics`](crates/application/src/metrics.rs) |
| GET | `/agent-evals` | [`engines::agent_evals_ep`](crates/presentation/src/server/engines.rs) | JSON [`AgentEvals`](crates/application/src/metrics.rs) |
| POST | `/digest` | [`projects::digest_ep`](crates/presentation/src/server/projects.rs) | JSON with rendered digest text |

Exact type shapes ([metrics.rs](crates/application/src/metrics.rs)):

```rust
pub struct Metrics {
    pub version: String,
    pub total_tickets: usize,
    pub by_status: BTreeMap<String, usize>,
    pub features_shipped: usize,
    pub features_in_flight: usize,
    pub bugs_open: usize,
    pub bugs_verified: usize,
    pub releases: usize,
    pub deploys_by_day: Vec<DayCount>,      // DayCount { day:String, count:usize }
    pub feature_ratio_pct: u32,
}

pub struct AgentEvals {
    pub per_role: Vec<RoleEval>,            // RoleEval { role:String, runs:u64, cost_usd:f64, avg_cost_usd:f64 }
    pub shipped_total: usize,
    pub shipped_7d: usize,
    pub parked: usize,
    pub failed_attempts: u64,
    pub churn_per_ship: f64,                // failed attempts per shipped ticket
    pub prs_stuck: usize,                   // PRs with 2+ fix rounds
    pub cost_per_ship_usd: f64,
}
```

State fields feeding these computations:
- sprints (`Vec<SprintRecord>`) + live Sprint → velocity/burndown/capacity inputs.
- history (`Vec<DeployRecord>`) → releases count + deploys-by-day buckets + feature ratio.
- spend {total_cost_usd,runs,metered_cost_by_role,runs_by_role} → cost-per-ship & avg-cost-per-run & budget alerts.
- ticket_fail_attempts → parked count & failed attempts & churn.
- pr_fix_attempts → PR-stuck metric.

## Configuration

Behaviour-shaping settings come from workflow config (`coxagent.json`) under `WorkflowConfig` ([config.rs](crates/application/src/config.rs)); there are no separate dashboard-config knobs — every KPI changes purely with state data plus these flags shaping what data exists:

| Field | Default | Role for this dashboard |
|-------|---------|-------------------------|
| `mode = kanban \ scrum` | kanban | Kanban leaves sprint metrics empty (no sprints); scrum enables velocity/burndown |
| `budget_usd : Option<f64>` | None ($USD cap; loop pauses at cap) | Drives spend/budget alerts only — does not change metric math |
| `sprint_unit = days \ cycles` | days | What a sprint window is measured in; resolved by [`SprintPolicy::from_config()`](crates/application/src/sprint.rs) |
| `sprint_length_days : u64` | 1 | Wall-clock sprint length when unit is days |
| `sprint_length_cycles : u64` | 10 | Sprint length when unit is cycles |

Budget alert thresholds are hard-coded client-side in shell.js's cost view (`window._budget` vs spend): red "Budget cap reached / loop paused" at ≥cap, amber warning at ≥80% of cap. These are not server-configurable beyond setting the budget itself.

## Edge cases and limits

Verified behaviour:
- Empty projects compute all-zero metrics (see test [`empty_state_is_all_zeroes`](crates/application/src/metrics.rs)).
- Feature ratio avoids division-by-zero via [`checked_div()`]→unwrap_or(0).
- When nothing has shipped, both churn-per-ship and cost-per-ship fall back to non-division values so no NaN/infinity surfaces.
- [`days_back()`](crates/application/src/metrics.rs) handles month/year wrap exactly using civil-calendar math.
- Burndown renders before any sprint has closed because it is cycle-based ([home.js]).
- A team that shipped nothing still commits to a small capacity floor so velocity never reads as unrecoverable zero ([capacity tests](crates/application/src/sprint.rs)).

Deliberately NOT done by this feature: it never mutates state — tuning decisions it produces are applied elsewhere by the cycle orchestrator; it does not persist its own history separate from state; pattern detection stops at self-tuning + duplicate/ritual flags and does not emit long-range trend forecasts.

This page states only what was verified against source during this pass; no failure modes beyond those listed above are asserted without further review of code paths outside these files.

## Code map

These paths implement or directly feed this dashboard:

- crates/application/src/metrics.rs — pure computations: Metrics, AgentEvals, RoleEval, DayCount types; compute(), agent_evals(), decide_tuning(), days_back(), digest_markdown().
- crates/presentation//src/server/mod.rs#L723-L734 registers routes
   *(path below)*
   routes /metrics (/api/projects/:pid/metrics), /agent-evals, /digest.
   Actual registration lines: mod.rs → `.route("/api/projects/:pid/metrics", …)`, `/agent-evals`, `/digest`.
   Canonical path: crates/presentation/src/server/mod.rs (routes at lines 723–734).
   *(noted twice intentionally? no—single canonical entry)*

