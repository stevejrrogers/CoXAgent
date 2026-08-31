FOLDER: Deploy Secrets

# Deploy secret rotation and legacy-store adoption

**Keywords:** deploy secrets, PG_PASSWORD, COXAGENT_ADMIN_PASSWORD, secret rotation, secret persistence, legacy store adoption, store_file, cxb031_store_file, migration bridge, docker compose interpolation

## Overview

For every docker-compose deploy it drives, `DockerComposeDeploy` must satisfy `${VAR:?}` interpolation on the required keys `PG_PASSWORD` and `COXAGENT_ADMIN_PASSWORD`. It never bakes a public constant (CXA-B017) and never overrides what an operator configured; only keys missing from both the process env and the project-dir `.env` get a cryptographically-random fallback. That fallback must stay stable across cycles — regenerating it every pass breaks Postgres auth and admin login against initialized named volumes — so it is persisted out-of-tree in a per-project store file.

This page covers that persistence plus its migration/adoption path. Ticket CXA-B042 documents how CXA-B039's "adopt legacy values with per-key preference" became an indefinite override: while the legacy (pre-CXA-B032) store file lingered after convergence it was consulted on every pass with preference over the modern store, so deleting only the modern store to force rotation had stale credentials resurrected each cycle. The fix expires the legacy file once adoption converges.

The feature lives on branch `feat/CXA-B042`, built from CXA-B028 through CXA-B041; it is not yet merged into `main`.

## How it works

The flow runs inside one compose deploy pass:

1. `resolve_deploy_secrets(work_dir)` calls `resolve_deploy_secrets_in(work_dir, &deploy_secrets_root())`.
2. It first calls `repair_store_permissions(secret_root, work_dir)`, which chowns both modern and legacy store files owner-only — remediating a world-readable file written by a pre-CXA-B033 build (CXA-B038).
3. It reads `.env`, then computes which required keys are missing via `missing_required_secrets(...)`. If none are missing from either source we return empty — nothing is persisted or seeded.
4. Otherwise it loads today's values from [`store_file(...)`](crates/infrastructure/src/deploy/docker_compose.rs) into a map.
5. It calls [`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored)`](crates/infrastructure/src/deploy/docker_compose.rs), where CXA-B036/B039/B042 all meet.
6. For each still-missing key it reuses the stored value or generates via [`random_secret()`](crates/infrastructure/src/deploy/docker_compose.rs), persists everything through [`write_stored_secrets(&store_file(...), &stored)`](crates/infrastructure/src/deploy/docker_compose.rs) (atomic temp-sibling + rename), and returns them.
7. The caller seeds them onto each child compose command with [`seed_deploy_secrets(&mut cmd, &secrets)`](crates/infrastructure/src/deploy/docker_compose.rs) before build/up attempts (normal deploys ~line 1005-1022; eviction retry ~1085; preview builds ~1353).

**Adoption rule.** Two deterministic filename derivations exist:
- modern — [`store_file(...)`](crates/infrastructure/src/deploy/docker_compose.rs) hashes `compose_project_name(work_dir)`, invariant under relocation because docker's pgdata named volume `<project>_db` is too (CXA-B032);
- legacy — [`cxb031_store_file(...)`](crates/infrastructure/src/deploy/docker_compose.rs) hashes today's canonicalised absolute path byte-for-byte as CXA-B031 did.

Per key present in F(old): if F(new) differs from or lacks that value we prefer F(old)'s V because V initialised pgdata; P only ever appeared via an intervening buggy-era cycle regenerating against initialized data (this is CXA-B039's fix — B036 had bailed whenever any divergent value already sat at F(new)). Adoption converges in one pass because resolution writes every adopted key straight back into F(new).

**Completion boundary (the fix).** After adoption copies values over, if every key present in F(old) now matches what resolution will persist into F(new), i.e., full convergence ([condition at docker_compose.rs:286]), [`expire_legacy_store(path)`](crates/infrastructure/src/deploy/docker_compose.rs) removes F(old). With no legacy source left there is nothing to resurrect when an admin deletes only F(new); legitimate rotation sticks instead of being reverted indefinitely.

## Usage

There is no user-facing CLI for secrets; behaviour is driven by config precedence plus ordinary deploys.

To rotate a compromised generated credential deliberately:

1. Delete that project's modern store file:
   ```bash
   # default root below; <hash> = sha256 of compose_project_name(work_dir)
   rm "$HOME/.local/share/coxagent/deploy-secrets/<hash>.env"
   ```
2. Trigger any deploy cycle for that project.
3. Verify regeneration took effect on live services:
   ```bash
   docker exec -it <project>_db printenv PG_PASSWORD
   ```

Before CXA-B042 this procedure silently failed when a converged legacy store existed: deleting only the modern file let stale V resurface next cycle.

The regression guard proving intended use is module test [`after_migration_converges_deleting_the_modern_store_regenerates_and_survives()`](crates/infrastructure/src/deploy/docker_compose.rs:~1966), inside `mod deploy_secret_tests`: seeds only F(old); asserts first resolve adopts V then expires F(old); deletes only F(new); asserts second resolve does NOT resurrect stale V but persists freshly-generated P.

## Interface

All identifiers below are private to module scope in [crates/infrastructure/src/deploy/docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs).

| Identifier | Role |
|---|---|
| `resolve_deploy_secrets(work_dir: &Path)` | real-pass entrypoint |
| `resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path)` | testable core of resolution |
| `missing_required_secrets(fn(&str)->bool + dot_env_keys: &HashSet<String>)` | which required keys need fallbacks |
| `seed_deploy_secrets(cmd: &mut Command + secrets:&[(String,String)])` | injects pairs as child env |
| `read_dot_env(path: &Path)->HashSet<String>` | parses operator `.env`, returns non-blank key set |
| [`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored)`](crates/infrastructure/src/deploy/docker_compose.rs) | per-key preference + expire-on-convergence |
| [`expire_legacy_store(path: &Path)`](crates/infrastructure/src/deploy/docker_compose.rs) | best-effort remove of converged F(old); new in CXA-B042 |
| `cxb031_store_file(secret_root, work_dir) -> PathBuf` (tests alias as `legacy_key_file`) | path-derived legacy filename replica |
| `store_file(secret_root, work_dir) -> PathBuf` (tests import under same name) | name-derived modern filename |
| `read_stored_secrets(path: &Path) -> HashMap<String,String>` | parses loose KEY=VALUE lines back out of a store file |
| `write_stored_secrets(path: &Path, secrets: &HashMap<String,String>)` | atomically persists owner-only temp-sibling + rename |
| `repair_store_permissions(secret_root, work_dir)` | chmod-0600 both stores |

No CLI flags exist for this feature; behaviour follows precedence rules plus one env var below.

## Configuration

One setting changes where durable secret state lives:

```text
COXAGENT_DEPLOY_SECRETS_DIR        explicit root dir verbatim      unset by default
```

`deploy_secrets_root()` resolves the directory (docker_compose.rs:171-183) in this order:

```text
if COXAGENT_DEPLOY_SECRETS_DIR set          -> use that dir verbatim;
else if HOME set                            -> $HOME/.local/share/coxagent/deploy-secrets;
else                                        -> $TMPDIR/coxagent-deploy-secrets (/tmp fallback)
```

Store files inside the root are named `<sha256>.env`. Their permissions are repaired owner-only (`0600`) whenever touched via `repair_store_permissions`.

Config precedence is embedded without any env switch:
- process-env-provided secret → left alone;
- project-dir `.env` → left alone;
- absent from BOTH → generate/persist a random fallback.

A blank or whitespace-only value counts as not-provided. An empty result from resolution means every required key was supplied externally.

## Edge cases and limits

What it deliberately does NOT do / failure modes:

1. **Operator-configured values are never clobbered.** A key resolvable from process env or `.env`, filtered by non-blank presence detection — if present we return nothing for it. We never override nor poison it.
2. **Relocated apps do not inherit foreign secrets.** Presence of F(old) at today's canonicalised path proves same-checkout-not-relocated; genuinely moved app falls through fresh.
3. **Convergence must fully match before expiry.** Only when every current-F(old)-key equals persisted candidate does expiry occur; partial divergence keeps adoption authoritative.

## Code map

- `crates/infrastructure/src/deploy/docker_compose.rs` — everything in this feature: `resolve_deploy_secrets_in`, `adopt_legacy_cxb031_secrets`, `expire_legacy_store` (CXA-B042), `cxb031_store_file`, `store_file`, `read_stored_secrets` / `write_stored_secrets`, `repair_store_permissions`, plus the full deploy-secret story from CXA-B017 through CXA-B041 and the regression test in `mod deploy_secret_tests`.

Note: on branch `main` this file is only 1307 lines and has none of these identifiers — the whole feature (including this fix) exists only on branch `feat/CXA-B042` (2090-line version).

## Related

- Ticket CXA-B039 — introduced the per-key legacy preference that CXA-B042 corrects; see its fix commit in branch history.
- Ticket CXA-B036 — first adoption bridge (bailed when any divergent value sat at F(new)); replaced by CXA-B039's rule.
- Tickets CXA-B028 / CXA-B031 / CXA-B032 — origin of out-of-tree secret persistence, path-derived store naming, then compose-project-name keying (the modern derivation).
- Tickets CXA-B033 / CXA-B038 — store permission hardening (`repair_store_permissions`) against world-readable credential files.
- Deploy health gate docs and ticket COX-C012 (`${VAR:?}` required-env interpolation) govern why these secrets exist.
</content>
