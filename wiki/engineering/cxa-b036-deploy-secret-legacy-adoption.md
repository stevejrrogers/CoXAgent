FOLDER: Deployment

# Deploy Secret Legacy Adoption (CXA-B036)

**Keywords:** legacy adoption, adopt_legacy_cxb031_secrets, path-keyed store, cxb031_store_file, store_file key derivation, PG_PASSWORD regenerated, pgdata volume auth failure, post-upgrade first deploy, secret store migration

## Overview

CXA-B036 is the code ticket that supplies a **migration** when `store_file()`'s hash input changes. CXA-B032 re-keyed persistence from SHA-256 of the canonicalised absolute path (CXA-B031) to SHA-256 of the compose project name; on an in-place upgrade that left any already-deployed unconfigured app orphaned — its PG_PASSWORD sat at B031's old path-derived filename while resolution read only B032's new name-derived one — so the **first post-upgrade cycle regenerated fresh random credentials against Postgres data already initialised with the old value**, resurrecting DB/admin auth failure on exactly the deployments this chain exists to stabilise. This page documents how legacy path-keyed secrets are located and adopted (`adopt_legacy_cxb031_secrets`), why they are never deleted inside adoption ([CXA-B044]), and how a later refinement (CXA-B039) widened recovery beyond what B036 first shipped. It is for anyone changing key derivation or debugging auth failure after upgrading an agent-deployed stack.

## How it works

All logic lives in `crates/infrastructure/src/deploy/docker_compose.rs`. Each pass runs `DockerComposeDeploy::deploy(work_dir)` → `resolve_deploy_secrets(work_dir)` → `resolve_deploy_secrets_in(work_dir, secret_root)`, which computes required secrets once per pass.

Two filename derivations matter:

- **Modern home** — [`store_file(secret_root, work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs:222) hashes [`compose_project_name(work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs:685) (`cox-<parent>-<dir>`, basenames only → invariant under relocation), producing `<digest>.env`. New secrets are written here.
- **Legacy source** — [`cxb031_store_file(secret_root, work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs:240) reconstructs B031's old filename byte-for-byte: SHA-256 of the *canonicalised absolute path* (with `.canonicalize()` fallback to the raw path on failure). It is a **read/migration source only** — never where new secrets get written.

Inside [`resolve_deploy_secrets_in`](crates/infrastructure/src/deploy/docker_compose.rs:96), after computing which keys are missing:

1. Read today's name-keyed store into an in-memory map (`read_stored_secrets(&store_file(...))`).
2. Call [`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored)`](crates/infrastructure/src/deploy/docker_compose.rs:298):
   - Reads F(old); empty legacy → no-op returning `false`.
   - [CXA-B043] ownership gate: if F(old)'s owner stamp (`read_owner_stamp`) names a different compose project than this checkout's [`compose_project_name`], abort adoption entirely.
   - For each key/value V in F(old): prefer V whenever today's map does not already hold it verbatim (`stored.get(k) != Some(v)`). This per-key preference recovers V even when an intervening cycle minted divergent P at F(new).
   - Returns a boolean "converged" verdict meaning every legacy key now sits verbatim where resolution will persist it.
3. The fallback loop resolves each missing key by reusing `stored.get(key)` or minting via `random_secret()`, then inserts back into `stored`.
4. Persist durably via [`write_stored_secrets_owned(&store_file(...), Some(owner), &stored)`](crates/infrastructure/src/deploy/docker_compose.rs:481) (atomic temp-sibling + rename + 0o600 hardening; ownership-stamp first line).
5. **Only if durable persist succeeded AND adoption reported convergence** does [`expire_converged_legacy_store(&cxb031_store_file(...))`](crates/infrastructure/src/deploy/docker_compose.rs:339) remove F(old).

[CXA-B044] ordering is load-bearing here: adoption itself never deletes; expiry happens only downstream after resolution has durably persisted exactly what convergence depends on. If persist fails before rename completes non-durable → returns non-true → nothing expires; F(old) stays so next pass re-adopts and retries rather than losing both stores at once.

History note / why CXA-B039 exists separately from this page: as first shipped here, B036 adopted legacy values ONLY while nothing existed yet at today's name-derived location — if any intervening deploy cycle between B032 and B036 had minted fresh random values into F(new), adoption bailed out early and never consulted F(old), losing V permanently even though it alone matched initialised pgdata. CXA-B039 widened this to per-key recovery regardless of whether F(new) holds a divergent pre-fix value; current code implements that wider rule.

## Usage

There is no user-facing flag or command for adoption; it runs automatically inside every app-driven deploy pass that needs a fallback secret for an unconfigured project.

```sh
# Reproduce with an isolated store root:
export COXAGENT_DEPLOY_SECRETS_DIR=/tmp/b036-demo
cd <an-unconfigured-project-with-a-compose-file>

# Simulate pre-CXA-B032 state by staging ONLY a legacy path-derived store:
#   write $COXAGENT_DEPLOY_SECRETS_DIR/<sha256-of-canonical-path>.env containing
#   PG_PASSWORD=stable-b031-password
#
# Run one post-upgrade deploy cycle:
coxagent <deploy-driving command> <project>

# Confirm NO regeneration happened — resolved value must equal the staged one,
# not a fresh random secret:
cat "$COXAGENT_DEPLOY_SECRETS_DIR"/<sha256-of-compose-project-name>.env
```

Because both derivations are deterministic functions of `work_dir`, an in-place upgrade always finds precisely which file B031 wrote for it; presence of F(old) at *today's* canonicalised path is itself proof this checkout was not relocated.

## Interface

All identifiers live in crates/infrastructure/src/deploy/docker_compose.rs unless noted:

- const OWNER_STAMP_PREFIX = "# owner=" — first-line ownership stamp written by persist_store / read back by read_owner_stamp ([CXA-B043])
- fn resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path) -> Vec<(String,String)> — single-pass precedence + retention + adopt + durable-persist + conditional expiry (~96)
- fn store_file(secret_root:, work_dir:) -> PathBuf — modern `<sha256-of-compose_project_name>.env`
- fn cxb031_store_file(secret_root:, work_dir:) -> PathBuf — reconstructed `<sha256-of-canonical-path>.env`, read-only migration source
- fn adopt_legacy_cxb031_secrets(...) -> bool — per-key merge preferring legacy V over divergent current P; ownership-gated; returns "converged" verdict only (~298)
- fn expire_converged_legacy_store(path:) / retire_superseded_path_keyed_store(...)/()— deferred/shortcut deletion gated on durable persist ([CXA-B044])
- fn read_stored_secrets(path:) -> HashMap<String,String> / read_owner_stamp(path:) -> Option<String> / write_stored_secrets_owned(...)->bool / persist_store(...)->bool / set_secret_perms(path)
- fn repair_store_permissions(secret_root:, work_dir:) — hardens both current and legacy files every pass ([CXA-B038])
- Test alias inside mod deploy_secret_tests (~1713): cxb031_store_file imported as legacy_key_file (~1715)

Regression guards live in mod deploy_secret_tests (~1713):

```
first_post_upgrade_cycle_reuses_the_pre_cxb032_persisted_value()            // ~2002  [B036]
post_upgrade_cycle_recovers_legacy_value_even_when_new_store_is_non_empty() // ~2157  [B039]
superseded_path_keyed_store_is_adopted_and_removed_even_when_all...         // ~2101  [B040]
```

## Configuration

| Setting | Default | Effect |
|---|---|---|
| Required key present in process env OR project-dir `.env` | n/a | Honoured verbatim; no fallback needed ⇒ skip adoption this pass |
| Required key absent everywhere | n/a | Stable stored value reused if present; else legacy value adopted from F(old); else freshly minted |
| COXAGENT_DEPLOY_SECRETS_DIR | `<HOME>/.local/share/coxagent/deploy-secrets` (temp dir when HOME unset) | Root directory holding both modern and reconstructed-legacy store files |
| Store ownership stamp | first line "# owner=<compose-project-name>" ([CXA-B043]) | Refuses adopting credentials authored by another project sharing this dir |

No CLI flags or config knobs were added for B036 itself; behaviour follows purely from which files exist plus these env settings.

## Edge cases and limits

What this deliberately does NOT do:

- It does not delete anything inside adopting itself; expiry happens only downstream after durable persist succeeds ([CXA-B044]).
- It does not write adopted/generated values anywhere inside `<project>/codebase/.env` or otherwise into source ([CXA-B028] guard).
- It will not adopt nor delete a legacy file owned by a differently-named project sharing this directory ([CXA-B043]) — prevents one project dragging another's credentials.
- A genuinely relocated/re-cloned app falls through fresh rather than adopting across hosts (presence of F(old) at today's canonical path proves non-relocation).

How it fails / known limits:

- If durable persistence fails before rename completes non-durable (`write_stored_secrets... == false`) → nothing expires; next converging pass re-adopts and retries rather than losing both stores.
- The stability guarantee holds only while persistence succeeds; an unwritable store silently degrades toward per-pass regeneration ([B027]/[B044] contract).
- As originally shipped here (pre-CXA-B039), recovery bailed out when today's name-derived store was already non-empty with divergent values; current code recovers those via per-key preference.

## Code map

— crates/infrastructure/src/deploy/docker_compose.rs —
Holds ALL derivation/adoption/persistence logic above including resolve_deploy_secrets_in (~96), adopt+expiry pair (~298/~339), retire_superseded_path_keyed_store (~364); consumers DockerComposeDeploy::deploy() (~1142). Unit-test module mod deploy_secret_tests (starts ~1713 carries regression guards cited above.)

— crates/app/tests/compose_security_gate.rs —
COX-C012 guard that forces credentials to carry the `${VAR:?msg}` marker and forbids bare datastore host-port binds; defines why PG_PASSWORD / COXAGENT_ADMIN_PASSWORD must be supplied-or-fallback at interpolation, i.e. the requirement this adoption chain seeds.

For repository-wide context use GitNexus MCP tools against id cxa targeting symbol names above (e.g., search_symbols("adopt_legacy_cxb"), symbol_refs("store_file")).

## Related

- cxa-b032-deploy-secret-relocation.md (CXA-B032) — changed store keying from path-derived to compose-project-name; this page is the migration bridge back for deployments B032 left behind, so the two are read together.
- cxa-b031-deploy-secret-rotation.md / cxa-b031-deploy-secret-stability.md (CXA-B031) — first made generated secrets durable out-of-tree under the now-legacy path-keyed filename (`cxb031_store_file`).
- deploy-secret-stability.md (CXA-B027) — incident: per-cycle rotation breaking auth on initialised volumes; the failure mode every ticket here guards against returning.
- cxa-b033-deploy-secret-permissions.md (CXA-B038) — hardens both current and legacy stores every pass, sharing resolve_deploy_secrets_in.
- CXA-B039 / CXA-B040 / CXA-B043 / CXA-B044 — adoption recovery widening, superseded-store retirement even without fallbacks, ownership stamping, and durable-persist ordering that constrain how and when legacy files may be deleted.
- docker-compose-deploys.md (CXA-B001) — end-to-end deploy lifecycle on this same adapter.
