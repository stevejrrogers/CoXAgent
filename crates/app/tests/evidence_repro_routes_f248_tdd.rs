//! CXA-F248 — Per-criterion reproduction routes recorded as clickable URLs on
//! Evidence + rendered in review panel (from CXA-F242). Acceptance gate.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "'api'- and screenshot-kind Evidence entries carry an optional resolved
//!    url/reproduction-path field populated from QA verdict routes where
//!    available, written through add-evidence consumption sites"
//! 2. "'route' values that cannot be mapped onto a known live base produce NO
//!    fabricated/unvalidated hyperlink — they are omitted or flagged rather
//!    than guessed"
//! 3. "(cd e2e && npx playwright test) passes after rendering changes - golden
//!    screenshots updated - zero console errors"
//!
//! WHERE THE SUBJECTS LIVE (this tree): the per-criterion evidence records are
//! `CaseEvidence` on the ticket aggregate's test cases
//! (`domain/src/test_case.rs`) — the reviewer's modal renders them
//! (`showTicket` → `tc.evidence.image|note`), and the QA verdict routes are
//! consumed today by the traceability matcher (`use_cases/coverage.rs`
//! `apply_verdict`/`sources_for`), driven from `run_test.rs` — so that is
//! where the resolved url is written, not in parallel bookkeeping.
//!
//! THE KNOWN LIVE BASE: the one resolvability source the evidence collector
//! itself uses — `config.deploy.host_port`, capture base
//! `http://127.0.0.1:{port}/` (`qa_evidence.rs`). A route maps onto THIS base
//! or maps onto nothing (`live_repro_url`).
//!
//! GUARD STYLE: pure functions over the real domain/application types where
//! the behaviour is Rust (no server, no harness, no port), and repo-source
//! scans for the two surfaces that have no executable seam (the classic-script
//! view and the e2e suite) — the established convention of this tree's
//! acceptance gates. A moved mechanism moves its guard with it
//! (`preflight_f239_tdd.rs` convention).
//!
//! AC → test map:
//! - AC1: [`ac1_case_evidence_declares_the_optional_repro_field`],
//!   [`ac1_the_verdict_consumption_site_writes_the_resolved_url`]
//! - AC2: [`ac2_an_unresolvable_route_is_omitted_not_guessed`],
//!   [`ac2_the_review_panel_renders_the_link_only_when_present`]
//! - AC3: [`ac3_an_e2e_spec_covers_the_evidence_repro_link_with_a_golden`],
//!   [`ac3_the_evidence_repro_spec_arms_the_console_error_gate`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::use_cases::coverage::live_repro_url;
use coxagent_domain::{Complexity, Priority, Ticket, TicketId, TicketType};

/// The optional resolved url/reproduction-path field AC1 puts on the
/// per-criterion evidence record.
const FIELD: &str = "repro";

/// The suite AC3's criterion runs: `cd e2e && npx playwright test`.
const E2E_SPECS_DIR: &str = "e2e/specs";

// --- repo-state scan helpers --------------------------------------------------

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

/// The source with ALL whitespace removed, so a guard survives the view code's
/// dense one-line formatting style without pinning its line breaks.
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The window of the view code that renders one test case's evidence, located
/// by the `tc.evidence` payload access rather than by name, so a refactor of
/// the surrounding function does not orphan the guard. Empty when nothing
/// renders test-case evidence at all.
fn test_case_render_window(src: &str) -> String {
    const TOKEN: &str = "tc.evidence&&tc.evidence";
    let Some(at) = src.find(TOKEN) else {
        return String::new();
    };
    let end = src[at..]
        .find("covPane(t)")
        .map_or(src.len(), |rel| at + rel);
    src[at..end].to_owned()
}

/// True when `field` is referenced BEFORE the `label` occurrence in the
/// flattened window with a `?` between them — the view code's conditional-
/// rendering idiom (`tc.evidence&&tc.evidence.repro?…`), i.e. the link renders
/// only when the field is present. An unconditional render or a field never
/// consulted does not satisfy it.
fn presence_guarded(hay_flat: &str, field: &str, label_flat: &str) -> bool {
    let Some(label_at) = hay_flat.find(label_flat) else {
        return false;
    };
    let Some(field_at) = hay_flat[..label_at].rfind(field) else {
        return false;
    };
    hay_flat[field_at..label_at].contains('?')
}

/// Every Playwright spec under `e2e/specs` with its repo-relative path,
/// sorted for deterministic failure messages.
fn spec_sources() -> Vec<(String, String)> {
    let dir = repo_root().join(E2E_SPECS_DIR);
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?.to_owned();
            (name.ends_with(".spec.ts")).then(|| {
                let rel = format!("{E2E_SPECS_DIR}/{name}");
                let src = std::fs::read_to_string(&p).ok()?;
                Some((rel, src))
            })?
        })
        .collect();
    out.sort();
    out
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: the per-criterion evidence record declares the optional resolved
/// url/reproduction-path field — `Option<String>` (optional on the type),
/// serde-defaulted and serde-skipped when `None` (absent on the wire), so
/// pre-F248 persisted tickets load unchanged and an unresolved route is
/// omitted, never serialized as a guessed link.
#[test]
fn ac1_case_evidence_declares_the_optional_repro_field() {
    let src = read("crates/domain/src/test_case.rs");
    let start = src
        .find("pub struct CaseEvidence")
        .unwrap_or_else(|| panic!("CaseEvidence moved out of domain/src/test_case.rs — point this guard at the record type that carries per-criterion evidence"));
    let st = &src[start..start + src[start..].find("\n}").map_or(src.len(), |rel| rel)];
    let decl = format!("pub {FIELD}: Option<String>");
    let field_at = st
        .find(&decl)
        .unwrap_or_else(|| panic!("CaseEvidence must declare `{decl}` — the optional resolved url/reproduction-path field AC1 names. Struct window: {st}"));
    let attrs = &st[..field_at];
    let attr_start = attrs
        .rfind("#[serde")
        .expect("a serde attribute precedes the field");
    let attr = &attrs[attr_start..];
    assert!(
        attr.contains("default"),
        "`{FIELD}` must be serde-defaulted so pre-F248 persisted records load unchanged — attribute: {attr}"
    );
    assert!(
        attr.contains("Option::is_none"),
        "`{FIELD}` must be serde-skipped when None so an unresolved route is OMITTED on the wire, never serialized as a fabricated link — attribute: {attr}"
    );
}

/// AC1: "...written through add-evidence consumption sites" — the verdict
/// consumption site (`apply_verdict`, where QA verdict routes are consumed
/// today into case evidence + sources) must write the resolved url onto the
/// case's evidence, fed from `run_test.rs`'s deploy base.
#[test]
fn ac1_the_verdict_consumption_site_writes_the_resolved_url() {
    let cov = read("crates/application/src/use_cases/coverage.rs");
    let start = cov.find("fn apply_verdict").unwrap_or_else(|| {
        panic!(
            "apply_verdict moved out of use_cases/coverage.rs — point this guard \
             at the site that applies TEST verdicts (incl. their routes) to a case"
        )
    });
    let site = &cov[start
        ..start
            + cov[start..]
                .find("\n/// ")
                .map_or(cov.len() - start, |rel| rel)];
    assert!(
        site.contains(FIELD) || site.contains("live_repro_url"),
        "the verdict consumption site must write the resolved `{FIELD}` (the QA \
         verdict route mapped onto the known live base) onto the case's \
         evidence — site window: {site}"
    );
    let run_test = read("crates/application/src/use_cases/run_test.rs");
    let call_at = run_test
        .find("record_verdicts(")
        .unwrap_or_else(|| panic!("run_test stopped calling record_verdicts — point this guard at the verdict-recording call"));
    let call = &run_test[call_at..call_at + run_test[call_at..].find(')').expect("call closes")];
    assert!(
        call.contains("host_port"),
        "run_test must feed the verdict consumption site the deploy base it \
         already holds (`config.deploy.host_port`) so routes resolve at the \
         point verdicts are recorded — call window: {call}"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "'route' values that cannot be mapped onto a known live base produce
/// NO fabricated/unvalidated hyperlink — they are omitted". The resolver is a
/// pure function over the real types: an app path plus the configured deploy
/// base resolves; every unmappable shape — no base, blank, a bare word, an
/// agent-invented absolute URL — stays `None`.
#[test]
fn ac2_an_unresolvable_route_is_omitted_not_guessed() {
    assert_eq!(
        live_repro_url("/settings", Some(8101)).as_deref(),
        Some("http://127.0.0.1:8101/settings"),
        "an app path on the deployed app resolves onto the collector's own base"
    );
    // The omission half — no base, no path, no link. Never a guess.
    assert_eq!(live_repro_url("/settings", None), None, "no deploy base");
    assert_eq!(live_repro_url("", Some(8101)), None, "no route");
    assert_eq!(live_repro_url("   ", Some(8101)), None, "blank route");
    assert_eq!(
        live_repro_url("settings", Some(8101)),
        None,
        "not an app path"
    );
    assert_eq!(
        live_repro_url("http://evil.example/settings", Some(8101)),
        None,
        "an unvalidated absolute URL is never turned into a hyperlink"
    );
    // And the domain refuses a blank write outright, so even a buggy caller
    // cannot materialize a placeholder link on the record.
    let mut t = Ticket::new(
        TicketId::new("F248").expect("id"),
        TicketType::Feature,
        "t",
        "d",
        Priority::Medium,
        Complexity::Small,
        false,
    )
    .expect("ticket");
    t.set_acceptance_criteria(vec!["ac".to_owned()]);
    t.ensure_test_cases_from_acceptance();
    assert!(!t.set_test_case_repro("ac", "   ".into()));
    assert!(
        t.test_cases()[0]
            .evidence
            .as_ref()
            .map_or(true, |e| e.repro.is_none()),
        "a refused blank write leaves the field absent — no placeholder link"
    );
}

/// AC2: the review panel half — the Test cases render in the reviewer's modal
/// and the reproduction link must be GUARDED by the field's presence (the
/// view's `&&…?…:""` idiom), so an entry with no resolved url renders NO link
/// at all — not a dead `<a>`, not an href built from the raw route text.
#[test]
fn ac2_the_review_panel_renders_the_link_only_when_present() {
    let chat = read("crates/presentation/src/web/js/chat.js");
    let pane = test_case_render_window(&chat);
    assert!(
        !pane.is_empty(),
        "no view code renders test-case evidence (`tc.evidence`) — point this \
         guard at the window that renders the review panel's Test cases"
    );
    let f = flat(&pane);
    let occurrences = f.matches(FIELD).count();
    assert!(
        occurrences > 0,
        "the review panel must render the evidence's resolved `{FIELD}` as a \
         clickable reproduction link (guarded by its presence) — the Test \
         cases render never references the field"
    );
    let guarded = presence_guarded(&f, FIELD, "tcrepro");
    assert!(
        guarded,
        "the `{FIELD}` link must be rendered conditionally on the field's \
         presence (the `&&…?…:\"\"` idiom) so an unresolved route produces NO \
         hyperlink at all — {occurrences} reference(s) in: {pane}"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "(cd e2e && npx playwright test) passes after rendering changes -
/// golden screenshots updated" — a run can only demonstrate that over
/// coverage: a spec that renders the reproduction link ON evidence entries in
/// the review panel and pins the changed visuals with a screenshot golden.
#[test]
fn ac3_an_e2e_spec_covers_the_evidence_repro_link_with_a_golden() {
    let specs = spec_sources();
    assert!(
        !specs.is_empty(),
        "no e2e specs found under {E2E_SPECS_DIR} — the acceptance suite itself is missing"
    );
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| src.contains(FIELD) && src.to_lowercase().contains("evidence"))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the evidence reproduction link (a spec pairing \
         `{FIELD}` with the evidence entries it renders on) — the `playwright \
         test` run of AC3 cannot demonstrate the changed review-panel visuals \
         without it"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("toHaveScreenshot"),
            "{rel} covers the evidence reproduction link but pins no golden — \
             AC3's 'golden screenshots updated' needs the changed review-panel \
             visuals locked to a baseline"
        );
    }
}

/// AC3: "...zero console errors" — the house mechanism is the console-error
/// gate (`armConsoleGate` + `assertNoConsoleErrors` from
/// `e2e/specs/helpers.mjs`). The evidence reproduction spec must arm it too.
#[test]
fn ac3_the_evidence_repro_spec_arms_the_console_error_gate() {
    let specs = spec_sources();
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| src.contains(FIELD) && src.to_lowercase().contains("evidence"))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the evidence reproduction link — there is no console-error gate to arm for it yet"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("armConsoleGate") && src.contains("assertNoConsoleErrors"),
            "{rel} must arm the console-error gate (armConsoleGate + \
             assertNoConsoleErrors) so 'zero console errors' is actually \
             asserted for the evidence reproduction render"
        );
    }
}
