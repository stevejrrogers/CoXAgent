FOLDER: Deploy Secrets
# Deploy Secrets
**Keywords:** deploy secret, PG_PASSWORD, fallback secret, name-derived store, path-keyed store, CXA-B036 migration, CXA-B039 adoption, legacy store adoption, compose project name, secret convergence

## Overview
Stable fallback credentials (e.g. `PG_PASSWORD`, `COXAGENT_ADMIN_PASSWORD`) for an unconfigured app-driven deploy are generated once and reused across every deploy cycle so they match what docker's named pgdata volume was initialised with. A migration bridge (`adopt_legacy_cxb031_secrets`) recovers values written by pre-CXA-B032 software under an obsolete path-derived filename into today's name-derived canonical store — even when that canonical store already holds a divergent value (the CXA-B039 bug this page documents).

It is for operators upgrading an already-deployed self-hosted app without re-supplying secrets manually, and for any agent that changes how deploy-secret persistence or migration behaves.

## How it works
`resolve_deploy_secrets_in(work_dir, secret_root)` (`crates/infrastructure/src/deploy/docker_compose.rs:96`) runs on each deploy pass:

1. `repair_store_permissions` hardens any pre-existing off-repo store file to owner-only (CXA-B038).
2. `read_dot_env` + `missing_required_secrets` compute which of the fixed `REQUIRED_SECRET_KEYS` (`["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]`, line 23) are missing from process env **and** project `.env`.
3. If nothing is missing: `retire_superseded_path_keyed_store` removes an obsolete path-keyed duplicate and returns empty (CXA-B040). Otherwise:
4. Read the modern name-derived store via `store_file(secret_root, work_dir)`.
5. Call **`adopt_legacy_cxb031_secrets(secret_root, work_dir, &mut stored)`** — the CXA-B036/CXA-B039 bridge — returning whether it converged.
6. Fill each missing key from `stored`, else mint a fresh random one; persist everything atomically via `write_stored_secrets_owned`.
7. Only after durable persist succeeds does expiry run: if convergence was recorded and the write returned true, `expire_converged_legacy_store` deletes F(old) (CXA-B044 ordering).

### Why it now works where B036 failed (the fix)
The bug ([CXA-B039]): CXA-B036 bailed out of adoption entirely whenever F(new) was non-empty — so an intervening post-switch cycle that minted divergent random values into F(new) made adoption never consult F(old)==V; V alone matches initialised pgdata but was lost permanently.

The fix changed adoption from an all-or-nothing gate to a **per-key preference**: for each key in F(old), insert V whenever F(new)'s current value differs from V or is absent (`stored.get(k) != Some(v)` → overwrite). V is preferred because it is the historical anchor that initialised pgdata; P only appeared via a buggy-era regeneration against initialized data.

Convergence contract: after one fixed pass both files hold equal values; expiry happens only in step 7 after durable persist so both stores are never lost at once ([CXA-B044]).

### Cross-project guard
`read_owner_stamp(F(old))` must equal today's `compose_project_name(work_dir)` before any legacy value is adopted or deleted ([CXA-B043]). A self-stamped or unstamped (pre-stamp-era) file remains eligible for genuine in-place migration; another project's stamped credentials are refused.

## Usage
No operator action needed after upgrade — resolution runs automatically on every compose up for an unconfigured app whose secrets come from fallback generation.

Repro / verification test that documents expected behaviour:

```rust
// crates/infrastructure/src/deploy/docker_compose.rs::post_upgrade_cycle_recovers_
// legacy_value_even_when_new_store_is_non_empty
let mut old = HashMap::new();
old.insert("PG_PASSWORD".to_owned(), "correct-v".to_owned());     // what initialised pgdata
write_stored_secrets(&legacy_key_file(&secret_root,&proj), &old);

let mut new = HashMap::new();
new.insert("PG_PASSWORD".to_owned(), "divergent-p".to_owned());   // B032-era wrong value
write_stored_secrets(&store_file(&secret_root,&proj), &new);

let first = resolve_deploy_secrets_in(&proj,&secret_root);
assert_eq!(pg(first), "correct-v");       // recovered over divergent P
assert_eq!(first,
    resolve_deploy_secrets_in(&proj,&secret_root)); // converges; no churn next pass
```

Run just this area's tests:

```sh
cd crates/infrastructure && cargo test --lib post_upgrade_cycle_recovers_
```

## Interface

Key functions in `crates/infrastructure/src/deploy/docker_compose.rs`:

| Function | Signature | Role |
|---|---|---|
| `resolve_deploy_secrets_in` | `(work_dir: &Path, secret_root: &Path) -> Vec<(String,String)>` | Orchestrator of one resolution/migration pass |
| `resolve_deploy_secrets` | `(work_dir: &Path) -> Vec<(String,String)>` | Thin wrapper using default root |
| `missing_required_secrets` | `<F>(provided_by_env:Fn(&str)->bool,&HashSet<String>) -> Vec<&'static str>` | Pure precedence computation |
| **`adopt_legacy_cxb031_secrets`** | `(&Path,&Path,&mut HashMap<String,String>) -> bool` | Per-key legacy→modern adoption; returns convergence verdict |
| `retire_superseded_path_keyed_store` | best-effort fn removing obsolete dup even when no fallback needed (CXA-B040) |

Paths / filenames:
- Modern canonical store: SHA-256 of **compose project name** → `<root>/<hash>.env`.
- Legacy source F(old): SHA-256 of **canonicalised absolute path** → `<root>/<hash>.env`.
- Both live under `/Users/<u>/.local/share/coxagent/deploy-secrets/<64hex>.env`.

Persisted format:
- First line ownership stamp if present: prefix constant ``OWNER_STAMP_PREFIX`` == ``# owner=`` followed by the owning compose project name.
- Remaining lines are loose ``KEY=VALUE`` entries; comments start with ``#``.
- On-disk project `.env`: KEY=VALUE assignments read by compose interpolation.

Constants:
- ``REQUIRED_SECRET_KEYS`` == ``["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]`` (line 23).

Return semantics you must honour:
- Adoption returns a **bool verdict**, never deletes ([CXA-B044]).
- Expiry calls return nothing (best-effort); failures only log via tracing.

## Configuration

Only one knob changes behaviour:

| Setting | Default | Effect |
|---|---|---|
| env var ``COXAGENT_DEPLOY_SECRETS_DIR`` | unset → `<HOME>/.local/share/coxagent/deploy-secrets`, else temp dir if no HOME (`deploy_secrets_root`, line 196) | Overrides where per-project store files live; used by tests/sandboxes to isolate state |

All other behaviour derives deterministically from a pure function of ``work_dir`` plus operator-provided env / `.env`. There is no per-project toggle to disable migration — presence of an eligible F(old) always drives adoption unless its owner stamp names another project ([CXA-B043]).

## Edge cases and limits

Deliberately NOT done:
- Adoption does not run at all when every required key comes from env or `.env`: step 3 short-circuits into retirement-only ([CXA-B040]), so externally-supplied secrets are never written through the store.
- Migration cannot drag credentials across hosts/apps due to two independent guards: resolution keys on compose project name not disk location ([CXA-B032]), and [CXA-B043] refuses foreign-owned stamped legacy files outright.
- Removal never races persistence [CXA-B044]: deletion waits until adopt-then-durable-persist succeeded atomically.

How it fails:
- A best-effort persist failure leaves F(old) in place so next pass re-adopts and retries — both stores never simultaneously gone.
- Unreadable/missing legacy file ⇒ treated as empty ⇒ adoption returns false ⇒ no-op.
- An unstamped old-store has no owner info ⇒ still eligible (preserves pre-stamp-era in-place upgrades); a wrongly-stamped one is skipped with debug log only.

Limits worth knowing:
- The keys list const fans out into build seeding as well as resolution here (~line 1349 reference).
- No retry throttling beyond recomputing on every deploy pass once infra eviction re-runs interpolation up front (~line 60 comment).

## Code map

This area lives entirely inside one module file:

```
crates/infrastructure/src/deploy/docker_compose.rs —
    resolve_deploy_secrets[_in], adopt_legacy_cxb031_secrets,
    retire_superseded_path_keyed_store,
    expire_converged_legacy_store,
    read_stored_secrets / write_stored_secrets[_owned],
    read_dot_env / read_owner_stamp,
    missing_required_secrets,
    REQUIRED_SECRET_KEYS / OWNER_STAMP_PREFIX consts;
contains the #[cfg(test)] module covering CXA-B039/B040/B043 regression cases.
```

Callers wiring this into builds/seeding sit alongside it (~line 1349); nothing else in infrastructure owns these functions.

## Related

This page documents ticket **CXA-B039**, which fixes the gap left by **CXA-B036**. The whole chain lives in the same module and shares one migration/stability concern, so each is a close relation:

- **[CXA-B031]** — original stability rule: once generated, a fallback secret never changes across deploy cycles; storage placed out-of-tree.
- **[CXA-B032]** — switched `store_file` key derivation from canonicalised absolute path to compose project name (so secret stability tracks docker's named pgdata volume).
- **[CXA-B033]** — era of hardened store-file permissions.
- **[CXA-B036]** — first migration bridge adopting pre-CXA-B032 values; shipped the F(new)-empty-only bug that CXA-B039 fixes.
- **[CXA-B038]** — repair store-file permissions on every resolution pass.
- **[CXA-B040]** — retire superseded path-keyed duplicate even on passes where no fallback secret is needed.
- **[CXA-B042]** / **[CXA-B044]** — completion boundary: expire an already-converged legacy store only after durable persist.
- **[CXA-B043]** — owner-stamp guard preventing adoption of another project's credentials (see the Cross-project guard section above).

Other Wiki pages: none yet live in this space root outside the new "Deploy Secrets" folder; add sibling pages here as downstream deployment plumbing (compose orchestration, host-port assignment) gets documented.
