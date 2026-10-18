FOLDER: Deployment
# Deploy Secret Legacy Adoption After Key-Derivation Change (CXA-B036)

**Keywords:** deploy secrets, store_file hash input, migration, key derivation, compose project name, SHA-256 path key, PG_PASSWORD, pgdata auth failure, legacy adoption, CXA-B031 path-keyed files

## Overview

CXA-B036 supplies a **migration** for when [`store_file()`](crates/infrastructure/src/deploy/docker_compose.rs) changes its hash input. CXA-B032 re-keyed durable deploy-secret persistence from SHA-256 of the canonicalised absolute path (the CXA-B031 scheme) to SHA-256 of the compose project name; on an in-place upgrade that left any already-deployed unconfigured app orphaned — its `PG_PASSWORD` sat at B031's old path-derived filename while resolution read only B032's new name-derived one — so the **first post-upgrade cycle regenerated fresh random credentials against Postgres data already initialised with the old value**, resurrecting DB/admin auth failure on exactly the deployments this chain exists to stabilise. This page is for anyone changing store-file key derivation or debugging auth failure right after upgrading an agent-deployed stack.

## How it works

All persistence lives in [`DockerComposeDeploy`](crates/infrastructure/src/deploy/docker_compose.rs)'s secret-store logic. A deploy resolves required secrets inside [`resolve_deploy_secrets_in(work_dir, secret_root)`](docker_compose.rs:96), which is where adoption runs:

1. **Read today's store.** [`read_stored_secrets(&store_file(secret_root, work_dir))`](docker_compose.rs:125) loads whatever currently sits at B032's *name*-derived filename into an in-memory map.
2. **Adopt legacy values.** [`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored)`](docker_compose.rs:298) reconstructs B031's old *path*-derived filename via [`cxb031_store_file(...)`](docker_compose.rs:240), reads it ([`read_stored_secrets`](docker_compose.rs:424)), and inserts each of its keys into `stored` whenever today's value differs or is absent ([CXA-B039] widened this from "only when F(new) was empty"). It returns a boolean verdict — whether convergence was reached — and never deletes anything itself ([CXA-B044]).
3. **Persist durably.** Only if adoption reported convergence AND [`write_stored_secrets_owned(&store_file(...), Some(owner), &stored)`](docker_compose.rs:481) durably committed (atomic temp-sibling + rename + 0o600 hardening) does [`expire_converged_legacy_store(&cxb031_store_file(...))`](docker_compose.rs:339) remove F(old). This ordering guarantees we can never lose both stores at once.
4. If no required secret is missing (all supplied externally), resolution short-circuits early but still calls [`retire_superseded_path_keyed_store(secret_root, work_dir)`](docker_compose.rs:364) so an obsolete path-keyed duplicate holding live credentials does not linger unreferenced forever (CXA-B040).

The modern key is derived by hashing [`compose_project_name(work_dir)`](docker_compose.rs:685) (`cox-<parent>-<dir>`, basenames only → invariant under relocation); the legacy reconstruction hashes `work_dir.canonicalize()` with fallback to the raw path on failure ([`cxb031_store_file`](docker_compose.rs:246)).

Both derivations are deterministic functions of `work_dir`, so an in-place upgrade always finds precisely which file B031 wrote; presence of F(old) at today's canonicalised path proves this checkout was not relocated, so adoption cannot drag a value across hosts.

## Usage

There is no user-facing command — migration runs automatically inside every app-driven deploy (`DockerComposeDeploy::deploy() → resolve_deploy_secrets_in(work_dir)`). To observe it manually after upgrading software that predates CXA-B032:

```sh
# Pre-upgrade state simulated with an OLD path-derived store only:
mkdir -p /tmp/cxab036-proj /tmp/cxab036-store
# ...an older coxagent wrote <sha256-of-canonical-path>.env here containing:
#   PG_PASSWORD=stable-b031-password
# ...no file exists yet under today's <sha256-of-compose-project-name>.env

COXAGENT_DEPLOY_SECRETS_DIR=/tmp/cxab036-store coxagent serve   # first post-upgrade deploy

# Verify adoption rather than regeneration:
ls /tmp/cxab036-store                                  # modern <digest>.env now exists
grep PG_PASSWORD /tmp/cxab036-store/<digest>.env       # -> stable-b031-password (reused)
```

The regression guard below encodes this exact scenario as a test; it passes only if resolution reuses `stable-b031-password`.

## Interface

Key identifiers in [`crates/infrastructure/src/deploy/docker_compose.rs`](../../../crates/infrastructure/src/deploy/docker_compose.rs):

- [`store_file(secret_root, work_dir) -> PathBuf`](docker_compose.rs:222) — modern canonical key: SHA-256 of `compose_project_name(work_dir)` → `<hex>.env`. Where new secrets are written.
- [`cxb031_store_file(secret_root, work_dir) -> PathBuf`](docker_compose.rs:240) — reconstructed pre-CXA-B032 key: SHA-256 of canonicalised absolute path → `<hex>.env`. Read/migration source only.
- [`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored) -> bool`](docker_compose.rs:298) — per-key recovery bridge; returns whether convergence was reached; never deletes.
- [`expire_converged_legacy_store(path)`](docker_compose.rs:339) — deletes F(old); must be called ONLY after durable persist of identical values.
- [`retire_superseded_path_keyed_store(secret_root, work_dir)`](docker_compose.rs:364) — removes an obsolete path-keyed duplicate even when no fallback is needed (empty-missing shortcut; CXA-B040).
- Support helpers used above also live in this file:
  - [`compose_project_name(work_dir) -> String`](docker_compose.rs:685) — derives `cox-<parent>-<dir>` from basenames only; the modern hash input.
  - [`read_stored_secrets(path)`](docker_compose.rs:424) — parses `KEY=VALUE` lines back into a map.
  - [`read_owner_stamp(path) -> Option<String>`](docker_compose.rs:446) — reads the first-line `# owner=<project>` stamp used by the CXA-B043 ownership rule.
  - [`write_stored_secrets_owned(path, owner: Option<&str>, secrets)`](docker_compose.rs:481) / `write_stored_secrets(path, secrets)` — durable persist entry points.

## Configuration

| Setting | Default | Effect |
|---|---|---|
| `COXAGENT_DEPLOY_SECRETS_DIR` | `<HOME>/.local/share/coxagent/deploy-secrets` (falls back to temp dir if `HOME` unset) | Root directory under which both `store_file()` and `cxb031_store_file()` resolve `<hex>.env` files. |
| Store file key input (`store_file`) | SHA-256 of `compose_project_name(work_dir)` | Changing this input is exactly what creates the orphan-window this migration repairs. |
| Legacy reconstruction input (`cxb031_store_file`) | SHA-256 of canonicalised absolute path (raw-path fallback on failure) | Read/migration source; never written. |

There is no flag that disables adoption — it runs on every resolution pass by design. The one config surface that moves where stores live is `COXAGENT_DEPLOY_SECRETS_DIR`, which lets tests/sandboxes isolate both current and legacy files together.

## Edge cases and limits

It deliberately does NOT do:

- **Migrate data already at B032's name-derived key.** Adoption covers only values B031-era code wrote under its old path-derived filename ([CXA-B039] handles the case where an intervening buggy cycle already minted a divergent value into F(new)).
- **Delete inside adoption.** Removal of F(old) happens only after convergence AND durable persist ([CXA-B044]); a failed or crash-interrupted persist leaves both stores intact so the next pass re-adopts and retries.
- **Drag credentials across hosts or projects.** F(old)'s presence at today's canonicalised path proves this checkout was not relocated; additionally [CXA-B043]'s ownership-stamp rule refuses to adopt or delete any legacy file authored by a differently-named project sharing this directory.
- If persistence fails entirely (unwritable dir), resolution still succeeds for that pass via per-process env but nothing durable lands until later; secret stability across restarts then depends on operator-supplied values.

## Code map

- [`crates/infrastructure/src/deploy/docker_compose.rs`](../../../crates/infrastructure/src/deploy/docker_compose.rs) — everything about generating/persisting deploy secrets under `DockerComposeDeploy`: key derivations (`store_file`, `cxb031_store_file`, `compose_project_name`), adoption/expiry (`adopt_legacy_cxb031_secrets`, `expire_converged_legacy_store`, `retire_superseded_path_keyed_store`), read/write helpers (`read_stored_secrets`, `read_owner_stamp`, `write_stored_secrets(_owned)`), orchestration (`resolve_deploy_secrets(_in)`), plus the test module with regression guards named after their tickets (e.g. [`first_post_upgrade_cycle_reuses_the_pre_cxb032_persisted_value`](docker_compose.rs:2002)).

## Related

- [CXA-B033 Deploy Secrets World-Readable on Disk](cxa-b033-deploy-secrets-world-readable.md) — same store-file family (`store_file`, `cxb031_store_file`); permission hardening for both current and legacy stores; also under `deployment/`.
- [CXA-B010 Pg Password Missing](cxa-b010-pg-password-missing.md) — the deploy failure this secret-generation exists to avoid; same compose files and required keys.
- [CXA-B001 Docker Compose Deploy Failure](cxa-b001-docker-compose-deploy-failure.md) — sibling deploy page in this folder.
- CXA-B031 / CXA-B032 — origin of durable per-project secret persistence (B031, path-keyed) and its re-keying to compose project names (B032), which created the orphan-window this migration repairs.
- CXA-B039 — widened recovery so adoption prefers V even when F(new) holds a divergent pre-fix value (this first shipped narrower).
- CXA-B040 / CXA-B042 / CXA-B043 / CXA-B044 — superseded-store retirement on no-fallback passes (B040), completion boundary (B042), cross-project ownership stamp guard (B043), and delete-only-after-durable-persist ordering (B044).

