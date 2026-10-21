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
    </div>`;
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
  }catch(e){toasty("Network error","err");}
  renderServicesPanel();
}
