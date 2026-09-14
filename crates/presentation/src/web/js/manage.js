// Manage (super admin): spaces, users, usage, audit.
// Split from index.html — classic script, load order matters (one shared scope).
// ---- Manage (super admin): spaces · users · usage ---------------------------
let MG=null;
const ROLE_COLORS={super:"#f59e0b",admin:"#22d3ee",director:"#a78bfa",manager:"#a78bfa",techlead:"#a78bfa",fe:"#34d399",be:"#60a5fa",ba:"#f472b6",viewer:"#6b7280"};
function roleChip(r,n){const c=ROLE_COLORS[r]||"#6b7280";return `<span class="mg-rolechip" style="--rc:${c}">${esc(r)}${n!==undefined?` <b>${n}</b>`:""}</span>`;}
async function renderManage(){
  // Live refresh (SSE/poll) must never clobber what the user is doing: skip
  // the repaint while typing in a Manage input (e.g. the Users search) — the
  // next event after they finish will catch the view up. The space form lives
  // in #sp-modal outside this view, so repaints can't touch it.
  const a=document.activeElement;
  if(a&&(a.tagName==="INPUT"||a.tagName==="TEXTAREA")&&a.closest('[id^="view-mg-"]'))return;
  try{MG=await(await fetch("/api/manage/overview")).json();}catch(e){return;}
  if(CUR==="mg-spaces")renderMgSpaces();
  else if(CUR==="mg-users")renderMgPeople();
  else if(CUR==="mg-usage")renderMgUsage();
  else if(CUR==="mg-fleet")renderMgFleet();
  else if(CUR==="mg-audit")renderMgAudit();
}
// Hub-wide audit trail (Admin/Super): every authenticated mutation, newest first.
async function renderMgAudit(){
  const el=document.getElementById("mg-audit-body");if(!el)return;
  let rows=[];try{const r=await fetch("/api/audit-log");if(r.ok)rows=await r.json();}catch(e){}
  const q=(window._mgaq||"").toLowerCase();
  const list=rows.filter(e=>!q||JSON.stringify(e).toLowerCase().includes(q)).slice(0,300);
  el.innerHTML=`<div class="dmsearch" style="margin-bottom:12px;max-width:340px"><i class="ti ti-search"></i><input placeholder="Filter by user, path, status…" value="${esc(window._mgaq||"")}" oninput="window._mgaq=this.value;renderMgAudit()"></div>
  <div class="panel" style="font-family:ui-monospace,monospace;font-size:12px">${list.map(e=>{
    const bad=(e.status||200)>=400;
    return `<div class="wsrow" style="border:none;border-bottom:1px solid var(--border);border-radius:0;background:transparent;padding:7px 4px;gap:12px">
      <span style="color:var(--dim);min-width:126px">${esc((e.at||e.time||"").slice(0,19).replace("T"," "))}</span>
      <span style="min-width:90px;color:var(--accent2)">${esc(e.user||"?")}</span>
      <span style="min-width:52px;font-weight:700">${esc((e.action||"").split(" ")[0])}</span>
      <span style="flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc((e.action||"").split(" ").slice(1).join(" "))}</span>
      <span style="color:${bad?"var(--red)":"var(--green)"}">${esc(String(e.status||""))}</span></div>`;
  }).join("")||'<div class="empty">no matching audit records</div>'}</div>`;
}
function renderMgSpaces(){
  const el=document.getElementById("mg-spaces-body");if(!el||!MG)return;
  const t=MG.totals||{};
  const stat=(v,l)=>`<div class="wsstat"><b>${v}</b><span>${l}</span></div>`;
  const allP=(MG.spaces||[]).flatMap(s=>s.projects||[]).concat(MG.unassigned_projects||[]);
  el.innerHTML=`
    <div class="wshero" style="margin-bottom:18px"><div class="wsmark"><i class="ti ti-shield-cog"></i></div>
      <div><div class="wsname">Hub control</div><div class="wstag">every space · every project · one screen</div></div>
      <div class="wsstats">${stat(t.projects||0,"Projects")}${stat((MG.spaces||[]).length,"Spaces")}${stat(t.users||0,"Users")}${stat(t.online||0,"Online")}${stat(money(t.spend||0),"Spend")}</div></div>
    <div class="wshead" style="margin-bottom:12px"><span class="wssec" style="margin:0">Spaces</span>
      <button class="pri" onclick="openSpaceModal()"><i class="ti ti-plus"></i> New space</button></div>
    <div class="wsgrid">${(MG.spaces||[]).map(sp=>`
      <div class="wscard mgcard" onclick="openMgSpace('${esc(sp.id)}')">
        <div class="wsc-h"><b>${esc(sp.name)}</b><span class="wsc-v">${esc(sp.id)}</span></div>
        <div class="wstag" style="font-size:12px;min-height:16px">${esc(sp.tagline||"")}</div>
        <div class="mg-admins">${(sp.admins||[]).map(a=>`<span class="wsava" title="space admin: ${esc(a)}">${esc(initials(a))}</span>`).join("")}<span class="mg-adminlbl">${esc((sp.admins||[]).join(", ")||"no admin")}</span></div>
        <div class="mg-roles">${Object.entries(sp.roles||{}).map(([r,n])=>roleChip(r,n)).join("")||'<span class="wsc-off">no members</span>'}</div>
        <div class="wsc-stats"><span>📁 ${(sp.projects||[]).length} projects</span><span>👥 ${sp.members||0}</span><span style="color:${sp.budget_usd&&sp.spend>=sp.budget_usd?"var(--red)":sp.budget_usd&&sp.spend>=sp.budget_usd*.8?"var(--amber)":"var(--amber)"}">🔥 ${money(sp.spend||0)}${sp.budget_usd?` / ${money(sp.budget_usd)}`:""}</span>${sp.budget_usd&&sp.spend>=sp.budget_usd?'<span style="color:var(--red);font-weight:700">⛔ over budget</span>':""}</div>
        <div class="wsc-online">${(sp.online||[]).slice(0,8).map(o=>`<span class="wsava on" title="${esc(o)}">${esc(initials(o))}</span>`).join("")||'<span class="wsc-off">no one online</span>'}</div>
      </div>`).join("")||'<div class="empty">no spaces yet — click New space</div>'}</div>
    ${(MG.unassigned_projects||[]).length?`<div class="panel" style="margin-top:14px;font-size:12.5px;color:var(--muted)"><i class="ti ti-alert-triangle" style="color:var(--amber)"></i> Not in any space: <b>${esc((MG.unassigned_projects||[]).join(", "))}</b> — assign via Edit space.</div>`:""}`;
}
// Space create/edit lives in a modal OUTSIDE the re-rendered view, so the live
// refresh (SSE/3s poll) can repaint the Spaces grid freely without touching it.
let SPM_ID=null; // null = creating; otherwise the space id being edited
let SPM_LITE=null; // set = space-admin lite edit (name/tagline only, from Home)
// Lite edit for space admins from Home: name + tagline only. Membership stays
// untouched — the PUT carries the space's current admins/projects verbatim.
function openSpaceModalLite(sp){
  SPM_ID=sp.id;SPM_LITE=sp;
  document.getElementById("spm-title").textContent="Edit space — "+sp.name;
  document.getElementById("spm-msg").textContent="Rename / tagline. Members & projects are managed by the Super Admin in Manage.";
  document.getElementById("spm-name").value=sp.name;
  document.getElementById("spm-tag").value=sp.tagline||"";
  document.getElementById("spm-admins-box").closest(".fr").style.display="none";
  document.getElementById("spm-projects-box").closest(".fr").style.display="none";
  document.getElementById("spm-members-row").style.display="none";
  document.getElementById("spm-budget-row").style.display="none";
  document.getElementById("spm-del").hidden=true;
  document.getElementById("spm-ok").innerHTML='<i class="ti ti-check"></i> Save';
  const m=document.getElementById("sp-modal");m.hidden=false;
  m.onkeydown=e=>{if(e.key==="Escape")closeSpaceModal();};m.tabIndex=-1;
  setTimeout(()=>document.getElementById("spm-name").focus(),30);
}
function openSpaceModal(id){
  SPM_ID=id||null;
  const sp=id?(((MG||{}).spaces||[]).find(x=>x.id===id)||null):null;
  if(id&&!sp)return;
  // One project = one space: offer only unassigned projects, plus (when
  // editing) the ones already in THIS space.
  const allP=[...new Set(((MG||{}).unassigned_projects||[]).concat(sp?(sp.projects||[]):[]))];
  document.getElementById("spm-title").textContent=sp?("Edit space — "+sp.name):"New space";
  document.getElementById("spm-msg").textContent=sp?"":"Group projects + appoint a space admin.";
  document.getElementById("spm-name").value=sp?sp.name:"";
  document.getElementById("spm-tag").value=sp?(sp.tagline||""):"";
  document.getElementById("spm-budget").value=sp&&sp.budget_usd?sp.budget_usd:"";
  document.getElementById("spm-budget-row").style.display="";
  document.getElementById("spm-admins-box").innerHTML=mgPicker("spm-admins",((MG||{}).users||[]).map(u=>u.username),sp?(sp.admins||[]):[]);
  document.getElementById("spm-members-box").innerHTML=mgPicker("spm-members",((MG||{}).users||[]).map(u=>u.username),sp?(sp.members||[]):[]);
  document.getElementById("spm-members-row").style.display="";
  document.getElementById("spm-projects-box").innerHTML=mgPicker("spm-projects",allP,sp?(sp.projects||[]):[]);
  document.getElementById("spm-del").hidden=!sp;
  document.getElementById("spm-ok").innerHTML='<i class="ti ti-check"></i> '+(sp?"Save":"Create");
  const m=document.getElementById("sp-modal");m.hidden=false;
  m.onkeydown=e=>{if(e.key==="Escape")closeSpaceModal();};m.tabIndex=-1;
  setTimeout(()=>document.getElementById("spm-name").focus(),30);
}
function closeSpaceModal(){document.getElementById("sp-modal").hidden=true;SPM_ID=null;SPM_LITE=null;
  document.getElementById("spm-admins-box").closest(".fr").style.display="";
  document.getElementById("spm-projects-box").closest(".fr").style.display="";
  document.getElementById("spm-members-row").style.display="";}
async function submitSpaceModal(){
  const name=val("spm-name").trim();if(!name){toasty("Name the space","warn");return;}
  const lite=SPM_LITE;
  const body=JSON.stringify(lite
    ?{name,tagline:val("spm-tag"),admins:lite.admins||[],projects:lite.projects||[],members:lite.members||[],budget_usd:lite.budget_usd||0}
    :{name,tagline:val("spm-tag"),admins:picked("spm-admins"),projects:picked("spm-projects"),members:picked("spm-members"),budget_usd:parseFloat(val("spm-budget"))||0});
  const ok=document.getElementById("spm-ok");ok.disabled=true;
  try{
    const r=await(SPM_ID
      ?fetch("/api/spaces/"+encodeURIComponent(SPM_ID),{method:"PUT",headers:{"Content-Type":"application/json"},body})
      :fetch("/api/spaces",{method:"POST",headers:{"Content-Type":"application/json"},body}));
    if(r.ok){
      toasty(SPM_ID?"Saved":"Space created","ok");
      const id=SPM_ID;closeSpaceModal();
      if(lite){renderHome();}
      else{MG=null;await renderManage();if(id&&CUR==="mg-space")openMgSpace(id);}
    }else toasty("Failed: "+await r.text(),"err");
  }catch(e){toasty("Network error","err");}
  ok.disabled=false;
}
// Checkbox-chip picker over real data — no free-text CSV (typos must be impossible).
function mgPicker(id,items,selected){
  const sel=new Set(selected||[]);
  return `<div class="mg-picker" id="${id}">${items.map(it=>
    `<label class="mg-pick ${sel.has(it)?"on":""}"><input type="checkbox" value="${esc(it)}" ${sel.has(it)?"checked":""} onchange="this.parentElement.classList.toggle('on',this.checked)"><span>${esc(it)}</span></label>`).join("")||'<span class="wsc-off">none available</span>'}</div>`;
}
function picked(id){return Array.from(document.querySelectorAll('#'+id+' input:checked')).map(i=>i.value);}
async function openMgSpace(id){
  nav("mg-space");
  const el=document.getElementById("mg-space-body");if(el)el.innerHTML='<div class="empty">loading…</div>';
  let d=null;try{d=await(await fetch("/api/manage/spaces/"+encodeURIComponent(id))).json();}catch(e){return;}
  renderMgSpaceDetail(d);
}
function renderMgSpaceDetail(d){
  const el=document.getElementById("mg-space-body");if(!el||!d)return;
  const sp=d.space||{},ps=d.projects||[],ms=d.members||[];
  const spend=ps.reduce((a,p)=>a+(p.spend||0),0),tokens=ps.reduce((a,p)=>a+(p.tokens||0),0);
  const shipped=ps.reduce((a,p)=>a+(p.shipped||0),0);
  const stat=(v,l)=>`<div class="wsstat"><b>${v}</b><span>${l}</span></div>`;
  document.getElementById("pg-title").textContent=sp.name||"Space";
  el.innerHTML=`
    <button class="gc-btn" style="margin-bottom:14px" onclick="nav('mg-spaces')"><i class="ti ti-arrow-left"></i> Spaces</button>
    <div class="wshero" style="margin-bottom:18px"><div class="wsmark">${esc(initials(sp.name||"S"))}</div>
      <div><div class="wsname">${esc(sp.name||"")}</div><div class="wstag">${esc(sp.tagline||"")} · admins: ${esc((sp.admins||[]).join(", ")||"—")}</div></div>
      <div class="wsstats">${stat(ps.length,"Projects")}${stat(ms.length,"Members")}${stat(shipped,"Shipped")}${stat(fmtK(tokens),"Tokens")}${stat(money(spend),"Spend")}</div></div>
    <div class="wshead" style="margin-bottom:12px"><span class="wssec" style="margin:0">Projects</span>
      <button class="gc-btn" onclick="openSpaceModal('${esc(sp.id)}')"><i class="ti ti-pencil"></i> Edit space</button></div>
    <div class="wsgrid" style="margin-bottom:20px">${ps.map(p=>{
      const sprint=p.sprint?`<div class="mg-sprint"><div class="mg-sprintbar"><div style="width:${p.sprint.committed?Math.round((p.sprint.done||0)/p.sprint.committed*100):0}%"></div></div><span>S${p.sprint.number}: ${p.sprint.done||0}/${p.sprint.committed||0} · ${esc((p.sprint.goal||"").slice(0,60))}</span></div>`:"";
      return `<div class="wscard" onclick="switchProject('${esc(p.id)}');setMode('workspace');nav('overview')">
        <div class="wsc-h"><b>${esc(p.name||p.id)}</b><span class="wsc-v">v${esc(p.version||"0")}</span></div>
        <div class="wsc-stats"><span>🚀 ${p.shipped||0}</span><span>⚙️ ${p.in_flight||0}</span><span>🐛 ${p.bugs_open||0}</span></div>
        <div class="wsc-stats" style="margin-top:6px"><span>${fmtK(p.tokens||0)} tok</span><span style="color:var(--amber)">${money(p.spend||0)}</span></div>
        ${sprint}
        <div class="wsc-online">${(p.online||[]).map(o=>`<span class="wsava on" title="${esc(o)}">${esc(initials(o))}</span>`).join("")||'<span class="wsc-off">no one online</span>'}</div>
      </div>`;}).join("")||'<div class="empty">no projects in this space — assign via Edit space</div>'}</div>
    <div class="wssec">Members</div>
    <div class="panel">${ms.sort((a,b)=>(b.spend||0)-(a.spend||0)).map(m=>`
      <div class="wsrow"><span class="wsava">${esc(initials(m.name||m.username))}</span>
        <span class="wsproj">${esc(m.name||m.username)} ${m.is_space_admin?'<i class="ti ti-crown" style="color:var(--amber)" title="space admin"></i>':""}</span>
        <span class="wsstate">${roleChip(m.role)}</span>
        <span class="wsspend">${money(m.spend||0)}</span></div>`).join("")||'<div class="empty">no members</div>'}</div>`;
}
async function mgDeleteSpace(id){
  if(!(await coxModal({title:"Delete space "+id+"?",message:"Doesn't delete projects or users — only removes this organizational grouping.",danger:true,confirmText:"Delete"})))return;
  try{await fetch("/api/spaces/"+encodeURIComponent(id),{method:"DELETE"});closeSpaceModal();MG=null;nav("mg-spaces");renderManage();}catch(e){toasty("Network error","err");}}
function renderMgPeople(){
  const el=document.getElementById("mg-people-body");if(!el||!MG)return;
  const q=(window._mgq||"").toLowerCase();
  const sf=(window._mgspace||"");
  const pf=(window._mgproj||"");
  const spaceOf=u=>(MG.spaces||[]).filter(s=>(s.admins||[]).includes(u.username)||(u.projects||[]).some(p=>(s.projects||[]).includes(p))).map(s=>s.name).join(", ");
  const roles=["super","admin","director","manager","techlead","dslead","dalead","ba","po","sa","sm","qa","fe","be","aie","ds","da","de","reviewer","viewer"];
  var list=(MG.users||[]).filter(u=>!q||u.username.toLowerCase().includes(q)||(u.name||"").toLowerCase().includes(q));
  // Filter by space
  if(sf){list=list.filter(u=>spaceOf(u).toLowerCase().includes(sf.toLowerCase()));}
  // Filter by project
  if(pf){list=list.filter(u=>(u.projects||[]).some(function(p){return p===pf||(p||"").toLowerCase()===pf.toLowerCase();}));}
  // Build space & project filter options
  var spaceOpts='<option value="">— all spaces —</option>'+((MG.spaces||[]).map(function(s){return '<option value="'+esc(s.id)+'"'+(sf===s.id?' selected':'')+'>'+esc(s.name||s.id)+'</option>';}).join(""));
  var allPids=new Set();(MG.users||[]).forEach(function(u){(u.projects||[]).forEach(function(p){allPids.add(p);});});
  var projOpts='<option value="">— all projects —</option>'+Array.from(allPids).sort().map(function(p){var nm=(PROJECTS.find(function(x){return x.id===p;})||{}).name||p;return '<option value="'+esc(p)+'"'+(pf===p?' selected':'')+'>'+esc(nm)+'</option>';}).join("");

  el.innerHTML=`<div class="sec" style="display:flex;align-items:center;justify-content:space-between;gap:12px;margin-bottom:14px;flex-wrap:wrap">
    <span style="font-size:14px;font-weight:600;display:flex;align-items:center;gap:8px"><i class="ti ti-users" style="color:var(--accent2)"></i> ${list.length} user${list.length!==1?"s":""}</span>
    <button class="pri add-user-btn" onclick="openInvite()"><i class="ti ti-user-plus"></i> Add user</button>
  </div>
  <div class="sec" style="display:flex;align-items:center;gap:10px;margin-bottom:16px;flex-wrap:wrap">
    <div class="dmsearch" style="max-width:220px;margin:0"><i class="ti ti-search"></i><input placeholder="Find user…" value="${esc(q)}" oninput="window._mgq=this.value;renderMgPeople()"></div>
    <select class="sel" style="max-width:200px;height:36px;font-size:12.5px;padding:0 10px" onchange="window._mgspace=this.value;renderMgPeople()">${spaceOpts}</select>
    <select class="sel" style="max-width:200px;height:36px;font-size:12.5px;padding:0 10px" onchange="window._mgproj=this.value;renderMgPeople()">${projOpts}</select>
  </div>
  <div class="panel">${list.sort(function(a,b){return(b.spend||0)-(a.spend||0);}).map(function(u){
    return `<div class="wsrow"><span class="wsava">${esc(initials(u.name||u.username))}</span>
      <span class="wsproj">${esc(u.name||u.username)} <span style="color:var(--dim);font-weight:400">@${esc(u.username)}</span></span>
      <span class="wsstate">${esc(spaceOf(u)||"no space")} · ${(u.projects||[]).length} projects</span>
      <span class="wsspend" title="total tokens burned">${money(u.spend||0)}</span>
      <select onchange="mgSetRole('${esc(u.username)}',this.value,'${esc(u.name||"")}')" class="mg-rolesel">
        ${roles.map(function(r){return '<option value="'+r+'"'+(u.role===r?' selected':'')+'>'+roleLabel(r)+'</option>';}).join("")}
      </select></div>`;}).join("")||'<div class="empty">no users</div>'}</div>`;
}
async function mgSetRole(username,role,name){
  try{const r=await fetch("/api/auth/users/"+encodeURIComponent(username),{method:"PUT",headers:{"Content-Type":"application/json"},body:JSON.stringify({name,email:"",role})});
    if(r.ok)toasty(username+" → "+role,"ok");else toasty("Failed: "+await r.text(),"err");}catch(e){}
  MG=null;renderManage();
}
function renderMgUsage(){
  const el=document.getElementById("mg-usage-body");if(!el||!MG)return;
  const bar=(n,c,max,color)=>`<div style="display:flex;align-items:center;gap:12px;padding:8px 0"><span style="min-width:150px;font-size:12.5px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(n)}</span>
    <div style="flex:1;background:var(--card2);border-radius:6px;height:8px;overflow:hidden"><div style="width:${Math.max(3,Math.round(c/max*100))}%;height:100%;background:${color}"></div></div>
    <span style="font-size:12px;font-family:ui-monospace,monospace;min-width:64px;text-align:right">${money(c)}</span></div>`;
  const sp=(MG.spaces||[]).map(s=>[s.name,s.spend||0]).sort((a,b)=>b[1]-a[1]);
  const spMax=sp.length?Math.max(sp[0][1],0.01):1;
  const us=(MG.users||[]).map(u=>[u.name||u.username,u.spend||0]).filter(x=>x[1]>0).sort((a,b)=>b[1]-a[1]).slice(0,10);
  const usMax=us.length?Math.max(us[0][1],0.01):1;
  // Depth pass round 3: totals header, share-of-total on every bar, and the
  // full contributor list behind a toggle instead of a silent top-10 cut.
  const spTotal=sp.reduce((a,x)=>a+x[1],0);
  const usAll=(MG.users||[]).map(u=>[u.name||u.username,u.spend||0]).filter(x=>x[1]>0).sort((a,b)=>b[1]-a[1]);
  const usTotal=usAll.reduce((a,x)=>a+x[1],0);
  const shown=window._mgUsageAll?usAll:us;
  const pct=(c,t)=>t>0?" · "+(c/t*100).toFixed(c/t>=0.1?0:1)+"%":"";
  const bar2=(n,c,max,color,t)=>`<div style="display:flex;align-items:center;gap:12px;padding:8px 0"><span style="min-width:150px;font-size:12.5px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(n)}</span>
    <div style="flex:1;background:var(--card2);border-radius:6px;height:8px;overflow:hidden"><div style="width:${Math.max(3,Math.round(c/max*100))}%;height:100%;background:${color}"></div></div>
    <span style="font-size:12px;font-family:ui-monospace,monospace;min-width:104px;text-align:right">${money(c)}<span style="color:var(--dim)">${pct(c,t)}</span></span></div>`;
  const more=usAll.length>us.length;
  el.innerHTML=`<div class="wssec">Spend by space <span style="font-weight:400;color:var(--dim);font-size:11.5px">· ${sp.length} space${sp.length===1?"":"s"} · ${money(spTotal)} total</span></div>
    <div class="panel" style="margin-bottom:18px">${sp.map(([n,c])=>bar2(n,c,spMax,"var(--accent2)",spTotal)).join("")||'<div class="empty">no spend yet — engine calls attribute here as agents run</div>'}</div>
    <div class="wssec">Burners 🔥 <span style="font-weight:400;color:var(--dim);font-size:11.5px">· ${usAll.length} contributor${usAll.length===1?"":"s"} · ${money(usTotal)} total</span></div>
    <div class="panel">${shown.map(([n,c])=>bar2(n,c,usMax,"var(--amber)",usTotal)).join("")||'<div class="empty">no per-user spend yet</div>'}
    ${more?`<div style="text-align:center;padding-top:8px"><button class="fchip" onclick="window._mgUsageAll=!window._mgUsageAll;renderMgUsage()">${window._mgUsageAll?"show top 10":"show all "+usAll.length}</button></div>`:""}</div>`;
}
// ---- Fleet spend cockpit (CXA-F278): cross-project burn · cap headroom · hub
// soft ceiling. Visibility-only: the endpoint pauses nothing, and the ceiling
// merely raises one deduplicated #general alert per day.
const FLEET_STATUS={over:["OVER","var(--red)"],approaching:["80%+","var(--amber)"],ok:["OK","var(--muted)"]};
function fleetBadge(s){const b=FLEET_STATUS[s]||FLEET_STATUS.ok;return `<span style="font-size:10px;font-weight:700;letter-spacing:.04em;text-transform:uppercase;color:${b[1]}">${b[0]}</span>`;}
async function renderMgFleet(){
  const el=document.getElementById("mg-fleet-body");if(!el)return;
  let fleet;
  try{
    const r=await fetch("/api/fleet/spend");
    if(r.status===403){el.innerHTML='<div class="empty">super admin only</div>';return;}
    fleet=await r.json();
  }catch(e){return;}
  const t=fleet.totals||{},ps=fleet.projects||[],sps=fleet.spaces||[];
  const stat=(v,l)=>`<div class="wsstat"><b>${v}</b><span>${l}</span></div>`;
  const head=(v)=>v===null||v===undefined?"—":money(v);
  const row=p=>{
    const cap=p.lifetime_cap_usd==null?"uncapped":money(p.lifetime_cap_usd);
    const dcap=p.daily_cap_usd==null?"uncapped":money(p.daily_cap_usd);
    return `<div class="wsrow" style="border:none;border-bottom:1px solid var(--border);border-radius:0;background:transparent;padding:7px 4px;gap:12px">
      <span style="min-width:170px;font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${esc(p.name)}${p.broken?' <span title="failed to load" style="color:var(--red);font-size:10px;font-weight:700;letter-spacing:.04em">BROKEN</span>':""}</span>
      <span style="min-width:86px;color:var(--dim);font-size:12px">${esc(p.space_id||"—")}</span>
      <span style="min-width:72px;text-align:right;font-family:ui-monospace,monospace;font-size:11.5px">${money(p.today_usd||0)}</span>
      <span style="min-width:72px;text-align:right;font-family:ui-monospace,monospace;font-size:11.5px">${money(p.spend_7d_usd||0)}</span>
      <span style="min-width:80px;text-align:right;font-family:ui-monospace,monospace;font-size:11.5px;font-weight:700">${money(p.spend_usd||0)}</span>
      <span style="min-width:86px;text-align:right;color:var(--dim);font-size:12px">${esc(cap)}</span>
      <span style="min-width:72px;text-align:right;font-family:ui-monospace,monospace;font-size:11.5px">${head(p.headroom_usd)}</span>
      <span style="min-width:80px;text-align:right;color:var(--dim);font-size:12px">${esc(dcap)}</span>
      <span style="min-width:72px;text-align:right;font-family:ui-monospace,monospace;font-size:11.5px">${head(p.headroom_today_usd)}</span>
      <span style="min-width:64px;text-align:right">${fleetBadge(p.status)}</span></div>`;
  };
  const header=`<div class="wsrow" style="border:none;border-bottom:1px solid var(--border2);border-radius:0;background:transparent;padding:0 4px 6px;gap:12px;color:var(--dim);font-size:10px;font-weight:700;letter-spacing:.06em;text-transform:uppercase">
    <span style="min-width:170px">Project</span><span style="min-width:86px">Space</span>
    <span style="min-width:72px;text-align:right">Today</span><span style="min-width:72px;text-align:right">7 days</span>
    <span style="min-width:80px;text-align:right">Total</span><span style="min-width:86px;text-align:right">Cap</span>
    <span style="min-width:72px;text-align:right">Headroom</span><span style="min-width:80px;text-align:right">Daily cap</span>
    <span style="min-width:72px;text-align:right">Today left</span><span style="min-width:64px;text-align:right">Status</span></div>`;
  const ceil=fleet.hub_ceiling_usd>0?fleet.hub_ceiling_usd:"";
  el.innerHTML=`
    <div class="wshero" style="margin-bottom:18px"><div class="wsmark"><i class="ti ti-report-money"></i></div>
      <div><div class="wsname">Fleet spend</div><div class="wstag">every project · one ledger</div></div>
      <div class="wsstats">${stat(money(t.today_usd||0),"Today")}${stat(money(t.spend_7d_usd||0),"7 days")}${stat(money(t.spend_usd||0),"All time")}${stat(t.projects||0,"Projects")}${stat(t.over||0,"Over cap")}${stat(t.approaching||0,"80%+")}${(t.broken||0)>0?stat(t.broken,"⚠ Broken"):""}</div></div>
    <div class="panel" style="margin-bottom:18px;display:flex;align-items:center;gap:12px;flex-wrap:wrap">
      <span style="font-size:12.5px;font-weight:600">Hub daily soft ceiling</span>
      <input id="fleet-ceiling" type="number" min="0" step="1" placeholder="uncapped" value="${ceil}" style="width:130px;background:var(--card2);border:1px solid var(--border);border-radius:8px;color:var(--text);padding:8px 11px;font-family:ui-monospace,monospace;font-size:11.5px">
      <button class="pri" onclick="saveFleetCeiling()"><i class="ti ti-check"></i> Save</button>
      <span style="color:var(--dim);font-size:12px">USD per day, hub-wide · 0/empty = uncapped · soft: one #general alert per day when today's burn crosses ${Math.round((fleet.hub_warn_pct||0.8)*100)}% — pauses nothing.${t.hub_headroom_usd!=null?` Headroom today: <b style="font-family:ui-monospace,monospace">${money(t.hub_headroom_usd)}</b>.`:""}</span></div>
    <div class="wssec">Projects by burn</div>
    <div class="panel" style="margin-bottom:18px">${header}${ps.map(row).join("")||'<div class="empty">no projects registered yet</div>'}</div>
    <div class="wssec">Spaces</div>
    <div class="panel">${sps.map(s=>`<div class="wsrow" style="border:none;border-bottom:1px solid var(--border);border-radius:0;background:transparent;padding:7px 4px;gap:12px">
      <span style="min-width:170px;font-weight:600">${esc(s.name)}</span>
      <span style="min-width:80px;text-align:right;font-family:ui-monospace,monospace;font-size:11.5px">${money(s.spend_usd||0)}</span>
      <span style="min-width:110px;text-align:right;color:var(--dim);font-size:12px">${s.budget_usd>0?("cap "+money(s.budget_usd)):"uncapped"}</span>
      <span style="min-width:64px;text-align:right">${fleetBadge(s.status)}</span></div>`).join("")||'<div class="empty">no spaces defined</div>'}</div>`;
}
async function saveFleetCeiling(){
  const inp=document.getElementById("fleet-ceiling");if(!inp)return;
  const raw=inp.value.trim();
  const body={ceiling_usd:raw===""?null:Number(raw)};
  try{
    const r=await fetch("/api/fleet/ceiling",{method:"PUT",headers:{"Content-Type":"application/json"},body:JSON.stringify(body)});
    if(r.ok){toasty(body.ceiling_usd?"Hub ceiling saved":"Hub ceiling cleared","ok");renderMgFleet();}
    else toasty("Failed: "+await r.text(),"err");
  }catch(e){toasty("Failed: network error","err");}
}
