FOLDER: Deploy Secrets
# Deploy-secret store permission remediation

**Keywords:** deploy secrets; 0600; world-readable; chmod; set_secret_perms; repair_store_permissions; resolve_deploy_secrets_in; store_file; cxb031_store_file; CXA-B038

## Overview

Hardens pre-existing off-repo deploy-secret store files back to owner-only (`0600`) on every resolution pass, even when an operator now supplies every required secret externally so no fallback secret is persisted that pass. Without it, a store file written world-readable (`0644`) by a pre-CXA-B033 build carrying live superuser credentials would sit lax forever once those creds are fully configured elsewhere. For operators who self-host deployments and for agents maintaining the deploy pipeline.

## How it works

The entry point is `resolve_deploy_secrets_in(work_dir, secret_root)` in `crates/infrastructure/src/deploy/docker_compose.rs`. Its very first statement — before it decides whether any fallback secret is even needed — calls `repair_store_permissions(secret_root, work_dir)`:

```
repair_store_permissions -> set_secret_perms(store_file(...))      // modern NAME-keyed file
                        -> set_secret_perms(cxb031_store_file(...)) // legacy PATH-keyed file
```

- `repair_store_permissions` (docker_compose.rs:559) runs owner-only hardening over both store files that can hold credentials at rest:
  - `store_file()` (docker_compose.rs:222) — the modern per-project NAME-keyed store (SHA-256 of the compose project name).
  - `cxb031_store_file()` (docker_compose.rs:240) — the obsolete pre-CXA-B032 PATH-keyed filename kept as a migration/adoption source.
- `set_secret_perms` (`#[cfg(unix)]`, docker_compose.rs:538) reads the current permissions via `std::fs::metadata`, sets mode to `0o600` on an in-memory copy using `PermissionsExt::set_mode`, then **writes that copy back** with `std::fs::set_permissions`. The comment at docker_compose.rs:540-542 records why this matters for CXA-B038: `metadata()` returns a *copy*, so mutating only that copy without writing it back would mean no file was ever actually chmodded.
- On non-Unix targets it is a no-op (`#[cfg(not(unix))]`, docker_compose.rs:549).

Because repair happens before the empty-missing shortcut at docker_compose.rs:113 (`if missing.is_empty() { ... return Vec::new(); }`), remediation also runs when every required key comes from process env or `.env` and nothing is written this pass. Previously hardening only happened inside persistence paths (`persist_store` -> temp write -> rename), which the externally-supplied case never reaches.

## Usage

No user-facing action or flag. Remediation is automatic on each deploy-resolution pass for any project with an existing off-repo store file whose mode allows group/world access.

The regression test simulates the buggy artifact directly:

```rust
// crates/infrastructure/src/deploy/docker_compose.rs :: mod deploy_secret_tests
// fn world_readable_store_file_is_remediated_even_when_no_fallback_is_needed (#[cfg(unix)])

write_stored_secrets(&store_path, &legacy);          // PG_PASSWORD=legacy-live-secret
chmod_for_test(&store_path, 0o644);                  // simulate pre-fix lax mode

let resolved = resolve_deploy_secrets_in(&proj, &secret_root);
assert!(resolved.is_empty());                        // everything supplied externally
assert_eq!(mode_for_test(&store_path), 0o600);       // STILL remediated to owner-only
```

To verify by hand on Unix:

```bash
ls -l "$HOME/.local/share/coxagent/deploy-secrets/"   # any *.env not owner-only?
# A previously-lax <hex>.env becomes:
-rw------- 1 you staff ... <hex>.env                  # after next resolution pass
```

## Interface

Private functions in module scope of `crates/infrastructure/src/deploy/docker_compose.rs` (no public API surface changed by this ticket):

| Name | Signature | Role |
|------|-----------|------|
| `repair_store_permissions` | `fn(secret_root: &Path, work_dir: &Path)` | Harden both candidate store files to owner-only. |
| `set_secret_perms` | Unix impl writes permissions back to disk; non-Unix impl no-ops | Apply mode $0o600$ from an in-memory permission copy via real setter. |
| `resolve_deploy_secrets_in` | returns fallback key/value pairs given inputs | Resolution entry point whose first statement triggers repair. |
| REQUIRED_SECRET_KEYS | const = `["PG_PASSWORD", "COXAGENT_ADMIN_PASSWORD"]` (docker_compose.rs:23) | Keys whose presence gates whether a fallback path runs at all. |

Targets hardened per pass:
- Modern canonical store path produced by [`store_file()`](docker_compose.rs): `<root>/<sha256-of-compose-project-name>.env`.
- Legacy superseded path produced by [`cxb031_store_file()`](docker_compose.rs): `<root>/<sha256-of-canonical-path>.env`.

## Configuration

None introduced by this ticket. Behaviour follows from existing constants and one env var that already govern where/how stores persist:

- Store location root — default derived in [`deploy_secrets_root()`](docker_compose.rs): `${HOME}/.local/share/coxagent/deploy-secrets`, overridable with env var **`COXAGENT_DEPLOY_SECRETS_DIR`**.
- Required keys list **REQUIRED_SECRET_KEYS** (defaults above).
- Target mode hard-coded as constant **$0o600$** inside [`set_secret_perms`]; not configurable.

There is deliberately no "skip remediation" switch — remediation must always run so credentials never linger world-readable.

## Edge cases and limits

This deliberately does NOT do / how it fails:

- **Best-effort IO.** A missing target file simply isn't present ([`set_secret_perms`](docker_compose.rs) swallows errors), so unreadable/unrepresentable-mode paths fail silently rather than failing a deploy.
- **Non-Unix:** safety-controlled away entirely (`#[cfg(not(unix))]`) where modes don't apply.
- **File bytes only.** Hardening covers FILE contents/permission bits, not necessarily the parent directory's own mode.
- Does NOT delete legacy/superseded files itself; deletion belongs to later siblings guarded by durable-persist ordering ([CXA-B040]/[CXA-B044]).
- Only runs when resolution is reached for a project. If nothing ever calls into deploy secret resolution for an affected project again after upgrade, remediation does not independently sweep all of disk.

## Code map

An agent should find everything relevant without searching:

| Path | What lives there |
|------|------------------|
| crates/infrastructure/src/deploy/docker_compose.rs — single module implementing DockerComposeDeploy incl.: REQUIRED_SECRET_KEYS (:23); resolve_deploy_secrets/resolve_deploy_secrets_in (:92/:96); repair hook call (:106); missing_required_secrets (:72); read_dot_env (:167); deploy_secrets_root (:196); stored-file helpers store_file/cxb031_store_file (:222/:240); adopt_legacy_cxb031_expiry commits expire_converged_legacy_expiry/store-file writer persist/persist-store persist fns close out CXA-B044 ordering further down same region |

Note colons denote line numbers at time of writing against tip state checked out under [git log d5ca23c]. Line drift acceptable across merges during parallel development — use ripgrep keywords B038 / B040 / B043 within the crate instead of relying on fixed offsets.

Related tests live in the same file under mod deployment test region with white-box access:
world_readable_case ~(:2043), superseded case ~(:2101). Fixture helpers chmod_for_test/mode_for_test stage modes ~(:1728).

## Related

Other Wiki pages / tickets referenced together in comments within this chain:
[CXA-F021](../Configuration/CXA-F021.md)
