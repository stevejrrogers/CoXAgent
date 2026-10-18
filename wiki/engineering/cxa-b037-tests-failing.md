FOLDER: Testing

# Failing cargo tests (engine spawn timeouts under CI load)

**Keywords:** cargo test, failing test, engine adapter, claude.rs, opencode.rs, AgentRequest timeout, flaky test under load, CI stability, mcp-config plumbing

## Overview

CXA-B037 fixed two unit tests that failed deterministically when the full
cargo workspace suite ran on a loaded CI machine. Both tests spawn a real child
process through the production agent-spawn path (`AgentEnginePort::run`) and assert
argv/env plumbing — they are not latency-sensitive — but were given tight wall-clock
timeouts a busy runner could outlast before any assertion ran. The fix raised those
budgets to 120 seconds so each test asserts *what* gets plumbed to the child,
never *how fast* it completes.

It matters to anyone who sees `cargo test -p coxagent-infrastructure` fail inside
the engine adapters, and to agents editing either adapter without regressing its
end-to-end plumbing test.

## How it works

The two affected tests exercise an engine's run path end-to-end instead of mocking
the child binary:

- **Claude** — crates/infrastructure/src/engine/claude.rs:704,
  `run_passes_mcp_config_flag_to_the_real_spawn`. It builds a real command via
  `crate::proc::agent_command`, appends arg-style flags plus MCP wiring through a temp config,
  then runs it through ClaudeEngine's private spawn path against a fake shell script that records its own argv.
- **OpenCode** — crates/infrastructure/src/engine/opencode.rs:937,
  `run_writes_cox_config_and_exports_opencode_config_env`. It writes an OpenCode config file before spawning and has the fake binary echo an env var back as output so resolution can be asserted on stdout.

Both funnel into their private per-engine `exec(...)` helper that wraps stdout streaming in
a wall-clock deadline:

```
claude   : let Ok(read) = tokio::time::timeout(timeout, read).await ...   claude.rs:320
opencode : let Ok(read) = tokio::time::timeout(timeout, read).await ...   opencode.rs:410
```

That budget originates from each caller passing its own duration into exec —
for these e2e tests it was hard-coded inside each test body.

The original bug was purely latency classification under load:

```
before (feat/CXA-B037 tip): claude Duration::from_secs(10) | opencode Duration::from_secs(20)
after                         both std::time::Duration::from_secs(120)
```

Under heavy CI even a one-line shell echo can exceed those short budgets; because exec kills the run on its own deadline before argv/env can be asserted, cargo reported them as timed-out failures — hence "Tests failing: cargo tests failed".

During integration onto main-backed branches commit `/099bca7`
("CXA-B037 shipped but not integrated…") re-applied identical hunks and relabeled the inline comments from CXA-B037 to **CXA-B041**. Both files today carry that label at exactly these two sites; note this when grepping for B037 in source.

## Usage

Run just these two tests directly (they exec real children on unix):

```bash
# Claude adapter e2e argv plumbing:
cargo test -p coxagent-infrastructure run_passes_mcp_config_flag_to_the_real_spawn

# OpenCode adapter e2e env/config plumbing:
cargo test -p coxagent-infrastructure run_writes_cox_config_and_exports_opencode_config_env

# Both together with output:
cargo test -p coxagent-infrastructure -- --nocapture \
  run_passes_mcp_config_flag_to_the_real_spawn \
  run_writes_cox_config_and_exports_opencode_config_env

# Stress them concurrently to reproduce the original load-triggered failure mode:
for i in $(seq 1 40); do \
  cargo nextest run -p coxagent-infrastructure -E 'test(run_)' & \
done; wait
```

Expected outcomes under normal load are pass within seconds; timing noise should never flip them now.
If either reports an elapsed/timeout outcome again after this change treat it as evidence of real host slowness,
not something to paper over by widening further (see Edge cases).

Assertions made by each:

```
claude   : outcome.succeeded();
           argv contains "--mcp-config" flag/value pair;
           args do NOT carry raw token material ("tok"/"Bearer");
           MCP server URL + Authorization resolve into mcpServers.coxagent.
opencode : outcome.succeeded();
           COX_OPENCODE_CONFIG file exists before spawn;
           child saw OPENCODE_CONFIG pointing at that file's path.
```

## Interface

No public API changed; only constants inside two unit tests were touched.

- Tests affected:
  - crates/infrastructure/src/engine/claude.rs:704 —
    `run_passes_mcp_config_flag_to_the_real_spawn`
    (`#[tokio::test]`, temp dir + fake sh binary).
    Timeout constant at ~line 760.
  - crates/infrastructure/src/engine/opencode.rs:937 —
    `run_writes_cox_config_and_exports_opencode_config_env`
    (`#[cfg(unix)] #[tokio::test]`, echoes env back as output).
    Timeout constant at ~line 973.
- Production symbols referenced by these paths:
  - crates/infrastructure/src/proc.rs — shared process layer holding agent_command / spawn_confined;
    every confined child goes through here.
  - Per-engine private exec helper wrapping stdout reads in tokio timeouts (claude.rs ~259 / ~320; opencode.rs ~356 / ~410).
  - crates/application/src/ports/outbound/engine.rs —
    trait AgentEnginePort + AgentRequest.timeout consumed by both engines' runs.
- In-code comments referencing this fix are labeled CXA-B041 after integration relabeling;
  94fea51 carries them as CXA-B037 on its branch tip.

## Configuration

There is no runtime configuration for these budgets; they are hard-coded inside each test body:

| Location | Before | After |
|----------|--------|-------|
| claude.rs (~760) | Duration::from_secs(10) | Duration::from_secs(120) |
| opencode.rs (~973) | Duration::from_secs(20) | Duration::from_secs(120) |

Each call site passes its own duration into exec(); there is no global "test timeout" knob,
and production runs keep their normal AgentRequest.timeout semantics unchanged.

## Edge cases and limits

What this deliberately does NOT do:

- It does not make spawn lighter-weight nor limit CI concurrency; it only widens deadlines so timing noise cannot flip pure-plumbing assertions.
- Workspace scope was only these two infrastructure-crate tests. A failure elsewhere has a different root cause than this ticket.
- Platform gate applies today because both harnesses exec real shells on unix-only paths.

How it fails / known limits:

- **Resist endless widening.** An unbounded or huge timeout masks genuine hangs rather than exposing them. These budgets exist only so slow-but-correct child runs are not killed by their own deadline before assertions run; keep them generous enough for heavy parallelism while leaving genuine hangs observable rather than masked forever.
- If they still fail after raising once more than twice already landed (10 -> 20 -> 120 across builds), do not simply bump again — measure first under idle CPU. If genuinely slow even idle, investigate host pathology rather than adjusting durations blindly across job sizes whose CPU quota varies between runners.
- These budgets cover assertion-up-front correctness only; they say nothing about whether production `AgentRequest.timeout` latency semantics themselves are correct overall.

## Code map

— crates/infrastructure/src/engine/claude.rs —
Claude engine adapter. Private log-streaming/spawn helper around standard output (~259), wrapping reads in tokio timeouts (~320); e2e plumbing assertion block at ~704–780 with timeout constant at ~760 labeled CXA-B041 after integration relabeling (originally CXA-B037).

— crates/infrastructure/src/proc.rs —
Shared process layer holding agent_command (~271) and spawn_confined (~369): every confined child spawned by either engine passes through here; both fixtures route via agent_command so neither produces direct std subprocess wiring that bypasses confinement port logic.

— mirrors —
This page is mirrored verbatim at docs/wiki/engineering/testing/cxa-b037-tests-failing.md alongside other Engineering wiki pages organized by topic folder (configuration/, deployment/, release-process/, testing/).

## Related

This page sits beside other bug-fix pages in the Engineering wiki space (docs/wiki/engineering/*). Verified related commits:

```
94fea51  fix(CXA-B037): Tests failing: cargo tests failed        feat/CXA-B037 tip (original)
099bca7  fix(CXA-B041): CXA-B037 shipped but not integrated…     re-applies identical hunks onto a
         main-backed branch and relabels in-code comments to CXA-B041
```

If you touch either engine adapter's spawn path, also run the hex IO gate so nothing reintroduces direct std process/std fs where a port should be used:
`cargo test -p coxagent-app hexagonal_gate`.
