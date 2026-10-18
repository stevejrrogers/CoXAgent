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

  // Panel identity + data dependency, mirroring OVERVIEW_PANELS in
  // overview_panels.rs (dom_id + dependency.feeds, kept in sync by the guard
  // test). `host` is the element the panel paints into.
  var PANELS = {
    drain: {
      domId: "ov-drain",
      dep: "open merge queue + refactor-goal flag",
      hint: "The drain banner appears while a refactor pause holds new work until the open green PRs merge.",
    },
    alerts: {
      domId: "ov-alerts",
      dep: "1 Hz state snapshot: deploy, budget, tickets, reverted_work",
      hint: "Alerts appear when a deploy fails, the budget cap is hit or reverted work stacks up — nothing needs you right now.",
    },
    working: {
      domId: "ov-working",
      dep: "activity trail + per-agent worker status",
      hint: "Busy agents appear here the moment they claim a ticket — start or resume a ticket and they show up.",
    },
    kpis: {
      domId: "kpis",
      dep: "state history: activity, tickets, releases, cost (client-side day bucketing)",
      hint: "KPI tiles fill in from the hub's state history — the first snapshot lands within a minute of connect.",
    },
    health: {
      domId: "ov-health",
      dep: "state snapshot: tickets, reviews, sprint commitments",
      hint: "Health cards fill in as reviews and sprint commitments are recorded in the snapshot.",
    },
    "recent-activity": {
      domId: "ov-activity",
      dep: "activity trail (last 7 entries, newest first)",
      hint: "Agent actions appear here with the first step after connect.",
    },
    releases: {
      domId: "ov-changelog",
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

  // The empty spec: icon + title + the what-fills-this hint.
  function emptySpec(panelKey) {
    return {
      state: "empty",
      icon: { ti: "inbox" },
      title: "Nothing here yet",
      body: PANELS[panelKey].hint,
      panelId: PANELS[panelKey].domId,
    };
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

  // Render `phase` for `panelKey` into its host element. READY is the
  // caller's affair (returns "ready" without painting); the shell owns the
  // other three. details: { error, when, retryable }.
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
      window.TerminalState.paint(host, emptySpec(panelKey));
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
    errorSpec: errorSpec,
    attributionHtml: attributionHtml,
    renderPhase: renderPhase,
    wireRetry: wireRetry,
    stampAttribution: stampAttribution,
    bootSkeletons: bootSkeletons,
  };
})();
