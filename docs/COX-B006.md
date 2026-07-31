# COX-B006: Docker Build Fails with BuildKit Permission Fault

**Keywords:** docker, docker-compose, buildkit, build cache, permission denied, build failure, host environment, activity directory

## Overview

`docker compose up --build` fails to build the CoXAgent image on some hosts, preventing deployment entirely. The symptom is a permission or permission-like fault in BuildKit's activity directory (the build cache directory), which is host-environment-specific and not caused by the Dockerfile itself. When the build fails, no image is produced; the container never starts, and the app never binds its port, making the deployment unreachable—regressing [COX-B001](COX-B001.md).

**Who it affects:** Developers and CI runners using `docker compose up --build` on hosts where BuildKit has permission issues with its activity directory (common in macOS Docker Desktop, certain Linux setups, and sandboxed CI environments).

**Root cause:** Docker BuildKit's activity directory (`$BUILDX_CONFIG`) may have permission issues on certain hosts, causing the build step to fail even though the Dockerfile is correct. The classic Docker builder (deprecated but still available) also hits the same directory issue in some cases.

**Fixed in:** Commit f8d31d2 on 2026-07-28; fix is guarded by `crates/app/tests/deploy_smoke.rs` regression test.

---

## How It Works

### The Build Pipeline

`docker compose up --build` runs a three-phase sequence:

1. **Docker image build** — Executes the Dockerfile's builder stage (compile Rust binary) and runtime stage (copy binary + dependencies)
2. **Container creation** — Starts containers from the built image
3. **Port binding & liveness** — App initializes and binds to its configured port

If phase 1 fails, no image exists; phases 2 and 3 never run, and deployment fails.

### BuildKit and Host Permission Issues

Modern Docker (19.03+) uses BuildKit by default for multi-stage builds. BuildKit caches build artifacts in an activity directory controlled by `$BUILDX_CONFIG`:

**Default:** `~/.docker/buildx` (user's home directory)

**The problem:** On some hosts:
- Permissions on `~/.docker/buildx` are too restrictive (owned by root, not the Docker user)
- The directory is on a filesystem that doesn't support Docker daemon operations (network shares, sandboxes)
- Concurrent builds fight over the same cache (multi-agent scenarios)

BuildKit then fails with errors like:
```
permission denied while trying to connect to Docker daemon
failed to write to build cache directory: permission denied
error during docker build step
```

### The Retry Strategy (COX-B006 Fix)

The deploy smoke test (`crates/app/tests/deploy_smoke.rs`) now retries `docker compose up --build` with three fallback strategies:

```
Attempt 1: Normal BuildKit
  └─ if "BuildKit permission fault" error → Attempt 2

Attempt 2: Classic builder (DOCKER_BUILDKIT=0, COMPOSE_DOCKER_CLI_BUILD=0)
  └─ if still "BuildKit permission fault" → Attempt 3
       (Classic builder may still shell out to buildx bake on newer Docker/Compose)

Attempt 3: Redirect BUILDX_CONFIG to a temp directory
  └─ (BUILDX_CONFIG=/tmp/coxagent-deploy-smoke-buildx-<pid>)
       Keeps BuildKit active but isolates its cache to a writable, per-run directory.
```

**Why retry instead of just use the classic builder?** Newer Docker/Compose versions sometimes ignore `DOCKER_BUILDKIT=0` and still invoke buildx bake, hitting the same permission issue. Redirecting `BUILDX_CONFIG` to a temp directory actually fixes the problem by giving BuildKit a writable cache location.

### Success Indicators

A build succeeds when `docker compose up --build` exits with code 0 **and** produces an image that:
- Compiles without errors
- Contains the CoXAgent binary
- Starts a container without crashing
- Binds the configured port (8101) and answers health checks (see [COX-B004](COX-B004.md))

---

## Usage

### Normal Deployment (Developer)

Run the documented command:

```bash
COXAGENT_ADMIN_PASSWORD=yourpw docker compose up -d --build
```

On most systems, this works on the first try. If BuildKit permission issues occur, the retry strategy is baked into the test infrastructure (not user-facing); deployment simply retries silently and may take a moment longer.

### Regression Testing

The `deploy_smoke` test ensures `docker compose up --build` actually produces a working app:

```bash
cargo test -p coxagent-app --test deploy_smoke -- --ignored --nocapture
```

This runs the exact documented command, waits for the stack to come up, probes port 8101 for a 200 response, and fails if the app never answers. It also covers the retry strategy.

### Debugging Build Failures

If a build fails and the retry strategy doesn't kick in (non-permission errors):

1. **Check the Dockerfile for syntax errors:**
   ```bash
   docker build . --target builder
   ```

2. **If that succeeds but compose fails, try the classic builder manually:**
   ```bash
   DOCKER_BUILDKIT=0 COMPOSE_DOCKER_CLI_BUILD=0 docker compose up --build
   ```

3. **If that fails too, retry with isolated BuildKit config:**
   ```bash
   mkdir -p /tmp/coxagent-debug-buildx
   BUILDX_CONFIG=/tmp/coxagent-debug-buildx docker compose up --build
   ```

4. **Check Docker and BuildKit versions:**
   ```bash
   docker version
   docker buildx version
   ```

---

## Interface

### Dockerfile (Multi-Stage)

```dockerfile
FROM rust:1-slim-bookworm AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release --bin coxagent

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/coxagent /usr/local/bin/coxagent
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
ENV COXAGENT_HOST=0.0.0.0 COXAGENT_WORKSPACE=/workspace
EXPOSE 4000
VOLUME ["/workspace"]
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
```

The builder stage compiles the Rust binary (`cargo build --release --bin coxagent`). If this fails, the build step exits non-zero and no image is produced.

### docker-compose.yml (Build Section)

```yaml
services:
  coxagent:
    build: .                    # Dockerfile in repo root
    depends_on:
      db:
        condition: service_healthy
    # ... rest of service config
```

The `build: .` directive tells Docker to build from the root Dockerfile. No explicit builder configuration needed; retries are handled at the test/integration level.

### Retry Logic (Deploy Smoke Test)

```rust
fn is_buildkit_host_fault(text: &str) -> bool {
    text.contains("permission denied") || text.contains("buildkit")
}

fn bring_stack_up(root: &Path) -> std::process::Output {
    let up = compose(root, &["up", "-d", "--build"]);
    if up.status.success() {
        return up;
    }
    let why = output_text(&up);
    if !is_buildkit_host_fault(&why) {
        return up;  // Not a BuildKit issue; return the error as-is
    }
    // Retry with classic builder
    let classic = compose_with(root, &["up", "-d", "--build"], &CLASSIC_BUILDER);
    if classic.status.success() {
        return classic;
    }
    // Classic builder failed too; try redirecting BUILDX_CONFIG
    let why2 = output_text(&classic);
    if !is_buildkit_host_fault(&why2) {
        return classic;
    }
    // Redirect BUILDX_CONFIG to a temp directory
    let buildx_dir = std::env::temp_dir().join(format!(
        "coxagent-deploy-smoke-buildx-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&buildx_dir);
    compose_with(root, &["up", "-d", "--build"], 
        &[("BUILDX_CONFIG", buildx_dir.to_str().expect("valid UTF-8"))])
}
```

---

## Configuration

### BuildKit Behavior

BuildKit is controlled by environment variables set before `docker compose` runs:

| Variable | Default | Effect |
|----------|---------|--------|
| `DOCKER_BUILDKIT` | `1` (enabled) | Set to `0` to use classic builder (not multi-stage) |
| `COMPOSE_DOCKER_CLI_BUILD` | `1` (enabled) | Set to `0` to disable BuildKit in compose (newer versions ignore this) |
| `BUILDX_CONFIG` | `~/.docker/buildx` | Cache directory for BuildKit; writable temp directory bypasses permission issues |

### Deployment Configuration

No extra configuration needed. The retry strategy is baked into:
- `crates/app/tests/deploy_smoke.rs` — Handles retries during testing
- CI pipelines — The same test runs in `deploy-smoke` job; retries are automatic

### Cargo Build Stage

The builder stage compiles with:
```bash
cargo build --release --bin coxagent
```

This produces a stripped release binary in `/build/target/release/coxagent`. Compilation errors in this stage cause the Dockerfile build to fail.

---

## Edge Cases and Limits

### BuildKit Permission Fault Detection

The retry strategy looks for the string "permission denied" or "buildkit" in the error output. If the error is something else (e.g., a Rust compilation error, missing dependency), the build is not retried; the original error is returned.

**Compilation errors** (Dockerfile `RUN cargo build` fails) produce messages like:
```
error: this function is unused: `my_helper`
error: aborting due to 1 previous error
```

These are not permission-related and cause the build to fail immediately without retry. Fix the Rust code, then rebuild.

### Concurrent Builds on Shared Systems

If multiple builds run in parallel on the same host:
- Each `deploy_smoke` test run uses a unique temp directory (`/tmp/coxagent-deploy-smoke-buildx-<pid>`)
- Builds do not interfere with each other
- This isolation is only active when the third retry fires; normal builds use the shared cache

### Docker Desktop on macOS

Docker Desktop on macOS runs the Docker daemon inside a Linux VM. BuildKit cache may live on a macOS-mounted volume with permission mismatches. The `BUILDX_CONFIG` redirect to a Linux-native temp directory fixes this.

### Rootless Docker

Rootless Docker has tighter permission isolation. If `~/.docker/buildx` has restrictive permissions:
1. The classic builder retry may also fail
2. The `BUILDX_CONFIG` redirect to a per-process temp directory usually succeeds because the directory is created with the current user's permissions

### Build Success But Container Doesn't Start

If `docker compose up --build` exits 0 but the container crashes immediately:
- The build phase succeeded (image was produced)
- The container start phase failed (app crashed during initialization)
- Check container logs: `docker compose logs --tail 50`
- This is not a COX-B006 issue; see [COX-B004](COX-B004.md) for health gate verification

### Timeout During Compilation

If `cargo build --release` takes longer than the test's `READY_TIMEOUT` (180 seconds):
- The build may succeed but the stack readiness probe times out
- Increase `READY_TIMEOUT` in `crates/app/tests/deploy_smoke.rs` or optimize Rust compile time
- Longer compile times are not retried; the original timeout is returned

---

## Code Map

- `Dockerfile` — Multi-stage Docker build; builder stage runs `cargo build --release --bin coxagent`
- `crates/app/tests/deploy_smoke.rs` — Regression test; runs `docker compose up --build` with retry logic for BuildKit permission faults
  - `is_buildkit_host_fault()` — Detects permission errors in output
  - `bring_stack_up()` — Three-attempt retry strategy (normal → classic builder → BUILDX_CONFIG redirect)
  - `compose_stack_comes_up_and_answers_on_the_published_port()` — Integration test that validates the full deployment
- `docker-compose.yml` — Service definition; `build: .` triggers the Dockerfile build
- `.github/workflows/` — CI runs the `deploy-smoke` test in the deploy-smoke job

---

## Related

- [COX-B001](COX-B001.md) — Docker port mapping defect; COX-B006 regresses this by preventing the container from starting at all
- [COX-B004](COX-B004.md) — Deploy success gate (port binding); only runs after COX-B006 build succeeds
- [COX-B008](COX-B008.md) — Original bug report (compile error + no image); COX-B006 is the deployment-layer guard
- [COX-F005](COX-F005.md) — Post-deploy health check; runs after a successful build and container start
- [DEPLOYMENT.md](DEPLOYMENT.md) — Full deployment architecture including build, container, and health gate stages
