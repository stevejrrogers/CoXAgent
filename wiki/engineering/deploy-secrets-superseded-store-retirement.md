FOLDER: Deployment

# Retiring Superseded Path-Keyed Secret Stores (CXA-B040)

**Keywords:** superseded secret store, path-keyed secrets, cxb031_store_file, retire_superseded_path_keyed_store, adopt_legacy_cxb031_secrets, expire_converged_legacy_store, deploy secrets migration, duplicate credentials, CXA-B044

## Overview

CXA-B032 switched how deploy secrets are stored on disk: from a SHA-256 of the project's canonicalised absolute *path* (the CXA-B031 scheme) to a SHA-256 of the compose *project name*. Any store file written by pre-CXA-B032 software under its old path-derived filename therefore became unreferenced — yet still held live DB/admin credentials at rest as an unremoved plaintext duplicate. This page documents how resolution now retires that superseded file for a project it owns: adopting any value it does not already know into the canonical name-keyed store and then deleting the obsolete duplicate — even on a deploy pass where every required secret is supplied externally so no fallback is generated. It is for anyone touching secret resolution or migration in the docker compose deploy adapter.

## How it works

Resolution entry point `resolve_deploy_secrets_in(work_dir, secret_root)` in crates/infrastructure/src/deploy/docker_compose.rs normally handles legacy stores only inside its fallback branch (`adopt_legacy_cxb031_secrets` → `expire_converged_legacy_store`). CXA-B040 closes the gap where an operator supplies **every** required secret externally: `missing_required_secrets` returns empty and resolution returns early without ever reaching adoption/expiry — so an obsolete duplicate would linger forever.

The fix adds an explicit call to `retire_superseded_path_keyed_store(secret_root, work_dir)` in that empty-missing shortcut (docker_compose.rs:119), before the early return. That function:

1. Recomputes where a legacy store would sit via `cxb031_store_file` (SHA of canonicalised absolute path with a path fallback on failure). If nothing exists there it is a no-op touching no host state.
2. Applies the CXA-B043 ownership rule via `read_owner_stamp`: if the file's first line (`# owner=...`) names a compose project different from today's `compose_project_name(work_dir)`, it refuses to adopt or delete anything — credentials belong to another app sharing this directory.
3. Merges every legacy key/value into what already lives in today's canonical `store_file` using `.entry(k).or_insert(v)` — so operator-configured or previously-adopted values always win; existing canonical values are never overridden.
4. Persists durably via `write_stored_secrets_owned` (atomic temp-sibling + rename), stamping ownership.
5. Only if that persist returned durable success does it call `std::fs::remove_file(&legacy_path)`. This preserves the CXA-B044 ordering constraint observed throughout this chain: deletion is gated on durable persistence of exactly those adopted values, so live credentials can never be lost before their new location owns them.

The same constraint protects expiry elsewhere: `adopt_legacy_cxb031_secrets` only *reports* whether convergence was reached (returns bool); actual deletion happens later in `expire_converged_legacy_store`, called only after successful persist inside resolution.

## Usage

No user action or flag triggers retirement; it runs automatically inside every app-driven deploy / cross-check that calls `resolve_deploy_secrets`. The observable effect after upgrading past CXA-B032 with pre-existing state:

```text
# Before B040 fix: <secret_root>/<sha-of-path>.env lingers alongside
#   <secret_root>/<sha-of-project-name>.env  -> two copies of live creds
# After one resolve pass:
$ ls <secret_root>/
    <sha-of-project-name>.env        # canonical store now owns all keys
    # ...the old <sha-of-path>.env is GONE
```

Log line confirming removal (`tracing::info!`, otherwise a warning):

```text
removed superseded path-keyed deploy secret store {old} after adopting its values into {new}
```

## Interface

All identifiers live in crates/infrastructure/src/deploy/docker_compose.rs:

- const OWNER_STAMP_PREFIX = "# owner=" — first line stamped by owned stores; read back by read_owner_stamp.
- fn retire_superseded_path_keyed_store(secret_root: &Path, work_dir: &Path) — adopts + deletes an obsolete path-keyed store for THIS project; runs in the empty-missing shortcut.
- fn cxb031_store_file(secret_root: &Path, work_dir: &Path) -> PathBuf — reconstructs where pre-CXA-B032 software stored secrets (SHA of canonicalised abs path).
- fn store_file(secret_root: &Path, work_dir: &Path) -> PathBuf — today's canonical name-keyed store path.
- fn compose_project_name(work_dir: &Path) -> String — identity used for both stamping and keying.
- fn read_stored_secrets(path) -> HashMap<String,String> / fn read_owner_stamp(path) -> Option<String> — read helpers used for merge and ownership checks.
- pub(crate) fn write_stored_secrets_owned(path, owner: Option<&str>, secrets) -> bool — durable atomic persist returning durability verdict.
- const REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]; PG_PASSWORD / COXAGENT_ADMIN_PASSWORD are the carried keys.

Consumers calling into this machinery are purely internal to this module; no other crate calls these functions directly.

## Configuration

Retirement introduces no new configuration surface beyond what previous tickets established:

| Setting | Effect |
|---|---|
| Env override COXAGENT_DEPLOY_SECRETS_DIR | Points secret_root somewhere isolated (tests/sandboxes use this); retirement operates under whatever root resolves here |
| Store-file ownership stamp (`# owner=`) | A stamped legacy file whose owner != today's compose project name is never adopted nor deleted |
| Absence/presence of required keys externally | Empty-missing shortcut now still triggers retirement instead of skipping it |

Deletion remains best-effort like all IO here; there is deliberately no knob to disable removal once converged-and-durable.

## Edge cases and limits

What this deliberately does **not** do:
- Never deletes before durable persistence succeeds ([CXA-B044]); if persist fails best-effort it logs a warning and leaves both files until adoption converges next pass so live creds are never stranded with neither home owning them.
- Never adopts or deletes a legacy file whose ownership stamp names another project ([CXA-B043]).
- Does not run inside per-key adoption (`adopt_legacy_cxb031_secrets`) itself; that function returns only a convergence verdict and expiry stays downstream.

How it fails / known limits:
- If remove_file errors after successful persist (`std::fs::remove_file` returning Err), it logs a warning but treats adoption as done; next pass re-attempts removal since values already match ([CXA-B042]-style retry).
- A genuinely relocated app falls through fresh rather than being migrated across hosts because presence of F(old) at TODAY'S canonicalised path is itself proof this checkout was not relocated.
- Only files under one shared root passed as secret_root are considered; retirement scopes strictly to this checkout's derived filenames.

Regression coverage lives in mod deploy_secret_tests within docker_compose.rs:
`superseded_path_keyed_store_is_adopted_and_removed_even_when_all_secrets_are_external` stages only an obsolete path-keyed store plus fully-external config and asserts both removal and adoption into canonicals (docker_compose.rs).

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs — DockerComposeDeploy adapter holding all retirement/adoption/expiry machinery described above plus DeployPort implementations that drive resolve_deploy_secrets per pass.

## Related

- wiki/engineering/deployment.md — "Deploy Secrets for Docker Compose (CXA-B017)": the honour-or-generate resolution rule this retirement machinery sits inside; read it first for the full precedence flow.
- CXA-B031/B032 — key derivation switched from path-hash (B031) to project-name-hash (B032); the superseded files B040 retires are B031-era artifacts.
- CXA-B036/B039 — legacy path-keyed adoption and recovery; B040 extends adoption into the empty-missing shortcut.
- CXA-B042/B044 — convergence expiry and the durable-persist-before-delete ordering constraint that gates all removal here.
- CXA-B043 — ownership stamping so adoption never drags another project's credentials across.
