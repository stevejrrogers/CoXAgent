// CXA-F420b [2/2] — Settings → Services: the Add-repo wizard (spec §3–§4).
// The machine does the homework (scan → proposal), the human edits and
// approves, the probe earns `active`. Classic script, shared scope: loaded
// before shell.js, renders into #services-panel inside the Settings view.

async function renderServicesPanel(){
  const el=document.getElementById("services-panel");if(!el)return;
  el.innerHTML='<div class="panel" style="color:var(--muted)">Loading services…</div>';
  let data={services:[]};
  try{const r=await fetch(api("/services"));if(r.ok)data=await r.json();}catch(e){}
  const rows=(data.services||[]).map((s,ix)=>{
    const st=s.status||"active";
    const col=st==="active"?"var(--green)":st==="needs-setup"?"var(--amber)":"var(--muted)";
    return `<div class="toolrow">
      <i class="ti ti-${st==="active"?"circle-check-filled":st==="needs-setup"?"alert-circle":"player-pause"}" style="color:${col};font-size:17px"></i>
      <div class="toolinfo"><div><b>${esc(s.name)}</b>
        <span class="tooldim">· ${esc(st)}${s.scopes&&s.scopes.length?` · scopes: ${esc(s.scopes.join(", "))}`:""}</span></div>
        <code class="toolpath">${esc(s.repo||"(project repo)")} → ${esc(s.work_dir||"(project work_dir)")}</code>
        ${s.build_cmd||s.test_cmd?`<div class="tooldim" style="font-size:11.5px">build: <code>${esc(s.build_cmd||"—")}</code> · test: <code>${esc(s.test_cmd||"—")}</code></div>`:""}
      </div>${ix===0?'<span class="tooldim" title="the first service is the default home for untagged tickets">default</span>'
        :st==="active"?`<button class="gc-btn" onclick="setServiceStatus('${esc(s.name)}','paused')" title="keep tickets, stop assignment"><i class="ti ti-player-pause"></i> Pause</button>`
        :st==="paused"||st==="needs-setup"?`<button class="gc-btn" onclick="setServiceStatus('${esc(s.name)}','active')"><i class="ti ti-player-play"></i> Resume</button>
         <button class="gc-btn" onclick="setServiceStatus('${esc(s.name)}','archived')" title="stop assignment and mark for cleanup"><i class="ti ti-archive"></i> Archive</button>`
        :`<button class="gc-btn" onclick="setServiceStatus('${esc(s.name)}','active')"><i class="ti ti-player-play"></i> Unarchive</button>`}
      </div>`;}).join("");
  el.innerHTML=`
    <div class="toolcard">
      <div class="toolhead"><i class="ti ti-stack-2" style="color:var(--accent2)"></i> Services
        <span class="tooldim" style="font-weight:400">· the repos and folders this project's agents own</span></div>
      <div class="toolgrid">${rows||'<div class="tooldim" style="padding:8px">No services yet — the project\'s own repo is the implicit default.</div>'}</div>
      <div class="toolnote"><i class="ti ti-info-circle"></i> A newly activated service gets its worker on the next hub restart.</div>
    </div>
    <div class="panel frm" style="margin-top:12px">
      <div style="font-weight:700;margin-bottom:6px"><i class="ti ti-git-branch" style="color:var(--accent2)"></i> Add a repo</div>
      <div class="fr"><span class="lbl">Git URL</span><input id="svc-url" placeholder="git@github.com:org/repo.git or https://…" style="flex:1;max-width:420px"/></div>
      <div class="fr"><span class="lbl">…or local path</span><input id="svc-path" placeholder="/path/to/existing/clone" style="flex:1;max-width:420px"/></div>
      <button class="gc-btn pri" id="svc-scan-btn" onclick="scanService()" style="margin-top:6px;align-self:flex-start"><i class="ti ti-radar-2"></i> Scan &amp; propose</button>
      <div id="svc-proposal"></div>
      <div id="svc-result" style="margin-top:6px;font-size:12.5px"></div>
    </div>
    <div class="panel frm" style="margin-top:12px">
      <div style="font-weight:700;margin-bottom:4px"><i class="ti ti-tags" style="color:var(--accent2)"></i> Retag suggestions
        <span class="tooldim" style="font-weight:400">· untagged backlog that looks like it belongs to another service</span></div>
      <button class="gc-btn" onclick="loadRetagSuggestions()" style="align-self:flex-start"><i class="ti ti-radar-2"></i> Suggest</button>
      <div id="svc-retag"></div>
    </div>
    <div class="panel frm" style="margin-top:12px">
      <div style="font-weight:700;margin-bottom:4px"><i class="ti ti-users" style="color:var(--accent2)"></i> Member service scope
        <span class="tooldim" style="font-weight:400">· who works on which service — empty = all (CXA-F420f)</span></div>
      <div id="svc-assign"></div>
    </div>`;
  renderServiceAssignments(data.services||[]);
}

async function renderServiceAssignments(services){
  const el=document.getElementById("svc-assign");if(!el)return;
  let asg={},members=[];
  try{const r=await fetch(api("/services/assignments"));if(r.ok)asg=(await r.json()).assignments||{};}catch(e){}
  try{const r=await fetch(api("/members"));if(r.ok)members=(await r.json()).filter(m=>m.assigned);}catch(e){}
  if(!members.length){el.innerHTML='<div class="tooldim">No project members (open mode, or none assigned) — scope applies only with accounts.</div>';return;}
  const names=services.map(s=>s.name);
  el.innerHTML=members.map(m=>{
    const mine=asg[m.username]||[];
    return `<div style="display:flex;align-items:center;gap:10px;font-size:12.5px;padding:3px 0;flex-wrap:wrap">
      <b style="min-width:120px">${esc(m.username)}</b>
      ${names.map(n=>`<label style="display:flex;align-items:center;gap:4px">
        <input type="checkbox" data-u="${esc(m.username)}" data-s="${esc(n)}" ${mine.includes(n)?"checked":""}
          onchange="saveServiceAssignment('${esc(m.username)}')"/> ${esc(n)}</label>`).join("")}
      <span class="tooldim">${mine.length?``:`· all services`}</span></div>`;
  }).join("");
}

async function saveServiceAssignment(user){
  const picked=[...document.querySelectorAll(`#svc-assign input[data-u="${CSS.escape(user)}"]`)]
    .filter(c=>c.checked).map(c=>c.dataset.s);
  try{
    const r=await fetch(api(`/services/assignments/${encodeURIComponent(user)}`),{method:"PUT",headers:{"Content-Type":"application/json"},body:JSON.stringify({services:picked})});
    const b=await r.json().catch(()=>({}));
    if(!r.ok){toasty(b.error||"save failed","err");}
    else{toasty(`${user}: ${picked.length?picked.join(", "):"all services"}`);}
  }catch(e){toasty(copyText("error.network",{where:"service assignment"}),"err");}
}

// Personal default lens (CXA-F420f): a scoped member's board opens on THEIR
// services. Soft scope — one click on "show all" (or any chip) widens it.
window.MYSVCS=window.MYSVCS||[];
(async function initMyServiceScope(){
  for(let i=0;i<50&&!(window.ME&&typeof api==="function");i++){await new Promise(r=>setTimeout(r,200));}
  if(!window.ME||!ME.username)return;
  try{
    const r=await fetch(api("/services/assignments"));
    if(!r.ok)return;
    const asg=(await r.json()).assignments||{};
    const mine=asg[ME.username]||[];
    if(mine.length){window.MYSVCS=mine;if(typeof renderActive==="function")renderActive();}
  }catch(e){}
})();
function toggleMyScope(){window.MYSVCS_OFF=!window.MYSVCS_OFF;renderActive();}

async function loadRetagSuggestions(){
  const el=document.getElementById("svc-retag");
  el.innerHTML='<div class="tooldim">Scanning…</div>';
  let b={suggestions:[]};
  try{const r=await fetch(api("/services/retag-suggestions"));if(r.ok)b=await r.json();}catch(e){}
  const sg=b.suggestions||[];
  if(!sg.length){el.innerHTML='<div class="tooldim" style="padding:6px 0">No suggestions — every eligible ticket already sits where it belongs (or evidence is ambiguous, which stays a human call).</div>';return;}
  el.innerHTML=`<div style="display:flex;flex-direction:column;gap:4px;margin-top:8px">${sg.map((x,i)=>`
    <label style="display:flex;align-items:center;gap:8px;font-size:12.5px">
      <input type="checkbox" class="retag-ck" data-i="${i}" checked/>
      <b>${esc(x.ticket)}</b> → <span class="tk">${esc(x.to)}</span>
      <span class="tooldim">· ${esc(x.why)}</span></label>`).join("")}
    <button class="gc-btn pri" onclick="applyRetags()" style="align-self:flex-start;margin-top:6px"><i class="ti ti-check"></i> Apply ticked</button></div>`;
  window._retagSuggestions=sg;
}

async function applyRetags(){
  const sg=window._retagSuggestions||[];
  const picked=[...document.querySelectorAll(".retag-ck")].filter(c=>c.checked).map(c=>sg[Number(c.dataset.i)]).filter(Boolean)
    .map(x=>({ticket:x.ticket,to:x.to}));
  if(!picked.length){toasty(copyFor("toast.nothingPicked",{surface:"retag suggestions"}),"err");return;}
  try{
    const r=await fetch(api("/services/retag"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({assignments:picked})});
    const b=await r.json().catch(()=>({}));
    if(!r.ok){toasty(b.error||"retag failed","err");return;}
    toasty(`Retagged ${(b.applied||[]).length} ticket(s)${(b.refused||[]).length?` · ${b.refused.length} refused`:""}`);
    if((b.refused||[]).length)console.warn("retag refused:",b.refused);
  }catch(e){toasty(copyText("error.network",{where:"retag"}),"err");}
  loadRetagSuggestions();
}

async function scanService(){
  const btn=document.getElementById("svc-scan-btn");
  const repo=document.getElementById("svc-url").value.trim();
  const path=document.getElementById("svc-path").value.trim();
  const out=document.getElementById("svc-result");
  if(!repo&&!path){out.innerHTML='<span style="color:var(--amber)">Give a git URL or a local path first.</span>';return;}
  btn.disabled=true;btn.innerHTML='<i class="ti ti-loader-2"></i> Scanning… (a URL is cloned first)';
  try{
    const r=await fetch(api("/services/scan"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({repo,path})});
    const b=await r.json().catch(()=>({}));
    if(!r.ok){out.innerHTML=`<span style="color:var(--red)">${esc(b.error||b||"scan failed")}</span>`;return;}
    const p=b.proposal||{};
    // The proposal card: every field editable — AI proposes, the human decides.
    document.getElementById("svc-proposal").innerHTML=`
      <div class="panel" style="margin-top:10px;border-color:var(--accent2)">
        <div style="font-weight:600;margin-bottom:4px"><i class="ti ti-sparkles" style="color:var(--accent2)"></i> Proposed service
          ${(p.languages||[]).map(l=>`<span class="tk" style="margin-left:4px">${esc(l)}</span>`).join("")}
          ${p.has_dockerfile?'<span class="tk" style="margin-left:4px">dockerfile</span>':''}</div>
        ${(p.notes||[]).map(n=>`<div style="font-size:12px;color:var(--amber)"><i class="ti ti-info-circle"></i> ${esc(n)}</div>`).join("")}
        <div class="fr"><span class="lbl">Name</span><input id="svcp-name" value="${esc(p.name||"")}" style="width:180px"/><span class="hint">tickets carry this as their service tag</span></div>
        <div class="fr"><span class="lbl">Build cmd</span><input id="svcp-build" value="${esc(p.build_cmd||"")}" style="flex:1;max-width:420px"/></div>
        <div class="fr"><span class="lbl">Test cmd</span><input id="svcp-test" value="${esc(p.test_cmd||"")}" style="flex:1;max-width:420px"/></div>
        <div class="fr"><span class="lbl">Scopes</span><input id="svcp-scopes" value="${esc((p.scopes||[]).join(", "))}" placeholder="monorepo folders, comma-separated — empty = whole repo" style="flex:1;max-width:420px"/></div>
        <input type="hidden" id="svcp-repo" value="${esc(b.repo||"")}"/>
        <input type="hidden" id="svcp-dir" value="${esc(b.work_dir||"")}"/>
        <div style="font-size:12px;color:var(--muted)">Clone: <code>${esc(b.work_dir||"")}</code></div>
        <button class="gc-btn pri" id="svc-add-btn" onclick="addService()" style="margin-top:8px;align-self:flex-start"><i class="ti ti-rocket"></i> Probe &amp; add</button>
        <span class="hint">runs build + test on the clean tree first — green becomes <b>active</b>, red is filed as <b>needs-setup</b> with the error</span>
      </div>`;
    out.innerHTML="";
  }catch(e){out.innerHTML='<span style="color:var(--red)">Network error during scan.</span>';}
  finally{btn.disabled=false;btn.innerHTML='<i class="ti ti-radar-2"></i> Scan &amp; propose';}
}

async function addService(){
  const btn=document.getElementById("svc-add-btn");
  const out=document.getElementById("svc-result");
  const body={
    name:document.getElementById("svcp-name").value.trim(),
    repo:document.getElementById("svcp-repo").value.trim(),
    work_dir:document.getElementById("svcp-dir").value.trim(),
    build_cmd:document.getElementById("svcp-build").value.trim(),
    test_cmd:document.getElementById("svcp-test").value.trim(),
    scopes:document.getElementById("svcp-scopes").value.split(",").map(s=>s.trim()).filter(Boolean),
    probe:true,
  };
  btn.disabled=true;btn.innerHTML='<i class="ti ti-loader-2"></i> Probing (build + test — can take minutes)…';
  try{
    const r=await fetch(api("/services"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
    const b=await r.json().catch(()=>({}));
    if(!r.ok){out.innerHTML=`<span style="color:var(--red)">${esc(b.error||b||"add failed")}</span>`;return;}
    if(b.status==="active"){
      out.innerHTML=`<span style="color:var(--green)"><i class="ti ti-circle-check"></i> <b>${esc(b.name)}</b> probed green and is <b>active</b>. ${esc(b.note||"")}</span>`;
    }else{
      const perr=b.probe&&b.probe.error?`<pre style="white-space:pre-wrap;font-size:11px;max-height:140px;overflow:auto;background:color-mix(in srgb,var(--red) 7%,transparent);padding:6px;border-radius:6px">${esc(b.probe.error)}</pre>`:"";
      out.innerHTML=`<span style="color:var(--amber)"><i class="ti ti-alert-circle"></i> <b>${esc(b.name)}</b> filed as <b>needs-setup</b> — ${esc(b.probe&&b.probe.failed?b.probe.failed+" failed":"probe skipped")}.</span>${perr}`;
    }
    renderServicesPanel();
  }catch(e){out.innerHTML='<span style="color:var(--red)">Network error — the probe may still be running; reopen Settings → Services to check.</span>';}
  finally{if(btn){btn.disabled=false;btn.innerHTML='<i class="ti ti-rocket"></i> Probe &amp; add';}}
}

async function setServiceStatus(name,status){
  try{
    const r=await fetch(api(`/services/${encodeURIComponent(name)}/status`),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({status})});
    const b=await r.json().catch(()=>({}));
    if(!r.ok){toasty(b.error||b||"status change failed","err");}
    else{toasty(`${name}: ${b.from} → ${b.status}. ${b.note||""}`);}
  }catch(e){toasty(copyText("error.network",{where:"service status"}),"err");}
  renderServicesPanel();
}
