FOLDER: Deployment

# Deploy Secret Rotation vs Persistence (CXA-B030)

**Keywords:** deploy secrets, secret rotation, secret persistence, out-of-tree store, PG_PASSWORD, COXAGENT_ADMIN_PASSWORD, unconfigured deploy, app-driven deploy, source-tree safety, stable secrets

## Overview

CXA-B030 reconciles two earlier tickets that pull against each other for unconfigured app-driven deploys. CXA-B028 forbids persisting generated superuser credentials anywhere inside a project's source tree (where agents read diffs from and commit from). Taken literally — "persist nothing at all" — that reintroduces CXA-B027's original bug: with no retained value from last cycle, every cycle mints a *fresh* random secret per required key (per-cycle rotation), which breaks Postgres auth and admin login against an already-initialized named data volume. This page is for anyone working on the docker-compose deploy adapter or reconciling the durability/no-source-persistence tension.

## How it works

The decision lives in `crates/infrastructure/src/deploy/docker_compose.rs`, invoked once per pass from `DockerComposeDeploy::deploy(work_dir)`, which calls `resolve_deploy_secrets(work_dir)` before building either `docker compose up -d --build` command (the initial run and any port-eviction retry).

`resolve_deploy_secrets` → `resolve_deploy_secrets_in(work_dir, secret_root)` applies one precedence rule per key in `REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]`, computed by `missing_required_secrets(...)`:

1. **Process env** — a non-blank value for the key leaves it alone.
2. **Project-dir `.env`** — a non-blank assignment parsed by `read_dot_env()` leaves it alone.
3. **Fallback** — only keys missing from both sources get a value: read back from this project's durable *out-of-tree store*, or freshly minted by `random_secret()` and persisted there.

The point of CXA-B030 is how step 3 satisfies both constraints at once:

- **Persistence is mandatory but strictly out-of-tree.** Fallback values are written to `${COXAGENT_DEPLOY_SECRETS_DIR}` (default `<HOME>/.local/share/coxagent/deploy-secrets`) as `<digest>.env`, atomically via temp-sibling + rename (`persist_store`) and hardened to owner-only (`set_secret_perms`, 0o600). They are never written back into `<project>/codebase/.env`; resolution only *reads* `.env` to detect operator config (`read_dot_env`) — CXA-B028's guard.
- **Retained values kill rotation.** Because step 3 first checks the out-of-tree store (`stored.get(key)`), an unconfigured project reuses last cycle's exact value instead of regenerating — so pgdata initialized with cycle N's password still matches on cycle N+1 (CXA-B031 stability).
- **Removing persistence would resurrect B027.** If resolution stopped persisting entirely ("B028 means no persistence at all"), then nothing remembers last cycle's value and every pass falls through to a fresh `random_secret()` — exactly B027's per-cycle rotation breaking auth on initialized volumes.

Keying follows docker's named-volume semantics: the store filename is SHA-256 of `compose_project_name(work_dir)` (basenames only → invariant under relocation), matching how docker persists pgdata in `<project>_db`. Legacy path-keyed files (`cxb031_store_file`) are adopted via ownership-stamped merge (`adopt_legacy_cxb031_secrets`, gated by CXA-B043) and retired only after durable persist (`expire_converged_legacy_store` / `retire_superseded_path_keyed_store`, CXA-B044 ordering).

Values reach compose as transient child-process env only via `seed_deploy_secrets(cmd, &secrets)`; they are never written to any file inside the work dir.

## Usage

There is no user-facing flag; stability-plus-no-source-persistence is automatic inside every app-driven deploy once any required key is absent everywhere.

```sh
# Watch resolution work against an isolated store without touching HOME:
export COXAGENT_DEPLOY_SECRETS_DIR=/tmp/demo-secrets
cd <an-unconfigured-project-with-a-compose-file>

# Run two deploy cycles; see what was stored OUT of tree:
ls -la "$COXAGENT_DEPLOY_SECRETS_DIR"
cat "$COXAGENT_DEPLOY_SECRETS_DIR"/*.env   # first line "# owner=..." then KEY=VALUE lines

# Confirm nothing landed in source:
ls <project-dir>/.env   # must not exist after resolution (CXA-B028)
```

To pin your own credentials instead of relying on generated/stored ones:

```bash
export PG_PASSWORD='your-pg-secret'
export COXAGENT_ADMIN_PASSWORD='your-admin-secret'
coxagent <deploy-driving command> <project>

# or place both in <project-dir>/.env:
#   PG_PASSWORD=your-pg-secret
#   COXAGENT_ADMIN_PASSWORD=your-admin-secret
```

An operator-supplied value always wins verbatim; nothing seeds or resolves for keys you supply.

## Interface

All identifiers live in crates/infrastructure/src/deploy/docker_compose.rs unless noted:

- const REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]
- fn random_secret() -> String — 32 chars from a charset dropping look-alikes (`0/O/1/l/I`) and YAML/DSN-breaking characters; never baked constants
- fn resolve_deploy_secrets(work_dir) -> Vec<(String,String)> — thin wrapper over resolve_deploy_secrets_in using default root
- fn resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path) -> Vec<(String,String)> — single-pass precedence + retention + persist + legacy adoption/expiry; never writes into work dir
- fn missing_required_secrets(provided_by_env: impl Fn(&str)->bool, dot_env_keys: &HashSet<String>) -> Vec<&'static str> — pure precedence rule
- fn seed_deploy_secrets(cmd: &mut tokio::process::Command, secrets: &[(String,String)]) — feeds resolved values into compose as process env only
- fn read_dot_env(path) -> HashSet<String> / read_stored_secrets(path) -> HashMap<String,String> / read_owner_stamp(path) -> Option<String>
- fn deploy_secrets_root() -> PathBuf / store_file(...) / cxb031_store_file(...)
- fn adopt_legacy_cxb031_secrets(...); retire_superseded_path_keyed_store(...); expire_converged_legacy_store(path)
- fn write_stored_secrets_owned(path, owner: Option<&str>, secrets) -> bool; persist_store(...); set_secret_perms(path); repair_store_permissions(...)
- Consumer: DockerComposeDeploy::deploy() (initial up + port-eviction retry)

Regression tests live in mod deploy_secret_tests (~line 1713): resolving_deploy_secrets_never_persists_them_to_work_dir_dot_env (~line 1859), precedence_honours_process_env_then_dot_env_then_fallback (~line 1744), generated_charset_excludes_similar_lookalikes (~line 1804).

## Configuration

| Setting | Default | Effect |
|---|---|---|
| Required key present in process env OR project-dir `.env` | n/a | Honoured verbatim; never overridden or poisoned |
| Required key absent everywhere | n/a | Stable stored value reused if already persisted out-of-tree; else minted once then persisted there |
| COXAGENT_DEPLOY_SECRETS_DIR | `<HOME>/.local/share/coxagent/deploy-secrets` (temp dir when HOME unset) | Root directory for durable per-project fallback stores; tests/sandboxes point this at isolated dirs |
| Store file permissions | 0o600 after write / repair | Credentials at rest are owner-only |
| Store ownership stamp | first line "# owner=<compose-project-name>" | Refuses adoption of credentials authored by another project sharing this dir |

No CLI flags or config-file knobs were added for B030 itself; behaviour is driven entirely by which values are present plus these env settings.

## Edge cases and limits

What this deliberately does NOT do:

- It never writes generated secrets back into `<project>/codebase/.env` or anywhere inside the source tree agents read diffs from / commit from (CXA-B028 guard).
- It does not rotate per cycle when nothing is configured — retention removes rotation.
- It does not clobber operator-configured values.
- It will not adopt or delete a legacy store owned by a differently-named project sharing this directory.

How it fails / known limits:

- Store persistence is best-effort ([CXA-B044]): if atomic rename fails before completion it returns non-durable so no partial content lingers; callers treat non-true as "nothing durable exists yet".
- Legacy deletion happens only after durable persist succeeds; otherwise the old file stays so next pass re-adopts rather than losing both stores.
- Keying follows docker's named-volume semantics via basenames only (`cox-parent-dir`) — genuinely distinct projects keep separate files even when base dirs resemble each other.
- If every required secret resolves externally resolution returns early with nothing seeded ([CXA-B040] superseded stores are still retired separately).
- The retention guarantee exists ONLY while persistence succeeds; an unwritable store silently degrades toward per-pass regeneration ([B044] best-effort contract).

## Code map

— crates/infrastructure/src/deploy/docker_compose.rs —
DockerComposeDeploy adapter body containing ALL resolution/persistence logic above plus deploy(), compose_project_name(), apply_resource_limits(), running_services() and port-eviction helpers extract_bind_port / compose_project_on_port / container_on_port / evictable_project(). Unit-test module mod deploy_secret_tests (~line 1713).

— crates/app/src/builders.rs —
build_auth() reads COXAGENT_ADMIN_USER / COXAGENT_ADMIN_PASSWORD and calls bootstrap_admin(); admin-login half of whatever stable password a deploy seeds.

— crates/infrastructure/src/auth.rs —
FileAuthService::bootstrap_admin(path, username, password) persists the admin account into auth.json (/ SqlAuthService variant).

— crates/app/tests/compose_security_gate.rs —
Guards compose interpolation/security behaviour around ${VAR} usage for these required keys ({$VAR}? markers).

— crates/app/tests/committed_secrets_gate.rs —
Anti-regression gate preventing committed source/literals from carrying ADMIN_PASSWORD-style values again.

— crates/app/tests/deploy_smoke.rs —
Boot-level smoke test of this repo’s own compose stack.

## Related

- wiki Engineering → Deployment → deploy-secret-stability.md (CXA-B027) — the incident this ticket guards against returning: per-cycle rotation breaking auth on initialized volumes. Overlap in HOW it works; B030 frames it as the B028-vs-B027 tension and its out-of-tree resolution.
- wiki Engineering → Deployment → deployment.md (CXA-B017) — the honour-or-generate precedence rule (`${VAR:?}` interpolation) that both B027 and B030 build on; distinct focus on operator-supplied vs generated values here.
- wiki Engineering → Deployment → docker-compose-deploys.md (CXA-B001) — end-to-end deploy lifecycle, health gate, rollback and failure-ticket format on this same adapter.
- CXA-B028 — no persistence of generated secrets into the source tree (the constraint whose over-reading would re-open B027).
- CXA-B031/B032/B036/CXA-B038/CXA-B039/CXA-B040/CXA-B043/CXA-B044 — durability, key-derivation, permission and adoption hardening landed after B030 to make generated secrets stable out-of-tree without ever persisting into source.
