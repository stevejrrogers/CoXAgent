FOLDER: Deployment
# Deploy Secrets World-Readable on Disk (CXA-B033)

**Keywords:** deploy secrets, docker compose, PG_PASSWORD, COXAGENT_ADMIN_PASSWORD, chmod 0600, 0644, file permissions, owner-only, stored secrets

## Overview

CXA-B033 was a security bug report: "Persisted deploy secrets written world-readable (0644) despite code claiming chmod 0600". When an app-driven deploy auto-generates missing credentials (e.g. `PG_PASSWORD`, `COXAGENT_ADMIN_PASSWORD`) they are persisted to an out-of-tree store file so later cycles reuse them — and that store was being created group/world-readable (0644) instead of the owner-only 0600 the surrounding comments claimed. This page is for anyone touching how generated deploy credentials are persisted or audited on disk.

## How it works

All persistence lives in `DockerComposeDeploy`'s secret-store logic in [`docker_compose.rs`](../../../crates/infrastructure/src/deploy/docker_compose.rs). When a project's compose stack needs a required secret that neither the process env nor a project-dir `.env` supplies ([`missing_required_secrets()`](docker_compose.rs:72)), resolution generates one via [`random_secret()`](docker_compose.rs:35) and stores it durably so later cycles reuse it:

1. Every resolution pass starts inside [`resolve_deploy_secrets_in()`](docker_compose.rs:96) with `repair_store_permissions(secret_root, work_dir)` (line 106), which clamps BOTH any existing name-keyed store AND any legacy CXA-B031 path-keyed store back to owner-only — even on passes where no fallback is needed.
2. When fallbacks ARE needed, [`write_stored_secrets_owned(store_file(...), Some(owner), &stored)`](docker_compose.rs:481) persists them atomically via a temp-sibling + rename inside [`persist_store()`](docker_compose.rs:489).
3. The mode change itself is [`set_secret_perms()`](docker_compose.rs:538): it reads metadata's permission *copy*, calls `perms.set_mode(0o600)`, then **writes that copy back with `set_permissions()`**. The root cause of CXA-B033 was that an earlier version mutated only the in-memory copy without writing it back — so no file was ever actually chmodded and every store stayed at whatever default mode its creator produced (0644).
4. Values resolve once per pass and are reused from `<hash>.env` so pgdata-initialised credentials stay stable; nothing is ever written into the project's source tree (CXA-B028).

The default secret root is out-of-tree at `<HOME>/.local/share/coxagent/deploy-secrets/<sha256>.env`, overridable via `COXAGENT_DEPLOY_SECRETS_DIR`. The filename key is SHA-256 of the compose project name ([`store_file()`](docker_compose.rs:222)), not the absolute path — matching docker's named-volume keying so clones/relocations reuse one password.

Related lax-permission handling exists for machine-local auth tokens too: in [`builders.rs`](../../../crates/app/src/builders.rs), [`provision_local_token()`](builders.rs:213) refuses to hand a group/world-readable `<home>/CoXAgent/operator.token` to `/store`, rather than using it; [`load_coordination()`](builders.rs:91) tightens a loose `coordination.json` to 0600 at load.

## Usage

There is no user-facing command — this runs automatically inside every app-driven deploy (`DockerComposeDeploy::deploy() → resolve_deploy_secrets(work_dir)`). To observe or exercise it manually:

```sh
# Trigger resolution + persistence by deploying an unconfigured compose project:
coxagent serve   # PG_PASSWORD / COXAGENT_ADMIN_PASSWORD get generated if unset

# Verify on-disk permission bits of generated stores:
find ~/.local/share/coxagent/deploy-secrets -name '*.env' -exec ls -l {} \;
# Expect "-rw-------" (0600), never "-rw-r--r--" (0644)
```

To confirm remediation of a pre-existing world-readable artifact:

```sh
mkdir -p /tmp/cxab033 && touch /tmp/cxab033/a.env && chmod 644 /tmp/cxab033/a.env
stat -f '%Lp' /tmp/cxab033/a.env            # -> "644"
COXAGENT_DEPLOY_SECRETS_DIR=/tmp/cxab033 coxagent serve   # next resolution pass clamps to 600
stat -f '%Lp' /tmp/cxab033/a.env            # -> now "600"
```

## Interface

Key identifiers in [`crates/infrastructure/src/deploy/docker_compose.rs`](../../../crates/infrastructure/src/deploy/docker_compose.rs):

- [`REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]`](docker_compose.rs:23) — keys that must always be resolvable for interpolation.
- [`missing_required_secrets(provided_by_env, dot_env_keys) -> Vec<&str>`](docker_compose.rs:72) — pure decision over which keys still need a fallback.
- [`resolve_deploy_secrets(work_dir)`](docker_compose.rs:92) → calls `resolve_deploy_secrets_in(work_dir, &deploy_secrets_root())`.
- [`resolve_deploy_secrets_in(work_dir, secret_root)`](docker_compose.rs:96) — full pipeline; repairs perms first; returns resolved key/value pairs.
- [`random_secret() -> String`](docker_compose.rs:35) — cryptographically-random 32-char fallback alphabet.
- [`write_stored_secrets_owned(path, owner: Option<&str>, secrets) -> bool`](docker_compose.rs:481) — production persist entry point; stamps ownership (`# owner=<project>`).
- [`persist_store(path, owner_stamp: Option<&str>, secrets)`](docker_compose.rs:489) — atomic temp-sibling + rename; returns whether committed durably (only `true` authorises deleting a superseded legacy store).
- [`set_secret_perms(path)`](docker_compose.rs:538) — sets mode **0600**, writing back via `set_permissions`. Non-unix builds are a no-op.
- [`repair_store_permissions(secret_root, work_dir)`](docker_compose.rs:559) — clamps current + legacy stores to 0600 even when nothing is persisted this pass (the CXA-B038 remediation entry point).
- [`read_stored_secrets(path)`](docker_compose.rs:424) / [`read_owner_stamp(path)`](docker_compose.rs:446) — read side; parse KEY=VALUE lines and the first-line `# owner=` stamp.
- Private helper port symbols in app crate for tokens-like files (`operator.token`, `coordination.json`) live in [`crates/app/src/builders.rs`](../../../crates/app/src/builders.rs): `operator_token_path()`, `provision_local_token()`, `load_coordination()`.

## Configuration

| Setting | Default | Effect |
|---|---|---|
| `COXAGENT_DEPLOY_SECRETS_DIR` | `<HOME>/.local/share/coxagent/deploy-secrets` (falls back to temp dir if `HOME` unset) | Root directory for per-project deploy-secret store files; lets tests/sandboxes isolate them. |
| Permission mode on written store files | hard-coded **0600** (`set_secret_perms`) | Not configurable by design; credentials are always clamped owner-only. |
| Required secret keys | `PG_PASSWORD`, `COXAGENT_ADMIN_PASSWORD` (`REQUIRED_SECRET_KEYS`) | Only these two get auto-generated fallbacks; every other credential must be operator-supplied. |

No env flag disables permission hardening — doing so would recreate CXA-B033. If your filesystem cannot represent modes, persistence is best-effort and simply skips the chmod rather than failing the deploy.

## Edge cases and limits

It deliberately does NOT do:

- **In-place tightening of an operator-authored `.env`** inside a project tree — permission hardening applies only to CoXAgent's own out-of-tree `<hash>.env` stores under the secret root, never files an operator created in their repo.
- **Runtime enforcement** after startup or on non-unix hosts (where mode bits don't exist); there the chmod is a silent no-op.
- **Hardening token-like machine-local files during write**: for these there is currently no writer found that creates them (the docstring claims login writes one but no such write path exists); readers such as [`provision_local_token()`](builders.rs:213) only *refuse* a loose file rather than fixing it. This asymmetry is worth knowing if you go looking for where credentials originate.
- When persistence itself fails (unwritable dir), resolution still succeeds for this pass via per-process env (`seed_deploy_secrets`) but nothing durable lands on disk until later; secret stability across restarts then depends on an operator supplying values.

## Code map

- [`crates/infrastructure/src/deploy/docker_compose.rs`](../../../crates/infrastructure/src/deploy/docker_compose.rs) — everything about persisting/generating deploy secrets under `DockerComposeDeploy`: `random_secret`, `missing_required_secrets`, `resolve_deploy_secrets(_in)`, `write_stored_secrets_owned`/`persist_store`, `set_secret_perms`, `repair_store_permissions`, plus migration/adoption helpers (`adopt_legacy_cxb031_secrets`, `retire_superseded_path_keyed_store`) and their test module.
- [`crates/app/src/builders.rs`](../../../crates/app/src/builders.rs) — machine-local token and coordination-config handling in app crate: `operator_token_path()`, `provision_local_token()`, `load_coordination()` (the readers that refuse/tighten loose perms).
- [`docs/wiki/engineering/deployment/cxa-b010-pg-password-missing.md`](cxa-b010-pg-password-missing.md) — sibling page on compose interpolation for these same required secrets.

## Related

- [CXA-B010 Pg Password Missing](cxa-b010-pg-password-missing.md) — the deploy failure this secret-generation exists to avoid; same compose files and required keys.
- CXA-B028 — no generated superuser credentials ever written into an agent-managed source tree (why stores are out-of-tree).
- CXA-B031 / CXA-B032 — origin of stable per-project secret persistence and its name-keyed store derivation.
- CXA-B038 (fix landing after B033's report) — repaired `set_secret_perms` to actually write back the mode, plus added `repair_store_permissions` remediation of pre-existing 0644 files; regression guard in [`deploy_secret_tests::world_readable_store_file_is_remediated_even_when_no_fallback_is_needed`](docker_compose.rs:2043).


