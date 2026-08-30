//! CXA-F250 — Idea-filing feasibility preview in the New-Ticket dialog: RED
//! half of the TDD pair.
//!
//! The ticket's acceptance criteria, verbatim:
//! 1. "From #ov-newticket open via openNewTicket(), entering a non-empty
//!    idea/description and clicking 'Check feasibility' renders within
//!    #ov-newticket a visible feasibility-preview panel showing one of
//!    Feasible / Needs clarification / Not feasible plus at least one
//!    sentence of rationale grounded in project context; clicking it
//!    performs no save (POST /tickets is never called)."
//! 2. "Clicking 'Check feasibility' with both title and description empty
//!    does not fire any network request: #nt-err shows an inline message
//!    such as 'Write the idea first' and no ticket is created."
//! 3. "Generating or viewing a feasibility preview has zero backlog side
//!    effects: closing/cancelling #ov-newticket afterwards leaves GET /state
//!    unchanged (no new ticket id appears) until an explicit Save is
//!    performed."
//! 4. "When the project has no AI engine configured, 'Check feasibility'
//!    fails gracefully with an inline #nt-err message containing '(needs a
//!    configured engine)', all typed form fields keep their entered values,
//!    and both Save buttons remain functional."
//! 5. "Any request failure or network error during preview shows an inline
//!    #nt-err message ('Network error.' etc.) without losing entered field
//!    values or throwing uncaught exceptions; cd e2e && npx playwright test
//!    passes with zero console errors."
//!
//! HOW THESE CRITERIA ARE ENCODED: pure functions over the bytes the hub
//! actually serves — the dialog markup (`web/index.html`) and the dialog
//! script (`web/js/shell.js`) — the same no-harness discipline as
//! `preflight_f239_tdd.rs` and `dashboard_file_pickers.rs`. No server, no
//! port, no invented types: every identifier below exists in production
//! today, so the suite compiles and each failing assertion fails only
//! because CXA-F250's behaviour is missing.
//!
//! DISCOVERY, NOT INVENTION: the feature does not exist yet, so the handler
//! behind 'Check feasibility' has no name to pin. The guards DISCOVER it the
//! way the browser does — find the button labelled 'Check feasibility' in
//! the #ov-newticket markup, read its inline `onclick`, and resolve that
//! function in the served script. Every dialog action in this codebase is
//! wired exactly that way (`teamAnalyze()`, `saveTicket(…)`,
//! `openNewTicket()` on inline `onclick` of `<button>` elements), and every
//! dialog async handler is self-contained — it issues its own `fetch`
//! (`teamAnalyze`, `saveTicket`). The guards pin that convention; if CXA-
//! F250 wires the click or its request some other way, move the guard with
//! the wiring.
//!
//! NOT ENCODED HERE — reported, not fabricated: the SEMANTIC half of AC1
//! ("at least one sentence of rationale grounded in project context") and
//! the engine-absence semantics of AC4 assert response data whose type
//! exists nowhere in the codebase — no feasibility endpoint, verdict enum or
//! rationale field is defined by any port, use case or OpenAPI entry today
//! (there is also no CXA-F250 design file in .coxagent/design/ to name one).
//! Pinning them would require inventing the response contract (forbidden:
//! no invented identifiers). What IS pinned: the three verdict labels the AC
//! names verbatim, a rationale render derived from the engine response (a
//! dynamic interpolation into the panel — a static verdict label alone
//! cannot be "a sentence of rationale"), and the failure-path strings the
//! ACs quote ('(needs a configured engine)', 'Network error.'). The runtime
//! halves — panel visibility in a real browser, GET /state unchanged, the
//! full `cd e2e && npx playwright test` suite with zero console errors —
//! are verified against the live app in the QA phase, exactly as
//! `preflight_f239_tdd.rs` defers its runtime halves.

/// The dashboard the router serves verbatim, carrying the #ov-newticket
/// dialog markup.
const DASHBOARD: &str = include_str!("../src/web/index.html");

/// The dialog script the dashboard loads — where every new-ticket dialog
/// function lives today (`openNewTicket`, `teamAnalyze`, `saveTicket`).
const SHELL: &str = include_str!("../src/web/js/shell.js");

/// The three verdict labels AC1 names, as displayed.
const VERDICTS: [&str; 3] = ["Feasible", "Needs clarification", "Not feasible"];

/// The inline-message fragments the ACs quote for #nt-err.
const EMPTY_IDEA_MSG: &str = "Write the idea first";
const NO_ENGINE_MSG: &str = "(needs a configured engine)";
const NETWORK_ERR_MSG: &str = "Network error.";

/// The button label ACs 1–5 are all about.
const BUTTON_LABEL: &str = "Check feasibility";

// ---------------------------------------------------------------------------
// Discovery helpers — resolve the feature the way the browser does.
// ---------------------------------------------------------------------------

/// The byte window of one overlay's markup, from its opening `<div class="ov"`
/// tag to the next overlay's — the dialog the ACs talk about, including the
/// backdrop-close `onclick` on the overlay tag itself.
fn overlay_window<'a>(src: &'a str, overlay_id: &str) -> &'a str {
    let Some(at) = src.find(&format!("id=\"{overlay_id}\"")) else {
        return "";
    };
    // Walk back to the tag opener so the overlay's own onclick is included.
    let start = src[..at].rfind('<').map_or(at, |p| p);
    let next = src[at + overlay_id.len()..]
        .find("id=\"ov-")
        .map_or(src.len(), |rel| at + overlay_id.len() + rel);
    &src[start..next]
}

/// The `<button …>` open tag containing `label` (buttons never nest, so the
/// last opener before the label is the enclosing one), or `None`.
fn dialog_button_for_label<'a>(src: &'a str, label: &str) -> Option<&'a str> {
    let at = src.find(label)?;
    let open = src[..at].rfind("<button")?;
    let end = open + src[open..].find('>')? + 1;
    let tag = &src[open..end];
    // The label must belong to this button, not to one that merely precedes it.
    let closes_before = src[open..at].matches("</button>").count();
    if closes_before > 0 {
        return None;
    }
    Some(tag)
}

/// The function name an `onclick="fn(…)"` attribute invokes.
fn onclick_handler(tag: &str) -> Option<&str> {
    let at = tag.find("onclick=\"")? + "onclick=\"".len();
    let rest = &tag[at..];
    let end = rest.find('"')?;
    let call = &rest[..end];
    let name_end = call.find('(')?;
    let name = call[..name_end].trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    Some(name)
}

/// One top-level function from a dashboard script: from its `function`/`async
/// function` header to the next top-level declaration. Scripts like shell.js
/// declare one function per line at column 0 and indent statements inside
/// bodies, so a newline at column 0 followed by a declaration keyword is a
/// reliable seam — the same discipline `health_gate.rs` applies to Rust fns.
fn js_fn<'a>(src: &'a str, name: &str) -> Option<&'a str> {
    let plain = format!("function {name}(");
    let awaited = format!("async function {name}(");
    let at = match (src.find(&awaited), src.find(&plain)) {
        (Some(a), Some(p)) => a.min(p),
        (Some(a), None) => a,
        (None, Some(p)) => p,
        (None, None) => return None,
    };
    let rest = &src[at..];
    let body = rest.find('{')?;
    let end = rest[body..]
        .find("\nfunction ")
        .into_iter()
        .chain(rest[body..].find("\nasync function "))
        .chain(rest[body..].find("\nconst "))
        .chain(rest[body..].find("\nlet "))
        .min()
        .map_or(rest.len(), |rel| body + rel);
    Some(&rest[..end])
}

/// Every element id the function looks up via `getElementById("…")`.
fn looked_up_ids(fn_src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = fn_src;
    while let Some(at) = rest.find("getElementById(\"") {
        let after = &rest[at + "getElementById(\"".len()..];
        let Some(end) = after.find('"') else {
            break;
        };
        out.push(&after[..end]);
        rest = after;
    }
    out
}

// ---------------------------------------------------------------------------
// Fixture sanity — the guards stand on bytes the hub really serves.
// ---------------------------------------------------------------------------

#[test]
fn the_guards_stand_on_the_dialog_surfaces_the_hub_actually_serves() {
    assert!(
        DASHBOARD.contains("<script src=\"/assets/js/shell.js\">"),
        "the dashboard no longer loads shell.js — these guards assert on a \
         script nobody serves; point them at the file that is"
    );
    let dialog = overlay_window(DASHBOARD, "ov-newticket");
    assert!(
        !dialog.is_empty(),
        "#ov-newticket vanished from the dashboard markup — the ACs have no \
         dialog left to gate"
    );
    for id in ["nt-title", "nt-desc", "nt-ac", "nt-err"] {
        assert!(
            dialog.contains(&format!("id=\"{id}\"")),
            "#{id} is gone from the new-ticket dialog — the guards below have \
             nothing left to assert on"
        );
    }
    // "both Save buttons remain functional" needs both to exist and to be
    // wired to the one code path that POSTs /tickets — explicit Save.
    assert!(
        dialog.contains("onclick=\"saveTicket(false)\"")
            && dialog.contains("onclick=\"saveTicket(true)\""),
        "the new-ticket dialog no longer wires both Save buttons to \
         saveTicket — AC4's 'both Save buttons remain functional' has no \
         fixture left"
    );
    assert!(
        js_fn(SHELL, "openNewTicket").is_some()
            && js_fn(SHELL, "saveTicket").is_some()
            && js_fn(SHELL, "ntDescGet").is_some(),
        "openNewTicket/saveTicket/ntDescGet vanished from shell.js — the \
         dialog plumbing the ACs build on is gone"
    );
    assert!(
        js_fn(SHELL, "saveTicket").is_some_and(|f| f.contains("\"/tickets\"")),
        "saveTicket no longer POSTs /tickets — the 'explicit Save' that AC3 \
         gates on no longer exists"
    );
}

// ---------------------------------------------------------------------------
// AC1 — the preview renders a verdict panel with rationale, and never saves
// ---------------------------------------------------------------------------

#[test]
fn ac1_check_feasibility_renders_a_verdict_panel_with_rationale_and_never_saves() {
    let dialog = overlay_window(DASHBOARD, "ov-newticket");
    let Some(button) = dialog_button_for_label(dialog, BUTTON_LABEL) else {
        panic!(
            "no button labelled '{BUTTON_LABEL}' in the #ov-newticket dialog — \
             the idea-filing feasibility preview (CXA-F250 AC1) does not exist \
             yet"
        );
    };
    let Some(name) = onclick_handler(button) else {
        panic!(
            "the '{BUTTON_LABEL}' button carries no inline onclick handler — \
             this suite resolves the click the way the browser does (see the \
             header note); move the guard if the wiring changed"
        );
    };
    let Some(handler) = js_fn(SHELL, name) else {
        panic!(
            "'{BUTTON_LABEL}' invokes {name}() but that function is not \
             defined in the served dialog script (shell.js) — AC1's preview \
             cannot render"
        );
    };
    for verdict in VERDICTS {
        assert!(
            handler.contains(verdict),
            "the preview must show one of Feasible / Needs clarification / \
             Not feasible, but the handler never spells '{verdict}' (CXA-F250 \
             AC1)"
        );
    }
    // "renders within #ov-newticket a visible feasibility-preview panel": the
    // handler must open a panel that lives in the dialog markup (the
    // #nt-notes precedent: a static container inside the overlay, shown on
    // demand) — not print into #nt-err, which is a one-line status element.
    let panel_ids: Vec<&str> = looked_up_ids(handler)
        .into_iter()
        .filter(|id| dialog.contains(&format!("id=\"{id}\"")))
        .collect();
    assert!(
        !panel_ids.is_empty(),
        "the feasibility handler never opens a panel that lives inside the \
         #ov-newticket markup — the preview has no dialog surface (CXA-F250 \
         AC1)"
    );
    assert!(
        handler.contains("style.display") || handler.contains("classList"),
        "the feasibility handler never toggles a panel's visibility — \
         'renders … a visible feasibility-preview panel' is not implemented \
         (CXA-F250 AC1)"
    );
    // "at least one sentence of rationale": prose derived from the engine's
    // response, not just a static label — a dynamic interpolation into the
    // panel render. The response FIELD the prose comes from is deliberately
    // not pinned: no such contract exists in the codebase yet (see header).
    let renders_prose = handler.contains("${") || {
        let inner = handler.find("innerHTML=");
        inner.is_some_and(|i| {
            let rhs = &handler[i + "innerHTML=".len()..];
            rhs.find('+').is_some_and(|p| {
                !rhs[..p].ends_with('\"') && !rhs[..p].ends_with('\'')
            })
        })
    };
    assert!(
        renders_prose,
        "the preview panel is rendered from static strings only — no \
         response-derived rationale prose is interpolated (CXA-F250 AC1)"
    );
    // "clicking it performs no save (POST /tickets is never called)": the
    // preview handler must neither call saveTicket nor touch /tickets.
    assert!(
        !handler.contains("saveTicket") && !handler.contains("\"/tickets\""),
        "the feasibility preview handler saves (calls saveTicket or POSTs \
         /tickets) — previewing must perform no save (CXA-F250 AC1)"
    );
}

// ---------------------------------------------------------------------------
// AC2 — empty dialog: inline message, no request, no ticket
// ---------------------------------------------------------------------------

#[test]
fn ac2_empty_dialog_shows_write_the_idea_first_and_fires_no_request() {
    let dialog = overlay_window(DASHBOARD, "ov-newticket");
    let name = dialog_button_for_label(dialog, BUTTON_LABEL)
        .and_then(onclick_handler)
        .unwrap_or_else(|| {
            panic!(
                "the '{BUTTON_LABEL}' button does not exist in the \
                 #ov-newticket dialog — AC2's empty-input guard (CXA-F250) \
                 has nothing to gate"
            )
        });
    let handler = js_fn(SHELL, name).unwrap_or_else(|| {
        panic!("{name}() (the '{BUTTON_LABEL}' handler) is not in shell.js")
    });
    // The guard must consult BOTH fields the AC names — the description via
    // its existing accessor and the title input — and it must speak before
    // any fetch happens: with both empty, no network request fires at all.
    let first_fetch = handler.find("fetch(");
    let guard_zone = first_fetch.map_or(handler, |at| &handler[..at]);
    assert!(
        guard_zone.contains("ntDescGet") && guard_zone.contains("nt-title"),
        "the empty-input guard does not consult both the description \
         (ntDescGet) and the title (nt-title) — clicking with one field \
         filled would take the wrong path (CXA-F250 AC2)"
    );
    let msg_at = handler.find(EMPTY_IDEA_MSG);
    assert!(
        msg_at.is_some(),
        "the handler never shows the inline '{EMPTY_IDEA_MSG}' message in \
         #nt-err on empty input (CXA-F250 AC2)"
    );
    if let (Some(m), Some(f)) = (msg_at, first_fetch) {
        assert!(
            m < f,
            "the '{EMPTY_IDEA_MSG}' guard sits after the first fetch( — on \
             empty input a network request fires before the message \
             (CXA-F250 AC2)"
        );
    }
    assert!(
        !handler.contains("saveTicket") && !handler.contains("\"/tickets\""),
        "the feasibility handler can create a ticket (saveTicket / POST \
         /tickets) — an empty-input click must create none, and a preview \
         click must never save (CXA-F250 AC2)"
    );
}

// ---------------------------------------------------------------------------
// AC3 — zero backlog side effects; explicit Save is the only writer
// ---------------------------------------------------------------------------

#[test]
fn ac3_preview_and_cancel_leave_the_backlog_untouched_until_explicit_save() {
    let dialog = overlay_window(DASHBOARD, "ov-newticket");
    let name = dialog_button_for_label(dialog, BUTTON_LABEL)
        .and_then(onclick_handler)
        .unwrap_or_else(|| {
            panic!(
                "the '{BUTTON_LABEL}' button does not exist — AC3's \
                 zero-side-effects preview (CXA-F250) has nothing to gate"
            )
        });
    let handler = js_fn(SHELL, name).unwrap_or_else(|| {
        panic!("{name}() (the '{BUTTON_LABEL}' handler) is not in shell.js")
    });
    assert!(
        !handler.contains("saveTicket") && !handler.contains("\"/tickets\""),
        "generating or viewing a feasibility preview writes to the backlog \
         (saveTicket / POST /tickets in the preview handler) — the preview \
         must have zero backlog side effects (CXA-F250 AC3)"
    );
    // Closing/cancelling afterwards: every close affordance in the dialog
    // (backdrop click, .x, Cancel) must close and nothing else — no ticket
    // creation smuggled into the dismiss path. Explicit Save stays the only
    // /tickets writer (pinned in the fixture-sanity test).
    let mut closes = 0;
    let mut rest = dialog;
    while let Some(at) = rest.find("onclick=\"") {
        let after = &rest[at + "onclick=\"".len()..];
        let end = after.find('"').map_or(after.len(), |e| e);
        let wiring = &after[..end];
        if wiring.contains("close_('ov-newticket')") {
            closes += 1;
            assert!(
                !wiring.contains("saveTicket") && !wiring.contains("fetch"),
                "a close affordance of #ov-newticket does more than close \
                 ({wiring}) — cancelling must leave GET /state unchanged \
                 (CXA-F250 AC3)"
            );
        }
        if end >= after.len() {
            break;
        }
        rest = &after[end..];
    }
    assert!(
        closes >= 2,
        "the dialog lost its cancel/close affordances ({closes} found) — \
         AC3's 'closing/cancelling #ov-newticket' path is gone"
    );
}

// ---------------------------------------------------------------------------
// AC4 — no configured engine: graceful inline failure, form intact
// ---------------------------------------------------------------------------

#[test]
fn ac4_without_a_configured_engine_the_preview_fails_gracefully_keeping_the_form() {
    let dialog = overlay_window(DASHBOARD, "ov-newticket");
    let name = dialog_button_for_label(dialog, BUTTON_LABEL)
        .and_then(onclick_handler)
        .unwrap_or_else(|| {
            panic!(
                "the '{BUTTON_LABEL}' button does not exist — AC4's \
                 no-engine failure path (CXA-F250) has nothing to gate"
            )
        });
    let handler = js_fn(SHELL, name).unwrap_or_else(|| {
        panic!("{name}() (the '{BUTTON_LABEL}' handler) is not in shell.js")
    });
    assert!(
        handler.contains(NO_ENGINE_MSG),
        "the feasibility handler never reports '{NO_ENGINE_MSG}' inline in \
         #nt-err when no engine is configured (CXA-F250 AC4)"
    );
    // "all typed form fields keep their entered values": the failure path
    // must not write any dialog field — the description via its setter, and
    // no `.value=` assignment anywhere in the handler (reads carry no `=`).
    assert!(
        !handler.contains("ntDescSet("),
        "the feasibility handler rewrites the description field — a failed \
         or successful preview must leave typed values alone (CXA-F250 AC4)"
    );
    assert!(
        !handler.contains(".value="),
        "the feasibility handler assigns a form field's .value — entered \
         values must survive the preview (CXA-F250 AC4)"
    );
    // "both Save buttons remain functional": the handler never references
    // saveTicket, so it can neither fire nor break the Save wiring pinned in
    // the fixture-sanity test.
    assert!(
        !handler.contains("saveTicket"),
        "the feasibility handler touches saveTicket — the Save buttons must \
         stay untouched by previewing (CXA-F250 AC4)"
    );
}

// ---------------------------------------------------------------------------
// AC5 — network failure: inline message, values kept, nothing thrown away
// ---------------------------------------------------------------------------

#[test]
fn ac5_network_failure_keeps_the_form_and_reports_inline() {
    let dialog = overlay_window(DASHBOARD, "ov-newticket");
    let name = dialog_button_for_label(dialog, BUTTON_LABEL)
        .and_then(onclick_handler)
        .unwrap_or_else(|| {
            panic!(
                "the '{BUTTON_LABEL}' button does not exist — AC5's network-\
                 error path (CXA-F250) has nothing to gate"
            )
        });
    let handler = js_fn(SHELL, name).unwrap_or_else(|| {
        panic!("{name}() (the '{BUTTON_LABEL}' handler) is not in shell.js")
    });
    // The request is guarded by try/catch — a rejected fetch or bad response
    // lands in the catch, reports inline in #nt-err, and throws nothing
    // uncaught (an uncaught throw would hit the window.onerror console gate
    // the e2e suite arms via armConsoleGate).
    assert!(
        handler.contains("catch"),
        "the feasibility handler makes its request without try/catch — a \
         network error would throw uncaught instead of reporting inline \
         (CXA-F250 AC5)"
    );
    assert!(
        handler.contains(NETWORK_ERR_MSG),
        "the feasibility handler never shows the inline '{NETWORK_ERR_MSG}' \
         message on request failure (CXA-F250 AC5)"
    );
    assert!(
        !handler.contains("ntDescSet(") && !handler.contains(".value="),
        "the feasibility handler rewrites form fields — entered values must \
         survive a failed request (CXA-F250 AC5)"
    );
}
