//! CXA-F275 — Global search: one box across tickets, wiki and chat threads.
//! RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Typing a query in the global search box returns labeled results grouped
//!    by kind (ticket, wiki page, chat message) within the current project
//!    scope, each deep-linking to its surface"
//! 2. "A user with no access to a project never sees that project's tickets,
//!    pages or threads in results, for any role including read-only viewer"
//! 3. "A closed/archived ticket that matches the query is returned with an
//!    'archived' label once archive read-back is live, and silently omitted
//!    before that ships — never an error"
//! 4. "Queries shorter than 2 characters, or with zero matches, show an
//!    explicit empty state rather than a blank panel"
//! 5. "Results are capped per kind with a 'show more' affordance so a
//!    400-ticket backlog cannot render an unbounded list"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure fixtures over the real state/domain
//! types the codebase has today (`ProjectState.tickets` / `.docs` / `.chat`,
//! `Ticket` driven through the REAL transition table, `DocPage`, `ChatMsg`,
//! `Channel::can_view`, `AuthUser.projects`) plus source-scan guards over the
//! module the search use case must live in — the same no-harness discipline as
//! `live_repro_url_f246_tdd.rs`, `reproduce_url_f244_tdd.rs` and
//! `verify_live_link_f245_tdd.rs`: no fake HTTP server, no host harness, no
//! network port, no invented identifiers. A test that called
//! `global_search(...)` directly could not compile today (no such symbol
//! exists anywhere in the workspace — verified before writing this file), so
//! the red half pins the missing use case where it must be declared, and the
//! green half pins the executable semantics over the types that DO exist.
//! Every failing assertion below fails only because CXA-F275's behaviour is
//! missing; if an assertion's mechanism moves during implementation, move the
//! guard with it (the `preflight_f239_tdd.rs` convention).
//!
//! DESIGN INPUT: `.coxagent/design/CXA-F275/` mocks the overlay (search
//! input, scope chips "All / Tickets / Wiki / Chat", grouped sections
//! "TICKETS / WIKI / CHAT", a "No results for …" empty state, keyboard
//! footer). The mock names no API, so the pure contract below is derived from
//! the ACs over the state types that exist. The chat kind is the project's
//! TEAM chat (`state.chat` + `Channel` visibility, the `#payments` room in
//! the mock with its thread replies) — not the agent system-chat feed, which
//! already has its own `/api/chat/search` and is a different surface.
//!
//! DESIGN QUESTION FLAGGED, NOT RESOLVED HERE: AC2 says "for any role",
//! while the house access rule (`river_scope` in server/fleet.rs, the same
//! rule `/api/projects/:pid/*` auth enforces) lets Super/Admin see every
//! project regardless of membership. These guards pin the house rule
//! (membership gates results, Super/Admin bypass documented beside the
//! check); if search must be STRICTER than the house rule, that is an SA
//! decision — the guard needles move with it.
//!
//! Red today, and why:
//!   * AC1 — no search use case exists: `crates/application/src/use_cases/`
//!     has no `global_search` module and `use_cases/mod.rs` registers none.
//!   * AC2 — nothing reads `AuthUser.projects` (or `Channel::can_view`) for
//!     a search surface; the only search endpoint is the system-chat feed's
//!     `/api/chat/search`, which is not this feature.
//!   * AC3 — the use case does not exist, so no code decides closed/archived
//!     tickets; the "once archive read-back is live" half is future work the
//!     AC itself schedules (no archived ticket concept exists in the domain
//!     today — `Status` has no `Archived` variant and `Ticket` carries no
//!     archive flag), so the binding contract now is "never an error, and
//!     the closed case is an explicit decision, not a crash".
//!   * AC4 — with no use case there is no minimum-length guard and no
//!     explicit empty state; `/api/chat/search` answers an empty query with
//!     a bare `[]`, which is exactly the blank panel this AC forbids.
//!   * AC5 — with no use case there is no per-kind cap and no show-more
//!     affordance.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * the three searchable surfaces are real `ProjectState` fields today;
//!   * closed tickets (Done / Verified / Rejected) are reachable through the
//!     REAL transition table, so AC3's premise data exists without
//!     fabrication;
//!   * chat visibility is already a pure membership decision
//!     (`Channel::can_view` / `channels_for`) the chat kind must inherit;
//!   * `AuthUser.projects` membership and the read-only `Viewer` role are
//!     representable, so AC2's data model exists;
//!   * a 400-ticket backlog builds as a plain state literal, so AC5's cap
//!     has a real fixture to be tested against.
//!
//! AC → test map:
//! - AC1: [`ac1_a_global_search_use_case_exists_over_project_state`] (RED),
//!   [`ac1_results_are_labeled_and_grouped_by_kind_across_the_three_surfaces`]
//!   (RED), [`ac1_each_result_deep_links_to_its_surface`] (RED), plus the
//!   green [`the_three_searchable_surfaces_are_real_project_state_today`]
//! - AC2: [`ac2_results_are_scoped_to_projects_the_viewer_is_a_member_of`]
//!   (RED), [`ac2_chat_results_respect_channel_visibility`] (RED), plus the
//!   green [`project_membership_and_read_only_roles_are_representable_today`]
//!   and [`chat_visibility_is_the_membership_rule_chat_results_must_inherit`]
//! - AC3: [`ac3_closed_tickets_are_an_explicit_decision_not_an_error`] (RED),
//!   plus the green
//!   [`closed_tickets_reach_terminal_statuses_through_the_real_transition_table`]
//! - AC4: [`ac4_short_and_zero_match_queries_yield_an_explicit_empty_state`]
//!   (RED)
//! - AC5: [`ac5_results_are_capped_per_kind_with_a_show_more_affordance`]
//!   (RED), plus the green [`a_400_ticket_backlog_builds_as_a_state_literal`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::auth::{AuthRole, AuthUser};
use coxagent_application::state::{ChatMsg, DocPage, ProjectState};
use coxagent_domain::{Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType};

/// The home of the pure search use case: application layer, zero IO — it
/// reads only the data passed in (`ProjectState`, the query, the viewer) and
/// every decision stays a pure function, per the house hexagonal rule. The
/// endpoint then just forwards the resolved principal.
const SEARCH_MODULE: &str = "crates/application/src/use_cases/global_search.rs";

/// The use-case registry the module must be declared in to compile into the
/// crate at all.
const USE_CASE_REGISTRY: &str = "crates/application/src/use_cases/mod.rs";

// --- repo-state scan helpers (the live_repro_url_f246_tdd.rs pattern) --------

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

fn lower(src: &str) -> String {
    src.to_ascii_lowercase()
}

// --- fixtures over the real state/domain types -------------------------------

/// A feature driven to `Done` through the REAL transition table — a closed
/// ticket in the exact shape AC3 governs.
fn done_feature(id: &str, title: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Feature,
        title,
        "the payment provider drops the connection mid-retry",
        Priority::High,
        Complexity::Medium,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            approach: "exponential backoff on the retry loop".to_owned(),
            ..TechnicalDesign::default()
        },
    )
    .expect("SA owns the design");
    t.transition_to(Role::Sa, Status::Ready)
        .expect("designed feature readies");
    t.claim(Role::DevFeature, "finn@mac", "2026-08-30T09:00:00Z")
        .expect("claim is a legal edge");
    t.transition_to(Role::DevFeature, Status::Done)
        .expect("DEV completes the work");
    t
}

/// A bug driven all the way to `Verified` through the REAL transition table —
/// the shipped-and-closed shape. Empty acceptance criteria leave nothing for
/// the coverage gate to demand (domain rule: emptied criteria verify freely).
fn verified_bug(id: &str, title: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Bug,
        title,
        "the payment webhook times out on the second retry",
        Priority::High,
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

/// A feature rejected at the Pending gate through the REAL transition table —
/// the duplicate/noise closure shape.
fn rejected_feature(id: &str, title: &str) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        TicketType::Feature,
        title,
        "duplicate of the payment retry work already shipped",
        Priority::Low,
        Complexity::Small,
        false,
    )
    .expect("valid ticket");
    t.transition_to(Role::Po, Status::Rejected)
        .expect("PO owns the reject gate");
    t
}

/// A wiki page exactly as DOCS/humans write them (`state.docs`).
fn wiki_page(id: &str, title: &str, body: &str) -> DocPage {
    DocPage {
        id: id.to_owned(),
        folder: "Product/Integrations".to_owned(),
        category: "product".to_owned(),
        title: title.to_owned(),
        body: body.to_owned(),
        updated_at: "2026-08-31T10:00:00Z".to_owned(),
        updated_by: "DOCS".to_owned(),
    }
}

/// A team-chat message in a channel, exactly as chat posts persist them
/// (`state.chat`).
fn chat_message(user: &str, body: &str, channel: &str) -> ChatMsg {
    ChatMsg {
        id: format!("msg-{user}-{channel}"),
        at: "2026-08-31T09:41:00Z".to_owned(),
        user: user.to_owned(),
        body: body.to_owned(),
        edited: None,
        channel: channel.to_owned(),
        attachments: Vec::new(),
        reactions: Vec::new(),
        thread_id: None,
        reply_count: 0,
        deleted: false,
    }
}

/// A viewer of the read-only role who IS a member of the searched project —
/// the AC2 subject the phrase "including read-only viewer" names.
fn member_viewer() -> AuthUser {
    AuthUser {
        username: "maya".to_owned(),
        name: String::new(),
        email: String::new(),
        role: AuthRole::Viewer,
        projects: vec!["cxa".to_owned()],
    }
}

/// A lead-tier role with NO membership of the searched project — role
/// authority must not stand in for project access (AC2: "for any role").
fn outsider_lead() -> AuthUser {
    AuthUser {
        username: "mallory".to_owned(),
        name: String::new(),
        email: String::new(),
        role: AuthRole::TechLead,
        projects: vec!["other-project".to_owned()],
    }
}

/// The searched project: one live ticket, one wiki page and one team-chat
/// message about payments, plus the private `#payments` channel the message
/// was posted in — every kind AC1 groups, all in one `ProjectState`.
fn searchable_state() -> ProjectState {
    let mut s = ProjectState::default();
    s.tickets.push(done_feature(
        "CXC-F275-live",
        "Fix payment webhook retries",
    ));
    s.docs.push(wiki_page(
        "doc-payments",
        "Payment webhook integration guide",
        "…configure retry with exponential backoff for failed payment hooks…",
    ));
    s.create_channel("Payments", "maya")
        .expect("maya creates the room");
    s.invite_to_channel("payments", "maya", "bob")
        .expect("maya invites bob");
    s.chat.push(chat_message(
        "maya",
        "the payment hook keeps timing out",
        "payments",
    ));
    s
}

// --- green guards: fixture validity over types that exist today --------------

/// AC1's premise: the three surfaces the search box spans are first-class
/// `ProjectState` data today — tickets, wiki pages and team-chat messages —
/// so the grouped results have real sources and the fixtures above build
/// without fabrication.
#[test]
fn the_three_searchable_surfaces_are_real_project_state_today() {
    let s = searchable_state();
    assert_eq!(s.tickets.len(), 1, "tickets are state.tickets");
    assert_eq!(s.docs.len(), 1, "wiki pages are state.docs");
    assert_eq!(s.chat.len(), 1, "team chat messages are state.chat");
    assert!(
        s.channel("payments").is_some(),
        "the channel the chat message was posted in resolves through state.channel"
    );
}

/// AC3's premise: closed tickets are reachable through the REAL transition
/// table (Done via DEV completion, Verified via the TEST gate, Rejected via
/// the PO gate), so "a closed/archived ticket that matches the query" is
/// data the codebase actually has. No `Archived` status exists yet — the
/// domain's terminal statuses are these, and the archive read-back AC3
/// schedules is future work, not a fixture to fake.
#[test]
fn closed_tickets_reach_terminal_statuses_through_the_real_transition_table() {
    for (t, expected) in [
        (done_feature("CXC-F275-done", "Fix payment webhook retries"), Status::Done),
        (verified_bug("CXC-F275-verified", "Payment webhook timeout"), Status::Verified),
        (
            rejected_feature("CXC-F275-rejected", "Duplicate payment retry idea"),
            Status::Rejected,
        ),
    ] {
        assert_eq!(
            t.status(),
            expected,
            "{} must sit in the terminal status the real transition table drove it to",
            t.id()
        );
    }
}

/// AC2's chat half, executable today: visibility of a team-chat message is
/// already a pure membership decision over the state the search will read.
/// A search result set that leaked a private room's messages would contradict
/// this shipped predicate — the chat kind must reuse it, not re-derive it.
#[test]
fn chat_visibility_is_the_membership_rule_chat_results_must_inherit() {
    let s = searchable_state();
    assert!(
        s.channel("payments")
            .expect("the room exists")
            .can_view("maya"),
        "the owner sees her room"
    );
    assert!(
        s.channel("payments")
            .expect("the room exists")
            .can_view("bob"),
        "an invited member sees the room"
    );
    assert!(
        !s.channel("payments")
            .expect("the room exists")
            .can_view("mallory"),
        "an outsider does not see the room"
    );
    let mallory_sees = s.channels_for("mallory");
    assert!(
        mallory_sees.iter().all(|c| c.id != "payments"),
        "channels_for already excludes rooms the outsider cannot view — the \
         search's chat kind must filter through the same predicate"
    );
}

/// AC2's data model, executable today: project membership rides on the user
/// (`AuthUser.projects`), and the read-only role AC2 calls out is a real
/// assignable role. Role authority alone is not access: the lead-tier
/// outsider holds a powerful role over OTHER projects and none here.
#[test]
fn project_membership_and_read_only_roles_are_representable_today() {
    let member = member_viewer();
    assert!(
        member.projects.iter().any(|p| p == "cxa"),
        "a member of the searched project is representable"
    );
    assert!(
        !AuthRole::Viewer.can_write(),
        "Viewer is the read-only role AC2 names"
    );
    let outsider = outsider_lead();
    assert!(
        outsider.role.is_lead() && !outsider.projects.iter().any(|p| p == "cxa"),
        "role authority without membership is representable — and must see \
         nothing from the project it is not a member of"
    );
}

/// AC5's premise: a 400-ticket backlog builds as a plain state literal — the
/// exact scale the per-kind cap must survive. No fixture gymnastics: the
/// same `Ticket::new` path every ticket takes.
#[test]
fn a_400_ticket_backlog_builds_as_a_state_literal() {
    let mut s = ProjectState::default();
    for i in 0..400 {
        s.tickets.push(
            Ticket::new(
                TicketId::new(format!("CXC-F275-{i:03}")).expect("valid ticket id"),
                TicketType::Feature,
                format!("payment backlog item {i:03}"),
                "seeded for the cap premise",
                Priority::Medium,
                Complexity::Small,
                false,
            )
            .expect("valid ticket"),
        );
    }
    assert_eq!(s.tickets.len(), 400, "the backlog AC5 names is buildable");
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "Typing a query in the global search box returns labeled results
/// grouped by kind (ticket, wiki page, chat message) within the current
/// project scope" — the pure use case the box calls must EXIST over
/// `ProjectState`. RED: no `global_search` module exists anywhere in
/// `crates/application`, and `use_cases/mod.rs` registers none.
#[test]
fn ac1_a_global_search_use_case_exists_over_project_state() {
    let Some(src) = try_read(SEARCH_MODULE) else {
        panic!(
            "no global search use case exists: {SEARCH_MODULE} is absent — \
             AC1's grouped, labeled, deep-linked results have nothing to run"
        );
    };
    assert!(
        !src.trim().is_empty(),
        "{SEARCH_MODULE} exists but is empty — AC1 has no implementation"
    );
    let registry = try_read(USE_CASE_REGISTRY)
        .unwrap_or_else(|| panic!("read {USE_CASE_REGISTRY}"));
    assert!(
        registry.contains("pub mod global_search"),
        "{SEARCH_MODULE} exists but is not registered in {USE_CASE_REGISTRY} \
         (`pub mod global_search`) — it never compiles into the crate"
    );
}

/// AC1: "...labeled results grouped by kind (ticket, wiki page, chat
/// message)..." — the use case must name all three kinds and carry the
/// grouping + labeling structure the overlay renders (the design mock's
/// "TICKETS / WIKI / CHAT" sections). RED: the module does not exist.
#[test]
fn ac1_results_are_labeled_and_grouped_by_kind_across_the_three_surfaces() {
    let src = lower(
        &try_read(SEARCH_MODULE).unwrap_or_else(|| {
            panic!(
                "no global search use case exists: {SEARCH_MODULE} is absent — \
                 AC1's grouped, labeled results have nothing to run"
            )
        }),
    );
    for kind in ["ticket", "chat"] {
        assert!(
            src.contains(kind),
            "the search use case must cover the '{kind}' kind AC1 names — \
             found none in {SEARCH_MODULE}"
        );
    }
    assert!(
        src.contains("wiki") || src.contains("doc"),
        "the search use case must cover the wiki-page kind AC1 names — \
         found no wiki/doc reference in {SEARCH_MODULE}"
    );
    assert!(
        src.contains("group"),
        "results must be grouped by kind (AC1) — no grouping structure in \
         {SEARCH_MODULE}"
    );
    assert!(
        src.contains("label"),
        "results must be labeled by kind (AC1) — no label in {SEARCH_MODULE}"
    );
}

/// AC1: "...each deep-linking to its surface" — every result carries a link
/// target to the surface that serves it. The SPA's real surfaces are the
/// hash routes `#board` (Work/tickets, opened via the ticket modal),
/// `#docs` (Wiki) and `#chat` (Chat) — see `nav()` in web/js/core.js.
/// RED: the module does not exist.
#[test]
fn ac1_each_result_deep_links_to_its_surface() {
    let src = lower(
        &try_read(SEARCH_MODULE).unwrap_or_else(|| {
            panic!(
                "no global search use case exists: {SEARCH_MODULE} is absent — \
                 AC1's deep links have nothing to build them"
            )
        }),
    );
    assert!(
        src.contains("link"),
        "every search result must carry a deep link to its surface (AC1) — \
         no link field/builder in {SEARCH_MODULE}"
    );
    assert!(
        src.contains("board") || src.contains("showticket"),
        "ticket results must deep-link to the ticket surface (the SPA's \
         #board view / showTicket modal) — no ticket target in \
         {SEARCH_MODULE}"
    );
    assert!(
        src.contains("docs") || src.contains("wiki"),
        "wiki results must deep-link to the wiki surface (the SPA's #docs \
         view) — no wiki target in {SEARCH_MODULE}"
    );
    assert!(
        src.contains("chat"),
        "chat results must deep-link to the chat surface (the SPA's #chat \
         view) — no chat target in {SEARCH_MODULE}"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "A user with no access to a project never sees that project's
/// tickets, pages or threads in results, for any role including read-only
/// viewer" — the scope decision must read the caller's project membership
/// (`AuthUser.projects`), with the house Super/Admin bypass documented
/// beside the check (see the design-question note in the file header).
/// RED: the use case does not exist, so nothing reads the caller's
/// membership for search.
#[test]
fn ac2_results_are_scoped_to_projects_the_viewer_is_a_member_of() {
    let src = try_read(SEARCH_MODULE).unwrap_or_else(|| {
        panic!(
            "no global search use case exists: {SEARCH_MODULE} is absent — \
             AC2's project scoping has nothing to enforce it"
        )
    });
    assert!(
        src.contains("projects"),
        "the search use case must scope results to the viewer's project \
         memberships (AuthUser.projects) — no membership read in \
         {SEARCH_MODULE}"
    );
    assert!(
        src.contains("Super") || src.contains("is_super") || src.contains("Admin"),
        "the search use case must document/pin the house Super/Admin bypass \
         beside the membership check (the river_scope rule) — see the \
         design-question note in this file's header"
    );
}

/// AC2: "...never sees that project's ... threads..." — chat results must be
/// filtered through the state's own channel-visibility predicate
/// (`Channel::can_view` / `channels_for`), not re-derived ad hoc. RED: the
/// use case does not exist.
#[test]
fn ac2_chat_results_respect_channel_visibility() {
    let src = lower(
        &try_read(SEARCH_MODULE).unwrap_or_else(|| {
            panic!(
                "no global search use case exists: {SEARCH_MODULE} is absent — \
                 AC2's chat visibility has nothing to enforce it"
            )
        }),
    );
    assert!(
        src.contains("can_view") || src.contains("channels_for"),
        "chat results must be filtered through the shipped channel \
         visibility predicate (Channel::can_view / channels_for) so a \
         private room's threads never leak into search — none referenced \
         in {SEARCH_MODULE}"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "A closed/archived ticket that matches the query is returned with an
/// 'archived' label once archive read-back is live, and silently omitted
/// before that ships — never an error". The binding contract today is the
/// second half: a matching closed ticket (Done / Verified / Rejected — the
/// terminal statuses the real transition table drives tickets to, pinned by
/// the green guard above) is an EXPLICIT decision inside the use case —
/// omitted silently, never an error. The 'archived'-label half activates
/// when archive read-back ships (no archived ticket concept exists in the
/// domain today); when it does, this guard's needle moves with it
/// (preflight_f239_tdd.rs convention). RED: the use case does not exist, so
/// no code decides the closed case at all.
#[test]
fn ac3_closed_tickets_are_an_explicit_decision_not_an_error() {
    let src = lower(
        &try_read(SEARCH_MODULE).unwrap_or_else(|| {
            panic!(
                "no global search use case exists: {SEARCH_MODULE} is absent — \
                 AC3's closed/archived decision has nothing to encode it"
            )
        }),
    );
    assert!(
        src.contains("closed") || src.contains("archived"),
        "the search use case must decide closed/archived tickets explicitly \
         (silently omitted until archive read-back ships, then labeled \
         'archived' — never an error): no closed/archived handling in \
         {SEARCH_MODULE}"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "Queries shorter than 2 characters, or with zero matches, show an
/// explicit empty state rather than a blank panel" — the use case must carry
/// the minimum-length guard AND an explicit empty result the overlay renders
/// as a message (the design mock's "No results for …" state), never a bare
/// blank. RED: the use case does not exist; the closest thing in the
/// codebase, `/api/chat/search`, answers an empty query with a bare `[]` —
/// exactly the blank this AC forbids.
#[test]
fn ac4_short_and_zero_match_queries_yield_an_explicit_empty_state() {
    let src = try_read(SEARCH_MODULE).unwrap_or_else(|| {
        panic!(
            "no global search use case exists: {SEARCH_MODULE} is absent — \
             AC4's empty state has nothing to render it"
        )
    });
    let flat = src.replace(' ', "");
    assert!(
        flat.contains("<2") || flat.contains("MIN_") || flat.contains("min_len"),
        "queries shorter than 2 characters must be refused by an explicit \
         minimum-length guard (AC4) — none found in {SEARCH_MODULE}"
    );
    let low = lower(&src);
    assert!(
        low.contains("empty") || low.contains("no_results") || low.contains("no results"),
        "zero matches must yield an explicit empty state, not a blank panel \
         (AC4) — no empty-state marker in {SEARCH_MODULE}"
    );
}

// --- AC5 ---------------------------------------------------------------------

/// AC5: "Results are capped per kind with a 'show more' affordance so a
/// 400-ticket backlog cannot render an unbounded list" — the use case must
/// bound each kind's returned list (a cap constant, a `take(...)`, or a
/// limit) AND expose the there-is-more flag the overlay renders as the
/// show-more affordance. RED: the use case does not exist; the 400-ticket
/// premise is real today (the green guard above builds it).
#[test]
fn ac5_results_are_capped_per_kind_with_a_show_more_affordance() {
    let src = lower(
        &try_read(SEARCH_MODULE).unwrap_or_else(|| {
            panic!(
                "no global search use case exists: {SEARCH_MODULE} is absent — \
                 AC5's per-kind cap has nothing to bound it"
            )
        }),
    );
    assert!(
        src.contains("take(") || src.contains("cap") || src.contains("limit"),
        "results must be capped per kind so a 400-ticket backlog cannot \
         render unbounded (AC5) — no cap in {SEARCH_MODULE}"
    );
    assert!(
        src.contains("show more")
            || src.contains("show_more")
            || src.contains("has_more"),
        "a capped result set must expose the there-is-more signal the \
         overlay renders as the 'show more' affordance (AC5) — none in \
         {SEARCH_MODULE}"
    );
}
