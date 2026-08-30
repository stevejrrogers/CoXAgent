//! CXA-F244 (CXA-F242-B) — Surface `reproduce_url` through every
//! verify-facing API surface + OpenAPI. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "GET /api/projects/:pid/inbox returns `reproduce_url` on every
//!    `kind:\"verify\"` item when resolvable, and omits or nulls it otherwise
//!    without dropping existing fields."
//! 2. "GET /api/projects/:pid/ticket/:id injects optional `reproduce_url` when
//!    the returned ticket is Fixed and awaiting verification."
//! 3. "Both endpoints remain backward compatible for clients that ignore the
//!    new field; JSON shapes verified by API-level tests using seeded store
//!    state."
//! 4. "OpenAPI document reflects the new optional field on both routes and
//!    openapi_routes_gate continues to pass."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the bytes the hub
//! actually serves — the inbox and work handler sources (`server/inbox.rs`,
//! `server/work.rs`), the OpenAPI table (`server/openapi.rs`) — plus the real
//! state/domain/config types the two handlers read. The same no-harness
//! discipline as `fleet_river_f233_gate.rs` and `preflight_f239_tdd.rs`:
//! no fake HTTP server, no host harness, no network port, no invented
//! identifiers. The suite compiles today and every failing assertion below
//! fails only because CXA-F244's behaviour is missing.
//!
//! WHERE THE FIELD COMES FROM (design grounding): the committed SA design for
//! the parent ticket (`.coxagent/design/CXA-F242-design.json`) derives the
//! live reproduction URL from `config.deploy.host_port` —
//! `http://127.0.0.1:{port}/` when set, nothing when absent — as "one source
//! of truth" for where a project's fix runs live, exactly how
//! `qa_evidence.rs` already builds its capture URLs. That committed design is
//! the codebase's only definition of "resolvable", so the data-side guards
//! pin that source. The field NAME below follows THIS ticket's AC text
//! (`reproduce_url`), which is the contract; note for the implementer: the
//! parent design labels the same field `repro_url` — the sub-ticket's ACs
//! govern.
//!
//! Red today, and why:
//!   * AC1 — the verify card built in `server/inbox.rs` carries only
//!     {kind, ticket, title, role, can_act}; the file never mentions
//!     `reproduce_url` nor the `host_port` resolvability source.
//!   * AC2 — the detail payload builder `ticket_detail_ep`
//!     (`server/work.rs`) never mentions `reproduce_url`.
//!   * AC4 — the OpenAPI table source never mentions `reproduce_url`, so
//!     neither route's document reflection can carry it.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * a Bug driven to `Fixed` through the REAL transition table, with DoD
//!     evidence attached and the verify-gate config flag on — exactly the
//!     state the inbox's verify condition reads today;
//!   * `deploy.host_port` as the optional resolvability axis (`None` default,
//!     parses from the config document);
//!   * the EXISTING JSON shapes the ACs promise not to break: the verify
//!     card's five fields, the detail payload's injected fields, and the
//!     serialized ticket's keys;
//!   * seeded store state round-trips through the real `JsonStateStore` (the
//!     fixture vehicle AC3 names), and the CXA-B051 route-drift invariant
//!     still holds over both route tables.
//!
//! NOT ENCODED HERE — verified against the live app in the QA phase:
//!   * the runtime response bodies (both handlers are `pub(super)`; the
//!     responses they build are the deliverable);
//!   * the omit-vs-null choice AC1 leaves open when `host_port` is unset;
//!   * the "awaiting verification" nuance of AC2 beyond the `Fixed` status
//!     plus attached evidence — the signal the inbox already uses for
//!     "awaiting a verdict" today;
//!   * the exact optional-modifier wording of AC4's reflection (the OpenAPI
//!     document models no response schemas today — operations carry summary
//!     text only — so the gate pins the field's presence per route and the
//!     word "optional" in the field's reflection).
//!
//! If an assertion's mechanism moves during implementation, move the guard
//! with it (the `preflight_f239_tdd.rs` convention).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use coxagent_application::config::Config;
use coxagent_application::ports::outbound::StateStorePort;
use coxagent_application::state::{Evidence, ProjectState};
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};
use coxagent_infrastructure::JsonStateStore;

/// The field every verify-facing surface must expose — this ticket's AC text
/// names it verbatim.
const FIELD: &str = "reproduce_url";

/// The two verify-facing routes the ACs name.
const INBOX_ROUTE: &str = "/api/projects/:pid/inbox";
const TICKET_ROUTE: &str = "/api/projects/:pid/ticket/:id";

/// The config document key the committed parent design (CXA-F242) pins as the
/// one resolvability source for the live reproduction URL.
const RESOLVABILITY_SOURCE: &str = "host_port";

/// Fixed RFC3339 instant for evidence stamps (deterministic fixtures).
const NOW: &str = "2026-08-28T00:00:00Z";

// --- repo-state scan helpers (the openapi_routes_gate.rs pattern) -----------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn read(rel: &str) -> String {
    let p = repo_root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The source window of the inbox's verify-card construction: from the
/// `"kind": "verify"` literal to the loop's `continue;`. Every field the card
/// carries lives inside this window; a card built anywhere else is a
/// different surface than the one the ACs name.
fn verify_card_window(src: &str) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(at) = rest.find("\"kind\": \"verify\"") {
        let after = &rest[at..];
        let end = after
            .find("continue;")
            .map_or(after.len().min(2000), |e| e + "continue;".len());
        out.push_str(&after[..end]);
        rest = &after[end..];
    }
    out
}

/// The source window of one handler: from its `pub(super) async fn` header to
/// the next one (or end of file). The detail payload's fields all live in
/// this window.
fn handler_window<'a>(src: &'a str, header: &str) -> &'a str {
    let at = src.find(header).unwrap_or(src.len());
    let rest = &src[at..];
    let end = rest[header.len()..]
        .find("\npub(super) async fn")
        .map_or(rest.len(), |rel| header.len() + rel);
    &rest[..end]
}

/// Every OpenAPI reflection window for `path`: from each quoted occurrence of
/// the path literal to the next table-item opener (a `route(` entry or a
/// `("` override tuple at the table's indent) or the table's `];` end — the
/// per-path slots the document builder reads. Concatenated, so an entry-level
/// comment, an inline summary override or a `SUMMARY_OVERRIDES` tuple all
/// count as the route's reflection.
fn openapi_reflection(src: &str, path: &str) -> String {
    let needle = format!("\"{path}\"");
    let mut out = String::new();
    let mut rest = src;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at..];
        let end = ["\n    route(", "\n    (", "\n];"]
            .iter()
            .filter_map(|stop| after.find(stop))
            .min()
            .unwrap_or(after.len().min(2000));
        out.push_str(&after[..end]);
        rest = &after[end..];
    }
    out
}

// --- fixtures over the real state/domain/config types ------------------------

/// A Bug driven to `Fixed` through the REAL transition table — the aggregate
/// boundary the hub itself goes through, not a struct-literal shortcut. A bug
/// starts `Open`; DEV claims it (`InProgress`) and completes the fix
/// (`Fixed`).
fn fixed_bug(id: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Bug,
        "Login returns 500 on an empty email",
        "POST /api/login with an empty email crashes the handler.",
        Priority::High,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("bug claim is a legal edge");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("DEV completes the fix");
    t
}

/// One DoD evidence record — the real `Evidence` shape the inbox's verify
/// condition keys on (`state.ticket_evidence.contains_key(&id)`).
fn screenshot_evidence() -> Evidence {
    Evidence {
        kind: "screenshot".to_owned(),
        label: "fixed login flow renders".to_owned(),
        detail: "evidence/login-fixed.png".to_owned(),
        at: NOW.to_owned(),
    }
}

/// The project state whose verify-gate condition holds for `id`: the ticket
/// is `Fixed` and carries evidence.
fn verify_gate_state(id: &str) -> ProjectState {
    let mut s = ProjectState::default();
    s.tickets.push(fixed_bug(id));
    s.ticket_evidence
        .insert(id.to_owned(), vec![screenshot_evidence()]);
    s
}

/// The project config with the hybrid verify gate on and the resolvability
/// source set to `port`.
fn verify_gate_config(port: Option<u16>) -> Config {
    let mut cfg = Config::default();
    cfg.workflow.human.gate_verify = true;
    cfg.deploy.host_port = port;
    cfg
}

// --- fixture validity (green guards) ----------------------------------------

/// The seeded state the ACs' verify surface is specified over is buildable
/// from the real types, and satisfies EXACTLY the condition the inbox's
/// verify card checks today: verify gate on, ticket `Fixed`, evidence
/// attached.
#[test]
fn the_verify_gate_fixture_is_buildable_from_real_types() {
    let cfg = verify_gate_config(Some(8080));
    assert!(
        cfg.workflow.human.gate_verify,
        "the hybrid verify gate must be switchable on"
    );
    let id = "CXC-244";
    let state = verify_gate_state(id);
    let t = state
        .tickets
        .iter()
        .find(|t| t.id().as_str() == id)
        .expect("seeded ticket present");
    assert_eq!(t.status(), Status::Fixed, "the fix is awaiting a verdict");
    assert!(
        state.ticket_evidence.contains_key(id),
        "DoD evidence is attached — the inbox's awaiting-verification signal"
    );
}

/// AC1's "when resolvable" has a real data source: `deploy.host_port`, absent
/// by default and parseable from the config document — the axis the parent
/// design (CXA-F242) pins as the one source of truth for the live URL.
#[test]
fn the_resolvability_source_is_the_optional_host_port() {
    assert_eq!(
        Config::default().deploy.host_port,
        None,
        "no host_port configured = the reproduction URL is not resolvable"
    );
    let cfg: Config =
        serde_json::from_str(r#"{"deploy":{"host_port":8080}}"#).expect("config doc parses");
    assert_eq!(
        cfg.deploy.host_port,
        Some(8080),
        "a configured host_port is the resolvable case"
    );
}

/// AC3, seeded-store-state half: the state fixture the API-level verification
/// will seed round-trips through the REAL project store — the same adapter
/// both handlers load from — unchanged. Proves the "seeded store state" AC3
/// names is buildable today, with no harness beyond a tempdir.
#[tokio::test]
async fn seeded_verify_gate_state_round_trips_through_the_project_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonStateStore::new(dir.path()).expect("store");
    let seeded = verify_gate_state("CXC-244");
    store.save(&seeded).await.expect("seed the store");
    assert_eq!(
        store.load().await.expect("reload"),
        seeded,
        "the verify-gate state survives persistence unchanged"
    );
}

/// AC3, shape half: the serialized ticket is the base of the detail payload
/// (`ticket_detail_ep` builds `serde_json::to_value(t)` plus injected keys),
/// so the keys clients already read are pinned — the field this ticket adds
/// must be additive to exactly this shape.
#[test]
fn the_detail_payload_shape_clients_rely_on_is_pinned() {
    let t = fixed_bug("CXC-244");
    let v = serde_json::to_value(&t).expect("ticket serializes");
    let obj = v.as_object().expect("the detail payload is a JSON object");
    for key in [
        "id",
        "type",
        "title",
        "description",
        "priority",
        "complexity",
        "status",
        "has_ui",
        "design",
    ] {
        assert!(obj.contains_key(key), "detail payload key `{key}` vanished");
    }
    assert_eq!(v["id"], "CXC-244", "the id clients route by");
    assert_eq!(v["status"], "fixed", "Fixed serializes snake_case");
}

// --- AC1 --------------------------------------------------------------------

/// AC1: "GET /api/projects/:pid/inbox returns `reproduce_url` on every
/// `kind:\"verify\"` item when resolvable" — the verify card the inbox builds
/// must carry the field. RED: today's card carries only
/// {kind, ticket, title, role, can_act}.
#[test]
fn ac1_inbox_verify_items_carry_reproduce_url() {
    let inbox = read("crates/presentation/src/server/inbox.rs");
    let card = verify_card_window(&inbox);
    assert!(
        !card.is_empty(),
        "the inbox no longer builds a `kind:\"verify\"` card — the verify \
         surface moved; point this guard at the code that builds it"
    );
    assert!(
        card.contains(FIELD),
        "the inbox's verify card must carry `{FIELD}` when the reproduction \
         URL is resolvable — found card: {card}"
    );
}

/// AC1, backward-compatibility half: "...without dropping existing fields" —
/// the verify card keeps every field it carries today, so clients that ignore
/// the new field see the same card.
#[test]
fn ac1_inbox_verify_items_keep_every_existing_field() {
    let inbox = read("crates/presentation/src/server/inbox.rs");
    let card = verify_card_window(&inbox);
    assert!(
        !card.is_empty(),
        "the inbox no longer builds a `kind:\"verify\"` card — the verify \
         surface moved; point this guard at the code that builds it"
    );
    for field in ["\"ticket\"", "\"title\"", "\"role\"", "\"can_act\""] {
        assert!(
            card.contains(field),
            "the verify card dropped its existing {field} field — CXA-F244 is \
             additive; card: {card}"
        );
    }
}

/// AC1, resolvability half: "...when resolvable, and omits or nulls it
/// otherwise" — the inbox can only decide that by consulting the one
/// resolvability source the committed parent design pins
/// (`config.deploy.host_port`, the same source `qa_evidence.rs` captures
/// against). RED: the inbox never reads it today.
#[test]
fn ac1_the_inbox_resolves_the_url_from_the_host_port_source() {
    let inbox = read("crates/presentation/src/server/inbox.rs");
    assert!(
        inbox.contains(RESOLVABILITY_SOURCE),
        "the inbox must consult `{RESOLVABILITY_SOURCE}` (the committed \
         CXA-F242 design's one resolvability source) to decide when the \
         verify card's `{FIELD}` is present"
    );
}

// --- AC2 --------------------------------------------------------------------

/// AC2: "GET /api/projects/:pid/ticket/:id injects optional `reproduce_url`
/// when the returned ticket is Fixed and awaiting verification" — the detail
/// payload builder must produce the field. RED: `ticket_detail_ep` injects
/// coverage_matrix / evidence / cost_hold / attachments / blocked_by today
/// and never `reproduce_url`.
#[test]
fn ac2_ticket_detail_injects_reproduce_url() {
    let work = read("crates/presentation/src/server/work.rs");
    let detail = handler_window(&work, "pub(super) async fn ticket_detail_ep");
    assert!(
        detail.contains("pub(super) async fn ticket_detail_ep"),
        "ticket_detail_ep moved out of server/work.rs — point this guard at \
         the handler that builds the detail payload"
    );
    assert!(
        detail.contains(FIELD),
        "the ticket detail payload must inject `{FIELD}` for a Fixed ticket \
         awaiting verification"
    );
}

/// AC2 + AC3, backward-compatibility half: the detail payload keeps every
/// computed field it injects today — the new field rides ALONGSIDE them.
#[test]
fn ac2_the_detail_payload_keeps_every_existing_injected_field() {
    let work = read("crates/presentation/src/server/work.rs");
    let detail = handler_window(&work, "pub(super) async fn ticket_detail_ep");
    for field in [
        "\"coverage_matrix\"",
        "\"evidence\"",
        "\"cost_hold\"",
        "\"attachments\"",
        "\"blocked_by\"",
    ] {
        assert!(
            detail.contains(field),
            "the detail payload dropped its existing {field} injection — \
             CXA-F244 is additive"
        );
    }
}

// --- AC4 --------------------------------------------------------------------

/// AC4: "OpenAPI document reflects the new optional field on ... routes" —
/// the inbox route's document reflection must carry the field. RED: the
/// OpenAPI source never mentions `reproduce_url`, so the served
/// `/api/openapi.json` cannot reflect it.
#[test]
fn ac4_openapi_reflects_reproduce_url_on_the_inbox_route() {
    let openapi = read("crates/presentation/src/server/openapi.rs");
    let reflection = openapi_reflection(&openapi, INBOX_ROUTE);
    assert!(
        !reflection.is_empty(),
        "{INBOX_ROUTE} vanished from the OpenAPI table — the CXA-B051 drift \
         gate fails first; this guard follows the table"
    );
    assert!(
        reflection.contains(FIELD),
        "the OpenAPI reflection for {INBOX_ROUTE} must document the optional \
         `{FIELD}` field — found: {reflection}"
    );
}

/// AC4: "...on both routes" — the ticket-detail route's document reflection
/// must carry the field too. RED for the same reason.
#[test]
fn ac4_openapi_reflects_reproduce_url_on_the_ticket_detail_route() {
    let openapi = read("crates/presentation/src/server/openapi.rs");
    let reflection = openapi_reflection(&openapi, TICKET_ROUTE);
    assert!(
        !reflection.is_empty(),
        "{TICKET_ROUTE} vanished from the OpenAPI table — the CXA-B051 drift \
         gate fails first; this guard follows the table"
    );
    assert!(
        reflection.contains(FIELD),
        "the OpenAPI reflection for {TICKET_ROUTE} must document the optional \
         `{FIELD}` field — found: {reflection}"
    );
}

/// AC4: "the new OPTIONAL field" — the reflection must say the field is
/// optional (a client ignoring it stays valid), not present it as required.
/// Weakest honest pin: the word appears in at least one route's reflection of
/// the field; the exact wording is a QA-phase check against the served
/// document.
#[test]
fn ac4_the_openapi_field_is_documented_as_optional() {
    let openapi = read("crates/presentation/src/server/openapi.rs");
    let both = format!(
        "{}{}",
        openapi_reflection(&openapi, INBOX_ROUTE),
        openapi_reflection(&openapi, TICKET_ROUTE)
    );
    assert!(
        both.to_ascii_lowercase().contains("optional"),
        "the OpenAPI reflection of `{FIELD}` must present it as an OPTIONAL \
         field on the verify-facing routes — found: {both}"
    );
}

/// AC4: "...and openapi_routes_gate continues to pass" — the CXA-B051
/// invariant the gate enforces (every registered service route documented,
/// no orphans), re-run here over the same two source files so an OpenAPI edit
/// made for this ticket that breaks the table shape fails HERE with the
/// reason attached.
#[test]
fn ac4_the_route_drift_gate_invariant_still_holds() {
    let registered: BTreeSet<String> =
        quoted_args(&read("crates/presentation/src/server/mod.rs"), ".route(")
            .into_iter()
            .filter(|p| is_service_path(p))
            .collect();
    let documented: BTreeSet<String> = quoted_args(
        &read("crates/presentation/src/server/openapi.rs"),
        "\n    route(",
    )
    .into_iter()
    .filter(|p| is_service_path(p))
    .collect();
    assert_eq!(
        registered, documented,
        "the OpenAPI ROUTES table must keep mirroring serve_full()'s \
         registrations exactly (the openapi_routes_gate invariant)"
    );
}

/// Every quoted path literal that follows `prefix` — copied from
/// `openapi_routes_gate.rs` so this guard enforces the invariant by the same
/// mechanism the gate itself uses.
fn quoted_args(src: &str, prefix: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = src;
    while let Some(start) = rest.find(prefix) {
        rest = &rest[start + prefix.len()..];
        if let Some(after_quote) = rest.trim_start().strip_prefix('"') {
            if let Some(end) = after_quote.find('"') {
                out.insert(after_quote[..end].to_owned());
                rest = &after_quote[end + 1..];
                continue;
            }
        }
        if rest.is_empty() {
            break;
        }
        rest = &rest[1..];
    }
    out
}

/// UI-shell / embedded-asset leaves deliberately not documented as service
/// routes — the openapi_routes_gate.rs filter, verbatim.
fn is_service_path(path: &str) -> bool {
    path != "/" && !path.starts_with("/assets/") && !path.starts_with("/join/")
}
