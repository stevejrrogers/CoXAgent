//! CXA-F303 — Auto-approval policy transparency: inspect what the adaptive
//! gate learned and force a shape back to always-ask. RED half of the TDD
//! pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "A policy view lists every ticket shape known to the adaptive gate with
//!    its current rule (auto-approve / preflight-fix / keep-asking), the
//!    number of approval samples behind it, and the reason string recorded
//!    with the last decision."
//! 2. "From the same view an operator can flip any shape to always-ask and
//!    back; after the flip the next cycle pass no longer auto-approves
//!    tickets of that shape and no restart is needed."
//! 3. "The view distinguishes auto-approval driven by the learned rule from
//!    auto-approval driven by the risk heuristic, showing the why and risk
//!    score recorded in the announcement."
//! 4. "Tickets auto-approved inside the undo window are listed with remaining
//!    undo time and a working undo action; expired ones appear as history
//!    attributed per shape."
//! 5. "When the adaptive gate is disabled in project config the panel shows
//!    that the gate is off and lists no applicable rules."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure fixtures over the real state/domain
//! types the codebase has today (`ProjectState.approval_samples` /
//! `.ask_again_shapes` / `.auto_approved_at`, `ApprovalSample`, `rule_for` ->
//! `Rule`, `shape_key`, `AdaptiveConfig`, the REAL transition table's undo
//! edge) plus source-scan guards over the surfaces that do not exist yet —
//! the same no-harness discipline as `global_search_f275_tdd.rs`,
//! `live_repro_url_f246_tdd.rs` and `evidence_repro_routes_f248_tdd.rs`: no
//! fake HTTP server, no host harness, no network port, no invented
//! identifiers. A test that called the policy read model directly could not
//! compile today (no such symbol exists anywhere in the workspace — verified
//! before writing this file: `approval_policy` / `approval-policy` match
//! nothing), so the red half pins the missing read model where it must be
//! declared, and the green half pins the executable semantics over the types
//! that DO exist. Every failing assertion below fails only because
//! CXA-F303's behaviour is missing; if an assertion's mechanism moves during
//! implementation, move the guard with it (the `preflight_f239_tdd.rs`
//! convention).
//!
//! The read model's home follows the shipped cockpit precedent (CXA-F238):
//! the brake cockpit is a pure read model over `&ProjectState`
//! (`metrics_brakes::brake_cockpit`) served by one thin authorized endpoint
//! (`server/tunecockpit.rs`, `/api/projects/:pid/brakes`). The approval
//! policy view is the same shape of thing for the adaptive gate, so it lives
//! beside its domain: `use_cases/approval_policy.rs`, sibling of
//! `approval_risk.rs` / `approval_memory.rs`, whose `rule_for` / `Rule` /
//! `shape_key` it must consume rather than re-derive.
//!
//! Red today, and why:
//!   * AC1 — no policy read model, route or panel exists anywhere.
//!   * AC2 — a shape can be forced to always-ask only as a SIDE EFFECT of
//!     undoing one auto-approval (`undo_approval_ep` pushes the shape);
//!     nothing anywhere REMOVES a shape from `ask_again_shapes`, so "flip
//!     ... and back" is impossible, and the policy surface registers no
//!     write route. The honoring half is green: the cycle pass reads
//!     `ask_again_shapes` from freshly loaded state every pass, so a flip
//!     applies next pass with no restart, and the field serde-round-trips.
//!   * AC3 — the announcement already records the why and the risk score
//!     (green), but no view distinguishes learned-rule auto-approvals from
//!     risk-heuristic ones (red).
//!   * AC4 — the inbox lists in-window auto-approvals with `minutes_left`
//!     and the undo endpoint works (green), but expired auto-approvals have
//!     no history surface attributed per shape anywhere (red), and the
//!     policy view itself does not exist (red).
//!   * AC5 — `AdaptiveConfig.enabled` is real and the pass early-returns on
//!     it (green), but no panel renders the gate-off state (red).
//!
//! AC → test map:
//! - AC1: [`ac1_an_approval_policy_read_model_exists_over_project_state`]
//!   (RED), [`ac1_the_read_model_lists_every_known_shape_with_rule_samples_and_reason`]
//!   (RED), [`ac1_the_policy_view_is_served_and_rendered`] (RED), plus the
//!   green [`the_gate_rule_pieces_the_view_reads_exist_today`]
//! - AC2: [`ac2_the_view_can_flip_a_shape_to_always_ask_and_back`] (RED),
//!   plus the green [`the_flip_honoring_mechanism_is_state_driven_today`]
//! - AC3: [`ac3_the_view_distinguishes_learned_rule_auto_approvals_from_risk_heuristic_ones`]
//!   (RED), plus the green [`the_announcement_records_the_why_and_risk_score_today`]
//! - AC4: [`ac4_the_view_lists_in_window_auto_approvals_with_their_undo_time`]
//!   (RED), [`ac4_expired_auto_approvals_appear_as_history_attributed_per_shape`]
//!   (RED), plus the green [`the_undo_window_data_and_the_undo_edge_exist_today`]
//! - AC5: [`ac5_a_disabled_gate_shows_the_gate_off_with_no_applicable_rules`]
//!   (RED), plus the green [`a_disabled_gate_is_representable_and_honored_today`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::config::AdaptiveConfig;
use coxagent_application::state::ProjectState;
use coxagent_application::use_cases::approval_memory::{rule_for, ApprovalSample, Rule};
use coxagent_application::use_cases::approval_risk::shape_key;
use coxagent_domain::transitions::{can_transition, transition_allowed};
use coxagent_domain::{
    Complexity, Priority, Role, Status, TechnicalDesign, Ticket, TicketId, TicketType,
};

/// The home of the pure policy read model: application layer, zero IO — it
/// reads only the data passed in (`ProjectState`, the adaptive config) and
/// every decision stays a pure function over `rule_for` / `shape_key`, per
/// the house hexagonal rule and the `brake_cockpit` precedent.
const POLICY_MODULE: &str = "crates/application/src/use_cases/approval_policy.rs";

/// The use-case registry the module must be declared in to compile into the
/// crate at all.
const USE_CASE_REGISTRY: &str = "crates/application/src/use_cases/mod.rs";

/// The cycle pass that must honor a flip on its NEXT pass (no restart): the
/// file that already reads `ask_again_shapes` from freshly loaded state.
const ADAPTIVE_PASS: &str = "crates/application/src/use_cases/cycle/audits.rs";

/// The route surface (server registration) the policy view is served from —
/// the same file that registers `/api/projects/:pid/brakes`.
const ROUTES: &str = "crates/presentation/src/server/mod.rs";

/// The route-drift gate's document: every registered route is documented here.
const OPENAPI: &str = "crates/presentation/src/server/openapi.rs";

/// Tokens that mean "a shape is REMOVED from the always-ask override list" —
/// the flip-BACK direction. Matched against whitespace-flattened source, so
/// they survive any formatting. Today's only `ask_again_shapes` writes are a
/// `contains` check and a `push` (the undo side effect); none match.
const FLIP_BACK_TOKENS: &[&str] = &[
    "ask_again_shapes.remove",
    "ask_again_shapes.retain",
    "ask_again_shapes.drain",
    "ask_again_shapes.swap_remove",
    "ask_again_shapes.replacen",
    "ask_again_shapes=",
];

// --- repo-state scan helpers (the global_search_f275_tdd.rs pattern) ---------

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

/// Source of a repo file, or `None` when absent — an absent file is the RED
/// condition itself, so the caller's assertion (not a read panic) must
/// report the miss with the acceptance criterion attached.
fn try_read(rel: &str) -> Option<String> {
    std::fs::read_to_string(repo_root().join(rel)).ok()
}

/// The policy read model's source, or a panic naming the AC — every red
/// assertion below reports the missing module, never a bare read error.
fn policy_module_source() -> String {
    try_read(POLICY_MODULE).unwrap_or_else(|| {
        panic!(
            "no approval-policy read model exists: {POLICY_MODULE} is absent — \
             the CXA-F303 policy view has nothing to run"
        )
    })
}

fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

fn low(src: &str) -> String {
    flat(src).to_ascii_lowercase()
}

fn any_of(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

/// Windows of `radius` chars around every occurrence of `needle` in
/// already-flattened source — the route-registration window scans.
fn windows_around(hay_flat: &str, needle: &str, radius: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = hay_flat[from..].find(needle) {
        let at = from + at;
        let start = at.saturating_sub(radius);
        let end = (at + needle.len() + radius).min(hay_flat.len());
        out.push(hay_flat[start..end].to_owned());
        from = at + needle.len();
    }
    out
}

/// Every presentation server source with its repo-relative path, sorted for
/// deterministic failure messages.
fn server_sources() -> Vec<(String, String)> {
    let dir = repo_root().join("crates/presentation/src/server");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.extension().is_some_and(|ext| ext == "rs") {
                return None;
            }
            let name = p.file_name()?.to_str()?.to_owned();
            let rel = format!("crates/presentation/src/server/{name}");
            let src = std::fs::read_to_string(&p).ok()?;
            Some((rel, src))
        })
        .collect();
    out.sort();
    out
}

/// Every non-vendored panel script with its repo-relative path, sorted.
fn web_js_sources() -> Vec<(String, String)> {
    let dir = repo_root().join("crates/presentation/src/web/js");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.extension().is_some_and(|ext| ext == "js") {
                return None;
            }
            let name = p.file_name()?.to_str()?.to_owned();
            if name == "mermaid.min.js" {
                return None;
            }
            let rel = format!("crates/presentation/src/web/js/{name}");
            let src = std::fs::read_to_string(&p).ok()?;
            Some((rel, src))
        })
        .collect();
    out.sort();
    out
}

// --- fixtures over the real state/domain types -------------------------------

/// One human decision exactly as the endpoints record it
/// (`state.approval_samples`).
fn sample(shape: &str, decision: &str, by: &str, reason: &str) -> ApprovalSample {
    ApprovalSample {
        shape: shape.to_owned(),
        decision: decision.to_owned(),
        by: by.to_owned(),
        reason: reason.to_owned(),
        at: "2026-08-31T09:00:00Z".to_owned(),
    }
}

/// A designed ticket exactly as the adaptive pass reads it (`shape_key` over
/// the real aggregate).
fn designed_ticket(
    id: &str,
    title: &str,
    kind: TicketType,
    cx: Complexity,
    files: &[&str],
) -> Ticket {
    let mut t = Ticket::new(
        TicketId::new(id).expect("valid ticket id"),
        kind,
        title.to_owned(),
        "the undo window must be inspectable".to_owned(),
        Priority::Medium,
        cx,
        false,
    )
    .expect("valid ticket");
    t.set_technical_design(
        Role::Sa,
        TechnicalDesign {
            files: files.iter().map(|f| (*f).to_owned()).collect(),
            ..TechnicalDesign::default()
        },
    )
    .expect("SA owns the design");
    t
}

// --- green guards: fixture validity over types that exist today --------------

/// AC1's premise: the rule vocabulary the view must show ("auto-approve /
/// preflight-fix / keep-asking") already emerges from real recorded samples
/// through the shipped learner, and the per-shape numbers the view must show
/// (sample count, last decision's reason) are derivable from the same
/// samples — the read model has real inputs, no fabrication needed.
#[test]
fn the_gate_rule_pieces_the_view_reads_exist_today() {
    let approvals: Vec<ApprovalSample> = (0..8)
        .map(|_| sample("test/small", "approve", "luffy", ""))
        .collect();
    assert_eq!(
        rule_for("test/small", &approvals, 8),
        Rule::AutoApprove {
            by: "luffy".to_owned(),
            samples: 8
        },
        "consistent approvals learn the auto-approve rule the view must show"
    );

    let rejects = vec![
        sample("feature/small", "reject", "luffy", "no acceptance criteria"),
        sample("feature/small", "reject", "luffy", "no acceptance criteria"),
    ];
    assert_eq!(
        rule_for("feature/small", &rejects, 8),
        Rule::PreflightFix {
            reason: "no acceptance criteria".to_owned(),
            samples: 2
        },
        "repeated same-reason rejections learn the preflight-fix rule"
    );

    let undone = {
        let mut v: Vec<ApprovalSample> = (0..8)
            .map(|_| sample("bug/medium", "approve", "luffy", ""))
            .collect();
        v.push(sample("bug/medium", "undo", "luffy", ""));
        v
    };
    assert_eq!(
        rule_for("bug/medium", &undone, 8),
        Rule::KeepAsking,
        "one undo outweighs the approvals — the keep-asking rule"
    );
    let few: Vec<ApprovalSample> = (0..3)
        .map(|_| sample("docs/small", "approve", "luffy", ""))
        .collect();
    assert_eq!(rule_for("docs/small", &few, 8), Rule::KeepAsking);

    // The counts and the last-decision reason behind a shape are derivable
    // from the recorded samples — exactly what AC1's listing must show.
    let samples: Vec<ApprovalSample> = approvals.into_iter().chain(rejects).collect();
    let mine: Vec<&ApprovalSample> = samples
        .iter()
        .filter(|s| s.shape == "feature/small")
        .collect();
    assert_eq!(mine.len(), 2, "samples per shape count directly");
    assert_eq!(
        mine.last().map(|s| s.reason.as_str()).unwrap_or_default(),
        "no acceptance criteria",
        "the reason string recorded with the last decision is on the sample"
    );

    // And the shape vocabulary itself comes from real tickets via shape_key.
    let t = designed_ticket(
        "CXC-F303-1",
        "Test coverage: policy view",
        TicketType::Chore,
        Complexity::Small,
        &["crates/app/tests/approval_policy_f303_tdd.rs"],
    );
    assert_eq!(shape_key(&t), "test/small", "shapes name the view's rows");
}

/// AC2's honoring half, executable today: the override list is persisted
/// state (serde round-trip — a flip survives without any restart) and the
/// adaptive pass consults it from freshly loaded state on every pass, so a
/// flip applies to the NEXT pass by construction.
#[test]
fn the_flip_honoring_mechanism_is_state_driven_today() {
    let mut s = ProjectState::default();
    s.ask_again_shapes.push("test/small".to_owned());
    let json = serde_json::to_string(&s).expect("state serializes");
    let back: ProjectState = serde_json::from_str(&json).expect("state round-trips");
    assert!(
        back.ask_again_shapes.iter().any(|sh| sh == "test/small"),
        "the always-ask override list is persisted state, not process memory"
    );
    let pass = read(ADAPTIVE_PASS);
    assert!(
        pass.contains("ask_again_shapes"),
        "the adaptive pass must consult the override list from state each pass \
         (no restart) — it stopped doing so"
    );
}

/// AC3's premise, executable today: the auto-approval announcement the view
/// must quote already records the why and the risk score.
#[test]
fn the_announcement_records_the_why_and_risk_score_today() {
    let pass = read(ADAPTIVE_PASS);
    assert!(
        pass.contains("verdict.why"),
        "the announcement must keep carrying the risk verdict's why — the view \
         has nothing to show otherwise"
    );
    assert!(
        pass.contains("risk {"),
        "the announcement must keep carrying the risk score — the view has \
         nothing to show otherwise"
    );
}

/// AC4's premise, executable today: the undo window resolves from config
/// (0 -> the 30-minute default), the in-window auto-approvals live in
/// `state.auto_approved_at`, and the undo action's domain edge is legal for
/// a human — the "working undo action" has real data and a real edge.
#[test]
fn the_undo_window_data_and_the_undo_edge_exist_today() {
    assert_eq!(
        AdaptiveConfig::default().undo_window_minutes(),
        30,
        "0 means the documented 30-minute default"
    );
    let cfg = AdaptiveConfig {
        enabled: true,
        undo_window_minutes: 45,
        ..AdaptiveConfig::default()
    };
    assert_eq!(cfg.undo_window_minutes(), 45, "an explicit window wins");

    let mut s = ProjectState::default();
    s.auto_approved_at
        .insert("CXC-F303-1".to_owned(), "2026-08-31T09:00:00Z".to_owned());
    assert!(
        s.auto_approved_at.contains_key("CXC-F303-1"),
        "in-window auto-approvals are state.auto_approved_at (id -> window-open time)"
    );

    for t in [TicketType::Feature, TicketType::Chore] {
        assert!(
            transition_allowed(t, Status::Ready, Status::Pending),
            "undo must return a not-yet-started {t:?} to Pending — the action \
             the view exposes has a legal edge"
        );
    }
    assert!(
        can_transition(Role::User, Status::Ready, Status::Pending),
        "the undo is the human operator's move"
    );
}

/// AC5's premise, executable today: the gate-off state is representable in
/// project config and the adaptive pass honors it by not running at all —
/// the panel has a real flag to render.
#[test]
fn a_disabled_gate_is_representable_and_honored_today() {
    let off = AdaptiveConfig {
        enabled: false,
        ..AdaptiveConfig::default()
    };
    assert!(!off.enabled, "adaptive.enabled: false is representable");
    let pass = read(ADAPTIVE_PASS);
    assert!(
        pass.contains("cfg.adaptive.enabled"),
        "the pass must early-return on the disabled flag so 'gate off' is a \
         real, honored state"
    );
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "A policy view lists every ticket shape known to the adaptive gate
/// with its current rule..." — the pure read model the view calls must EXIST
/// over `ProjectState`. RED: no `approval_policy` module exists anywhere in
/// `crates/application`, and `use_cases/mod.rs` registers none.
#[test]
fn ac1_an_approval_policy_read_model_exists_over_project_state() {
    let src = policy_module_source();
    assert!(
        !src.trim().is_empty(),
        "{POLICY_MODULE} exists but is empty — AC1 has no implementation"
    );
    let registry = read(USE_CASE_REGISTRY);
    assert!(
        registry.contains("pub mod approval_policy"),
        "{POLICY_MODULE} exists but is not registered in {USE_CASE_REGISTRY} \
         (`pub mod approval_policy`) — it never compiles into the crate"
    );
}

/// AC1: "...every ticket shape known to the adaptive gate with its current
/// rule (auto-approve / preflight-fix / keep-asking), the number of approval
/// samples behind it, and the reason string recorded with the last decision"
/// — the read model must enumerate shapes from BOTH sources the gate knows
/// (recorded samples and the board's tickets via `shape_key`), derive each
/// rule through the shipped learner, and carry the count and last-decision
/// reason per shape. RED: the module does not exist.
#[test]
fn ac1_the_read_model_lists_every_known_shape_with_rule_samples_and_reason() {
    let src = low(&policy_module_source());
    assert!(
        src.contains("approval_samples"),
        "every shape KNOWN to the gate starts from the recorded approval \
         samples — none referenced in {POLICY_MODULE}"
    );
    assert!(
        src.contains("shape_key"),
        "shapes with no samples yet are still known to the gate through the \
         board's tickets (shape_key) — none referenced in {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["rule_for", "rule"]),
        "the current rule per shape must come from the shipped learner \
         (rule_for/Rule) — none referenced in {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["auto_approve", "autoapprove", "auto-approve"]),
        "the auto-approve rule (AC1) must be nameable in the listing — none \
         in {POLICY_MODULE}"
    );
    assert!(
        src.contains("preflight"),
        "the preflight-fix rule (AC1) must be nameable in the listing — none \
         in {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["keep_asking", "keepasking", "keep-asking"]),
        "the keep-asking rule (AC1) must be nameable in the listing — none \
         in {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["samples", "sample_count", "count"]),
        "the number of approval samples behind each rule (AC1) must be in the \
         listing — none in {POLICY_MODULE}"
    );
    assert!(
        src.contains("reason"),
        "the reason string recorded with the last decision (AC1) must be in \
         the listing — none in {POLICY_MODULE}"
    );
    assert!(
        src.contains("last"),
        "the reason is the LAST decision's (AC1) — the listing must take the \
         latest sample's reason, not the first — none in {POLICY_MODULE}"
    );
}

/// AC1: "A policy view" — the operator-facing surface: a registered, documented
/// route and a panel that fetches it (the `openapi_routes_gate.rs` house rule:
/// every registered route is documented; the new-view house rule: a view that
/// never loads renders nothing). RED: nothing anywhere references the
/// policy surface.
#[test]
fn ac1_the_policy_view_is_served_and_rendered() {
    let routes = flat(&read(ROUTES));
    assert!(
        routes.contains("approval-policy"),
        "no policy view route is registered in {ROUTES} — AC1's view has no \
         server surface"
    );
    let openapi = read(OPENAPI);
    assert!(
        openapi.contains("approval-policy"),
        "the policy view route must be documented in {OPENAPI} (the \
         route-drift gate invariant)"
    );
    let js = web_js_sources();
    assert!(
        js.iter().any(|(_, src)| src.contains("approval-policy")),
        "no panel script fetches the policy view (approval-policy) — AC1's \
         view would never render"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "From the same view an operator can flip any shape to always-ask and
/// back" — flipping BACK is the missing half: today a shape enters
/// `ask_again_shapes` only as the side effect of undoing one auto-approval,
/// and NOTHING ever removes a shape, so a flipped shape can never return to
/// its learned rule. The flip must also be an operator action ON the policy
/// surface: a write route beside the read route. RED: no removal of
/// `ask_again_shapes` exists anywhere, and the policy surface registers no
/// write route.
#[test]
fn ac2_the_view_can_flip_a_shape_to_always_ask_and_back() {
    let mut sources = server_sources();
    if let Some(src) = try_read(POLICY_MODULE) {
        sources.push((POLICY_MODULE.to_owned(), src));
    }
    let flipped_back = sources
        .iter()
        .any(|(_, src)| FLIP_BACK_TOKENS.iter().any(|t| flat(src).contains(t)));
    assert!(
        flipped_back,
        "nothing ever removes a shape from `ask_again_shapes` — an operator \
         can force a shape to always-ask (the undo path pushes it) but can \
         never flip it BACK, so AC2's 'and back' is impossible today"
    );
    let routes = flat(&read(ROUTES));
    let write_route = windows_around(&routes, "approval-policy", 160)
        .iter()
        .any(|w| any_of(w, &["post(", "put(", "patch(", "delete("]));
    assert!(
        write_route,
        "the policy surface registers no write route — the flip is not an \
         operator action on the view (AC2)"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "The view distinguishes auto-approval driven by the learned rule
/// from auto-approval driven by the risk heuristic, showing the why and risk
/// score recorded in the announcement" — the read model must carry the
/// driver of each auto-approval (learned rule vs risk heuristic) alongside
/// the why and risk score the announcement already records (green guard
/// above). RED: the module does not exist, so nothing distinguishes the
/// drivers.
#[test]
fn ac3_the_view_distinguishes_learned_rule_auto_approvals_from_risk_heuristic_ones() {
    let src = low(&policy_module_source());
    assert!(
        src.contains("learned"),
        "auto-approvals driven by the learned rule (AC3) must be labeled as \
         such — no learned marker in {POLICY_MODULE}"
    );
    assert!(
        src.contains("risk"),
        "auto-approvals driven by the risk heuristic (AC3) must be labeled \
         with the risk driver — none in {POLICY_MODULE}"
    );
    assert!(
        src.contains("why"),
        "the why recorded in the announcement (AC3) must be shown — none in \
         {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["score", "risk_score"]),
        "the risk score recorded in the announcement (AC3) must be shown — \
         none in {POLICY_MODULE}"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// AC4: "Tickets auto-approved inside the undo window are listed with
/// remaining undo time and a working undo action" — in the policy view: the
/// read model must list the live `auto_approved_at` entries with their
/// remaining undo time and carry the undo action the endpoints already back
/// (green guard above). RED: the module does not exist.
#[test]
fn ac4_the_view_lists_in_window_auto_approvals_with_their_undo_time() {
    let src = low(&policy_module_source());
    assert!(
        src.contains("auto_approved_at"),
        "the in-window auto-approvals come from state.auto_approved_at — not \
         referenced in {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["undo_window_minutes", "minutes_left"]),
        "each listed auto-approval must carry its remaining undo time (AC4) — \
         none in {POLICY_MODULE}"
    );
    assert!(
        src.contains("undo"),
        "each listed auto-approval must carry a working undo action (AC4) — \
         none in {POLICY_MODULE}"
    );
}

/// AC4: "expired ones appear as history attributed per shape" — an
/// auto-approval whose window closed must remain inspectable, attributed to
/// the shape it auto-approved (not vanish with its map entry). RED: no
/// expired-auto-approval history exists anywhere; `auto_approved_at` entries
/// are only ever removed on undo/approval or dropped as dangling.
#[test]
fn ac4_expired_auto_approvals_appear_as_history_attributed_per_shape() {
    let src = low(&policy_module_source());
    assert!(
        any_of(&src, &["expired", "history"]),
        "auto-approvals past their undo window must appear as history (AC4) — \
         no expired/history surface in {POLICY_MODULE}"
    );
    assert!(
        src.contains("shape"),
        "the history must be attributed per shape (AC4) — no shape \
         attribution in {POLICY_MODULE}"
    );
}

// --- AC5 ---------------------------------------------------------------------

/// AC5: "When the adaptive gate is disabled in project config the panel
/// shows that the gate is off and lists no applicable rules" — the read
/// model must expose the config's enabled flag and an explicit off state
/// whose applicable-rules list is empty (the green guard above proves the
/// flag is real and honored by the pass). RED: the module does not exist.
#[test]
fn ac5_a_disabled_gate_shows_the_gate_off_with_no_applicable_rules() {
    let src = low(&policy_module_source());
    assert!(
        src.contains("enabled"),
        "the read model must expose the adaptive gate's enabled flag (AC5) — \
         none in {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["off", "disabled", "gate_off"]),
        "a disabled gate must render as an explicit off state (AC5) — none in \
         {POLICY_MODULE}"
    );
    assert!(
        any_of(&src, &["rules", "rule"]),
        "the applicable-rules list the off state empties (AC5) must exist in \
         the read model — none in {POLICY_MODULE}"
    );
}
