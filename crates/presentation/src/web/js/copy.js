// Copy layer (CXA-B191) — single source of truth for shared user-facing
// microcopy in the web UI, plus the renderers the regression guards check.
// Loaded before every feature script (see server/mod.rs ASSETS order).
//
// GUARD BASELINE — COPY_GATE_EXCEPTIONS must stay EMPTY (or only ever shrink).
// Appending here is the documented escape hatch (wiki: cxa-b191-microcopy-
// guardrails); every entry needs an owner, a reason and a linked ticket.
window.COPY_GATE_EXCEPTIONS = [];

// The copy catalog: one entry per shared string. `text` returns a template
// with LITERAL {token} placeholders (no JS interpolation — copyText fills
// them from params) and `params` documents every token the template uses.
// The placeholder guard (copy_catalog_b191.rs) verifies template tokens ==
// documented params on this exact file; a mismatch fails the build.
window.CXACOPY = {
  "loading.generic": { params: [], label: "generic loading skeleton", text: () => `loading…` },
  "loading.river":   { params: [], label: "activity river boot line", text: () => `loading activity…` },
  "loading.inbox":   { params: [], label: "inbox panel skeleton",     text: () => `loading inbox…` },
  "loading.team":    { params: [], label: "team panels skeleton",     text: () => `loading team…` },
  "empty.generic":   { params: [], label: "generic empty state",      text: () => `nothing here yet` },
  "empty.inbox":     { params: ["kind"], label: "inbox empty state",  text: () => `no {kind} items — new ones land here automatically` },
  "empty.search":    { params: ["query"], label: "search empty state", text: () => `no results for “{query}” — try a shorter query` },
  "error.load":      { params: ["what"], label: "panel load failure", text: () => `could not load {what} — retry or check the server logs` },
  "toast.saved":     { params: ["what"], label: "generic saved confirmation", text: () => `{what} saved` },
  "toast.deleted":   { params: ["what"], label: "generic deleted confirmation", text: () => `{what} deleted` },
  "toast.long":      { params: ["what", "id"], label: "long-value toast (guard stress fixture)", text: () => `{what} · ref {id}` },
  "action.retry":    { params: [], label: "retry button label",   text: () => `Retry` },
  "action.dismiss":  { params: [], label: "dismiss button label", text: () => `Dismiss` }
};

// Fixed skeleton kinds (CXA-B163 invariant): every skeleton span in the DOM
// must carry one of these data-copy-kind values.
window.COPY_SKELETON_KINDS = ["generic", "river", "inbox", "team"];

// Resolve a catalog key: catalog text, then visible broken marker — never a
// silent empty string. Fills every {token} present in params; an unfilled
// token stays visible so the guards (and eyes) catch it.
window.copyText = function(key, params){
  const entry = window.CXACOPY[key];
  if(!entry || typeof entry.text !== "function") return "_copy.broken";
  let out;
  try { out = entry.text(params || {}); } catch(_e){ return "_copy.broken"; }
  if(typeof out !== "string" || !out.length) return "_copy.broken";
  return out.replace(/\{([a-z_]+)\}/g, function(_, token){
    const v = params ? params[token] : undefined;
    return (v === undefined || v === null) ? "{" + token + "}" : String(v);
  });
};

// Load/refresh skeleton state (CXA-B163): a terminal-state span with a fixed
// data-copy-kind. `where` names the calling surface for guard messages.
window.skeletonFor = function(kind, where){
  const el = document.createElement("span");
  el.className = "skel";
  el.setAttribute("data-copy-kind", String(kind));
  el.setAttribute("data-copy-where", String(where || "unattributed"));
  el.textContent = window.copyText("loading." + (window.COPY_SKELETON_KINDS.indexOf(kind) >= 0 ? kind : "generic"));
  return el;
};

// Toast via the copy layer: the message enters the DOM as text (never markup),
// announced to assistive tech (role=status, aria-live=polite).
window.toastCopy = function(key, params, opts){
  const host = document.getElementById("toasts");
  const o = opts || {};
  const t = document.createElement("div");
  t.className = "toast" + (o.type ? " " + o.type : "");
  t.setAttribute("role", "status");
  t.setAttribute("aria-live", "polite");
  if(o.where) t.setAttribute("data-copy-where", String(o.where));
  const body = document.createElement("div");
  body.className = "t-msg";
  body.textContent = window.copyText(key, params); // text node — long/adversarial values are inert
  t.appendChild(body);
  (host || document.body).appendChild(t);
  setTimeout(function(){ if(t.parentNode) t.parentNode.removeChild(t); }, 6000);
  return t;
};

// Overflow fallback via the copy layer: a text-overflow cell renders the
// resolved text and MUST carry the full string as title + aria-label, so
// truncation never hides information from keyboard or screen-reader users.
window.ellipsisCopy = function(key, params, where){
  const full = window.copyText(key, params);
  const s = document.createElement("span");
  s.className = "tof";
  s.style.cssText = "display:block;white-space:nowrap;overflow:hidden;text-overflow:ellipsis";
  if(where) s.setAttribute("data-copy-where", String(where));
  s.title = full;                 // hover tooltip = full value
  s.setAttribute("aria-label", full); // AT = full value
  s.textContent = full;
  return s;
};

// Copy-layer integrity check over the live document: an empty result is the
// green state; every entry names a surface (data-copy-where) and a reason.
window.__copyIntegrityCheck = function(){
  const bad = [];
  document.querySelectorAll("span.tof").forEach(function(el){
    if(!el.getAttribute("title")) bad.push("ellipsis span missing its accessible title (at " + (el.getAttribute("data-copy-where") || "unknown surface") + ")");
  });
  document.querySelectorAll(".toast").forEach(function(el){
    if(el.getAttribute("role") !== "status" || el.getAttribute("aria-live") !== "polite")
      bad.push("toast missing role=status/aria-live=polite (at " + (el.getAttribute("data-copy-where") || "unknown surface") + ")");
  });
  document.querySelectorAll("span.skel").forEach(function(el){
    const k = el.getAttribute("data-copy-kind");
    if(!k || window.COPY_SKELETON_KINDS.indexOf(k) < 0)
      bad.push("skeleton span without a fixed data-copy-kind (at " + (el.getAttribute("data-copy-where") || "unknown surface") + ")");
  });
  return bad;
};
