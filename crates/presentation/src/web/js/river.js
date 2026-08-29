// Fleet river (CXA-F233): ONE live SSE stream of every agent across every
// registered project — hub-level view, no PID. New activity appears as the
// agents work; events carrying the human-hold flag are marked so an operator
// can intercept them now instead of after the ledger compiles.
// Split from index.html — classic script, one shared scope, load order matters.
const RIVER_MAX_ROWS=80;
let RIVER_ES=null,RIVER_PROJECTS=[],RIVER_STATES={},RIVER_FPROJ="",RIVER_FPHASE="";
function riverUrl(){
  const q=[];
  if(RIVER_FPROJ)q.push("project_id="+encodeURIComponent(RIVER_FPROJ));
  if(RIVER_FPHASE)q.push("phase="+encodeURIComponent(RIVER_FPHASE));
  return "/api/fleet/river"+(q.length?"?"+q.join("&"):"");
}
function riverLive(on){
  const d=document.getElementById("river-live");if(d)d.className="dot "+(on?"live":"off");
}
function closeFleetRiver(){if(RIVER_ES){try{RIVER_ES.close();}catch(_){}RIVER_ES=null;}}
function openFleetRiver(){
  closeFleetRiver();
  renderRiverShell();
  riverLive(false);
  const es=new EventSource(riverUrl());RIVER_ES=es;
  es.onmessage=e=>{let d=null;try{d=JSON.parse(e.data);}catch(_){return;}handleRiverEvent(d);};
  es.onerror=()=>{setConn(false);riverLive(false);};
}
function setRiverProj(v){RIVER_FPROJ=v||"";openFleetRiver();}
function setRiverPhase(v){RIVER_FPHASE=v||"";openFleetRiver();}
function renderRiverShell(){
  const el=document.getElementById("river-body");if(!el)return;
  const phases=[["","All phases"]].concat(AGENTS.map(a=>[a[0],a[0]]));
  el.innerHTML=`<div class="panel" style="display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin-bottom:12px">
      <span class="dot off" id="river-live"></span>
      <b style="font-size:13px">Live agent activity</b>
      <span style="font-size:11px;color:var(--dim)">every project · every agent · real-time</span>
      <span style="flex:1"></span>
      <select class="river-sel" onchange="setRiverProj(this.value)">
        <option value="">All projects</option>
        ${RIVER_PROJECTS.map(p=>`<option value="${esc(p.id)}"${RIVER_FPROJ===p.id?" selected":""}${p.broken?" disabled":""}>${esc(p.name||p.id)}${p.broken?" (broken)":""}</option>`).join("")}
      </select>
      <select class="river-sel" onchange="setRiverPhase(this.value)">
        ${phases.map(([v,l])=>`<option value="${esc(v)}"${RIVER_FPHASE===v?" selected":""}>${esc(l)}</option>`).join("")}
      </select>
    </div>
    <div class="panel" id="river-strip"><div class="empty">connecting…</div></div>
    <div class="panel" id="river-feed"><div class="empty">connecting…</div></div>`;
}
function handleRiverEvent(d){
  if(d.type==="hello"){
    RIVER_PROJECTS=Array.isArray(d.projects)?d.projects:[];
    RIVER_STATES={};
    renderRiverShell();renderRiverStrip();
    riverLive(true);
    // A fleet whose every registration is broken has nothing to stream into
    // the feed — say so instead of leaving a "connecting…" that never lands.
    if(!RIVER_PROJECTS.some(p=>!p.broken)){
      const feed=document.getElementById("river-feed");
      if(feed)feed.innerHTML='<div class="empty">no live agents to stream — fix the broken configs above</div>';
    }
    setConn(true);
  }else if(d.type==="empty"){
    // The friendly empty-state payload (an empty fleet, or a filter that
    // excludes everything) — a renderable prompt, never an error.
    const feed=document.getElementById("river-feed");if(!feed)return;
    feed.innerHTML=`<div class="riv-empty"><i class="ti ti-waves"></i>
      <div class="riv-empty-t">${esc(d.message||"Nothing is flowing right now")}</div>
      <div class="riv-empty-s">Add a project from Home, or clear the filters above — the river starts on its own.</div></div>`;
    const strip=document.getElementById("river-strip");if(strip)strip.innerHTML="";
    riverLive(true);setConn(true);
  }else if(d.type==="project_broken"){
    // A registered project that failed to load keeps its reason marker in the
    // fleet view — visible, actionable, never silently dropped (COX-B043).
    RIVER_STATES[d.project_id]={name:d.name,alias:"",broken:true,error:d.error,config_path:d.config_path};
    renderRiverStrip();
  }else if(d.type==="project_state"){
    RIVER_STATES[d.project_id]={name:d.name,alias:d.alias,runner:d.runner,needs_human:!!d.needs_human,insufficient:!!d.insufficient_data,viewers:d.viewers};
    renderRiverStrip();
    riverLive(true);
    setConn(true);
  }else if(d.type==="agent_activity"){
    riverRow(d);
  }
}
function riverPhaseLabel(s){
  const r=(s&&s.runner)||{};
  if(r.active_role)return esc(r.active_role.replace(/_/g,"-"))+(r.active_note?` · ${esc(r.active_note)}`:"");
  return r.mode==="running"?`cycle ${r.cycle||0}`:esc(r.mode||"idle");
}
// The per-project strip: one card per included project — its live runner
// phase, who is online, and the two flags the river derives for it.
function renderRiverStrip(){
  const strip=document.getElementById("river-strip");if(!strip)return;
  const known=RIVER_PROJECTS.length?RIVER_PROJECTS:Object.keys(RIVER_STATES).map(id=>({id,name:RIVER_STATES[id].name,alias:RIVER_STATES[id].alias}));
  if(!known.length){strip.innerHTML='<div class="empty">no projects in this view</div>';return;}
  strip.innerHTML=known.map(p=>{
    const s=RIVER_STATES[p.id];
    if(s&&s.broken)return `<div class="riv-proj" title="${escAttr((s.error||"invalid config")+" — "+(s.config_path||""))}">
      <span class="sdot" style="background:var(--card2)"></span>
      <b class="riv-pname" style="color:var(--muted)">${esc(p.name||p.id)}</b>
      <span class="riv-alert"><i class="ti ti-alert-triangle"></i> NOT LOADED</span>
      <span class="riv-phase">${esc(s.error||"invalid config")}</span>
      <span style="flex:1"></span><span class="riv-insuff">fix the config, then restart the hub</span>
    </div>`;
    const live=s&&s.runner&&s.runner.mode==="running";
    const flags=[];
    if(s&&s.needs_human)flags.push('<span class="riv-alert"><i class="ti ti-hand-stop"></i> HUMAN ACTION NEEDED</span>');
    if(s&&s.insufficient)flags.push('<span class="riv-insuff" title="fewer than two cycles completed">(insufficient data)</span>');
    const online=s&&Array.isArray(s.online)?s.online:[];
    return `<div class="riv-proj">
      <span class="sdot" style="background:${projColor(p.id)}"></span>
      <b class="riv-pname" title="${esc(p.name||p.id)}">${esc(p.name||p.id)}</b>
      ${p.alias?`<span class="riv-alias">${esc(p.alias)}</span>`:""}
      <span class="dot ${live?"live":"off"}"></span>
      <span class="riv-phase">${s?riverPhaseLabel(s):"…"}</span>
      ${flags.join("")}
      <span style="flex:1"></span>
      ${online.length?`<span class="riv-online" title="${escAttr(online.join(", "))}"><i class="ti ti-eye"></i> ${online.length}</span>`:""}
    </div>`;}).join("");
}
// One activity row, newest on top. Human-hold events are visually distinct
// (badge + border, not colour alone); routine progress rows stay quiet.
function riverRow(d){
  const feed=document.getElementById("river-feed");if(!feed)return;
  const first=feed.querySelector(".riv-empty, .empty");if(first)first.remove();
  const e=d.entry||{};
  const p=(RIVER_PROJECTS.find(x=>x.id===d.project_id)||{name:d.project_id});
  const col=cvar(AC[e.agent]||"--muted");
  const row=document.createElement("div");
  row.className="riv-row"+(d.needs_human?" riv-needs":"");
  row.innerHTML=`<div class="tl-node" style="--nc:${col}"><i class="ti ti-${actIcon(e.action)}"></i></div>
    <div class="tl-body"><div class="tl-line">
      <span class="riv-who" style="color:${col}">${esc(e.agent||"—")}</span>
      <span class="tl-act">${esc(e.action||"")}</span>
      ${e.ticket?`<span class="tk">${esc(e.ticket)}</span>`:""}
      <span class="riv-projtag"><span class="sdot" style="background:${projColor(d.project_id)}"></span>${esc(p.name||p.id)}</span>
      ${d.needs_human?'<span class="riv-alert"><i class="ti ti-hand-stop"></i> HUMAN ACTION NEEDED</span>':""}
    </div>
    <div class="tl-t">${esc(relTime(e.at))}${d.insufficient_data?' · <span class="riv-insuff">(insufficient data)</span>':""}</div></div>`;
  feed.prepend(row);
  while(feed.children.length>RIVER_MAX_ROWS)feed.removeChild(feed.lastChild);
}
