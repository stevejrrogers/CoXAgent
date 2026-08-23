FOLDER: Testing

# Failing cargo tests (engine spawn timeouts under CI load)

**Keywords:** cargo test, failing test, engine, claude, opencode, AgentRequest, timeout, flaky test, CI, mcp-config

## Overview

CXA-B037 fixed two unit tests that failed deterministically when the full
cargo workspace suite ran on a loaded CI machine. Both tests spawn a real child
process through the production agent-spawn path and assert argv/env plumbing —
they are not latency-sensitive — but were given tight wall-clock timeouts a busy
runner could outlast. The fix raised those timeouts to 120s so each assertion is
about *what* gets plumbed to the child, never *how fast* it completes.

It is for anyone who sees `cargo test` fail inside `claude.rs` or `opencode.rs`,
and for agents editing either engine adapter without regressing its e2e test.

## How it works

The two affected tests exercise `AgentEnginePort::run()` end-to-end instead of
mocking the child binary:

- `ClaudeEngine::run` (crates/infrastructure/src/engine/claude.rs:161) builds a
  real command via `crate::proc::agent_command`, then appends `-p`,
  `--append-system-prompt`, and routes an MCP server through a temp file pointed to by
  `--mcp-config`. The fake binary is a shell script that records its own argv.
- The opencode counterpart (opencode.rs:937) writes an MCP/OPENCODE config file before
  spawning and has the fake binary echo the resolved env var back as a text event.
- Both engines funnel into their private `exec(...)` helper that wraps stdout streaming in
  `tokio::time::timeout(timeout, read)` — claude.rs:320. That wall-clock budget comes from
  `AgentRequest.timeout`, which flows unchanged from the caller into each engine's spawn.

The original bug: each test passed a small hard-coded timeout (`10s` claude / `20s`
opencode). Under heavy CI load even a one-line shell echo can exceed that budget,
so the run was killed by its own deadline before argv/env could be asserted —
hence "Tests failing: cargo tests failed".

The fix commits `/94fea51` (feat/CXA-B037 tip) changed both to
`std::time::Duration::from_secs(120)` with an inline comment explaining why latency is irrelevant.
During integration on top of main, `/099bca7` ("CXA-B037 shipped but not integrated…")
re-applied identical hunks and relabeled the in-code comments CXA-B041; both files today carry
that label at exactly these two sites.

## Usage

Run just these two tests directly (they exec real children on unix):

```bash
# Claude adapter e2e plumbing:
cargo test -p coxagent-infrastructure run_passes_mcp_config_flag_to_the_real_spawn

# OpenCode adapter e2e env/config plumbing:
cargo test -p coxagent-infrastructure run_writes_cox_config_and_exports_opencode_config_env

# Both together with output:
cargo test -p coxagent-infrastructure -- --nocapture \
  run_passes_mcp_config_flag_to_the_real_spawn \
  run_writes_cox_config_and_exports_opencode_config_env

# Stress them concurrently to reproduce the original load-triggered failure mode:
for i in $(seq 1 40); do cargo nextest run -p coxagent-infrastructure -E 'test(run_)' & done; wait
```

Expected outcomes under normal load — pass within seconds. If either reports an elapsed /
timed-out outcome again, the budget has been tightened below what this class of host needs;
do not raise it blindly (see Edge cases).

Assertions made by each:

```
claude : outcome.succeeded();
        argv contains "--mcp-config" and project id "cxc";
        argv does NOT contain "tok" nor "Bearer"; mcpServers.coxagent.url + Authorization set.
opencode : outcome.succeeded(); COX_OPENCODE_CONFIG file exists before spawn;
          child saw OPENCODE_CONFIG pointing at that file's path.
```

## Interface

No public API changed; only constants inside two unit tests were touched.

- Tests affected:
  - crates/infrastructure/src/engine/claude.rs:704 — `run_passes_mcp_config_flag_to_the_real_spawn`
    (`#[tokio::test]`, temp dir + fake sh binary).
  - crates/infrastructure/src/engine/opencode.rs:937 — `run_writes_cox_config_and_exports_opencode_config_env`
    (`#[cfg(unix)] #[tokio::test]`, echoes env back as text event).
- Supporting production symbols referenced by these paths:
  - crates/infrastructure/src/proc.rs:271 — `agent_command`
  - crates/infrastructure/src/proc.rs:369 — `spawn_confined`
  - crate::engine apply_shim_path; private per-engine `exec(timeout,…)` wrapping stdout reads.

## Configuration

No runtime configuration exists for these budgets; they are hard-coded inside each test:

| Location | Before | Now |
|----------|--------|-----|
| claude.rs (~760) | from_secs(10) | from_secs(120) |
| opencode.rs (~966) | from_secs(20) | from_secs(120) |

Each call site passes its own duration; there is no global "test timeout" knob.

## Edge cases and limits

- **Deliberately out of scope:** this does not make spawn lighter-weight nor limit CI concurrency;
   it only widens the deadline so timing noise cannot flip a pure-plumbing assertion.
- **Workspace scope:** only these two infrastructure crate tests were implicated. A failure in any other
   crate's suite has a different root cause than this ticket.
- **Platform gate:** both tests are unix-only (`#[cfg(unix)]`) because they exec real shells; skipped on Windows.
- **Resist endless widening.** An unbounded/large timeout masks genuine hangs. If they still fail here,
   treat it as evidence of real slowness (e.g. pathological host), re-measure under idle CPU first,
   then investigate rather than simply bumping again.

## Code map

- crates/infrastructure/src/engine/claude.rs — Claude adapter; private log-streaming/exec helper with tokio timeout (~320); e2e plumbing asserts at ~704–780.
- crates/infrastructure/src/engine/opencode.rs — OpenCode adapter; same exec pattern (~356); e2e config/env asserts at ~937–1000.
- crates/infrastructure/src/proc.rs — shared process layer holding agent_command (271), spawn_confined (369): every confined child goes through here.
- crates/application/src/ports/outbound/engine.rs — trait AgentEnginePort + AgentRequest.timeout definition consumed by both engines' runs.
## Related

This page lives beside the other bug-fix pages in the Engineering wiki space (docs/wiki/engineering/*).

Verified related commits:

```
94fea51  fix(CXA-B037): Tests failing: cargo tests failed        feat/CXA-B037 tip (original)
099bca7  fix(CXA-B041): CXA-B037 shipped but not integrated…     re-applies identical hunks onto a
         main-backed branch and relabels in-code comments to CXA-B041
```

If you touch either engine adapter's spawn path, also run the hex IO gate so nothing reintroduces
direct std::process/std::fs where a port should be used:
`cargo test -p coxagent-app hexagonal_gate`.
