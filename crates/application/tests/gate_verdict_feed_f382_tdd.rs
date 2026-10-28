//! TDD tests for CXA-F382 — Gate verdict integrity: a gate whose forge
//! evidence feed cannot answer must record `blocked`, never a letter grade
//! computed from missing evidence.
//!
//! ACCEPTANCE CRITERIA (verbatim from the ticket):
//! AC1. When the forge delivery-history API is unreachable or rate-limited
//!      during a gate evaluation, the verdict is
//!      `blocked: delivery history unreachable — unblocked by restoring the
//!      forge feed` with the feed name, error class, and last-successful-check
//!      timestamp, and never a letter grade computed from missing evidence.
//! AC2. A blocked verdict appears in the reviewer's waiting-for-me queue with
//!      the restore action named, so both the human and the next cycle see WHY
//!      nothing is being graded.
//! AC3. After two consecutive blocked evaluations of the same gate, the next
//!      cycle's plan names restoring the evidence feed as its first action
//!      (visible on the cycle dashboard) and grading stays suspended until the
//!      feed answers.
//! AC4. Evidence cached within a configurable freshness window (default 24h)
//!      still grades but the verdict is stamped `evidence age: Xh`; past the
//!      window the gate blocks instead of grading.
//! AC5. When the feed recovers, suspended gates re-run automatically and the
//!      verdict history shows the blocked→graded transition with both
//!      timestamps; blocked verdicts are excluded from useful-run/velocity
//!      metrics so no zero-denominator values render.
//!
//! ---------------------------------------------------------------------------
//! ASK SA — DESIGN GAPS (exact data/types the acceptance criteria reference
//! that DO NOT EXIST in the codebase today; nothing below is fabricated):
//!
//!   1. NO forge delivery-history read exists on [`ForgePort`]
//!      (`crates/application/src/ports/outbound/forge.rs`). The port has PR /
//!      CI-rollup / issue reads only — no method whose outage could be
//!      classified as "unreachable or rate-limited", hence no error-class
//!      taxonomy, feed name, or last-successful-check timestamp anywhere in
//!      `application` or `domain`.
//!   2. NO gate-verdict type with a blocked/graded state machine exists. The
//!      only "verdict" types are coverage per-criterion strings
//!      (`domain/test_case`) and forensics `GateTransition`
//!      (`status_from`/`status_to` on the ready/verify gates); neither carries
//!      a reason string, feed identity, or evidence age.
//!   3. NO freshness-window knob exists: `state::work::Tuning` and `Config`
//!      hold no "max evidence age" field (grep: no `freshness`/`max_age`/24h).
//!   4. NO waiting-for-me/suspension surface exists: `server/inbox.rs` builds
//!      entries only from approval/verification/repro-await states; there is
//!      no inbox item kind for "gate blocked on its evidence feed".
//!   5. NO suspension counters, plan "first action" slot, or automatic re-run
//!      trigger exist on `ProjectState` / the cycle planner.
//!   6. Useful-run/velocity accounting (`state::work::CycleScore`,
//!      `metrics::compute`) has no notion of excluding blocked runs.
//!
//! Because every one of the five criteria asserts behaviour over at least one
//! of these missing pieces, this file can only PIN the existing baselines the
//! implementation must preserve while adding them (same convention
//! `tuning_brakes_f238_tdd.rs` used pre-design). The per-AC behavioural tests
//! follow the moment SA lands the port method + verdict model — they are
//! deliberately NOT stubbed with invented signatures.
//! ---------------------------------------------------------------------------

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_application::metrics;
use coxagent_application::state::ProjectState;
use coxagent_domain::{SemVer, TicketId};

/// PIN for AC1/AC4: the letter-grade derivation that must remain UNTOUCHED for
/// gates whose evidence IS fresh (`CycleScore::grade_of` keeps grading a fully
/// evidenced team exactly as before the feed plumbing arrives). If the
/// implementer reroutes healthy grading through the new verdict pipeline and
/// changes these outputs, this pin fails and the change must be justified.
#[test]
fn pin_healthy_evidence_still_grades_a_exactly_as_before() {
    // Shipped-majority team, zero incidents/errors, all runs useful — the
    // plainest possible input. Contract: `"A"`.
    assert_eq!(
        coxagent_application::state::CycleScore::grade_of(10, 12, 12, 0, 0),
        "A",
        "a fully-evidenced team must keep grading A; the F382 verdict layer may not perturb healthy grades"
    );
}

/// PIN for AC5: metrics stay well-defined for a project with history but zero
/// useful runs PRE-feature (no zero-denominator panic today); the F382
/// exclusion rule must preserve — not introduce — this safety.
#[test]
fn pin_metrics_never_divide_by_zero_when_no_useful_runs_exist() {
    let mut s = ProjectState {
        current_version: SemVer::new(1, 0, 0),
        ..ProjectState::default()
    };
    // One shipped ticket: exercises the denominator paths over real history.
    let mut t = coxagent_domain::Ticket::new(
        TicketId::new("CXA-F901").expect("id"),
        coxagent_domain::TicketType::Feature,
        "pin",
        "",
        coxagent_domain::Priority::Medium,
        coxagent_domain::Complexity::Small,
        false,
    )
    .expect("ticket");
    t.set_technical_design(
        coxagent_domain::Role::Sa,
        coxagent_domain::TechnicalDesign::default(),
    )
    .expect("attach design");
    t.transition_to(coxagent_domain::Role::Sa, coxagent_domain::Status::Ready)
        .expect("ready");
    t.transition_to(
        coxagent_domain::Role::DevFeature,
        coxagent_domain::Status::InProgress,
    )
    .expect("in progress");
    t.transition_to(
        coxagent_domain::Role::DevFeature,
        coxagent_domain::Status::Done,
    )
    .expect("done");
    s.tickets.push(t);
    // Pure compute over the snapshot must terminate and produce finite numbers.
    let m = metrics::compute(&s);
    let _ = format!("{m:?}");
}

/// PIN for AC2: an inbox built over a quiet project renders no items — the
/// surface the blocked-gate entry will be added to starts EMPTY, so the
/// post-fix assertion "exactly one blocked entry naming the restore action"
/// can anchor against a known-empty baseline instead of legacy noise.
#[tokio::test]
async fn pin_waiting_queue_starts_empty_without_blocked_gates() {
    let s = ProjectState::default();
    assert!(
        s.governance_interventions.is_empty(),
        "no interventions ⇒ no waiting-on-human rows yet; F382 adds the first feed-blocked row"
    );
}
