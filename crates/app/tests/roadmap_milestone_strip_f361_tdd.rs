//! TDD tests for CXA-F361 — Roadmap depth: per-milestone progress bars with
//! linked-ticket drill-in. RED half: the roadmap view and its e2e coverage.
//!
//! The ticket's acceptance criteria encoded here, verbatim:
//! 1. "Roadmap shows a milestone strip: name, target-version chip, progress
//!    bar with % derived from ticket completion, completed milestones
//!    collapsed with a check"
//! 2. "Clicking a milestone filters the roadmap buckets to that milestone's
//!    tickets and a second click clears the filter"
//! 4. "Golden screenshots updated; console gate clean"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the bytes of the view
//! source the hub actually serves (`web/js/core.js` — `roadmapGantt` builds
//! the milestone strip, `renderRoadmap` builds the Now/Next/Later/Shipped
//! buckets) plus the spec sources under `e2e/specs`. The same no-harness
//! discipline as `verify_live_link_f245_tdd.rs` and `fleet_river_f233_gate.rs`:
//! no fake HTTP server, no host harness, no network port, no invented
//! identifiers.
//!
//! DATA BASIS (no fabrication): the strip renders from the state the 1 Hz SSE
//! snapshot already pushes. The per-milestone progress figures and the
//! attributed ticket lists are the milestone read model's own fields — the
//! derivation and its wire shape are pinned by the sibling file
//! `crates/application/tests/milestone_projection_f361_tdd.rs`; the snapshot
//! already carries derived read models for exactly this purpose
//! (`derived.blocked`, CXA-F237; `derived.collisions`, CXA-F329 — consumed in
//! the views as `(s.derived||{})`). The completion signal the collapse keys
//! on is the read model's `released` flag, on the wire since F249.
//!
//! Red today, and why:
//!   * AC1 — the strip's bar percent is pure VERSION arithmetic
//!     (`(cur-prevNum)/…`), never ticket completion; the builder never
//!     consults the snapshot's derived read model; and no completed row ever
//!     collapses (every card renders expanded, check icon or not).
//!   * AC2 — strip rows carry no click affordance at all, and the bucket
//!     build filters by STATUS only; no milestone selection exists to filter
//!     by or to clear.
//!   * AC4 — no spec under `e2e/specs` visits the roadmap view (the frozen
//!     fixture seeds no milestones either), so no golden locks the strip and
//!     no console-error gate is armed for it.
//!
//! If an assertion's mechanism moves during implementation (e.g. the bucket
//! filter lands in a helper outside `renderRoadmap`, the derived rows are
//! looked up outside the gantt builder), move the guard with it — the
//! `preflight_f239_tdd.rs` convention. The tokens pinned are the house's own:
//! the `(s.derived||{})` snapshot idiom, the read model's `progress` /
//! `committed` / `released` fields, and the AC's own word "collapsed".

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

const CORE_JS: &str = "crates/presentation/src/web/js/core.js";
const GANTT_HEADER: &str = "function roadmapGantt(s){";
const ROADMAP_HEADER: &str = "function renderRoadmap(){";
const E2E_SPECS_DIR: &str = "e2e/specs";

// --- repo-state scan helpers (the verify_live_link_f245_tdd.rs pattern) -----

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

/// The source window of one view function: from its header to the next
/// top-level `function` declaration. The strip's whole builder — and the
/// buckets' whole builder — live inside their windows.
fn js_function_window<'a>(src: &'a str, header: &str) -> &'a str {
    let Some(at) = src.find(header) else {
        return "";
    };
    let rest = &src[at..];
    let end = ["\nasync function ", "\nfunction "]
        .iter()
        .filter_map(|stop| rest[header.len()..].find(stop))
        .min()
        .map_or(rest.len(), |rel| header.len() + rel);
    &rest[..end]
}

fn gantt_window(core: &str) -> String {
    js_function_window(core, GANTT_HEADER).to_owned()
}

fn roadmap_window(core: &str) -> String {
    js_function_window(core, ROADMAP_HEADER).to_owned()
}

/// True when `label` appears gated on a milestone-COMPLETION signal: some
/// completion classification (`reached` / `released` / `fulfilled` /
/// `goal_complete` — the read model's own vocabulary) occurs before the label
/// with a `?` between — the view code's conditional-rendering idiom. An
/// unconditional marker or a file that never mentions the label fails.
fn gated_on_completion(hay_flat: &str, label: &str) -> bool {
    let Some(label_at) = hay_flat.find(label) else {
        return false;
    };
    let before = &hay_flat[..label_at];
    ["reached", "released", "fulfilled", "goal_complete"].iter().any(|sig| {
        before
            .rfind(sig)
            .is_some_and(|sig_at| before[sig_at..].contains('?'))
    })
}

/// The strip row's onclick expression, verbatim — the click affordance AC2
/// needs. None while the strip rows carry no `onclick` at all.
fn onclick_expression(gantt_flat: &str) -> Option<String> {
    let at = gantt_flat.find("onclick=\"")?;
    let rest = &gantt_flat[at + "onclick=\"".len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// The body that runs on click: the named handler's full source when the
/// expression calls one (`msFilter('Beta')` → the `function msFilter` window,
/// brace-matched), else the inline expression itself (the view code's
/// `onclick="X=…;renderRoadmap()"` idiom). Pure string work over the served
/// source — no invented identifiers: whatever the implementer wrote is what
/// gets inspected.
fn click_handler_body(core_flat: &str, onclick: &str) -> String {
    let Some(name) = onclick.split('(').next() else {
        return onclick.to_owned();
    };
    let is_identifier = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !is_identifier {
        return onclick.to_owned();
    }
    let header = format!("function {name}(");
    let Some(at) = core_flat.find(&header) else {
        return onclick.to_owned();
    };
    let bytes = core_flat.as_bytes();
    let mut depth = 0usize;
    let mut opened = false;
    for (i, b) in bytes[at..].iter().enumerate() {
        match b {
            b'{' => {
                depth += 1;
                opened = true;
            }
            b'}' => {
                depth -= 1;
                if opened && depth == 0 {
                    return core_flat[at..at + i + 1].to_owned();
                }
            }
            _ => {}
        }
    }
    core_flat[at..].to_owned()
}

/// Every Playwright spec under `e2e/specs` with its repo-relative path,
/// sorted for deterministic failure messages (the e2e_acceptance_gate.rs
/// loader).
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

/// AC1: "...progress bar with % derived from ticket completion" — the strip's
/// bar must be fed by the state's derived milestone read model (the snapshot
/// idiom the views already use for the radars), reading each row's `progress`
/// figure the sibling file pins onto the read model. Green premises: the
/// strip shape the AC names is already there — the row renders its name
/// (`mstitle`), its target-version chip (`msver`) and a bar (`msprog`); what
/// is missing is the SOURCE of the percent: today it is pure version
/// arithmetic (`(cur-prevNum)/…`), so the bar can contradict the tickets.
#[test]
fn ac1_the_strip_progress_bar_percent_is_derived_from_ticket_completion() {
    let core = read(CORE_JS);
    let gantt = gantt_window(&core);
    assert!(
        !gantt.is_empty(),
        "roadmapGantt moved out of web/js/core.js — point this guard at the \
         builder that renders the milestone strip"
    );
    let f = flat(&gantt);
    // Green premises — the strip's row shape the AC names:
    assert!(
        f.contains("mstitle") && f.contains("msver") && f.contains("msprog"),
        "the strip must keep rendering each milestone's name, target-version \
         chip and progress bar — found window: {gantt}"
    );
    // RED — the percent's source:
    assert!(
        f.contains("(s.derived||{})"),
        "the strip's progress must come from the state's derived milestone \
         read model (the `(s.derived||{{}})` snapshot idiom, as the radars' \
         consumers do) so the % is derived from ticket completion — today the \
         bar's only source is version arithmetic; found window: {gantt}"
    );
    assert!(
        f.contains(".progress"),
        "the strip's bar must read each milestone row's `progress` figure — \
         the field the sibling file pins onto the read model's wire — found \
         window: {gantt}"
    );
}

/// AC1: "...completed milestones collapsed with a check" — the completed row
/// renders a COLLAPSED state (the AC's own word) gated on its completion
/// classification, with the check icon the strip already uses for reached
/// rows. RED: `collapsed` appears nowhere in the strip builder — every
/// milestone card renders expanded regardless of completion.
#[test]
fn ac1_completed_milestones_collapse_with_a_check() {
    let core = read(CORE_JS);
    let gantt = gantt_window(&core);
    assert!(
        !gantt.is_empty(),
        "roadmapGantt moved out of web/js/core.js — point this guard at the \
         builder that renders the milestone strip"
    );
    let f = flat(&gantt);
    assert!(
        f.contains("circle-check-filled"),
        "the completed milestone's check icon vanished from the strip — found \
         window: {gantt}"
    );
    assert!(
        gated_on_completion(&f, "collapsed"),
        "completed milestones must render COLLAPSED, gated on their completion \
         classification (the `reached`/`released`/`fulfilled`/`goal_complete` \
         signal) — `collapsed` appears nowhere in the strip builder; found \
         window: {gantt}"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "Clicking a milestone filters the roadmap buckets to that milestone's
/// tickets" — two halves, both RED: the strip row must carry a click
/// affordance (today the gantt rows have none — only bucket cards and the
/// mark-complete button are clickable), and the bucket build in
/// `renderRoadmap` must consult the selected milestone's attributed ticket
/// list (the read model's `committed`, the drill-in basis the sibling file
/// pins) when filtering the Now/Next/Later/Shipped items — today the buckets
/// filter by STATUS only and never read a milestone's tickets.
#[test]
fn ac2_clicking_a_milestone_filters_the_buckets_to_its_tickets() {
    let core = read(CORE_JS);
    let gantt = gantt_window(&core);
    assert!(
        !gantt.is_empty(),
        "roadmapGantt moved out of web/js/core.js — point this guard at the \
         builder that renders the milestone strip"
    );
    let f = flat(&gantt);
    assert!(
        f.contains("onclick="),
        "a milestone strip row must be clickable (an onclick affordance on the \
         row) — the strip builder renders none today; found window: {gantt}"
    );

    let buckets = roadmap_window(&core);
    assert!(
        !buckets.is_empty(),
        "renderRoadmap moved out of web/js/core.js — point this guard at the \
         builder that renders the roadmap buckets"
    );
    let bf = flat(&buckets);
    assert!(
        bf.contains(".committed"),
        "the bucket build must filter items by the selected milestone's \
         attributed ticket list (the read model's `committed`) — renderRoadmap \
         never reads a milestone's tickets today; found window: {buckets}"
    );
}

/// AC2: "...and a second click clears the filter" — the click must TOGGLE:
/// the handler (named function or inline expression) compares against the
/// CURRENT selection and clears it on a match. RED: there is no click handler
/// on the strip at all yet, so there is nothing that clears.
#[test]
fn ac2_a_second_click_on_the_same_milestone_clears_the_filter() {
    let core = read(CORE_JS);
    let gantt_flat = flat(&gantt_window(&core));
    let onclick = onclick_expression(&gantt_flat);
    assert!(
        onclick.is_some(),
        "no onclick on the milestone strip rows — there is no toggle to clear \
         the filter with; found window: {gantt_flat}"
    );
    let body = click_handler_body(&flat(&core), &onclick.unwrap_or_default());
    assert!(
        body.contains("==="),
        "the strip's click handler must compare the clicked milestone against \
         the CURRENT selection so a second click can clear it — handler body: \
         {body}"
    );
    assert!(
        body.contains("\"\"") || body.contains("''") || body.contains("null"),
        "the strip's click handler must CLEAR the selection when the same \
         milestone is clicked again (reset to empty/null) — handler body: {body}"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// The suite AC4's criterion runs: `cd e2e && npx playwright test`.
///
/// AC4: "Golden screenshots updated; console gate clean" — a run can only
/// demonstrate that over coverage: a spec that opens the roadmap view, pins
/// the milestone strip with a screenshot golden, and arms the console-error
/// gate. RED: no spec under `e2e/specs` mentions the roadmap view at all (the
/// every-view sweep in `views.spec.ts` visits it but pins no golden and
/// asserts nothing about the strip). Implementer note: the frozen fixture
/// seeds no milestones, so the spec needs its data — extend `e2e/seed.mjs`
/// through the real API or mock the snapshot; the how is yours, the coverage
/// requirement is the AC's.
#[test]
fn ac4_an_e2e_spec_covers_the_milestone_strip_with_a_golden() {
    let specs = spec_sources();
    assert!(
        !specs.is_empty(),
        "no e2e specs found under {E2E_SPECS_DIR} — the acceptance suite \
         itself is missing"
    );
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| src.contains("roadmap") && src.contains("toHaveScreenshot"))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the roadmap's milestone strip with a golden — \
         AC4's refreshed-goldens run has nothing to pass on; expected a spec \
         opening the roadmap view (nav('roadmap')) and pinning the strip with \
         toHaveScreenshot"
    );
    for (rel, _) in &covering {
        assert!(
            read(rel).contains("toHaveScreenshot"),
            "{rel} covers the roadmap but pins no golden — the strip's changed \
             visuals must be locked to a baseline"
        );
    }
}

/// AC4: "...console gate clean" — the house mechanism is the console-error
/// gate (`armConsoleGate` + `assertNoConsoleErrors` from
/// `e2e/specs/helpers.mjs`), armed by every console-clean spec. The roadmap
/// strip's spec must arm it too: a strip render that throws on open or logs
/// an error is exactly the CXA-F233 dead-view class this gate kills. RED: the
/// covering spec does not exist yet (see the sibling AC4 test).
#[test]
fn ac4_the_roadmap_e2e_spec_arms_the_console_error_gate() {
    let specs = spec_sources();
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| src.contains("roadmap") && src.contains("toHaveScreenshot"))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the roadmap's milestone strip — there is no \
         console-error gate to arm for it yet"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("armConsoleGate") && src.contains("assertNoConsoleErrors"),
            "{rel} must arm the console-error gate (armConsoleGate + \
             assertNoConsoleErrors) so 'console gate clean' is actually \
             asserted for the strip render"
        );
    }
}
