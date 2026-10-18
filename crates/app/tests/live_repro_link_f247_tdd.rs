//! CXA-F247 — One-click live reproduction link on the verify-gate decision
//! surface. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "'kind':'verify' inbox cards render a working Open-live-preview anchor
//!    linking to their repro_url when present, and show no broken element
//!    when repro_url is absent"
//! 2. "Clicking the link opens https?://... correctly target=_blank
//!    rel=noopener; scheme/host are never XSS-injected unsanitized"
//! 3. "'Send back' interaction remains functional from within flow surfaced
//!    around the same reproduction link"
//! 4. "(cd e2e && npx playwright test) passes with updated golden screenshot
//!    and zero console errors"
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the bytes of the view
//! source the hub actually serves — the verify branch of the inbox render
//! loop (`web/js/inbox.js`) and the send-back flow it wires — plus the e2e
//! spec sources the AC4 run executes, plus the real application-layer URL
//! builder the payloads use (`repro_url::compute_live_repro_url`). The same
//! no-harness discipline as `reproduce_url_f244_tdd.rs` and
//! `verify_live_link_f245_tdd.rs`: no fake HTTP server, no host harness, no
//! network port, no invented identifiers. Every failing assertion below
//! fails only because CXA-F247's behaviour is missing; if an assertion's
//! mechanism moves during implementation, move the guard with it (the
//! `preflight_f239_tdd.rs` convention).
//!
//! TWO NAMING DECISIONS, made explicit (both follow the house rule that the
//! sub-ticket's AC text governs — the precedent set in this file family):
//!
//! * The FIELD. AC1 says the anchor links "their repro_url". The wire field
//!   the hub ships is `reproduce_url` — CXA-F244's governing AC chose that
//!   name (its header records the parent CXA-F242 design calling the same
//!   field `repro_url`), and the server payload (`server/inbox.rs`,
//!   `server/work.rs`) plus the F244/F245 guard suites all pin it. A view
//!   reading a field the server never sends would be tests-green and
//!   app-broken, and a wire rename here would break three guard suites whose
//!   ACs governed. These guards therefore bind AC1's behaviour to the SHIPPED
//!   field name: `repro_url` is read as the documented alias for the same
//!   reproduction-URL data. If the SA intends a real wire rename, that is a
//!   cross-stack change that must move the F244/F245 guards in the same PR —
//!   flagged here, not fabricated.
//!
//! * The LABEL. AC1 names the control "Open-live-preview". The hyphenated
//!   compound is normalized to a case/separator-insensitive match, so
//!   "Open live preview", "Open-live-preview" and "OPEN LIVE PREVIEW" all
//!   satisfy it. The F245-shipped label "Open live instance" does NOT match
//!   (and fails these tests today, together with the button-vs-anchor gap),
//!   and the F247 design mockup's "LIVE REPRO" pill is a design detail — the
//!   AC text governs, as in CXA-F244/CXA-F245.
//!
//! Red today, and why:
//!   * AC1 — the verify branch renders a BUTTON (`ibtn("Open live
//!     instance", window.open(...))`), not an anchor; no "Open live preview"
//!     control exists.
//!   * AC2 — the click path is `window.open(...)`, which cannot carry
//!     `rel="noopener"`; and the URL reaches the click handler through
//!     `esc()` only (which escapes `& < >` and nothing else) with NO scheme
//!     whitelist — a `javascript:` or `data:` value, or a protocol-relative
//!     `//host`, would ride the click unsanitized.
//!   * AC4 — no spec under `e2e/specs` covers the Open-live-preview anchor
//!     (the existing `verify-live-link.spec.ts` covers the F245 button).
//!
//! Green guards, kept green on purpose: the presence axis (the field is
//! consulted before any control markup, so an absent/null `reproduce_url`
//! renders no broken element) and the Send back flow (AC3 is a regression
//! criterion — the anchor's arrival must not break it).
//!
//! AC → test map:
//! - premise: [`the_url_the_anchor_links_is_a_json_string_or_null`]
//! - AC1: [`ac1_the_verify_card_renders_a_working_open_live_preview_anchor`],
//!   [`ac1_the_anchor_shows_no_broken_element_when_repro_url_is_absent`]
//! - AC2: [`ac2_the_anchor_opens_the_url_with_target_blank_and_noopener`],
//!   [`ac2_the_scheme_and_host_are_never_injected_unsanitized`]
//! - AC3: [`ac3_send_back_remains_functional_in_the_flow_around_the_repro_link`]
//! - AC4: [`ac4_an_e2e_spec_covers_the_open_live_preview_anchor_with_an_updated_golden`],
//!   [`ac4_the_verify_card_e2e_spec_arms_the_console_error_gate`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::repro_url::compute_live_repro_url;

/// The payload field the verify card renders — shipped under this name by
/// CXA-F244 (see the naming note above for the AC's `repro_url` alias).
const FIELD: &str = "it.reproduce_url";

/// The control name, word-for-word from AC1's "Open-live-preview" — matched
/// normalized (see [`soft`]).
const CONTROL_LABEL: &str = "Open live preview";

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

/// The source with whitespace and hyphens removed and lowercased, so a guard
/// matches the AC's hyphenated "Open-live-preview" and the rendered
/// "Open live preview" alike, in any casing, without pinning the view code's
/// dense one-line formatting. Punctuation that carries structure (`?`, quotes,
/// parens) is kept — the presence guard below reads it.
fn soft(src: &str) -> String {
    src.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The source with ALL whitespace removed, case preserved — for checks on
/// exact source tokens (`target="_blank"`, `<a`, `href=`).
fn flat(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The source window of the inbox's verify card branch: from
/// `it.kind==="verify"` to the next branch of the render chain or the loop's
/// final `el.innerHTML=html;`. Everything that branch renders lives inside
/// this window; a control built anywhere else is a different surface than
/// the ACs name. (The `verify_live_link_f245_tdd.rs` helper, verbatim.)
fn card_branch_window(src: &str, kind: &str) -> String {
    let needle = format!("it.kind===\"{kind}\"");
    let Some(at) = src.find(&needle) else {
        return String::new();
    };
    let after = &src[at + needle.len()..];
    let next_branch = after.find("it.kind===\"");
    let loop_end = after.find("el.innerHTML=html;");
    let end = match (next_branch, loop_end) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) | (None, Some(a)) => a,
        (None, None) => after.len(),
    };
    format!("{}{}", needle, &after[..end])
}

/// The source window of one view function: from its header to the next
/// top-level (`async`)? `function` declaration.
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

/// True when `field` is referenced BEFORE the `label` occurrence in the
/// softened window with a `?` between them — the view code's conditional-
/// rendering idiom (`it.reproduce_url?ibtn(...):""`), i.e. the control renders
/// only when the field is present and the ternary's empty false arm is what
/// shows no broken element. A compound guard (`it.reproduce_url&&…?`) still
/// satisfies it; an unconditional render or a field never consulted does not.
/// (The `verify_live_link_f245_tdd.rs` mechanism, over [`soft`] so the
/// normalized label matches.)
fn presence_guarded(hay_soft: &str, field: &str, label_soft: &str) -> bool {
    let Some(label_at) = hay_soft.find(label_soft) else {
        return false;
    };
    let Some(field_at) = hay_soft[..label_at].rfind(field) else {
        return false;
    };
    hay_soft[field_at..label_at].contains('?')
}

/// Every Playwright spec under `e2e/specs` with its repo-relative path,
/// sorted for deterministic failure messages (the `e2e_acceptance_gate.rs`
/// loader, as used by `verify_live_link_f245_tdd.rs`).
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

// --- premise: the data the anchor links exists (CXA-F244 shipped) -----------

/// The anchor links real data, not a fixture invented for this ticket: the
/// verify payload's URL field serializes to a JSON string when the deploy
/// port resolves and JSON null otherwise — exactly the present/absent axis
/// AC1's show/hide logic keys on (a JS falsy `null`, never `undefined`).
#[test]
fn the_url_the_anchor_links_is_a_json_string_or_null() {
    let present = serde_json::json!(compute_live_repro_url(Some(4517)));
    assert_eq!(
        present,
        serde_json::json!("http://127.0.0.1:4517/"),
        "a resolvable deploy serializes the URL string the anchor must link"
    );
    let absent = serde_json::json!(compute_live_repro_url(None));
    assert_eq!(
        absent,
        serde_json::json!(null),
        "no host_port serializes JSON null — the falsy the hide arm keys on"
    );
}

// --- AC1 --------------------------------------------------------------------

/// AC1: "'kind':'verify' inbox cards render a working Open-live-preview
/// anchor linking to their repro_url when present" — the verify branch must
/// render an ANCHOR (`<a ... href=...>`), named "Open live preview" (AC1's
/// own words, normalized), whose target is the card's own reproduction-URL
/// field. RED: the branch renders a `window.open` BUTTON labelled
/// "Open live instance" — no `<a`, no `href=`, no such control name.
#[test]
fn ac1_the_verify_card_renders_a_working_open_live_preview_anchor() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    let f = flat(&branch);
    assert!(
        f.contains("<a") && f.contains("href="),
        "the verify card's reproduction control must be an ANCHOR element \
         with an href (a working link), not the current `window.open` \
         button — found branch: {branch}"
    );
    assert!(
        soft(&branch).contains(&soft(CONTROL_LABEL)),
        "the verify card's anchor must be named `{CONTROL_LABEL}` (AC1's \
         'Open-live-preview', normalized) — found branch: {branch}"
    );
    assert!(
        f.contains(FIELD),
        "the anchor must link the card's own {FIELD} — found branch: {branch}"
    );
}

/// AC1: "...and show no broken element when repro_url is absent" — the
/// anchor's rendering must be GUARDED by the field's presence (the view
/// code's `field?…:""` idiom), so the absent case renders no link at all —
/// not a dead anchor, not an empty href. RED until the anchor exists, then a
/// standing guard: the F245 ternary already has the right shape, and it must
/// survive the control's replacement.
#[test]
fn ac1_the_anchor_shows_no_broken_element_when_repro_url_is_absent() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    assert!(
        presence_guarded(&soft(&branch), FIELD, &soft(CONTROL_LABEL)),
        "the `{CONTROL_LABEL}` anchor must be rendered conditionally on \
         {FIELD} presence (the `field?…:\"\"` card-action idiom, compound \
         guards allowed) so an absent/null url shows no broken element — \
         found branch: {branch}"
    );
}

// --- AC2 --------------------------------------------------------------------

/// AC2: "Clicking the link opens https?://... correctly target=_blank
/// rel=noopener" — the anchor itself must carry the open-in-new-tab and
/// opener-severing attributes; a `window.open` call cannot express `rel` and
/// does not satisfy the criterion. RED: the branch has neither attribute.
#[test]
fn ac2_the_anchor_opens_the_url_with_target_blank_and_noopener() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    let f = flat(&branch);
    assert!(
        f.contains("target=\"_blank\""),
        "the `{CONTROL_LABEL}` anchor must open the live instance in a new \
         tab via target=\"_blank\" — found branch: {branch}"
    );
    assert!(
        f.contains("rel=\"noopener"),
        "the `{CONTROL_LABEL}` anchor must carry rel=\"noopener\" (or \
         \"noopener noreferrer\") so the opened tab cannot reach back into \
         the dashboard — found branch: {branch}"
    );
}

/// AC2: "...scheme/host are never XSS-injected unsanitized" — before the
/// reproduction URL reaches any markup it must pass an explicit https?-only
/// scheme whitelist, so a `javascript:`/`data:` scheme (or a protocol-
/// relative `//host` inheriting the dashboard's scheme) can never ride the
/// href. RED: the branch hands the field to the click handler through `esc()`
/// alone, which escapes `& < >` and no scheme — the house linkify guard
/// (`/(https?:\/\/[^\s<]+)/g`, `web/js/chat.js`) is the idiom to match.
/// If the whitelist is centralized in a shared helper instead of inline,
/// move this guard with the mechanism (the `preflight_f239_tdd.rs`
/// convention).
#[test]
fn ac2_the_scheme_and_host_are_never_injected_unsanitized() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    let f = flat(&branch);
    let whitelisted = f.contains("https?:") || (f.contains("https://") && f.contains("http://"));
    assert!(
        whitelisted,
        "the verify branch must sanitize the reproduction URL against an \
         explicit https?-only scheme whitelist BEFORE it reaches the anchor \
         markup (rejecting javascript:/data: schemes and protocol-relative \
         //host forms) — found neither a `https?:` pattern nor both scheme \
         literals in the branch: {branch}"
    );
}

// --- AC3 --------------------------------------------------------------------

/// AC3: "'Send back' interaction remains functional from within flow surfaced
/// around the same reproduction link" — a regression criterion: adding the
/// anchor must not break the send-back decision that shares the card. Both
/// verify surfaces (inbox card and reviewer modal) wire the SAME
/// `inboxSendBack` flow, so the guard pins it end to end: the branch still
/// renders the Send back control for this card, and `inboxSendBack` still
/// carries its reason to the send-back endpoint. GREEN today — and it must
/// stay green once the anchor lands.
#[test]
fn ac3_send_back_remains_functional_in_the_flow_around_the_repro_link() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    assert!(
        branch.contains("ibtn(\"Send back\"") && branch.contains("inboxSendBack('"),
        "the verify card lost its Send back control — CXA-F247 adds the \
         reproduction anchor alongside it, never instead of it — found \
         branch: {branch}"
    );
    let flow = js_function_window(&inbox, "async function inboxSendBack(id,url){");
    assert!(
        !flow.is_empty(),
        "inboxSendBack moved out of web/js/inbox.js (or lost the `url` \
         parameter that carries the shown reproduction link into the \
         send-back dialog — plan step 2) — point this guard at the function \
         that carries the send-back flow"
    );
    let flow_flat = flat(flow);
    assert!(
        flow_flat.contains("send-back"),
        "the send-back flow no longer targets the /send-back endpoint — the \
         interaction AC3 protects is broken"
    );
    assert!(
        flow_flat.contains("reason"),
        "the send-back flow no longer carries the reviewer's reason — the \
         agents would redo work blind"
    );
}

// --- AC4 --------------------------------------------------------------------

/// AC4: "(cd e2e && npx playwright test) passes with updated golden
/// screenshot and zero console errors" — a run can only demonstrate that
/// over coverage: a spec that renders the Open-live-preview anchor, asserts
/// its new-tab/noopener click contract (AC2 at runtime), and pins the changed
/// visuals with a screenshot golden. RED: the only verify-link spec covers
/// the F245 button ("Open live instance"); none covers this ticket's anchor.
/// The golden's actual regeneration is enforced by the run itself — a stale
/// golden fails `toHaveScreenshot` until refreshed.
#[test]
fn ac4_an_e2e_spec_covers_the_open_live_preview_anchor_with_an_updated_golden() {
    let specs = spec_sources();
    assert!(
        !specs.is_empty(),
        "no e2e specs found under e2e/specs — the acceptance suite itself is \
         missing"
    );
    let names: Vec<String> = specs.iter().map(|(rel, _)| rel.clone()).collect();
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| soft(src).contains(&soft(CONTROL_LABEL)))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the verify card's `{CONTROL_LABEL}` anchor — the \
         `playwright test` run of AC4 cannot demonstrate the updated golden \
         or the click contract without it; expected one of {names:?} to \
         cover it (updating e2e/specs/verify-live-link.spec.ts counts)"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("noopener"),
            "{rel} covers the `{CONTROL_LABEL}` anchor but never asserts its \
             rel=noopener click contract — AC2's runtime behaviour is only \
             demonstrated if the spec asserts it"
        );
        assert!(
            src.contains("toHaveScreenshot"),
            "{rel} covers the `{CONTROL_LABEL}` anchor but pins no golden — \
             AC4's 'updated golden screenshot' needs the changed verify-card \
             visuals locked to a refreshed baseline"
        );
    }
}

/// AC4: "...and zero console errors" — the house mechanism is the
/// console-error gate (`armConsoleGate` + `assertNoConsoleErrors` from
/// `e2e/specs/helpers.mjs`), armed by every console-clean spec. A verify-card
/// render that throws on open or logs an error is exactly the CXA-F233
/// dead-view class this gate kills. RED: the covering spec does not exist
/// yet (see the sibling AC4 test).
#[test]
fn ac4_the_verify_card_e2e_spec_arms_the_console_error_gate() {
    let specs = spec_sources();
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| soft(src).contains(&soft(CONTROL_LABEL)))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the verify card's `{CONTROL_LABEL}` anchor — \
         there is no console-error gate to arm for it yet"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("armConsoleGate") && src.contains("assertNoConsoleErrors"),
            "{rel} must arm the console-error gate (armConsoleGate + \
             assertNoConsoleErrors) so 'zero console errors' is actually \
             asserted for the verify-card render"
        );
    }
}
