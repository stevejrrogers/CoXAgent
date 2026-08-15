FOLDER: Analytics
# Agent Cycle Performance Dashboard

**Keywords:** velocity, burndown, burn rate, throughput, sprint capacity, agent evals, cost per ship, retry churn per ship, pattern recognition, self-tuning

## Overview

The Agent Cycle Performance Dashboard turns persisted project state into live team-health telemetry for anyone watching the web UI or reading team chat. It reports what shipped vs committed (velocity), how fast work is draining and at what dollar cost (burn rate), and surfaces recurring patterns: high retry churn flips DEV to bugs-first; a backlog outgrowing throughput pauses BA intake; ritual/duplicate tickets collapse into one slot. It is read-only — it derives numbers from state and never mutates it; tuning decisions are applied elsewhere by the cycle orchestrator.

## How it works

All metrics are pure functions over state, so the dashboard, reports and SM retro read identical numbers. The two computation entry points live in `crates/application/src/metrics.rs`:

- `metrics::compute(state)` returns a `Metrics`: status distribution (`by_status`), `features_shipped`, `features_in_flight`, `bugs_open`, `bugs_verified`, `releases` (length of deploy history), `deploys_by_day: Vec<DayCount>` (a burndown source) and `feature_ratio_pct`. A history record counts as a feature via private `is_feature_id()` — first char of the last dash-segment (`F...` / `C...`) of its ticket id.
- `metrics::agent_evals(state)` returns an `AgentEvals`: per-role runs/average cost (`RoleEval { role, runs }`, where per-run average = metered_cost_by_role / runs_by_role — both measure the same window); then rolled-up figures: shipped_total, shipped_7d (computed with private civil-calendar helper days_back()), parked (tickets with >=3 failed attempts in state.ticket_fail_attempts), retry churn per ship (= failed attempts / shipped), PRs stuck in the fix ladder (state.pr_fix_attempts >= 2) and cost per ship.

Velocity comes from the sprint archive written by roll_over() in crates/application/src/sprint.rs: each closed sprint pushes a SprintRecord onto state.sprints. Next-sprint capacity is inferred from that history by private sprint_capacity(): average of the last 5 done-counts plus half-stretch against a floor of 3; a brand-new project with no history commits twice that floor (6). Live progress uses done_count(). Sprint boundaries are written only through this single path — timed rollover via advance() or an early close via close_now() — so every archived record has an identical shape.

Pattern recognition is three mechanisms:

1. Self-tuning brakes - decide_tuning(evals,&backlog,&current) returns a new Tuning using hysteresis: churn-per-ship >1.5 -> bugs-first on / <0.8 off; backlog >25 -> skip-BA on / <12 off.
2. Ritual / duplicate detection - is_backlog_meta(title) flags ceremony titles ("burndown", "triage", "stabiliz", "grooming") combined with backlog context ("bug"/"backlog"/"sprint") so only one ritual ticket stays open; duplicates_existing(title,&existing) dedupes near-paraphrases via Jaccard >=0.6.
3. UI duplicate warning - classic scripts flag actionable tickets sharing a normalised title.

Serving happens over HTTP in the presentation crate; rendering lives in classic scripts under web/js home/core/shell drawing KPIs + charts + evals panel respectively.

## Usage

Open any project's web UI home tab to see KPIs plus:

* Sprint card showing Committed / In progress / Shipped / Remaining / Cycles left / Velocity avg + an SVG burndown (renderSprintPanel, home.js).
* Velocity bar chart comparing shipped cyan vs committed grey across last 10 closed sprints (velocityHtml(sprints), core.js).
* Throughput sparkline covering last 14 days shipped/day + ticket-status stacked bar (chartsHtml(s), core.js).
* Agents evals panel showing Shipped total · Shipped 7d · Cost/ship · Retry churn/ship · Parked · PR stuck counts + per-role avg cost when they exist (loadAgentEvals(), shell.js).

Post an on-demand digest into team chat:

```bash
curl -X POST -H "Authorization: Bearer $TOKEN" \
  http://localhost:<host_port>/api/projects/<pid>/digest
# => {"ok":true,"digest":"Daily digest ..."}
```

Example metrics payload:

```json
GET http://localhost:<host_port>/api/projects/<pid>/metrics
{
  "version": "0.x.y",
  "total_tickets": n,
  "by_status": {"pending":..,"in_progress":..,"done":..},
  "features_shipped": ..,
  "features_in_flight": ..,
  "bugs_open": ..,
  "bugs_verified": ..,
  "releases": ..
}
```

Example evals payload:

```json
GET http://localhost:<host_port>/api/projects/<pid>/agent-evals
{ ... }
```

## Interface

HTTP endpoints registered in `crates/presentation/src/server/mod.rs`, scoped under `/api/projects/:pid/*`. All pass through the router-level auth middleware `auth_mw` (crates/presentation/src/server/auth.rs), which is a pass-through when no auth is configured and otherwise gates routes:

| Method | Path | Handler | Returns |
|--------|------|---------|---------|
| GET | `/api/projects/:pid/metrics` | status::metrics_ep | JSON Metrics |
| GET | `/api/projects/:pid/agent-evals` | engines::agent_evals_ep | JSON AgentEvals |
| POST | `/api/projects/:pid/digest` | projects::digest_ep | JSON {ok, digest} |

Handlers call the pure functions directly: metrics_ep -> metrics::compute(&state) (status.rs), agent_evals_ep -> metrics::agent_evals(&state) (engines.rs), digest_ep builds via metrics::digest_markdown(&state, &now) and posts into AGENTS_CHANNEL chat (projects.rs).

State fields feeding these computations (all read-only inputs):

- sprints (`Vec<SprintRecord>`) + live Sprint -> velocity / burndown / capacity inputs.
- history (`Vec<DeployRecord>`) -> releases count + deploys-by-day buckets + feature ratio.
- spend { total_cost_usd, runs, by_role, runs_by_role, metered_cost_by_role } -> cost-per-ship & avg-cost-per-run & budget alerts.
- ticket_fail_attempts -> parked count & failed attempts & churn.
- pr_fix_attempts -> PR-stuck metric.

## Configuration

Behaviour-shaping settings come from workflow config (coxagent.json) under WorkflowConfig (crates/application/src/config.rs); there are no separate dashboard-config knobs — every KPI changes purely with state data plus these flags shaping what data exists:

| Field | Default | Role for this dashboard |
|-------|---------|-------------------------|
| `mode = kanban \ scrum` | kanban | Kanban leaves sprint metrics empty (no sprints); scrum enables velocity/burndown |
| `budget_usd : Option<f64>` | None ($USD cap; loop pauses at cap) | Drives spend/budget alerts only — does not change metric math |
| `sprint_unit = days \ cycles` | days (`SprintUnit::Days`) | What a sprint window is measured in; resolved by SprintPolicy::from_config() in sprint.rs |
| `sprint_length_days : u64` | 1 (`default_sprint_days()`) | Wall-clock sprint length when unit is days |
| `sprint_length_cycles : u64` | 10 (`default_sprint_len()`) | Sprint length when unit is cycles |

Budget alert thresholds are hard-coded client-side in core.js alertsHtml: red "Budget cap reached ... loop paused" at spend >= budget (core.js:137), amber "Budget nearly reached" at spend >= 80% of budget (core.js:138). The hub manage view colors spend the same way (manage.js:54). These are not server-configurable beyond setting the budget itself.

## Edge cases and limits

Verified behaviour:

- Empty projects compute all-zero metrics (test empty_state_is_all_zeroes in metrics.rs).
- Feature ratio avoids division-by-zero via checked_div() -> unwrap_or(0).
- When nothing has shipped, cost-per-ship falls back to 0.0 and churn-per-ship to the raw failed-attempt count so no NaN/infinity surfaces.
- days_back() handles month/year wrap exactly using civil-calendar math.
- Burndown renders before any sprint has closed because it is cycle-based (home.js renderSprintPanel).
- A team that shipped nothing still commits to a small capacity floor so velocity never reads as unrecoverable zero (capacity_tests in sprint.rs).

Deliberately NOT done by this feature: it never mutates state — tuning decisions it produces are applied elsewhere by the cycle orchestrator; it does not persist its own history separate from state; pattern detection stops at self-tuning + duplicate/ritual flags and does not emit long-range trend forecasts.

## Code map

These paths implement or directly feed this dashboard:

- `crates/application/src/metrics.rs` — the whole computation core: `Metrics`, `AgentEvals`, `RoleEval`, `DayCount` types and the pure functions compute(), agent_evals(), decide_tuning(), days_back(), digest_markdown(); includes unit tests empty_state_is_all_zeroes, agent_evals_math, tuning_hysteresis.
- `crates/application/src/sprint.rs` — sprint lifecycle that feeds velocity/burndown: advance(), roll_over() (writes each closed sprint's outcome), close_now(), commit_ticket/uncommit_ticket, done_count(), private sprint_capacity(); resolves window config through SprintPolicy::from_config().
- `crates/application/src/state/work.rs` — state shapes behind the numbers: Sprint (live sprint), SprintRecord { number, goal, committed, done } (velocity history), Tuning, and Spend maps (by_role, runs_by_role, metered_cost_by_role) used by evals.
- `crates/application/src/config.rs` — workflow config knobs (`mode`, budget_usd, sprint_unit/length) under WorkflowConfig; defaults in Configuration above.
- `crates/application/src/parsing.rs` — pattern detection helpers is_backlog_meta() (ritual titles) and duplicates_existing() (Jaccard >=0.6 dedupe).
- `crates/presentation/src/server/mod.rs` — route registration: metrics at L723, agent-evals at L724, digest at L734.
- `crates/presentation/src/server/auth.rs` — auth_mw router-level gate that guards these routes when auth is configured.
- `crates/presentation/src/server/status.rs` — metrics_ep handler -> metrics::compute(&state).
- `crates/presentation/src/server/engines.rs` — agent_evals_ep handler -> metrics::agent_evals(&state).
- `crates/presentation/src/server/projects.rs` — digest_ep handler -> builds via metrics::digest_markdown() and posts into AGENTS_CHANNEL chat.
- Web rendering (classic scripts): home.js renderSprintPanel draws the sprint card + burndown + velocity (L186); core.js velocityHtml renders shipped-vs-committed bars (L145), chartsHtml draws throughput sparkline + status bar (L157), alertsHtml shows budget alerts with thresholds at L137/L138; shell.js loadAgentEvals renders the Agents evals panel from /agent-evals (L1163).

## Related

- **CXA-F012 / Incident postmortem loop** — sibling Product page; both are read-only telemetry over the same ProjectState and share the serving path in server/mod.rs plus deploy history and chat-notify plumbing.
- **Sprint lifecycle** (`crates/application/src/sprint.rs`) — this dashboard's velocity/burndown numbers come entirely from sprint state written by roll_over(); capacity inference lives alongside it. Scrum-mode config turns velocity on vs kanban off.
- **Cycle self-tuning orchestration** (`crates/application/src/use_cases/cycle/mod.rs` and ceremonies.rs) — consumes decide_tuning() results to write back a new Tuning; this dashboard only computes decisions, ceremonies applies them. It is also what names CXA-F018's "cycle" angle end to end.
