//! CXA-F274 — Read-back: serve archived tickets to the board UI and detail
//! endpoints. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "GET /api/projects/:pid/tickets/archive returns tickets previously
//!    evicted by the archival use case with all persisted fields intact;
//!    unauthorized requests are rejected like sibling endpoints"
//! 2. "Requesting a ticket id that exists only in the archive via the
//!    existing ticket-detail path returns the archived ticket instead of a
//!    not-found error"
//! 3. "Board closed/done view lists archived tickets and closed-ticket counts
//!    equal hot closed + archived; with an empty archive the UI and API are
//!    indistinguishable from current behavior"
//! 4. "Playwright e2e suite passes including the console-error gate
//!    (`cd e2e && npx playwright test`), with golden screenshots refreshed if
//!    the board changed"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure fixtures over the real state/domain
//! types the codebase has today (`Ticket` with its full persisted field set,
//! the REAL transition table) plus source-scan guards over the files the
//! read-back must live in — the same no-harness discipline as
//! `reproduce_url_f244_tdd.rs`, `global_search_f275_tdd.rs` and
//! `verify_live_link_f245_tdd.rs`: no fake HTTP server, no host harness, no
//! network port, no invented identifiers. Every failing assertion below fails
//! only because CXA-F274's behaviour is missing; if an assertion's mechanism
//! moves during implementation, move the guard with it (the
//! `preflight_f239_tdd.rs` convention).
//!
//! DESIGN INPUT (where every identifier below comes from): this repo's own
//! parked WIP checkpoint `0d314923` ("WIP checkpoint: CXA-F274 — parked by
//! slot hygiene (engine died mid-edit)") holds the complete read-back design
//! and is the codebase's only definition of the archive surface:
//!   * `ArchiveStorePort` — the cold store holding tickets evicted from the
//!     hot `ProjectState`, keyed by project id
//!     (`crates/application/src/ports/outbound/archive.rs`, registered in
//!     `ports/outbound/mod.rs`);
//!   * the pure read model `crates/application/src/archive_read.rs` —
//!     id-descending order, offset/limit window with clamps, cross-page total
//!     (the hexagonal rule: the read decision is a pure function over what
//!     the adapter returns; IO lives in the infra adapter, the dev/e2e
//!     `MemoryArchiveStore` under `crates/infrastructure/src/`);
//!   * `GET /api/projects/:pid/tickets/archive` → `ticket_archive_ep`
//!     (`crates/presentation/src/server/archive.rs`), registered in the same
//!     auth-gated router section as every sibling and documented in the
//!     OpenAPI table (the `openapi_routes_gate` invariant);
//!   * the detail path's hot-first/then-archive fallback (`resolve_detail`),
//!     which answers the archived ticket on a hot miss, keeps the unchanged
//!     404 on a miss on BOTH stores, and stamps the payload `"archived":true`;
//!   * the board read-back in `crates/presentation/src/web/js/core.js` — a
//!     one-time probe (limit=1) sizes the archive, an Archive column lists the
//!     archived tickets with their saved closed statuses, and a closed-count
//!     summary reads "closed: N hot + M archived";
//!   * console-gated Playwright coverage: the empty-archive invariance spec
//!     in the main suite plus the populated-archive spec with a pinned board
//!     golden, seeded from `e2e/fixtures/archive-seed.json` (the populated run
//!     boots its own server via `e2e/run-server-archive.sh` +
//!     `e2e/playwright.archive.config.ts`).
//!
//! PREREQUISITE FLAGGED, NOT FABRICATED: the WRITE side — the archival use
//! case that evicts tickets ("the archival use case" AC1 names; checkpoint
//! lineage CXA-F272/F273) — is NOT in the working tree: nothing evicts
//! tickets today and `crates/application/src/state/integrity.rs` still pins
//! "Tickets are never removed from the aggregate". No test here fakes an
//! eviction; the read-back contract is pinned at the seams the design
//! defines (port, route, detail fallback, board, e2e). An end-to-end
//! evict→read-back test cannot compile today because neither the eviction
//! use case nor the port exists in the tree — resuming checkpoint 0d314923
//! (which carries both this read-back and the port) is what turns this suite
//! green.
//!
//! "ALL PERSISTED FIELDS INTACT" READING (per the checkpoint design): the
//! board LIST serves the full serialized aggregate minus only the on-demand
//! `design` specs — the exact rule hot list payloads already follow (design
//! loads through the detail dialog) — plus the `"archived"` stamp; the
//! DETAIL fallback serves the FULL record, design specs included. The green
//! guards below prove every persisted field survives a serde round-trip, so
//! "intact" has a real, buildable meaning.
//!
//! Red today, and why:
//!   * AC1 — no `/api/projects/:pid/tickets/archive` route exists
//!     (`server/mod.rs` registers none, the OpenAPI table lists none), no
//!     `ArchiveStorePort` / read model / adapter exists, and nothing in the
//!     tree stamps a ticket `"archived"`.
//!   * AC2 — `ticket_detail_ep` looks up `state.tickets` only and answers
//!     `not_found()` on a miss; no archive fallback exists anywhere.
//!   * AC3 — the board JS never mentions an archive: no probe, no Archive
//!     column, no hot+archived closed summary; the pinned empty shape
//!     `{"tickets":[],"total":0}` exists nowhere.
//!   * AC4 — no e2e spec covers the archive surface, so no console-gated
//!     spec and no refreshed board golden exist.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * a fully-loaded ticket (SA design, acceptance criteria, created stamp,
//!     claim) round-trips serde with every persisted field intact — the
//!     concrete meaning of AC1's "all persisted fields intact";
//!   * closed tickets reach all four closed board statuses (Done, Fixed,
//!     Verified, Documented) through the REAL transition table — the exact
//!     statuses the board's closed/done columns (UCOLS done/shipped) list,
//!     so AC3's premise data exists without fabrication;
//!   * today's detail miss contract is the unchanged 404 "no such project" —
//!     the baseline AC2 must preserve.
//!
//! AC → test map:
//! - AC1: [`ac1_the_archive_listing_route_is_registered_and_documented_like_siblings`]
//!   (RED), [`ac1_the_route_sits_behind_the_same_auth_gate_as_sibling_endpoints`]
//!   (RED), [`ac1_the_read_back_reads_a_cold_store_port_through_a_pure_application_read_model`]
//!   (RED), [`ac1_the_listing_serves_every_persisted_field_with_only_design_stripped_and_the_archived_stamp`]
//!   (RED), plus the green [`every_persisted_ticket_field_survives_a_serde_round_trip`]
//! - AC2: [`ac2_a_hot_miss_falls_back_to_the_archive_in_the_detail_path`]
//!   (RED), plus the green [`the_detail_miss_contract_today_is_the_unchanged_404`]
//! - AC3: [`ac3_the_board_lists_archived_tickets_and_counts_hot_closed_plus_archived`]
//!   (RED), [`ac3_an_empty_or_unwired_archive_is_indistinguishable_from_today`]
//!   (RED), plus the green [`the_closed_done_view_is_two_real_board_columns_today`]
//!   and [`closed_tickets_reach_every_closed_board_status_through_the_real_transition_table`]
//! - AC4: [`ac4_a_console_gated_e2e_spec_covers_the_archive_read_back`]
//!   (RED), [`ac4_the_populated_archive_board_pins_a_refreshed_golden_screenshot`]
//!   (RED)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};

/// Where the archive HTTP surface must live (the work surface's sibling
/// module, per the checkpoint layout and the server's one-logical-module
/// rule).
const SERVER_ARCHIVE: &str = "crates/presentation/src/server/archive.rs";

/// The router the archive route must be registered in, behind the same
/// `auth_mw` route_layer as every sibling endpoint.
const SERVER_ROUTER: &str = "crates/presentation/src/server/mod.rs";

/// The OpenAPI table every registered service route must appear in
/// (`openapi_routes_gate.rs` invariant).
const OPENAPI_TABLE: &str = "crates/presentation/src/server/openapi.rs";

/// The detail-path handler AC2 extends with the cold-store fallback.
const WORK_HANDLER: &str = "crates/presentation/src/server/work.rs";

/// The cold-store port the read-back reads (and the eviction writes).
const ARCHIVE_PORT: &str = "crates/application/src/ports/outbound/archive.rs";

/// The port registry the port module must be declared in.
const PORT_REGISTRY: &str = "crates/application/src/ports/outbound/mod.rs";

/// The pure read model over the port result (order, window, total).
const ARCHIVE_READ_MODEL: &str = "crates/application/src/archive_read.rs";

/// The dev/e2e infra adapter for the cold store.
const ARCHIVE_ADAPTER: &str = "crates/infrastructure/src/archive_memory.rs";

/// The board JS the archive read-back renders from.
const BOARD_JS: &str = "crates/presentation/src/web/js/core.js";

/// The main-suite e2e spec (empty-archive invariance, console gate armed).
const E2E_EMPTY_SPEC: &str = "e2e/specs/archive.spec.ts";

/// The populated-archive spec with the pinned board golden.
const E2E_POPULATED_SPEC: &str = "e2e/specs-archive/archive.spec.ts";

/// The seed fixture the populated e2e run boots from (real ticket data).
const E2E_SEED: &str = "e2e/fixtures/archive-seed.json";

/// The route AC1 names, verbatim.
const ARCHIVE_ROUTE: &str = "/api/projects/:pid/tickets/archive";

/// The byte-exact empty shape both empty cases must answer (AC3's
/// indistinguishability half, per the checkpoint design).
const EMPTY_SHAPE: &str = r#"{"tickets":[],"total":0}"#;

// --- repo-state scan helpers (the global_search_f275_tdd.rs pattern) ---------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Source of a repo file, or `None` when absent — an absent file is the RED
/// condition itself, so the caller's assertion (not a read panic) must
/// report the miss with the acceptance criterion attached.
fn try_read(rel: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(rel)).ok()
}

fn read(rel: &str) -> String {
    try_read(rel).unwrap_or_else(|| panic!("read {rel}"))
}

fn lower(src: &str) -> String {
    src.to_ascii_lowercase()
}

/// Whitespace-insensitive view for matching source literals whose formatting
/// may drift (rustfmt, prettier).
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

// --- fixtures over the real state/domain types -------------------------------

/// A fully-loaded ticket: every persisted field the aggregate carries, set
/// through the real domain API — the concrete fixture behind AC1's "all
/// persisted fields intact".
fn fully_loaded_ticket(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Feature,
        "Serve the ticket archive back to the board",
        "eviction must never make finished work invisible",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "cold store behind the ArchiveStorePort".to_owned(),
            files: vec!["crates/application/src/ports/outbound/archive.rs".to_owned()],
            ..TechnicalDesign::default()
        },
    )
    .expect("SA owns the design");
    t.set_acceptance_criteria(vec![
        "the archive lists evicted tickets".to_owned(),
        "an archive-only id still opens by id".to_owned(),
    ]);
    t.stamp_created_at("2026-09-01T00:00:00Z");
    t
}

/// A feature driven to `Done` through the REAL transition table — the first
/// closed status of the board's done column.
fn done_feature(id: &str) -> Ticket {
    let mut t = fully_loaded_ticket(id);
    t.transition_to(Role::Sa, Status::Ready)
        .expect("designed feature readies");
    t.claim(Role::DevFeature, "finn@mac", "2026-09-01T09:00:00Z")
        .expect("claim is a legal edge");
    t.transition_to(Role::DevFeature, Status::Done)
        .expect("DEV completes the work");
    t
}

/// A bug driven to `Verified` through the REAL transition table — the
/// shipped column's bug-side status.
fn verified_bug(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Bug,
        "archive probe errors the console on a dead cold store",
        "the probe must degrade, never throw",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("bug claim is a legal edge");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("DEV completes the fix");
    t.transition_to(Role::Test, Status::Verified)
        .expect("TEST renders the verify verdict");
    t
}

/// A feature driven to `Documented` through the REAL transition table —
/// the shipped column's feature-side status.
fn documented_feature(id: &str) -> Ticket {
    let mut t = done_feature(id);
    t.transition_to(Role::Docs, Status::Documented)
        .expect("DOCS closes the loop");
    t
}

// --- green guards: fixture validity over types that exist today --------------

/// AC1's premise: "all persisted fields intact" is concrete — a fully-loaded
/// ticket round-trips serde with every persisted field equal, so the archive
/// (which persists whole `Ticket` aggregates per the design) has real data to
/// keep intact and this suite has a buildable fixture.
#[test]
fn every_persisted_ticket_field_survives_a_serde_round_trip() {
    let t = fully_loaded_ticket("CXC-F274-001");
    let json = serde_json::to_value(&t).expect("ticket serializes");
    let back: Ticket = serde_json::from_value(json).expect("ticket deserializes");
    assert_eq!(back.id(), t.id(), "id intact");
    assert_eq!(back.ticket_type(), t.ticket_type(), "type intact");
    assert_eq!(back.title(), t.title(), "title intact");
    assert_eq!(back.description(), t.description(), "description intact");
    assert_eq!(back.priority(), t.priority(), "priority intact");
    assert_eq!(back.complexity(), t.complexity(), "complexity intact");
    assert_eq!(back.status(), t.status(), "status intact");
    assert_eq!(back.has_ui(), t.has_ui(), "has_ui intact");
    assert_eq!(
        back.design().technical.clone().map(|d| d.approach),
        t.design().technical.clone().map(|d| d.approach),
        "the SA design specs survive — the detail fallback serves them"
    );
    assert_eq!(
        back.acceptance_criteria(),
        t.acceptance_criteria(),
        "acceptance criteria intact"
    );
    assert_eq!(back.created_at(), t.created_at(), "created_at intact");
}

/// AC3's premise: every closed status the board's closed/done columns list is
/// reachable through the REAL transition table, so "archived tickets" (the
/// evicted closed work) is data the codebase actually has — no fabricated
/// status, no fake fixture.
#[test]
fn closed_tickets_reach_every_closed_board_status_through_the_real_transition_table() {
    let done = done_feature("CXC-F274-done");
    assert_eq!(done.status(), Status::Done, "DEV completes features");
    let documented = documented_feature("CXC-F274-documented");
    assert_eq!(documented.status(), Status::Documented, "DOCS ships docs");
    let verified = verified_bug("CXC-F274-verified");
    assert_eq!(verified.status(), Status::Verified, "TEST verifies fixes");
}

/// AC3's subject: the board's closed/done view is the `done` and `shipped`
/// columns of `UCOLS` — real board code today, listing exactly the four
/// closed statuses the fixtures above reach. The archive surface must join
/// THIS view, not invent another.
#[test]
fn the_closed_done_view_is_two_real_board_columns_today() {
    let js = flat(&read(BOARD_JS));
    assert!(
        js.contains(r#"["done","Done","--green",["done","fixed"]]"#),
        "the board's done column (done+fixed) exists in {BOARD_JS}"
    );
    assert!(
        js.contains(r#"["shipped","Shipped","--teal",["documented","verified"]]"#),
        "the board's shipped column (documented+verified) exists in {BOARD_JS}"
    );
}

/// AC2's baseline: today a detail lookup that misses the hot state answers
/// the unchanged 404 "no such project" — the exact behaviour AC2 preserves
/// for a miss on BOTH stores (archival must never change the honest-miss
/// contract, only the archive-only miss must stop being a 404).
#[test]
fn the_detail_miss_contract_today_is_the_unchanged_404() {
    let work = read(WORK_HANDLER);
    assert!(
        work.contains("map_or_else(not_found"),
        "the detail handler answers not_found() on a hot miss today — the \
         baseline {WORK_HANDLER} pins"
    );
    let server = read(SERVER_ROUTER);
    assert!(
        server.contains("no such project"),
        "the 404 body is \"no such project\" — the byte-identical miss \
         contract AC2 preserves"
    );
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "GET /api/projects/:pid/tickets/archive returns tickets previously
/// evicted by the archival use case..." — the route must be registered in the
/// project router and documented in the OpenAPI table like every sibling
/// (the `openapi_routes_gate` invariant). RED: no registration and no table
/// row exist.
#[test]
fn ac1_the_archive_listing_route_is_registered_and_documented_like_siblings() {
    let router = read(SERVER_ROUTER);
    assert!(
        router.contains(ARCHIVE_ROUTE),
        "{SERVER_ROUTER} never registers {ARCHIVE_ROUTE} — AC1's listing \
         endpoint does not exist"
    );
    assert!(
        router.contains("ticket_archive_ep"),
        "the archive route must be wired to its handler (ticket_archive_ep) \
         in {SERVER_ROUTER}"
    );
    let openapi = read(OPENAPI_TABLE);
    assert!(
        openapi.contains(ARCHIVE_ROUTE),
        "{OPENAPI_TABLE} never documents {ARCHIVE_ROUTE} — the route-drift \
         gate invariant requires every registered route documented"
    );
}

/// AC1: "...unauthorized requests are rejected like sibling endpoints" — the
/// archive route must sit in the SAME `route_layer(auth_mw)`-gated router
/// section as `/api/projects/:pid/ticket/:id` and must NOT be added to the
/// middleware's public-path list. RED: the route does not exist, so it is in
/// neither place.
#[test]
fn ac1_the_route_sits_behind_the_same_auth_gate_as_sibling_endpoints() {
    let router = read(SERVER_ROUTER);
    let route_at = router.find(ARCHIVE_ROUTE).unwrap_or_else(|| {
        panic!(
            "{ARCHIVE_ROUTE} is not registered in {SERVER_ROUTER} — no auth \
             placement to verify"
        )
    });
    let auth_layer = "route_layer(axum::middleware::from_fn_with_state(state.clone(), auth_mw))";
    let gate_at = router
        .find(auth_layer)
        .unwrap_or_else(|| panic!("the auth_mw route_layer vanished from {SERVER_ROUTER}"));
    assert!(
        route_at < gate_at,
        "the archive route must be registered BEFORE the auth_mw route_layer \
         so unauthorized requests are refused exactly like sibling endpoints"
    );
    let auth = read("crates/presentation/src/server/auth.rs");
    assert!(
        !auth.contains(ARCHIVE_ROUTE),
        "auth_mw's public-path list must never name {ARCHIVE_ROUTE} — the \
         archive is project data, gated like every sibling"
    );
}

/// AC1: "...returns tickets previously evicted by the archival use case..." —
/// the read-back must read the SAME cold store the eviction writes, through
/// the application-layer port and a PURE read model (order, window, total —
/// the hexagonal rule), with the infra adapter holding the IO. RED: none of
/// the three exist in the tree.
#[test]
fn ac1_the_read_back_reads_a_cold_store_port_through_a_pure_application_read_model() {
    let port = try_read(ARCHIVE_PORT).unwrap_or_else(|| {
        panic!(
            "no cold-store port exists: {ARCHIVE_PORT} is absent — AC1 has \
             no archive to read through"
        )
    });
    assert!(
        port.contains("ArchiveStorePort"),
        "{ARCHIVE_PORT} must declare the ArchiveStorePort the read-back and \
         the eviction share"
    );
    for read_op in ["get", "list"] {
        assert!(
            lower(&port).contains(&format!("fn {read_op}")),
            "ArchiveStorePort must expose `{read_op}` — the read-back's \
             reads (AC1 lists, AC2 detail fallback)"
        );
    }
    let registry = read(PORT_REGISTRY);
    assert!(
        registry.contains("archive"),
        "{ARCHIVE_PORT} must be registered in {PORT_REGISTRY} to compile \
         into the crate at all"
    );
    let model = try_read(ARCHIVE_READ_MODEL).unwrap_or_else(|| {
        panic!(
            "no pure read model exists: {ARCHIVE_READ_MODEL} is absent — \
             the read decision must be a pure function over the port result"
        )
    });
    let model = lower(&model);
    assert!(
        model.contains("sort") || model.contains("desc"),
        "the read model must order the archive (newest ids first) — no \
         ordering in {ARCHIVE_READ_MODEL}"
    );
    assert!(
        model.contains("window") || model.contains("offset") || model.contains("limit"),
        "the read model must resolve the page window (offset/limit) — none \
         in {ARCHIVE_READ_MODEL}"
    );
    assert!(
        model.contains("total"),
        "the read model must carry the cross-page total the board sizes its \
         paging with — none in {ARCHIVE_READ_MODEL}"
    );
    let adapter = try_read(ARCHIVE_ADAPTER).unwrap_or_else(|| {
        panic!(
            "no infra adapter exists: {ARCHIVE_ADAPTER} is absent — the IO \
             must live in an adapter, never in the read model"
        )
    });
    assert!(
        adapter.contains("ArchiveStorePort"),
        "{ARCHIVE_ADAPTER} must implement ArchiveStorePort (the \
             MemoryArchiveStore dev/e2e adapter)"
    );
}

/// AC1: "...with all persisted fields intact" — the listing serves the full
/// serialized aggregate, stripping only the on-demand `design` specs (the
/// same rule hot list payloads follow; design loads through the detail
/// dialog) and stamping `"archived":true`, with the cross-page `total` the
/// board's summary counts. RED: the handler does not exist.
#[test]
fn ac1_the_listing_serves_every_persisted_field_with_only_design_stripped_and_the_archived_stamp() {
    let src = try_read(SERVER_ARCHIVE).unwrap_or_else(|| {
        panic!(
            "no archive HTTP surface exists: {SERVER_ARCHIVE} is absent — \
             AC1's listing has nothing to serve it"
        )
    });
    assert!(
        src.contains(ARCHIVE_ROUTE) || src.contains("tickets/archive"),
        "the archive handler module must serve {ARCHIVE_ROUTE}"
    );
    assert!(
        src.contains("serde_json::to_value"),
        "the listing must serialize the FULL ticket aggregate (all persisted \
         fields intact) — no full serialization in {SERVER_ARCHIVE}"
    );
    assert!(
        src.contains(r#"remove("design")"#),
        "the listing strips only the on-demand design specs (the hot \
         list-payload rule) — no design strip in {SERVER_ARCHIVE}"
    );
    assert!(
        src.contains(r#""archived""#),
        "archived rows must be stamped \"archived\":true — no stamp in \
         {SERVER_ARCHIVE}"
    );
    assert!(
        src.contains("total"),
        "the listing must serve the cross-page total — none in \
         {SERVER_ARCHIVE}"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "Requesting a ticket id that exists only in the archive via the
/// existing ticket-detail path returns the archived ticket instead of a
/// not-found error" — `ticket_detail_ep` must resolve the lookup through the
/// archive fallback (`resolve_detail` per the design): the hot tickets in,
/// the cold store consulted ONLY on a hot miss, the full archived record
/// served stamped `"archived":true`, and the unchanged 404 kept for a miss
/// on BOTH stores. RED: the detail path has no fallback — an archive-only id
/// answers the 404 today (the word "archived" already in {WORK_HEADER}'s
/// sprint-archival comment is a different concept and does not satisfy this
/// guard).
#[test]
fn ac2_a_hot_miss_falls_back_to_the_archive_in_the_detail_path() {
    let work = read(WORK_HANDLER);
    let flat_work = flat(&work);
    assert!(
        flat_work.contains("resolve_detail(&state.tickets"),
        "{WORK_HANDLER} never falls back to an archive on a hot miss — an \
         archive-only id answers the 404 AC2 forbids (the fallback must \
         resolve hot-first, feeding state.tickets to the resolver)"
    );
    assert!(
        flat_work.contains(r#"insert("archived""#),
        "the archived payload must be stamped \"archived\":true so the dialog \
         can render read-only — no stamp in {WORK_HANDLER}"
    );
    assert!(
        work.contains("map_or_else(not_found"),
        "a miss on BOTH stores must keep the unchanged 404 (the baseline \
         guard pins the body) — the not_found path must survive"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "Board closed/done view lists archived tickets and closed-ticket
/// counts equal hot closed + archived" — the board JS must probe the archive
/// endpoint, list the archived tickets on the board (each carrying its saved
/// closed status — the four UCOLS closed statuses), and surface a closed
/// summary expressing hot closed + archived. RED: the board JS never
/// mentions an archive.
#[test]
fn ac3_the_board_lists_archived_tickets_and_counts_hot_closed_plus_archived() {
    let js = try_read(BOARD_JS).unwrap_or_else(|| panic!("read {BOARD_JS}"));
    let low = lower(&js);
    assert!(
        js.contains("tickets/archive"),
        "the board never probes {ARCHIVE_ROUTE} — archived tickets cannot \
         reach the board (AC3)"
    );
    for closed in ["done", "fixed", "documented", "verified"] {
        assert!(
            low.contains(closed),
            "the board's archive surface must show each archived ticket's \
             saved closed status ('{closed}' is one of the four closed \
             statuses the done/shipped columns list) — none in {BOARD_JS}"
        );
    }
    assert!(
        flat(&js).contains(r#"["done","fixed","documented","verified"]"#),
        "the board's archive surface must know the closed-status set the \
         done/shipped columns list (the same set eviction archives, so hot \
         + archived counts every finished ticket exactly once) — no closed \
         status set in {BOARD_JS}"
    );
    assert!(
        low.contains("hot") && low.contains("archived"),
        "the closed-count summary must read hot closed + archived (AC3: \
         counts equal hot closed + archived) — no hot/archived summary in \
         {BOARD_JS}"
    );
    assert!(
        low.contains("closed"),
        "the summary must label itself as the closed count — none in \
         {BOARD_JS}"
    );
}

/// AC3: "...with an empty archive the UI and API are indistinguishable from
/// current behavior" — the API answers the pinned byte-exact empty shape
/// whether NO store is wired or the wired store is empty, and the board's
/// probe renders nothing (and never errors the console) on an empty or
/// failed probe. RED: neither the empty shape nor the probe exists.
#[test]
fn ac3_an_empty_or_unwired_archive_is_indistinguishable_from_today() {
    let src = try_read(SERVER_ARCHIVE).unwrap_or_else(|| {
        panic!(
            "no archive HTTP surface exists: {SERVER_ARCHIVE} is absent — \
             the pinned empty shape has no home"
        )
    });
    assert!(
        flat(&src).contains(EMPTY_SHAPE),
        "an unwired or empty archive must answer the byte-exact shape \
         {EMPTY_SHAPE} so the API is indistinguishable from today — no \
         such literal in {SERVER_ARCHIVE}"
    );
    let js = read(BOARD_JS);
    let low = lower(&js);
    assert!(
        low.contains("total<1") || low.contains("total < 1"),
        "an archive of size 0 must render no board surface (the probe \
         degrades to today's board) — no zero-guard in {BOARD_JS}"
    );
    assert!(
        low.contains("catch"),
        "a failed probe must degrade to the pre-archive board and never \
         error the console — no catch in {BOARD_JS}"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "Playwright e2e suite passes including the console-error gate
/// (`cd e2e && npx playwright test`)..." — the main suite must cover the
/// archive read-back with the console-error gate armed on its tests (the
/// empty-archive invariance half: the fixture server boots with no archive,
/// so the spec pins the unchanged API/board). RED: no such spec exists.
#[test]
fn ac4_a_console_gated_e2e_spec_covers_the_archive_read_back() {
    let spec = try_read(E2E_EMPTY_SPEC).unwrap_or_else(|| {
        panic!(
            "no e2e coverage exists: {E2E_EMPTY_SPEC} is absent — AC4's \
             console-gated suite never exercises the archive surface"
        )
    });
    assert!(
        spec.contains("armConsoleGate") && spec.contains("assertNoConsoleErrors"),
        "the archive e2e spec must arm the console-error gate (the \
         `e2e_acceptance_gate` requires it on every spec)"
    );
    assert!(
        spec.contains("tickets/archive"),
        "the e2e spec must exercise {ARCHIVE_ROUTE}"
    );
}

/// AC4: "...with golden screenshots refreshed if the board changed" — the
/// populated-archive board (chip, Archive column, closed summary) must pin a
/// golden screenshot, seeded from real ticket data the fixture builds. RED:
/// neither the populated spec nor its seed fixture exists.
#[test]
fn ac4_the_populated_archive_board_pins_a_refreshed_golden_screenshot() {
    let spec = try_read(E2E_POPULATED_SPEC).unwrap_or_else(|| {
        panic!(
            "no populated-archive e2e coverage exists: {E2E_POPULATED_SPEC} \
             is absent — the changed board has no golden to refresh"
        )
    });
    assert!(
        spec.contains("toHaveScreenshot"),
        "the populated-archive board must pin a golden screenshot (AC4: \
         goldens refreshed when the board changed) — none in \
         {E2E_POPULATED_SPEC}"
    );
    assert!(
        spec.contains("assertNoConsoleErrors"),
        "the populated-archive spec must arm the console-error gate too"
    );
    let seed = try_read(E2E_SEED).unwrap_or_else(|| {
        panic!(
            "the populated e2e run has no seed: {E2E_SEED} is absent — the \
             golden has no real data to boot from"
        )
    });
    assert!(
        seed.contains(r#""id":"#) && seed.contains(r#""status":"#),
        "{E2E_SEED} must seed real archived-ticket records (ids and closed \
         statuses — buildable fixture data, never fabricated shapes)"
    );
}
