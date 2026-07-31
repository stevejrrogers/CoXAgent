# COX-B004: Deploy Success Gate Never Checks Port Binding

**Keywords:** deploy, docker compose, health gate, port binding, liveness check, tcp, deployment failure detection, post-deploy verification

## Overview

The deploy success/rollback gate reports a deployment as successful when `docker compose up` exits with code 0. **The defect:** containers exiting 0 only proves they *started*, not that the application *inside* bound its configured port. An app can start, crash, or hang during initialization while the container dutifully reports success, and the gate passes it through.

**Who it affects:** Anyone deploying CoXAgent via `docker compose`. Dead-on-arrival deployments appear to succeed until a human tries to use the app and encounters connection refused.

**Root cause:** The deploy gate checked only the compose command's exit code, ignoring whether the running app accepted connections. A healthy container is not evidence of a healthy app.

**Fixed in:** COX-B004 added the `verify_deploy_health` gate; related regressions were discovered and fixed in COX-B009 and COX-B026.

---

## How It Works

The deploy lifecycle has two parts: starting containers and verifying the app inside is alive.

### Docker Compose Lifecycle

```
docker compose up ──> Containers START ──> App BINDS port ──> Connection accepted
                           ↑                      ↑
                      (exit 0)          (TCP liveness check)
```

Docker only guarantees the first step: containers started. It has no way to know if the app inside succeeded.

### The Health Gate (COX-B004)

After `docker compose up` exits 0, the gate polls the app's port for liveness:

```rust
pub async fn verify_deploy_health(deploy: &Arc<dyn DeployPort>, host_port: Option<u16>) -> bool {
    const ATTEMPTS: u32 = 15;
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    // Polls every 2 seconds for up to 30 seconds (15 attempts)
    for attempt in 0..ATTEMPTS {
        if deploy.health(port).await.unwrap_or(false) {
            return true;  // App is accepting connections
        }
        if attempt + 1 < ATTEMPTS {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
    false  // App never bound the port
}
```

**Why polling?** `docker compose up` returns seconds before the app finishes initialization and binds its port. A single immediate probe would false-fail healthy deploys. Polling waits for the app to become ready.

### Deployment Outcomes

| Scenario | Compose Exit | Port Accepts Connections | Gate Result | Outcome |
|----------|:---:|:---:|:---:|---|
| **App binds quickly** | 0 | ✓ on attempt 1 | Pass | Deploy succeeds immediately (no polling overhead) |
| **App binds slowly** (10s init) | 0 | ✓ on attempt 5 | Pass | Deploy succeeds after polling |
| **Port never accepts** (app crashes) | 0 | ✗ for all 15 attempts | **Fail** | Deploy marked failed; triggers rollback if enabled |
| **Port never accepts** (app hangs) | 0 | ✗ for all 15 attempts | **Fail** | Deploy marked failed; triggers rollback if enabled |
| **Health check itself errors** | 0 | Error (probe can't run) | **Fail** | Treat as unhealthy; don't wave through |

### Error Handling Philosophy

If the health check itself fails to run (e.g., probe logic errors), the gate treats it as unhealthy and fails the deploy. This prevents a broken health check from becoming a way to skip the gate. A gate that can fail silently is not a gate.

## Usage

Deploy the stack:

```bash
COXAGENT_ADMIN_PASSWORD=yourpw COXAGENT_DEPLOY_PORT=8101 docker compose up -d --build
```

The deployment process:

1. Runs `docker compose up -d --build`
2. If compose exits 0, polls `127.0.0.1:8101` every 2 seconds
3. On first successful connection, deploy succeeds
4. If no connection after 30 seconds, deploy fails
5. If auto-rollback is enabled and a prior healthy deploy exists, rolls back

Verify deployment succeeded:

```bash
curl -I http://localhost:8101
# HTTP/1.1 200 OK (or other 2xx/3xx/4xx; any response means the app bound the port)
```

## Interface

### DeployPort Trait Methods

**health() — TCP liveness check**

```rust
pub async fn health(&self, _port: u16) -> Result<bool, PortError> {
    // Default: no-op (returns true). Adapters override with actual TCP check.
}
```

**wait_healthy() — Polling probe (COX-F005)**

```rust
pub async fn wait_healthy(
    &self,
    port: u16,
    timeout: std::time::Duration,
) -> crate::state::HealthCheckResult
```

Returns HTTP status, response time, and pass/fail. Used for detailed diagnostics in deploy history.

### verify_deploy_health() — Shared Gate Function

```rust
pub async fn verify_deploy_health(
    deploy: &Arc<dyn DeployPort>,
    host_port: Option<u16>,
) -> bool
```

The mandatory gate every deploy call site must run through. Returns true only if the app accepts connections before the timeout.

**Deployment call sites that use this gate:**

- `crates/application/src/use_cases/cycle.rs` — autonomous deploy in the workflow cycle
- `crates/application/src/use_cases/run_chat_reply.rs` — chat's "deploy" command
- `crates/presentation/src/server.rs` — PR-preview endpoint deploy and rollback

All call sites are verified at compile time by `crates/app/tests/health_gate.rs`.

## Configuration

### Host Port Configuration

Every CoXAgent instance must declare a `host_port`:

```rust
pub struct DeployConfig {
    /// TCP port on which the deployed app should answer.
    pub host_port: Option<u16>,
    /// Whether deploy gates are enabled.
    pub enabled: bool,
    /// Maximum wait for health check (COX-F005).
    pub health_check_timeout_secs: u64,
}
```

**Default:** `host_port: None` (no health check runs; gate always passes)  
**Typical:** `host_port: Some(8101)` (poll `127.0.0.1:8101`)

### Environment Variables

Resource limits (optional, best-effort):

- `COXAGENT_DEPLOY_CPUS` — CPU limit per container (default: `2`)
- `COXAGENT_DEPLOY_MEM` — Memory limit per container (default: `1g`)
- `COXAGENT_DEPLOY_PORT` — Host port (default: read from config; if set via env, overrides config)

### Polling Behavior

These are hardcoded in `verify_deploy_health`:

- **Poll interval:** 2 seconds
- **Max attempts:** 15 (30 seconds total)
- **Timeout:** No timeout; either a port accepts or 30s elapses

## Edge Cases and Limits

### No Host Port Configured

If `host_port` is `None`, the gate always passes without probing:

```rust
let Some(port) = host_port else {
    return true;  // Nothing to check; deploy succeeds
};
```

This allows port-less projects (e.g., batch jobs, services that don't listen) to deploy without hanging. You must opt into port checking by setting `host_port`.

### Health Check Cannot Be Skipped

Every deploy call site is compile-time verified to call `verify_deploy_health` or a function that does (verified by `health_gate.rs` test). Adding a new deploy path that skips the gate causes CI to fail. This is by design — a gate that can be forgotten is one that will be.

### App That Takes Longer Than 30 Seconds to Start

The gate polls for 30 seconds (15 × 2s). If your app takes longer to bind:

1. It reports `false` after 30s
2. Deploy is marked failed
3. If auto-rollback is enabled, the prior deploy is restored
4. Increase polling time by adjusting `ATTEMPTS` in `verify_deploy_health` (rebuild required)

**Workaround:** Speed up app startup, or increase the polling budget. A 30s gate is chosen to balance waiting for slow startups against hanging forever on broken ones.

### Connection Refused vs. HTTP Error

| Response | Gate Verdict |
|----------|:---:|
| Connection refused, timeout, DNS error | Fail (app hasn't bound port) |
| HTTP 5xx (Internal Server Error) | Fail (app crashed after binding) |
| HTTP 2xx/3xx/4xx (app responds) | Pass (app is up) |

The detailed HTTP-level probe (COX-F005) distinguishes these; the TCP liveness gate (COX-B004) sees only "port accepts connections" or "doesn't".

### Health Check on Localhost Only

The gate checks `127.0.0.1:<host_port>`. Remote clients must use DNS or an exposed IP. If your firewall or network layer blocks localhost loopback, the health check will fail even if the app is running (unlikely, but possible in container-in-VM scenarios).

### Probe Error = Deploy Fails

If the health check itself errors (e.g., a bug in the probe logic), the gate treats it as unhealthy:

```rust
if deploy.health(port).await.unwrap_or(false) {
    return true;  // Only passes if health() returns Ok(true)
}
// Ok(false), Err(...), and timeout all fail the gate
```

This is intentional — a broken gate should not silently wave deploys through.

## Code Map

- `crates/application/src/ports/outbound/deploy.rs` — `verify_deploy_health()` gate, `wait_healthy()` probe, `DeployPort` trait definition
- `crates/app/tests/health_gate.rs` — Compile-time verification that every deploy call site runs through the gate (COX-B009 regression test)
- `crates/infrastructure/src/deploy/docker_compose.rs` — Docker Compose adapter; implements `DeployPort::health()` with actual TCP liveness check
- `crates/application/src/use_cases/cycle.rs` — Autonomous deploy; gates success on `verify_deploy_health`
- `crates/application/src/use_cases/run_chat_reply.rs` — Chat "deploy" command; gates success on `verify_deploy_health`
- `crates/presentation/src/server.rs` — PR-preview endpoint; gates both deploy and rollback on health checks
- `docker-compose.yml` — Container configuration; depends on adapter's `health()` method to probe the running app

## Related

- [COX-B001](COX-B001.md) — Docker port mapping defect (app on wrong port); COX-B004 gates *whether the app bound any port*, not which one
- [COX-B009](COX-B009.md) — Regression where chat "deploy" and PR-preview bypassed the COX-B004 gate; now wired on all call sites
- [COX-B026](COX-B026.md) — PR-preview health gate could skip on negative or floating-point port values; now validated
- [COX-F005](COX-F005.md) — Detailed post-deploy health endpoint probe; captures HTTP status and response time, runs after COX-B004 passes
- [COX-B018](COX-B018.md) — Single immediate probe would false-fail slow startups; polling is the solution
- [DEPLOYMENT.md](DEPLOYMENT.md) — Deployment architecture and health gate orchestration
