FOLDER: -
# CXA-B036 Legacy Secret Migration into the Name-Derived Store

**Keywords:** resolve_deploy_secrets_in, adopt_legacy_cxb031_secrets, cxb031_store_file, store_file, legacy path-keyed secret store, CXA-B032 key-derivation switch, PG_PASSWORD resurrection auth failure, retire_superseded_path_keyed_store, owner stamp CXA-B043

## Overview

CXA-B036 is a deploy-secret *migration bridge*. Before CXA-B032 changed how fallback secrets are persisted — from hashing a project's absolute filesystem path to hashing its docker compose project *name* — an already-deployed unconfigured app kept its generated `PG_PASSWORD` / `COXAGENT_ADMIN_PASSWORD` under a **path-derived** filename (SHA-256 of the canonicalised absolute path). After upgrading past B032 that value lived at a filename resolution no longer reads; resolving only today's **name-derived** store found nothing and minted fresh random secrets against Postgres volumes already initialised with the old ones — resurrecting DB/admin auth failure on every such upgrade.

This page documents that migration in its current (hardened) form. Adoption now runs through CXA-B039/CXA-B040/CXA-B042/CXA-B043/CXA-B044 as well as the original B036 rule, so it recovers pre-switch values even when today's store already holds divergent data, converges in one pass instead of rotating forever, never drags in another project's credentials, and never deletes either file until values are durably persisted. It is for anyone debugging post-upgrade PG/admin auth failures or editing the deploy-secret resolution code.

## How it works

Secret resolution for one deploy pass runs through [`resolve_deploy_secrets_in(work_dir, secret_root)`](crates/infrastructure/src/deploy/docker_compose.rs), called by [`resolve_deploy_secrets(work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs) using [`deploy_secrets_root()`](crates/infrastructure/src/deploy/docker_compose.rs) as root. Order of operations:

1. **[`repair_store_permissions(secret_root, work_dir)`]** hardens any pre-existing off-repo store file (both modern and legacy) to owner-only (`0o600`) first — before deciding whether fallback secrets are even needed (CXA-B038).
2. **[`missing_required_secrets(...)`]** computes which of [`REQUIRED_SECRET_KEYS`] (`PG_PASSWORD`, `COXAGENT_ADMIN_PASSWORD`) are provided neither by process env nor by `<project>/.env`. If none are missing:
   - Call **[`retire_superseded_path_keyed_store(...)`]** (CXA-B040): an obsolete path-keyed duplicate left by earlier software must be adopted into the canonical store and removed even when no fallback is needed.
   - Return an empty list immediately.
3. Otherwise read today's name-derived store via [`read_stored_secrets(&store_file(secret_root, work_dir))`]. If it was empty (original B036 case) or holds divergent values (the B039 case), **[`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored)`]** merges values from yesterday's path-keyed file [`cxb031_store_file(...)`], preferring each legacy value whenever today's stored value differs from or is absent for that key.
4. For every still-missing key take its stored value or generate once via [`random_secret()`], then persist with **[`write_stored_secrets_owned(&store_file(...), Some(owner), &stored)`]** (atomic temp-sibling + rename; writes an ownership-stamp first line).
5. Only if persistence succeeded AND adoption converged does **[`expire_converged_legacy_store(&cxb031_store_file(...))`]** delete yesterday's file (CXA-B044).

### Key derivations

- **[`store_file(root, work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs)** — TODAY'S canonical location: SHA-256 over [`compose_project_name(work_dir)`] only (`cox-<parent>-<dir>`, sanitized/truncated to 60 chars). Because it keys on what docker names a named volume by (`<project>_db`) rather than disk location, it survives relocating/cloning a checkout between hosts while distinguishing unrelated projects.
- **[`cxb031_store_file(root, work_dir)`](crates/infrastructure/src/deploy/docker_compose.rs)** — LEGACY predecessor filename: SHA-256 over today's canonicalised absolute path (with a plain-path fallback when `canonicalize()` fails). It replicates byte-for-byte what pre-CXA-B032 wrote so an already-deployed app can still be located during upgrade windows.

### Why recovery converges

Adoption never rotates forever because every adopted key is written straight back into F(new) on step 4; after one fixed run both files hold equal values and stay equal thereafter — asserted directly in `post_upgrade_cycle_recovers_legacy_value_even_when_new_store_is_non_empty`. A second resolve yields identical output instead of churning credentials each cycle.

## Usage

This has **no end-user CLI or API surface** — it runs automatically inside every app-driven docker compose deploy when some required secret isn't supplied externally ([docker_compose.rs:1161](crates/infrastructure/src/deploy/docker_compose.rs)). An operator observing DB/admin auth failure after upgrading past CXA-B032 should simply re-run deployment once with nothing configured; resolution adopts yesterday's persisted value instead of regenerating:

```sh
unset PG_PASSWORD COXAGENT_ADMIN_PASSWORD      # simulate "unconfigured" resolution
coxagent run --project <dir> ...                # first post-upgrade pass adopts legacy creds
```

To see which files resolution consults without deploying:

```sh
echo "$COXAGENT_DEPLOY_SECRETS_DIR"            # default ~/.local/share/coxagent/deploy-secrets
# F(new) = $root/<sha256-of-compose-project-name>.env   -> modern name-derived store
# F(old) = $root/<sha256-of-canonicalised-path>.env     -> legacy path-keyed source being migrated
```

Rotation caveat worth knowing: because convergence **expires F(old)** once durable ([CXA‑B042]), deleting only F(new) afterwards genuinely forces new credentials rather than resurrecting stale V from F(old).

## Interface

Functions implementing this chain in [docker_compose.rs](crates/infrastructure/src/deploy/docker_compose.rs):

```
fn resolve_deploy_secrets(work_dir: &Path) -> Vec<(String,String)>                    // public entry point
fn resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path)
    -> Vec<(String,String)>                                                           // pure decision core under test
fn missing_required_secrets(
    provided_by_env: impl Fn(&str)->bool,
    dot_env_keys: &HashSet<String>) -> Vec<&'static str>                              // pure predicate over sources
fn adopt_legacy_cxb031_secrets(
    secret_root,&Path , work_dir,&mut stored HashMap)->bool                            // per-key merge; verdict only
fn retire_superseded_path_keyed_store(secret_root,&Path , work_dir)                    // C040 empty-shortcut removal gate
fn expire_converged_legacy_store(path:&Path )                                          // C044 durable deletion boundary
pub(crate) fn write_stored_secrets_owned(
    path:,owner Option<&str>,secrets &HashMap)->bool                                  // durable atomic persist + stamping[^1]
pub(crate) fn compose_project_name(dir:&Path)->String                                  // hash input + owner stamp + volume anchor[^2]
#[cfg(test)] fn write_stored_secrets(path:,secrets)→bool                              // unstamped variant used by fixtures[^1]
```

Constants / helpers referenced:

```
const REQUIRED_SECRET_KEYS : &[&str] = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"];
const OWNER_STAMP_PREFIX   : &str  = "# owner=";
fn repair_store_permissions(secret_root,&Path , work_dir);                             // 0600 remediation both stores[^3]
```

[^1]: Production persistence always goes through owned/stamped writes (`write_stored_secrets_owned → persist_store`); unstamped writing survives only for test fixtures/pre-stamp stores.
[^2]: Deterministic `cox-<parent>-<dir>` identity defining docker project naming; doubles as ownership-stamp author and pgdata-volume anchor.
[^3]: Hardening step runs regardless of whether any fallback persists this pass (CXA‑B038).

The two observable filenames under `deploy-secret-root`: `<sha-of-compose-name>.env` vs `<sha-of-canonicalised-path>.env`.

## Configuration

| Setting | Effect | Default |
|---|---|---|
| Process env e.g. `PG_PASSWORD`, non-blank | Satisfies "provided", excludes key from fallback generation | none |
| `<project>/.env`, non-blank assigned keys | Satisfies "provided", excludes keys | none |
| Missing-from-both keys | Reused-from-store or freshly random each pass | regenerated once per project if not persisted |
| Fallback generator [`random_secret()`] | 32-char cryptorandom alphabet for new un-persisted secrets | N/A |
| Secret-store root ([`deploy_secrets_root()`]) | Where both `.env.storekey`s live during migration window; outside any source tree (CXA‑B028) | `$HOME/.local/share/coxagent/deploy-secrets`, else system temp |

There are no runtime switches controlling adoption itself — precedence across sources rules all behaviour ([mirrors Compose interpolation precedence documented on sibling pages]).

## Edge cases and limits

What this deliberately does NOT do:

- It does not copy credentials across hosts or relocated checkouts without evidence they belong here (**CXA‑B043**): adoption refuses any legacy file whose ownership stamp names a different compose project than today's; self-stamped-or-unstamped files stay eligible so genuine in-place upgrades proceed.
- It never deletes either side inside adoption itself (**CXA‑B044**): removal happens only after durability confirmed via atomic rename+persist downstream gates.
- When every required secret comes externally supplied we short-circuit to retirement/adoption-only handling (**CXA‑B040**) rather than running full generation.
- Filesystems that cannot represent Unix modes skip permission hardening gracefully rather than failing deploys.

Failure modes degrade safely toward regeneration rather than trusting unknown sources; best-effort IO problems log warnings without crashing a deploy pass.

## Code map

All implementation lives in one module plus its inline regression tests:

- crates/infrastructure/src/deploy/docker_compose.rs lines ~23–45 — constants including `REQUIRED_SECRET_KEYS`, `OWNER_STAMP_PREFIX`, plus the unguessable-alphabet generator shared by resolver paths.
- crates/infrastructure/src/deploy/docker_compose.rs lines ~47–81 — missing-source predicate forming the fallback decision core.
- crates/infrastructure/src/deploy/docker_compose.rs lines ~83–152 — resolve entry/core incl early-empty shortcut calling retirement gate then adoption+durable-persist+expiry ordering.
- crates/infrastructure/src/deploy/docker_compose.rs lines ~190–254 / 662–679 — two deterministic derivations switching between modern-vs-legacy filenames per semantics above (`compose_project_name`, sanitization rules).
- crates/infrastructure/src/deploy/docker_compose.rs lines ~296–420 — adopt verdict function + durable-expiry + superseded-retirement gates incl owner-stamp checks backing cross-project refusal.
- crates/infrastructure/src/deploy/docker_compose.rs lines ~422–570 – atomic owned/un-owned persists temp-sibling rename style hardening perms repairs described earlier stepwise.
- crates/infrastructure/src/deploy/docker_compose.rs `#[cfg(test)]` module around lines 1700–2395 — the inline regression suite, including `first_post_upgrade_cycle_reuses_the_pre_cxb032_persisted_value` (B036), `post_upgrade_cycle_recovers_legacy_value_even_when_new_store_is_non_empty` (B039), `superseded_path_keyed_store_is_adopted_and_removed_even_when_all_secrets_are_external` (B040), `after_migration_converges_deleting_the_modern_store_regenerates_and_survives` (B042), and `cross_project_legacy_store_is_not_adopted_but_own_is` (B043).
- crates/infrastructure/src/deploy/mod.rs — declares `pub mod docker_compose;` and re-exports the adapter `DockerComposeDeploy` for consumers; the secret-resolution helpers themselves stay private to this module.

## Related

- docs/wiki/engineering/deployment/cxa-b010-pg-password-missing.md — documents the `${PG_PASSWORD}` interpolation error this migration exists to prevent from recurring after upgrade; shares the same secret keys and precedence sources.
- docs/wiki/engineering/deployment/cxa-b001-docker-compose-deploy-failure.md — sibling deploy page on compose project-name collisions and port handling; uses [`compose_project_name`] in the same file.
- Ticket chain CXA-B030 → B031 → B032 → B036 → B039 → B040 → B042 → B043 → B044 — sequential hardening of generated-secret persistence: secret stability keying (B031), switch to name-derived files (B032), legacy adoption when F(new) empty (B036) then non-empty-divergent (B039), retirement on no-fallback passes (B040), rotation-after-convergence boundary (B042), cross-project ownership stamping (B043), durable-persist-before-delete ordering (B044).

