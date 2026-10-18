FOLDER: Deployment

# Legacy World-Readable Deploy-Secret Remediation (CXA-B038)

**Keywords:** deploy secrets, world-readable, 0644, 0600, chmod, permissions at rest, repair_store_permissions, set_secret_perms, remediation, resolve_deploy_secrets_in

## Overview

CXA-B038 closes a leak window left by CXA-B033: store files written world-readable (`0644`) by a pre-fix build carried live superuser credentials but were only ever chmodded `0600` on a path that **persists new secrets**. Once an operator supplies every required secret externally (`PG_PASSWORD`, `COXAGENT_ADMIN_PASSWORD`), resolution short-circuits before persisting anything — so that lax-permission file sat world-readable on disk forever. This change runs a proactive repair at the top of every resolution pass so any pre-existing off-repo store file is remediated to owner-only even when no fallback secret is needed this pass. It is for anyone working on deploy-secret persistence or file-permission security in the docker-compose deploy adapter.

## How it works

Deploy secrets are resolved and persisted out-of-tree by [`resolve_deploy_secrets_in(work_dir, secret_root)`](crates/infrastructure/src/deploy/docker_compose.rs) in [crates/infrastructure/src/deploy/docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs). The first statement of that pass is now:

```rust
fn resolve_deploy_secrets_in(work_dir: &std::path::Path, secret_root: &std::path::Path)
    -> Vec<(String, String)> {
    // CXA-B038: harden ANY pre-existing off-repo store file to owner-only on every
    // resolution pass, before deciding whether fallback secrets are even needed.
    repair_store_permissions(secret_root, work_dir);
    ...
```

[`repair_store_permissions(secret_root, work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs) calls [`set_secret_perms(path)`](crates/infrastructure/src/deploy/docker_compose.rs) on **both** candidate store files for this checkout:

- the current name-keyed store ([`store_file()`](crates/infrastructure/src/deploy/docker_compose.rs)) — SHA-256 of the compose project name; and
- any legacy CXA-B031 path-keyed store ([`cxb031_store_file()`](crates/infrastructure/src/deploy/docker_compose.rs)) — SHA-256 of the canonicalised absolute path.

Both may hold live superuser credentials authored at lax permissions by a pre-CXA-B033 build. Repairing them here — unconditionally at the head of resolution — means an operator who fully configures both required secrets (taking the empty-missing early return below) still triggers remediation instead of leaving credentials readable.

The same commit also corrected [`set_secret_perms()`](crates/infrastructure/src/deploy/docker_compose.rs) itself so it actually writes back to disk:

```rust
// AFTER (fixed): metadata() returns an owned COPY; set_mode alone mutates only
// that copy unless we WRITE IT BACK with set_permissions.
if let Ok(meta) = std::fs::metadata(path) {
    let mut perms = meta.permissions();
    perms.set_mode(0o600);
    let _ = std::fs::set_permissions(path, perms);
}
```

The earlier buggy form (`meta.permissions().set_mode(0o600);`) mutated a transient owned copy and never touched disk — this is detailed on [the CXA-B033 page](wiki/engineering/cxa-b033-deploy-secret-permissions.md).

## Usage

There is no user-facing toggle; remediation is automatic at the start of every app-driven deploy's secret-resolution pass.

```sh
# Point resolution at an isolated store to observe behavior without touching HOME:
export COXAGENT_DEPLOY_SECRETS_DIR=/tmp/demo-secrets

# Stage exactly what a pre-fix build left behind:
#  - both required secrets configured in <project>/.env (so NO fallback is needed), and
#  - a store file relaxed to world-readable mode:
chmod 0644 "$COXAGENT_DEPLOY_SECRETS_DIR"/<digest>.env

# Run any deploy-driving command against <project>:
coxagent <deploy-driving command> <project>

# The repository-owned bytes were never rewritten this pass (nothing missing),
# yet permissions must now be owner-only:
stat -c '%a' "$COXAGENT_DEPLOY_SECRETS_DIR"/<digest>.env   # prints 600 after remediation
```

Both candidate files (`store_file` and `cxb031_store_file`) are remediated regardless of which one carries live credentials.

## Interface

All identifiers live in module scope of [docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs):

| Identifier | Line | Role |
|---|---|---|
| `resolve_deploy_secrets_in(work_dir, secret_root)` | 96 | Resolution pass; now calls `repair_store_permissions` first |
| `repair_store_permissions(secret_root, work_dir)` | 559 | Remediates current + legacy store files to owner-only; best-effort |
| `#[cfg(unix)] set_secret_perms(path)` | 538 | Hardens one file to `0600`, writing back with `set_permissions` |
| `#[cfg(not(unix))] set_secret_perms(_path)` | 550 | No-op where Unix modes are unavailable |

Store-location helpers used above — also in [docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs): `deploy_secrets_root()` (line 196), `store_file(secret_root, work_dir)` (line 222), `cxb031_store_file(secret_root, work_dir)` (line 240).

## Configuration

- **`COXAGENT_DEPLOY_SECRETS_DIR`** — overrides where per-project secret stores live. Defaults to `<HOME>/.local/share/coxagent/deploy-secrets`, falling back to `<tempdir>/coxagent-deploy-secrets`. Both candidate filenames under this root are remediated each pass.
- There is **no flag** controlling remediation or hardening — it always applies wherever Unix file modes exist.
- The process **umask** determines what mode files get *without* hardening; B038 exists because that default was too lax (`022 → 0644`).

## Edge cases and limits

- **Non-unix platforms**: permission bits cannot be enforced; both branches of permission handling degrade through best-effort/no-op paths rather than failing deploys.
- **Exactly two files remediated**: repair covers only this checkout's canonical name-keyed store plus its one legacy path-keyed filename (`cxb031_store_file`) — not arbitrary stray files elsewhere under the root.
- **Best-effort IO**: if hardening fails generally (e.g. an unwritable mount), resolution continues rather than failing the whole deploy.
- **Not a broader audit tool**: CoXAgent-authored stores only; unrelated world-readable files outside these known paths are untouched.
- The writer path (`persist_store`) still hardens its own temp sibling before atomic rename ([CXA-B044] durability); B038 adds healing for artifacts already on disk from older builds.

## Code map

One cohesive unit (the docker-compose deploy adapter) implements persistence, remediation, and their tests:

- [crates/infrastructure/src/deploy/docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs) — everything for this feature:
  - module doc + security-intent comments: lines 1–45 (honour-or-generate rule, CXA-B017/CXA-B028, `REQUIRED_SECRET_KEYS`);
  - resolution pass [`resolve_deploy_secrets_in`](crates/infrastructure/src/deploy/docker_compose.rs) (line 96), which calls remediation first;
  - [`repair_store_permissions`](crates/infrastructure/src/deploy/docker_compose.rs) (line 559), the CXA-B038 addition;
  - [`set_secret_perms`](crates/infrastructure/src/deploy/docker_compose.rs) Unix impl (line 538) / non-unix no-op (line 550);
  - persistence helpers: `persist_store` (489), `write_stored_secrets_owned` (481), test-only `write_stored_secrets` (474);
  - store-path helpers: `deploy_secrets_root` (196), `store_file` (222), `cxb031_store_file` (240);
- Regression guard for this exact behaviour lives **inline** in [`mod deploy_secret_tests`](crates/infrastructure/src/deploy/docker_compose.rs): `#[cfg(unix)] world_readable_store_file_is_remediated_even_when_no_fallback_is_needed` (line 2043) stages both required secrets in `<project>/.env`, relaxes a store file to `0644` with `chmod_for_test`, asserts resolution returns empty, then asserts the file is back to `0600` via `mode_for_test`.

There is no separate fixture file; all coverage ships inside [docker_compose.rs's inline test modules](crates/infrastructure/src/deploy/docker_compose.rs).

## Related

This page documents ticket **CXA-B038**, part of the deploy-secret stability/permissions chain that all share `resolve_deploy_secrets_in`.

- Pages in this space:
  - [wiki/engineering/cxa-b033-deploy-secret-permissions.md](wiki/engineering/cxa-b033-deploy-secret-permissions.md) — parent harden-on-write fix; B038 layers proactive repair on top and is referenced there.
  - [wiki/engineering/cxa-b030-deploy-secret-stability.md](wiki/engineering/cxa-b030-deploy-secret-stability.md) — out-of-tree persistence, keying, atomic-write durability.
- Tickets sharing the same code path / ordering contract:
  - **CXA-B017 / CXA-B028** — never bake or persist source-known credentials; motivates the out-of-tree store being hardened here.
  - **CXA-B031 / CXA-B032** — legacy path-keyed store derivation that produced the world-readable artifacts B038 remediates.
  - **CXA-B036 / CXA-B039 / CXA-B040** — legacy-store adoption that shares the early-exit shortcut B038 repairs around.
  - **CXA-B042 / CXA-B043 / CXA-B044** — legacy-store expiry ordering and ownership stamping that guard every delete/persist decision in this pass.
