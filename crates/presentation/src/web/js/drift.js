// Drift alerts (CXA-F226): the operator surface for architecture-conformance
// violations. One entry per open alert — the violating area, the violation
// message, and a shortcut to the filed bug — under an aggregate header that
// always shows the open count. Zero renders as zero, never hidden: "no drift"
// must read as "scanned and clean", not "not scanned". Data rides the
// serialized state (s.drift_alerts), the same surface as engine_incidents.
function renderDriftAlerts(s){
  const el=document.getElementById("ov-drift");if(!el)return;
  const alerts=(s&&s.drift_alerts)||[];
  const n=alerts.length;
  const badge=`<span style="font-size:11px;font-weight:700;padding:2px 8px;border-radius:20px;margin-left:8px;background:color-mix(in srgb,${n?'var(--red)':'var(--green)'} 15%,transparent);color:${n?'var(--red)':'var(--green)'}">${n} open</span>`;
  let html=`<div class="panel" style="margin-bottom:12px;padding:10px 14px;display:flex;align-items:center;gap:10px;${n?'border-color:var(--red)':''}">
    <i class="ti ti-${n?'git-branch':'shield-check'}" style="font-size:18px;color:${n?'var(--red)':'var(--green)'}"></i>
    <div style="flex:1;font-size:13px;font-weight:600">Architecture drift${badge}</div>
    <div style="font-size:12px;color:var(--muted)">${n?"the codebase diverges from the declared stack — each entry links its filed bug":"no open drift alerts — all scanned areas conform to the declared stack"}</div></div>`;
  for(const a of alerts){
    // The entry heading is the filed bug's own title ("Architecture drift in
    // server") read from state — one source of truth for the wording.
    const t=a.ticket?(STATE.tickets||[]).find(x=>x.id===a.ticket):null;
    html+=`<div class="panel" style="margin-bottom:8px;padding:10px 14px;border-left:3px solid var(--red)">
      <div style="display:flex;align-items:center;gap:10px">
        <div style="flex:1;min-width:0">
          <div style="font-size:12.5px;font-weight:600">${esc(t?t.title:"Architecture drift in "+a.area)}</div>
          <div style="font-size:12px;color:var(--muted);margin-top:2px">${esc(a.message)}</div></div>
        ${a.ticket?`<button class="tk-btn" onclick="showTicket('${esc(a.ticket)}')">${esc(a.ticket)}</button>`:""}
      </div></div>`;
  }
  setHTML(el,html);
}
