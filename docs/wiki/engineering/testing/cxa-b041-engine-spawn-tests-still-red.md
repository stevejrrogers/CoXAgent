FOLDER: Testing

# Engine spawn tests still red (CXA-B037 not integrated)

**Keywords:** engine, claude, opencode, AgentRequest, timeout, from_secs, spawn, cargo test, red suite, integration

## Overview

CXA-B041 tracks that CXA-B037 — a fix to two engine-spawn unit tests that time out under CI load — "shipped but not integrated": the full workspace test suite is still red. This page records what actually shipped versus what is on `main`, and why raising the tests' hard-coded wall-clock budget did not turn CI green. It is for any agent or engineer who sees `cargo test` fail inside `claude.rs` or `opencode.rs`, or who touches either engine adapter's spawn path.

The decisive fact: **neither CXA-B037 nor CXA-B041 ever landed on `main`**. Plain `main` still passes a 10s timeout to the claude e2e test and a 20s timeout to the opencode one (`git show main:crates/infrastructure/src/engine/claude.rs` → `from_secs(10)`; likewise opencode.rs → `from_secs(20)`). Any CI run against the base branch therefore uses the original tight budgets and can time out under load by construction — that alone keeps "the full test suite red".

## How it works

Both affected tests drive a real child through the production spawn path rather than mocking it:

- `ClaudeEngine::run()` (claude.rs:161) builds a command via `crate::proc::agent_command()`, appends `-p`, then writes an MCP config file and points at it with `--mcp-config`. The fake binary records its own argv.
- `OpencodeEngine::run()` (opencode.rs:255) calls `ensure_opencode_mcp_config()` then sets env vars; its fake binary echoes back `${OPENCODE_CONFIG}`.
- Both funnel into their private per-engine helper whose stdout read is wrapped in a tokio deadline:
  - claude.rs wraps streaming in `tokio::time::timeout(request.timeout, read)` (~line 320).
  - opencode.rs uses its private `.exec(cmd, live, request.timeout, sandbox)` with an identical wrap.
  That budget comes straight from field `.timeout` of struct literal typed coxagent_application::ports::outbound::{AgentRequest}, unchanged into each spawn.

What each "fix" actually changed:

| Commit | Branch | Change |
|--------|--------|--------|
| `/94fea51` (CXA-B037 tip) | feat/CXA-B037 | claude 10s -> 120s; opencode 20s -> 120s |
| `/099bca7` (CXA-B041 tip) | feat/CXA-B041 | identical hunks re-applied; comments relabelled |

Neither commit touched production code — only two constants inside two unit tests plus comments. Because both exist only on feature branches (`git branch --contains /099bca7`: feat/CXA-B039/B041/B042/B043/B044/F012/F021), any checkout based on plain main exercises budgets of **10s / 20s**, never 120s.

Local reproduction note for this machine (HEAD = feat/CXA-B043 already carries `/099bca7`): both tests pass in isolation (~0.5 s), together (~0.9 s), under four concurrent copies (< seconds), and in the whole coxagent-infrastructure lib suite (~8 s). So they no longer fail here once HEAD carries B041's hunks; they only stay red against any base branch lacking them.

## Usage

Run just these two directly:

```bash
# Claude adapter e2e plumbing:
cargo test -p coxagent-infrastructure run_passes_mcp_config_flag_to_the_real_spawn

# OpenCode adapter e2e config/env plumbing:
cargo test -p coxagent-infrastructure run_writes_cox_config_and_exports_opencode_config_env
```

To see what CI/base sees when it does NOT carry B041's hunks:

```bash
git show main:crates/infrastructure/src/engine/claude.rs   # timeout stays from_secs(10)
git show main:crates/infrastructure/src/engine/opencode.rs # timeout stays from_secs(20)
```

To confirm whether your checkout already integrates B041:

```bash
grep -rn "Generous on purpose" crates/infrastructure/src/engine/
```

Both sites present -> your tree carries `/099bca7`. Absent -> you are running pre-integration behavior even if your source tree otherwise looks modern.

Assertions each makes once running:

```
claude : outcome.succeeded();
        argv contains "--mcp-config" and project id "cxc";
        argv does NOT contain "tok" nor "Bearer"; mcpServers.coxagent.url + Authorization set.
opencode : outcome.succeeded(); COX_OPENCODE_CONFIG file exists before spawn;
          child saw OPENCODE_CONFIG pointing at that file's path.
```

## Interface

No public API changed by B037 or B041; only constants inside two unit tests plus comments were touched.

Affected tests:
- crates/infrastructure/src/engine/claude.rs:704 — `run_passes_mcp_config_flag_to_the_real_spawn` (`#[tokio::test]`)
- crates/infrastructure/src/engine/opencode.rs:937 — `run_writes_cox_config_and_exports_opencode_config_env` (`#[cfg(unix)] #[tokio::test]`)

Supporting production symbols invoked by these paths:
- crates/infrastructure/src/proc.rs:271 — `agent_command`
- crates/infrastructure/src/proc.rs:369 — `spawn_confined`
- crate::engine::apply_shim_path; private per-engine exec helpers wrapping tokio deadline reads

Budget literals at each call site today (HEAD with B041):

```
claude   : .timeout = std::time::Duration::from_secs(120)   // was from_secs(10)
opencode : .timeout = std::time::Duration::from_secs(120)   // was from_secs(20)
```

## Configuration

No runtime configuration controls these budgets; they are hard-coded literals inside each test assigned to field `.timeout` of the `AgentRequest` struct. There is no global "test timeout" knob anywhere in either crate's config surface.

Baseline vs integrated value matrix:

| Site | On plain main today | After B03X/B04X hunks land |
|------|---------------------|----------------------------|
| claude e2e `.timeout` (claude.rs ~756-760) | from_secs(10) | from_secs(120) |
| opencode e2e `.timeout` (opencode.rs ~966-973) | from_secs(20) | from_secs(120) |

The Seatbelt retry layer adds no constant here because both tests build engines without `.with_sandbox(true)`; sandbox confinement is off for these runs, so no Seatbelt apply-retry budget applies.

## Edge cases and limits

The core trap this ticket names: **raising a deadline cannot fix a hang.** If a test times out deterministically rather than merely slow-under-load, then whatever blocks it will eventually exceed any budget — bumping 10 -> 120 seconds only moves when the failure message appears. Classify before widening again:

- Slow-under-load class (what B037/B041 assumed): raises help; assert-plumbing-only latency is irrelevant to correctness.
- Deterministic-hang class (what the ticket title implies): no wall-clock budget helps; find and fix the actual blocker instead.

Deliberately out of scope for B037/B041: reducing spawn cost or limiting CI concurrency — both only widen deadlines.

Both tests are unix-only (`#[cfg(unix)]`) because they exec real shells; skipped on Windows.

Platform gate caution: these exec real children through `proc::agent_command` + `spawn_confined`. Any change to that shared process layer can re-introduce timing sensitivity here — run both tests after editing proc.rs or either engine adapter.

## Code map

- crates/infrastructure/src/engine/claude.rs — Claude adapter; private exec helper with tokio deadline (~320); e2e plumbing assert at ~704-793
- crates/infrastructure/src/engine/opencode.rs — OpenCode adapter; same exec pattern; e2e config/env assert at ~937-994
- crates/infrastructure/src/proc.rs — shared process layer holding agent_command (271), spawn_confined (369)
- crates/application/src/ports/outbound/engine.rs — trait AgentEnginePort + AgentRequest.timeout consumed by both engines

## Related

Other pages in this Wiki space:
- docs/wiki/engineering/testing/cxa-b037-tests-failing.md — records CXA-B037 itself as if fixed; this page corrects that record: the hunks never landed on main
- docs/wiki/engineering/deployment/* , configuration/* , release-process/* — sibling bug-fix pages

Verified commits:
```
94fea51  fix(CXA-B037): Tests failing: cargo tests failed        feat/CXA-B037 tip
099bca7  fix(CXA-B041): CXA-B037 shipped but not integrated...    re-applies identical hunks;
         on main today neither commit is present                   relabels comments to CXA-A/B041
```

If you touch either engine adapter's spawn path, also run the hex IO gate so nothing reintroduces direct std::process/std::fs where a port should be used: `cargo test -p coxagent-app hexagonal_gate`.
