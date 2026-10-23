//! CXA-B194 — the Overview panels' four common states (skeleton, ready,
//! empty, error) route through ONE owner: the shared TerminalState component
//! (`web/js/terminal_state.js`) driven by the wiring in
//! `web/js/overview_states.js`, which mirrors the frozen Rust manifest
//! (`src/overview_panels.rs`, CXA-B195). No Overview file may render bare
//! 'loading…' or ad-hoc spinner/error markup again (the B131/B163 bug class).
//!
//! The tests exercise the wiring's pure parts in isolation — the Node runner
//! evaluates the script and calls `window.OvPanelStates.*` directly, no live
//! server, no network. Retriability and the retry re-dispatch are covered
//! against a stub fetch at the same boundary the browser uses.

// Test scratch-parsing of shipped JS/HTML bytes: an `expect` here fails the
// test loudly, which is exactly what a guardrail should do (repo convention).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use coxagent_presentation::overview_panels;
use std::process::Command;

/// Load the wiring script and return the JS evaluation harness.
fn node() -> Command {
    Command::new("node")
}

fn run_node(script: &str) -> String {
    let out =
        node().arg("-e").arg(script).output().unwrap_or_else(|e| {
            panic!("node must be available to evaluate the wiring script: {e}")
        });
    assert!(
        out.status.success(),
        "node evaluation failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Evaluate overview_states.js (with a minimal `window`) and return the value
/// of the last expression.
fn eval_js(expr: &str) -> String {
    let wiring = include_str!("../src/web/js/overview_states.js");
    let shell = include_str!("../src/web/js/terminal_state.js");
    // The wiring resolves panel mounts through the Overview view-mesh
    // (CXA-B232b), so the mesh must load before overview_states.js —
    // mirroring index.html / APP_JS order.
    let mesh = include_str!("../src/web/js/overview_vm.js");
    let script =
        format!("const window=globalThis;\n{shell}\n{mesh}\n{wiring}\n;console.log(({expr}));");
    run_node(&script)
}

// ---------------------------------------------------------------------------
// Pure contract: the four phases and the transition rule
// ---------------------------------------------------------------------------

/// phaseOf is the single transition rule: ok+data → ready, ok without data →
/// empty, failure → error, and Ready is sticky (a painted panel never falls
/// back into a shell state).
#[test]
fn phase_of_covers_the_four_states_and_ready_is_sticky() {
    for phase in ["loading", "empty", "error"] {
        assert_eq!(
            eval_js(&format!("OvPanelStates.phaseOf('{phase}', null)")),
            phase
        );
        assert_eq!(
            eval_js(&format!(
                "OvPanelStates.phaseOf('{phase}', {{ok:true,hasData:true}})"
            )),
            "ready"
        );
        assert_eq!(
            eval_js(&format!(
                "OvPanelStates.phaseOf('{phase}', {{ok:true,hasData:false}})"
            )),
            "empty"
        );
        assert_eq!(
            eval_js(&format!("OvPanelStates.phaseOf('{phase}', {{ok:false}})")),
            "error"
        );
    }
    // Ready never regresses, whatever the outcome.
    assert_eq!(
        eval_js("OvPanelStates.phaseOf('ready', {ok:false})"),
        "ready"
    );
    assert_eq!(
        eval_js("OvPanelStates.phaseOf('ready', {ok:true,hasData:false})"),
        "ready"
    );
}

/// SKELETON must name the panel's data dependency — the B131 bug was a
/// permanent bare `loading…` with no hint of what was being waited on.
#[test]
fn skeleton_names_the_data_dependency_and_never_a_bare_loading() {
    for key in [
        "drain",
        "alerts",
        "working",
        "kpis",
        "health",
        "recent-activity",
        "releases",
    ] {
        let spec = eval_js(&format!(
            "JSON.stringify(OvPanelStates.loadingSpec('{key}'))"
        ));
        assert!(
            spec.contains("\"state\":\"loading\""),
            "{key}: not a loading spec: {spec}"
        );
        assert!(
            spec.contains("Loading "),
            "{key}: skeleton does not name its dependency: {spec}"
        );
        assert!(
            !spec.contains("\"loading…\"") && !spec.contains("loading…\""),
            "{key}: bare 'loading…' text leaked into the skeleton: {spec}"
        );
    }
}

/// EMPTY must carry the what-fills-this hint, per panel — not a generic empty.
#[test]
fn empty_spec_carries_a_panel_specific_hint() {
    let kpis = eval_js("JSON.stringify(OvPanelStates.emptySpec('kpis'))");
    assert!(kpis.contains("\"state\":\"empty\""), "{kpis}");
    assert!(
        kpis.contains("state history") || kpis.contains("snapshot"),
        "kpis empty hint does not say what fills the panel: {kpis}"
    );
    let alerts = eval_js("JSON.stringify(OvPanelStates.emptySpec('alerts'))");
    assert!(
        alerts.contains("nothing needs you"),
        "alerts empty hint not panel-specific: {alerts}"
    );
}

/// ERROR carries the real cause (attribution) and is retryable by default;
/// a non-retryable failure keeps the message but drops the Retry action —
/// the caller then dims it with the reason via dimRetry's marker.
#[test]
fn error_spec_carries_the_cause_and_retryability() {
    let retryable = eval_js("JSON.stringify(OvPanelStates.errorSpec('kpis', {message:'snapshot request failed: 503 Bad Gateway'}))");
    assert!(
        retryable.contains("503 Bad Gateway"),
        "cause missing: {retryable}"
    );
    assert!(
        retryable.contains("\"Retry\""),
        "retryable error lost its Retry action: {retryable}"
    );

    let hopeless = eval_js("JSON.stringify(OvPanelStates.errorSpec('kpis', {message:'viewer is not authorized', retryable:false}))");
    assert!(
        hopeless.contains("not authorized"),
        "cause missing: {hopeless}"
    );
    assert!(
        !hopeless.contains("\"Retry\""),
        "non-retryable error still offers Retry: {hopeless}"
    );
}

/// Unknown failure shapes still surface as errors with a cause — never
/// swallowed into a silent idle.
#[test]
fn unknown_failure_shapes_degrade_to_a_retryable_cause() {
    let spec = eval_js("JSON.stringify(OvPanelStates.errorSpec('alerts', 'socket closed'))");
    assert!(spec.contains("socket closed"), "{spec}");
    assert!(spec.contains("\"Retry\""), "{spec}");
}

/// The wiring's panel identities must mirror the frozen Rust manifest
/// (overview_panels.rs): same dom ids, same dependency descriptions.
#[test]
fn js_panel_manifest_mirrors_the_rust_manifest() {
    let panels = overview_panels::OVERVIEW_PANELS;
    for p in panels {
        let key = p.id.to_string();
        let dom = eval_js(&format!(
            "OvPanelStates.PANELS['{key}'] ? OvPanelStates.PANELS['{key}'].domId : 'MISSING'"
        ));
        assert_eq!(
            dom, p.dom_id,
            "{key}: JS wiring drifted from the Rust manifest"
        );
        let dep = eval_js(&format!("OvPanelStates.PANELS['{key}'].dep"));
        assert_eq!(
            dep, p.dependency.feeds,
            "{key}: dependency description drifted between JS wiring and Rust manifest"
        );
    }
}

// ---------------------------------------------------------------------------
// Retriability at the port boundary (stub fetch, no server, no network)
// ---------------------------------------------------------------------------

/// Retry re-dispatches the fetch after a failure and reaches READY when it
/// succeeds; the fetch stub is the same boundary the browser's click handler
/// uses (`wireRetry(host,key,fetchImpl)`).
#[test]
fn retry_after_failure_redispatches_and_reaches_ready() {
    let script = format!(
        r#"
const window=globalThis;
var __noopEl=function(){{return {{id:"",textContent:""}};}};
window.document={{getElementById:function(){{return null;}},createElement:__noopEl,head:{{appendChild:function(){{}}}}}};
{}
const calls={{n:0}};
const host={{querySelectorAll:(sel)=>sel==='[data-ts-slot="primaryAction"]'?[btn]:[]}};
const btn={{disabled:false,listeners:{{}},addEventListener(t,f){{this.listeners[t]=f;}},style:{{}}}};
const painted=[];
window.TerminalState={{paint:(hostEl,spec)=>painted.push(spec)}};
window.OvPanelStates.renderPhase('kpis','error',{{error:{{message:'first fetch failed'}}}});
window.OvPanelStates.wireRetry(host,'kpis',()=>{{calls.n++;return Promise.resolve({{ok:true,hasData:true,paint:()=>painted.push('CALLER_PAINT')}});}});
btn.listeners.click();
setTimeout(()=>{{
  if(calls.n!==1) throw new Error('retry did not re-dispatch the fetch: '+calls.n);
  if(!painted.includes('CALLER_PAINT')) throw new Error('retry never reached READY: '+JSON.stringify(painted));
  console.log('OK');
}},0);
"#,
        include_str!("../src/web/js/overview_states.js")
    );
    assert_eq!(run_node(&script), "OK");
}

/// A failing retry re-enters ERROR with the new cause — it must not idle on
/// a spinner or swallow the second failure.
#[test]
fn failing_retry_reenters_error_with_the_new_cause() {
    let script = format!(
        r#"
const window=globalThis;
var __noopEl=function(){{return {{id:"",textContent:""}};}};
window.document={{getElementById:function(){{return null;}},createElement:__noopEl,head:{{appendChild:function(){{}}}}}};
{}
const btn={{disabled:false,listeners:{{}},addEventListener(t,f){{this.listeners[t]=f;}},style:{{}}}};
const host={{querySelectorAll:(sel)=>sel==='[data-ts-slot="primaryAction"]'?[btn]:[]}};
const painted=[];
window.TerminalState={{paint:(hostEl,spec)=>painted.push(spec)}};
window.OvPanelStates.renderPhase('alerts','error',{{error:{{message:'first failure'}}}});
window.OvPanelStates.wireRetry(host,'alerts',()=>Promise.reject({{message:'second failure: engine down'}}));
btn.listeners.click();
setTimeout(()=>{{
  const last=painted[painted.length-1];
  if(!last||last.state!=='error') throw new Error('retry did not re-enter ERROR: '+JSON.stringify(painted));
  if(!String(last.body).includes('second failure')) throw new Error('new cause not shown: '+JSON.stringify(last));
  console.log('OK');
}},0);
"#,
        include_str!("../src/web/js/overview_states.js")
    );
    assert_eq!(run_node(&script), "OK");
}

// ---------------------------------------------------------------------------
// Served-bytes gate: the Overview files own no bare 'loading…' markup
// ---------------------------------------------------------------------------

/// Repo grep equivalent: no bare 'loading…' / spinner-only text may live in
/// the Overview scripts — the shell is the single owner of those states.
#[test]
fn no_bare_loading_markup_left_in_overview_files() {
    // terminal_state.js is deliberately absent: the shared shell is the ONE
    // owner of the loading state, so its spinner markup is the point.
    for file in [
        "src/web/js/overview_states.js",
        "src/web/js/kpis.js",
        "src/overview_panels.rs",
    ] {
        let body = std::fs::read_to_string(
            env!("CARGO_MANIFEST_DIR").to_string() + "/src/" + file.trim_start_matches("src/"),
        )
        .unwrap_or_else(|e| panic!("{file}: unreadable: {e}"));
        for needle in ["loading…", "loading...", "spinner"] {
            assert!(
                !body.to_lowercase().contains(needle),
                "{file}: bare '{needle}' markup is back — PanelShell owns that state"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The Rust manifest side (re-run the frozen B195 contract on this branch)
// ---------------------------------------------------------------------------

/// Every manifest panel still reaches a ready/zero terminal state and the
/// lookup helper resolves every entry (B195 contract, unchanged here).
#[test]
fn manifest_contract_still_holds_on_this_branch() {
    for p in overview_panels::OVERVIEW_PANELS {
        assert!(
            p.states
                .iter()
                .any(|s| s.is_terminal() && !matches!(s, overview_panels::TerminalState::Loading)),
            "{}: no terminal state in the manifest",
            p.dom_id
        );
        assert!(
            overview_panels::panel(p.id).is_some(),
            "{}: lookup lost",
            p.dom_id
        );
    }
}

// ---------------------------------------------------------------------------
// CXA-B271 — Empty-state depth: guidance, counts, filtered-empty inverse,
// paginated empty page, permission-dimmed unauthorized actions. All pure
// functions over the wiring's view-model slice `{counts, filter, total,
// sort, page, pageSize, onClearFilter, onBack, permission}` — no DOM, no IO;
// the one rendered scenario goes through TerminalState.paint exactly as the
// browser does.
// ---------------------------------------------------------------------------

/// Without a detail slice the empty state is still never a bare blank: it
/// carries the panel-specific what-fills-this hint and a non-defaulted copy.
#[test]
fn b271_no_data_empty_state_names_what_fills_the_panel() {
    let spec = eval_js("JSON.stringify(OvPanelStates.emptySpec('working', null))");
    assert!(
        spec.contains("\"state\":\"empty\""),
        "not an empty spec: {spec}"
    );
    assert!(
        spec.contains("claim a ticket"),
        "empty state lost the what-fills-this guidance: {spec}"
    );
    // Bare-blank guard: title AND body are always non-empty strings.
    let body_nonblank = eval_js(
        "OvPanelStates.emptySpec('releases', {}).title.length > 0 && OvPanelStates.emptySpec('releases', {}).body.length > 0",
    );
    assert_eq!(body_nonblank, "true", "empty state rendered a bare blank");
}

/// Sibling data elsewhere in the panel's scope surfaces as a count line —
/// zero never stands alone.
#[test]
fn b271_no_data_with_scope_counts_shows_the_totals() {
    let spec =
        eval_js("JSON.stringify(OvPanelStates.emptySpec('alerts', {counts: '12 tickets open'}))");
    assert!(
        spec.contains("12 tickets open"),
        "scope counts missing from the no-data state: {spec}"
    );
}

/// Filtered-empty echoes the active filter verbatim, offers its inverse
/// (clear filter) and states that the sort survives the clear.
#[test]
fn b271_filtered_empty_echoes_the_filter_and_offers_the_inverse() {
    let spec = eval_js(
        r"JSON.stringify(OvPanelStates.emptySpec('kpis', {filter: 'cost>0', total: 41, sort: 'newest first'}))",
    );
    assert!(
        spec.contains("Nothing matches the current filter"),
        "filtered-empty kept the generic title: {spec}"
    );
    assert!(
        spec.contains("cost&gt;0") || spec.contains("cost>0") || spec.contains("cost\\u003e0"),
        "active filter not echoed verbatim: {spec}"
    );
    assert!(
        spec.contains("41 entries exist without the filter"),
        "unfiltered total missing: {spec}"
    );
    assert!(
        spec.contains("Still sorted by newest first"),
        "sort preservation not stated: {spec}"
    );
    assert!(
        spec.contains("Clear filter"),
        "inverse affordance (clear filter) missing: {spec}"
    );
}

/// A pagination window past its end shows position ('no results on page N'),
/// the totals and a way back to the last real page.
#[test]
fn b271_past_end_page_shows_position_and_a_way_back() {
    let spec = eval_js(
        "JSON.stringify(OvPanelStates.emptySpec('kpis', {page: 7, pageSize: 50, total: 120}))",
    );
    assert!(
        spec.contains("No results on page 7"),
        "page position missing: {spec}"
    );
    assert!(
        spec.contains("120 entries in total") && spec.contains("(3 pages)"),
        "totals/pages context missing: {spec}"
    );
    assert!(
        spec.contains("Back to page 3"),
        "way back to the last populated page missing: {spec}"
    );
}

/// Variant decision is pure: past-end wins over an active filter (both can be
/// set), an in-range page with a filter is filtered-empty, everything else is
/// plain no-data.
#[test]
fn b271_variant_decision_precedence_is_pure() {
    // Past-end beats filter.
    let v =
        eval_js("OvPanelStates.emptyVariantOf({filter: 'x', page: 4, pageSize: 10, total: 20})");
    assert_eq!(v, "past-end-page");
    // In-range page + filter → filtered.
    let v = eval_js("OvPanelStates.emptyVariantOf({filter: 'x', page: 1, pageSize: 10, total: 0})");
    assert_eq!(v, "filtered");
    // Plain shapes degrade to no-data, never throw.
    for shape in ["null", "{\"", "{'filter':''}", "{}"] {
        let _ = shape; // placeholder to keep the loop readable below
    }
    let v = eval_js("OvPanelStates.emptyVariantOf(null)");
    assert_eq!(v, "no-data");
    let v = eval_js("OvPanelStates.emptyVariantOf({})");
    assert_eq!(v, "no-data");
    let v = eval_js("OvPanelStates.emptyVariantOf({filter: ''})");
    assert_eq!(v, "no-data");
}

/// Rendered (through TerminalState.paint, the real path): a DENIED
/// clear-filter stays visible and labelled but renders dimmed-with-reason —
/// never hidden, never silently inert.
#[test]
fn b271_unauthorized_action_renders_dimmed_with_a_why_not_hidden() {
    let script = format!(
        r#"
const window=globalThis;
var __noopEl=function(){{return {{id:"",textContent:""}};}};
window.document={{getElementById:function(){{return null;}},createElement:__noopEl,head:{{appendChild:function(){{}}}}}};
{}
{}
const btn={{
  slot:"secondaryAction", disabled:false, attrs:{{}}, style:{{}},
  getAttribute:function(){{ return this.slot; }},
  setAttribute:function(k,v){{ this.attrs[k]=String(v); }},
  addEventListener:function(){{}}
}};
const host={{
  innerHTML:"",
  querySelectorAll:function(){{ return [btn]; }}
}};
window.OvPanelStates.renderPhase('kpis','empty',{{emptyDetail:{{
  filter:"mine", onClearFilter:function(){{}},
  permission:{{allowed:false, deniedWhy:"viewers cannot edit board filters"}}
}}}}, host);
if(!host.innerHTML.includes("Clear filter")) throw new Error("denied affordance was hidden: "+host.innerHTML);
if(btn.disabled!==true) throw new Error("denied action not disabled: "+JSON.stringify(btn.attrs));
if(btn.attrs["data-ts-dimmed"]!=="1") throw new Error("dim marker missing: "+JSON.stringify(btn.attrs));
if(!String(btn.attrs.title||"").includes("viewers cannot edit board filters")) throw new Error("why-hint missing: "+btn.attrs.title);
console.log("OK");
"#,
        include_str!("../src/web/js/terminal_state.js"),
        include_str!("../src/web/js/overview_states.js")
    );
    assert_eq!(run_node(&script), "OK");
}

/// The allowed path stays untouched: no dim marker, no disabled, click bound.
#[test]
fn b271_authorized_actions_render_undimmed_and_clickable() {
    let script = format!(
        r#"
const window=globalThis;
var __noopEl=function(){{return {{id:"",textContent:""}};}};
window.document={{getElementById:function(){{return null;}},createElement:__noopEl,head:{{appendChild:function(){{}}}}}};
{}
{}
let fired=false;
const btn={{
  slot:"secondaryAction", disabled:false, attrs:{{}}, style:{{}}, fire:function(){{}},
  getAttribute:function(){{ return this.slot; }},
  setAttribute:function(k,v){{ this.attrs[k]=String(v); }},
  addEventListener:function(ev,fn){{ this.fire=function(){{ fn(); }}; }}
}};
const host={{
  innerHTML:"",
  querySelectorAll:function(){{ return [btn]; }}
}};
window.OvPanelStates.renderPhase('kpis','empty',{{emptyDetail:{{
  filter:"mine", onClearFilter:function(){{ fired=true; }}
}}}}, host);
if(btn.disabled===true || btn.attrs["data-ts-dimmed"]) throw new Error("allowed action was dimmed: "+JSON.stringify(btn.attrs));
btn.fire();
if(!fired) throw new Error("clear-filter callback not bound");
console.log("OK");
"#,
        include_str!("../src/web/js/terminal_state.js"),
        include_str!("../src/web/js/overview_states.js")
    );
    assert_eq!(run_node(&script), "OK");
}

/// The unauthorized clear-filter still KEEPS its label in the spec even when
/// permission info arrives only at paint time (pure wrapper, no dropping).
#[test]
fn b271_permission_wrapper_dims_but_never_drops_the_label() {
    let spec = eval_js(
        r#"JSON.stringify(OvPanelStates.authorizedAction({label: "Clear filter", onClick: null}, {allowed: false, deniedWhy: "read-only role"}))"#,
    );
    assert!(spec.contains("Clear filter"), "label dropped: {spec}");
    assert!(
        spec.contains("read-only role"),
        "why-hint not attached: {spec}"
    );
    // Allowed or absent permission → the action passes through unchanged.
    let passthrough = eval_js(
        r#"JSON.stringify(OvPanelStates.authorizedAction({label: "Clear filter"}, {allowed: true}))"#,
    );
    assert!(passthrough.contains("Clear filter"), "{passthrough}");
    assert!(!passthrough.contains("unauthorizedReason"), "{passthrough}");
}
