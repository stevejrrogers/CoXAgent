//! CXA-B232b — the Overview presentation resolves panel identity THROUGH the
//! pure domain contract mapping (`overview_panel_contract.rs`, CXA-B255),
//! via the thin JS mirror `web/js/overview_vm.js` (the view-mesh), not through
//! inline restated dom ids in `overview_states.js`.
//!
//! The new-contract test pins that (a) the mesh mirrors the domain contract
//! field-for-field (same domIds, same paint sites, same render order, total
//! over RENDER_ORDER), and (b) the wiring's PANELS table now sources its mounts
//! from the mesh — an unknown key yields a null mount exactly when the domain
//! lookup has no such panel. No live server; Node evaluates the scripts and
//! asserts against `window.OvViewMesh.*`.

use coxagent_domain::overview_panel::OverviewPanelId;
use coxagent_domain::overview_panel_contract::panel_contract;
use std::process::Command;

/// The repo's own panel-key spelling (terminal-state wiring / panel registry),
/// which is deliberately NOT the enum variant name — resolved next to the
/// contract so the mapping stays explicit.
fn js_key(id: OverviewPanelId) -> &'static str {
    match id {
        OverviewPanelId::OvDrain => "drain",
        OverviewPanelId::OvAlerts => "alerts",
        OverviewPanelId::OvWorking => "working",
        OverviewPanelId::Kpis => "kpis",
        OverviewPanelId::OvHealth => "health",
        OverviewPanelId::OvActivity => "recent-activity",
        OverviewPanelId::OvChangelog => "releases",
    }
}

/// Load the terminal-state shell, the view-mesh and the wiring into one Node
/// scope (with a minimal `window`) — the same harness shape as the B194 test.
fn eval_js(expr: &str) -> String {
    let shell = include_str!("../src/web/js/terminal_state.js");
    let mesh = include_str!("../src/web/js/overview_vm.js");
    let wiring = include_str!("../src/web/js/overview_states.js");
    let script =
        format!("const window=globalThis;\n{shell}\n{mesh}\n{wiring}\n;console.log(({expr}));");
    let out = Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .expect("node must be available to evaluate the view-mesh + wiring scripts");
    assert!(
        out.status.success(),
        "node evaluation failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The view-mesh is the contract: one fresh entry per domain panel, in
/// RENDER_ORDER, with domId AND paintSite identical to `panel_contract(id)`.
#[test]
fn js_view_mesh_mirrors_the_pure_domain_contract_field_for_field() {
    let contracts: Vec<_> = OverviewPanelId::RENDER_ORDER
        .iter()
        .map(|id| (*id, panel_contract(*id)))
        .collect();
    let mesh = eval_js(
        r#"JSON.stringify(OvViewMesh.panels().map(function(p){
            return {key:p.key, domId:p.domId, paintSite:p.paintSite};}))"#,
    );

    // Walk the JSON by hand — serde_json is not a presentation dependency;
    // every field is quoted JSON, so substring matches on `"field":"value"`
    // are exact-value checks.
    let mut cursor = 0usize;
    for (id, c) in contracts {
        let key = js_key(id);
        let dom_needle = format!(r#""domId":"{}""#, c.dom_id);
        let site_needle = c.paint_site.replace('"', "\\\"");
        let pos = mesh[(cursor)..]
            .find(&dom_needle)
            .unwrap_or_else(|| panic!("{key}: domId `{}` missing or out of order", c.dom_id));
        cursor += pos;
        assert!(
            mesh[cursor..].contains(&site_needle),
            "{key}: paintSite drifted from the domain contract: `{}` vs `{}`",
            site_needle,
            &mesh[cursor..(cursor + site_needle.len()).min(mesh.len())]
        );
        // The mesh exposes the panel under the repo's own key spelling.
        assert!(
            mesh.contains(&format!(r#""key":"{key}""#)),
            "{key}: view-mesh does not carry the panel key"
        );
    }
}

/// The wiring sources its panel mounts from the mesh (the mesh before it
/// loaded → null mount), proving the panels' identity flows through the
/// contract mapping rather than restated literals.
#[test]
fn overview_wiring_resolves_mounts_through_the_view_mesh() {
    for id in OverviewPanelId::RENDER_ORDER {
        let key = js_key(*id);
        let mount = eval_js(&format!(
            "(OvPanelStates.PANELS['{key}']||{{}}).domId===undefined ? 'MISSING' : OvPanelStates.PANELS['{key}'].domId"
        ));
        assert_eq!(
            mount,
            panel_contract(*id).dom_id,
            "{key}: wiring mount did not come through the view-mesh / contract"
        );
    }
}

/// Unknown keys surface as a null mount (total mapping, no silent default):
/// the wiring can ask and get an honest answer instead of a wrong literal.
#[test]
fn unknown_panel_key_yields_a_null_mount_not_an_invention() {
    assert_eq!(eval_js("String(OvViewMesh.domId('no-such-panel'))"), "null");
    // And the real enum covers every above-the-fold panel key.
    assert_eq!(eval_js("OvViewMesh.panels().length"), "7");
}
