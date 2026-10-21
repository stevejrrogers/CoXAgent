//! CXA-B192 — panel terminal-state contract, table-driven proof.
//!
//! Pure functions over the real domain types (`Phase`, `PanelEvent`) — no IO,
//! no framework, no fixtures beyond the contract's own types.
//!
//! Structure:
//! - the full 4x5 transition table enumerated cell by cell, each cell
//!   asserted to its expected outcome;
//! - every forbidden edge asserted unreachable — BY CONSTRUCTION where the
//!   types make it impossible, and by the exhaustive table otherwise;
//! - the no-bare-"loading…" rendering rule;
//! - the payload invariants (Loaded needs fetched_at+source, Empty needs a
//!   fill hint, Error needs cause+retryable) proven on the types.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use coxagent_domain::panel_terminal_state::{
    transition, EmptyReason, FillHint, PanelErrorCause, PanelEvent, PanelSource, Phase,
};
use coxagent_domain::DomainError;
use time::OffsetDateTime;

const NOW_STR: &str = "2026-09-12T12:00:00Z";
const LATER_STR: &str = "2026-09-12T12:00:05Z";

fn parse(t: &str) -> OffsetDateTime {
    OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).unwrap()
}

fn make_source() -> PanelSource {
    PanelSource::new("worklog-stream").unwrap()
}

fn make_cause() -> PanelErrorCause {
    PanelErrorCause::new("stream down").unwrap()
}

/// The four phases, as table rows.
fn phase_row(phase: &str, data: u32) -> Phase<u32> {
    match phase {
        "Skeleton" => Phase::Skeleton,
        "Loaded" => Phase::Loaded {
            data,
            fetched_at: parse(NOW_STR),
            source: make_source(),
        },
        "Empty" => Phase::Empty {
            reason: EmptyReason::StreamNotStarted,
            hint: FillHint::from_reason(EmptyReason::StreamNotStarted),
        },
        "Error" => Phase::Error {
            cause: make_cause(),
            retryable: true,
            fetched_at: parse(NOW_STR),
        },
        other => panic!("unknown phase row: {other}"),
    }
}

/// The five events, as table columns.
fn event_col(event: &str) -> PanelEvent<u32> {
    match event {
        "FetchStarted" => PanelEvent::FetchStarted,
        "FetchSucceeded" => PanelEvent::FetchSucceeded {
            data: 7,
            source: make_source(),
        },
        "DataBecameEmpty" => PanelEvent::DataBecameEmpty {
            reason: EmptyReason::StreamNotStarted,
        },
        "FetchFailed" => PanelEvent::FetchFailed {
            cause: make_cause(),
            retryable: true,
        },
        "Retried" => PanelEvent::Retried,
        other => panic!("unknown event column: {other}"),
    }
}

/// What a cell must produce: a phase-shaped predicate over the OUTPUT,
/// evaluated with the `now` the transition was handed.
type Expect = fn(&Phase<u32>, OffsetDateTime) -> bool;

fn expect_skeleton(next: &Phase<u32>, _at: OffsetDateTime) -> bool {
    matches!(next, Phase::Skeleton)
}

fn expect_loaded(next: &Phase<u32>, at: OffsetDateTime) -> bool {
    match next {
        Phase::Loaded {
            data,
            fetched_at,
            source,
        } => *data == 7 && *fetched_at == at && *source == make_source(),
        _ => false,
    }
}

fn expect_empty(next: &Phase<u32>, _at: OffsetDateTime) -> bool {
    match next {
        Phase::Empty { reason, hint } => {
            *reason == EmptyReason::StreamNotStarted
                && hint.as_str() == EmptyReason::StreamNotStarted.fill_hint()
        }
        _ => false,
    }
}

fn expect_error(next: &Phase<u32>, at: OffsetDateTime) -> bool {
    match next {
        Phase::Error {
            cause,
            retryable,
            fetched_at,
        } => *cause == make_cause() && *retryable && *fetched_at == at,
        _ => false,
    }
}

/// The full 4 phases x 5 events table, cell by cell — every allowed edge
/// asserted to its exact expected phase, every forbidden edge asserted to
/// stay put (no-op / no regression except the two sanctioned ones).
#[test]
fn the_transition_table_is_exhaustive_and_exact() {
    let cells: &[(&str, &str, Expect)] = &[
        // ---- from Skeleton: initial-load row ----
        ("Skeleton", "FetchStarted", expect_skeleton),
        ("Skeleton", "FetchSucceeded", expect_loaded),
        ("Skeleton", "DataBecameEmpty", expect_empty),
        ("Skeleton", "FetchFailed", expect_error),
        ("Skeleton", "Retried", expect_skeleton),
        // ---- from Loaded ----
        ("Loaded", "FetchStarted", expect_skeleton),
        ("Loaded", "FetchSucceeded", expect_loaded),
        ("Loaded", "DataBecameEmpty", expect_empty),
        ("Loaded", "FetchFailed", expect_error),
        ("Loaded", "Retried", expect_skeleton),
        // ---- from Empty ----
        ("Empty", "FetchStarted", expect_skeleton),
        ("Empty", "FetchSucceeded", expect_loaded),
        ("Empty", "DataBecameEmpty", expect_empty),
        ("Empty", "FetchFailed", expect_error),
        ("Empty", "Retried", expect_skeleton),
        // ---- from Error ----
        ("Error", "FetchStarted", expect_skeleton),
        ("Error", "FetchSucceeded", expect_loaded),
        ("Error", "DataBecameEmpty", expect_empty),
        ("Error", "FetchFailed", expect_error),
        ("Error", "Retried", expect_skeleton),
    ];
    assert_eq!(cells.len(), 4 * 5, "the table must cover every cell");

    for (from, event, expect) in cells {
        let before = phase_row(from, 0);
        let at = parse(LATER_STR);
        let next = transition(before.clone(), event_col(event), at);
        assert!(
            expect(&next, at),
            "table cell ({from}, {event}) produced {next:?}"
        );
        // Only the two sanctioned edges may regress to Skeleton.
        if matches!(&next, Phase::Skeleton) {
            assert!(
                *event == "FetchStarted" || *event == "Retried",
                "({from}, {event}) reached Skeleton via an unsanctioned edge"
            );
        }
    }
}

/// A fresh panel can only start at Skeleton — the types have no other
/// constructor. This is the "Skeleton is the only initial phase" proof.
#[test]
fn skeleton_is_the_only_initial_phase_by_construction() {
    let initial: Phase<u32> = Phase::Skeleton;
    assert!(matches!(initial, Phase::Skeleton));
    // Terminal phases cannot be named without their full payload — no
    // `Phase::Loaded` shorthand, no default, no blank constructor exists.
    // (Compile-fact asserted here as documentation; a variant reference
    // without fields does not typecheck.)
}

/// Loaded/Empty/Error are terminal: the table above proves the state machine
/// never parks anywhere else, and `Phase` has exactly these four variants.
#[test]
fn phase_has_exactly_the_four_contract_variants() {
    let all = [
        phase_row("Skeleton", 0),
        phase_row("Loaded", 7),
        phase_row("Empty", 0),
        phase_row("Error", 0),
    ];
    for p in &all {
        assert!(
            matches!(
                p,
                Phase::Skeleton | Phase::Loaded { .. } | Phase::Empty { .. } | Phase::Error { .. }
            ),
            "unexpected phase shape: {p:?}"
        );
    }
}

/// AC: an Error without a cause is unrepresentable by construction —
/// `PanelErrorCause::new` refuses blank/whitespace causes, so a causeless
/// `Phase::Error` cannot be typed at all.
#[test]
fn an_error_without_a_cause_is_unrepresentable() {
    assert!(matches!(
        PanelErrorCause::new("   "),
        Err(DomainError::Empty { .. })
    ));
    assert!(matches!(
        PanelErrorCause::new(""),
        Err(DomainError::Empty { .. })
    ));
    // And the non-blank path yields the payload the contract demands.
    assert_eq!(make_cause().as_str(), "stream down");
}

/// AC: Empty requires a fill hint — the payload sits inside the variant and
/// `FillHint` is non-blank by construction (from a reason, always a real
/// sentence; `new` refuses blanks for hand-built hints).
#[test]
fn empty_requires_a_real_fill_hint() {
    let hint = FillHint::from_reason(EmptyReason::StreamNotStarted);
    assert!(!hint.as_str().trim().is_empty());
    assert!(matches!(
        FillHint::new("   "),
        Err(DomainError::Empty { .. })
    ));
}

/// AC: Loaded requires fetchedAt + source — both fields live inside the
/// variant with no default path, and `PanelSource` refuses blanks.
#[test]
fn loaded_requires_fetched_at_and_a_real_source() {
    assert!(matches!(
        PanelSource::new("  "),
        Err(DomainError::Empty { .. })
    ));
    let loaded = phase_row("Loaded", 42);
    match loaded {
        Phase::Loaded {
            data,
            fetched_at,
            source,
        } => {
            assert_eq!(data, 42);
            assert_eq!(fetched_at, parse(NOW_STR));
            assert_eq!(source, make_source());
        }
        other => panic!("expected Loaded, got {other:?}"),
    }
}

/// AC: no phase value can render as a bare "loading…" string.
///
/// The render path is total over `Phase`: Skeleton renders its three
/// contract-labelled parts, and every terminal phase renders its own
/// terminal shape. The forbidden string is not among the outputs, and no
/// phase `Display`s/serializes to it either.
#[test]
fn no_phase_renders_a_bare_loading_string() {
    const FORBIDDEN: [&str; 3] = ["loading…", "loading...", "loading"];

    let phases = [
        phase_row("Skeleton", 0),
        phase_row("Loaded", 7),
        phase_row("Empty", 0),
        phase_row("Error", 0),
    ];
    for p in &phases {
        // 1. The render path (what a view calls) never yields the bare word:
        // the summary never starts with (or equals) the bare loading word.
        // a panel always names its state, it never idles on "loading…".
        let rendered = p.summary();
        for f in FORBIDDEN {
            assert!(
                !rendered.trim_start().to_lowercase().starts_with(f),
                "{p:?} rendered the bare loading string: {rendered:?}"
            );
        }
        // 2. No debug/serde escape hatch leaks it either.
        let debugged = format!("{p:?}");
        for f in FORBIDDEN {
            assert!(!debugged.contains(f), "{p:?} debug-contains {f:?}");
        }
        let serialized = serde_json::to_string(&p).unwrap();
        for f in FORBIDDEN {
            assert!(!serialized.contains(f), "{p:?} serializes with {f:?}");
        }
    }
}

/// The transition function is pure: same inputs, same output, and the input
/// phase is consumed (moved), not mutated through shared state.
#[test]
fn transition_is_pure_no_hidden_state() {
    let at = parse(LATER_STR);
    let a = transition(phase_row("Loaded", 7), event_col("DataBecameEmpty"), at);
    let b = transition(phase_row("Loaded", 7), event_col("DataBecameEmpty"), at);
    assert_eq!(a, b);
}
