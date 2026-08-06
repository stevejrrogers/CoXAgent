# Handoff: REST-store integration (DONE up to STEP C)

Branch: `rest-runner`. Repo builds GREEN + clippy clean + hex-gate pass.
Work item "runner reaches state through the gateway over REST instead of
direct Postgres/Redis" is functionally COMPLETE as an opt-in mechanism.

## Committed & verified
- a000007  P1   `RestStateStore` adapter implements full `StateStorePort`
                (load/save/claim_ticket, leader+stage leases, worker heartbeat,
                operator lock). Conflicts -> HTTP 409 -> `PortError::Conflict`.
- c0095ac  P2   Gateway endpoint `POST /api/projects/:pid/store?op=...`
                dispatches every op onto this hub's own store adapter.
- 51e37b1  P3A  `AnyStateStore::Rest(RestStateStore)` variant + dispatch arms;
                rest_store compiled unconditionally; re-exported at crate root.
- e150ed4      (not ours) evidence-based debt sweep / clippy ratchet.
- 4953714   STEP C wiring in make_store:
                import RestConfig/RestStateStore in app/lib.rs;
                when `COXAGENT_REMOTE_STORE_URL` is set (+ DB_DSN unset),
                make_store builds a RestStateStore via AnyStateStore::Rest.
                Default path unchanged when env unset.

Verify used: cargo check --workspace, clippy --workspace --all-targets,
cargo fmt --check, hex gate test — all green.

## How to activate (runtime)
Set env on the runner process:
    COXAGENT_REMOTE_STORE_URL=http://127.0.0.1:4000
    # optional bearer:
    COXAGENT_REMOTE_TOKEN=<token>
Leave COXAGENT_DB_DSN / COXAGENT_REDIS_URL unset for that process to use REST.

## Future follow-ups (not yet scoped)
- Auth from login session : harvest a real bearer token in the UI/opencode flow
                            and feed `COXAGENT_REMOTE_TOKEN` instead of manual/env.
- P4 broad : neutralize embedded-hub direct-DB path in thin-client desktop so
             nothing outside the gateway touches Postgres/Redis; currently opt-in
             only, default still direct-DB for untouched operators.
- Run full workspace test suite + e2e before merging to main.


## P5a recipe : enforce auth on /store (not yet applied)
Goal : mirror existing endpoints so /store requires a principal ONLY when the
gateway has auth enabled (open modes stay open).
File : crates/presentation/src/server/store_rpc.rs , fn store_rpc_ep .
1 ) Add a Headers extractor to the signature (order fine last):
       headers: axum::http::HeaderMap,
2 ) After resolving the project and before `match q.op.as_str()`, insert:
       match &app.auth {
           Some(auth) => {
               if super::resolve_principal(auth, &headers).await.is_none() {
                   return (
                       axum :: http :: StatusCode :: UNAUTHORIZED ,
                       "sign in first",
                   ) .into_response();
               }
           }
           None => {}
       }
3 ) Verify: cargo fmt -p coxagent-presentation && cargo check --workspace
    && cargo clippy --workspace --all-targets .
Notes :
- resolve_principal is defined in server/mod.rs and reachable via super.
- Imports IntoResponse/Response already exist in store_rpc.rs.
- Runner token source: personal API token minted by my_tokens_ep /
  create_my_token_ep (prefix user:<name>:); harvest at login into env
  COXAGENT_REMOTE_TOKEN fed to RestStateStore cfg.token.
