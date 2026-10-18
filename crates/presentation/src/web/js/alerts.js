// Outbound alert delivery history (CXA-F235): what the runner spooled to the
// project webhook, what the receiver acknowledged, what is retrying with
// backoff, and what died — with one-click replay for dead alerts. Rendered in
// the Activity view; data comes from the runner's durable outbox spool.
let ALERTS_AT = 0, ALERTS_CACHE = [];

function alertsApi(p){return "/api/projects/"+encodeURIComponent(PID)+p;}

function alertStatusBadge(e){
  if(e.status==="delivered")return `<span style="font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px;padding:2px 8px;border-radius:20px;background:color-mix(in srgb,var(--green) 14%,transparent);color:var(--green)">delivered</span>`;
  if(e.status==="dead")return `<span style="font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px;padding:2px 8px;border-radius:20px;background:color-mix(in srgb,var(--red) 14%,transparent);color:var(--red)">failed</span>`;
  if(e.retrying)return `<span style="font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px;padding:2px 8px;border-radius:20px;background:color-mix(in srgb,var(--amber) 14%,transparent);color:var(--amber)">retrying · ${e.attempts}</span>`;
  return `<span style="font-size:10px;font-weight:700;text-transform:uppercase;letter-spacing:.4px;padding:2px 8px;border-radius:20px;background:color-mix(in srgb,var(--teal) 14%,transparent);color:var(--teal)">pending</span>`;
}

function alertRow(e){
  const when=(e.created_at?new Date(e.created_at*1000):null);
  const at=when&&!isNaN(when)?when.toISOString().slice(0,16).replace("T"," "):"";
  const replay=e.status==="dead"
    ?`<button class="btn-ghost" style="padding:4px 10px;font-size:11px" onclick="replayAlert(${e.id})"><i class="ti ti-refresh"></i> Replay</button>`
    :"";
  return `<div style="display:flex;align-items:center;gap:10px;padding:8px 4px;border-bottom:1px solid var(--border)">
    <div style="flex:1;min-width:0">
      <div style="font-size:12.5px;font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${esc(e.message)}</div>
      <div style="font-size:11px;color:var(--dim);font-family:ui-monospace,Menlo,monospace">${esc(e.kind)} · id ${e.id}${at?" · "+at:""}</div>
    </div>
    ${alertStatusBadge(e)}${replay}
  </div>`;
}

function renderAlerts(){
  const el=document.getElementById("alerts-body");
  if(!el||!PID)return;
  // The live refresh repaints every second; the fetch itself throttles to 3s.
  if(Date.now()-ALERTS_AT<3000){paintAlerts();return;}
  ALERTS_AT=Date.now();
  fetch(alertsApi("/alerts")).then(r=>r.ok?r.json():[]).then(rows=>{
    ALERTS_CACHE=Array.isArray(rows)?rows:[];
    paintAlerts();
  }).catch(()=>{ if(el)el.innerHTML='<div class="empty">unable to load alert history</div>'; });
}

function paintAlerts(){
  const el=document.getElementById("alerts-body");
  if(!el)return;
  el.innerHTML=ALERTS_CACHE.length?ALERTS_CACHE.map(alertRow).join("")
    :'<div class="empty">no outbound alerts yet — webhook events appear here with their delivery status</div>';
}

function replayAlert(id){
  fetch(alertsApi("/alerts/"+id+"/replay"),{method:"POST"}).then(r=>{
    if(r.ok){ALERTS_AT=0;renderAlerts();}
  }).catch(()=>{});
}
