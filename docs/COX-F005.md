FOLDER: -

# COX-F005: Post-Deploy Health Check

**Keywords:** health check, deployment gate, polling, HTTP probe, docker compose, app liveness, auto-rollback

## Overview

The post-deploy health check ensures the deployed application actually runs and answers requests after `docker compose up` succeeds. A successful compose exit code (0) only proves containers started; it says nothing about whether the app inside bound its port or crashed immediately. This gate polls the app's health endpoint to close that gap, capturing HTTP status and timing diagnostics, and triggering auto-rollback if health checks fail.

**Who it's for:** Every deploy path—the autonomous cycle, chat's "deploy" command, and the PR-preview endpoint—uses this gate to ensure consistent quality.

---

## How It Works

### The Mandatory Gate

Every successful `docker compose up` is followed by an HTTP health probe. The app gets a bounded window to answer on its configured port; if it never does (connection refused, persistent 5xx status, or timeout), the deploy is rejected and rollback is attempted.

**Polling, not a single probe:** `docker compose up` returns as soon as containers *start*, seconds before the app inside binds its port. Polling waits for the app to become ready while respecting a bounded timeout, so an app with a cold-start delay (e.g., database migrations) can still pass.

- **Poll interval:** 2 seconds
- **Per-probe HTTP timeout:** 5 seconds
- **Deployment gate timeout:** Fixed at 30 seconds (shared gate used by all deploy paths)
- **Cycle recording timeout:** Configurable via `config.deploy.health_check_timeout_secs` (default: 60 seconds)

### Response Classification

The HTTP GET on the app's root (`http://127.0.0.1:<port>/`) is classified as follows:

| HTTP Status | Interpretation | Outcome |
|-------------|---|---|
| **2xx** | Healthy | ✅ Pass (app is up) |
| **3xx** | Healthy | ✅ Pass (app is up and redirecting) |
| **4xx** | Healthy | ✅ Pass (app is up; request was bad) |
| **5xx** | Unhealthy | ❌ Fail (app is broken) |
| **No response** | Unreachable | ❌ Fail (connection refused, timeout) |

This design prevents unnecessary rollbacks for projects that don't have a root route or use it differently — only 5xx or absence of response triggers failure.

### Two Gates, Same Probe Mechanism

**Shared gate (COX-B004/B009):** `verify_deploy_health()` is called by all deploy paths (cycle, chat, PR preview). Hard-coded 30-second timeout. Returns bool only (no result recording). Used to immediately fail deploys that don't answer on their port.

**Cycle recording gate (COX-F005):** `run_health_check()` in the cycle. Configurable timeout (default 60s). Records full result (HTTP status, response time) in deploy history for dashboard visibility.

Both use the same underlying health check mechanism (`health_check()` method → `wait_healthy()` polling loop).

---

## Usage

### Configuring the Health Check

Enable health checking by setting a port in your deploy configuration:

```toml
[deploy]
host_port = 8101
enabled = true
auto_rollback = false
health_check_timeout_secs = 60
```

Once configured, the health check runs automatically after every deployment. No additional setup required in code.

### Deployment Flow

The health check integrates into the deployment lifecycle as follows:

```
┌─────────────────────────────────────────────────┐
│ DEV completes feature/bug: new commit pushed    │
└───────────────┬─────────────────────────────────┘
                │
        ┌───────▼────────────┐
        │ Deploy (docker up) │
        └───────┬────────────┘
                │
        ┌───────▼────────────────────────────────┐
        │ Health check: GET /                    │
        │ Poll every 2s for up to 30s            │
        │ (shared gate via verify_deploy_health) │
        └───────┬────────────────────────────────┘
                │
            ❌ FAIL → attempt auto-rollback
                │
            ✅ PASS
                │
        ┌───────▼────────────────────────────────┐
        │ Run test suite                         │
        └───────┬────────────────────────────────┘
                │
        ┌───────▼────────────────────────────────┐
        │ Record deploy + health_check result    │
        │ (cycle's F005 gate records detail)     │
        │ Update refs/coxagent/last-good if ok   │
        └────────────────────────────────────────┘
```

### When Health Check Fails

If the health endpoint never answers or returns 5xx within the bound:

1. **Log the failure** with HTTP status and response time
2. **Reject the deploy** — it never becomes the auto-rollback target
3. **Trigger auto-rollback** (if enabled) to the last known-good deploy
4. **File a bug** if rollback fails or is not enabled
5. **Continue the cycle** — one bad deploy never stalls the team

Failure is recorded in `deploy_attempt.health_check` with:
- `passed: false`
- `http_status`: The last probe's status (or `None` if unreachable)
- `response_time_ms`: Milliseconds for the last probe

### Environment Variables

Resource limits on deployed containers (best-effort):
- `COXAGENT_DEPLOY_CPUS`: CPU limit per container (default: `2`)
- `COXAGENT_DEPLOY_MEM`: Memory limit per container (default: `1g`)

Set either to `off` to disable the limit.

Example:
```bash
export COXAGENT_DEPLOY_CPUS=4
export COXAGENT_DEPLOY_MEM=2g
cargo run  # All deployed containers limited to 4 CPUs, 2GB memory
```

---

## Interface

### DeployPort Trait Methods

#### `async fn health(&self, port: u16) -> Result<bool, PortError>`

TCP liveness check. Is something accepting connections on the port? Used by the ops monitor (background, after deploy) to detect crashes. Default: always true (no monitoring).

#### `async fn health_check(&self, port: u16) -> HealthCheckResult`

HTTP health probe. GET the app's root and capture diagnostics.

**Return type:**
```rust
pub struct HealthCheckResult {
    pub passed: bool,           // true if 1xx–4xx, false if 5xx or unreachable
    pub http_status: Option<u16>, // HTTP status if endpoint was reachable
    pub response_time_ms: Option<u64>, // Round-trip time in milliseconds
}
```

Default implementation wraps `health()` for adapters that don't override it.

#### `async fn wait_healthy(&self, port: u16, timeout: Duration) -> HealthCheckResult`

The polling gate. Poll `health_check()` every 2 seconds until it passes or `timeout` elapses. Guarantees: returns the probe that decided the outcome; never hangs indefinitely.

**Polling logic (simplified):**
```rust
let deadline = now() + timeout;
loop {
    let result = self.health_check(port).await;
    if result.passed {
        return result;
    }
    if now() + POLL_INTERVAL >= deadline {
        return result;
    }
    sleep(2 seconds).await;
}
```

### DockerComposeDeploy Implementation

**HTTP health check probe:**

```rust
async fn health_check(&self, port: u16) -> HealthCheckResult {
    let url = format!("http://127.0.0.1:{port}/");
    let start = Instant::now();
    
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))  // Per-probe HTTP timeout
        .build()
    {
        Ok(c) => c,
        Err(_) => return HealthCheckResult {
            passed: false,
            http_status: None,
            response_time_ms: None,
        }
    };
    
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
    /// Timeout for the health endpoint probe (COX-F005 cycle recording).
    pub health_check_timeout_secs: u64,
}
```

### Defaults

- `host_port`: `None` (no health check — app must opt in by configuring a port)
- `health_check_timeout_secs`: `60` seconds (cycle recording only; shared gate is always 30s)
- `auto_rollback`: `false` (opt-in; off by default)

### Configuration Examples

**Basic: Minimal Health Check**

```toml
[deploy]
host_port = 8101
enabled = true
auto_rollback = false
health_check_timeout_secs = 60  # Cycle recording timeout
```

Health check runs after every deploy via the 30-second shared gate. The cycle also records HTTP diagnostics (timeout 60s). Failures only file bugs — no automatic recovery.

**Production: Auto-Rollback Enabled**

```toml
[deploy]
host_port = 8101
enabled = true
auto_rollback = true
max_rollback_age_secs = 3600      # 1 hour
health_check_timeout_secs = 90    # Allow extra startup time for cycle recording
migration_detection_paths = ["db/migrations"]
```

Failed deploys roll back automatically if a known-good version exists within the last hour and no migrations have run since. Shared gate always uses 30s; cycle recording waits up to 90s.

---

## Edge Cases and Limits

### No Host Port Configured

If `config.deploy.host_port` is `None`:
- Health check is skipped entirely
- Deploy proceeds as if health check passed
- **Use case:** Projects that don't expose an HTTP port (e.g., background workers)

### Adapters Without HTTP Health

Test doubles and simple adapters that override only `health()` but not `health_check()`:
- `wait_healthy` uses the default `health_check` implementation
- Default wraps `health()` with timing, no HTTP status
- Deploy works but no HTTP-level diagnostics recorded

### Hang Guard

Both gates wrap `wait_healthy` with a timeout guard to catch wedged adapters:

```rust
// Shared gate (verify_deploy_health)
const HANG_GUARD: Duration = Duration::from_secs(10);
let result = tokio::time::timeout(30.secs() + HANG_GUARD, 
    deploy.wait_healthy(port, 30.secs())).await;

// Cycle recording (run_health_check)
const HANG_GUARD: Duration = Duration::from_secs(10);
let result = tokio::time::timeout(bound + HANG_GUARD, 
    deploy.wait_healthy(port, bound)).await;
```

If a custom adapter's `health_check` wedges (takes >5s without returning), the hang guard fires after the configured timeout, returning `passed: false` with no detail. Catches adapter bugs without losing diagnostics when adapters behave correctly.

### Never-Healthy Endpoint

If the endpoint never answers (connection refused, timeout, or persistent 5xx):
- Polling continues for the full timeout window
- Caller gets the *last* probe's detail, not a bare timeout
- **Useful for logs:** "probe 15 of 15 at 30s: connection refused" vs. "timeout"

### Auto-Rollback Constraints

Auto-rollback is skipped (failure filed as a bug instead) if:
- `config.deploy.auto_rollback` is `false` (default)
- No prior known-good deploy exists (first deploy ever)
- The known-good deploy is older than `max_rollback_age_secs` (default: 3600s = 1 hour)
- Any database migrations ran between the known-good and failed deployment

### Chat Deploy and PR Preview

The chat "deploy" command and PR-preview endpoint use the same 30-second shared gate (`verify_deploy_health`), but only record a pass/fail bool, not full HTTP diagnostics. This keeps those flows fast and predictable while still preventing silent deploy failures.

---

## Code Map

- `crates/application/src/ports/outbound/deploy.rs` — `DeployPort` trait, `wait_healthy()`, `health_check()` default, `verify_deploy_health()` shared gate
- `crates/application/src/state.rs` — `HealthCheckResult` structure
- `crates/infrastructure/src/deploy/docker_compose.rs` — `DockerComposeDeploy.health_check()` HTTP implementation
- `crates/application/src/use_cases/cycle.rs` — `run_health_check()` cycle recording, `verify_health_after_deploy()` rollback gate, health failure recording
- `crates/application/src/config.rs` — `DeployConfig.health_check_timeout_secs` configuration
- `crates/app/tests/health_gate.rs` — COX-B009 integration tests

---

## Related

- **COX-B004/B009:** Mandatory post-deploy health gate (`verify_deploy_health`)
- **COX-B001:** Auto-rollback to last known-good deploy on failure
- **RunCycleUseCase:** `crates/application/src/use_cases/cycle.rs`
- **PR preview endpoint:** Uses `verify_deploy_health` to gate deployments
- **Chat deploy command:** Uses `verify_deploy_health` to gate deployments
