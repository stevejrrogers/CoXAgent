FOLDER: Deployment

# Deploy Secret Stability Under Relocation (CXA-B032)

**Keywords:** secret stability, filesystem path keying, compose project name, pgdata volume auth failure, relocate checkout, re-clone deploy, PG_PASSWORD regenerated, store_file key derivation, deploy secrets relocation

## Overview

CXA-B032 fixes a stable-secrets bug introduced when generated deploy secrets were first made durable (CXA-B031): they were persisted under a store file **keyed by SHA-256 of the checkout's canonicalised absolute path**. Because that key changes whenever an app-driven deploy moves on disk — a fresh clone, or an agent slot re-cloned at a new path — an unconfigured app resolved to an *empty* store and regenerated fresh random credentials against Postgres data that had been initialised with the old ones. Result: DB/admin auth failure on relocation even though "stable" secrets were supposedly in place. The fix re-keys persistence by **compose project name** (basenames only), which is invariant under relocation and matches exactly how docker names its pgdata volume (`<project>_db`). This page is for anyone changing `store_file()` key derivation or diagnosing auth failure after moving/cloning an agent-driven stack.

## How it works

All logic lives in `crates/infrastructure/src/deploy/docker_compose.rs`. Per pass, `DockerComposeDeploy::deploy(work_dir)` calls `resolve_deploy_secrets(work_dir)` → `resolve_deploy_secrets_in(work_dir, secret_root)`, which reads/writes one out-of-tree store file whose location is decided by two functions:

- **`store_file(secret_root, work_dir)` — today's (B032) canonical key.** SHA-256 of `compose_project_name(work_dir)` appended as `<hex>.env` under `${COXAGENT_DEPLOY_SECRETS_DIR}`.
- **`cxb031_store_file(secret_root, work_dir)` — legacy pre-B032 key.** SHA-256 of `work_dir.canonicalize()` (with path-fallback on failure) appended as `<hex>.env`. This is now only a *read/migration source*, never where new secrets are written.

The pivot is `compose_project_name(work_dir)`:

```
name = "cox-" + sanitize(parent-basename) + "-" + sanitize(dir-basename)
       truncated to 60 chars; trailing '-' trimmed
```

It derives from **basenames only**, so two checkouts of the same logical app at different absolute roots (`/tmp/a/reloc/app` vs `/tmp/b/reloc/app`) produce the *same* project name and therefore resolve to the *same* store file — while genuinely distinct projects keep separate files. Docker persists postgres data in a named volume keyed by that same compose project identity (`<project>_db`), so secret stability now tracks exactly what docker uses to persist pgdata.

When a fallback secret is needed (missing from both process env and project-dir `.env`, see `missing_required_secrets`), resolution reads it back from this relocated-stable store before ever calling `random_secret()`. A relocated app therefore reuses its prior PG_PASSWORD rather than minting one Postgres can't honour.

The switch also created upgrade-migration obligations handled by sibling tickets (adoption via `adopt_legacy_cxb031_secrets`, retirement via `retire_superseded_path_keyed_store`) — those are covered on their own pages but depend on this exact pair of filename derivations matching byte-for-byte.

## Usage

There is no user-facing flag; B032 behaviour is automatic inside every app-driven deploy once any required secret is absent everywhere.

```sh
# Point resolution at an isolated store so you can watch relocation without HOME:
export COXAGENT_DEPLOY_SECRETS_DIR=/tmp/demo-secrets

# Same logical unconfigured app deployed from two different absolute roots:
mkdir -p /tmp/a/proj /tmp/b/proj      # identical basename pair at different roots
cd /tmp/a/proj && <run-a-deploy-cycle> # mints & stores PG_PASSWORD under proj's digest
cd /tmp/b/proj && <run-a-deploy-cycle> # SAME digest -> SAME PG_PASSWORD reused

ls "$COXAGENT_DEPLOY_SECRETS_DIR"      # one .env whose digest equals for both clones
```

Expected result: both cycles resolve identical values despite different paths; no regeneration occurs against already-initialized pgdata.

## Interface

Identifiers live in crates/infrastructure/src/deploy/docker_compose.rs unless noted:

- fn compose_project_name(work_dir: &Path) -> String — basename-only identity (`cox-<parent>-<dir>`); truncates to 60 chars; trims trailing '-'; lowercases and maps non-alphanumerics to '-'
- fn store_file(secret_root: &Path, work_dir: &Path) -> PathBuf — canonical B032 key: SHA-256 of compose_project_name → `<digest>.env`
- fn cxb031_store_file(secret_root: &Path, work_dir: &Path) -> PathBuf — legacy pre-B032 key: SHA-256 of canonicalised abs path → `<digest>.env`
- fn resolve_deploy_secrets_in(work_dir: &Path, secret_root: &Path) -> Vec<(String,String)> — reads/writes via these keys per pass
- fn write_stored_secrets_owned(path: &Path, owner: Option<&str>, secrets: &HashMap<String,String>) -> bool — persists canonically with ownership stamp; fn write_stored_secrets(path, secrets) — un-stamped variant; both delegate to persist_store(path, owner_stamp, secrets)
- fn persist_store(path, owner_stamp: Option<&str>, secrets) -> bool — atomic temp-sibling + rename; fn set_secret_perms(path) 0o600; fn repair_store_permissions(secret_root, work_dir); fn read_stored_secrets(path)
- fn adopt_legacy_cxb031_secrets(...)-> bool; retire_superseded_path_keyed_store(...); expire_converged_legacy_store(path)
- fn missing_required_secrets(...)-> Vec<&'static str>; random_secret() -> String
- const REQUIRED_SECRET_KEYS = ["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]

Consumers of resolved+seeded values: DockerComposeDeploy::deploy() and compose_build_check() via seed_deploy_secrets().

## Configuration

| Setting | Default | Effect |
|---|---|---|
| Checkout location / absolute path | n/a | Deliberately NOT part of the store key since B032; relocating must not change resolved secrets |
| Parent + dir basenames | n/a | The sole input to compose_project_name; equal basenames ⇒ same store ⇒ same secrets |
| COXAGENT_DEPLOY_SECRETS_DIR | `<HOME>/.local/share/coxagent/deploy-secrets` (temp dir if HOME unset) | Root holding each `<digest>.env` produced by these keys |
| Required keys present in env or `.env` | n/a | Honour-or-generate precedence unchanged by B032 |

No CLI flags or config-file knobs were added for B032 itself; behaviour is driven entirely by which values are present plus these env settings.

## Edge cases and limits

What this deliberately does NOT do:

- It does not make two *genuinely distinct* projects share credentials even if their base dirs resemble each other — basenames still differ ⇒ separate digest files.
- It does not carry credentials across hosts when a checkout truly loses its basename identity (e.g., renamed parent/dir); such an app falls through fresh rather than silently reusing another host's value.
- It does not alone migrate pre-existing deployments stored under the legacy path-keyed filename — that requires adoption (`adopt_legacy_cxb031_secrets`, CXA-B036/B039/CXA-B043).

How it fails / known limits:

- If composed names collide after truncation/sanitisation there could be sharing between unrelated projects that happen to share basenames within 60 chars; acceptable given docker's own volume naming uses comparable identities.
- Relocation robustness holds only while parent+dir basenames are preserved; renaming either changes identity and starts over.
- Legacy deletion stays best-effort and ordered after durable persist ([CXA-B044]); until then both old/new files can coexist safely because F(old) is read-only here.
- The regression guard simulates relocation as two temp roots with identical basename pairs `/reloc/app`, asserting equal resolved secrets across paths (`secrets_survive_relocating_the_app_between_paths`).

## Code map

All paths relative to repo root:

- crates/infrastructure/src/deploy/docker_compose.rs — DockerComposeDeploy adapter body; key-derivation pair `store_file` (line ~222) vs legacy `cxb031_store_file` (line ~240) plus `compose_project_name` (line ~685), `resolve_deploy_secrets_in` (line ~96), `missing_required_secrets` (~72), `random_secret`, `read_dot_env` (~167). Unit-test module mod deploy_secret_tests (starts line ~1713) holds the B032 regression guard secrets_survive_relocating_the_app_between_paths (line ~1945).
- crates/app/src/builders.rs — build_auth() reads COXAGENT_ADMIN_USER / COXAGENT_ADMIN_PASSWORD into bootstrap_admin(); admin-login half that must agree with whatever stable password resolution seeds after relocation.
- wiki Engineering → Deployment → deploy-secret-stability.md, cxa-b030-deploy-secret-stability.md, cxa-b031-deploy-secret-stability(.md variants) — sibling docs for this ticket chain; overlap in the resolution flow, distinct focus on relocation here.

## Related

- CXA-B031 — made generated secrets durable out-of-tree; its path-keyed store is exactly the bug B032 re-keys away from (legacy filename kept as `cxb031_store_file`).
- CXA-B036 / CXA-B039 / CXA-B043 / CXA-B044 — adoption and expiry ordering that migrate pre-B032 path-keyed stores safely into the relocated-stable canonical store.
- CXA-B040 — retires superseded path-keyed duplicates even when no fallback is needed.
- wiki Engineering → Deployment → deploy-secret-stability.md (CXA-B027) — honour-or-generate precedence and durability machinery this keying plugs into.
- wiki Engineering → Deployment → docker-compose-deploys.md (CXA-B001) — end-to-end deploy lifecycle on this same adapter.
