FOLDER: Deployment
# Deploy Secrets Are Never Written Into Source (CXA-B028)

**Keywords:** superuser credentials, cleartext, PG_PASSWORD, COXAGENT_ADMIN_PASSWORD, random secret fallback, out-of-tree store, deploy secrets root, .env never written back, working tree checkout

## Overview

When an app-driven docker-compose deploy needs a required secret such as PG_PASSWORD or COXAGENT_ADMIN_PASSWORD that an operator configured nowhere - not process env and not a project-dir .env - the adapter generates a cryptographically random fallback so variable interpolation resolves and services boot without any known credential. CXA-B028 is the rule that those generated superuser credentials must never be persisted into the agent-managed source working tree at `<project>/codebase/.env`, where agents read diffs from and commit from; durable storage lives only in an out-of-tree per-user directory keyed by compose project name. This matters to anyone who deploys unconfigured apps and to any agent that changes secret generation or persistence.

## How it works

All of it lives in one adapter file implementing the port `coxagent_application::ports::outbound::DeployPort`: `crates/infrastructure/src/deploy/docker_compose.rs`.

At each deploy pass (`DockerComposeDeploy::deploy`, line 1180) resolution is computed once:

1. `read_dot_env(work_dir.join(".env"))` collects which keys an operator already assigned in the project dir (presence only; values are never read back out).
2. `missing_required_secrets(provided_by_env, &dot_env_keys)` returns exactly the keys absent from BOTH process env and `.env`. A key supplied by either source is left entirely alone - never overridden or poisoned (CXA-B017). If nothing is missing it retires any superseded legacy store and returns empty.
3. For each missing key it reads prior values back via `read_stored_secrets(&store_file(...))`, reusing them if present (CXA-B031 stability across cycles), else generating a fresh value with `random_secret()` - 32 chars from a look-alike-free charset that cannot break YAML or a DSN URL.
4. It durably persists via `write_stored_secrets_owned(...)`, which performs an atomic temp-sibling-plus-rename inside `persist_store`, hardens perms to owner-only with `set_secret_perms` (0600), writes an ownership-stamp first line (`# owner=<compose-project>`, CXA-B043), then runs legacy adoption/migration bridges (CXA-B036/B039/B043/B044).
5. The CXA-B028 guarantee: resolution never writes anything into work_dir. It returns transient key/value pairs that deploy injects into just this one child process via `seed_deploy_secrets(cmd, &secrets)` (`cmd.env(key, value)`) on the single `docker compose up -d --build` invocation.

So generated credentials exist only as ephemeral per-pass child-process env on one compose command; durable copies live only under out-of-tree stores - never as `<project>/codebase/.env`.

Store location decision: `deploy_secrets_root()` defaults to `${HOME}/.local/share/coxagent/deploy-secrets`, overridable with environment variable COXAGENT_DEPLOY_SECRETS_DIR. Each project's filename is SHA-256 of its compose project name (`store_file`, line 222) - not its absolute path (CXA-B032) - so stability tracks what Docker uses for named volumes such as `<project>_db` across re-clones and relocations. A legacy path-keyed filename survives only as a read/migration source (`cxb031_store_file`, line 240).

## Usage

No action needed to benefit: deploying any unconfigured app automatically gets stable random off-tree credentials.

Provide your own credential instead of letting the agent generate one:

```sh
# Option A: process environment
export PG_PASSWORD='myStrongPgPass' COXAGENT_ADMIN_PASSWORD='myAdminPass'
coxagent serve

# Option B: project-dir .env in <project>/codebase
cat > codebase/.env <<'EOF'
PG_PASSWORD=myStrongPgPass
COXAGENT_ADMIN_PASSWORD=myAdminPass
EOF
```

Both are honoured verbatim and never clobbered; only keys missing from both get a random fallback.

Relocate or isolate durable state:

```sh
export COXAGENT_DEPLOY_SECRETS_DIR=/var/lib/coxagent-deploy-secrets   # tests & sandboxes point here too
```

Verify behaviour:

```sh
cargo test -p coxagent-infrastructure --lib \
  resolving_deploy_secrets_never_persists_them_to_work_dir_dot_env \
  generated_secrets_are_random_never_baked_constants \
  generated_secrets_are_stable_across_cycles_for_the_same_project
```

## Interface

Adapter implementing DeployPort: struct `DockerComposeDeploy`. Key functions within docker_compose.rs:

The following constants and functions are all declared inside docker_compose.rs:

- REQUIRED_SECRET_KEYS - array constant equal to ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"], the keys compose requires via required-variable interpolation.
- OWNER_STAMP_PREFIX - string constant "# owner=", the first line persisted_store writes and read_owner_stamp parses.
- resolve_deploy_secrets(work_dir) -> Vec of (String, String) - per-pass entrypoint used by deploy; resolves durable secrets under deploy_secrets_root().
- resolve_deploy_secrets_in(work_dir, secret_root) -> Vec of (String, String) - resolution core implementing precedence, legacy adoption and durable persistence.
- missing_required_secrets(provided_by_env: fn(&str)->bool, dot_env_keys: &HashSet<String>) -> Vec of &'static str - returns which required keys need a fallback seed; pure over its inputs.
- random_secret() -> String - 32-character CSPRNG fallback from a look-alike-free charset safe for YAML and DSN URLs.
- read_dot_env(path) -> HashSet<String> - which keys an operator assigned non-blank in a project-dir .env (presence only).
- deploy_secrets_root() -> PathBuf - resolves the out-of-tree store root.
- store_file(secret_root, work_dir) -> PathBuf - SHA-256 of compose_project_name under secret_root; current store filename.
- cxb031_store_file(secret_root, work_dir) -> PathBuf - SHA-256 of canonical absolute path; legacy migration read-source only.
- read_stored_secrets(path) -> HashMap of String to String - parses KEY=VALUE store lines back out.
- write_stored_secrets_owned(path, owner: Option<&str>, secrets) -> bool (pub(crate)) - durable store writer; stamps OWNER_STAMP_PREFIX plus owner on line one and persists atomically through persist_store. Returns whether the value was committed durably. A test-only variant without the stamp is write_stored_secrets (cfg test).
- seed_deploy_secrets(cmd: &mut Command, secrets: &[(String,String)]) - injects resolved pairs into one child process via cmd.env(key, value); never touches disk.
- set_secret_perms(path) / repair_store_permissions(secret_root, work_dir) - harden existing store files to owner-only (0600), best-effort.

## Configuration

All secret behaviour lives in docker_compose.rs; there is no config file for it.

| Setting | Default | Effect |
|---|---|---|
| Required keys | PG_PASSWORD and COXAGENT_ADMIN_PASSWORD only | Which compose interpolation variables get fallback treatment |
| Operator supply (process env or project-dir .env) | none | When set for a key that key is left alone entirely; only keys missing from both get a generated value |
| COXAGENT_DEPLOY_SECRETS_DIR | absent → $HOME/.local/share/coxagent/deploy-secrets; else temp dir /coxagent-deploy-secrets if HOME unset | Root of the out-of-tree durable store; tests and sandboxes point this at an isolated location |

## Edge cases and limits

It deliberately does NOT do:

- Never writes generated secrets into any project source tree: resolution never creates `<project>/codebase/.env` and never edits one that exists. The regression guard `resolving_deploy_secrets_never_persists_them_to_work_dir_dot_env` asserts no `.env` appears after resolution against a bare work dir.
- No per-cycle rotation for an unconfigured app: once generated for a project under its compose-project-name key it stays stable across cycles (CXA-B031), matching how Docker persists pgdata in named volumes.
- Store persistence is best-effort by design: if atomic rename fails it drops its temp sibling and returns false; a deploy still proceeds because credentials were already injected as child-process env for this pass. Failure to persist never blocks or fails a deploy.
- Secret file permission hardening is best-effort too: on non-Unix platforms set_secret_perms is a no-op rather than failing.

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs - the entire deploy-secret subsystem plus DockerComposeDeploy itself. Constants REQUIRED_SECRET_KEYS / OWNER_STAMP_PREFIX; functions random_secret, read_dot_env, missing_required_secrets, resolve_deploy_secrets, resolve_deploy_secrets_in, deploy_secrets_root, store_file, cxb031_store_file, read_stored_secrets, persist_store and write_stored_secrets_owned (pub(crate)), set_secret_perms, repair_store_permissions, retire_superseded_path_keyed_store (CXA-B040), adopt legacy bridges (CXA-B036/B039/B043/B044), and seed_deploy_secrets.
- crates/infrastructure/src/deploy/mod.rs - module declaration exporting docker_compose; also hosts scoped_tests.rs sibling.
- crates/application/src/ports/outbound/deploy.rs - the DeployPort trait and DeployReport that DockerComposeDeploy implements behind.
- crates/app/src/builders.rs - wires DockerComposeDeploy into the composed service graph at boot.
- The regression guard for this ticket lives inside docker_compose.rs in the cfg(test) module deploy_secret_tests: the unit test resolving_deploy_secrets_never_persists_them_to_work_dir_dot_env. It is also cited under Usage above.

## Related

- CXA-B010 Compose Deploy Missing PG_PASSWORD - sibling page (cxa-b010-pg-password-missing.md) on why compose interpolation fails; this page is what resolves those vars for app-driven deploys so `${VAR:?}` succeeds without a known credential.
- CXA-B001 Docker Compose Deploy Failure - sibling deployment incident page (cxa-b001-docker-compose-deploy-failure.md); shares the same DockerComposeDeploy adapter.
- CXA-B017 - no reader of source can predict a deployment superuser password; the upstream principle B028's no-secret-in-source rule enforces.
- CXA-B031 / B032 / B033 / B036 / B038 / B039 / B040 / B043 / B044 - successive hardening of this store (stability keying by compose project name instead of path; owner-only permissions at rest and repair; adoption/migration bridges). Each has its own regression guard in deploy_secret_tests.


