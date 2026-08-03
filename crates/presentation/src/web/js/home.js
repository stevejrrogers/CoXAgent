// Workspace home, invites, sprint/backlog panels, team discussion.
// Split from index.html — classic script, load order matters (one shared scope).
// ---- Workspace home: company hero, my agents, project grid, members, invites ----
// Home space filter: "" = all spaces the user can see; persisted per browser.
function homeSpace(){return localStorage.getItem("coxhomespace")||"";}
function setHomeSpace(id){localStorage.setItem("coxhomespace",id||"");renderHome();}
async function renderHome(){
  let ov={projects:[],members:[],workspace:{}},me={agents:[]},spd={spaces:[]};
  try{[ov,me,spd]=await Promise.all([fetch("/api/workspace/overview").then(r=>r.json()),fetch("/api/me/agents").then(r=>r.json()),fetch("/api/spaces").then(r=>r.json()).catch(()=>({spaces:[]}))]);}catch(e){return;}
  const w=ov.workspace||{};
  const spaces=(spd&&spd.spaces)||[];
  let sel=homeSpace();if(sel&&!spaces.some(s=>s.id===sel)){sel="";localStorage.removeItem("coxhomespace");}
  const selSp=spaces.find(s=>s.id===sel)||null;
  // Scope the dashboard to the picked space (projects, stats, hero identity).
  const allProjects=ov.projects||[];
  const projects=selSp?allProjects.filter(p=>(selSp.projects||[]).includes(p.id)):allProjects;
  const totalSpend=projects.reduce((a,p)=>a+(p.spend||0),0);
  const online=new Set();projects.forEach(p=>(p.online||[]).forEach(o=>online.add(o)));
  // Scope agents + team to the picked space too — a space with no projects
  // must show an empty dashboard, not the whole hub's.
  const pidSet=new Set(projects.map(p=>p.id));
  const agents=selSp?(me.agents||[]).filter(a=>pidSet.has(a.project)):(me.agents||[]);
  const members=selSp?(ov.members||[]).filter(m=>
      (selSp.admins||[]).some(a=>a.toLowerCase()===(m.username||"").toLowerCase())
      ||(selSp.members||[]).some(a=>a.toLowerCase()===(m.username||"").toLowerCase())
      ||(m.projects||[]).some(p=>pidSet.has(p)))
    :(ov.members||[]);
  // Sidebar "Home" shows the company name once the workspace exists.
  const navH=document.getElementById("nav-home");
  if(navH)navH.innerHTML=w.name?`<i class="ti ti-building-skyscraper"></i> ${esc(w.name)}`:'<i class="ti ti-home"></i> Home';
  const isAdmin=ME&&(ME.role==="admin"||ME.role==="super");
  const nm=selSp?selSp.name:(w.name||"Your workspace");
  const tag=selSp?(selSp.tagline||`${(selSp.projects||[]).length} projects · space`):(w.tagline||"one company · many projects · autonomous teams");
  const stat=(v,l)=>`<div class="wsstat"><b>${v}</b><span>${l}</span></div>`;
  const spaceBar=spaces.length?`<div class="spacebar">
      <button class="spchip${sel?"":" on"}" onclick="setHomeSpace('')"><i class="ti ti-layout-grid"></i> All</button>
      ${spaces.map(s=>`<button class="spchip${sel===s.id?" on":""}" onclick="setHomeSpace('${esc(s.id)}')"><span class="sdot" style="background:${projColor(s.id)}"></span>${esc(s.name)}<span class="spn">${(s.projects||[]).length}</span></button>`).join("")}
    </div>`:"";
  // Space admins (and Super) manage THEIR space right here — name/tagline via
  // the lite modal; membership/projects stay Super-only in Manage.
  const meU=(ME&&ME.username||"").toLowerCase();
  const canEditSp=selSp&&(ME&&ME.role==="super"||(selSp.admins||[]).some(a=>a.toLowerCase()===meU));
  window._homeSpaces=spaces;
  const editBtn=canEditSp?`<button class="gc-btn" style="margin-left:14px" onclick="openSpaceModalLite(window._homeSpaces.find(s=>s.id==='${esc(selSp.id)}'))"><i class="ti ti-pencil"></i> Edit space</button>`:"";
  document.getElementById("ws-hero").innerHTML=spaceBar+`<div class="wshero">
      <div class="wsmark">${esc(initials(nm))}</div>
      <div><div class="wsname">${esc(nm)}</div><div class="wstag">${esc(tag)}</div></div>
      ${editBtn}
      <div class="wsstats">${stat(projects.length,"Projects")}${stat(members.length,"Members")}${stat(online.size,"Online")}${stat(money(totalSpend),"Spend")}</div></div>`;
  ov=Object.assign({},ov,{projects,members});
  document.getElementById("ws-myagents").innerHTML=agents.length?agents.map(a=>{
    const st=a.online?`<span class="wsdot on"></span><b>${esc((a.role||"").toUpperCase())}</b> ${a.ticket?`<span class="tid">${esc(a.ticket)}</span>`:""}`
      :(a.desired===false?'<span class="wsdot off"></span>stopped':'<span class="wsdot idle"></span>idle');
    const op=a.operator?`'${esc(a.operator)}'`:null;
    const btn=a.online?`<button class="agbtn stop" onclick="myAgentCtl('${esc(a.project)}',${op},'stop')" title="Stop"><i class="ti ti-player-stop-filled"></i></button>`
      :`<button class="agbtn" onclick="myAgentCtl('${esc(a.project)}',${op},'start')" title="Start"><i class="ti ti-player-play-filled"></i></button>`;
    return `<div class="wsrow wscard-a"><div style="flex:1;min-width:0">
      <div class="wsproj">${esc(a.name||a.project)}</div>
      <div class="wsstate">${st}</div>
      <div class="wsspend">${fmtK(a.tokens||0)} tok · ${money(a.cost||0)}</div></div>${btn}</div>`;}).join("")
    :'<div class="empty" style="padding:14px">No agents yet — open a project and Start yours.</div>';
  document.getElementById("ws-projects").innerHTML=(ov.projects||[]).map(p=>`
    <div class="wscard" onclick="switchProject('${esc(p.id)}');nav('overview')">
      <div class="wsc-h"><b>${esc(p.name||p.id)}</b><span class="tk">${esc(p.alias||"")}</span><span class="wsc-v">v${esc(p.version||"0")}</span></div>
      ${p.sprint?`<div class="wsc-goal"><i class="ti ti-target-arrow"></i> S${p.sprint.number}: ${esc(p.sprint.goal||"")}</div>`:''}
      <div class="wsc-stats"><span>🚀 ${p.shipped||0}</span><span>⚙️ ${p.in_flight||0}</span><span>🐛 ${p.bugs_open||0}</span><span>${money(p.spend||0)}</span></div>
      <div class="wsc-online">${(p.online||[]).map(o=>`<span class="wsava" title="${esc(o)}">${esc(initials(o))}</span>`).join("")||'<span class="wsc-off">no one online</span>'}</div>
    </div>`).join("")||'<div class="empty">no projects yet</div>';
  document.getElementById("ws-members").innerHTML=(ov.members||[]).map(m=>
    `<span class="wsmem" title="${esc((m.projects||[]).join(", ")||"all projects")}"><span class="wsava">${esc(initials(m.name||m.username))}</span> ${esc(m.name||m.username)} ${m.role==="admin"?'<i class="ti ti-crown" style="color:var(--amber)" title="workspace admin"></i>':""}<span class="wsrole">${esc(m.role)}</span></span>`).join("")||'<div class="empty">running open — no accounts</div>';
}
async function saveWs(){try{
  const downloads={releases_repo:val("dl-repo"),latest_version:(window._appLatest&&window._appLatest.latest_version)||"",macos:val("dl-mac"),windows:val("dl-win"),linux:val("dl-lin"),ios:val("dl-ios")};
  await fetch("/api/workspace",{method:"PUT",headers:{"Content-Type":"application/json"},body:JSON.stringify({name:val("wsn"),tagline:val("wst"),accent:"",conventions:val("wsc"),downloads})});
  toasty("Saved — conventions apply to new agent runs","ok");checkAppUpdate();renderHome();}catch(e){}}
// ── App distribution: update banner + downloads modal ────────────────────────
// The hub polls GitHub Releases (releases_repo) and republishes version + URLs
// at /api/app/latest; every client compares against the hub's own version.
function verGt(a,b){const pa=String(a).split(".").map(Number),pb=String(b).split(".").map(Number);
  for(let i=0;i<3;i++){const x=pa[i]||0,y=pb[i]||0;if(x!==y)return x>y;}return false;}
async function checkAppUpdate(){
  let d=null;try{d=await(await fetch("/api/app/latest")).json();}catch(e){return;}
  window._appLatest=d;
  const newer=!!(d.latest_version&&d.hub_version&&verGt(d.latest_version,d.hub_version));
  // The get-app button transforms while an update exists: rocket icon, accent
  // pulse + amber dot — visible even after the toast was dismissed.
  const btn=document.getElementById("getapp-btn"),ic=document.getElementById("getapp-ic");
  if(btn){
    btn.classList.toggle("has-update",newer);
    btn.title=newer?("CoXAgent "+d.latest_version+" đã có — bấm để cập nhật"):"Get the app";
    const dot=btn.querySelector(".upd-dot");if(dot)dot.hidden=!newer;
    if(ic)ic.className=newer?"ti ti-rocket":"ti ti-download";
    const ver=document.getElementById("getapp-ver");
    if(ver){ver.hidden=!newer;ver.textContent=newer?("v"+d.latest_version):"";}
  }
}
function coxSelfUpdate(url){
  try{
    window.webkit.messageHandlers.coxupdate.postMessage(url);
    toasty("Đang tải bản cập nhật — app sẽ tự khởi động lại khi xong","ok");
    const ic=document.getElementById("getapp-ic");
    if(ic)ic.className="ti ti-loader-2 att-spin";
    close_("ov-getapp");
  }catch(e){window.open(url,"_blank");}
}
async function openGetApp(){
  // Always fetch fresh — the hub may have just discovered a new release —
  // and resync the banner/button so a stale "update available" never lingers
  // after you're already on the newest build.
  await checkAppUpdate();
  const d=window._appLatest||{downloads:{}};
  const dl=d.downloads||{};
  const upToDate=d.latest_version&&d.hub_version&&!verGt(d.latest_version,d.hub_version);
  document.getElementById("ga-ver").textContent=
    !d.latest_version?"chưa cấu hình release — Settings → Workspace"
    :upToDate?("bạn đang chạy v"+d.hub_version+" — mới nhất ✓")
    :("bạn đang chạy v"+d.hub_version+" → mới nhất v"+d.latest_version);
  const plats=[["macos","macOS","brand-apple",".dmg"],["windows","Windows","brand-windows",".exe"],["linux","Linux","brand-ubuntu",".tar.gz"],["ios","iOS","device-mobile","App Store / TestFlight"]];
  const mine=/Mac/i.test(navigator.platform)?"macos":/Win/i.test(navigator.platform)?"windows":/Linux/i.test(navigator.platform)?"linux":"";
  // Inside the macOS shell we can self-update in place — one click, app
  // restarts itself on the new version. Elsewhere: normal browser download.
  const canSelf=!!(window.webkit&&window.webkit.messageHandlers&&window.webkit.messageHandlers.coxupdate);
  document.getElementById("ga-list").innerHTML=plats.map(([k,label,icon,hint])=>{
    const url=dl[k];const on=k===mine;
    if(!url)return `<div class="gc-btn" style="display:flex;align-items:center;gap:10px;padding:12px 14px;opacity:.45;cursor:default"><i class="ti ti-${icon}" style="font-size:19px"></i><b style="flex:1">${label}</b><span style="font-size:11px">chưa có bản build</span></div>`;
    if(k==="macos"&&canSelf)
      return `<button onclick="coxSelfUpdate(location.origin+'/api/app/download/macos.dmg')" class="gc-btn" style="display:flex;align-items:center;gap:10px;padding:12px 14px;border-color:var(--accent);cursor:pointer;width:100%;text-align:left"><i class="ti ti-${icon}" style="font-size:19px"></i><b style="flex:1">${label}</b><span style="font-size:11px;color:var(--accent2)">cập nhật & tự khởi động lại</span><i class="ti ti-refresh"></i></button>`;
    return `<a href="${esc(url)}" target="_blank" rel="noopener" class="gc-btn" style="display:flex;align-items:center;gap:10px;padding:12px 14px;text-decoration:none;${on?"border-color:var(--accent)":""}"><i class="ti ti-${icon}" style="font-size:19px"></i><b style="flex:1">${label}</b><span style="font-size:11px;color:var(--dim)">${hint}${on?" · máy này":""}</span><i class="ti ti-download"></i></a>`;
  }).join("");
  const nw=document.getElementById("ga-notes");
  if(nw){const has=!!(d.notes&&d.notes.trim())&&!upToDate;nw.hidden=!has;
    if(has)document.getElementById("ga-notes-body").textContent=d.notes.trim();}
  document.getElementById("ov-getapp").classList.add("open");
}
// Projects the caller may invite into: super = every project; space admin =
// projects of their spaces (mirrors the server-side invite scoping).
function invProjectPicker(){
  const sup=ME&&ME.role==="super";
  const spaces=window._homeSpaces||[];
  const items=sup?(window.PROJECTS||[]).map(p=>p.id)
    :[...new Set(spaces.filter(s=>(s.admins||[]).some(a=>a.toLowerCase()===(ME&&ME.username||"").toLowerCase())).flatMap(s=>s.projects||[]))];
  return mgPicker("inv-projpick",items,[]);
}
async function createInvite(){
  const projects=picked("inv-projpick");
  try{const r=await(await fetch("/api/workspace/invites",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({role:val("inv-role"),projects,uses:parseInt(val("inv-uses")||"5",10)})})).json();
    if(r.ok){const url=location.origin+r.url;document.getElementById("inv-new").innerHTML=`<div class="wsrow" style="background:color-mix(in srgb,var(--green) 8%,transparent)"><span class="wsproj" style="font-family:ui-monospace,monospace;font-size:12px;user-select:all">${esc(url)}</span><button class="ticket-actions" onclick="navigator.clipboard.writeText('${esc(url)}');toasty('Copied','ok')"><i class="ti ti-copy"></i> Copy</button></div>`;renderHome();}
  }catch(e){}}
function copyInvite(t){navigator.clipboard.writeText(location.origin+"/join/"+t);toasty("Invite link copied","ok");}
async function revokeInvite(t){try{await fetch("/api/workspace/invites/"+t,{method:"DELETE"});renderHome();}catch(e){}}
async function renderTokenSaver(){
  const el=document.getElementById("cost-tokensaver");if(!el)return;
  let d={};try{d=await(await fetch("/api/token-saver")).json();}catch(e){return;}
  if(!d.samples){setHTML(el,'<div class="empty" style="padding:6px">No compressions recorded yet — the saver kicks in on large/noisy tool output while agents run.</div>');return;}
  const fc=n=>n>=1e6?(n/1e6).toFixed(1)+"M":n>=1e3?(n/1e3).toFixed(1)+"K":String(n);
  setHTML(el,`<div style="display:flex;gap:26px;flex-wrap:wrap;align-items:flex-end">
    <div><div style="font-size:27px;font-weight:700;color:var(--green);letter-spacing:-.02em;line-height:1">${d.pct}%</div><div style="font-size:12px;color:var(--muted);margin-top:4px">output shrunk</div></div>
    <div><div style="font-size:20px;font-weight:600">${fc(d.saved)}</div><div style="font-size:12px;color:var(--muted)">chars saved</div></div>
    <div><div style="font-size:20px;font-weight:600">${fc(d.before)} → ${fc(d.after)}</div><div style="font-size:12px;color:var(--muted)">before → after</div></div>
    <div><div style="font-size:20px;font-weight:600">${d.samples}</div><div style="font-size:12px;color:var(--muted)">tool outputs compressed</div></div>
  </div><div style="margin-top:14px;background:var(--card2);border-radius:6px;height:8px;overflow:hidden"><div style="width:${Math.min(100,d.pct)}%;height:100%;background:var(--green)"></div></div>`);}

function cmpVer(a,b){const pa=(a||"0.0.0").split(".").map(Number),pb=(b||"0.0.0").split(".").map(Number);
  for(let i=0;i<3;i++){if((pa[i]||0)!==(pb[i]||0))return (pa[i]||0)-(pb[i]||0);}return 0;}
function tRow(t,live){const shipped=t.status==="done"||t.status==="documented";
  const col=shipped?"var(--green)":(live?"var(--accent2)":"var(--muted)");
  return `<div class="act" onclick="showTicket('${t.id}')" style="cursor:pointer"><div class="ad" style="background:${col}22;color:${col}"><i class="ti ti-${shipped?'check':(live?'loader-2':'circle')}" style="font-size:13px"></i></div>
    <div class="atx"><span class="tk">${esc(t.id)}</span> ${esc(t.title||'(removed)')}</div><span class="tm">${esc(t.status||'?')}</span></div>`;}
function renderSprintPanel(s){
  const sp=s.sprint;const el=document.getElementById("sprint-body");
  if(!sp){el.innerHTML='<div class="panel" style="text-align:center;padding:32px 20px"><i class="ti ti-run" style="font-size:30px;color:var(--accent2)"></i><div style="font-size:15px;font-weight:600;margin:10px 0 4px">Kanban mode</div><div class="empty" style="padding:0">No sprints in kanban flow. Switch the workflow to <b style="color:var(--text)">scrum</b> in <a onclick="nav(\'settings\')" style="color:var(--accent2);cursor:pointer">Settings</a> so the PO plans sprints by priority.</div></div>';return;}
  const byId=id=>(s.tickets||[]).find(x=>x.id===id)||{id,title:"(removed)",status:"?"};
  const committed=(sp.committed||[]).map(byId);
  const isDone=t=>["done","documented","verified"].includes(t.status);
  const isProg=t=>["in_progress","fixed"].includes(t.status);
  const done=committed.filter(isDone),prog=committed.filter(isProg);
  const todo=committed.filter(t=>!isDone(t)&&!isProg(t));
  const total=committed.length,pct=total?Math.round(done.length/total*100):0;
  // Sprint window from cycles: length, elapsed, remaining.
  const cyc=(window.RUNNER&&window.RUNNER.cycle)||0;
  const len=sp.length_cycles||sp.length||0;
  const elapsed=Math.max(0,cyc-(sp.started_cycle||0));
  const left=len?Math.max(0,len-elapsed):null;
  // Velocity: average shipped over closed sprints.
  const hist=(s.sprints||[]).filter(x=>x.done!=null);
  const vel=hist.length?Math.round(hist.reduce((a,x)=>a+x.done,0)/hist.length*10)/10:null;
  const ms=s.milestones||[],cur=String(s.current_version||"0.0.0");
  const activeMs=ms.find(m=>cmpVer(m.target_version,cur)>0)||ms[ms.length-1];
  const msBadge=activeMs?`<span class="msbadge"><i class="ti ti-target"></i> ${esc(activeMs.name)} · v${esc(activeMs.target_version)}</span>`:'';
  const rank={high:0,medium:1,low:2};
  const inSp=new Set(sp.committed||[]);
  const upnext=(s.tickets||[]).filter(t=>["pending","ready"].includes(t.status)&&t.type!=="bug"&&!inSp.has(t.id))
    .sort((a,b)=>(rank[a.priority]??3)-(rank[b.priority]??3)).slice(0,total||5);
  const pc={high:"var(--red)",medium:"var(--amber)",low:"var(--muted)"};
  const card=t=>{const col=pc[t.priority]||"var(--border2)";
    return `<div class="sp-card" style="border-left-color:${col}" onclick="showTicket('${t.id}')">
      <div class="t">${esc(t.title||'(removed)')}</div>
      <div class="m"><span class="id">${esc(t.id)}</span><i class="ti ti-${t.type==='bug'?'bug':'bulb'}" style="font-size:11px"></i><span style="margin-left:auto;color:${col}">${esc(t.priority||'—')}</span></div></div>`;};
  const colHtml=(label,dot,items)=>`<div class="sp-col"><div class="sp-colh"><span class="dot2" style="background:${dot}"></span>${label}<span class="n">${items.length}</span></div>${items.map(card).join("")||'<div class="empty">—</div>'}</div>`;
  const stat=(v,k,c)=>`<div class="sp-stat"><div class="v"${c?` style="color:${c}"`:""}>${v}</div><div class="k">${k}</div></div>`;
  el.innerHTML=`
    <div class="panel sprintcard">
      <div class="sprinthead">
        <div><div class="sprintno">Sprint #${sp.number} <span class="pbadge on" style="margin-left:6px">● active</span></div>
          <div class="sprintgoal">${esc(sp.goal)}</div>${msBadge}</div>
        <div class="sp-ring" style="--p:${pct}"><div class="in"><b>${pct}%</b><span>done</span></div></div>
        <button class="sp-close" onclick="closeSprintNow()" title="Archive this sprint now and open the next one">
          <i class="ti ti-flag-check"></i> Close sprint</button>
      </div>
      <div class="sp-stats">
        ${stat(total,"Committed")}
        ${stat(prog.length,"In progress","var(--accent2)")}
        ${stat(done.length,"Shipped","var(--green)")}
        ${stat(total-done.length,"Remaining","var(--amber)")}
        ${stat(left==null?"—":left,"Cycles left")}
        ${stat(vel==null?"—":vel,"Velocity avg")}
      </div>
    </div>
    ${spBurndown(total,done.length,elapsed,len)}
    <div class="sec">Sprint board</div>
    <div class="sp-board">
      ${colHtml("To do","var(--muted)",todo)}
      ${colHtml("In progress","var(--accent2)",prog)}
      ${colHtml("Done","var(--green)",done)}
    </div>
    <div class="sec" style="margin-top:16px">Up next <span style="font-size:11px;color:var(--dim);font-weight:400">· top of the backlog the PO pulls into the next sprint — use <b>+ sprint</b> on the <a onclick="setWorkTab('backlog')" style="cursor:pointer;color:var(--accent2)">Backlog tab</a> to pull one into THIS sprint</span></div>
    <div class="panel">${upnext.map(t=>tRow(t,false)).join("")||'<div class="empty">backlog clear — nothing queued</div>'}</div>
    ${velocityHtml(s.sprints||[])}`;
}
// Burndown: ideal line (committed→0 across the cycle window) vs work actually
// remaining now. Cycle-based, so it reads even before any sprint has closed.
function spBurndown(total,doneN,elapsed,len){
  if(!len||!total)return"";
  const W=560,H=140,pl=34,pr=12,pt=12,pb=24,iw=W-pl-pr,ih=H-pt-pb;
  const x=c=>pl+(len?c/len:0)*iw, y=v=>pt+(1-(total?v/total:0))*ih;
  const remaining=total-doneN,ex=Math.min(elapsed,len);
  const ideal=`${x(0)},${y(total)} ${x(len)},${y(0)}`;
  const actual=`${x(0)},${y(total)} ${x(ex)},${y(remaining)}`;
  const yticks=[0,Math.ceil(total/2),total].filter((v,i,a)=>a.indexOf(v)===i);
  return `<div class="sp-burn"><div style="font-size:12px;font-weight:600;color:var(--muted);margin-bottom:6px">Burndown <span style="color:var(--dim);font-weight:400">· work remaining vs the ideal pace</span></div>
    <svg viewBox="0 0 ${W} ${H}" style="width:100%;height:auto;display:block">
      ${yticks.map(v=>`<line x1="${pl}" y1="${y(v)}" x2="${W-pr}" y2="${y(v)}" stroke="var(--border2)" stroke-width="1"/><text x="${pl-6}" y="${y(v)+3}" text-anchor="end" font-size="9" fill="var(--dim)">${v}</text>`).join("")}
      <polyline points="${ideal}" fill="none" stroke="var(--border2)" stroke-width="1.5" stroke-dasharray="4 4"/>
      <polyline points="${actual}" fill="none" stroke="var(--accent2)" stroke-width="2.5" stroke-linecap="round"/>
      <circle cx="${x(ex)}" cy="${y(remaining)}" r="4" fill="var(--accent2)"/>
      <text x="${pl}" y="${H-8}" font-size="9" fill="var(--dim)">cycle ${(RUNNER&&RUNNER.cycle-elapsed)||0}</text>
      <text x="${W-pr}" y="${H-8}" text-anchor="end" font-size="9" fill="var(--dim)">+${len} cycles</text>
    </svg>
    <div style="display:flex;gap:16px;font-size:10.5px;color:var(--dim);margin-top:2px"><span><b style="color:var(--accent2)">━</b> remaining (${remaining})</span><span><b style="color:var(--muted)">┄</b> ideal</span></div></div>`;
}
function renderBacklogPanel(s){
  const el=document.getElementById("backlog-body");
  // Backlog = not yet in flight and not shipped: pending/ready/open, newest-priority first.
  const rank={high:0,medium:1,low:2};
  const items=(s.tickets||[]).filter(t=>["pending","ready","open"].includes(t.status))
    .sort((a,b)=>(rank[a.priority]??3)-(rank[b.priority]??3));
  const committed=new Set((s.sprint&&s.sprint.committed)||[]);
  const pc={high:"var(--red)",medium:"var(--amber)",low:"var(--muted)"};
  const rows=items.map(t=>{const col=pc[t.priority]||"var(--muted)";const inSp=committed.has(t.id);
    return `<div class="act" onclick="showTicket('${t.id}')" style="cursor:pointer"><div class="ad" style="background:${col}22;color:${col}"><i class="ti ti-${t.type==='bug'?'bug':'bulb'}" style="font-size:13px"></i></div>
      <div class="atx"><span class="tk">${esc(t.id)}</span> ${esc(t.title)} <span class="fchip" style="padding:1px 8px;font-size:10px;border:none;background:${col}22;color:${col}">${esc(t.priority||'—')}</span>${inSp?' <span class="fchip" style="padding:1px 8px;font-size:10px;border:none;background:var(--accentbg);color:var(--accent2)">in sprint</span>':''}</div>
      <button class="sp-scope" onclick="event.stopPropagation();sprintScope('${t.id}',${inSp?"false":"true"})" title="${inSp?'Drop from the running sprint':'Pull into the running sprint'}">${inSp?'− sprint':'+ sprint'}</button>
      <span class="tm">${esc(t.status)}</span></div>`;}).join("");
  el.innerHTML=`<div class="filters"><span style="font-size:12px;color:var(--muted)">Prioritised backlog — the PO pulls from the top into each sprint.</span><div style="flex:1"></div><span class="fchip">${items.length} waiting</span></div>
    <div class="panel">${rows||'<div class="empty">backlog is clear — every ticket is in flight or shipped</div>'}</div>`;
}
// Pull a backlog ticket into the sprint that is already running, or drop it.
// The automatic commit only happens at roll-over; this is how a person changes
// their mind mid-sprint without editing the ticket.
async function sprintScope(id,add){
  try{
    const r=await fetch(api("/sprint/"+(add?"commit":"drop")),
      {method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({tickets:[id]})});
    if(!r.ok){toasty((await r.text())||"Sprint update failed","err");return;}
    toasty(add?`${id} pulled into the sprint`:`${id} dropped from the sprint`);
    await refreshDisc();
  }catch(e){toasty("Network error","err");}
}
// Close the running sprint now instead of waiting out its cycle window. The
// sprint is archived exactly as a timed roll-over archives it.
async function closeSprintNow(){
  const ok=await coxModal({title:"Close sprint",
    message:"Archive the running sprint now and open the next one? Unfinished tickets stay in the backlog and the next sprint commits from the top.",
    confirmText:"Close sprint"});
  if(!ok)return;
  try{
    const r=await fetch(api("/sprint/close"),{method:"POST"});
    if(!r.ok){toasty((await r.text())||"Close failed","err");return;}
    const d=await r.json();
    toasty(`Sprint closed — #${d.sprint} is now open`);
    await refreshDisc();
  }catch(e){toasty("Network error","err");}
}
function loadComments(){if(CUR==="discuss")renderDiscuss();}
// Show the current sprint goal as a read-only chip; set it with /sprint <goal>.
function syncSprintGoal(s){
  const bar=document.getElementById("sprint-goal-bar"),txt=document.getElementById("sprint-goal-txt");
  if(!bar||!txt)return;
  const g=(s&&s.sprint_goal||"").trim();
  txt.textContent=g; bar.style.display=g?"flex":"none";
}
// --- Slash commands in the team chat ---
const SLASH_CMDS=[
  ["/sprint","<goal>","Set the sprint goal — BA proposes tickets toward it"],
  ["/discuss","<topic>","Team discusses a topic; SM decides"],
  ["/standup","","SM runs a standup roundup"],
  ["/arch","","SA reviews architecture & files refactors"],
  ["/docs","","DOCS reviews the Wiki & fills gaps"],
  ["/merge","","SA merges every green open PR right now (oldest first)"],
  ["/digest","","Post a daily digest (shipped · spend · sprint) to chat"],
  ["/start","","Start your operator"],
  ["/stop","","Stop your operator"],
  ["/pause","","Pause the runner"],
  ["/help","","Show this list"],
];
async function refreshDisc(){try{const s=await(await fetch(api("/state"))).json();render(s);renderDiscuss();}catch(e){}}
// Returns true if `body` was a slash command (and was handled — don't post it).
async function handleSlash(body){
  if(body[0]!=="/")return false;
  const m=body.match(/^\/(\S+)\s*([\s\S]*)$/); if(!m)return false;
  const cmd=m[1].toLowerCase(), arg=(m[2]||"").trim(), dummy={};
  switch(cmd){
    case "sprint": case "goal":
      if(!arg){toasty("Usage: /sprint <goal text>","warn");return true;}
      try{await fetch(api("/sprint/goal"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({goal:arg})});toasty("Sprint goal set","ok");}catch(e){}
      refreshDisc();return true;
    case "discuss": case "discussion":
      if(!arg){toasty("Usage: /discuss <topic>","warn");return true;}
      setDiscBusy("Team is discussing "+arg+"…");
      try{await fetch(api("/discuss"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({topic:arg})});}catch(e){}
      refreshDisc();return true;
    case "merge": case "merge_queue":
      mergeSweep(); return true;
    case "digest":
      try{await fetch(api("/digest"),{method:"POST"});toasty("Digest posted to chat","ok");}catch(e){}
      refreshDisc();return true;
    case "standup": setDiscBusy("SM is running the standup…"); runStandup(dummy); return true;
    case "arch": case "architecture": setDiscBusy("SA is reviewing the architecture…"); reviewRun(dummy,"architecture-review","SA"); return true;
    case "docs": setDiscBusy("DOCS is reviewing the Wiki…"); reviewRun(dummy,"docs-review","DOCS"); return true;
    case "start": case "resume": ctl("resume"); toasty("Starting…","ok"); return true;
    case "stop": ctl("stop"); toasty("Stopping…","ok"); return true;
    case "pause": ctl("pause"); toasty("Paused","ok"); return true;
    case "help": toasty(SLASH_CMDS.map(c=>c[0]+(c[1]?" "+c[1]:"")).join("  ·  "),"ok"); return true;
    default: toasty("Unknown command "+"/"+cmd+" — try /help","warn"); return true;
  }
}
// Slash autocomplete menu under the message box.
function slashMenu(){
  const inp=document.getElementById("disc-input"),menu=document.getElementById("slash-menu");
  if(!inp||!menu)return;
  const v=inp.value;
  if(v[0]!=="/"||v.includes(" ")){menu.style.display="none";return;}
  const q=v.slice(1).toLowerCase();
  const hits=SLASH_CMDS.filter(c=>c[0].slice(1).startsWith(q));
  if(!hits.length){menu.style.display="none";return;}
  menu.innerHTML=hits.map(c=>`<div class="slash-item" onclick="pickSlash('${c[0]}',${c[1]?1:0})"><b>${c[0]}</b> <span class="slash-arg">${esc(c[1])}</span><span class="slash-desc">${esc(c[2])}</span></div>`).join("");
  menu.style.display="block";
}
function pickSlash(cmd,hasArg){
  const inp=document.getElementById("disc-input");
  if(hasArg){inp.value=cmd+" ";inp.focus();document.getElementById("slash-menu").style.display="none";}
  else{inp.value=cmd;document.getElementById("slash-menu").style.display="none";sendComment();}
}
// A live "working…" bubble shown in the feed while a long agent action runs.
let DISC_BUSY=null;
function setDiscBusy(label){DISC_BUSY={label,count:window.DISC_MSGN||0,since:Date.now()};if(CUR==="discuss")renderDiscuss();}
// Turn ticket ids (e.g. OT-F001, CXC-B123) in rendered message HTML into
// clickable links that open the ticket detail. Skips ids already inside a tag.
function linkifyTickets(html){
  return html.replace(/(^|[^\w>])([A-Z]{2,6}-[FBC]\d{1,4})\b/g,
    (_,pre,id)=>`${pre}<a class="tref" onclick="event.stopPropagation();showTicket('${id}')">${id}</a>`);
}
function renderDiscuss(){
  const sel=document.getElementById("disc-filter");if(!sel)return;
  syncSprintGoal(STATE);
  const _tb=document.getElementById("disc-toolbar");if(_tb&&!_tb.dataset.i){_tb.innerHTML=mdToolbar("disc-input");_tb.dataset.i="1";}
  const want=sel.value;
  const ids=(STATE.tickets||[]).map(t=>t.id);
  const opts=['<option value="">Team channel</option>'].concat(ids.map(id=>`<option value="${esc(id)}">${esc(id)}</option>`)).join("");
  if(sel.dataset.n!=String(ids.length)){sel.innerHTML=opts;sel.value=want;sel.dataset.n=String(ids.length);}
  document.getElementById("disc-title").textContent=want?want:"Team channel";
  document.getElementById("disc-sub").textContent=want?"thread for this ticket":"standups, decisions & threads";
  const msgs=(STATE.comments||[]).filter(c=>want?c.ticket===want:!c.ticket);
  const box=document.getElementById("disc-thread");
  const me=(ME&&ME.username)||"";
  // Re-render only when the thread actually changed — otherwise every 1s SSE
  // tick would rebuild the DOM and yank the scroll position to the bottom.
  const sig=want+":"+msgs.length+":"+(msgs.length?(msgs[msgs.length-1].at||""):"");
  if(box.dataset.sig===sig)return;
  // Keep the reading position unless the user is already at the bottom.
  const atBottom=box.scrollHeight-box.scrollTop-box.clientHeight<80;
  box.dataset.sig=sig;
  if(!msgs.length){box.innerHTML='<div class="chatempty"><i class="ti ti-messages"></i><div>No messages yet</div><span>Send a message, or spin up an agent discussion.</span></div>';return;}
  let lastDay="";
  box.innerHTML=msgs.map(c=>{
    const t=c.at||"";const day=t.slice(0,10);const time=t.slice(11,16);
    let sep="";
    if(day&&day!==lastDay){lastDay=day;sep=`<div class="chatday"><span>${esc(day)}</span></div>`;}
    const ev=scrumEvent(c);
    if(ev){ // scrum / system event → centered timeline pill, not a chat bubble
      return `${sep}<div class="cevent"><span class="cev-pill ${ev.kind?'cev-'+ev.kind:''}" style="--ec:${ev.col}"><i class="ti ti-${ev.icon}"></i> ${esc(ev.text)}<span class="cev-t">${esc(time)}</span></span></div>`;
    }
    const isAgent=!!AC[c.author];
    const mine=!isAgent&&c.author===me;
    const col=cvar(AC[c.author]||"--muted");
    const nick=AGENT_NICK[c.author];
    const name=nick||c.author;
    // Agent messages: show the nickname as the person, the role as a tag, and —
    // when several workers run in parallel — who (operator@host) posted it.
    const roleTag=(isAgent&&nick)?` <span class="crole">${esc(c.author)}</span>`:'';
    const byChip=(isAgent&&c.by)?` <span class="cby" title="worker that posted this">${esc(c.by)}</span>`:'';
    const badge=isAgent?'<span class="cbadge">agent</span>':'';
    const tick=(c.ticket&&!want)?` <span class="ctk tref" onclick="event.stopPropagation();showTicket('${esc(c.ticket)}')" style="cursor:pointer">${esc(c.ticket)}</span>`:'';
    return `${sep}<div class="cmsg${mine?' mine':''}">
      <div class="cav" style="background:${col}22;color:${col}">${esc(initials(name))}</div>
      <div class="ccol"><div class="chead"><b>${esc(name)}</b>${roleTag}${badge}${byChip}${tick}<span class="ctime">${esc(time)}</span></div>
        ${c.body?`<div class="cbub md">${linkifyTickets(mdRender(c.body))}</div>`:''}${attHtml(c.attachments)}</div></div>`;}).join("");
  // Working indicator: a "thinking…" bubble while a chat-triggered action runs.
  // Clears once new messages arrive (the agent replied/acted) or after a while.
  if(DISC_BUSY){
    // Count growth still clears the indicator for the fire-and-forget triggers
    // (standup, reviews); the time bound is the last resort for those.
    if(msgs.length>DISC_BUSY.count||Date.now()-DISC_BUSY.since>150000){DISC_BUSY=null;}
    else{box.innerHTML+=`<div class="cmsg"><div class="cav" style="background:color-mix(in srgb,var(--accent2) 20%,transparent);color:var(--accent2)"><i class="ti ti-loader-2 att-spin"></i></div><div class="ccol"><div class="cbub working"><i class="ti ti-loader-2 att-spin"></i> ${esc(DISC_BUSY.label)}</div></div></div>`;}
  }
  window.DISC_MSGN=msgs.length;
  if(atBottom)box.scrollTop=box.scrollHeight;
}
// Classify SM/DEPLOY scrum notifications so they render as a timeline event
// (sprint open/close, ship, deploy) rather than a conversational bubble.
// Wrap the selection (or insert) markdown in a text input/textarea.
function mdIns(id,b,a){const el=document.getElementById(id);if(!el)return;
  const s=el.selectionStart??el.value.length,e=el.selectionEnd??el.value.length,v=el.value,sel=v.slice(s,e)||"text";
  el.value=v.slice(0,s)+b+sel+a+v.slice(e);el.focus();
  try{el.selectionStart=s+b.length;el.selectionEnd=s+b.length+sel.length;}catch(_){}}
function mdToolbar(id){return `<div class="cmtool">
  <button type="button" title="Bold" onmousedown="event.preventDefault()" onclick="mdIns('${id}','**','**')"><i class="ti ti-bold"></i></button>
  <button type="button" title="Italic" onmousedown="event.preventDefault()" onclick="mdIns('${id}','_','_')"><i class="ti ti-italic"></i></button>
  <button type="button" title="Inline code" onmousedown="event.preventDefault()" onclick="mdIns('${id}','\`','\`')"><i class="ti ti-code"></i></button>
  <button type="button" title="Bullet list" onmousedown="event.preventDefault()" onclick="mdIns('${id}','\\n- ','')"><i class="ti ti-list"></i></button>
  <button type="button" title="Link" onmousedown="event.preventDefault()" onclick="mdIns('${id}','[','](https://)')"><i class="ti ti-link"></i></button>
</div>`;}
function scrumEvent(c){
  const b=(c.body||"").trim();const a=c.author||"";
  // Strip a leading emoji/symbol so "📋 Sprint review…" still classifies.
  const s=b.replace(/^[^\w]+/,"");
  if(a==="DEPLOY"||/^deploy (ok|failed)/i.test(s)) return {icon:/failed/i.test(s)?"cloud-x":"cloud-check",col:/failed/i.test(s)?"var(--red)":"var(--green)",text:b};
  if(/^shipped:/i.test(s)) return {icon:"rocket",col:"var(--green)",text:b};
  // Facilitated scrum discussion: topic → decision → action stand out.
  if(/^discussion\b/i.test(s)) return {icon:"messages",col:"var(--accent2)",text:b,kind:"disc"};
  if(/^decision:/i.test(s)) return {icon:"circle-check",col:"var(--green)",text:b,kind:"decision"};
  if(/^action:/i.test(s)) return {icon:"ticket",col:"var(--amber)",text:b,kind:"action"};
  if(/^(blocker|blocked|heads up)\b/i.test(s)||/rejecting/i.test(s)) return {icon:"alert-triangle",col:"var(--amber)",text:b};
  if(/^sprint \d+ review/i.test(s)) return {icon:"clipboard-check",col:"var(--accent2)",text:b};
  if(/^sprint \d+ retro/i.test(s)) return {icon:"refresh",col:"var(--amber)",text:b};
  if(/^sprint \d+ planning/i.test(s)) return {icon:"run",col:"var(--accent2)",text:b};
  if(/^retro lesson/i.test(s)) return {icon:"bulb",col:"var(--green)",text:b,kind:"decision"};
  if(/^commitment:/i.test(s)) return {icon:"target",col:"var(--green)",text:b,kind:"decision"};
  if(/^backlog grooming/i.test(s)) return {icon:"broom",col:"var(--accent2)",text:b,kind:"disc"};
  if(/^standup\b/i.test(s)) return {icon:"microphone",col:"var(--accent2)",text:b,kind:"disc"};
  if(/^(retro|review)\b/i.test(s)) return {icon:"users-group",col:"var(--accent2)",text:b};
  if(/^sprint\b/i.test(s)) return {icon:"flag",col:"var(--accent2)",text:b};
  return null;
}
async function runStandup(btn){
  const old=btn.innerHTML;btn.innerHTML='<i class="ti ti-loader-2"></i> …';btn.disabled=true;
  document.getElementById("disc-filter").value="";
  try{
    const r=await fetch(api("/standup"),{method:"POST"});
    if(r.ok){const s=await(await fetch(api("/state"))).json();render(s);renderDiscuss();}
  }catch(e){}
  btn.innerHTML=old;btn.disabled=false;
}
async function reviewRun(btn,path,who){
  const old=btn.innerHTML;btn.innerHTML='<i class="ti ti-loader-2"></i> …';btn.disabled=true;
  document.getElementById("disc-filter").value="";
  try{
    const r=await fetch(api("/"+path),{method:"POST"});
    if(r.ok){const d=await r.json();const n=d.filed??d.written??0;
      toasty(who+" review done"+(n?" — "+n+" item(s)":" — nothing needed"),"ok");
      const s=await(await fetch(api("/state"))).json();render(s);renderDiscuss();
    } else {toasty("Review failed: "+(await r.text()||r.status)+" (needs a configured engine)","err");}
  }catch(e){toasty("Network error","err");}
  btn.innerHTML=old;btn.disabled=false;
}
async function startDiscussion(){
  const topic=await coxModal({title:"Agent discussion",message:"Chủ đề để team thảo luận — PO + SA cho ý kiến, SM chốt.",input:{placeholder:"e.g. có nên chuyển store sang Postgres ngay sprint này không?",multiline:true},confirmText:"Start discussion"});
  if(!topic||!topic.trim())return;
  const lbl=document.getElementById("disc-run-label");lbl.textContent="Agents discussing…";
  document.getElementById("disc-filter").value="";
  try{
    const r=await fetch(api("/discuss"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({topic:topic.trim()})});
    if(!r.ok){toasty("Discussion failed: "+(await r.text()||r.status)+" (needs a configured engine)","err");}
    else{const s=await(await fetch(api("/state"))).json();render(s);renderDiscuss();}
  }catch(e){toasty("Network error","err");}
  lbl.textContent="Start agent discussion";
}
function sendComment(){const inp=document.getElementById("disc-input");const body=inp.value.trim();
  const attachments=(ATT.disc||[]).slice();
  if(!body&&!attachments.length)return;
  // Slash command? Handle it instead of posting a message.
  const sm=document.getElementById("slash-menu");if(sm)sm.style.display="none";
  if(body[0]==="/"){inp.value="";handleSlash(body);return;}
  const ticket=document.getElementById("disc-filter").value||null;
  inp.value="";ATT.disc=[];renderAttStrip("disc");
  fetch(api("/comments"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({body,ticket,attachments})})
    .then(()=>{
      // Team-channel messages get an intelligent agent reply (routed to the
      // right role; runs the review/standup/discussion if that's the ask).
      if(!ticket && body){
        const lbl=document.getElementById("disc-run-label");if(lbl)lbl.textContent="Agent replying…";
        setDiscBusy("The team is thinking…");
        fetch(api("/chat-reply"),{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({message:body})})
          .then(async()=>{try{const s=await(await fetch(api("/state"))).json();render(s);}catch(e){}})
          .catch(()=>{})
          // The request settling IS the team finishing — clear on it. Waiting
          // for the message count to grow leaves the spinner up forever when
          // the reply lands without changing that count, or the call fails.
          .finally(()=>{if(lbl)lbl.textContent="Agent discussion";DISC_BUSY=null;renderDiscuss();});
      }
    })
    .catch(()=>{});}
// ── Team chat (human-to-human channel, live over WebSocket) ─────────────────
