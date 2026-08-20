FOLDER: Deployment

# Deploy Secret Per-Key Legacy Recovery (CXA-B039)

**Keywords:** legacy adoption, adopt_legacy_cxb031_secrets, per-key recovery, non-empty new store, divergent value, cxb031_store_file, store_file key derivation, PG_PASSWORD auth failure, post-upgrade deploy, CXA-B036 bypass

## Overview

CXA-B039 is a correctness fix to the CXA-B036 deploy-secret migration. As B036 first shipped, its `adopt_legacy_cxb031_secrets` bailed out entirely as soon as **any** value already existed in today's NEW name-derived store — so if an intervening post-CXA-B032 cycle had minted a fresh random `P` into the modern file while old pgdata was still initialised with legacy `V`, adoption never consulted the old path-keyed file and the correct value was lost permanently. B039 widens adoption to **per-key** recovery: each legacy key/value is taken unless today's map already holds that same value verbatim. It is for anyone debugging auth failure after upgrading an agent-deployed stack where a pre-fix deploy may have written into the new store.

## How it works

All logic lives in `crates/infrastructure/src/deploy/docker_compose.rs`. The flow is shared with B036; only one step changed.

1. `DockerComposeDeploy::deploy(work_dir)` → `resolve_deploy_secrets(work_dir)` → [`resolve_deploy_secrets_in(work_dir, secret_root)`](crates/infrastructure/src/deploy/docker_compose.rs:96).
2. If every required secret is supplied externally (env or project `.env`), resolution short-circuits to [`retire_superseded_path_keyed_store`](crates/infrastructure/src/deploy/docker_compose.rs:364) and returns nothing — B040 path.
3. Otherwise it reads today's name-keyed store into an in-memory map via [`read_stored_secrets(&store_file(...))`](crates/infrastructure/src/deploy/docker_compose.rs:125).
4. It calls [`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored)`](crates/infrastructure/src/deploy/docker_compose.rs:298). This is where B039 lives:
   - Reads F(old) = [`cxb031_store_file`](crates/infrastructure/src/deploy/docker_compose.rs:240); empty legacy → no-op returning `false`.
   - [CXA-B043] ownership gate: if F(old)'s owner stamp (`read_owner_stamp`) names a different compose project than this checkout's `compose_project_name`, abort.
   - **The B039 fix** — for each legacy key/value V (lines 315-321): prefer V whenever today's map does not already hold it verbatim (`stored.get(k) != Some(v)`), then insert V over any divergent P currently sitting at F(new).
   - Returns "converged" = non-empty legacy AND every carried key now sits verbatim in `stored`.
5. The fallback loop reuses `stored.get(key)` or mints via [`random_secret()`](crates/infrastructure/src/deploy/docker_compose.rs:35), inserts back.
6. Durable persist via [`write_stored_secrets_owned(&store_file(...), Some(owner), &stored)`](crates/infrastructure/src/deploy/docker_compose.rs:481); **only if** persist succeeded AND adoption reported convergence does [`expire_converged_legacy_store(&cxb031_store_file(...))`](crates/infrastructure/src/deploy/docker_compose.rs:339) remove F(old).

Before B039 the inner loop was replaced by an early exit whenever `stored` (the new-file map) was non-empty; it never reached per-key preference and never even read F(old)'s values once any modern value existed.

## Usage

There is no user-facing flag; recovery runs automatically inside every app-driven deploy pass that needs a fallback secret for an unconfigured project.

```sh
# Reproduce with an isolated store root:
export COXAGENT_DEPLOY_SECRETS_DIR=/tmp/b039-demo
cd <an-unconfigured-project-with-a-compose-file>

# Stage state where an intervening cycle ALREADY wrote a wrong P into F(new),
# while only F(old) holds what initialised pgdata:
#   $COXAGENT_DEPLOY_SECRETS_DIR/<sha256-of-canonical-path>.env : PG_PASSWORD=correct-v
#   $COXAGENT_DEPLOY_SECRETS_DIR/<sha256-of-compose-project-name>.env : PG_PASSWORD=divergent-p

coxagent <deploy-driving command> <project>

# Resolved PG_PASSWORD must equal 'correct-v' (recovered from F(old)), NOT 'divergent-p'
cat "$COXAGENT_DEPLOY_SECRETS_DIR"/<sha256-of-compose-project-name>.env
```

Recovery also converges: run resolve twice and both passes return identical secrets (no per-cycle churn).

## Interface

All identifiers live in crates/infrastructure/src/deploy/docker_compose.rs unless noted:

- fn resolve_deploy_secrets_in(work_dir:, secret_root:) -> Vec<(String,String)> — single-pass precedence + retention + per-key adopt + durable-persist + conditional expiry (~96)
- fn adopt_legacy_cxb031_secrets(secret_root:, work_dir:, stored: &mut HashMap<String,String>) -> bool — **the function CXA-B039 changed**: per-key merge preferring legacy V over divergent current P; ownership-gated; returns "converged" verdict (~298)
- fn cxb031_store_file(secret_root:, work_dir:) -> PathBuf — reconstructed `<sha256-of-canonical-path>.env`, read-only migration source (~240)
- fn store_file(secret_root:, work_dir:) -> PathBuf — modern `<sha256-of-compose_project_name>.env`, where adopted values are persisted (~222)
- fn expire_converged_legacy_store(path:) / retire_superseded_path_keyed_store(...) — deferred/shortcut deletion gated on durable persist ([CXA-B044])
- fn read_stored_secrets(path:) / read_owner_stamp(path:) / write_stored_secrets_owned(...)->bool / random_secret() / repair_store_permissions(...)
- Test alias inside mod deploy_secret_tests (~1713): cxb031_store_file imported as legacy_key_file (~1715)

Regression guard in mod deploy_secret_tests (~1713):

```
post_upgrade_cycle_recovers_legacy_value_even_when_new_store_is_non_empty() // ~2157  [B039]
```

This test stages exactly 'F(new)=divergent-p + F(old)=correct-v', asserts resolution recovers `correct-v`, and that a second resolve returns identical results (proving convergence over rotation).

## Configuration

| Setting | Default | Effect |
|---|---|---|
| Required key present in process env OR project-dir `.env` | n/a | Honoured verbatim; fallback not needed ⇒ skips per-key adoption this pass |
| Required key absent everywhere | n/a | Reuse stable stored value if present; else recover legacy V from F(old) even when F(new) holds divergent P; else freshly minted |
| COXAGENT_DEPLOY_SECRETS_DIR | `<HOME>/.local/share/coxagent/deploy-secrets` (temp dir when HOME unset) | Root directory holding both modern and reconstructed-legacy store files |
| Store ownership stamp | first line "# owner=<compose-project-name>" ([CXA-B043]) | Refuses adopting credentials authored by another project sharing this dir |

No CLI flags or config knobs were added for B039 itself; behaviour follows purely from which files exist plus these env settings.

## Edge cases and limits

What this deliberately does NOT do:

- It does not delete anything inside adopting itself; expiry happens downstream only after durable persist succeeds ([CXA-B044]).
- It will not adopt nor delete a legacy file owned by a differently-named project sharing this directory ([CXA-B043]).
- A genuinely relocated/re-cloned app falls through fresh rather than adopting across hosts.
- Per-key preference still cannot help when NO distinct correct value survives anywhere on disk (both stores empty or both storing the same wrong P); there is no oracle for which of two divergent values matches pgdata other than presence of V in F(old).

How it fails / known limits:

- If durable persistence fails before rename completes non-durable → nothing expires; next converging pass re-adopts and retries rather than losing both stores at once.
- Without upstream CXA-B036/B032 having produced a real distinct V at F(old), there is nothing extra to recover here beyond what ordinary retention already did.
- Convergence depends on write success like all persistence here; unwritable stores degrade toward regeneration ([B027]/[B044] contract).

## Code map

— crates/infrastructure/src/deploy/docker_compose.rs —
Holds ALL derivation/adoption/persistence logic above including resolve_deploy_secrets_in (~96), adopt+expiry pair (~298/~339), retire_superseded_path_keyed_store (~364); consumers DockerComposeDeploy::deploy() (~1142). Unit-test module mod deploy_secret_tests starts ~1713 carrying regression guard post_upgrade_cycle_recovers...non_empty at ~2157.

— crates/app/tests/compose_security_gate.rs —
COX-C012 guard forcing credentials to carry `${VAR:?msg}` marker and forbidding bare datastore host-port binds; defines why PG_PASSWORD / COXAGENT_ADMIN_PASSWORD must be supplied-or-fallback at interpolation — the requirement this adoption chain seeds.

For repository-wide context use GitNexus MCP tools against id cxa targeting symbol names above (e.g., search_symbols("adopt_legacy_cxb"), symbol_refs("store_file")).

## Related

- cxa-b036-deploy-secret-legacy-adoption.md (CXA-B036) — parent ticket whose migration this page widens from whole-store bail-out to per-key recovery; read together.
- cxa-b032-deploy-secret-relocation.md (CXA-B032) — changed store keying from path-derived to compose-project-name, creating the two-store gap this fix bridges.
- cxa-b040 (superseded-store retirement even without fallbacks), cxa-b043-deployment-stability.md-style ownership stamping ([CXA-B043]), cxa-b044 durable-persist ordering ([CXA-B044]) — constrain how/when recovery may converge and delete.
- cxa-b033-deploy-secret-permissions.md (CXA-B038) / repair_store_permissions — hardens both current and legacy stores every pass sharing resolve_deploy_secrets_in.
