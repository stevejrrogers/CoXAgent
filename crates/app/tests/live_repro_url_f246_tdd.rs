//! CXA-F246 — Derive and expose a live reproduction URL per shipped ticket
//! (from CXA-F242). RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "live_repro_url returns Some(http://HOST_PORT.../) when deploy.host_port
//!    is set and None when it is absent, verified by pure unit tests over
//!    DeployConfig literals"
//! 2. "A shipped UI/api ticket's resolved live reproduction URL is recorded
//!    under state.repro_urls[ticket] when evidence is collected (add_evidence
//!    path writes it)"
//! 3. "Loading a pre-change persisted snapshot lacking repro_urls deserializes
//!    cleanly to an empty map with no panic/migration failure"
//! 4. "/api/projects/:pid/inbox emits a repro_url field on each 'kind':'verify'
//!    item derived from state.repro_urls"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the real
//! state/domain/config types the codebase has today (`ProjectState`,
//! `DeployConfig`, `compute_live_repro_url`) plus source-scan guards over the
//! files the hub actually serves from (`state/mod.rs`, `qa_evidence.rs`,
//! `server/inbox.rs`) — the same no-harness discipline as
//! `reproduce_url_f244_tdd.rs` and `verify_live_link_f245_tdd.rs`: no fake
//! HTTP server, no host harness, no network port, no invented identifiers. A
//! test that called `state.repro_urls` or `.live_repro_url()` directly could
//! not compile today (neither symbol exists), so the red half pins the missing
//! symbols where they must be declared, and the green half pins the executable
//! semantics over the types that DO exist. Every failing assertion below fails
//! only because CXA-F246's behaviour is missing; if an assertion's mechanism
//! moves during implementation, move the guard with it (the
//! `preflight_f239_tdd.rs` convention).
//!
//! Red today, and why:
//!   * AC1 — no `live_repro_url` exists anywhere in `crates/application` (only
//!     `repro_url::compute_live_repro_url(host_port: Option<u16>)`, which takes
//!     a bare port, not the deploy config the AC names).
//!   * AC2 — `ProjectState` (state/mod.rs) declares no `repro_urls` field, and
//!     the evidence-collection path (`qa_evidence.rs`) records only
//!     `ticket_evidence` — it never writes a per-ticket reproduction URL.
//!   * AC3 — with no `repro_urls` field there is no `#[serde(default)]` for a
//!     legacy document to fall back to; the empty-map half is only executable
//!     once the field exists (the round-trip guard below already pins the
//!     no-panic/no-migration contract on both sides of the change).
//!   * AC4 — the inbox verify card carries `reproduce_url` (CXA-F244) derived
//!     from `config.deploy.host_port`; it never emits `repro_url` nor reads a
//!     `state.repro_urls` map.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * the URL format AC1 asserts is the codebase's own:
//!     `compute_live_repro_url` over `DeployConfig` literals;
//!   * the add_evidence path is real and callable for BOTH evidence classes
//!     the AC names (ui → screenshot, api → request/response) through the real
//!     `ProjectState::add_evidence_for`;
//!   * a committed PRE-CHANGE persisted snapshot that lacks `repro_urls`
//!     exists in the repo (`e2e/fixtures/state/state.json`, the frozen e2e
//!     fixture) and loads cleanly today — the exact document shape AC3
//!     governs;
//!   * the inbox verify card this ticket extends is the shipped F244 card.
//!
//! IMPLEMENTER NOTES (contract collisions flagged, not resolved here):
//!   * AC4 names the field `repro_url` while CXA-F244's shipped contract (and
//!     its still-passing guard, `reproduce_url_f244_tdd.rs`) pins
//!     `reproduce_url` on the SAME card — both ACs govern their own ticket, so
//!     the card carries both fields (different derivations: config-derived vs
//!     state-derived) until the team merges them deliberately.
//!   * The committed design `.coxagent/CXA-F246_DESIGN.json` describes a
//!     DIFFERENT surface for this ticket id (`DeployRecord.url` + a
//!     `GET /api/projects/{pid}/shipped` endpoint) than the ACs above. Per the
//!     F244/F245 precedent the sub-ticket's AC text governs; the design
//!     mismatch is flagged to the SA for reconciliation.
//!
//! AC → test map:
//! - AC1: [`ac1_live_repro_url_exists_derived_over_the_deploy_config`] (RED),
//!   plus the green
//!   [`the_deploy_config_literal_resolves_the_live_url_format`]
//! - AC2: [`ac2_project_state_declares_repro_urls_keyed_by_ticket`] (RED),
//!   [`ac2_the_add_evidence_path_records_the_resolved_url`] (RED), plus the
//!   green [`the_add_evidence_path_is_real_and_callable_for_ui_and_api_tickets`]
//! - AC3: [`ac3_the_repro_urls_field_defaults_for_pre_change_snapshots`] (RED),
//!   plus the green [`a_committed_pre_change_snapshot_loads_cleanly_today`] and
//!   [`a_snapshot_without_the_repro_urls_key_round_trips_unchanged`]
//! - AC4: [`ac4_inbox_verify_items_carry_repro_url`] (RED),
//!   [`ac4_the_inbox_derives_repro_url_from_state_repro_urls`] (RED), plus the
//!   green [`the_inbox_verify_card_exists_with_its_shipped_shape`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::config::DeployConfig;
use coxagent_application::repro_url::compute_live_repro_url;
use coxagent_application::state::ProjectState;
use coxagent_domain::{Complexity, Priority, Role, Status, Ticket, TicketId, TicketType};

/// The state field AC2/AC3/AC4 name — `state.repro_urls`, keyed by ticket id.
const FIELD: &str = "repro_urls";

/// The wire field AC4 names — distinct from CXA-F244's `reproduce_url` (see
/// the implementer notes); quoted so it can never substring-match the older
/// key.
const WIRE_FIELD: &str = "\"repro_url\"";

/// The committed pre-change persisted snapshot AC3 governs: the frozen e2e
/// fixture is a real `ProjectState` document written before `repro_urls`
/// existed, tracked in git, and regenerated never.
const LEGACY_SNAPSHOT: &str = "e2e/fixtures/state/state.json";

// --- repo-state scan helpers (the reproduce_url_f244_tdd.rs pattern) ---------

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

/// The source window of one top-level item: from its `header` to the next item
/// introduced by `terminator` (or end of file). Everything the item declares
/// lives inside this window. An absent header yields an empty window — the
/// caller's own assertion, not an index panic, must report the miss.
fn window_of<'a>(src: &'a str, header: &str, terminator: &str) -> &'a str {
    let Some(at) = src.find(header) else {
        return "";
    };
    let rest = &src[at..];
    let end = rest[header.len()..]
        .find(terminator)
        .map_or(rest.len(), |rel| header.len() + rel);
    &rest[..end]
}

/// The source window of the `ProjectState` struct: from its header to the
/// closing brace at column zero — every declared field lives inside it.
fn project_state_struct_window(src: &str) -> &str {
    window_of(src, "pub struct ProjectState", "\n}")
}

/// The source window of the inbox's verify-card construction: from the
/// `"kind": "verify"` literal to the loop's `continue;` — the same window
/// `reproduce_url_f244_tdd.rs` pins. Concatenated over every occurrence, so
/// the card is pinned wherever it is built.
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

// --- fixtures over the real state/domain/config types ------------------------

/// A Bug driven to `Fixed` through the REAL transition table — the shipped,
/// awaiting-verification shape the verify gate and evidence collection key on.
fn fixed_bug(id: &str, has_ui: bool) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Bug,
        "Login returns 500 on an empty email",
        "POST /api/login with an empty email crashes the handler.",
        Priority::High,
        Complexity::Small,
        has_ui,
    )
    .expect("valid ticket");
    t.transition_to(Role::DevBug, Status::InProgress)
        .expect("bug claim is a legal edge");
    t.transition_to(Role::DevBug, Status::Fixed)
        .expect("DEV completes the fix");
    t
}

/// The project state whose shipped ticket carries DoD evidence — one UI ticket
/// (screenshot evidence, the `collect_ui_evidence` shape) and one api ticket
/// (live request/response evidence, the `collect_api_evidence` shape) — both
/// through the REAL `add_evidence_for` path AC2 names.
fn shipped_state_with_evidence() -> ProjectState {
    let mut s = ProjectState::default();
    s.tickets.push(fixed_bug("CXC-246-ui", true));
    s.tickets.push(fixed_bug("CXC-246-api", false));
    s.add_evidence_for(
        "CXC-246-ui",
        "screenshot",
        "deployed UI screenshot",
        "/api/projects/cxa/media/evidence-CXC-246-ui.png",
        &["verify"],
        "TEST",
    );
    s.add_evidence_for(
        "CXC-246-api",
        "api",
        "live request/response",
        "GET http://127.0.0.1:4517/api/health\nHTTP 200",
        &["verify"],
        "TEST",
    );
    s
}

// --- green guards: fixture validity over types that exist today --------------

/// AC1's semantic half, executable today: over `DeployConfig` literals, the
/// live reproduction URL is `http://127.0.0.1:{host_port}/` when the port is
/// set and `None` when it is absent — the exact format AC1 asserts, already
/// pinned by the pure builder the payloads use. When `live_repro_url` lands
/// (AC1's red guard forces it), THIS is the contract its unit tests must
/// reproduce, argument for argument.
#[test]
fn the_deploy_config_literal_resolves_the_live_url_format() {
    let set = DeployConfig {
        host_port: Some(4517),
        ..DeployConfig::default()
    };
    assert_eq!(
        compute_live_repro_url(set.host_port).as_deref(),
        Some("http://127.0.0.1:4517/"),
        "a set deploy.host_port resolves the capture-base URL"
    );
    let absent = DeployConfig::default();
    assert_eq!(
        compute_live_repro_url(absent.host_port),
        None,
        "an absent deploy.host_port resolves nothing"
    );
}

/// AC2's premise: the add_evidence path is real and callable for BOTH
/// evidence classes the AC names — a UI ticket's screenshot and an api
/// ticket's live request/response — through the real aggregate method, and
/// the records land keyed by ticket id (the same key `state.repro_urls`
/// must use).
#[test]
fn the_add_evidence_path_is_real_and_callable_for_ui_and_api_tickets() {
    let s = shipped_state_with_evidence();
    assert!(
        s.ticket_evidence.contains_key("CXC-246-ui"),
        "the UI ticket's collected evidence is keyed by ticket id"
    );
    assert!(
        s.ticket_evidence.contains_key("CXC-246-api"),
        "the api ticket's collected evidence is keyed by ticket id"
    );
}

/// AC3, premise half: the committed snapshot is a genuine PRE-CHANGE document
/// — it lacks `repro_urls` — and it is a real `ProjectState` document (the
/// same type the hub loads), so what this test proves about loading is what
/// the hub's load path does.
#[test]
fn a_committed_pre_change_snapshot_loads_cleanly_today() {
    let doc = read(LEGACY_SNAPSHOT);
    assert!(
        !doc.contains(FIELD),
        "{LEGACY_SNAPSHOT} now contains `{FIELD}` — it is no longer a \
         pre-change snapshot; point AC3's fixture at a persisted document \
         that lacks the key (or freeze a new one) so the legacy-load contract \
         stays testable"
    );
    let loaded: ProjectState = serde_json::from_str(&doc)
        .expect("the pre-change snapshot deserializes with no panic/migration failure");
    assert_eq!(
        loaded.schema_version, 1,
        "the loaded document is the real persisted shape"
    );
}

/// AC3, contract half: a persisted document WITHOUT the `repro_urls` key (a
/// pre-change snapshot, made explicit by removing the key whatever the
/// serializer's skip policy) loads to the SAME state — clean, no panic, no
/// migration failure, no data loss. Green today and the regression guard that
/// must STAY green once the `#[serde(default)]` field exists: if the field
/// ever deserializes as required, or a legacy document panics the loader,
/// this fails with the reason attached.
#[test]
fn a_snapshot_without_the_repro_urls_key_round_trips_unchanged() {
    let shipped = shipped_state_with_evidence();
    let mut doc = serde_json::to_value(&shipped).expect("state serializes");
    doc.as_object_mut()
        .expect("the state document is a JSON object")
        .remove(FIELD);
    let legacy: ProjectState =
        serde_json::from_value(doc).expect("a snapshot lacking repro_urls loads cleanly");
    assert_eq!(
        legacy, shipped,
        "loading a pre-change snapshot must yield the identical state — the \
         missing {FIELD} key defaults, never migrates or drops data"
    );
}

/// AC4's premise: the verify card this ticket extends is the shipped F244
/// card — one construction site, carrying the fields clients already read and
/// the F244 field whose contract (`reproduce_url_f244_tdd.rs`) must keep
/// passing alongside CXA-F246's additive `repro_url`.
#[test]
fn the_inbox_verify_card_exists_with_its_shipped_shape() {
    let inbox = read("crates/presentation/src/server/inbox.rs");
    let card = verify_card_window(&inbox);
    assert!(
        !card.is_empty(),
        "the inbox no longer builds a `kind:\"verify\"` card — the verify \
         surface moved; point this guard at the code that builds it"
    );
    for field in [
        "\"ticket\"",
        "\"title\"",
        "\"role\"",
        "\"can_act\"",
        "\"reproduce_url\"",
    ] {
        assert!(
            card.contains(field),
            "the verify card lost its shipped {field} field — CXA-F246 is \
             additive; card: {card}"
        );
    }
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "`live_repro_url` returns Some(http://HOST_PORT.../) when
/// deploy.host_port is set and None when it is absent" — the named function
/// must EXIST, derived over the deploy config (not a bare port), so pure unit
/// tests can run it over `DeployConfig` literals. RED: no `live_repro_url` is
/// defined anywhere in `crates/application` — the closest symbol,
/// `compute_live_repro_url`, takes `Option<u16>` and is not what the AC
/// names. Accepted homes: an inherent method on `DeployConfig` (config.rs's
/// impl block) or a free function typed over the deploy config in the
/// `repro_url` modules — the AC pins the contract, not the placement. If you
/// put it elsewhere, move this guard with it.
#[test]
fn ac1_live_repro_url_exists_derived_over_the_deploy_config() {
    let config = read("crates/application/src/config.rs");
    let impl_block = window_of(&config, "impl DeployConfig", "\n}");
    if impl_block.contains("fn live_repro_url") {
        return;
    }
    for home in [
        "crates/application/src/repro_url.rs",
        "crates/application/src/use_cases/repro_url.rs",
    ] {
        let src = read(home);
        if src.contains("fn live_repro_url") {
            assert!(
                src.contains("DeployConfig"),
                "`live_repro_url` in {home} must be derived over the deploy \
                 config (`DeployConfig`), not a bare port — AC1 tests run it \
                 over DeployConfig literals"
            );
            return;
        }
    }
    panic!(
        "no `fn live_repro_url` exists in crates/application (config.rs's \
         impl DeployConfig, repro_url.rs, use_cases/repro_url.rs) — AC1's \
         pure unit tests over DeployConfig literals have nothing to call"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "A shipped UI/api ticket's resolved live reproduction URL is recorded
/// under `state.repro_urls[ticket]`" — the state aggregate must declare the
/// per-ticket map. RED: `ProjectState` (state/mod.rs) declares no
/// `repro_urls` field.
#[test]
fn ac2_project_state_declares_repro_urls_keyed_by_ticket() {
    let state = read("crates/application/src/state/mod.rs");
    let struct_window = project_state_struct_window(&state);
    assert!(
        struct_window.contains("pub repro_urls"),
        "ProjectState must declare a `pub repro_urls` map keyed by ticket id \
         (state.repro_urls[ticket]) — found struct window: {}",
        struct_window.len()
    );
}

/// AC2: "...when evidence is collected (add_evidence path writes it)" — the
/// evidence-collection path must be the writer: the collect use case
/// (`qa_evidence.rs`, which resolves `config.deploy.host_port` and files the
/// evidence) or the `add_evidence` path itself must reference the map. RED:
/// neither mentions `repro_urls` today — evidence collection records only
/// `ticket_evidence`, so a collected URL is never persisted per ticket.
#[test]
fn ac2_the_add_evidence_path_records_the_resolved_url() {
    let state = read("crates/application/src/state/mod.rs");
    let add_path = window_of(&state, "pub fn add_evidence_for", "\npub fn ");
    let qa = read("crates/application/src/use_cases/cycle/qa_evidence.rs");
    assert!(
        qa.contains(FIELD) || add_path.contains(FIELD),
        "the evidence-collection path (qa_evidence.rs, or the add_evidence \
         path in state/mod.rs) must record the resolved live reproduction \
         URL under state.{FIELD}[ticket] when it files UI/api evidence"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "Loading a pre-change persisted snapshot lacking repro_urls
/// deserializes cleanly to an empty map..." — the fallback that makes a
/// legacy document load is `#[serde(default)]` on the field. RED: the field
/// does not exist, so no default backs a legacy document; the executable
/// no-panic half is pinned by the green round-trip guards above and stays
/// binding on both sides of the change.
#[test]
fn ac3_the_repro_urls_field_defaults_for_pre_change_snapshots() {
    let state = read("crates/application/src/state/mod.rs");
    let struct_window = project_state_struct_window(&state);
    let at = struct_window
        .find("pub repro_urls")
        .expect("ProjectState declares pub repro_urls (AC2's guard pins it)");
    let before = &struct_window[..at];
    let tail = &before[before.len().saturating_sub(200)..];
    assert!(
        tail.contains("#[serde(default"),
        "the `repro_urls` field must carry `#[serde(default)]` so a \
         pre-change persisted snapshot lacking the key deserializes to an \
         empty map with no panic/migration failure — attributes found before \
         the field: {tail}"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "/api/projects/:pid/inbox emits a `repro_url` field on each
/// 'kind':'verify' item" — the verify card the endpoint builds must carry the
/// field named by THIS ticket's AC (distinct from F244's `reproduce_url`, see
/// the implementer notes; the quoted needle cannot substring-match the older
/// key). RED: the card emits only `reproduce_url` today.
#[test]
fn ac4_inbox_verify_items_carry_repro_url() {
    let inbox = read("crates/presentation/src/server/inbox.rs");
    let card = verify_card_window(&inbox);
    assert!(
        !card.is_empty(),
        "the inbox no longer builds a `kind:\"verify\"` card — the verify \
         surface moved; point this guard at the code that builds it"
    );
    assert!(
        card.contains(WIRE_FIELD),
        "the inbox verify card must emit a `repro_url` field on every \
         kind:verify item — found card: {card}"
    );
}

/// AC4: "...derived from state.repro_urls" — the inbox must read the
/// per-ticket map (not only the config-derived base it uses for
/// `reproduce_url` today) inside the inbox handler. RED: `inbox_ep` never
/// references `repro_urls` — the needle cannot substring-match
/// `repro_url::compute_live_repro_url`, so only a real read of the state map
/// satisfies it.
#[test]
fn ac4_the_inbox_derives_repro_url_from_state_repro_urls() {
    let inbox = read("crates/presentation/src/server/inbox.rs");
    let handler = window_of(
        &inbox,
        "pub(super) async fn inbox_ep",
        "\npub(super) async fn",
    );
    assert!(
        handler.contains("pub(super) async fn inbox_ep"),
        "inbox_ep moved out of server/inbox.rs — point this guard at the \
         handler that builds the inbox"
    );
    assert!(
        handler.contains(FIELD),
        "the inbox handler must derive the verify card's `repro_url` from \
         state.{FIELD} (the per-ticket map recorded by AC2), not only from \
         config.deploy.host_port"
    );
}
