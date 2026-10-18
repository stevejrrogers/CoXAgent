FOLDER: -

# Deploy Secret Rotation vs Stability (CXA-B031)

**Keywords:** per-cycle secret rotation, deploy secrets, stable secrets, PG_PASSWORD, COXAGENT_ADMIN_PASSWORD, unconfigured deploy, app-driven deploy, resolve_deploy_secrets_in, out-of-tree store, pgdata volume auth failure

## Overview

CXA-B031 is the code ticket that makes auto-generated superuser secrets **stable across deploy cycles** for unconfigured app-driven deploys — i.e. it stops per-cycle secret rotation. CXA-B030 shipped no code; this ticket is what actually lands the fix that closes CXA-B027's regression (fresh random secrets every pass breaking Postgres auth and admin login against an already-initialized named data volume). It is for anyone working on the docker-compose deploy adapter or debugging DB/admin auth failures on agent-deployed stacks.

## How it works

The decision lives in `crates/infrastructure/src/deploy/docker_compose.rs`. Each pass calls `DockerComposeDeploy::deploy(work_dir)` (`fn deploy`, ~line 1142), which computes secrets ONCE via `resolve_deploy_secrets(work_dir)` → `resolve_deploy_secrets_in(work_dir, secret_root)` before building either compose command (the initial run and any port-eviction retry).

`resolve_deploy_secrets_in` applies one precedence rule per key in `REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]`, computed by the pure helper `missing_required_secrets(provided_by_env, dot_env_keys)`:

1. **Process env** — a non-blank value leaves that key alone.
2. **Project-dir `.env`** — a non-blank assignment parsed by `read_dot_env()` leaves it alone.
3. **Fallback (this ticket's path)** — only keys missing from BOTH sources get a value: reuse this project's durable out-of-tree store (`stored.get(key)`), or freshly minted by `random_secret()` and persisted there.

The stability guarantee is the fallback loop (docker_compose.rs:131-138):

```rust
for key in missing {
    let value = match stored.get(key) {
        Some(existing) => existing.clone(), // B031: reuse last cycle's exact value
        None => random_secret(),
    };
    stored.insert(key.to_owned(), value.clone());
    resolved.push((key.to_owned(), value));
}
```

Because step 3 checks the store BEFORE generating, an unconfigured project reuses last cycle's exact value instead of regenerating — so pgdata initialized with cycle N's password still matches on cycle N+1. Retention removes rotation; removing retention would resurrect CXA-B027.

Keying follows docker's named-volume semantics: the store filename is SHA-256 of `compose_project_name(work_dir)` (basenames only → invariant under relocation), matching how docker persists pgdata in `<project>_db`. Fallback values are written OUT of tree to `${COXAGENT_DEPLOY_SECRETS_DIR}` as `<digest>.env`, atomically via temp-sibling + rename (`persist_store`) and hardened to owner-only (`set_secret_perms`, 0o600) — never into `<project>/codebase/.env` (CXA-B028 guard). Legacy path-keyed files are adopted via ownership-stamped merge (`adopt_legacy_cxb031_secrets`, gated by CXA-B043) and retired only after durable persist (`expire_converged_legacy_store` / `retire_superseded_path_keyed_store`, CXA-B044 ordering).

Values reach compose as transient child-process env only through `seed_deploy_secrets(cmd, &secrets)`; they are never written to any file inside the work dir.

## Usage

There is no user-facing flag; stability-plus-no-source-persistence is automatic inside every app-driven deploy once any required key is absent everywhere.

```sh
# Watch resolution work against an isolated store without touching HOME:
export COXAGENT_DEPLOY_SECRETS_DIR=/tmp/demo-secrets
cd <an-unconfigured-project-with-a-compose-file>

# Run two deploy cycles; confirm one stable value was persisted OUT of tree:
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
