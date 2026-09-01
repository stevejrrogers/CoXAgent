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
- Auth from login session : DONE since this audit — the login flow persists
                            the personal API token to operator.token and
                            `provision_local_token` feeds it to
                            `COXAGENT_REMOTE_TOKEN` (see P5a notes).
- P4 broad : neutralize embedded-hub direct-DB path in thin-client desktop so
             nothing outside the gateway touches Postgres/Redis; currently opt-in
             only, default still direct-DB for untouched operators.
- Run full workspace test suite + e2e before merging to main.


## P5a status: APPLIED and guarded (was "not yet applied" — stale, corrected CXA-F285)

Enforcement landed server-side, twice over (defense-in-depth):
- Middleware: `auth_mw` gates the path by policy —
  `write_gate_ok` demands `can_manage()` for the raw store RPC
  (crates/presentation/src/server/auth.rs:130-148, `is_store_rpc_path`
  :152-156) and the per-project membership branch (:345-356) refuses
  non-members (Super/Admin exempt). Unauthenticated -> 401
  `{"error":"unauthenticated"}` (:279-285).
- Handler: `authorize_store_call`
  (crates/presentation/src/server/store_rpc.rs:282-314) re-checks the same
  policy inside the endpoint — 401 "sign in first" (:290-292), 403
  "management role required" (:293-301), 403 "not a member of this project"
  (:302-312) — shared by the POST surface and the GET audit surface
  (:399, :437). Open mode (`app.auth` = None) stays fully open (:287-289).

The sketch below is the ORIGINAL P5a recipe (historical). The landed shape
goes beyond it: manage-role + membership enforcement, not just the 401 wall.
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
- resolve_principal is defined in server/guards.rs (bearer first, cookie
  fallback) and reachable via super.
- Imports IntoResponse/Response already exist in store_rpc.rs.
- Runner token source: personal API token minted by my_tokens_ep /
  create_my_token_ep (prefix user:<name>:); the login flow persists it to
  `<home>/CoXAgent/operator.token` (or `COXAGENT_TOKEN_FILE`), and
  `provision_local_token` (crates/app/src/builders.rs:246-280) feeds it into
  `COXAGENT_REMOTE_TOKEN` for separately-spawned operator processes.

## /store caller inventory (CXA-F285)

Every caller of `POST /api/projects/:pid/store`, verified in-repo. There is
NO web-UI caller: `crates/presentation/src/web/` (index.html + js/) has no
reference to the store route — all /store traffic is runner/CLI processes
(matching the recorded team decision).

1. RestStateStore adapter — crates/infrastructure/src/state/rest_store.rs.
   The runner-side StateStorePort adapter: EVERY op becomes
   `POST {base}/api/projects/:pid/store?op=<op>` with a JSON `Args` body and
   `Authorization: Bearer <cfg.token>` (:124-152). 409 -> `PortError::Conflict`
   (retry contract for read-modify-write); any other non-2xx -> Backend error
   carrying the status + a remedy hint (401 -> "set COXAGENT_REMOTE_TOKEN…",
   403 -> "present a token for a manage-tier member", :104-122). Ops on the
   wire: load, version, save(+revision), claim_ticket, acquire_leader,
   claim_stage, release_stage, heartbeat, workers, set_desired, get_desired,
   acquire_operator — the twelve-op surface pinned by store_rpc_auth_gate.rs.
2. make_store wiring — crates/app/src/builders.rs:28-51. REMOTE-first
   precedence: `COXAGENT_REMOTE_STORE_URL` non-empty -> RestStateStore;
   bearer = `COXAGENT_REMOTE_TOKEN` (which `provision_local_token`
   (:246-280) may have hydrated from the login-written operator.token file,
   owner-only-checked). REMOTE wins over a DB DSN if both are set.
3. Runner cycle ops through the same adapter — crates/app/src/lib.rs:
   worker-registry heartbeats (idle/45s loop :1237-1261, per-phase reporter
   :1263-1278) and the single-instance operator lock (:1287-1292,
   acquire_operator). Credential class identical to (1): whatever
   make_store built for the process.
4. CLI engine probe — `coxagent probe --hub <url> --project <pid>`
   (crates/presentation/src/cli.rs:27-34 -> run_probe,
   crates/app/src/lib.rs:1811-1850): one-shot RestStateStore heartbeat that
   advertises this machine's agent engines to the hub so /api/engines
   populates without a runner cycle. Token from `COXAGENT_REMOTE_TOKEN` or
   the operator.token file (:1820-1829). This is the "cli.rs engine
   heartbeat" caller class.
5. deploy/watchdog-cxa.sh — the one non-Rust caller. Logs in, caches the
   `cox_session` cookie, then drives `POST /api/projects/cxa/store` op=load /
   op=version / op=save (read-modify-write with the revision + 409 retry,
   :91-126) to self-heal rejected 'Debt sweep' noise tickets. Cookie-class
   credential: session principals DO carry the user's project memberships
   (unlike bearer service principals — see annotation A), and the hub's
   root account is Super, so the gate passes.
6. In-repo tests/guards exercising the endpoint:
   - crates/app/tests/store_rpc_auth_gate.rs — 12 ops x {anonymous,
     member-tier, admin} through the real `auth_mw`; open-mode control.
   - crates/app/tests/rest_state_store_contract.rs (+ rest_store_support/)
     — the adapter contract over a live serve_full gateway.
   - crates/app/tests/store_auth_e2e_f285_tdd.rs — NEW (CXA-F285): real
     bearer personas through serve_full (the bearer path the earlier guards
     never exercised — their StubAuth answered None for every bearer);
     pins AC1-AC4 in-process.
   - crates/presentation/src/server/store_rpc_{guard,auth_enforcement,
     stale_write,audit}_tests.rs, pr_review_gate_tests.rs — handler-level
     bodies, audit gate, "the raw store RPC is not an ordinary write".
7. Browser e2e — e2e/specs-auth/store-auth.spec.ts — NEW (CXA-F285): the
   first auth-enabled e2e over the real hub (`run-server-auth.sh`, port
   4518): anonymous twelve-op 401 + state-untouched, forged bearer 401,
   admin personal token wire shape (my/tokens -> bearer load/version),
   read-modify-write save + replay, write-tier member 403 (write gate),
   manage-tier non-member 403 (membership branch), and a CROSS-PROCESS
   runner (`coxagent report` with COXAGENT_REMOTE_STORE_URL +
   COXAGENT_REMOTE_TOKEN env — the exact make_store wiring, proven by a
   seeded state delta only the gateway could have served) with its 401
   negative. Run: `cd e2e && npx playwright test
   --config playwright.auth.config.ts`.

### Wire-shape annotations (verified, CXA-F285)

A. Bearer service principals carry NO project memberships:
   `principal_for_bearer` resolves any personal/service token to
   `projects: []` — FileAuthService (crates/infrastructure/src/auth.rs:433-444)
   and SqlAuthService (crates/infrastructure/src/sql_auth.rs:671-689) alike.
   Consequence: through /store's membership branch, only Super/Admin-tier
   tokens pass; a lead-tier (e.g. TechLead) member's personal token is
   refused 403 "not a member of this project" even though the ACCOUNT is a
   member (session cookies carry memberships; bearer tokens do not).
   FLAG for SA: if lead-tier runner tokens are wanted, the token principal
   must inherit the minting member's projects — product decision, not a bug
   fixed in this audit.
B. 401 body through the real stack is auth_mw's `{"error":"unauthenticated"}`;
   the handler's "sign in first" sits behind it as defense-in-depth (reached
   only if the route ever leaves the middleware). Both layers pinned:
   store_rpc_guard_tests.rs pins the handler bodies directly.
C. Revisions/409 are BACKEND-dependent: the file-backed store
   (JsonStateStore) leaves `current_version`/`save_expecting` at the port
   defaults (revision `null`, no conflict — StateStorePort defaults,
   crates/application/src/ports/outbound/state_store.rs:179-199); the
   Postgres store tracks revisions and answers 409 on stale writes. The
   409 contract is pinned in-process (rest_state_store_contract.rs,
   store_rpc_stale_write_tests.rs); the e2e spec asserts the documented
   envelope, not which backend the hub runs.
D. E2E rate-window note: every non-GET /api/auth/* request shares one
   20-per-60s bucket per client IP, and the auth suite's specs together
   spend 16 of those 20. run-server-auth.sh therefore sets
   `COXAGENT_TRUST_PROXY=1` (the limiter's documented shared-IP knob,
   rate_limit.rs:80-105) and store-auth.spec.ts carries its own
   X-Forwarded-For identity, so the file's credential calls are limited as
   their own client. FLAG: rate_limit.rs's doc table advertises
   AUTH_RATE_MAX / AUTH_RATE_WINDOW as env-tunable, but both are hardcoded
   constants (rate_limit.rs:30-32, wired at server/mod.rs:1063-1065) —
   either honor the env or fix the doc; and any future spec that logs in
   without an XFF identity will 429 the tail of the suite.
