FOLDER: Deployment

# Deploy Secrets for Docker Compose (CXA-B017)

**Keywords:** deploy secrets, docker compose, PG_PASSWORD, COXAGENT_ADMIN_PASSWORD, random secret, ${VAR:?} interpolation, credential baking, DockerComposeDeploy

## Overview

CXA-B017 closes a security hole left by the CXA-B010 fix. B010 began seeding `PG_PASSWORD` and `COXAGENT_ADMIN_PASSWORD` into every app-driven docker-compose deploy so `${VAR:?}` interpolation would not kill the stack before anything started — but it used hardcoded source-published credentials (`ci-verify-pg`, `ci-verify-admin`) that anyone reading source could use to log in as superuser on any deployment. This page documents how those public constants were replaced with an honour-or-generate rule while keeping deploys from dying at interpolation time. It is for anyone working on the docker compose deploy adapter or its build cross-check.

## How it works

The entry point is `DockerComposeDeploy::deploy()` in crates/infrastructure/src/deploy/docker_compose.rs: when a project has a compose file it calls `resolve_deploy_secrets(work_dir)` once per pass and feeds the result through `seed_deploy_secrets` into both the initial `docker compose up -d --build` command and any port-eviction retry — so a retry re-runs identical interpolation against state the first `up` created.

Resolution (`resolve_deploy_secrets_in`) applies this precedence to each key in `REQUIRED_SECRET_KEYS`:

1. **Process environment** — if this process already carries a non-blank value (the provided-by-env closure), leave it alone.
2. **Project-dir `.env`** — if an operator-authored `.env` assigns a non-blank value (parsed by `read_dot_env`, which skips comments/blanks/blank values, tolerates leading `export`, splits on first `=`), leave it alone.
3. **Fallback** — only keys missing from *both* get a fresh cryptographically-random 32-char value from `random_secret()`. Its charset drops look-alikes (`0/O/1/l/I`) and characters that would break YAML or a DSN URL.

The pure decision rule lives in `missing_required_secrets()`, which takes two closures/sets so every precedence branch is deterministically unit-testable without real IO.

Later tickets layered durability onto B017's randomness inside this same function:
- **CXA-B031/B032/B036/B039**: fallback secrets are persisted out-of-tree keyed by a hash of the compose *project name* (not disk path) and reused across cycles so pgdata volumes keep their superuser password; legacy path-keyed stores are adopted during upgrade.
- **CXA-B038**: store files are remediated to owner-only (`0o600`) permissions on every pass.

The same honour-or-generate rule drives `compose_build_check()`, so even an automated build of this repo never bakes public constants into its verification command.

## Usage

No new CLI flags were added for B017; behaviour is automatic inside every app-driven deploy:

```text
$ coxagent <deploy-driving command> <project>
```

Resolution outcomes:

| Process env | Project `.env` | Result |
|---|---|---|
| none | none | both keys get fresh random values |
| one key set | none | configured key honoured; other gets random |
| both set anywhere | anything | nothing seeded; no clobbering |

To pin your own credentials instead of relying on generated ones:

```bash
export PG_PASSWORD='your-pg-secret'
export COXAGENT_ADMIN_PASSWORD='your-admin-secret'
coxagent <deploy-driving command> <project>

# or place them in <project-dir>/.env:
#   PG_PASSWORD=your-pg-secret
#   COXAGENT_ADMIN_PASSWORD=your-admin-secret
```

An operator-supplied secret is always honoured verbatim and never overwritten with ours.

## Interface

All identifiers live in crates/infrastructure/src/deploy/docker_compose.rs:

- const REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]
- fn random_secret() -> String
- fn resolve_deploy_secrets(work_dir: &Path) -> Vec<(String,String)>
- fn resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path) -> Vec<(String,String)>
- fn missing_required_secrets(provided_by_env: impl Fn(&str)->bool, dot_env_keys: &HashSet<String>) -> Vec<&'static str>
- fn seed_deploy_secrets(cmd: &mut tokio::process::Command, secrets: &[(String,String)])
- fn read_dot_env(path: &Path) -> HashSet<String>

The two consumers that call `resolve_deploy_secrets` and `seed_deploy_secrets` are `DockerComposeDeploy::deploy()` (seeds both the initial `up` and the port-eviction retry) and `compose_build_check()` (seeds a throwaway `docker compose build`).

## Configuration

B017 introduced no new CLI flags or config-file knobs; behaviour is driven entirely by which values are present at resolution time:

| Setting | Behaviour |
|---|---|
| Required key present (non-blank) in process env OR project-dir `.env` | Honoured verbatim; never overridden or poisoned |
| Required key absent everywhere | Fresh 32-char random fallback via thread RNG from an unambiguous charset |

There is deliberately no insecure default constant to disable — absence of configuration yields secure randomness. Later durability tickets added their own settings (for example the out-of-tree persistent-store root, overridable via env for tests/sandboxes), which belong to those pages rather than B017.

## Edge cases and limits

What this deliberately does **not** do:
- It never writes generated secrets back into `<project-dir>/.env` or anywhere inside the project source tree.
- It never overrides or poisons an operator-configured value — if a key resolves from either process env or `.env`, nothing is returned for it.
- It does not rotate per-cycle when nothing is configured; later stability work made fallbacks durable/persistent instead.
- A world-readable pre-existing store file does not break resolution; permission repair runs before deciding whether fallbacks are needed (CXA-B038).

How it fails / known limits:
- If the persistent-store write fails best-effort during adoption, legacy files stay in place so the next pass re-adopts and retries rather than losing both stores.
- If both required keys are configured externally, resolution returns early with nothing seeded — correct because no fallback was needed.
- The old baked constants must never resurface; tests assert generated secrets differ from them on every call.

Test coverage lives mainly in `mod deploy_secret_tests` within docker_compose.rs: precedence/branch coverage (`precedence_honours_process_env_then_dot_env_then_fallback`), randomness vs baked constants plus length/charset/syntax assertions (`generated_secrets_are_random_never_baked_constants`), charset look-alike exclusion (`generated_charset_excludes_similar_lookalikes`) and `.env` parsing boundary cases including split-on-first-equals preserving passwords containing special characters (`dot_env_parser_detects_only_real_assignments`, over 20 draws). Related crate-level gate suites live under crates/app/tests/.

## Code map

- crates/infrastructure/src/deploy/docker_compose.rs — `DockerComposeDeploy`: secret resolution (`REQUIRED_SECRET_KEYS`, `random_secret`, `resolve_deploy_secrets[_in]`, `missing_required_secrets`, `seed_deploy_secrets`, `read_dot_env`) plus its two consumers in deploy() and compose_build_check(); later-ticket store machinery lives here too.
- crates/app/tests/deploy_smoke.rs — COX-B008 deployment smoke test: boots this repo's own compose stack (seeding `ci-smoke` values so `${VAR:?}` interpolation resolves) and probes the hub on host port 8101. Uses its own constants, unrelated to CXA-B017 resolution.
- crates/app/tests/compose_security_gate.rs — guards compose interpolation/security behaviour around `${VAR}` usage for these keys.
- crates/app/tests/committed_secrets_gate.rs — anti-regression gate preventing committed source/literals from carrying ADMIN_PASSWORD-style values again.

## Related

- CXA-B010 — original fix that seeded these vars but used public constants; CXA-B017 hardens it.
- CXA-B028/B031/B032/B036/B039/CXA-B038/CXA-B044 — durability/stability/permission hardening layered onto this same resolution path in later commits.
