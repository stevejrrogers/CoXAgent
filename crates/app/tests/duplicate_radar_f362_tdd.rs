//! CXA-F362 — Duplicate radar depth: scan-now action, last-scan stamp and
//! scan cadence. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "Dupes view header shows last-scan relative time and the scan cadence;
//!    a Scan now button (admin-gated) re-runs the scan and re-renders with a
//!    spinner state"
//! 2. "Dismissing a duplicate pair shows a 10s undo toast that restores it"
//! 3. "scanned_at is persisted server-side and survives hub restart"
//! 4. "Golden screenshots updated; console gate clean"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the bytes of the
//! sources the hub actually serves and ships — the dupes view block of
//! `web/js/shell.js` (`renderDupes` → `dupesCard` → `dupeAction`, treated as
//! one contiguous region because the depth pass adds helpers inside it), the
//! radar's HTTP adapter (`server/duplicate_radar.rs`), the persisted hub doc
//! (`server/hub_docs.rs`), and the e2e spec sources the AC4 run executes —
//! plus, as the premise, the REAL pure scan core the re-run re-executes
//! (`use_cases::duplicate_radar::find_cross_project_duplicates`). The same
//! no-harness discipline as `live_repro_link_f247_tdd.rs`,
//! `live_repro_url_f246_tdd.rs` and `reproduce_url_f244_tdd.rs`: no fake HTTP
//! server, no host harness, no network port. Every failing assertion below
//! fails only because CXA-F362's behaviour is missing; if an assertion's
//! mechanism moves during implementation, move the guard with it (the
//! `preflight_f239_tdd.rs` convention).
//!
//! WHY SOURCE-WINDOW GUARDS FOR THE PERSISTED STATE: AC3's `scanned_at` has
//! exactly one coherent home — `WorkspaceDoc` (`server/hub_docs.rs`), the
//! hub-level doc that already persists the radar's other server state, the
//! `dupe_allowlist`, and whose `Ws::load`/`Ws::save` IS the hub-restart
//! boundary (the whole doc round-trips through `workspace.json` / the shared
//! KV). `WorkspaceDoc` is `pub(super)` to the presentation crate, so an
//! integration test cannot name the type — the `live_repro_url_f246_tdd.rs`
//! struct-window precedent (`project_state_struct_window`) is the family's
//! mechanism for exactly this: pin the field on the struct's source, with the
//! serde-defaulted additive-migration idiom every field on that doc uses, so
//! a pre-F362 workspace doc loads clean after a restart and the saved stamp
//! round-trips by construction. No type is invented, no fixture fabricated.
//!
//! FOUR NAMING/BOUNDARY DECISIONS, made explicit (the sub-ticket's AC text
//! governs — the precedent set by `live_repro_link_f247_tdd.rs`):
//!
//! * THE STAMP. AC3 names the persisted field `scanned_at` word-for-word.
//!   The Rust field and the JS wire value are bound through [`norm`], which
//!   strips `_` so the AC's snake_case `scanned_at` and the view's idiomatic
//!   camelCase `scannedAt` both match — no wire rename is guessed beyond what
//!   the AC itself names.
//!
//! * THE DISMISS. AC2's verb is "Dismissing". The dupes view today offers
//!   Redirect / Reject / Allow both — and `crates/domain/src/transitions.rs`
//!   gives `Rejected` NO outgoing transition (it is terminal), so an undoable
//!   dismissal CANNOT be the reject verdict: restoring a rejected ticket would
//!   be an illegal domain transition. "Dismiss" is therefore a new,
//!   non-destructive radar-level action on the reported pair, matched
//!   normalized ([`norm`]) so any casing/separator of "dismiss" satisfies it.
//!   Whether the dismissal is client-transient or server-persisted is a
//!   design decision the ACs do not make — these guards deliberately do not
//!   pin it (only the toast, its 10 s window, and the restore do). FLAG FOR
//!   THE SA: that persistence semantics gap should be settled before green.
//!
//! * THE CADENCE. AC1 says the header shows "the scan cadence". No radar
//!   cadence exists anywhere yet (the radar computes per GET; there is no
//!   background scan task and no cadence config), so the guards pin the
//!   PRESENCE of the cadence in the header, not its value — the interval and
//!   who schedules it (background task vs per-request stamp) are SA decisions
//!   this RED half must not fabricate. FLAG FOR THE SA: same gap class.
//!
//! * THE ADMIN GATE. "(admin-gated)" is enforced where it is observable:
//!   the button renders only for hub admins (the shipped client check
//!   `isHubAdmin()`, `shell.js`), and the re-run is gated server-side through
//!   the house authorization gate `gate_principal` (whose signature only
//!   accepts `AuthRole` predicates — WHICH predicate, `can_manage` vs a
//!   stricter admin set, is the implementer's call; the guard pins the gate
//!   mechanism, not the predicate).
//!
//! The 10 s window is pinned as `10000` through [`norm`], so the idiomatic
//! `setTimeout(…, 10000)` and an ES2021 numeric separator `10_000` both
//! match. The spinner is pinned to the house spinner idiom `att-spin`
//! (`ti ti-loader-2 att-spin`, used by shell.js/core.js for every in-flight
//! state). The relative time is pinned to the shipped `relTime()` helper —
//! the exact mechanism the People view uses for "last active".
//!
//! Red today, and why:
//!   * AC1 — the dupes header (the count panel in `renderDupes`) shows only
//!     the pair count and a hint; no `relTime(`, no cadence, no Scan now
//!     control, no spinner; the radar adapter has no scan handler at all.
//!   * AC2 — no dismiss action exists, `toast()` has a hardcoded 3000 ms
//!     lifetime and no action support, and nothing in the dupes flow undo.
//!   * AC3 — no `scanned_at` exists anywhere in the codebase (grep across
//!     crates/), the GET response carries only `crossProjectDuplicates`, and
//!     nothing stamps or persists a scan instant.
//!   * AC4 — `e2e/specs/dupes.spec.ts` covers the F253 surface only: no
//!     scan-now/dismiss/undo coverage and not a single `toHaveScreenshot`
//!     golden (there is no `dupes.spec.ts-snapshots/` directory).
//!
//! AC → test map:
//! - premise: [`premise_the_pure_scan_core_the_rerun_reexecutes_is_intact`]
//! - AC1: [`ac1_the_dupes_header_shows_the_last_scan_relative_time`],
//!   [`ac1_the_dupes_header_shows_the_scan_cadence`],
//!   [`ac1_a_scan_now_button_is_rendered_only_for_hub_admins`],
//!   [`ac1_the_scan_rerun_rerenders_with_a_spinner_state`],
//!   [`ac1_the_scan_rerun_exists_server_side_and_is_admin_gated`]
//! - AC2: [`ac2_dismissing_a_duplicate_pair_is_a_first_class_action`],
//!   [`ac2_the_dismissal_shows_a_ten_second_undo_toast`],
//!   [`ac2_undo_restores_the_dismissed_pair`]
//! - AC3: [`ac3_the_workspace_doc_declares_a_defaulted_scanned_at_field`],
//!   [`ac3_a_scan_stamps_and_persists_scanned_at_server_side`],
//!   [`ac3_the_radar_payload_serves_the_stamp_from_the_persisted_doc`]
//! - AC4: [`ac4_an_e2e_spec_covers_the_scan_now_and_undo_toast_with_updated_goldens`],
//!   [`ac4_the_dupes_e2e_spec_arms_the_console_error_gate`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::use_cases::duplicate_radar::{
    find_cross_project_duplicates, TicketSnapshot,
};

// --- repo-state scan helpers (the live_repro_link_f247_tdd.rs pattern) ------

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

/// The source lowercased with whitespace, hyphens AND underscores removed, so
/// one normalized token matches the AC's own phrasing and every idiomatic
/// spelling of it across the stack: the AC's `scanned_at` matches a Rust
/// `scanned_at` field and a JS `scannedAt` wire read; "Scan now" matches
/// `scanNow`; "10s" matches `10000` and `10_000`; `att-spin` matches the
/// class attribute it rides in. Punctuation that carries structure (`(`, `"`,
/// `?`) is kept — the positional guards read it.
fn norm(src: &str) -> String {
    src.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The source with ALL whitespace removed, case preserved — for checks on
/// exact source tokens (`relTime(`, `#[serde(default)]`, `workspace.save`).
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The source window of one item: from its `header` to the next `terminator`
/// (or end of file). Everything the item declares lives inside this window;
/// an absent header yields an empty window — the repoint signal (the
/// `live_repro_url_f246_tdd.rs` helper, verbatim).
fn window_of<'a>(src: &'a str, header: &str, terminator: &str) -> &'a str {
    let Some(at) = src.find(header) else {
        return "";
    };
    let rest = &src[at..];
    let end = rest
        .find(terminator)
        .map_or(rest.len(), |rel| rel + terminator.len());
    &rest[..end]
}

/// The contiguous dupes region of the view: from `renderDupes`'s header to
/// `renderEngineHealth`'s header. The F253 block is one contiguous run
/// (renderDupes → dupesCard → dupeAction), and the depth pass adds its
/// helpers inside it, so everything the dupes view renders and wires —
/// header, cards, actions, scan-now flow, dismissal — lives inside this
/// window. An empty window means the block moved files: point this guard at
/// the code that builds the dupes view.
fn dupes_region(shell: &str) -> String {
    const START: &str = "async function renderDupes(){";
    const END: &str = "\nfunction renderEngineHealth(){";
    let Some(at) = shell.find(START) else {
        return String::new();
    };
    let rest = &shell[at..];
    let Some(end) = rest.find(END) else {
        return String::new();
    };
    rest[..=end].to_owned()
}

/// The source windows of every TOP-LEVEL Rust fn whose header contains
/// `needle` (case-insensitive), from the header line to the column-0 `}` that
/// closes the item. Only column-0 `fn`/`async fn`/`pub…fn` lines count, so
/// indented test-module fns and method calls never match. Empty when no such
/// fn exists — the RED signal for a handler that does not exist yet.
fn fn_windows_named(src: &str, needle: &str) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    let needle = needle.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let is_fn_header = !line.starts_with(char::is_whitespace)
            && (line.starts_with("fn ")
                || line.starts_with("async fn ")
                || line.starts_with("pub"))
            && line.contains("fn ");
        if is_fn_header && line.to_ascii_lowercase().contains(&needle) {
            let mut window = String::from(line);
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim_end() != "}" {
                window.push('\n');
                window.push_str(lines[j]);
                j += 1;
            }
            if j < lines.len() {
                window.push_str("\n}");
            }
            out.push(window);
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// True when SOME occurrence of `anchor` in the normalized source has every
/// needle within `within` chars AFTER it — the scan-now / dismissal flow is a
/// short run of statements (spinner → fetch → re-render), so the flow's
/// tokens sit within a few hundred normalized chars of the flow's anchor
/// token, whichever occurrence of it (button label or fn header) starts the
/// flow. A compound flow factored into distant helpers means moving the guard
/// with the mechanism (the `preflight_f239_tdd.rs` convention).
fn any_span_has(hay: &str, anchor: &str, within: usize, needles: &[&str]) -> bool {
    let mut from = 0;
    while let Some(rel) = hay[from..].find(anchor) {
        let start = from + rel;
        // norm keeps multi-byte punctuation (· — ─), so byte offsets must be
        // snapped to char boundaries before slicing.
        let end = (start + anchor.len() + within).min(hay.len());
        let end = (start..=end)
            .rev()
            .find(|i| hay.is_char_boundary(*i))
            .unwrap_or(end);
        let window = &hay[start..end];
        if needles.iter().all(|n| window.contains(n)) {
            return true;
        }
        from = start + anchor.len();
    }
    false
}

/// True when SOME occurrence of `token` has `anchor` within `within` chars
/// BEFORE it — for "the header renders the stamp through relTime()" and "the
/// payload reads the stamp off the persisted workspace doc", where the
/// mechanism idiomatically precedes the AC-named token.
fn preceded_within(hay: &str, token: &str, within: usize, anchor: &str) -> bool {
    let mut from = 0;
    while let Some(rel) = hay[from..].find(token) {
        let start = from + rel;
        // norm keeps multi-byte punctuation (· — ─), so the byte offset must
        // be snapped forward to a char boundary before slicing.
        let lo = start.saturating_sub(within);
        let lo = (lo..=start)
            .find(|i| hay.is_char_boundary(*i))
            .unwrap_or(start);
        let window = &hay[lo..start];
        if window.contains(anchor) {
            return true;
        }
        from = start + token.len();
    }
    false
}

/// Either order of the two tokens within `within` chars — the header renders
/// `relTime(data.scannedAt)` (helper first) or `data.scannedAt` piped through
/// a relative-time call (stamp first); both satisfy "shows last-scan
/// relative time".
fn near(hay: &str, token: &str, within: usize, other: &str) -> bool {
    preceded_within(hay, token, within, other) || any_span_has(hay, token, within, &[other])
}

/// Every Playwright spec under `e2e/specs` with its repo-relative path,
/// sorted for deterministic failure messages (the
/// `live_repro_link_f247_tdd.rs` loader, verbatim).
fn spec_sources() -> Vec<(String, String)> {
    let dir = repo_root().join("e2e/specs");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("list {}: {e}", dir.display()))
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?.to_owned();
            (name.ends_with(".spec.ts")).then(|| {
                let rel = format!("e2e/specs/{name}");
                let src = std::fs::read_to_string(&p).ok()?;
                Some((rel, src))
            })?
        })
        .collect();
    out.sort();
    out
}

// --- premise: the scan the button re-runs is the REAL pure core -------------

/// The Scan now action's whole job is to re-execute the radar's scan. That
/// scan is the pure, shipped decision core
/// `use_cases::duplicate_radar::find_cross_project_duplicates` — the premise
/// pins that the core the re-run will invoke still behaves over real
/// snapshots (two projects filing the same ask collapse into one entry), and
/// that the radar's HTTP adapter still drives it through that core (no
/// second, diverging scan implementation snuck into the adapter). GREEN
/// today — a regression guard the depth pass must not break, and the anchor
/// that keeps "re-runs the scan" bound to real data the codebase has.
#[test]
fn premise_the_pure_scan_core_the_rerun_reexecutes_is_intact() {
    let snaps = vec![
        TicketSnapshot {
            project_id: "p1".to_owned(),
            project_name: "Alpha".to_owned(),
            ticket_id: "T-9".to_owned(),
            title: "Fix flaky login".to_owned(),
            scope: "session tokens expire mid-run".to_owned(),
            service_tag: None,
        },
        TicketSnapshot {
            project_id: "p2".to_owned(),
            project_name: "Beta".to_owned(),
            ticket_id: "T-2".to_owned(),
            title: "fix flaky login".to_owned(),
            scope: "login drops after every refresh".to_owned(),
            service_tag: None,
        },
    ];
    let out = find_cross_project_duplicates(&snaps, &[]);
    assert_eq!(out.len(), 1, "one cross-project ask, one radar entry");
    let server = read("crates/presentation/src/server/duplicate_radar.rs");
    assert!(
        server.contains("find_cross_project_duplicates"),
        "the radar's HTTP adapter stopped driving the pure scan core — the \
         Scan now re-run would re-run the wrong thing"
    );
}

// --- AC1 --------------------------------------------------------------------

/// AC1: "Dupes view header shows last-scan relative time" — the dupes view
/// must render the last scan's instant through the shipped relative-time
/// helper `relTime()` (the People view's "2h ago" idiom), fed by the
/// AC-named stamp. RED: the header shows only the pair count; `relTime(`
/// never appears in the dupes region.
#[test]
fn ac1_the_dupes_header_shows_the_last_scan_relative_time() {
    let shell = read("crates/presentation/src/web/js/shell.js");
    let region = dupes_region(&shell);
    assert!(
        !region.is_empty(),
        "the dupes view block moved out of web/js/shell.js — point this \
         guard at the code that builds the dupes view"
    );
    let n = norm(&region);
    assert!(
        n.contains("scannedat"),
        "the dupes view never reads the last-scan stamp (normalized \
         `scannedat`: the AC's `scanned_at`, wire-idiomatic `scannedAt`) — \
         the header has no last-scan time to show"
    );
    assert!(
        near(&n, "scannedat", 160, "reltime("),
        "the dupes header must render the last-scan stamp through the \
         shipped relative-time helper `relTime(...)` (within 160 normalized \
         chars of the stamp read) so the header shows '2h ago'-style time — \
         found region: {region}"
    );
}

/// AC1: "…and the scan cadence" — the header must also show the cadence the
/// radar scans on. RED: no cadence token exists in the dupes region (the
/// radar has no cadence at all yet — see the THE CADENCE naming note: the
/// guard pins presence, the SA owns the value).
#[test]
fn ac1_the_dupes_header_shows_the_scan_cadence() {
    let shell = read("crates/presentation/src/web/js/shell.js");
    let region = dupes_region(&shell);
    assert!(
        !region.is_empty(),
        "the dupes view block moved out of web/js/shell.js — point this \
         guard at the code that builds the dupes view"
    );
    assert!(
        norm(&region).contains("cadence"),
        "the dupes header never mentions the scan cadence — AC1's header \
         must show the cadence alongside the last-scan time — found region: \
         {region}"
    );
}

/// AC1: "a Scan now button (admin-gated)" — the view must offer a control
/// named by the AC's own words ("Scan now", normalized: `scanNow`, "Scan
/// now", "scan_now" all satisfy), and it must render ONLY for hub admins —
/// the shipped client gate `isHubAdmin()` guarding the control's render. RED:
/// no Scan now control exists and `isHubAdmin` never appears in the dupes
/// region.
#[test]
fn ac1_a_scan_now_button_is_rendered_only_for_hub_admins() {
    let shell = read("crates/presentation/src/web/js/shell.js");
    let region = dupes_region(&shell);
    assert!(
        !region.is_empty(),
        "the dupes view block moved out of web/js/shell.js — point this \
         guard at the code that builds the dupes view"
    );
    let n = norm(&region);
    assert!(
        n.contains("scannow"),
        "the dupes view offers no Scan now control (normalized `scannow`) — \
         found region: {region}"
    );
    assert!(
        preceded_within(&n, "scannow", 400, "ishubadmin"),
        "the Scan now control must be admin-gated in the view — rendered \
         only when the caller is a hub admin via the shipped `isHubAdmin()` \
         check (within 400 normalized chars before the control) — found \
         region: {region}"
    );
}

/// AC1: "…re-runs the scan and re-renders with a spinner state" — clicking
/// Scan now issues a NEW scan request to the radar (a `fetch`), shows the
/// house spinner idiom (`att-spin`) while it runs, and re-renders the radar
/// (`renderDupes()`) when it resolves. RED: no scan-now flow exists — no
/// fetch, no spinner, no re-render near any scan-now token.
#[test]
fn ac1_the_scan_rerun_rerenders_with_a_spinner_state() {
    let shell = read("crates/presentation/src/web/js/shell.js");
    let region = dupes_region(&shell);
    assert!(
        !region.is_empty(),
        "the dupes view block moved out of web/js/shell.js — point this \
         guard at the code that builds the dupes view"
    );
    let n = norm(&region);
    assert!(
        n.contains("scannow"),
        "the dupes view offers no Scan now control — no re-run flow to \
         render — found region: {region}"
    );
    assert!(
        any_span_has(&n, "scannow", 600, &["fetch", "attspin"]),
        "the Scan now flow must show the house spinner (`att-spin`, the \
         `ti-loader-2 att-spin` idiom) while it re-runs the scan (a `fetch` \
         to the radar) — within 600 normalized chars of the flow's anchor \
         token — found region: {region}"
    );
    assert!(
        any_span_has(&n, "scannow", 900, &["renderdupes("]),
        "the Scan now flow must RE-RENDER the radar (`renderDupes()`) once \
         the re-run resolves — within 900 normalized chars of the flow's \
         anchor token — found region: {region}"
    );
}

/// AC1: "(admin-gated)" — the gate must hold where the power is: the radar
/// adapter must expose the re-run as its own scan action, and that handler
/// must authorize through the house gate `gate_principal` (the same
/// mechanism the pair-verdict endpoint uses) — hiding the button client-side
/// alone would leave the re-run drivable by anyone. RED: the adapter has no
/// scan handler at all.
#[test]
fn ac1_the_scan_rerun_exists_server_side_and_is_admin_gated() {
    let src = read("crates/presentation/src/server/duplicate_radar.rs");
    let scans = fn_windows_named(&src, "scan");
    assert!(
        !scans.is_empty(),
        "the radar's HTTP adapter exposes no scan handler — there is no \
         server-side re-run for the Scan now button to drive"
    );
    assert!(
        scans.iter().any(|w| w.contains("gate_principal")),
        "the scan re-run handler must gate through the house authorization \
         gate `gate_principal` (an `AuthRole` predicate — which one is the \
         implementer's call) so the re-run is admin-gated server-side, not \
         only hidden client-side — found handler(s): {scans:?}"
    );
}

// --- AC2 --------------------------------------------------------------------

/// AC2: "Dismissing a duplicate pair …" — the view must offer a dismissal
/// action on a reported pair. Per the THE DISMISS naming note this cannot be
/// the reject verdict (`Rejected` is terminal in the domain transition
/// table), so it is a non-destructive radar action matched normalized. RED:
/// the word "dismiss" never appears in the dupes region.
#[test]
fn ac2_dismissing_a_duplicate_pair_is_a_first_class_action() {
    let shell = read("crates/presentation/src/web/js/shell.js");
    let region = dupes_region(&shell);
    assert!(
        !region.is_empty(),
        "the dupes view block moved out of web/js/shell.js — point this \
         guard at the code that builds the dupes view"
    );
    assert!(
        norm(&region).contains("dismiss"),
        "the dupes view offers no way to dismiss a duplicate pair (AC2's \
         verb, normalized) — found region: {region}"
    );
}

/// AC2: "…shows a 10s undo toast that restores it" — the dismissal must
/// surface a toast (the house `toast` mechanism / toasts container) carrying
/// an Undo control, living 10 seconds (`10000`, normalized so `10_000` also
/// matches — the shipped `toast()` hardcodes 3000 ms and supports no action,
/// so the toast itself must gain both). RED: nothing in the dismissal flow
/// exists yet.
#[test]
fn ac2_the_dismissal_shows_a_ten_second_undo_toast() {
    let shell = read("crates/presentation/src/web/js/shell.js");
    let region = dupes_region(&shell);
    assert!(
        !region.is_empty(),
        "the dupes view block moved out of web/js/shell.js — point this \
         guard at the code that builds the dupes view"
    );
    let n = norm(&region);
    assert!(
        n.contains("dismiss"),
        "no dismissal action exists — there is no toast to pin — found \
         region: {region}"
    );
    assert!(
        any_span_has(&n, "dismiss", 700, &["toast", "undo", "10000"]),
        "the dismissal must show a toast carrying an Undo control with a \
         10-second window (`10000` ms, normalized so `10_000` matches) — \
         within 700 normalized chars of the dismissal flow — found region: \
         {region}"
    );
}

/// AC2: "…that restores it" — the Undo control must put the dismissed pair
/// back: the undo path re-renders the radar with the pair restored (the
/// shipped `renderDupes()` re-run from inside the undo flow). RED: no undo
/// exists in the dupes flow.
#[test]
fn ac2_undo_restores_the_dismissed_pair() {
    let shell = read("crates/presentation/src/web/js/shell.js");
    let region = dupes_region(&shell);
    assert!(
        !region.is_empty(),
        "the dupes view block moved out of web/js/shell.js — point this \
         guard at the code that builds the dupes view"
    );
    let n = norm(&region);
    assert!(
        n.contains("undo"),
        "the dismissal flow has no Undo control — nothing can restore the \
         pair — found region: {region}"
    );
    assert!(
        any_span_has(&n, "undo", 700, &["renderdupes("]),
        "the Undo control must restore the dismissed pair by re-rendering \
         the radar (`renderDupes()`) with it back — within 700 normalized \
         chars of the undo flow — found region: {region}"
    );
}

// --- AC3 --------------------------------------------------------------------

/// AC3: "scanned_at is persisted server-side" — the stamp's home is
/// `WorkspaceDoc` (the hub doc that already persists the radar's
/// `dupe_allowlist`, saved through `Ws::save` and restored through
/// `Ws::load` — THE hub-restart boundary). The field must be declared
/// serde-defaulted: the additive-migration idiom every field on that doc
/// uses, so a workspace doc written before this field loads clean after a
/// hub restart and the saved stamp round-trips by construction. RED:
/// `WorkspaceDoc` declares no `scanned_at` (grep across crates/ finds none).
/// The type is `pub(super)` to the presentation crate, so this guard reads
/// the struct's source — the `live_repro_url_f246_tdd.rs` struct-window
/// precedent for exactly this situation.
#[test]
fn ac3_the_workspace_doc_declares_a_defaulted_scanned_at_field() {
    let hub = read("crates/presentation/src/server/hub_docs.rs");
    let doc = window_of(&hub, "struct WorkspaceDoc", "\n}");
    assert!(
        !doc.is_empty(),
        "WorkspaceDoc moved out of crates/presentation/src/server/hub_docs.rs \
         — point this guard at the persisted hub workspace doc"
    );
    let f = flat(doc);
    assert!(
        f.contains("scanned_at"),
        "the persisted hub workspace doc declares no `scanned_at` field — \
         AC3's stamp has nowhere to persist — found struct: {doc}"
    );
    assert!(
        preceded_within(&f, "scanned_at", 40, "#[serde(default)]"),
        "the `scanned_at` field must be `#[serde(default)]` (within 40 flat \
         chars before it — its own attribute, not a neighbour's) so a \
         workspace doc that predates the stamp loads clean after a hub \
         restart instead of failing the load — found struct: {doc}"
    );
}

/// AC3: "scanned_at is persisted server-side …" — a scan must WRITE the
/// stamp: the scan handler sets `scanned_at` to the RFC3339 instant
/// (`now_rfc3339()`, the idiom `allow_pair` uses for its `at` field) and
/// saves the workspace doc (`app.workspace.save()`), so the stamp reaches
/// the persisted doc — not an in-memory value that dies with the process.
/// RED: no scan handler exists and nothing writes `scanned_at`.
#[test]
fn ac3_a_scan_stamps_and_persists_scanned_at_server_side() {
    let src = read("crates/presentation/src/server/duplicate_radar.rs");
    let scans = fn_windows_named(&src, "scan");
    assert!(
        !scans.is_empty(),
        "the radar's HTTP adapter exposes no scan handler — nothing runs to \
         stamp `scanned_at`"
    );
    assert!(
        scans.iter().any(|w| {
            let w = flat(w);
            w.contains("scanned_at") && w.contains("now_rfc3339") && w.contains("workspace.save")
        }),
        "the scan handler must stamp `scanned_at` with `now_rfc3339()` AND \
         persist it through the workspace doc's save (`app.workspace.save()`) \
         so the stamp survives the process — found handler(s): {scans:?}"
    );
}

/// AC3: "…and survives hub restart" — restart survival is only real if the
/// SERVED stamp comes back FROM the persisted doc: `duplicates_ep` must read
/// `scanned_at` off the workspace doc it loads (the same doc `Ws::load`
/// restores from disk/KV at boot), so after a restart the header shows the
/// persisted instant rather than a blank or a request-time fabrication. RED:
/// the GET response carries only `crossProjectDuplicates` — no stamp is
/// served at all.
#[test]
fn ac3_the_radar_payload_serves_the_stamp_from_the_persisted_doc() {
    let src = read("crates/presentation/src/server/duplicate_radar.rs");
    let ep = window_of(&src, "async fn duplicates_ep", "\n}");
    assert!(
        !ep.is_empty(),
        "duplicates_ep moved or was renamed — point this guard at the \
         handler serving GET /api/workspace/duplicates"
    );
    let f = flat(ep);
    assert!(
        f.contains("scanned_at"),
        "GET /api/workspace/duplicates serves no `scanned_at` — the header's \
         last-scan time has no server source"
    );
    assert!(
        preceded_within(&f, "scanned_at", 400, "workspace"),
        "the served `scanned_at` must be read off the persisted workspace \
         doc (the state `Ws::load` restores after a hub restart — the doc \
         read within 400 flat chars before the stamp), never minted fresh at \
         request time — found handler: {ep}"
    );
}

// --- AC4 --------------------------------------------------------------------

/// AC4: "Golden screenshots updated; console gate clean" — a run can only
/// demonstrate that over coverage: a spec that exercises the depth pass's
/// changed visuals — the Scan now flow (with its spinner state), the
/// dismissal and its undo toast — and pins them with `toHaveScreenshot`
/// goldens. RED: `e2e/specs/dupes.spec.ts` covers only the F253 surface; no
/// spec mentions the scan-now flow, so nothing pins the new goldens. The
/// golden's actual regeneration is enforced by the run itself — a stale
/// golden fails `toHaveScreenshot` until refreshed.
#[test]
fn ac4_an_e2e_spec_covers_the_scan_now_and_undo_toast_with_updated_goldens() {
    let specs = spec_sources();
    assert!(
        !specs.is_empty(),
        "no e2e specs found under e2e/specs — the acceptance suite itself is \
         missing"
    );
    let names: Vec<String> = specs.iter().map(|(rel, _)| rel.clone()).collect();
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| norm(src).contains("scannow"))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the dupes Scan now flow — the `playwright test` \
         run of AC4 cannot demonstrate the updated goldens without it; \
         expected one of {names:?} to cover it (updating \
         e2e/specs/dupes.spec.ts counts)"
    );
    for (rel, src) in &covering {
        let n = norm(src);
        assert!(
            n.contains("dismiss") && n.contains("undo"),
            "{rel} covers Scan now but never exercises the dismissal and its \
             undo toast — AC2's changed visuals are only pinned if the spec \
             drives them"
        );
        assert!(
            n.contains("spin"),
            "{rel} covers Scan now but never asserts the spinner state the \
             re-run renders — AC1's in-flight visual is only pinned if the \
             spec asserts it"
        );
        assert!(
            src.contains("toHaveScreenshot"),
            "{rel} covers the dupes depth pass but pins no golden — AC4's \
             'updated golden screenshot' needs the changed dupes visuals \
             locked to a refreshed baseline"
        );
    }
}

/// AC4: "…console gate clean" — the house mechanism is the console-error
/// gate (`armConsoleGate` + `assertNoConsoleErrors` from
/// `e2e/specs/helpers.mjs`), armed by every console-clean spec. A dupes
/// render that throws on open or logs an error is exactly the CXA-F233
/// dead-view class this gate kills. RED: no spec covers the scan-now flow,
/// so there is no console-error gate armed over the new behaviour yet.
#[test]
fn ac4_the_dupes_e2e_spec_arms_the_console_error_gate() {
    let specs = spec_sources();
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| norm(src).contains("scannow"))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the dupes Scan now flow — there is no \
         console-error gate to arm for the depth pass yet"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("armConsoleGate") && src.contains("assertNoConsoleErrors"),
            "{rel} must arm the console-error gate (armConsoleGate + \
             assertNoConsoleErrors) so 'zero console errors' is actually \
             asserted for the depth pass's dupes renders"
        );
    }
}
