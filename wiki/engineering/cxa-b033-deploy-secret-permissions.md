FOLDER: Deployment

# Persisted Deploy Secrets and 0600 Permissions (CXA-B033)

**Keywords:** deploy secrets, file permissions, chmod, 0600, 0644, world-readable, umask, set_secret_perms, repair_store_permissions, persist_store, PG_PASSWORD

## Overview

CXA-B033 reports that generated superuser credentials persisted by `DockerComposeDeploy` were written world-readable (`0644`) even though the code appeared to chmod them `0600`. The root cause was not a missing chmod call but a call that silently did nothing on disk: `set_secret_perms` mutated a *copy* of the permissions object returned by `std::fs::metadata()` without writing it back via `set_permissions`, so every freshly-persisted secret file kept whatever mode the process umask produced (typically `0644`). This page is for anyone working on deploy-secret persistence or security remediation in the docker-compose deploy adapter.

## How it works

Deploy secrets are persisted out-of-tree by `persist_store()` in [crates/infrastructure/src/deploy/docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs). The write path is:

1. Build the dot-env body (optional owner-stamp first line + one `KEY=VALUE` per line).
2. Write to a **temp sibling** (`<name>.env.tmp-<pid>`) so readers never observe partial content.
3. Call [`set_secret_perms(&tmp)`](crates/infrastructure/src/deploy/docker_compose.rs) — hardens permissions **before** rename.
4. Atomically rename temp → final store file; only a successful rename counts as durable ([CXA-B044] ordering).

The bug lived in step 3's pre-fix implementation:

```rust
// BEFORE (buggy): mutates an in-memory copy, never touches disk
if let Ok(meta) = std::fs::metadata(path) {
    meta.permissions().set_mode(0o600);   // permissions() is an owned COPY
}
```

`std::fs::metadata()` returns an owned metadata value; `.permissions()` hands back another owned copy and `.set_mode(0o600)` only mutates that transient copy. Nothing was ever written back to disk with `std::fs::set_permissions`, so files kept their default umask-derived mode (`0644`) despite source claiming owner-only.

The fix (landed as CXA-B038) mutates through a binding then writes back:

```rust
// AFTER (fixed)
if let Ok(meta) = std::fs::metadata(path) {
    let mut perms = meta.permissions();              // copy we own and mutate...
    perms.set_mode(0o600);
    let _ = std::fs::set_permissions(path, perms);   // ...then WRITE BACK to disk
}
```

Because hardening previously happened **only** inside the persist path (`write_stored_secrets_owned` → `persist_store`), any store file left world-readable by an older build sat at `0644` forever once every required secret was supplied externally — resolution short-circuits before persisting anything ([CXA-B038]). The fix therefore also added [`repair_store_permissions(secret_root, work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs), which runs at the top of every resolution pass ([`resolve_deploy_secrets_in()`](crates/infrastructure/src/deploy/docker_compose.rs)) and calls [`set_secret_perms()`](crates/infrastructure/src/deploy/docker_compose.rs) on **both** the current name-keyed store ([`store_file()`](crates/infrastructure/src/deploy/docker_compose.rs)) and any legacy path-keyed store ([`cxb031_store_file()`](crates/infrastructure/src/deploy/docker_compose.rs)) — before deciding whether fallbacks are needed — remediating lax pre-existing artifacts even on no-write passes.

All permission handling is best-effort: a filesystem that cannot represent modes or a failed chmod never fails a deploy; on non-unix targets [`#[cfg(not(unix))] set_secret_perms(_path)`](crates/infrastructure/src/deploy/docker_compose.rs) is a no-op.

## Usage

There is no user-facing toggle; hardened persistence and remediation are automatic on every app-driven deploy.

```sh
# Point resolution at an isolated store to observe behavior without touching HOME:
export COXAGENT_DEPLOY_SECRETS_DIR=/tmp/demo-secrets

# Run two deploy cycles against an unconfigured compose project.
# After each cycle both written files must be owner-only:
ls -la "$COXAGENT_DEPLOY_SECRETS_DIR"
#   -rw-------   you   staff   <digest>.env        <- 0600 expected

# Simulate a pre-fix artifact and re-resolve; it must be repaired back to 0600:
chmod 0644 "$COXAGENT_DEPLOY_SECRETS_DIR"/<digest>.env
coxagent <deploy-driving command> <project>
stat -c '%a' "$COXAGENT_DEPLOY_SECRETS_DIR"/<digest>.env   # prints 600 after remediation
```

An operator who fully configures both required secrets still triggers repair of any legacy world-readable store — repair runs before resolution's early return when nothing needs seeding.

## Interface

All identifiers live in module scope of [docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs):

| Identifier | Role |
|---|---|
| `persist_store(path, owner_stamp: Option<&str>, secrets)` | Temp-sibling write → harden → atomic rename; returns whether durable |
| `write_stored_secrets_owned(path, owner: Option<&str>, secrets)` | Production entry point used by resolution/adoption |
| `#[cfg(test)] write_stored_secrets(path, secrets)` | Unstamped variant for tests / fixtures |
| `#[cfg(unix)] set_secret_perms(path)` | Hardens one file to owner-only; must write back with `set_permissions`, never just mutate |
| `#[cfg(not(unix))] set_secret_perms(_path)` | No-op on non-unix |
| `repair_store_permissions(secret_root, work_dir)` | Remediate current + legacy store files at top of each resolution pass |
| `resolve_deploy_secrets_in(work_dir, secret_root)` | Calls repair first; else resolves/persists fallbacks |

Store-location helpers used above — all in [docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs): `deploy_secrets_root()`, `store_file(secret_root, work_dir)`, `cxb031_store_file(secret_root, work_dir)`. In module-level tests, `chmod_for_test(path, mode)` stages a mode and `mode_for_test(path)` reads one back (both Unix-only).

## Configuration

- **`COXAGENT_DEPLOY_SECRETS_DIR`** — overrides where per-project secret stores live. Defaults to `<HOME>/.local/share/coxagent/deploy-secrets`, falling back to `<tempdir>/coxagent-deploy-secrets`.
- There is **no flag** for permission hardening itself — it always applies wherever Unix file modes exist.
- The process **umask** determines what mode a file gets *without* hardening; this ticket exists precisely because that default was too lax (`022 → 0644`).

## Edge cases and limits

- **Non-unix platforms**: permission bits cannot be enforced here; `set_secret_perms` is a no-op (`#[cfg(not(unix))]`).
- **Best-effort IO**: if hardening or persistence fails generally, resolve continues rather than failing the whole deploy; failures are logged/traced.
- **Exactly two files remediated**: repair covers only this checkout's canonical name-keyed store plus its one legacy path-keyed filename (`cxb031_store_file`) — not arbitrary stray files elsewhere in the root.
- **Not a broader audit tool**: this hardens CoXAgent-authored stores only; unrelated world-readable files outside these known paths are untouched.
- **Durability semantics**: only an atomic temp→rename counts as durable ([CXA-B044]); non-durable writes drop their temp sibling with no modern-store change behind.
- Filesystems/mounts that cannot represent Unix modes degrade silently because all such failures are treated as best-effort.

## Code map

One cohesive unit implements both persistence and remediation:

- [crates/infrastructure/src/deploy/docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs) — `persist_store` / `write_stored_secrets_owned`, `set_secret_perms`, `repair_store_permissions`, the resolution pass (`resolve_deploy_secrets_in`), store-path helpers (`deploy_secrets_root`, `store_file`, `cxb031_store_file`), and the inline `mod deploy_secret_tests` regression guards.
- [wiki/engineering/cxa-b030-deploy-secret-stability.md](wiki/engineering/cxa-b030-deploy-secret-stability.md) — the parent ticket documenting out-of-tree persistence, keying, and atomic-write durability that this permission hardening layers onto.
- [wiki/engineering/deployment.md](wiki/engineering/deployment.md) — the deploy-secrets overview (CXA-B017) covering honour-or-generate fallbacks; notes B038's permission remediation.

## Related

- **CXA-B028** — forbids persisting generated secrets inside any project source tree; motivates the out-of-tree store that B033 hardens.
- **CXA-B030 / CXA-B031 / CXA-B032** — persistence, key-derivation stability, and relocation semantics of the same out-of-tree store.
- **CXA-B036 / CXA-B039 / CXA-B040 / CXA-B043 / CXA-B044** — legacy-store adoption and durable-persist ordering that share `resolve_deploy_secrets_in` with this hardening path; B044's atomic-write durability contract is referenced here.
- **CXA-B038** — the fix commit for this ticket: writes permissions back via `set_permissions` and adds `repair_store_permissions`.
- Pages in this space: [deployment.md](wiki/engineering/deployment.md), [cxa-b030-deploy-secret-stability.md](wiki/engineering/cxa-b030-deploy-secret-stability.md), [docker-compose-deploys.md](wiki/engineering/docker-compose-deploys.md).
