// Overview view-mesh (CXA-B232b, subtask 2/3 of CXA-B232).
//
// The presentation side of the PURE Overview→panel-contract mapping
// (crates/domain/src/overview_panel_contract.rs, CXA-B255/B232a): one entry
// per above-the-fold panel, in the domain's declared RENDER_ORDER, carrying
// the stable mount (`domId`) and the rendering-evidence paint site. Like the
// domain mapping it is TOTAL and SNAPSHOT-FREE — panel identity is static
// screen structure, not data — and it deliberately carries NO state, phase or
// business logic: what a panel SHOWS belongs to overview_states.js /
// terminal_state.js, this module owns only what the panel IS.
//
// Consumers (overview_states.js today) resolve panel identity THROUGH this
// mesh instead of restating dom ids inline, so a mount rename is a one-place,
// contract-checked edit. The pinned test
// crates/presentation/tests/overview_view_mesh_b232b.rs cross-checks every
// field against the domain contract so the two sides cannot drift.
(function () {
  "use strict";

  // The contract table, mirrored in render order. `key` is this repo's
  // panel-key spelling (overview_states.js / panel registry); `domId` +
  // `paintSite` restate `panel_contract(id)` verbatim.
  var CONTRACT = [
    { key: "drain", domId: "ov-drain", paintSite: 'crates/presentation/src/web/js/core.js:797 drainBanner("ov-drain") · shell.js:750' },
    { key: "alerts", domId: "ov-alerts", paintSite: "crates/presentation/src/web/js/core.js:796 alertsHtml()" },
    { key: "working", domId: "ov-working", paintSite: "crates/presentation/src/web/js/core.js:1022 workers renderer" },
    { key: "kpis", domId: "kpis", paintSite: "crates/presentation/src/web/js/core.js:798 overviewKpis() · kpis.js:76" },
    { key: "health", domId: "ov-health", paintSite: "crates/presentation/src/web/js/chat.js:1713 health renderer" },
    { key: "recent-activity", domId: "ov-activity", paintSite: "crates/presentation/src/web/js/core.js:814 activity renderer" },
    { key: "releases", domId: "ov-changelog", paintSite: "crates/presentation/src/web/js/core.js:816 releases renderer" },
  ];

  function byKey(key) {
    for (var i = 0; i < CONTRACT.length; i++) if (CONTRACT[i].key === key) return CONTRACT[i];
    return null;
  }

  // Every above-the-fold panel's contract, top-of-view to bottom — fresh
  // copies, so callers cannot mutate the shared table.
  function panels() {
    return CONTRACT.map(function (c) {
      return { key: c.key, domId: c.domId, paintSite: c.paintSite };
    });
  }

  // The panel's stable DOM mount, or null for an unknown key — identity
  // lookup, never a rendering decision.
  function domId(panelKey) {
    var c = byKey(panelKey);
    return c ? c.domId : null;
  }

  window.OvViewMesh = { panels: panels, domId: domId };
})();
