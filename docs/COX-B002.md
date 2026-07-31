# COX-B002: Sandboxing Silently No-ops for Hermes Engine

**Keywords:** hermes, sandbox, confinement, file write, security, seatbelt, bwrap, agent isolation, workspace

## Overview

HermesEngine defaults to unconfined agent execution — file writes are unrestricted. When sandboxing is not explicitly enabled via `.with_sandbox(true)`, the engine runs the agent without attempting confinement and reports `SandboxStatus::NotRequested` to indicate no sandboxing was requested. **The defect:** this happens silently with no warning, even if confinement was expected. Unlike macOS Seatbelt and Linux Bubblewrap adapters which report `SandboxStatus::Unavailable` when sandboxing is requested but unavailable on the host, HermesEngine offers no signal that writes might be unprotected.

**Who it affects:** Anyone deploying HermesEngine as an agent engine in CoXAgent. The risk is elevated when Hermes runs in untrusted or multi-tenant environments where an agent bug could write files outside the project workspace.

**Root cause:** HermesEngine.new() defaults `sandbox: false`. The builder does not warn when this field is left in its default state, and the engine silently reports `NotRequested` instead of forcing the caller to acknowledge the risk explicitly.

---

## How It Works

The CoXAgent agent framework runs CLI engines (claude, opencode, hermes, ...) in confined spawned processes. Each engine adapter controls whether file writes to the agent are restricted.

### Write Confinement Architecture

1. **Engine declares a policy:** Each engine's `sandbox_status()` returns one of:
   - `NotRequested` — sandboxing was not enabled; the run is **unrestricted**
   - `Confined(mechanism)` — writes are confined to workspace + tool caches via the named mechanism (e.g. "seatbelt" on macOS, "bwrap" on Linux)
   - `Unavailable(reason)` — sandboxing was **requested** but this host cannot provide it; the run is **unrestricted**, and callers are **warned**

2. **Caller sees the status:** The run's `AgentOutcome` carries the `SandboxStatus`, surfaced in logs and dashboards so operators know whether writes were actually confined.

3. **Platform enforcement:** On macOS, `crate::proc::output_confined()` wraps the spawn with Seatbelt (`sandbox-exec`). On Linux, it uses Bubblewrap (`bwrap`) if available. If neither is available on the host AND sandboxing was requested, the system sets status to `Unavailable` and documents the gap.

### The Hermes Gap

```rust
// Hermes defaults to unconfined
let engine = HermesEngine::new("hermes-3-llama-3.2-3b");
assert_eq!(engine.sandbox_status(), SandboxStatus::NotRequested);
// ↑ Run is UNCONFINED. No warning, no `Unavailable` signal.
// Caller must explicitly opt in:

let engine = HermesEngine::new("...").with_sandbox(true);
assert_ne!(engine.sandbox_status(), SandboxStatus::NotRequested);
// ↑ Now confined (if platform supports it) OR marked `Unavailable`
```

Contrast with ClaudeEngine and OpencodeEngine, which follow the same pattern but have identical gaps — all three engines silently accept the unconfined default.

## Usage

### Create an Engine

Declare a Hermes engine:

```rust
use coxagent_infrastructure::engine::HermesEngine;

let engine = HermesEngine::new("hermes-3-llama-3.2-3b");
```

### Enable Sandboxing (Recommended)

Confine file writes to the workspace:

```rust
let engine = HermesEngine::new("hermes-3-llama-3.2-3b")
    .with_sandbox(true);
```

On macOS, this uses Seatbelt. On Linux with Bubblewrap available, it uses `bwrap`. If the host cannot provide confinement, the engine will report `SandboxStatus::Unavailable` and the run executes unconfined with a clear warning.

### Check Sandbox Status

Inspect the policy (typically done on the outcome, not the engine itself):

```rust
let outcome = engine.run(request).await?;
match outcome.sandbox {
    SandboxStatus::Confined(mechanism) => {
        println!("Confined via {}", mechanism);
    }
    SandboxStatus::Unavailable(reason) => {
        eprintln!("WARNING: Sandboxing unavailable — {}", reason);
    }
    SandboxStatus::NotRequested => {
        println!("Sandboxing was not requested");
    }
}
```

## Interface

### HermesEngine

```rust
pub struct HermesEngine {
    model: String,
    binary: String,
    sandbox: bool,  // Defaults to false
}

impl HermesEngine {
    /// Create a new engine for the specified model.
    pub fn new(model: impl Into<String>) -> Self;

    /// Enable or disable write confinement.
    pub fn with_sandbox(mut self, sandbox: bool) -> Self;

    /// Report the confinement policy.
    pub fn sandbox_status(&self) -> SandboxStatus;

    /// Run the agent in the configured model/sandbox setting.
    pub async fn run(&self, request: AgentRequest) -> Result<AgentOutcome, PortError>;
}
```

### SandboxStatus Enum

```rust
pub enum SandboxStatus {
    /// No sandboxing was requested.
    NotRequested,
    /// Writes were confined via the named mechanism.
    Confined(&'static str),
    /// Sandboxing was requested but is unavailable on this host.
    Unavailable(&'static str),
}
```

## Configuration

### Sandbox Flag

The `sandbox` field on HermesEngine controls whether the run attempts confinement:

- **false (default):** Run unconfined; agent can write anywhere.
- **true:** Request confinement. On macOS/Linux with Seatbelt/Bubblewrap, writes are restricted. Otherwise, report `Unavailable`.

### Platform Support

| Platform | Supported | Mechanism | Enabled When |
|----------|-----------|-----------|--------------|
| **macOS** | ✓ | Seatbelt (`sandbox-exec`) | `sandbox=true` |
| **Linux** | ✓ (opt-in) | Bubblewrap (`bwrap`) | `sandbox=true` AND `bwrap` on `PATH` |
| **Other** | ✗ | None | `sandbox=true` → reports `Unavailable` |

### Writable Paths (When Confined)

When sandboxing is active, an agent can write to:

- The managed project's workspace (e.g. `/path/to/workspace/codebase`)
- Tool caches: `~/.cargo`, `~/.npm`, `~/.gradle`, `~/.m2`, etc.
- CoXAgent state: `~/CoXAgent`, `~/.claude`, `~/.config`, `~/.cache`, `~/.local`
- Temp directories: `$TMPDIR` (if set), `/tmp` (Linux), `/private/tmp` (macOS)

Everything else stays **read-only**.

## Edge Cases and Limits

### Default Behavior Is Unconfined

Creating a HermesEngine with `new()` defaults to `sandbox: false`. No warning is issued.

```rust
let engine = HermesEngine::new("hermes-3-llama-3.2-3b");
// engine.sandbox_status() == SandboxStatus::NotRequested
// The run will be UNCONFINED.
```

Callers must explicitly opt into sandboxing; there is no "secure by default" signal.

### SandboxStatus::NotRequested Is Silent

Unlike `Unavailable`, which warns the caller ("sandboxing requested but not available"), `NotRequested` conveys no flag. Operators cannot distinguish between:

- Deliberate choice to run unconfined (intended `NotRequested`)
- Accidental omission of `.with_sandbox(true)` (unintended `NotRequested`)

### Silent No-op vs. Explicit Unavailable

Platforms without Seatbelt or Bubblewrap behave differently depending on the flag:

```rust
// Flag OFF (default) — silent no-op
let engine = HermesEngine::new("hermes-3-llama-3.2-3b");
let outcome = engine.run(request).await?;
assert_eq!(outcome.sandbox, SandboxStatus::NotRequested);
// Run is unconfined. No warning in logs or UI.

// Flag ON — explicit unavailable
let engine = HermesEngine::new("hermes-3-llama-3.2-3b").with_sandbox(true);
let outcome = engine.run(request).await?;
// On a host without Seatbelt/Bubblewrap:
assert_eq!(outcome.sandbox, SandboxStatus::Unavailable("..."));
// Run is unconfined, BUT the outcome warns the caller.
```

### Workarounds

**To ensure confinement is actually applied:**

1. **Explicitly enable it:** `.with_sandbox(true)` on every engine.
2. **Check the outcome:** Verify `outcome.sandbox != SandboxStatus::NotRequested` after the run.
3. **Fail on unavailability:** Treat `SandboxStatus::Unavailable` as a deployment error if your platform must support confinement.

**To silence warnings in trusted/single-tenant deployments:**

Document the choice and leave `sandbox: false` (default). The run is unconfined but this is expected and approved.

### No Transitive Safety

Confinement applies only to the direct agent CLI spawn. Tools the agent invokes (build, test, git commands) are confined under the same Seatbelt/Bubblewrap profile, but third-party tools run by those commands inherit the same restrictions. If a build script or test explicitly breaks out of the sandbox, confinement is bypassed — this is a feature (intentional escapes are possible) not a bug.

## Code map

- `crates/infrastructure/src/engine/hermes.rs` — HermesEngine adapter; struct definition, `.new()` constructor, `.with_sandbox()` builder, `.sandbox_status()` method, `.run()` implementation
- `crates/infrastructure/src/engine/claude.rs` — ClaudeEngine with identical sandbox default and builder pattern
- `crates/infrastructure/src/engine/opencode.rs` — OpencodeEngine with identical sandbox default and builder pattern
- `crates/infrastructure/src/proc.rs` — Core confinement machinery; `sandbox_status()` policy check, `confined_command()` Seatbelt/Bubblewrap setup, `agent_command()` command builder, `output_confined()` Seatbelt retry wrapper, `spawn_confined()` and `spawn_confined_within()` streaming spawn helpers
- `crates/application/src/ports/outbound/engine.rs` — Port types; `SandboxStatus` enum, `AgentEnginePort` trait
- `crates/app/tests/sandbox_confinement_gate.rs` — Regression test validating every `agent_command()` caller routes through `output_confined()`/`spawn_confined()` (COX-B022 safety gate)

## Related

- [COX-B013](COX-B013.md) — Transient Seatbelt failures on macOS; the retry wrapper that makes confinement reliable on macOS
- [COX-B022](COX-B022.md) — Confinement wiring regression; the compile-time gate that catches if this fix is lost in a forward-port or merge
- [COX-B004](COX-B004.md) — Post-deploy health gate; verifies the running app bound its port (unrelated to sandboxing)
- `crates/infrastructure/src/engine/claude.rs` — ClaudeEngine with identical sandbox no-op defaults
- `crates/infrastructure/src/engine/opencode.rs` — OpencodeEngine with identical sandbox no-op defaults
- `crates/application/src/config.rs` — Engine configuration and discovery logic
