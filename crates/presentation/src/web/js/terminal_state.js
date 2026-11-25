// Shared terminal-state component (CXA-B196, subtask 2/3 of CXA-B173a).
//
// ONE component for every above-the-fold Overview panel's non-ready states.
// The frozen contract lives in the Rust manifest
// (crates/presentation/src/overview_panels.rs: TerminalState, Slot,
// required_slots/optional_slots); REQUIRED/OPTIONAL/ROLES below mirror it so
// the guard test can cross-check both sides. States (from the manifest):
//
//   loading    spinner + title (title only required; body optional why-copy)
//   empty      icon + title + body — the body IS the "what fills this" hint
//   error      icon + title + body + primary(retry) — the body IS the reason
//   ready      caller's own panel HTML; the component is only the shell elsewhere
//   attention  icon + title + body + primary + secondary — operator must act
//   zero       like empty but healthy: every value at its floor, no history yet
//
// Pure presentation, per the B196 ticket: props in, HTML out, callbacks bound.
// No fetch, no imports, no store reads, no navigation — the caller owns the
// data request, the retry callback and the transition out of `loading`
// (a panel resting on `loading` forever is the B131 bug class this kills).
//
// Required slots missing sensible copy DEFAULT rather than render blank, so a
// panel can never ship a bare 0 or a silent blank; a defaulted slot is marked
// data-ts-defaulted="1" so wiring/tests can see the fallback fired.
//
// Unknown state names and unknown spec keys THROW — rejecting unknown input is
// the JS-side stand-in for the type-level rejection the Rust enum enforces.
(function () {
  "use strict";

  // Mirror of the Rust contract: TerminalState::name() order, Slot::* names.
  var STATES = ["loading", "empty", "error", "ready", "attention", "zero"];
  var REQUIRED = {
    loading: ["title"],
    empty: ["icon", "title", "body"],
    error: ["icon", "title", "body", "primaryAction"],
    ready: [],
    attention: ["icon", "title", "body", "primaryAction", "secondaryAction"],
    zero: ["icon", "title", "body"]
  };
  var OPTIONAL = {
    loading: ["body"],
    empty: ["secondaryAction"],
    error: ["secondaryAction"],
    ready: ["title", "primaryAction", "secondaryAction"],
    attention: [],
    zero: []
  };
  // States that owe the operator a reason get role="alert" (assertive);
  // everything else announces politely. Matches TerminalState::requires_reason.
  var ROLES = {
    loading: "status",
    empty: "status",
    error: "alert",
    ready: "status",
    attention: "alert",
    zero: "status"
  };

  // Defaults so no required slot ever renders empty. Copy is generic on
  // purpose: CXA-B173c passes the per-panel copy; these only catch a panel
  // that shipped without any.
  var DEFAULTS = {
    loading: { title: "Loading…" },
    empty: {
      icon: { ti: "inbox" },
      title: "Nothing here yet",
      body: "This fills in as the project produces the data it needs."
    },
    error: {
      icon: { ti: "alert-triangle" },
      title: "Couldn't load this panel",
      body: "The data request failed — no reason was given.",
      primary: { label: "Retry" }
    },
    attention: {
      icon: { ti: "alert-circle" },
      title: "Needs your attention",
      body: "This panel is paused until an operator acts on it.",
      primary: { label: "Review" },
      secondary: { label: "Dismiss" }
    },
    zero: {
      icon: { ti: "chart-bar" },
      title: "Nothing recorded yet",
      body: "Every value is at its floor — history appears as work lands."
    }
  };

  // Spec keys each state understands: identity (state, panelId), the state's
  // required slots, its optional slots, and ready's trusted readyHtml.
  // Anything else is rejected — render() throws on unknown keys.
  var SPEC_KEYS = {
    loading: ["state", "panelId", "title", "body"],
    empty: ["state", "panelId", "icon", "title", "body", "secondary"],
    error: ["state", "panelId", "icon", "title", "body", "primary", "secondary"],
    ready: ["state", "panelId", "title", "readyHtml", "primary", "secondary"],
    attention: ["state", "panelId", "icon", "title", "body", "primary", "secondary"],
    zero: ["state", "panelId", "icon", "title", "body"]
  };

  // Self-contained escaping: this module must not lean on core.js globals, so
  // it stays testable and servable in isolation (B196: implement in isolation).
  function esc(s) {
    return String(s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  function isIcon(x) {
    return !!x && typeof x === "object" && !Array.isArray(x) && typeof x.ti === "string";
  }

  function iconHtml(icon) {
    if (typeof icon === "string")
      return '<span class="ts-icon" data-ts-slot="icon" aria-hidden="true">' + esc(icon) + "</span>";
    if (isIcon(icon))
      return '<i class="ti ti-' + esc(icon.ti) + ' ts-icon" data-ts-slot="icon" aria-hidden="true"></i>';
    throw new TypeError("TerminalState: icon must be a glyph string or {ti:…}");
  }

  // Resolve an action slot: caller's value wins; error/attention fall back to
  // the DEFAULTS so the required affordance can never be missing.
  function resolveAction(state, spec, key, defaultKey) {
    var d = DEFAULTS[state] || {};
    var own = spec[key];
    if (own == null) {
      if (d[defaultKey] == null) return null;
      return { label: d[defaultKey].label, onClick: null, defaulted: 1 };
    }
    if (typeof own !== "object" || typeof own.label !== "string")
      throw new TypeError("TerminalState: " + key + " must be {label, onClick}");
    return { label: own.label, onClick: own.onClick, defaulted: 0 };
  }

  // One stylesheet for the component, injected once — keeps app.css untouched
  // (B196: zero changes outside the component) and every panel theme-consistent
  // by reusing the existing CSS variables.
  var STYLE_ID = "ts-state-style";
  function ensureStyles() {
    if (document.getElementById(STYLE_ID)) return;
    var st = document.createElement("style");
    st.id = STYLE_ID;
    st.textContent =
      ".ts-state{display:flex;flex-direction:column;align-items:center;justify-content:center;" +
      "gap:8px;text-align:center;color:var(--dim);padding:26px 14px;border:1px dashed var(--border);" +
      "border-radius:var(--r);background:var(--card)}" +
      ".ts-state--error,.ts-state--attention{border-color:var(--amber,#c8a719)}" +
      ".ts-icon{font-size:22px;line-height:1}" +
      ".ts-spinner{width:18px;height:18px;border:2px solid var(--border);border-top-color:var(--dim);" +
      "border-radius:50%;animation:ts-spin .9s linear infinite}" +
      "@keyframes ts-spin{to{transform:rotate(360deg)}}" +
      ".ts-title{font-size:13px;font-weight:600;color:var(--fg,#ddd)}" +
      ".ts-body{font-size:12px;max-width:46ch}" +
      ".ts-actions{display:flex;gap:8px;margin-top:4px}";
    document.head.appendChild(st);
  }

  // TerminalState.render(spec) -> HTML string. Throws on unknown state,
  // unknown spec keys, a malformed action, or `ready` without readyHtml.
  function render(spec) {
    if (!spec || typeof spec !== "object") throw new TypeError("TerminalState: spec object required");
    var state = spec.state;
    if (STATES.indexOf(state) < 0)
      throw new TypeError("TerminalState: unknown state " + JSON.stringify(state));
    var allowed = SPEC_KEYS[state];
    for (var k in spec) {
      if (Object.prototype.hasOwnProperty.call(spec, k) && allowed.indexOf(k) < 0)
        throw new TypeError("TerminalState[" + state + "]: unknown spec key " + JSON.stringify(k));
    }
    if (state === "ready" && typeof spec.readyHtml !== "string")
      throw new TypeError("TerminalState[ready]: readyHtml (trusted panel HTML) required");

    var d = DEFAULTS[state] || {};
    var parts = [];

    if (state === "loading") {
      // The spinner is the component's own affair — Loading requires no icon.
      parts.push('<span class="ts-spinner" aria-hidden="true"></span>');
    } else if (state !== "ready") {
      parts.push(iconHtml(spec.icon != null ? spec.icon : d.icon));
    }

    if (state !== "ready") {
      var title = spec.title != null ? spec.title : d.title;
      var defT = spec.title == null && d.title != null ? ' data-ts-defaulted="1"' : "";
      parts.push('<div class="ts-title" data-ts-slot="title"' + defT + ">" + esc(title) + "</div>");
      var body = spec.body != null ? spec.body : d.body;
      var defB = spec.body == null && d.body ? ' data-ts-defaulted="1"' : "";
      if (body)
        parts.push('<div class="ts-body" data-ts-slot="body"' + defB + ">" + esc(body) + "</div>");
    } else if (spec.title != null) {
      parts.push('<div class="ts-title" data-ts-slot="title">' + esc(spec.title) + "</div>");
    }

    var acts = [];
    var primary = resolveAction(state, spec, "primary", "primary");
    var secondary = resolveAction(state, spec, "secondary", "secondary");
    if (primary) acts.push({ slot: "primaryAction", a: primary });
    if (secondary) acts.push({ slot: "secondaryAction", a: secondary });

    if (state === "ready") {
      // The caller owns ready-shaped panels: their HTML is trusted (the same
      // codebase renders it), never escaped.
      parts.push('<div data-ts-slot="readyBody">' + spec.readyHtml + "</div>");
    }
    if (acts.length) {
      var btns = acts
        .map(function (x, i) {
          return (
            '<button type="button" class="btn" data-ts-slot="' + x.slot + '" data-ts-act="' + i + '"' +
            (x.a.defaulted ? ' data-ts-defaulted="1"' : "") + ">" + esc(x.a.label) + "</button>"
          );
        })
        .join("");
      parts.push('<div class="ts-actions">' + btns + "</div>");
    }

    var role = ROLES[state];
    return (
      '<div class="ts-state ts-state--' + state + '" role="' + role + '" data-ts-state="' + state + '"' +
      ' data-ts-panel="' + esc(spec.panelId || "") + '">' +
      parts.join("") + "</div>"
    );
  }

  // TerminalState.paint(host, spec): render + bind the caller's callbacks.
  // onClick handlers stay in JS (never serialized), so callbacks are the
  // caller's responsibility exactly as the contract says.
  function paint(host, spec) {
    ensureStyles();
    host.innerHTML = render(spec);
    var acts = [];
    var p = resolveAction(spec.state, spec, "primary", "primary");
    var s = resolveAction(spec.state, spec, "secondary", "secondary");
    if (p && p.onClick) acts.push(p);
    if (s && s.onClick) acts.push(s);
    Array.prototype.forEach.call(host.querySelectorAll("[data-ts-act]"), function (btn) {
      // Bind by slot, not index: a defaulted action has no onClick and must
      // not shift the caller's handlers onto the wrong button.
      var slot = btn.getAttribute("data-ts-slot");
      var a = slot === "primaryAction" ? p : s;
      if (a && a.onClick) btn.addEventListener("click", a.onClick);
    });
    return host.firstElementChild || { innerHTML: host.innerHTML };
  }

  // Wiring/tests surface (CXA-B173c consumes STATES/REQUIRED/OPTIONAL/ROLES).
  window.TerminalState = {
    STATES: STATES,
    REQUIRED: REQUIRED,
    OPTIONAL: OPTIONAL,
    ROLES: ROLES,
    render: render,
    paint: paint
  };
})();
