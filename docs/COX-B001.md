FOLDER: -

# COX-B001: Docker Port Binding

**Keywords:** docker, port binding, docker-compose, host port, deployment, app unreachable, 8101, 4000

## Overview

The self-hosted Docker deployment (docker-compose) exposes the CoXAgent application to external clients via a host port. COX-B001 was a deployment bug where `docker-compose.yml` mapped the container's internal port (4000) to the wrong host port (also 4000), when it should have mapped to port 8101. This made the app unreachable at `http://localhost:8101`, the documented entry point. The symptom was "App is DOWN — no response on port 8101" despite `docker compose up` succeeding.

**Who it affects:** Anyone deploying CoXAgent via `docker compose up` for self-hosting or development.

**Root cause:** Incorrect port mapping in `docker-compose.yml` — the `coxagent` service had `ports: ["4000:4000"]` instead of `ports: ["8101:4000"]`.

**Fixed in:** [PR #2](https://github.com/anthropics/coxagent/pull/2) on 2026-07-26.

---

## How It Works

The Docker networking stack operates in layers:

1. **Container internal binding:** The CoXAgent application binds to `0.0.0.0:4000` inside its container (`crates/presentation/src/server.rs`). This is the hardcoded app port.

2. **Port mapping:** `docker-compose.yml` defines a `ports` directive that maps a host port to the container port. Format: `"<host_port>:<container_port>"`.

3. **Client connection:** External clients (browser, curl, other services) connect to `http://localhost:<host_port>`. Docker's port-forwarding kernel module routes this traffic to the container's mapped port.

### Broken State (Before COX-B001 Fix)

```
Host: localhost:4000 ──docker port-forward──> Container: 0.0.0.0:4000 ✓
Host: localhost:8101 ──[no mapping]──> ✗ Connection refused
```

Users saw:
```bash
$ curl http://localhost:8101
curl: (7) Failed to connect to localhost port 8101: Connection refused
```

### Fixed State (After COX-B001 Fix)

```
Host: localhost:8101 ──docker port-forward──> Container: 0.0.0.0:4000 ✓
```

Users now see:
```bash
$ curl http://localhost:8101
<!DOCTYPE html>
<html>
  <!-- app responds normally -->
</html>
```

## Usage

Deploy the stack:

```bash
COXAGENT_ADMIN_PASSWORD=yourpw docker compose up -d --build
```

Access the application:

```
http://localhost:8101
```

Log in as `root` with the password specified above.

To verify the app is listening:

```bash
curl -I http://localhost:8101
```

## Interface

### docker-compose.yml (services.coxagent)

```yaml
services:
  coxagent:
    build: .
    depends_on:
      db:
        condition: service_healthy
    environment:
      COXAGENT_DB_DSN: postgres://postgres:${PG_PASSWORD:-coxagent_dev}@db:5432/coxagent
      COXAGENT_ADMIN_USER: root
      COXAGENT_ADMIN_PASSWORD: ${COXAGENT_ADMIN_PASSWORD:-changeme}
    ports:
      - "8101:4000"   # Maps host port 8101 → container port 4000
    volumes:
      - workspace:/workspace
```

The `ports` directive is the critical line. It must match the app's internal port (4000) and expose it on the expected host port (8101).

### README.md (Deployment Instructions)

```markdown
# Self-host CoXAgent: the hub + dashboard, backed by Postgres

Bring up with:
    COXAGENT_ADMIN_PASSWORD=yourpw docker compose up -d --build

then open http://localhost:8101 and log in as root.

# Host port is fixed at 8101 (this project's assigned deploy.host_port)
# so it never collides with a live hub bound to 4000 on the same docker host.
```

## Configuration

### Port Assignment

Port 8101 is this project's hardcoded `deploy.host_port`. Changing it requires updating:

1. **docker-compose.yml** — the `coxagent` service `ports` entry
2. **README.md** — the deployment instruction URL
3. **Any derived or override compose files** — if your CI/CD or multi-stage deploy uses a separate compose override, update it there too

### Why 8101?

Port 8101 is explicitly reserved to avoid collision with a live hub instance running on port 4000 on the same Docker host. If two services on the same host try to bind the same port, the second fails with `port is already allocated`. Port 8101 provides safe separation.

### Port Mapping Format

Docker Compose port mappings use the syntax `"<host_port>:<container_port>"`:

- **host_port:** The port clients use to reach the app (from outside the container)
- **container_port:** The port the app listens on inside the container (must match app's hardcoded bind port)

Wrong: `"4000:4000"` maps both to 4000 (app unreachable on 8101)  
Correct: `"8101:4000"` maps host 8101 → container 4000 (app reachable at 8101)

## Edge Cases and Limits

### Port Already in Use

If another process (another CoXAgent instance, test server, etc.) already holds port 8101, `docker compose up` fails:

```
Error response from daemon: driver failed programming external connectivity on endpoint...
Bind for 0.0.0.0:8101 failed: port is already allocated.
```

**Resolution:**
1. Kill the process holding port 8101, or
2. Choose a different host port by editing `docker-compose.yml` and README, or
3. Use `docker compose down` to stop a prior CoXAgent stack on the same port

### Localhost vs. Service DNS

Within the Docker network:

| Where | Address | Port |
|-------|---------|------|
| **From host (browser, curl)** | `http://localhost:8101` or `http://127.0.0.1:8101` | Host port (8101) |
| **From another container in same network** | `http://coxagent:4000` | Internal port (4000), no mapping needed |

Containers see each other by service name; the port mapping only applies to host→container traffic, not container→container.

### Firewall

If the host has a firewall (macOS, Linux `ufw`, Windows Defender, cloud security groups), port 8101 must be allowed for external access. Connections to `localhost` from the same machine bypass firewall rules; remote clients may not.

### Multiple Stacks on the Same Host

If you need to run multiple CoXAgent instances on one Docker host:

- Each must use a different host port (e.g., 8101, 8102, 8103)
- Each must have its own database volume and credentials
- Update docker-compose.yml and README separately for each instance

Alternatively, use separate Docker networks to isolate services completely.

## Code Map

- `docker-compose.yml` — Service definitions; the `coxagent` service's `ports` key defines the host→container port mapping
- `README.md` — User-facing deployment instructions; documents `http://localhost:8101` as the entry point
- `crates/presentation/src/server.rs` — TCP listener bind; runs `tokio::net::TcpListener::bind()` on the configured address (line 2399)
- `crates/app/src/lib.rs` — Port configuration; the `serve` command path determines the port from CLI args or config (lines 164-165, 826)
- `.github/workflows/` — CI jobs may use docker-compose or derived images; they reference the default `docker-compose.yml` unless overridden

## Related

- [COX-B004](COX-B004.md) — Post-deploy health gate (TCP liveness check); verifies app actually bound its port after container starts
- [COX-B011](COX-B011.md) — README Docker self-host instructions pointed to wrong port after COX-B001 fix; a guard was added to prevent similar errors
- [COX-F005](COX-F005.md) — Pre-deploy health check and detailed probe; complements COX-B004 with HTTP diagnostics
- [DEPLOYMENT.md](DEPLOYMENT.md) — Three deploy shapes (macOS app, docker-compose, Kubernetes/Helm) and their port assignment rules
- [docker-compose.yml](../docker-compose.yml) — Live configuration file
