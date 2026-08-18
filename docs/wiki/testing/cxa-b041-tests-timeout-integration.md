FOLDER: Testing

# Engine spawn tests time out deterministically (CXA-B041)

**Keywords:** cargo test, failing test suite, engine adapter, claude.rs, opencode.rs, AgentRequest timeout, spawn timeout, CI red build, mcp-config plumbing integration

## Overview

CXA-B041 exists because CXA-B037 "shipped but not integrated": its fix — widening
the wall-clock timeouts on two engine-spawn unit tests from 10s/20s to 120s — landed
on branch `feat/CXA-B037` (commit 94fea51) but was **never merged into `main`**, so
main's full suite stayed red with those two tests timing out deterministically.
CXA-B041 re-applies that same widening onto a main-backed branch (commit 099bca7)
and relabels the inline comments from CXA-B037 to CXA-B041.

It matters to anyone who sees `cargo test -p coxagent-infrastructure --lib` fail inside
the engine adapters under load, and to any agent touching either adapter's real-spawn
plumbing test without regressing it. Read this alongside cxa-b037-tests-failing.md,
which documents the identical change; this page focuses on what "not integrated" meant
and why a wider timeout is a stopgap, not a root-cause cure.

## How it works

The two affected tests exercise an engine's production spawn path end-to-end instead of
mocking the child binary. Each builds a command via `crate::proc::agent_command`, funnels it
through the per-engine private `exec(...)` helper, and runs it against a fake shell script that
records argv / echoes an env var back:

- **Claude** — crates/infrastructure/src/engine/claude.rs:704,
  `run_passes_mcp_config_flag_to_the_real_spawn`. Asserts that a temp-file-backed MCP config
  (`--mcp-config`) reaches argv of a real spawned child without leaking the token.
- **OpenCode** — crates/infrastructure/src/engine/opencode.rs:937,
  `run_writes_cox_config_and_exports_opencode_config_env`. Writes an OpenCode config before
  spawning and has the fake binary echo back whether it saw `OPENCODE_CONFIG`.

Both wrap their stdout-streaming read in one wall-clock deadline:

```
claude   : let Ok(read) = tokio::time::timeout(timeout, read).await ... claude.rs:320   (inside fn exec)
opencode : let Ok(read) = tokio::time::timeout(timeout, read).await ... opencode.rs:410 (inside fn exec)
```

That budget comes from each test body passing its own duration into `.run(AgentRequest{ timeout,… })`.
The chain: caller sets `AgentRequest.timeout` -> engine `.run()` passes it to `.exec()` -> the single
reads-completion future races against that deadline. On expiry each engine kills the whole process group
via proc.rs ~402 (`kill_group`) and returns a Backend("... timed out") error rather than hanging itself.

The integration gap this ticket closed:

```
before B037        : claude Duration::from_secs(10) | opencode Duration::from_secs(20)
B037 fix (94fea51) : both std::time::Duration::from_secs(120)   <- landed on feat/CXA-B037 ONLY
main               : still from_secs(10)/(20)                    <- never picked up B037's commit
B041 fix (099bca7) : re-applies from_secs(120), relabels comments CXA-B041 -> both files now at 120s
```

Verified commit ancestry:

```
94fea51 NOT ancestor of main      # original CXA-B037 fix never integrated into main
94fea51 NOT ancestor of HEAD      # even current work carries B041's own copy, not B037's commit object itself
099bca7 EXISTS                    # fix(CXA-B041): re-applied hunks + relabelled comments; touches only claude.rs + opencode.rs (+5/-1 each)
```

On current HEAD both files carry exactly one matching hunk labelled CXA-B041:
claude.rs ~760 (`from_secs(10)` -> comment + `from_secs(120)`), opencode.rs ~973 (`from_secs(20)` -> same).

## Usage

Run just these two tests directly; they exec real children on unix:

```bash
cargo test -p coxagent-infrastructure run_passes_mcp_config_flag_to_the_real_spawn          # Claude adapter e2e argv plumbing
cargo test -p coxagent-infrastructure run_writes_cox_config_and_exports_opencode_config_env # OpenCode adapter e2e env plumbing

# Both together with output:
cargo test -p coxagent-infrastructure -- --nocapture \
  run_passes_mcp_config_flag_to_the_real_spawn \
  run_writes_cox_config_and_exports_opencode_config_env

# Full infrastructure lib crate:
cargo test -p coxagent-infrastructure --lib

# Reproduce under parallelism / host-load stress:
for i in $(seq 1 40); do cargo nextest run -p coxagent-infrastructure -E 'test(run_)' & done; wait
```

On this host both pass in isolation AND as part of the full crate (`113 passed`, ~8 s).
An elapsed/timeout here after this change is evidence of genuine host pathology or overload —
see Edge cases before bumping anything again.

Assertions each makes when green:

```
claude   : outcome.succeeded(); argv contains "--mcp-config"; argv has NO raw token ("tok"/"Bearer");
           MCP server URL + Authorization resolve into mcpServers.coxagent.
opencode : outcome.succeeded(); COX_OPENCODE_CONFIG file exists before spawn;
           child saw OPENCODE_CONFIG pointing at that file's path.
```

## Interface

No public API changed; only constants inside two unit tests were touched.

- crates/infrastructure/src/engine/claude.rs:704 — `run_passes_mcp_config_flag_to_the_real_spawn`
  (`#[tokio::test]`, temp dir + fake sh binary). Timeout constant at ~line 760.
- crates/infrastructure/src/engine/opencode.rs:937 — `run_writes_cox_config_and_exports_opencode_config_env`
  (`#[cfg(unix)] #[tokio::test]`, echoes env back as output). Timeout constant at ~line 973.
- Production symbols these paths exercise:
  - crates/infrastructure/src/proc.rs — shared process layer holding `agent_command` (~271),
    low-priority spawn / optional Seatbelt/Bubblewrap confinement via `spawn_confined` (~369),
    plus heavy-concurrency gate (~29–43). Every agent child routes through here rather than spawning directly.
  - Per-engine private exec helper wrapping stdout reads in tokio timeouts —
    claude.rs:259/~320 and opencode.rs:356/~410. On deadline expiry each calls proc kill_group(~402).
- crates/application/src/ports/outbound/engine.rs — trait AgentEnginePort + AgentRequest.timeout,
   consumed by both engines' `.run()` / `.resume_run()`.

In-code comments referencing this fix are labelled **CXA-B041** after integration relabeling;
commit fea51 carries them as CXA-B037 on its branch tip.

## Configuration

No runtime configuration controls these budgets; they are hard-coded inside each test body:

| Location | Before | After |
|----------|--------|-------|
| claude.rs (~760)   | Duration::from_secs(10) | Duration::from_secs(120) |
| opencode.rs (~973) | Duration::from_secs(20) | Duration::from_secs(120) |

Each call site passes its own duration into `.exec()`; there is no global "test timeout"
knob. Production runs keep their normal semantics unchanged — only these two hard-coded values moved;
no env var changes these budgets directly (CI can still influence wall-clock scheduling).

## Edge cases and limits

What this deliberately does NOT do:

- It does not make spawning lighter-weight nor cap CI concurrency. It only widens deadlines so timing noise cannot flip pure-plumbing assertions that are otherwise correct-but-slow under load.
- Scope was only these two infrastructure-crate unit tests. A failing suite anywhere else has a different root cause than this ticket.
- Platform-gated today because both harnesses exec real shells over unix-only paths (`#[cfg(unix)]`).

How it fails today / known limits:

- If heavy parallelism starves CPU slots long enough for even one-line echo latencies to exceed budget,
  exec fires kill_group(...), returns Backend("... timed out"), cargo reports elapsed/failure though plumbing was correct.
- **Widening an already-wide timeout does not cure a deterministic hang.** This is the most important limit for future readers:
   raising durations masks latency classification but never fixes something that blocks forever regardless of budget. These budgets exist so slow-but-correct children are not killed by their own deadline before assertions run; keep them generous enough for heavy parallelism while leaving genuine hangs observable rather than masked forever. If they fail again after raising twice across builds now ("currently raised twice" across code-review wording changes), measure under idle CPU first; if genuinely slow even idle investigate host pathology rather than blindly bumping durations whose job-size CPU quota varies between runners.
- These budgets assert *what* gets plumbed up-front only; they say nothing about whether production AgentRequest.timeout latency semantics themselves are correct overall.

## Code map

- crates/infrastructure/src/engine/claude.rs — Claude engine adapter. Private `exec` (~259) wraps stdout reads in a tokio timeout (~320); e2e plumbing test at ~704 with the 120s constant at ~760 (labelled CXA-B041).
- crates/infrastructure/src/engine/opencode.rs — OpenCode engine adapter. Private `exec` (~356) wraps stdout reads in a tokio timeout (~410); e2e config/env test at ~937 with the 120s constant at ~973 (labelled CXA-B041).
- crates/infrastructure/src/proc.rs — shared process layer: `agent_command` (~271), `spawn_confined` (~369), heavy-concurrency gate (~29–43), `kill_group` (~402). Every agent child routes through here.
- crates/application/src/ports/outbound/engine.rs — trait `AgentEnginePort` + `AgentRequest.timeout`, consumed by both engines' `.run()` / `.resume_run()`.
- docs/wiki engineering testing pages — this page's sibling cxa-b037-tests-failing.md documents the identical change; both live under the 'testing/' topic of the Engineering wiki space.

## Related

This ticket is the integration of CXA-B037, so it connects most directly to:

- CXA-B037 "Tests failing: cargo tests failed" — original fix on feat/CXA-B037 (commit 94fea51), never merged to main; documented in cxa-b037-tests-failing.md.
- Any engine-spawn change should re-run these two tests plus the hex IO gate (`cargo test -p coxagent-app hexagonal_gate`) so no direct std::process/std::fs reintroduction sneaks into an adapter.
