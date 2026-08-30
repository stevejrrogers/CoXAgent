//! CXA-F245 (CXA-F242-C) — Render the live reproduction link in every human
//! verification view. RED half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "An inbox card of kind=verify renders an 'Open live instance' control
//!    linking to its reproduce_url when present, opens it in a new tab, and
//!    hides entirely when reproduce_url is absent/null."
//! 2. "The ticket detail/modal surface used by reviewers also shows/hides the
//!    same live link driven by reproduce_url presence."
//! 3. "Existing non-verify inbox cards are unchanged visually/data-wise apart
//!    from adding available verify links."
//! 4. "cd e2e && npx playwright test passes with refreshed goldens where
//!    verify-card visuals changed; no console errors introduced."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the bytes of the view
//! sources the hub actually serves — the inbox card builder
//! (`web/js/inbox.js`) and the reviewer's ticket modal (`web/js/chat.js`,
//! `showTicket`) — plus the real application-layer URL builder the shipped
//! F244 payloads use (`repro_url::compute_live_repro_url`). The same no-harness
//! discipline as `reproduce_url_f244_tdd.rs` and `fleet_river_f233_gate.rs`:
//! no fake HTTP server, no host harness, no network port, no invented
//! identifiers. Every failing assertion below fails only because CXA-F245's
//! behaviour is missing; if an assertion's mechanism moves during
//! implementation, move the guard with it (the `preflight_f239_tdd.rs`
//! convention).
//!
//! The label pinned is the AC's own words — 'Open live instance'. Note for the
//! implementer: the committed design mockup
//! (`.coxagent/design/CXA-F245/inbox-verify-card.svg`) labels the same control
//! "Open live app" — the sub-ticket's AC text governs, as in CXA-F244.
//!
//! Red today, and why:
//!   * AC1 — the verify branch of `renderInbox` (`web/js/inbox.js`) renders
//!     only Evidence / Send back / Verified; the file never mentions
//!     `reproduce_url`, `window.open`, nor any presence guard for the link.
//!   * AC2 — `showTicket` (`web/js/chat.js`) renders Send back / Mark Verified
//!     for `status==="fixed"` but never references `t.reproduce_url`.
//!   * AC4 — no spec under `e2e/specs` exercises the verify card at all (the
//!     frozen e2e fixture seeds no verify-gate state: `workflow` is empty, no
//!     Fixed+evidence ticket, `deploy` unset), so the AC's "playwright passes
//!     with refreshed goldens where verify-card visuals changed" has no
//!     surface to pass on; the console-error gate has nothing armed for it.
//!
//! Green guards (fixture validity — house rule: every fixture must be
//! buildable from data the codebase actually has):
//!   * the field the views render rides on the SHIPPED F244 payloads: the
//!     inbox verify card (`server/inbox.rs`) and the detail injection
//!     (`server/work.rs`) already carry `reproduce_url` — deep shape pins live
//!     in `reproduce_url_f244_tdd.rs`, not duplicated here;
//!   * `compute_live_repro_url` serializes to a JSON string when resolvable
//!     and JSON null otherwise — exactly the present/absent axis the AC's
//!     show/hide logic keys on (a JS falsy `null`, never `undefined`);
//!   * every non-verify card branch keeps its existing action labels and the
//!     data fields its card visually renders.
//!
//! NOT ENCODED HERE — verified against the live app in the QA phase:
//!   * the actual `cd e2e && npx playwright test` run passing (AC4's run
//!     itself) and which exact goldens get refreshed — pinned instead by the
//!     executable invariant that a spec covering the verify card's live link
//!     EXISTS, carries a golden, and arms the console-error gate;
//!   * the visual styling of the control (the mockup's cyan emphasis is a
//!     design detail; the AC pins the control, the link target, the new tab
//!     and the show/hide axis).
//!
//! AC → test map:
//! - AC1: [`ac1_the_verify_card_renders_an_open_live_instance_control`],
//!   [`ac1_the_control_opens_the_live_instance_in_a_new_tab`],
//!   [`ac1_the_control_hides_entirely_when_reproduce_url_is_absent_or_null`],
//!   plus the green [`the_wire_field_the_views_render_is_present_or_null_json`]
//! - AC2: [`ac2_the_ticket_modal_shows_the_same_live_link`],
//!   [`ac2_the_modal_link_is_driven_by_reproduce_url_presence`]
//! - AC3: [`ac3_non_verify_cards_keep_their_existing_actions_and_data`],
//!   [`ac3_the_verify_link_is_added_only_to_verify_cards`]
//! - AC4: [`ac4_an_e2e_spec_covers_the_verify_card_live_link_with_a_golden`],
//!   [`ac4_the_verify_card_e2e_spec_arms_the_console_error_gate`]

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use coxagent_application::repro_url::compute_live_repro_url;

/// The control label, word-for-word from the AC text — the contract the views
/// must render (the design mockup's "Open live app" does not govern).
const CONTROL_LABEL: &str = "Open live instance";

/// The payload field both verification views render — named verbatim by the
/// ACs and already shipped by CXA-F244.
const FIELD: &str = "reproduce_url";

// --- repo-state scan helpers (the reproduce_url_f244_tdd.rs pattern) --------

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

fn count_occurrences(hay: &str, needle: &str) -> usize {
    hay.match_indices(needle).count()
}

/// The source window of ONE card branch of the inbox's render loop: from
/// `it.kind==="kind"` to the next branch of the chain or the loop's final
/// `el.innerHTML=html;`. Everything that branch renders lives inside this
/// window; a card built anywhere else is a different surface than the ACs name.
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
/// top-level (`async`)? `function` declaration. The modal's whole render lives
/// in this window.
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
/// flattened window with a `?` between them — the view code's conditional-
/// rendering idiom (`it.url?ibtn("Open PR",…):""`, `${t.status==="fixed"?…}`),
/// i.e. the control renders only when the field is present and the ternary's
/// empty false arm is what hides it entirely. A compound guard
/// (`t.reproduce_url&&…?`) still satisfies it; an unconditional render or a
/// field never consulted does not. If the mechanism moves, move the guard.
fn presence_guarded(hay_flat: &str, field: &str, label_flat: &str) -> bool {
    let Some(label_at) = hay_flat.find(label_flat) else {
        return false;
    };
    let Some(field_at) = hay_flat[..label_at].rfind(field) else {
        return false;
    };
    hay_flat[field_at..label_at].contains('?')
}

/// The source window of the inbox's server-side verify card — the same window
/// `reproduce_url_f244_tdd.rs` pins — used here only to prove the field the
/// views render is really in the payload (green premise, not re-pinned deep).
fn server_verify_card_window(src: &str) -> String {
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

// --- green guards: the data the views render exists (CXA-F244 shipped) -------

/// The views' show/hide axis is the wire shape F244 already ships: a JSON
/// string when the deploy port is resolvable, JSON NULL otherwise — a JS
/// falsy, so the AC's "absent/null" collapses into one falsy check in the
/// view. If the payload ever serialized the absent case as `undefined` (field
/// omitted) or `""`, this pin catches the drift the ternaries would read
/// differently.
#[test]
fn the_wire_field_the_views_render_is_present_or_null_json() {
    let present = serde_json::json!(compute_live_repro_url(Some(4517)));
    assert_eq!(
        present,
        serde_json::json!("http://127.0.0.1:4517/"),
        "a resolvable deploy serializes the URL string the control must link"
    );
    let absent = serde_json::json!(compute_live_repro_url(None));
    assert_eq!(
        absent,
        serde_json::json!(null),
        "no host_port serializes JSON null — the falsy the view's hide arm keys on"
    );
}

/// Green premise: the inbox verify card and the ticket-detail payload the two
/// views read already carry the field (CXA-F244). If either payload loses the
/// field, the view controls would render from nothing — fail HERE with the
/// surface named, not silently in a screenshot.
#[test]
fn the_server_payloads_the_views_read_already_carry_the_field() {
    let inbox = read("crates/presentation/src/server/inbox.rs");
    let card = server_verify_card_window(&inbox);
    assert!(
        !card.is_empty(),
        "the inbox no longer builds a `kind: \"verify\"` card — the payload \
         surface moved; point this premise guard at the code that builds it"
    );
    assert!(
        card.contains(FIELD),
        "the inbox verify card payload lost `{FIELD}` — the view control would \
         render from nothing; card: {card}"
    );

    let work = read("crates/presentation/src/server/work.rs");
    let detail = js_function_window(&work, "pub(super) async fn ticket_detail_ep");
    assert!(
        detail.contains("pub(super) async fn ticket_detail_ep"),
        "ticket_detail_ep moved out of server/work.rs — point this premise \
         guard at the handler that builds the detail payload"
    );
    assert!(
        detail.contains(FIELD),
        "the ticket detail payload lost the `{FIELD}` injection — the modal \
         control would render from nothing"
    );
}

// --- AC1 ---------------------------------------------------------------------

/// AC1: "An inbox card of kind=verify renders an 'Open live instance' control
/// linking to its reproduce_url when present" — the verify branch of the inbox
/// render loop must render the control, and its link target must be the
/// card's OWN `reproduce_url` field. RED: the branch renders only
/// Evidence / Send back / Verified; `reproduce_url` appears nowhere in
/// `web/js/inbox.js`.
#[test]
fn ac1_the_verify_card_renders_an_open_live_instance_control() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    let f = flat(&branch);
    assert!(
        f.contains(&flat(CONTROL_LABEL)),
        "the verify inbox card must render an `{CONTROL_LABEL}` control — \
         found branch: {branch}"
    );
    assert!(
        f.contains(&flat("it.reproduce_url")),
        "the `{CONTROL_LABEL}` control must link the card's own \
         it.{FIELD} — found branch: {branch}"
    );
}

/// AC1: "...opens it in a new tab" — the live instance is a DIFFERENT origin
/// than the dashboard, and this codebase's new-tab pattern for exactly that
/// case on inbox cards is `window.open(...,'_blank')` (the `human_eyes`
/// "Open PR" card, the `pr_stuck` "Open on GitHub" card). RED: the verify
/// branch contains no window.open at all.
#[test]
fn ac1_the_control_opens_the_live_instance_in_a_new_tab() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    let f = flat(&branch);
    assert!(
        f.contains("window.open("),
        "the verify card's `{CONTROL_LABEL}` control must open the live \
         instance — found branch: {branch}"
    );
    assert!(
        f.contains("'_blank'"),
        "the `{CONTROL_LABEL}` control must open the live instance in a NEW \
         TAB (the `window.open(...,'_blank')` house pattern) — found branch: \
         {branch}"
    );
}

/// AC1: "...and hides entirely when reproduce_url is absent/null" — the
/// control's rendering must be GUARDED by the field's presence (the view
/// code's `field?…:""` card-action idiom, cf. the human_eyes card's
/// `it.url?`), so the absent case renders no link at all — not a dead button,
/// not an empty href. RED: the branch never consults `it.reproduce_url`.
#[test]
fn ac1_the_control_hides_entirely_when_reproduce_url_is_absent_or_null() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    assert!(
        presence_guarded(&flat(&branch), "it.reproduce_url", &flat(CONTROL_LABEL)),
        "the `{CONTROL_LABEL}` control must be rendered conditionally on \
         it.{FIELD} presence (the `field?…:\"\"` card-action idiom) so it \
         hides entirely when the field is absent/null — found branch: {branch}"
    );
}

// --- AC2 ---------------------------------------------------------------------

/// AC2: "The ticket detail/modal surface used by reviewers also shows/hides
/// the same live link driven by reproduce_url presence." The reviewer's
/// surface is `showTicket` (`web/js/chat.js`) — the modal the inbox's
/// "Evidence" button opens, which already renders the verify actions
/// (Send back / Mark Verified) for `status==="fixed"`. It must show the SAME
/// live link (the `{CONTROL_LABEL}` control over the detail payload's
/// `{FIELD}`). RED: `showTicket` never references `t.reproduce_url`.
#[test]
fn ac2_the_ticket_modal_shows_the_same_live_link() {
    let chat = read("crates/presentation/src/web/js/chat.js");
    let modal = js_function_window(&chat, "async function showTicket(id){");
    assert!(
        !modal.is_empty(),
        "showTicket moved out of web/js/chat.js — point this guard at the \
         function that renders the reviewer's ticket modal"
    );
    let f = flat(modal);
    assert!(
        f.contains(&flat(CONTROL_LABEL)),
        "the reviewer's ticket modal must show the same `{CONTROL_LABEL}` \
         live link the inbox card shows — found modal window: {modal}"
    );
    assert!(
        f.contains(&flat("t.reproduce_url")),
        "the modal's live link must be driven by the detail payload's \
         t.{FIELD} — found modal window: {modal}"
    );
}

/// AC2: "...shows/hides ... driven by reproduce_url presence" — the modal's
/// control must be guarded by the field's presence (the same
/// presence_guarded mechanism as AC1), so a ticket without a resolvable
/// deploy renders no live link. RED: the modal never consults the field.
#[test]
fn ac2_the_modal_link_is_driven_by_reproduce_url_presence() {
    let chat = read("crates/presentation/src/web/js/chat.js");
    let modal = js_function_window(&chat, "async function showTicket(id){");
    assert!(
        !modal.is_empty(),
        "showTicket moved out of web/js/chat.js — point this guard at the \
         function that renders the reviewer's ticket modal"
    );
    assert!(
        presence_guarded(&flat(modal), "t.reproduce_url", &flat(CONTROL_LABEL)),
        "the modal's `{CONTROL_LABEL}` link must be rendered conditionally on \
         t.{FIELD} presence so it hides when the field is absent/null — found \
         modal window: {modal}"
    );
}

// --- AC3 ---------------------------------------------------------------------

/// AC3: "Existing non-verify inbox cards are unchanged visually/data-wise
/// apart from adding available verify links." Green guard pinning what
/// "unchanged" means, branch by branch: every non-verify card keeps its
/// existing action buttons (the visual contract) and the data fields its card
/// renders (the data contract). A CXA-F245 edit that touches another card's
/// buttons or fields fails HERE. (Server-side card shapes are already pinned
/// by `reproduce_url_f244_tdd.rs`; this pins the view half.)
#[test]
fn ac3_non_verify_cards_keep_their_existing_actions_and_data() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let unchanged: &[(&str, &[&str])] = &[
        ("approve_ready", &["Review", "Reject", "Approve"]),
        ("cost_approve", &["Review", "Reject", "Approve spend", "estimate_usd"]),
        ("assigned", &["Return to agents", "status"]),
        ("question", &["Answer in Scrum", "asked_at", "deferred"]),
        ("auto_approved", &["Review", "Undo", "minutes_left"]),
        ("review_pr", &["Open review", "number"]),
        ("human_eyes", &["Open PR", "Dismiss", "Land it", "reason"]),
        ("reverted_work", &["Open ticket", "Dismiss", "Confirm revert", "sha"]),
        ("pr_stuck", &["Open on GitHub", "Review queue", "mergeable"]),
    ];
    for (kind, tokens) in unchanged {
        let branch = card_branch_window(&inbox, kind);
        assert!(
            !branch.is_empty(),
            "the inbox no longer builds a kind={kind} card — the render chain \
             moved; point this guard at the code that builds it"
        );
        for token in *tokens {
            assert!(
                branch.contains(token),
                "the existing {kind} inbox card lost `{token}` — CXA-F245 is \
                 additive to VERIFY cards only; branch: {branch}"
            );
        }
    }
    // The collapsed on-hold summary card is built before the render loop, not
    // in a `it.kind===` branch — pinned against the whole view source.
    assert!(
        inbox.contains("View on board"),
        "the collapsed on-hold inbox card lost `View on board` — CXA-F245 is \
         additive to VERIFY cards only"
    );
}

/// AC3: "...apart from adding available verify links" — the additive half:
/// every `reproduce_url` reference the inbox view gains must live inside the
/// VERIFY card's branch. A link control leaking into any other card's branch
/// changes that card and fails here. GREEN today (zero references anywhere —
/// AC1's red tests force them to appear), and the guard that keeps the change
/// scoped once they do.
#[test]
fn ac3_the_verify_link_is_added_only_to_verify_cards() {
    let inbox = read("crates/presentation/src/web/js/inbox.js");
    let branch = card_branch_window(&inbox, "verify");
    assert!(
        !branch.is_empty(),
        "the inbox no longer builds a kind=verify card — the verify surface \
         moved; point this guard at the code that builds it"
    );
    let total = count_occurrences(&inbox, FIELD);
    let in_verify_branch = count_occurrences(&branch, FIELD);
    assert_eq!(
        total, in_verify_branch,
        "every `{FIELD}` reference in the inbox view must belong to the verify \
         card's branch (non-verify cards stay unchanged) — {total} in file vs \
         {in_verify_branch} in the verify branch"
    );
}

// --- AC4 ---------------------------------------------------------------------

/// The suite AC4's criterion runs: `cd e2e && npx playwright test`.
const E2E_SPECS_DIR: &str = "e2e/specs";

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

/// AC4: "cd e2e && npx playwright test passes with refreshed goldens where
/// verify-card visuals changed" — a run can only demonstrate that over
/// coverage: a spec that renders the verify card's live link and pins it with
/// a screenshot golden. RED: no spec under `e2e/specs` mentions the control
/// (the verify card has no e2e surface at all — the frozen fixture seeds no
/// verify-gate state), so the criterion has nothing to pass on. Implementer
/// note: rendering the card in the suite also needs the e2e fixture (or a
/// route mock) to carry verify-gate state with a resolvable `host_port` — the
/// how is yours, the coverage requirement is the AC's.
#[test]
fn ac4_an_e2e_spec_covers_the_verify_card_live_link_with_a_golden() {
    let specs = spec_sources();
    assert!(
        !specs.is_empty(),
        "no e2e specs found under {E2E_SPECS_DIR} — the acceptance suite \
         itself is missing"
    );
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| src.contains(CONTROL_LABEL))
        .collect();
    let names: Vec<&str> = covering.iter().map(|(rel, _)| rel.as_str()).collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the verify card's `{CONTROL_LABEL}` live link — \
         the `playwright test` run of AC4 cannot demonstrate the changed \
         verify-card visuals without it; expected one of {names:?} to exist"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("toHaveScreenshot"),
            "{rel} covers the verify card's live link but pins no golden — \
             AC4's 'refreshed goldens where verify-card visuals changed' \
             needs the changed visuals locked to a baseline"
        );
    }
}

/// AC4: "...no console errors introduced" — the house mechanism is the
/// console-error gate (`armConsoleGate` + `assertNoConsoleErrors` from
/// `e2e/specs/helpers.mjs`), armed by every console-clean spec. The verify
/// card's spec must arm it too: a verify-card render that throws on open or
/// logs an error is exactly the CXA-F233 dead-view class this gate kills.
/// RED: the covering spec does not exist yet (see the sibling AC4 test).
#[test]
fn ac4_the_verify_card_e2e_spec_arms_the_console_error_gate() {
    let specs = spec_sources();
    let covering: Vec<(String, String)> = specs
        .into_iter()
        .filter(|(_, src)| src.contains(CONTROL_LABEL))
        .collect();
    assert!(
        !covering.is_empty(),
        "no e2e spec covers the verify card's `{CONTROL_LABEL}` live link — \
         there is no console-error gate to arm for it yet"
    );
    for (rel, src) in &covering {
        assert!(
            src.contains("armConsoleGate") && src.contains("assertNoConsoleErrors"),
            "{rel} must arm the console-error gate (armConsoleGate + \
             assertNoConsoleErrors) so 'no console errors introduced' is \
             actually asserted for the verify-card render"
        );
    }
}
