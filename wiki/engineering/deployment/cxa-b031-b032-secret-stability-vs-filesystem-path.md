FOLDER: -

# Deploy Secret Stability vs Filesystem Path (CXA-B031 / CXA-B032)

**Keywords:** secret stability, filesystem path key, relocation, re-clone, pgdata volume, compose_project_name, store_file, cxb031_store_file, PG_PASSWORD regeneration, named volume

## Overview

CXA-B031 introduced **stable** fallback superuser credentials for unconfigured app-driven deploys: once generated for a project they are persisted out-of-tree and reused verbatim across cycles instead of rotating per pass. But B031 keyed that persisted store by a SHA-256 of the project's canonicalised **absolute filesystem path**. CXA-B032 closes the follow-on hole: because docker persists Postgres data in a **named volume keyed by compose project name** (`<project>_db`) — independent of where on disk an app lives — relocating or re-cloning an app made B031 regenerate fresh secrets against pgdata already initialised with old values, resurrecting DB/admin auth failure. The fix keys the store file by `compose_project_name()` instead of absolute path. This page is for anyone reading how deploy-secret persistence is addressed or changing what happens when a checkout moves.

## How it works

All logic lives in `crates/infrastructure/src/deploy/docker_compose.rs`. Every app-driven deploy calls `DockerComposeDeploy::deploy()`, which resolves secrets via `resolve_deploy_secrets(work_dir)` once per pass and feeds them through `seed_deploy_secrets()` into both the initial `docker compose up -d --build` and any port-eviction retry; `compose_build_check()` resolves/seeds them too (docker_compose.rs:1160-1240).

Resolution (`resolve_deploy_secrets_in`) honours precedence per required key (`REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]`): process env → project-dir `.env` → else reuse-or-generate from this project's out-of-tree store (via `read_stored_secrets`, or freshly from `random_secret()`), then durably persist back.

The decisive function is **`store_file(secret_root, work_dir)`**:

- **B031-era behaviour:** hashed SHA-256 of `work_dir.canonicalize()` — the absolute disk location.
- **Current main (the B032 fix):** hashes only **`compose_project_name(work_dir)`**, which derives from basenames as ``cox-<parent>-<dir>` (sanitized), so it is invariant under relocation while still distinguishing unrelated projects. Because docker names its persistent pgdata volume after that same compose project name (`<project>_db`), secret stability now tracks exactly what survives a move/clone — so a relocated checkout resolves to the *same* store file and *same* secrets.

The pre-switch derivation is preserved byte-for-byte as **`cxb031_store_file(secret_root, work_dir)`** (SHA-256 of canonicalised abs path with canonicalise-with-path-fallback on failure) purely as a read/migration source so already-deployed apps can be adopted during upgrade windows ([CXA-B036]/[CXA-B039]); new writes always go to modern [`store_file`].

Storage root defaults to `${HOME}/.local/share/coxagent/deploy-secrets`, outside any source tree (`deploy_secrets_root()`), preserving CXA-B028's no-secret-in-source guarantee.

## Usage

No CLI flags — behaviour is automatic on every app-driven deploy:

```text
$ coxagent <deploy-driving command> <project>
```

Relocation outcome table:

| Situation | Store lookup | Result |
|---|---|---|
| Unconfigured app stays at one path across cycles | same name-derived store | stable value reused verbatim |
| Same logical app re-cloned/moved to another absolute path | same parent+dir basenames → same store | value reused verbatim — no regeneration |
| Genuinely different project | different basename pair → different store | separate secrets |

To pin credentials yourself:

```bash
export PG_PASSWORD='your-pg-secret'
export COXAGENT_ADMIN_PASSWORD='your-admin-secret'
coxagent <deploy-driving command> <project>
```

Operator-supplied values are honoured verbatim and never overridden or stored.

## Interface

All identifiers live in crates/infrastructure/src/deploy/docker_compose.rs:

- const REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]
- const OWNER_STAMP_PREFIX = "# owner="
- fn random_secret() -> String
- fn resolve_deploy_secrets(work_dir: &Path) -> Vec<(String,String)>
- fn resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path) -> Vec<(String,String)>
- fn missing_required_secrets(...) -> Vec<&'static str>
- fn seed_deploy_secrets(cmd: &mut Command, secrets: &[(String,String)])
- fn read_dot_env(path: &Path) -> HashSet<String>
- fn deploy_secrets_root() -> PathBuf
- fn compose_project_name(work_dir: &Path) -> String   (``cox-<parent>-<dir>`)
- fn store_file(secret_root: &Path, work_dir: &Path) -> PathBuf   (B032+ NAME-derived key)
- fn cxb031_store_file(secret_root: &Path, work_dir: &Path) -> PathBuf   (legacy PATH-derived key)
- read_stored_secrets / write_stored_secrets(_owned), read_owner_stamp
- repair_store_permissions / adopt_legacy_cxb031_secrets / expire_converged_legacy_store / retire_superseded_path_keyed_store

Consumers calling resolution/seeding twice are `DockerComposeDeploy::deploy()` and `compose_build_check()`. Regression guards in mod tests call these functions directly.

## Configuration

No new flags were introduced by this chain; behaviour follows which values exist at resolution time plus one environment knob controlling where secrets persist:

| Setting | Default | Behaviour |
|---|---|---|
| Required key present (non-blank) in env or `.env` | n/a | Honoured verbatim; never overridden or poisoned |
| Required key absent everywhere + not stored for this logical project | n/a | Fresh random fallback persisted under NAME-derived key |
| Required key absent but already stored under NAME-derived key | n/a | Stored value reused even after relocation/re-clone |
| COXAGENT_DEPLOY_SECRETS_DIR | `${HOME}/.local/share/coxagent/deploy-secrets` (~temp when no HOME), outside any repo so agents cannot commit it |

There is deliberately no knob reverting to an insecure default constant nor any switch forcing regeneration against initialized pgdata.

## Edge cases and limits

What this deliberately does **not** do:
- It does not regenerate credentials purely because a checkout moved paths — that was B031's bug; post-B032 resolution intentionally reuses whatever initialised pgdata.
- Presence of F(old)[`cxb031_store_file`] at today's canonicalised path proves this checkout was NOT relocated; adoption prefers V over any divergent P minted by an intervening buggy-era cycle ([CXA-B039]), converging in one pass without rotating forever.
- The CXA-B043 ownership-stamp rule refuses adopting creds authored by a differently-named project sharing this directory.
- Legacy deletion ([CXA-B044]) only fires AFTER values are durably persisted into modern [`store_file`], so both stores can never be lost at once.
- It never writes generated secrets back into `<project-dir>/.env`, `<project>/codebase`, or anywhere an agent would diff/commit them; operator-supplied values are never overridden or poisoned.

How it fails / known limits:
- Relocation preserves identity only while parent+dir basenames agree AND sanitization plus 60-byte truncation does not collide two genuinely different projects onto one store file; otherwise distinct projects stay isolated.
- Best-effort IO failures during adoption/persistence are non-fatal to a deploy (logged via tracing); legacy files simply stay put so the next converging pass retries rather than losing both stores.

Regression coverage lives in mod-level tests within docker_compose.rs itself: `secrets_survive_relocating_the_app_between_paths` (different absolute roots sharing basenames must yield equal resolved secrets), plus stability-equivalence guards asserting generated fallbacks differ from baked constants and stay equal across consecutive passes (`generated_secrets_are_stable_across_cycles_for_the_same_project`).

## Code map

The entire secret-resolution and storage design for this chain lives in one file:

- crates/infrastructure/src/deploy/docker_compose.rs — `DockerComposeDeploy` adapter hosting all identifiers listed under Interface above: key derivation (`compose_project_name`, `store_file`, `cxb031_store_file`), resolution (`resolve_deploy_secrets[_in]`, `missing_required_secrets`, `seed_deploy_secrets`, `read_dot_env`), durable out-of-tree storage + owner stamp (`deploy_secrets_root`, read/write store, `read_owner_stamp`), permission hardening and migration/retirement (`repair_store_permissions`, `adopt_legacy_cxb031_secrets`, `expire_converged_legacy_store`, `retire_superseded_path_keyed_store`), plus the relocation/stability regression tests in its mod test suite.

No separate implementation file was introduced by CXA-B031/B032; companion guarantee gates live at:

- crates/app/tests/compose_security_gate.rs — gate enforcing `${VAR:?}` required markers for these credential keys.
- crates/app/tests/committed_secrets_gate.rs — anti-regression gate preventing ADMIN_PASSWORD-style literals being committed.

## Related

- CXA-B017 — origin of the honour-or-generate rule and REQUIRED_SECRET_KEYS that this resolution path extends.
- CXA-B027 / CXA-B028 / CXA-B030 — cleartext persistence into source, its removal (no-secret-in-source), then why per-cycle rotation lost to stable durability.
- wiki/engineering/deployment/cxa-b030-per-cycle-secret-rotation.md — sibling page covering the same resolution/storage path end-to-end.
- CXA-B036 / CXA-B039 — migration bridge adopting pre-switch path-keyed values during upgrade windows so already-initialized pgdata keeps working.
- CXA-B040 / CXA-B042 / CXA-B043 / CXA-B044 — retiring superseded stores safely, ownership stamping across shared dirs, and durable-persist-before-delete ordering.
