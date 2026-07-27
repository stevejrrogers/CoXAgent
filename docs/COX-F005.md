# COX-F005: Pre-Deploy Health Check

## Overview

The pre-deploy health check (COX-F005) is a mandatory gate that verifies the deployed application is actually running and accepting requests after a `docker compose up` succeeds. This gate prevents silent deploy failures where containers start but the application inside never binds its configured port or crashes immediately after startup.

**Who it's for:** Every deploy path in CoXAgent — the autonomous cycle, chat's "deploy" command, and the PR-preview endpoint — uses this gate to ensure consistent quality.

**Core value:** A successful `docker compose up` exit code (0) only proves containers started; it says nothing about whether the app inside actually bound its port. This gate closes that gap by probing the app's health endpoint, capturing detailed diagnostics (HTTP status, response time), and triggering auto-rollback if the app never comes up.

---

## How It Works

### The Gate Chain

Every deploy request runs through two sequential gates:

1. **COX-B004 (Liveness Gate):** A simple yes/no TCP liveness check — is something accepting connections on `127.0.0.1:<host_port>`? This is the mandatory gate that blocks all deploy paths.

2. **COX-F005 (Detailed Health Gate):** A richer HTTP probe that captures the full diagnostic picture (HTTP status, response time, pass/fail) and records it in deploy history. This runs *after* COX-B004 passes.

### Polling Strategy

The health check uses polling, not a single probe:

- **Poll interval:** 2 seconds
- **Timeout:** Configurable via `config.deploy.health_check_timeout_secs` (default: 60 seconds)
- **Probe count:** Up to 30 probes (60 seconds ÷ 2 seconds/interval)

**Why polling?** `docker compose up` returns as soon as containers *start*, seconds before the app inside binds its port. A single immediate probe would fail perfectly healthy deploys. Polling waits for the app to become ready while respecting a bounded timeout, so an endpoint that never answers resolves to `passed: false` rather than hanging.

### Response Classification

The HTTP GET on the app's root (`http://127.0.0.1:<port>/`) is classified as follows:

| HTTP Status | Interpretation | Outcome |
|-------------|---|---|
| **2xx** | Healthy | ✅ Pass (app is up) |
| **3xx** | Healthy | ✅ Pass (app is up and redirecting) |
| **4xx** | Healthy | ✅ Pass (app is up; request was bad) |
| **5xx** | Unhealthy | ❌ Fail (app is broken) |
| **No response** | Unreachable | ❌ Fail (connection refused, timeout, DNS failure) |

This design prevents unnecessary rollbacks for projects that don't have a root route or use it differently — we only fail on 5xx (genuinely broken app) or complete absence of a response (app never bound the port).

---

## Configuration

Health check behavior is controlled via `DeployConfig`:

```rust
pub struct DeployConfig {
    /// TCP port on which the deployed app should answer.
    pub host_port: Option<u16>,
    /// Whether deploy/test gates are enabled.
    pub enabled: bool,
    /// Whether to auto-rollback to the last known-good deploy on failure.
    pub auto_rollback: bool,
    /// Maximum age of a known-good deploy before rollback is skipped.
    pub max_rollback_age_secs: u64,
    /// Database migration detection paths (to prevent rollback across schema changes).
    pub migration_detection_paths: Vec<String>,
    /// Timeout for the health endpoint probe (COX-F005).
    pub health_check_timeout_secs: u64,
}
```

### Defaults

- `host_port`: `None` (no health check — app must opt in by configuring a port)
- `health_check_timeout_secs`: `60` seconds
- `auto_rollback`: `false` (opt-in; off by default)

### Environment Variables

Resource limits on deployed containers (best-effort):
- `COXAGENT_DEPLOY_CPUS`: CPU limit per container (default: `2`)
- `COXAGENT_DEPLOY_MEM`: Memory limit per container (default: `1g`)

Set either to `off` to disable the limit.

---

## Behavior in the Cycle

### Deploy Success Flow

```
┌─────────────────────────────────────────────────┐
│ DEV completes feature/bug: new commit pushed    │
└───────────────┬─────────────────────────────────┘
                │
        ┌───────▼────────────┐
        │ Deploy (docker up) │
        └───────┬────────────┘
                │
        ┌───────▼──────────────────────────┐
        │ COX-B004: TCP liveness (yes/no)  │
        │ polls every 2s for 30s          │
        └───────┬──────────────────────────┘
                │
            ❌ FAIL → attempt auto-rollback
                │
            ✅ PASS
                │
        ┌───────▼─────────────────────────┐
        │ COX-F005: HTTP health endpoint   │
        │ GET /; capture status & timing   │
        └───────┬──────────────────────────┘
                │
            ❌ FAIL (5xx or no response)
                │
            ✅ PASS (any 1xx–4xx)
                │
        ┌───────▼─────────────────┐
        │ Run test suite           │
        └───────┬─────────────────┘
                │
        ┌───────▼─────────────────────────────────┐
        │ Record deploy + health_check result     │
        │ Update refs/coxagent/last-good if ok    │
        └─────────────────────────────────────────┘
```

### On Health Check Failure

If the health endpoint never answers or returns 5xx within the timeout:

1. **Log the failure** with HTTP status and response time
2. **Trigger auto-rollback** (if enabled) to the last known-good deploy
3. **File a bug** if rollback fails or is not enabled
4. **Continue the cycle** — one bad deploy never stalls the team

The failure is recorded in `deploy_attempt.health_check` with:
- `passed: false`
- `http_status`: The last probe's status (or `None` if unreachable)
- `response_time_ms`: Milliseconds for the last probe

---

## API & Ports

### DeployPort Trait

Three health-related methods:

#### `async fn health(&self, port: u16) -> Result<bool, PortError>`

**TCP liveness check (COX-B004).** Is something accepting connections on the port?

- **Default:** `Ok(true)` (no daemon needed)
- **Used by:** Auto-rollback path (needs only yes/no)
- **Adapters override:** Yes, for project-specific health logic

#### `async fn health_check(&self, port: u16) -> HealthCheckResult`

**HTTP health probe (COX-F005).** GET the app's root and capture diagnostics.

```rust
pub struct HealthCheckResult {
    pub passed: bool,           // true if 1xx–4xx or 5xx not received
    pub http_status: Option<u16>, // HTTP status if endpoint was reachable
    pub response_time_ms: Option<u64>, // Round-trip time in milliseconds
}
```

- **Default:** Wraps `health()` so adapters that don't override it still report timing
- **Used by:** Deploy history and dashboard diagnostics
- **Adapters override:** Optional; `DockerComposeDeploy` does

#### `async fn wait_healthy(&self, port: u16, timeout: Duration) -> HealthCheckResult`

**The polling gate (COX-F005).** Poll `health_check()` every 2 seconds until it passes or `timeout` elapses.

```rust
// Polling loop (pseudocode)
let deadline = now() + timeout;
loop {
    let result = self.health_check(port).await;
    if result.passed {
        return result;  // Pass immediately; no waiting if healthy
    }
    if now() + POLL_INTERVAL >= deadline {
        return result;  // Timeout; return last probe's detail
    }
    sleep(2 seconds).await;
}
```

- **Guarantee:** Returns the probe that decided the outcome (pass or final timeout)
- **No hanging:** Unreachable endpoints timeout with `passed: false`, never hang
- **Deterministic timing:** Last probe's time survives, not a bare "timeout" marker

### Example: DockerComposeDeploy

```rust
async fn health_check(&self, port: u16) -> HealthCheckResult {
    let url = format!("http://127.0.0.1:{port}/");
    let start = std::time::Instant::now();
    
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))  // Per-probe timeout
        .build()?;
    
    match client.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status();
            HealthCheckResult {
                passed: !status.is_server_error(),  // 1xx–4xx pass
                http_status: Some(status.as_u16()),
                response_time_ms: Some(elapsed_ms),
            }
        }
        Err(_) => HealthCheckResult {
            passed: false,
            http_status: None,
            response_time_ms: Some(elapsed_ms),
        }
    }
}
```

---

## Deploy History Recording

Each deploy attempt captures health check diagnostics:

```rust
pub struct DeployAttempt {
    pub success: bool,
    pub deployed: bool,
    pub summary: String,
    pub commit_sha: Option<String>,
    pub health_check: Option<HealthCheckResult>,  // ← COX-F005 result
}
```

The `health_check` field is populated *after* `wait_healthy` completes, allowing the dashboard and logs to show:
- Whether the health check passed
- What HTTP status was returned (if reachable)
- How long the probe took
- The probe's timestamp (via `DeployAttempt.at`)

---

## Edge Cases & Limitations

### No Host Port Configured

If `config.deploy.host_port` is `None`:
- Health check is skipped entirely
- `run_health_check()` returns `(true, None)` immediately
- Deploy proceeds as if health check passed
- Use case: Projects that don't expose an HTTP port (e.g., background workers, internal services)

### Adapters Without HTTP Health

Test doubles and simple adapters (e.g., `ScriptedDeploy`) that override only `health()` but not `health_check()`:
- `wait_healthy` uses the default `health_check` implementation
- Default wraps `health()` and reports only timing, no HTTP status
- Deploy works but dashboard shows `http_status: None`
- No breaking change to existing tests

### Hang Guard

The cycle wraps `wait_healthy` with a hang guard:

```rust
const HANG_GUARD: Duration = Duration::from_secs(10);  // Slack above the configured bound
let result = tokio::time::timeout(bound + HANG_GUARD, deploy.wait_healthy(port, bound)).await;
```

If a custom adapter's `health_check` wedges (takes >5s without returning), the hang guard fires after `health_check_timeout_secs + 10s`, returning `passed: false` with no detail. This catches adapter bugs without losing probe diagnostics when adapters behave correctly.

### Never Healthy Endpoint

If the endpoint never answers (connection refused, timeout, or persistent 5xx):
- Polling continues for the full timeout window
- Caller gets the *last* probe's detail (not a bare timeout)
- Useful for logs: "probe 30 of 30 at 60s: connection refused" vs. "timeout"

### Auto-Rollback Constraints

Auto-rollback is skipped (and the failure is filed as a bug) if:
- `config.deploy.auto_rollback` is `false` (default)
- No prior known-good deploy exists (first deploy ever)
- The known-good deploy is older than `max_rollback_age_secs` (default: 3600s = 1 hour)
- Any database migrations ran between the known-good and failed deployment

---

## Tests & Verification

### Unit Tests

Located in `crates/application/src/ports/outbound/deploy.rs`:

- `already_healthy_resolves_immediately_on_the_first_probe`: Healthy endpoint costs exactly one probe and no waiting
- `never_healthy_resolves_false_within_the_bound`: Unhealthy endpoint resolves within the configured timeout, not hanging
- `default_health_check_reports_timing_without_a_status`: Default implementation carries timing even without HTTP status

### Integration Test

Located in `crates/application/src/use_cases/cycle.rs`:

- `health_check_failure_triggers_rollback_like_any_other_deploy_failure`: A deploy whose containers start but never bind the port triggers auto-rollback, same as a hard deploy failure

Example scenario:
1. Forward deploy starts but app never binds port → health check fails
2. Auto-rollback triggers, redeploying the last known-good version
3. Rollback deploy succeeds and health check passes
4. Cycle records rollback success and continues

---

## Configuration Examples

### Basic: Minimal Health Check

```toml
[deploy]
host_port = 8101
enabled = true
auto_rollback = false
health_check_timeout_secs = 60  # Default
```

Health check runs after every deploy, but failures only file bugs — no automatic recovery.

### Production: Auto-Rollback Enabled

```toml
[deploy]
host_port = 8101
enabled = true
auto_rollback = true
max_rollback_age_secs = 3600      # 1 hour
health_check_timeout_secs = 90    # Allow extra startup time
migration_detection_paths = ["db/migrations"]
```

Failed deploys roll back automatically if a known-good version exists within the last hour and no migrations have run since.

### Docker Resource Limits

```bash
export COXAGENT_DEPLOY_CPUS=4
export COXAGENT_DEPLOY_MEM=2g
cargo run  # All deployed containers limited to 4 CPUs, 2GB memory
```

Disable limits with `off`:
```bash
export COXAGENT_DEPLOY_CPUS=off
```

---

## Implementation Notes

### Ports Used

- Health probes connect to `127.0.0.1:<host_port>` (e.g., `127.0.0.1:8101`)
- HTTP client timeout per probe: 5 seconds
- TCP liveness check (COX-B004): 3-second timeout
- Polling interval: 2 seconds

### Timing Semantics

- `response_time_ms` is the round-trip time for a single probe GET request
- Each poll costs 0–5 seconds, depending on what the endpoint does
- Timeouts are cumulative: if a probe takes 3s to fail and we poll 30 times, worst case is 90+ seconds
- The last probe's timing is recorded, not the sum or average

### Executor Assumptions

- Runs on Tokio async runtime (`tokio::time::sleep`, `tokio::time::timeout`)
- No blocking calls (all I/O is async)
- `reqwest` client is used for HTTP probes (Docker adapter)

---

## See Also

- **COX-B001:** Auto-rollback to last known-good deploy on failure
- **COX-B004:** Mandatory post-deploy liveness gate
- **COX-F001:** Auto-rollback mechanics and known-good tracking
- `DeployPort` trait: `crates/application/src/ports/outbound/deploy.rs`
- `RunCycleUseCase`: `crates/application/src/use_cases/cycle.rs` (see `run_health_check`, `verify_health_after_deploy`)
- `DockerComposeDeploy`: `crates/infrastructure/src/deploy/docker_compose.rs` (HTTP health_check implementation)
