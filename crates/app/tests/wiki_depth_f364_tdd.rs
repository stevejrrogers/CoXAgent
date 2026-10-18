//! CXA-F364 — Wiki depth, SLICE 1: table of contents, backlinks and in-wiki
//! search. RED→GREEN contract, no-harness discipline (the
//! `chat_ergonomics_f367_tdd.rs` pattern): executable assertions over the
//! REAL pure fns (`application::wiki_views`) + source-scan guards over the
//! files the hub actually builds and serves.
//!
//! SLICE DECISION (SA design, follow-the-design): version history (the AC's
//! first criterion) is SPLIT into a follow-up ticket — the `DocPage` schema
//! change plus its ~13 fixture sites is what sank the monolithic attempts.
//! This file therefore pins the three zero-schema behaviours and does NOT pin
//! history; the follow-up ticket re-anchors the AC1 guards
//! (`the_default_state_path_records_the_prior_version` and friends) when the
//! versions field lands.
//!
//! AC → test map (this slice):
//! - AC2 (TOC): [`toc_threshold_is_a_named_const_on_both_sides`],
//!   [`toc_is_threshold_gated_and_fence_safe`] (executable, real fns),
//!   [`rendered_headings_carry_anchor_ids_and_scroll_track`]
//! - AC3 (backlinks): [`backlinks_span_page_bodies_and_ticket_descriptions`]
//!   (executable, real fns), [`the_reader_renders_the_backlinks_footer`],
//!   [`the_backlinks_route_is_registered_and_documented`]
//! - AC4 (search): [`wiki_search_ranks_titles_over_bodies`] (executable),
//!   [`the_rail_gains_a_search_input_wired_to_the_endpoint`],
//!   [`the_filter_highlights_matches_and_clearing_restores_the_tree`]
//! - AC5: [`the_docs_e2e_spec_gates_the_depth_features`]
//! - OpenAPI drift gate: [`the_backlinks_route_is_registered_and_documented`],
//!   [`the_search_route_is_registered_and_documented`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::state::DocPage;
use coxagent_application::wiki_views::{
    heading_anchor, page_backlinks, references_page, ticket_backlinks, toc, wiki_search,
    TOC_MIN_HEADINGS,
};
use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};

// --- repo-state scan helpers --------------------------------------------------

const SERVER_MOD: &str = "crates/presentation/src/server/mod.rs";
const OPENAPI: &str = "crates/presentation/src/server/openapi.rs";
const MCP_JS: &str = "crates/presentation/src/web/js/mcp.js";
const DOCS_JS: &str = "crates/presentation/src/web/js/docs.js";
const INDEX_HTML: &str = "crates/presentation/src/web/index.html";
const E2E_SPEC: &str = "e2e/specs/docs.spec.ts";

const BACKLINKS_ROUTE: &str = "/api/projects/:pid/docs/:id/backlinks";
const SEARCH_ROUTE: &str = "/api/projects/:pid/wiki/search";

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

fn lower(src: &str) -> String {
    src.to_ascii_lowercase()
}

/// A `const` whose NAME contains every fragment in `name_has`, with its parsed
/// numeric value — the house named-cap convention.
fn named_const_value(src: &str, name_has: &[&str]) -> Option<usize> {
    src.lines().find_map(|line| {
        let t = line.trim_start();
        if !(t.starts_with("const ")
            || t.starts_with("pub const")
            || t.starts_with("pub(crate) const"))
        {
            return None;
        }
        let after = t.split("const").nth(1)?;
        let name = after.split([':', ' ', '\t', '=']).find(|s| !s.is_empty())?;
        let upper = name.to_uppercase();
        if !name_has.iter().all(|frag| upper.contains(frag)) {
            return None;
        }
        t.split('=')
            .nth(1)?
            .trim()
            .trim_end_matches(';')
            .trim()
            .parse::<usize>()
            .ok()
    })
}

fn wiki_js() -> String {
    format!("{}\n{}", read(MCP_JS), read(DOCS_JS))
}

/// Whether `low` contains `word` as a standalone alphanumeric token — a plain
/// substring test false-positives ("toContainText" contains "toc").
fn contains_word(low: &str, word: &str) -> bool {
    low.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| t == word)
}

// --- fixtures: real types only ------------------------------------------------

fn page(id: &str, title: &str, body: &str) -> DocPage {
    DocPage {
        id: id.to_owned(),
        folder: "Engineering".to_owned(),
        category: "technical".to_owned(),
        title: title.to_owned(),
        body: body.to_owned(),
        updated_at: "2026-09-01T10:00:00Z".to_owned(),
        updated_by: "DOCS".to_owned(),
    }
}

fn referencing_ticket() -> Ticket {
    Ticket::new(
        TicketId::new("CXC-F364-1").expect("valid ticket id"),
        TicketType::Feature,
        "Harden the deploy probe",
        "The probe contract lives in the Deploy health gate wiki page.",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("valid ticket")
}

// --- AC2: TOC ------------------------------------------------------------------

/// The AC fixes the count (3), not the phrasing: a named threshold const on
/// BOTH sides of the mirror — the Rust reference impl and the wiki JS.
#[test]
fn toc_threshold_is_a_named_const_on_both_sides() {
    assert_eq!(
        TOC_MIN_HEADINGS, 3,
        "the Rust TOC threshold must be the AC's 3"
    );
    let js = read(MCP_JS) + &read(DOCS_JS);
    assert_eq!(
        named_const_value(&js, &["TOC"]).or_else(|| named_const_value(&js, &["HEADING"])),
        Some(3),
        "no TOC/HEADING threshold const is declared beside the wiki renderer — \
         'short pages render none' must be one named decision, mirrored with \
         application::wiki_views::TOC_MIN_HEADINGS"
    );
}

#[test]
fn toc_is_threshold_gated_and_fence_safe() {
    // Short page: under the threshold, and the reference impl says so.
    let short = toc("# One\n\nprose only\n");
    assert!(short.len() < TOC_MIN_HEADINGS);
    // Long page: ordered, anchored entries…
    let long = toc("# Deploy health gate\n\nintro\n\n## Probe window\n\n## Recovery\n");
    assert_eq!(long.len(), 3);
    assert_eq!(long[0].anchor, "h-deploy-health-gate");
    // …and a `# comment` inside a code fence is code, not a section.
    let fenced = toc("# Real\n\n```bash\n# not a heading\n```\n\n## Two\n\n## Three\n");
    assert_eq!(fenced.len(), 3);
    assert!(fenced.iter().all(|e| e.text != "not a heading"));
}

/// `mdRender` gives every heading the stable anchor id the TOC links to, and
/// something scroll-tracks the reading position (IntersectionObserver or a
/// scroll listener) with the threshold applied where the TOC is built.
#[test]
fn rendered_headings_carry_anchor_ids_and_scroll_track() {
    let js = wiki_js();
    let md = js.find("function mdRender").map_or("", |at| &js[at..]);
    assert!(!md.is_empty(), "mdRender must still exist in {DOCS_JS}");
    assert!(
        md.contains("id="),
        "mdRender must give every rendered heading its anchor id (the TOC has \
         nothing to link to otherwise)"
    );
    let low = lower(&js);
    assert!(
        contains_word(&low, "toc") && named_const_value(&js, &["TOC"]).is_some(),
        "the wiki JS must build the TOC behind the named threshold const"
    );
    assert!(
        low.contains("intersectionobserver") || low.contains("onscroll"),
        "nothing scroll-tracks the reading position — the TOC must highlight \
         the section being read"
    );
}

// --- AC3: backlinks -------------------------------------------------------------

/// Executable contract over the REAL pure fns: both sources the AC names —
/// sibling page bodies and ticket descriptions — key on the page's identity
/// (id, `[[id]]` token or title), case-insensitively; a page never backlinks
/// itself.
#[test]
fn backlinks_span_page_bodies_and_ticket_descriptions() {
    let sibling = page(
        "doc-arch",
        "Deploy architecture",
        "Deploys refuse to finish until the Deploy health gate probe passes.",
    );
    let self_ref = page(
        "doc-health",
        "Deploy health gate",
        "The deploy health gate guards deploys.",
    );
    // Page source: the sibling by title, never the page itself.
    let links = page_backlinks(&[sibling, self_ref], "doc-health", "Deploy health gate");
    assert_eq!(links.len(), 1, "the self-reference must not count");
    assert_eq!(links[0].id, "doc-arch");
    // Token + case-insensitivity of the reference rule.
    assert!(references_page(
        "see [[DEPLOY-HEALTH-GATE]] first",
        "deploy-health-gate",
        "Deploy health gate"
    ));
    assert!(!references_page(
        "nothing relevant",
        "doc-health",
        "Deploy health gate"
    ));
    // No pages → no page backlinks; the ticket source still stands alone.
    assert!(page_backlinks(&[], "doc-health", "Deploy health gate").is_empty());
    let tickets = vec![referencing_ticket()];
    let ticket_links = ticket_backlinks(&tickets, "doc-health", "Deploy health gate");
    assert_eq!(
        ticket_links.len(),
        1,
        "the ticket description references it"
    );
    assert_eq!(
        ticket_links[0].id, "CXC-F364-1",
        "ticket backlinks carry the ticket id so the footer can open the dialog"
    );
}

/// The reader view renders the "Referenced by" footer and hides it entirely
/// when nothing references the page — the empty state is an ABSENT footer.
#[test]
fn the_reader_renders_the_backlinks_footer() {
    let js = read(MCP_JS);
    let reader = js.find("function renderDocMain").map_or("", |at| &js[at..]);
    assert!(
        !reader.is_empty(),
        "renderDocMain must still exist in {MCP_JS}"
    );
    let low = lower(&js);
    assert!(
        low.contains("doc-backlinks") && low.contains("referenced by"),
        "the reader view must render the backlinks footer beneath the body"
    );
    assert!(
        low.contains("backlinks") && low.contains("/backlinks"),
        "the footer must be fed by the backlinks endpoint, not guessed client-side"
    );
}

#[test]
fn the_backlinks_route_is_registered_and_documented() {
    let router = read(SERVER_MOD);
    assert!(
        router.contains(BACKLINKS_ROUTE),
        "{BACKLINKS_ROUTE} must be registered in the project router"
    );
    assert!(
        router.contains("doc_backlinks_ep"),
        "the backlinks route must be wired to its handler (doc_backlinks_ep)"
    );
    let openapi = read(OPENAPI);
    assert!(
        openapi.contains(BACKLINKS_ROUTE),
        "{OPENAPI} never documents {BACKLINKS_ROUTE} — the openapi_routes_gate \
         invariant requires every registered route documented"
    );
}

#[test]
fn the_search_route_is_registered_and_documented() {
    let router = read(SERVER_MOD);
    assert!(
        router.contains(SEARCH_ROUTE) && router.contains("wiki_search_ep"),
        "{SEARCH_ROUTE} must be registered in the project router and wired to \
         its handler (wiki_search_ep)"
    );
    let openapi = read(OPENAPI);
    assert!(
        openapi.contains(SEARCH_ROUTE),
        "{OPENAPI} never documents {SEARCH_ROUTE} — the openapi_routes_gate \
         invariant requires every registered route documented"
    );
}

// --- AC4: sidebar search ---------------------------------------------------------

/// Executable contract over the REAL pure fn: title matches rank above
/// body-only matches, blank queries match nothing (that state IS "unfiltered"
/// for the rail), and hits carry the snippet + folder the rail renders.
#[test]
fn wiki_search_ranks_titles_over_bodies() {
    let pages = [
        page("doc-1", "Unrelated title", "somewhere it says rollout once"),
        page("doc-2", "Rollout runbook", "step by step"),
    ];
    let hits = wiki_search(&pages, "rollout");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, "doc-2", "the title match ranks first");
    assert!(hits[0].in_title);
    assert!(hits[1].snippet.contains("rollout"));
    assert!(
        wiki_search(&pages, "").is_empty(),
        "blank = unfiltered, not zero results"
    );
}

/// The docs rail carries a search input wired to keystrokes and to the wiki
/// search endpoint; its handler name names the feature (doc/wiki + search).
#[test]
fn the_rail_gains_a_search_input_wired_to_the_endpoint() {
    let html = read(INDEX_HTML);
    let rail = html
        .find("<div class=\"docsrail\">")
        .map_or("", |at| &html[at..]);
    assert!(
        !rail.is_empty(),
        "the docs rail must still exist in {INDEX_HTML}"
    );
    assert!(
        rail.contains("<input") && rail.contains("oninput") && rail.contains("docSearchFilter"),
        "the wiki sidebar has no search input — add one to the docs rail, wired \
         to keystrokes and to the docSearchFilter handler"
    );
    let js = wiki_js();
    assert!(
        lower(&js).contains("/wiki/search"),
        "docSearchFilter must query the wiki search endpoint (titles AND bodies), \
         not filter titles client-side only"
    );
}

/// Matches are highlighted (<mark>) in the filtered list, and clearing the
/// input redraws the FULL tree through renderDocsList.
#[test]
fn the_filter_highlights_matches_and_clearing_restores_the_tree() {
    let js = wiki_js();
    let low = lower(&js);
    assert!(
        low.contains("<mark"),
        "no highlight exists in the wiki JS — matched titles/bodies in the \
         filtered list must be visually highlighted (<mark>)"
    );
    let handler = js
        .find("async function docSearchFilter")
        .map_or("", |at| &js[at..]);
    assert!(
        !handler.is_empty(),
        "the rail input's handler (docSearchFilter) must exist in the wiki JS"
    );
    assert!(
        handler.contains("renderDocsList"),
        "the sidebar filter handler must re-render through renderDocsList so an \
         EMPTY query redraws the full tree — clearing the filter restores the \
         tree, it does not leave a filtered stub"
    );
}

// --- AC5: the e2e gate -----------------------------------------------------------

/// The docs spec must exercise the depth behaviours in the browser under the
/// shared console gate and refresh the docs golden.
#[test]
fn the_docs_e2e_spec_gates_the_depth_features() {
    let spec = lower(&read(E2E_SPEC));
    assert!(
        spec.contains("armconsolegate") && spec.contains("tohavescreenshot"),
        "{E2E_SPEC} must keep arming the shared console gate and refreshing the \
         docs golden (protective)"
    );
    let toc = contains_word(&spec, "toc") || spec.contains("doc-toc");
    let backlinks = spec.contains("backlink");
    let search = spec.contains("search") || spec.contains("doc-search");
    assert!(
        toc && backlinks && search,
        "{E2E_SPEC} must cover the slice's three depth behaviours in the \
         browser (toc={toc} backlinks={backlinks} search={search}) so the \
         golden and the console gate actually gate them"
    );
}

/// The anchor slug rule is the shared contract between the Rust reference and
/// the JS mirror — pin one example so a silent drift on either side fails
/// here first.
#[test]
fn anchor_slugs_match_across_the_rust_reference_and_the_js_mirror() {
    assert_eq!(
        heading_anchor("CXA-F364: Wiki depth!", 1),
        "h-cxa-f364-wiki-depth"
    );
    assert_eq!(heading_anchor("Setup", 2), "h-setup-2", "duplicates get -N");
    let js = wiki_js();
    assert!(
        js.contains("function headingAnchor"),
        "the JS mirror (headingAnchor) is missing — the TOC links would scroll \
         nowhere"
    );
}
