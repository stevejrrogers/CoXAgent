// Overview panel terminal-state wiring (CXA-B194, subtask 3/3 of CXA-B172).
//
// ONE owner for the four common states of the above-the-fold Overview panels:
// SKELETON (initial fetch — progress mark + what it is waiting on), READY (the
// panel's own content, untouched, plus a fetched-at attribution line),
// EMPTY (no data yet + the hint that says what fills the panel) and ERROR
// (the real cause + retry, dimmed with the reason when retrying cannot help).
// Rendering goes through the shared TerminalState component
// (web/js/terminal_state.js) — this module never paints bare load markup,
// progress-only markup or ad-hoc error divs; the per-panel identity and
// dependencies mirror the frozen Rust manifest
// (crates/presentation/src/overview_panels.rs) and the guard test
// overview_panel_terminal_state_b194.rs cross-checks both sides.
//
// The retry path is the port boundary in the browser: wireRetry(host, key,
// fetchImpl) re-dispatches the caller's fetch and transitions the panel via
// the pure phaseOf() rule — SKELETON/ERROR/EMPTY + outcome → terminal state.
// Pure parts (phaseOf, errorFrom, hints, dependency labels) are exported for
// the Node-based guard tests; none of them touch the DOM.
(function () {
  "use strict";

  // Panel identity resolution goes THROUGH the Overview view-mesh
  // (web/js/overview_vm.js, CXA-B232b): the mesh owns each panel's stable DOM
  // mount and paint-site evidence as mirrored from the pure domain mapping
  // (overview_panel_contract.rs); this module adds only the per-panel data
  // dependency + empty-state hint that terminal states need. No dom id is
  // restated here — mounts resolve via OvViewMesh.domId(), never inline.
  function meshDom(panelKey) {
    return window.OvViewMesh ? window.OvViewMesh.domId(panelKey) : null;
  }

  // Per-panel data dependency (mount comes from the view-mesh; dependency
  // labels stay in sync with OVERVIEW_PANELS via the guard test).
  var PANELS = {
    drain: {
      domId: meshDom("drain"),
      dep: "open merge queue + refactor-goal flag",
      hint: "The drain banner appears while a refactor pause holds new work until the open green PRs merge.",
    },
    alerts: {
      domId: meshDom("alerts"),
      dep: "1 Hz state snapshot: deploy, budget, tickets, reverted_work",
      hint: "Alerts appear when a deploy fails, the budget cap is hit or reverted work stacks up — nothing needs you right now.",
    },
    working: {
      domId: meshDom("working"),
      dep: "activity trail + per-agent worker status",
      hint: "Busy agents appear here the moment they claim a ticket — start or resume a ticket and they show up.",
    },
    kpis: {
      domId: meshDom("kpis"),
      dep: "state history: activity, tickets, releases, cost (client-side day bucketing)",
      hint: "KPI tiles fill in from the hub's state history — the first snapshot lands within a minute of connect.",
    },
    health: {
      domId: meshDom("health"),
      dep: "state snapshot: tickets, reviews, sprint commitments",
      hint: "Health cards fill in as reviews and sprint commitments are recorded in the snapshot.",
    },
    "recent-activity": {
      domId: meshDom("recent-activity"),
      dep: "activity trail (last 7 entries, newest first)",
      hint: "Agent actions appear here with the first step after connect.",
    },
    releases: {
      domId: meshDom("releases"),
      dep: "deploy history (zero-token changelog)",
      hint: "No releases yet — the changelog fills from deploy history after the first ship.",
    },
  };

  // The panel's own emptiness rule: does its host element hold painted
  // content? A host with no children is the EMPTY dataset case.
  function hostHasContent(panelKey) {
    var p = PANELS[panelKey];
    var host = p && document.getElementById(p.domId);
    return !!(host && host.children && host.children.length);
  }

  // Pure transition — the phase the panel is in plus the outcome of one
  // dispatch of its data fetch decide the next phase. `outcome` is
  // { ok: bool, hasData: bool } (null outcome = still pending → stay).
  // Ready is sticky: a painted panel does not fall back to a shell state.
  function phaseOf(phase, outcome) {
    if (phase === "ready") return "ready";
    if (!outcome) return phase === "loading" ? "loading" : phase === "empty" ? "empty" : "error";
    if (outcome.ok) return outcome.hasData ? "ready" : "empty";
    return "error";
  }

  // ------------------------------------------------------------------
  // Empty-state depth (CXA-B271). ALL empty-state copy lives in
  // EMPTY_COPY — one reviewable file is the rule (this module), so the
  // guidance, filtered-echo, pagination and denial strings stay auditable
  // in a single place next to the per-panel hints above. Templates use
  // LITERAL {token} placeholders filled by fillTpl(); an unfilled token
  // stays visible so missing data is caught by eyes and tests, never
  // silently blanked.
  // ------------------------------------------------------------------
  var EMPTY_COPY = {
    noDataTitle: "Nothing here yet",
    countsLine: " ({counts})",
    filteredTitle: "Nothing matches the current filter",
    filteredEcho: 'Active filter: "{filter}".',
    filteredTotal: "{total} entries exist without the filter.",
    sortKept: "Still sorted by {sort} — clearing the filter keeps the sort.",
    clearFilter: "Clear filter",
    pageTitle: "No results on page {page}",
    pageBody: "{total} entries in total — page {page} is past the end ({pages} pages).",
    backToPage: "Back to page {page}",
    denyDefault: "you do not have permission for this action",
  };

  // Fill literal {token} placeholders; an unknown token survives visibly.
  function fillTpl(tpl, params) {
    return String(tpl).replace(/\{([a-z]+)\}/g, function (_, t) {
      var v = params ? params[t] : undefined;
      return v === undefined ? "{" + t + "}" : String(v);
    });
  }

  // Which empty variant applies, decided PURELY from the detail the caller
  // hands over — no DOM, no IO: a window past its end beats an active
  // filter (both can be set at once); anything else is the plain no-data
  // state whose hint already says what fills the panel.
  function emptyVariantOf(d) {
    if (!d || typeof d !== "object") return "no-data";
    if (
      typeof d.page === "number" &&
      typeof d.pageSize === "number" &&
      d.pageSize > 0 &&
      typeof d.total === "number" &&
      d.page > Math.max(1, Math.ceil(d.total / d.pageSize))
    )
      return "past-end-page";
    if (d.filter) return "filtered";
    return "no-data";
  }

  // Permission wrapper: a DENIED action KEEPS its affordance and carries
  // the why (deniedWhy). Dimming happens at paint time (renderPhase →
  // dimEmptyActions); the label is never dropped from the spec.
  function authorizedAction(action, permission) {
    if (!action || !permission || permission.allowed !== false) return action;
    var out = {};
    for (var k in action)
      if (Object.prototype.hasOwnProperty.call(action, k)) out[k] = action[k];
    out.unauthorizedReason = permission.deniedWhy || EMPTY_COPY.denyDefault;
    return out;
  }

  // The empty spec, per variant. `detail` (optional) is the caller's
  // view-model slice: { counts, filter, total, sort, page, pageSize,
  // onClearFilter, onBack, permission }.
  function emptySpec(panelKey, detail) {
    var p = PANELS[panelKey];
    var spec = {
      state: "empty",
      icon: { ti: "inbox" },
      title: EMPTY_COPY.noDataTitle,
      body: p.hint,
      panelId: p.domId,
    };
    if (!detail || typeof detail !== "object") return spec;

    var variant = emptyVariantOf(detail);
    if (variant === "filtered") {
      spec.title = EMPTY_COPY.filteredTitle;
      spec.body =
        fillTpl(EMPTY_COPY.filteredEcho, { filter: detail.filter }) +
        (detail.total != null
          ? " " + fillTpl(EMPTY_COPY.filteredTotal, { total: detail.total })
          : "") +
        (detail.sort ? " " + fillTpl(EMPTY_COPY.sortKept, { sort: detail.sort }) : "");
      spec.secondary = authorizedAction(
        { label: EMPTY_COPY.clearFilter, onClick: detail.onClearFilter || null },
        detail.permission
      );
    } else if (variant === "past-end-page") {
      var pages = Math.max(1, Math.ceil(detail.total / detail.pageSize));
      spec.title = fillTpl(EMPTY_COPY.pageTitle, { page: detail.page });
      spec.body = fillTpl(EMPTY_COPY.pageBody, {
        total: detail.total,
        page: detail.page,
        pages: pages,
      });
      spec.secondary = authorizedAction(
        { label: fillTpl(EMPTY_COPY.backToPage, { page: pages }), onClick: detail.onBack || null },
        detail.permission
      );
    } else if (detail.counts) {
      // Plain no-data with sibling data in scope: surface the total so the
      // zero state is never a context-free blank.
      spec.body += fillTpl(EMPTY_COPY.countsLine, { counts: detail.counts });
    }
    return spec;
  }

  // Map any failure shape to { reason, retryable, kind }. Unknown shapes
  // degrade to a retryable network cause — a failure is never swallowed.
  function errorFrom(err) {
    if (err && typeof err === "object") {
      var reason = err.message || err.reason || (err.status ? "the data request failed with status " + err.status : "");
      if (!reason) reason = "the data request failed without a reason";
      return {
        reason: String(reason),
        retryable: err.retryable !== false,
        kind: err.kind || "network",
      };
    }
    return { reason: String(err), retryable: true, kind: "network" };
  }

  // The loading (skeleton) spec: the progress mark comes from the component;
  // the title names the data dependency, never a bare load marker.
  function loadingSpec(panelKey) {
    return { state: "loading", title: "Loading " + PANELS[panelKey].dep + "…", panelId: PANELS[panelKey].domId };
  }

  // The error spec: the body IS the real cause (attribution); the primary
  // action is Retry when the cause is retryable, absent when it is not —
  // dimRetry then marks the component's defaulted Retry as hopeless.
  function errorSpec(panelKey, err) {
    var e = errorFrom(err);
    var spec = {
      state: "error",
      icon: { ti: "alert-triangle" },
      title: "Couldn't load " + PANELS[panelKey].dep,
      body: e.reason + (e.retryable ? "" : " — retrying cannot help until the underlying issue is fixed"),
      panelId: PANELS[panelKey].domId,
    };
    if (e.retryable) spec.primary = { label: "Retry", onClick: null };
    return spec;
  }

  // Mark the rendered Retry as dimmed-with-reason for a non-retryable cause:
  // the affordance stays visible (permission-dimmed, not hidden) and the
  // tooltip states why it will not help.
  function dimRetry(host, reason) {
    Array.prototype.forEach.call(host.querySelectorAll('[data-ts-slot="primaryAction"]'), function (btn) {
      if (btn.disabled !== undefined) btn.disabled = true;
      if (btn.setAttribute) {
        btn.setAttribute("data-ts-dimmed", "1");
        btn.setAttribute("title", "Retry unavailable: " + reason);
      }
      if (btn.style) btn.style.opacity = "0.45";
    });
  }

  // Attribution line the shell appends for READY: when the data was fetched.
  function attributionHtml(when) {
    return (
      '<div class="ts-fetched" data-ts-attrib="fetched">Fetched ' +
      String(when).replace(/[&<>"]/g, function (c) {
        return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c];
      }) +
      "</div>"
    );
  }

  // Dim every action of an already-painted EMPTY state whose permission
  // says denied (CXA-B271): the affordance stays visible and labelled —
  // never hidden — and carries the why as tooltip + data attribute.
  function dimEmptyActions(host, permission) {
    if (!permission || permission.allowed !== false) return;
    var why = permission.deniedWhy || "you do not have permission for this action";
    Array.prototype.forEach.call(
      host.querySelectorAll(".ts-actions [data-ts-slot]"),
      function (btn) {
        if (btn.disabled !== undefined) btn.disabled = true;
        btn.setAttribute("data-ts-dimmed", "1");
        btn.setAttribute("title", why);
        if (btn.style) btn.style.opacity = "0.45";
      }
    );
  }

  // Render `phase` for `panelKey` into its host element. READY is the
  // caller's affair (returns "ready" without painting); the shell owns the
  // other three. details: { error, when, retryable, emptyDetail }.
  // `hostEl` (optional) lets a caller that already holds the host element —
  // the retry path, or a DOM-less guard test — paint without a document.
  function renderPhase(panelKey, phase, details, hostEl) {
    var p = PANELS[panelKey];
    var host = hostEl || (typeof document === "undefined" ? null : document.getElementById(p.domId));
    if (!host || !window.TerminalState) return phase;
    var d = details || {};
    if (phase === "ready") return phase; // caller paints its own body
    if (phase === "loading") {
      window.TerminalState.paint(host, loadingSpec(panelKey));
      return phase;
    }
    if (phase === "empty") {
      window.TerminalState.paint(host, emptySpec(panelKey, d.emptyDetail));
      dimEmptyActions(host, d.emptyDetail && d.emptyDetail.permission);
      return phase;
    }
    // error
    window.TerminalState.paint(host, errorSpec(panelKey, d.error));
    if (d.error && errorFrom(d.error).retryable === false) dimRetry(host, errorFrom(d.error).reason);
    return phase;
  }

  // Wire the Retry button of an already-painted ERROR state to `fetchImpl` —
  // the port boundary. Re-dispatches the fetch, re-renders per phaseOf from
  // the ERROR phase, and on success lets the caller paint via out.paint().
  function wireRetry(host, panelKey, fetchImpl) {
    Array.prototype.forEach.call(host.querySelectorAll('[data-ts-slot="primaryAction"]'), function (btn) {
      if (btn.disabled) return; // dimmed retry is inert
      btn.addEventListener("click", function () {
        renderPhase(panelKey, "loading", {}, host);
        Promise.resolve()
          .then(fetchImpl)
          .then(function (out) {
            var next = phaseOf("error", out);
            if (next === "ready") {
              if (out && typeof out.paint === "function") out.paint();
              if (out && out.fetchedWhen) stampAttribution(panelKey, out.fetchedWhen);
            } else {
              renderPhase(panelKey, next, {}, host);
            }
          })
          .catch(function (e) {
            renderPhase(panelKey, "error", { error: e }, host);
          });
      });
    });
  }

  // Append the fetched-at attribution to a panel's host (READY attribution).
  function stampAttribution(panelKey, when) {
    var host = typeof document === "undefined" ? null : document.getElementById(PANELS[panelKey].domId);
    if (host && host.insertAdjacentHTML) host.insertAdjacentHTML("beforeend", attributionHtml(when));
  }

  // First paint: every above-the-fold panel starts as a SKELETON that names
  // its dependency — no panel ever rests on bare load markup again.
  function bootSkeletons() {
    Object.keys(PANELS).forEach(function (k) {
      var host = typeof document === "undefined" ? null : document.getElementById(PANELS[k].domId);
      if (host) window.TerminalState.paint(host, loadingSpec(k));
    });
  }

  window.OvPanelStates = {
    PANELS: PANELS,
    phaseOf: phaseOf,
    errorFrom: errorFrom,
    loadingSpec: loadingSpec,
    emptySpec: emptySpec,
    emptyVariantOf: emptyVariantOf,
    authorizedAction: authorizedAction,
    EMPTY_COPY: EMPTY_COPY,
    errorSpec: errorSpec,
    attributionHtml: attributionHtml,
    renderPhase: renderPhase,
    wireRetry: wireRetry,
    stampAttribution: stampAttribution,
    bootSkeletons: bootSkeletons,
  };
})();
