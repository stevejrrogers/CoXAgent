FOLDER: -

# Deploy Secret Stability vs Per-Cycle Rotation (CXA-B027 / CXA-B028 / CXA-B030)

**Keywords:** per-cycle secret rotation, CXA-B027, CXA-B028, CXA-B030, secret stability, cleartext persistence, no-secret-in-source, resolve_deploy_secrets_in, REQUIRED_SECRET_KEYS, PG_PASSWORD durability

## Overview

This ticket chain (CXA-B027 → CXA-B028 → CXA-B030) concerns how fallback superuser credentials behave across repeated app-driven deploys of an unconfigured project. `CXA-B027` introduced cleartext persistence of generated secrets into the agent-managed source tree; `CXA-B028` removed that security regression; `CXA-B030` was filed to reintroduce *per-cycle rotation* once persistence was gone. This page documents what actually shipped: main resolved the underlying auth breakage with **stable**, durably-persisted out-of-tree secrets instead of rotation. It is for anyone reading the deploy-secret resolution path or deciding whether per-cycle rotation should ever return.

## How it works

The relevant code lives in `crates/infrastructure/src/deploy/docker_compose.rs`. Every app-driven deploy calls `DockerComposeDeploy::deploy()`, which invokes `resolve_deploy_secrets(work_dir)` once per pass and feeds the result through `seed_deploy_secrets()` into both the initial `docker compose up -d --build` and any port-eviction retry (docker_compose.rs:1161, 1178, 1241). The same honour-or-generate rule drives `compose_build_check()` (docker_compose.rs:1464).

Resolution in `resolve_deploy_secrets_in()` applies precedence per key in `REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]`: honour a non-blank process-env value; else honour a non-blank project-dir `.env` assignment; else use a fresh random fallback from `random_secret()`. The pure branch decision is computed by `missing_required_secrets()`.

The decisive change relative to this ticket chain is in that fallback branch:

- **B027-era behaviour:** cleartext values were persisted into the agent-managed source working tree so they would survive cycles — but that leaked live superuser credentials where diffs/commits would carry them.
- **B030's intent (per-cycle rotation):** after B028 removed that persistence, rotate each missing key fresh every pass. This breaks Postgres/admin auth once pgdata volumes are initialised with an earlier value — exactly B027's original symptom.
- **Current main (B031 + successors):** stability won. Once generated for a project, a fallback secret never changes across cycles. It is persisted OUT-OF-TREE under a SHA-256 of the compose project name (`store_file()`, keyed by basename-derived name so it tracks docker's named pgdata volume), reused on later passes via `read_stored_secrets()`, and kept owner-only by `repair_store_permissions()`. Legacy path-keyed stores are adopted by `adopt_legacy_cxb031_secrets()` and retired only after durable persist via `expire_converged_legacy_store()`/`retire_superseded_path_keyed_store()`.

So B028's guarantee ("never persist secrets into the source working tree") holds — storage sits under `${HOME}/.local/share/coxagent/deploy-secrets`, never inside `<project>/codebase/.env` — while B027's auth breakage is fixed not by rotating but by making one stable password durable out-of-tree.

## Usage

There are no CLI flags for this chain; behaviour is automatic on every app-driven deploy:

```text
$ coxagent <deploy-driving command> <project>
```

Resolution outcome table:

| Process env | Project `.env` | Persisted store | Result |
|---|---|---|---|
| none | none | absent | fresh random seeded AND persisted for reuse next cycle |
| none | none | present | previous value reused verbatim (stable across cycles) |
| one/both set anywhere | anything | any | configured keys honoured verbatim; nothing seeded or clobbered |

To pin credentials yourself:

```bash
export PG_PASSWORD='your-pg-secret'
export COXAGENT_ADMIN_PASSWORD='your-admin-secret'
coxagent <deploy-driving command> <project>
```

An operator-supplied secret is always honoured verbatim and never overwritten.

## Interface

All identifiers live in crates/infrastructure/src/deploy/docker_compose.rs:

- const REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]
- const OWNER_STAMP_PREFIX = "# owner="
- fn random_secret() -> String
- fn resolve_deploy_secrets(work_dir: &Path) -> Vec<(String,String)>
- fn resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path) -> Vec<(String,String)>
- fn missing_required_secrets(provided_by_env: impl Fn(&str)->bool, dot_env_keys: &HashSet<String>) -> Vec<&'static str>
- fn seed_deploy_secrets(cmd: &mut Command, secrets: &[(String,String)])
- fn read_dot_env(path: &Path) -> HashSet<String>
- fn deploy_secrets_root() -> PathBuf
- fn store_file(secret_root: &Path, work_dir: &Path) -> PathBuf   (name-derived key)
- fn cxb031_store_file(secret_root: &Path, work_dir: &Path) -> PathBuf   (legacy path-derived key)
- read_stored_secrets / write_stored_secrets(_owned), read_owner_stamp
- repair_store_permissions(...), adopt_legacy_cxb031_secrets(...), expire_converged_legacy_store(...), retire_superseded_path_keyed_store(...)

Consumers calling resolution/seeding twice are `DockerComposeDeploy::deploy()` and `compose_build_check()`.

## Configuration

No new flags were introduced by this chain; behaviour follows which values exist at resolution time plus one environment knob controlling where secrets persist:

| Setting | Default | Behaviour |
|---|---|---|
| Required key present (non-blank) in env or `.env` | n/a | Honoured verbatim; never overridden or poisoned |
| Required key absent everywhere + not stored | n/a | Fresh random fallback persisted for reuse next cycle |
| Required key absent but already stored for project | n/a | Stored value reused verbatim — STABLE across cycles |
| COXAGENT_DEPLOY_SECRETS_DIR | `${HOME}/.local/share/coxagent/deploy-secrets` (~temp when no HOME), outside any repo so agents cannot commit it |

There is deliberately no insecure default constant to disable randomness nor any knob re-enabling per-cycle rotation on main.

## Edge cases and limits

What this deliberately does **not** do:
- It does **not** implement per-cycle secret rotation on main — despite CXA-B030 being filed to do so after B028 removed persistence. Rotation breaks pgdata-backed auth and lost to stable durability.
- It never writes generated secrets back into `<project-dir>/.env`, `<project>/codebase`, or anywhere an agent would diff/commit them (the B028 guarantee).
- It never overrides or poisons an operator-configured value.
- Unlike pre-release designs it never persists credentials inside the source working tree at all (the CXA-B028 no-secret-in-source guarantee).

How it fails / known limits:
- If persistent-store writing fails best-effort during adoption/migration ([CXA-B044] ordering), legacy files stay put so the next pass re-adopts rather than losing both stores.
- A genuinely relocated checkout falls through fresh if neither store matches today's context; adoption intentionally refuses another project's owner stamp ([CXA-B043]) rather than dragging credentials across projects/hosts.
- Tests assert generated fallbacks always differ from baked constants and stay equal across consecutive resolution passes (`pass2 == pass1`, the CXA-B031 stability guard).

Test coverage lives mostly in mod-level guards within docker_compose.rs (`precedence_honours_process_env_then_dot_env_then_fallback`, stability guard asserting pass2 equals pass1 for an unconfigured project), with crate-level gates under crates/app/tests/.

## Code map

This ticket chain shipped zero net code change on its own branch — `feat/CXA-B030`'s only commit (`a27e1f0`) touched two unrelated docs, and neither CXA-B028 nor CXA-B030 is an ancestor of `main`. There is therefore no dedicated implementation file; the stable-durability design that superseded this chain lives in one place:

- crates/infrastructure/src/deploy/docker_compose.rs — `DockerComposeDeploy`: secret resolution (`REQUIRED_SECRET_KEYS`, `random_secret`, `resolve_deploy_secrets[_in]`, `missing_required_secrets`, `seed_deploy_secrets`, `read_dot_env`), durable out-of-tree storage (`deploy_secrets_root`, `store_file`, `cxb031_store_file`, read/write store + owner stamp), permission hardening and migration/retirement (`repair_store_permissions`, `adopt_legacy_cxb031_secrets`, `expire_converged_legacy_store`, `retire_superseded_path_keyed_store`), plus the stability guards in mod deploy tests.
- crates/app/tests/compose_security_gate.rs — gate enforcing `${VAR:?}` required markers (CXA-B029) for these credential keys.
- crates/app/tests/committed_secrets_gate.rs — anti-regression gate preventing ADMIN_PASSWORD-style literals being committed (the B027-cleartext failure class).

## Related

- CXA-B017 — origin of the honour-or-generate rule and `REQUIRED_SECRET_KEYS`; this chain and the durability tickets all extend that same resolution path.
- CXA-B027 / CXA-B028 — cleartext persistence into the source tree, then its removal (the no-secret-in-source guarantee this page preserves).
- CXA-B031 / B032 / B036 / B039 / B040 / B042 / B043 / B044 — the durable out-of-tree stability design that superseded per-cycle rotation.
- CXA-B029 — compose security gate enforcing `${VAR:?}` markers for these credential keys.
- docs/wiki/engineering/deployment/cxa-b036-legacy-secret-migration.md — migration bridge that keeps pre-switch values during upgrade windows.
