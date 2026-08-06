# Handoff: finish REST-store opt-in wiring (STEP C)

Branch: `rest-runner` (base: main ab18c90). Repo builds GREEN + clippy clean.
Work left is ONE small wiring edit to make the runner optionally use the
control-plane gateway over REST instead of opening Postgres/Redis directly.

## Already done & committed (verified clean)
- a000007  P1  `RestStateStore` adapter implements full `StateStorePort`
            (load/save/claim_ticket, leader+stage leases, worker heartbeat,
            operator lock). Conflicts -> HTTP 409 -> `PortError::Conflict`.
- c0095ac  P2  Gateway endpoint `POST /api/projects/:pid/store?op=...`
            dispatches every op onto this hub's own store adapter.
- 51e37b1  P3A `AnyStateStore::Rest(RestStateStore)` variant + dispatch arms;
            rest_store compiled unconditionally; re-exported at crate root.

One earlier attempt added then reverted a lib.rs import; tree is currently
clean at these commits. Nothing uncommitted of value remains.

## What remains: make_store opt-in branch (~8 lines)

1) crates/app/src/lib.rs line ~21 (`use coxagent_infrastructure::{ ... }`),
   add two names so they reach builders via `use super::*`:
       JsonStateStore, RestConfig, RestStateStore, SqlStateStore
   (alphabetical before SqlStateStore).

2) crates/app/src/builders.rs, fn make_store — insert BEFORE its first
   statement (`    match std::env :: var ("COXAGENT_DB_DSN") {`):

   // Opt-in remote mode : front-end a gateway over REST instead of direct DB .
   if let Ok(url) = std :: env :: var ("COXAGENT_REMOTE_STORE_URL") {
       if !url.is_empty() {
           let cfg = RestConfig { base_url : url ,
                                  project_id : id .to_string() ,
                                  token : None };
           let store = RestStateStore :: new(cfg)?;
           tracing :: info ("[{id}] state store : REMOTE gateway");
           return Ok(Arc::new(AnyStateStore :: Rest (store)));
       }
   }

   Default behaviour stays byte-for-byte unchanged when the env var is unset,
   so the running/live system cannot be affected unless explicitly opted in.

3) Verify:
       cargo fmt -p coxagent-app
       cargo check -p coxagent-app
       cargo clippy -p coxagent-app --all-targets
       cargo clippy --workspace --all-targets     # and workspace green

## Design intent / follow-ups not yet scoped
- Auth : `RestConfig.token` exists for a bearer token; harvest it from the
         user login session later (needs UI/opencode flow plumbing).
- P4    broad : neutralize embedded-hub direct-DB path in thin-client desktop;
         run full test suite + e2e before merging to main.
