//! CXA-B196 — the shared terminal-state component (`web/js/terminal_state.js`),
//! implemented against the frozen CXA-B195 manifest
//! (`src/overview_panels.rs`: `TerminalState` / `Slot` / `required_slots`).
//!
//! These are the component's own unit tests (subtask 2/3): every state, every
//! slot present-or-absent variant, the defaulting rule, unknown-input rejection
//! and callback binding — all asserted against the exact bytes the module
//! produces. The module runs under the same Node runtime the repo already
//! ships for `e2e`; the tests never start a server and never touch a network
//! port: they render strings and count slots in them.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The component under test, byte-for-byte what the server embeds.
const MODULE: &str = include_str!("../src/web/js/terminal_state.js");
/// The served-asset table — proves the module is actually shipped, not just
/// present on disk (the B131 failure mode: bytes nobody serves).
const SERVER: &str = include_str!("../src/server/mod.rs");
/// The dashboard shell — proves the script tag loads it ahead of core.js.
const INDEX_HTML: &str = include_str!("../src/web/index.html");

use coxagent_presentation::overview_panels::{
    optional_slots, panel, required_slots, OverviewPanelId, Slot, TerminalState,
};

/// Minimal DOM shim: `ensureStyles` only needs `getElementById` /
/// `createElement` / `head.appendChild`; nothing else touches the DOM before
/// the scenario prints its JSON.
const DOM_SHIM: &str = r#"
globalThis.window = {};
globalThis.document = {
  _styles: {},
  getElementById: function (id) { return this._styles[id] || null; },
  createElement: function () { return { id: "", textContent: "" }; },
  head: { appendChild: function (el) { document._styles[el.id] = el; } }
};
"#;

static RUN: AtomicUsize = AtomicUsize::new(0);

/// Run a scenario against the real module in a scratch dir; return stdout.
/// The scenario must end by printing exactly one line of JSON.
fn run_js(scenario: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "tsb196-{}-{}",
        std::process::id(),
        RUN.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let module = dir.join("terminal_state.js");
    std::fs::write(&module, MODULE).expect("write module");
    // Shim first (window/document must exist before the IIFE assigns into
    // window and before paint injects styles), then the module, then scenario.
    let main = dir.join("scenario.js");
    std::fs::write(
        &main,
        format!(
            "{DOM_SHIM}\nrequire({:?});\n{scenario}\n",
            module.display().to_string().replace('\\', "/")
        ),
    )
    .expect("write scenario");
    let out = Command::new("node")
        .arg(&main)
        .output()
        .expect("node runtime (same one the e2e/ stack uses)");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "scenario failed:\n--stderr--\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// `render(spec)` → `{threw, message?, html?}` as JSON.
fn render_json(spec: &str) -> (bool, String) {
    let out = run_js(&format!(
        r#"function rj(s){{try{{return{{threw:false,html:window.TerminalState.render(s)}};}}catch(e){{return{{threw:true,message:String(e.message)}};}}}}
console.log(JSON.stringify(rj({spec})));"#
    ));
    let threw = out.contains(r#""threw":true"#);
    (threw, out)
}

fn threw_with(spec: &str) -> String {
    let (threw, out) = render_json(spec);
    assert!(threw, "render({spec}) should have thrown, got: {out}");
    out
}

fn html_of(spec: &str) -> String {
    let (threw, out) = render_json(spec);
    assert!(!threw, "render({spec}) threw: {out}");
    unescape_json(&out)
}

/// The render output rides inside a JSON string: `"` became `\u0022` etc.
/// Only quotes matter for the substring matches below.
fn unescape_json(s: &str) -> String {
    // JSON.stringify escapes `"` as `\"` (and some engines as `\u0022`);
    // both must come back to raw quotes for the substring matches below.
    s.replace("\\u0022", "\"").replace("\\\"", "\"")
}

// ---------------------------------------------------------------------------
// Wiring guards — the module must be the SERVED component, not a dead file.
// ---------------------------------------------------------------------------

#[test]
fn the_module_is_served_by_the_router_and_loaded_by_the_shell() {
    // Format-agnostic: rustfmt may split the tuple across lines.
    assert!(
        SERVER.contains(r#""terminal_state.js""#)
            && SERVER.contains(r#"include_str!("../web/js/terminal_state.js")"#),
        "server/mod.rs must embed terminal_state.js as a served asset"
    );
    let core = INDEX_HTML
        .find(r#"src="/assets/js/core.js""#)
        .expect("core.js script tag");
    let ts = INDEX_HTML
        .find(r#"src="/assets/js/terminal_state.js""#)
        .expect("terminal_state.js script tag");
    assert!(ts < core, "terminal_state.js must load before core.js");
}

#[test]
fn js_contract_mirrors_the_frozen_rust_manifest() {
    let out = run_js(
        r#"console.log(JSON.stringify({S:window.TerminalState.STATES,R:window.TerminalState.REQUIRED,O:window.TerminalState.OPTIONAL,L:window.TerminalState.ROLES}));"#,
    );
    // Every Rust state name exists in the JS state list, in manifest order.
    let rust_states = ["loading", "empty", "error", "ready", "attention", "zero"];
    for name in rust_states {
        assert!(
            out.contains(&format!("\"{name}\"")),
            "JS missing state {name}: {out}"
        );
    }
    // The Rust slot table IS the expectation — no hand-copied duplicate.
    for state in [
        TerminalState::Loading,
        TerminalState::Empty,
        TerminalState::Error,
        TerminalState::Ready,
        TerminalState::Attention,
        TerminalState::Zero,
    ] {
        let name = state.name();
        for slot in required_slots(state) {
            assert!(
                out.contains(&format!("\"{}\":[", slot_key(slot)))
                    || out.contains(&format!("\",\"{}\"", slot_key(slot)))
                    || out.contains(&format!("[\"{}", slot_key(slot))),
                "JS REQUIRED[{name}] missing Rust-required slot {}:\n{out}",
                slot_key(slot)
            );
        }
        for slot in optional_slots(state) {
            assert!(
                out.contains(&format!("\",\"{}\"", slot_key(slot)))
                    || out.contains(&format!("[\"{}", slot_key(slot))),
                "JS OPTIONAL[{name}] missing Rust-optional slot {}:\n{out}",
                slot_key(slot)
            );
        }
        let expect_role = if state.requires_reason() {
            "alert"
        } else {
            "status"
        };
        assert!(
            out.contains(&format!("{name}\":\"{expect_role}\"")),
            "ROLES[{name}] must be {expect_role}:\n{out}"
        );
    }
}

fn slot_key(slot: &Slot) -> &'static str {
    match slot {
        Slot::Icon => "icon",
        Slot::Title => "title",
        Slot::Body => "body",
        Slot::PrimaryAction => "primaryAction",
        Slot::SecondaryAction => "secondaryAction",
    }
}

/// Slot name → the `data-ts-slot` the renderer stamps on that element.
fn dom_slot(slot: &Slot) -> &'static str {
    slot_key(slot)
}

// ---------------------------------------------------------------------------
// Per-state rendering: every state, every required slot present.
// ---------------------------------------------------------------------------

/// The happy-path spec for each state, per the manifest's slot contract.
fn good_spec(state: TerminalState) -> String {
    match state {
        TerminalState::Loading => r#"{"state":"loading","title":"Loading project"}"#,
        TerminalState::Empty => {
            r#"{"state":"empty","icon":{"ti":"inbox"},"title":"No releases yet","body":"Releases appear when the first deploy lands."}"#
        }
        TerminalState::Error => {
            r#"{"state":"error","icon":{"ti":"alert-triangle"},"title":"Couldn't load","body":"GET /state returned 503","primary":{"label":"Retry","onClick":null}}"#
        }
        TerminalState::Ready => r#"{"state":"ready","readyHtml":"<ul class='kris'><li>ok</li></ul>"}"#,
        TerminalState::Attention => {
            r#"{"state":"attention","icon":{"ti":"pause"},"title":"Drain paused","body":"2 green PRs must merge first","primary":{"label":"Resume","onClick":null},"secondary":{"label":"Details","onClick":null}}"#
        }
        TerminalState::Zero => {
            r#"{"state":"zero","icon":{"ti":"chart-bar"},"title":"No spend yet","body":"Cost appears with the first agent run."}"#
        }
    }
    .to_string()
}

#[test]
fn every_state_renders_its_required_slots_and_announces_correctly() {
    for state in [
        TerminalState::Loading,
        TerminalState::Empty,
        TerminalState::Error,
        TerminalState::Attention,
        TerminalState::Zero,
    ] {
        let html = html_of(&good_spec(state));
        assert!(
            html.contains(&format!(r#"data-ts-state="{}""#, state.name())),
            "{:?}: missing data-ts-state\n{html}",
            state
        );
        for slot in required_slots(state) {
            assert!(
                html.contains(&format!(r#"data-ts-slot="{}""#, dom_slot(slot))),
                "{:?}: required slot {} not rendered\n{html}",
                state,
                slot_key(slot)
            );
        }
        let want_role = if state.requires_reason() {
            "alert"
        } else {
            "status"
        };
        assert!(
            html.contains(&format!(r#"role="{want_role}""#)),
            "{:?}: expected role={want_role}\n{html}",
            state
        );
    }
}

#[test]
fn loading_paints_its_own_spinner_and_takes_no_icon() {
    let html = html_of(&good_spec(TerminalState::Loading));
    assert!(
        html.contains("ts-spinner"),
        "loading must show the spinner\n{html}"
    );
    assert!(
        !html.contains("data-ts-slot=\"icon\""),
        "loading has no icon slot\n{html}"
    );
}

#[test]
fn empty_body_is_the_hint_and_error_body_is_the_reason() {
    let empty = html_of(&good_spec(TerminalState::Empty));
    assert!(
        empty.contains("Releases appear when the first deploy lands."),
        "empty body must carry the what-fills-this hint\n{empty}"
    );
    let err = html_of(&good_spec(TerminalState::Error));
    assert!(
        err.contains("GET /state returned 503"),
        "error body must surface the reason verbatim\n{err}"
    );
    assert!(
        err.contains("ts-state--error") && err.contains("ts-alert")
            || err.contains("role=\"alert\""),
        "error is an assertive alert\n{err}"
    );
}

#[test]
fn error_renders_the_retry_and_attention_both_actions() {
    let err = html_of(&good_spec(TerminalState::Error));
    assert!(
        err.contains("data-ts-slot=\"primaryAction\""),
        "error needs retry\n{err}"
    );
    assert!(err.contains(">Retry<"), "{err}");
    let att = html_of(&good_spec(TerminalState::Attention));
    assert!(
        att.contains("data-ts-slot=\"primaryAction\"")
            && att.contains("data-ts-slot=\"secondaryAction\""),
        "attention renders both actions\n{att}"
    );
}

#[test]
fn ready_is_caller_html_only_and_rejects_ready_without_it() {
    let ready = html_of(&good_spec(TerminalState::Ready));
    assert!(
        ready.contains("<ul"),
        "ready keeps caller HTML verbatim\n{ready}"
    );
    assert!(
        !ready.contains("data-ts-slot=\"body\"") && !ready.contains("data-ts-slot=\"icon\""),
        "ready demands no slots\n{ready}"
    );
    let out = threw_with(r#"{"state":"ready"}"#);
    assert!(out.contains("readyHtml"), "{out}");
}

#[test]
fn optional_slots_are_absent_unless_given() {
    // empty without secondary → no actions row at all
    let bare = html_of(r#"{"state":"empty","icon":{"ti":"inbox"},"title":"t","body":"b"}"#);
    assert!(!bare.contains("data-ts-slot=\"secondaryAction\""), "{bare}");
    // empty WITH secondary → present
    let with = html_of(
        r#"{"state":"empty","icon":{"ti":"inbox"},"title":"t","body":"b","secondary":{"label":"Configure","onClick":null}}"#,
    );
    assert!(
        with.contains("data-ts-slot=\"secondaryAction\"") && with.contains(">Configure<"),
        "{with}"
    );
    // error without secondary → exactly one action
    let err = html_of(&good_spec(TerminalState::Error));
    assert!(!err.contains("data-ts-slot=\"secondaryAction\""), "{err}");
    // loading with optional body → body present; without → absent
    let lb = html_of(r#"{"state":"loading","title":"t","body":"fetching /state"}"#);
    assert!(
        lb.contains("data-ts-slot=\"body\"") && lb.contains("fetching /state"),
        "{lb}"
    );
    let l0 = html_of(r#"{"state":"loading","title":"t"}"#);
    assert!(!l0.contains("data-ts-slot=\"body\""), "{l0}");
}

// ---------------------------------------------------------------------------
// Defaults: no required slot can render blank; defaults are visible.
// ---------------------------------------------------------------------------

#[test]
fn every_required_slot_has_a_sensible_default_and_is_marked() {
    // Bare minimum spec per state → still no blank: title/body/action defaults.
    let l = html_of(r#"{"state":"loading"}"#);
    assert!(
        l.contains("Loading…") && l.contains("data-ts-defaulted"),
        "{l}"
    );
    let e = html_of(r#"{"state":"empty","icon":{"ti":"inbox"},"title":"Nothing yet"}"#);
    assert!(
        e.contains("data-ts-slot=\"body\"") && e.contains("data-ts-defaulted=\"1\""),
        "empty defaults the hint body\n{e}"
    );
    let err = html_of(r#"{"state":"error"}"#);
    assert!(
        err.contains("data-ts-defaulted"),
        "error defaults missing slots\n{err}"
    );
    assert!(
        err.contains(">Retry<"),
        "error defaults the retry affordance\n{err}"
    );
    assert!(
        err.contains("data-ts-slot=\"body\""),
        "error defaults the reason slot\n{err}"
    );
    let a = html_of(r#"{"state":"attention"}"#);
    assert!(
        a.contains(">Review<") && a.contains(">Dismiss<"),
        "attention defaults both actions\n{a}"
    );
    let z = html_of(r#"{"state":"zero"}"#);
    assert!(
        z.contains("data-ts-slot=\"body\""),
        "zero defaults the hint\n{z}"
    );
}

#[test]
fn caller_copy_always_wins_over_defaults() {
    let e = html_of(
        r#"{"state":"empty","icon":{"ti":"inbox"},"title":"No releases yet","body":"Releases appear when the first deploy lands."}"#,
    );
    assert!(
        !e.contains("data-ts-defaulted"),
        "explicit copy must not be flagged\n{e}"
    );
}

// ---------------------------------------------------------------------------
// Unknown input is rejected (the JS stand-in for the Rust enum's type check).
// ---------------------------------------------------------------------------

#[test]
fn unknown_state_and_unknown_spec_keys_throw() {
    let out = threw_with(r#"{"state":"shimmering"}"#);
    assert!(out.contains("unknown state"), "{out}");
    let out = threw_with(
        r#"{"state":"empty","icon":{"ti":"inbox"},"title":"t","body":"b","sparkle":true}"#,
    );
    assert!(
        out.contains("unknown spec key") && out.contains("sparkle"),
        "{out}"
    );
    let out = threw_with(
        r#"{"state":"error","icon":{"ti":"x"},"title":"t","body":"b","primary":"Retry"}"#,
    );
    assert!(
        out.contains("primary"),
        "non-object action is rejected\n{out}"
    );
    let out = threw_with(r#"null"#);
    assert!(out.contains("spec object required"), "{out}");
}

#[test]
fn all_html_is_escaped_including_panel_id() {
    let e = html_of(
        r#"{"state":"empty","icon":{"ti":"inbox"},"title":"<img src=x onerror=alert(1)>","body":"<script>alert(2)</script>"}"#,
    );
    assert!(
        !e.contains("<img") && !e.contains("<script>"),
        "copy must be escaped\n{e}"
    );
    assert!(e.contains("&lt;img"), "{e}");
    let p = html_of(r#"{"state":"loading","title":"t","panelId":"<b>ov</b>"}"#);
    assert!(!p.contains("<b>"), "panelId must be escaped\n{p}");
}

#[test]
fn every_manifest_panel_state_combination_renders_with_its_dom_id() {
    for p in panel(OverviewPanelId::Drain).into_iter().chain(
        panel(OverviewPanelId::Alerts)
            .into_iter()
            .chain(panel(OverviewPanelId::Working).into_iter())
            .chain(panel(OverviewPanelId::Kpis).into_iter())
            .chain(panel(OverviewPanelId::Health).into_iter())
            .chain(panel(OverviewPanelId::RecentActivity).into_iter())
            .chain(panel(OverviewPanelId::Releases).into_iter()),
    ) {
        for state in p.states {
            let spec = match state {
                TerminalState::Ready => format!(
                    r#"{{"state":"ready","readyHtml":"<div>{}</div>","panelId":"{}"}}"#,
                    p.dom_id, p.dom_id
                ),
                TerminalState::Attention => format!(
                    r#"{{"state":"attention","panelId":"{}","primary":{{"label":"Resume"}},"secondary":{{"label":"Details"}}}}"#,
                    p.dom_id
                ),
                TerminalState::Error => format!(
                    r#"{{"state":"error","panelId":"{}","primary":{{"label":"Retry"}}}}"#,
                    p.dom_id
                ),
                other => format!(r#"{{"state":"{}","panelId":"{}"}}"#, other.name(), p.dom_id),
            };
            let html = html_of(&spec);
            assert!(
                html.contains(&format!(r#"data-ts-panel="{}""#, p.dom_id)),
                "{} × {:?}: panel id missing\n{html}",
                p.dom_id,
                state
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Callbacks: paint binds the caller's onClick; callbacks are never serialized.
// ---------------------------------------------------------------------------

#[test]
fn paint_binds_the_callers_callbacks_and_nothing_else() {
    let out = run_js(
        r#"var host={innerHTML:"",querySelectorAll:function(sel){if(this._btns)return this._btns;var out=[],re=/data-ts-act="(\d)"/g,m;while((m=re.exec(this.innerHTML))){var b={i:m[1],clicked:0,slot:m[1]==="0"?"primaryAction":"secondaryAction"};b.getAttribute=function(){return this.slot;};b.addEventListener=function(ev,fn){this.fire=function(){this.clicked++;fn();};};out.push(b);}this._btns=out;return out;},firstElementChild:null};
var calls={p:0,s:0};
window.TerminalState.paint(host,{state:"error",icon:{ti:"alert-triangle"},title:"boom",body:"503",primary:{label:"Retry",onClick:function(){calls.p++;}},secondary:{label:"Details",onClick:function(){calls.s++;}}});
var btns=host.querySelectorAll("[data-ts-act]");
btns.forEach(function(b){b.fire();});
console.log(JSON.stringify({primary:calls.p,secondary:calls.s,binds:btns.length}));"#,
    );
    assert!(
        out.contains(r#""primary":1"#)
            && out.contains(r#""secondary":1"#)
            && out.contains(r#""binds":2"#),
        "each bound button must fire exactly its own callback: {out}"
    );
}

#[test]
fn paint_without_callbacks_renders_inert_buttons() {
    // A defaulted retry (no caller onClick) must not throw and must not bind.
    let out = run_js(
        r#"var host={innerHTML:"",querySelectorAll:function(){return [];},firstElementChild:null};
var el=window.TerminalState.paint(host,{state:"error"}).innerHTML||host.innerHTML;
console.log(JSON.stringify({ok:!!el&&el.indexOf("ts-state--error")>=0}));"#,
    );
    assert!(
        out.contains(r#""ok":true"#),
        "paint must work with zero callbacks: {out}"
    );
}
